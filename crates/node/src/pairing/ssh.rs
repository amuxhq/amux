//! SSH pairing identity exchange and stdio relay helpers.

use std::path::Path;
#[cfg(unix)]
use std::pin::Pin;
#[cfg(unix)]
use std::task::{Context, Poll};

use prost::Message as _;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
#[cfg(unix)]
use tokio::io::{ReadBuf, copy_bidirectional};

use super::PairingAdmin;
use crate::identity::{DeviceIdentity, IdentityError, load_or_create_device_identity_in};
use crate::{HostId, ProfileId};

/// A device on the far end of an SSH pairing, as it described itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshPairingPeer {
    pub host_id: HostId,
    pub pubkey: Vec<u8>,
    pub name: String,
}

/// A peer's identity and the profile on its machine it paired from, which
/// later SSH relays select.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshPairingProfile {
    pub identity: SshPairingPeer,
    pub profile: ProfileId,
}

/// How to reach a peer paired over SSH: the ssh destination and the profile
/// there.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshTarget {
    pub target: String,
    pub profile: ProfileId,
}

const SSH_PAIRING_FRAME_MAX_BYTES: usize = 4096;
const HOST_ID_LEN: usize = 16;
const PUBKEY_LEN: usize = 32;
const MAX_PAIRING_NAME_BYTES: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum SshPairingError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protobuf encode error: {0}")]
    Encode(#[from] prost::EncodeError),
    #[error("protobuf decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("SSH pairing frame is too large: {actual} bytes > {max} bytes")]
    FrameTooLarge { actual: usize, max: usize },
    #[error("SSH pairing protocol violation: {0}")]
    Protocol(&'static str),
    #[error("SSH pairing peer is this device")]
    SelfPairing,
    #[error("client service error: {0}")]
    Client(String),
    #[error("identity store error: {0}")]
    Identity(String),
    #[error("SSH transport error: {0}")]
    Transport(String),
}

pub async fn pair_via_ssh_target<P, N, T>(
    data_dir: P,
    local_name: N,
    target: T,
    client: &dyn PairingAdmin,
) -> Result<SshPairingProfile, SshPairingError>
where
    P: AsRef<Path>,
    N: AsRef<str>,
    T: Into<String>,
{
    let target = target.into();
    validate_ssh_target(&target)?;
    let io = crate::transport::spawn_ssh_pair_recv(&target)
        .map_err(|error| SshPairingError::Transport(error.to_string()))?;
    pair_via_ssh_initiator(io, data_dir, local_name, target, client).await
}

pub async fn pair_via_ssh_initiator<IO, P, N, T>(
    mut io: IO,
    data_dir: P,
    local_name: N,
    target: T,
    client: &dyn PairingAdmin,
) -> Result<SshPairingProfile, SshPairingError>
where
    IO: AsyncRead + AsyncWrite + Unpin,
    P: AsRef<Path>,
    N: AsRef<str>,
    T: Into<String>,
{
    let target = target.into();
    validate_ssh_target(&target)?;
    let peer = exchange_as_ssh_initiator(
        &mut io,
        data_dir.as_ref(),
        local_name.as_ref(),
        client.profile_id(),
    )
    .await?;
    client
        .pair_ssh_peer(
            peer.identity.clone(),
            Some(SshTarget {
                target,
                profile: peer.profile,
            }),
        )
        .await
        .map_err(|error| SshPairingError::Client(error.to_string()))?;

    Ok(peer)
}

pub async fn pair_via_ssh_responder<IO, P, N>(
    mut io: IO,
    data_dir: P,
    local_name: N,
    client: &dyn PairingAdmin,
) -> Result<SshPairingProfile, SshPairingError>
where
    IO: AsyncRead + AsyncWrite + Unpin,
    P: AsRef<Path>,
    N: AsRef<str>,
{
    let (local_peer, peer) = read_ssh_responder_peer(
        &mut io,
        data_dir.as_ref(),
        local_name.as_ref(),
        client.profile_id(),
    )
    .await?;
    client
        .pair_ssh_peer(peer.identity.clone(), None)
        .await
        .map_err(|error| SshPairingError::Client(error.to_string()))?;
    write_identity(&mut io, &local_peer).await?;

    Ok(peer)
}

#[cfg(unix)]
pub async fn pair_via_ssh_responder_stdio<P, N>(
    data_dir: P,
    local_name: N,
    client: &dyn PairingAdmin,
) -> Result<SshPairingProfile, SshPairingError>
where
    P: AsRef<Path>,
    N: AsRef<str>,
{
    let stdio = SplitIo::new(tokio::io::stdin(), tokio::io::stdout());
    pair_via_ssh_responder(stdio, data_dir, local_name, client).await
}

async fn exchange_as_ssh_initiator<IO>(
    io: &mut IO,
    data_dir: &Path,
    local_name: &str,
    profile: ProfileId,
) -> Result<SshPairingProfile, SshPairingError>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let (local_identity, local_peer) = local_ssh_identity(data_dir, local_name, profile)?;

    write_identity(io, &local_peer).await?;
    let peer = read_identity(io).await?;
    reject_self_pairing(&local_identity, &peer.identity)?;
    Ok(peer)
}

async fn read_ssh_responder_peer<IO>(
    io: &mut IO,
    data_dir: &Path,
    local_name: &str,
    profile: ProfileId,
) -> Result<(SshPairingProfile, SshPairingProfile), SshPairingError>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let (local_identity, local_peer) = local_ssh_identity(data_dir, local_name, profile)?;

    let peer = read_identity(io).await?;
    if let Err(error @ SshPairingError::SelfPairing) =
        reject_self_pairing(&local_identity, &peer.identity)
    {
        write_identity(io, &local_peer).await?;
        return Err(error);
    }
    Ok((local_peer, peer))
}

#[cfg(unix)]
pub async fn relay_stdio_to_unix_socket<P>(socket_path: P) -> Result<(), SshPairingError>
where
    P: AsRef<Path>,
{
    let mut stdio = SplitIo::new(tokio::io::stdin(), tokio::io::stdout());
    // Through the short link a long path is bound under, as the listener
    // bound it.
    let mut socket = agent_dir::local_socket::connect(socket_path.as_ref()).await?;
    copy_bidirectional(&mut stdio, &mut socket).await?;
    Ok(())
}

fn local_ssh_identity(
    data_dir: &Path,
    local_name: &str,
    profile: ProfileId,
) -> Result<(DeviceIdentity, SshPairingProfile), SshPairingError> {
    validate_pairing_name(local_name)?;
    let identity = load_or_create_device_identity_in(data_dir).map_err(identity_error)?;
    let peer = SshPairingPeer {
        host_id: identity.host_id,
        pubkey: identity.public_key().to_vec(),
        name: local_name.to_string(),
    };
    Ok((
        identity,
        SshPairingProfile {
            identity: peer,
            profile,
        },
    ))
}

async fn write_identity<IO>(io: &mut IO, profile: &SshPairingProfile) -> Result<(), SshPairingError>
where
    IO: AsyncWrite + Unpin,
{
    let peer = &profile.identity;
    validate_peer(peer)?;
    let frame = wire::pb::SshPairingIdentity {
        identity: Some(wire::pb::PairingIdentity {
            expires_at_ms: 0,
            host_id: peer.host_id.as_bytes().to_vec(),
            pubkey: peer.pubkey.clone(),
            name: peer.name.clone(),
        }),
        profile_id: profile.profile.as_bytes().to_vec(),
    }
    .encode_to_vec();
    if frame.len() > SSH_PAIRING_FRAME_MAX_BYTES {
        return Err(SshPairingError::FrameTooLarge {
            actual: frame.len(),
            max: SSH_PAIRING_FRAME_MAX_BYTES,
        });
    }

    io.write_all(&(frame.len() as u32).to_be_bytes()).await?;
    io.write_all(&frame).await?;
    io.flush().await?;
    Ok(())
}

async fn read_identity<IO>(io: &mut IO) -> Result<SshPairingProfile, SshPairingError>
where
    IO: AsyncRead + Unpin,
{
    let mut len = [0_u8; 4];
    io.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > SSH_PAIRING_FRAME_MAX_BYTES {
        return Err(SshPairingError::FrameTooLarge {
            actual: len,
            max: SSH_PAIRING_FRAME_MAX_BYTES,
        });
    }

    let mut frame = vec![0_u8; len];
    io.read_exact(&mut frame).await?;
    let envelope = wire::pb::SshPairingIdentity::decode(frame.as_slice())?;
    let profile = ProfileId::from_slice(&envelope.profile_id)
        .map_err(|_| SshPairingError::Protocol("profile_id must be a UUID"))?;
    let identity = envelope
        .identity
        .ok_or(SshPairingError::Protocol("identity is required"))?;
    let peer = pairing_identity_to_peer(identity)?;
    Ok(SshPairingProfile {
        identity: peer,
        profile,
    })
}

fn pairing_identity_to_peer(
    identity: wire::pb::PairingIdentity,
) -> Result<SshPairingPeer, SshPairingError> {
    if identity.host_id.len() != HOST_ID_LEN {
        return Err(SshPairingError::Protocol("host_id must be 16 bytes"));
    }
    let mut host_id = [0_u8; HOST_ID_LEN];
    host_id.copy_from_slice(&identity.host_id);
    let peer = SshPairingPeer {
        host_id: HostId::from_bytes(host_id),
        pubkey: identity.pubkey,
        name: identity.name,
    };
    validate_peer(&peer)?;
    Ok(peer)
}

fn validate_peer(peer: &SshPairingPeer) -> Result<(), SshPairingError> {
    if peer.pubkey.len() != PUBKEY_LEN {
        return Err(SshPairingError::Protocol("pubkey must be 32 bytes"));
    }
    validate_pairing_name(&peer.name)
}

fn validate_pairing_name(name: &str) -> Result<(), SshPairingError> {
    if name.len() > MAX_PAIRING_NAME_BYTES {
        return Err(SshPairingError::Protocol("name is too long"));
    }
    Ok(())
}

fn validate_ssh_target(target: &str) -> Result<(), SshPairingError> {
    if target.trim().is_empty() {
        return Err(SshPairingError::Protocol("SSH target must not be empty"));
    }
    if target.starts_with('-') {
        return Err(SshPairingError::Protocol(
            "SSH target must not begin with '-'",
        ));
    }
    Ok(())
}

fn reject_self_pairing(
    local_identity: &DeviceIdentity,
    peer: &SshPairingPeer,
) -> Result<(), SshPairingError> {
    if peer.host_id == local_identity.host_id || peer.pubkey == local_identity.public_key() {
        Err(SshPairingError::SelfPairing)
    } else {
        Ok(())
    }
}

fn identity_error(error: IdentityError) -> SshPairingError {
    match error {
        IdentityError::Trust(daemon_core::trust::TrustError::DuplicatePubkey(_)) => {
            SshPairingError::Protocol("pubkey is already trusted by another host")
        }
        other => SshPairingError::Identity(other.to_string()),
    }
}

#[cfg(unix)]
struct SplitIo<R, W> {
    reader: R,
    writer: W,
}

#[cfg(unix)]
impl<R, W> SplitIo<R, W> {
    fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }
}

#[cfg(unix)]
impl<R, W> AsyncRead for SplitIo<R, W>
where
    R: AsyncRead + Unpin,
    W: Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.reader).poll_read(cx, buf)
    }
}

#[cfg(unix)]
impl<R, W> AsyncWrite for SplitIo<R, W>
where
    R: Unpin,
    W: AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.writer).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.writer).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.writer).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[tokio::test]
    async fn ssh_pairing_exchanges_identities_over_length_prefixed_stdio() {
        let initiator_dir = tempfile::tempdir().unwrap();
        let responder_dir = tempfile::tempdir().unwrap();
        let (mut initiator_io, mut responder_io) = tokio::io::duplex(64 * 1024);
        let initiator_profile = ProfileId::new_v4();
        let responder_profile = ProfileId::new_v4();

        let (initiator_result, responder_result) = tokio::join!(
            exchange_as_ssh_initiator(
                &mut initiator_io,
                initiator_dir.path(),
                "laptop",
                initiator_profile,
            ),
            async {
                let (local_peer, peer) = read_ssh_responder_peer(
                    &mut responder_io,
                    responder_dir.path(),
                    "workstation",
                    responder_profile,
                )
                .await?;
                write_identity(&mut responder_io, &local_peer).await?;
                Ok::<_, SshPairingError>(peer)
            },
        );

        let responder_peer = initiator_result.unwrap();
        let initiator_peer = responder_result.unwrap();
        assert_eq!(responder_peer.profile, responder_profile);
        assert_eq!(initiator_peer.profile, initiator_profile);
        assert_eq!(responder_peer.identity.name, "workstation");
        assert_eq!(initiator_peer.identity.name, "laptop");
    }

    #[tokio::test]
    async fn ssh_pairing_rejects_self_pairing_after_both_sides_exchange_identities() {
        let shared_dir = tempfile::tempdir().unwrap();
        let (mut initiator_io, mut responder_io) = tokio::io::duplex(64 * 1024);

        let (initiator_result, responder_result) = tokio::join!(
            exchange_as_ssh_initiator(
                &mut initiator_io,
                shared_dir.path(),
                "same",
                ProfileId::new_v4()
            ),
            read_ssh_responder_peer(
                &mut responder_io,
                shared_dir.path(),
                "same",
                ProfileId::new_v4()
            ),
        );

        assert!(matches!(
            initiator_result.unwrap_err(),
            SshPairingError::SelfPairing
        ));
        assert!(matches!(
            responder_result.unwrap_err(),
            SshPairingError::SelfPairing
        ));
    }

    #[test]
    fn ssh_pairing_rejects_ssh_option_like_targets() {
        assert!(matches!(
            validate_ssh_target("-oProxyCommand=bad").unwrap_err(),
            SshPairingError::Protocol("SSH target must not begin with '-'")
        ));
    }

    #[tokio::test]
    async fn ssh_pairing_rejects_missing_or_label_profile_selectors() {
        for selector in ["", "work"] {
            let frame = wire::pb::SshPairingIdentity {
                identity: Some(wire::pb::PairingIdentity {
                    expires_at_ms: 0,
                    host_id: HostId::new_v4().as_bytes().to_vec(),
                    pubkey: vec![1; PUBKEY_LEN],
                    name: "workstation".into(),
                }),
                profile_id: selector.as_bytes().to_vec(),
            }
            .encode_to_vec();
            let (mut writer, mut reader) = tokio::io::duplex(4096);
            writer
                .write_all(&(frame.len() as u32).to_be_bytes())
                .await
                .unwrap();
            writer.write_all(&frame).await.unwrap();
            assert!(matches!(
                read_identity(&mut reader).await.unwrap_err(),
                SshPairingError::Protocol("profile_id must be a UUID")
            ));
        }
    }

    #[tokio::test]
    async fn ssh_pairing_rejects_oversized_identity_frames_before_buffering() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        writer
            .write_all(&((SSH_PAIRING_FRAME_MAX_BYTES as u32) + 1).to_be_bytes())
            .await
            .unwrap();

        assert!(matches!(
            read_identity(&mut reader).await.unwrap_err(),
            SshPairingError::FrameTooLarge { .. }
        ));
    }
}
