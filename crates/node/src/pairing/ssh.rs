//! SSH pairing's identity exchange, and the stdio relay `amux relay` runs.
//!
//! SSH authenticated both machines, so the two hosts only trade
//! identities, each a length-prefixed `PairingIdentity`: the near end runs
//! `amux internal ssh-pair` over SSH, writes its own and reads the far end's; the
//! far end reads, trusts the near end, then answers. Each side stores the
//! trust through its own daemon, which validates the identity and refuses
//! a pairing with itself.

use std::path::Path;

use prost::Message as _;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use wire::PairingIdentity;

const SSH_PAIRING_FRAME_MAX_BYTES: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum SshPairingError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protobuf decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("SSH pairing frame is too large: {actual} bytes > {max} bytes")]
    FrameTooLarge { actual: usize, max: usize },
    #[error("SSH transport error: {0}")]
    Transport(String),
}

/// Runs `amux internal ssh-pair` at `target` over SSH and trades identities with
/// it: `own` goes first, and the far end's comes back once it has trusted
/// this host.
pub async fn exchange_over_ssh(
    target: &str,
    own: &PairingIdentity,
) -> Result<PairingIdentity, SshPairingError> {
    let mut io = crate::transport::spawn_ssh_pair_recv(target)
        .map_err(|error| SshPairingError::Transport(error.to_string()))?;
    let stderr = io.take_stderr();
    let traded = async {
        write_ssh_identity(&mut io, own).await?;
        read_ssh_identity(&mut io).await
    }
    .await;
    match traded {
        Ok(peer) => Ok(peer),
        // A session that ends early shows here as a bare end of stream; what
        // ssh or the far end said is the useful part.
        Err(error) => Err(SshPairingError::Transport(
            crate::transport::with_said(format!("{error} (over SSH to {target})"), stderr).await,
        )),
    }
}

/// This process's stdin and stdout as one stream: what SSH gives the
/// command it runs.
pub fn ssh_stdio() -> impl AsyncRead + AsyncWrite + Unpin {
    tokio::io::join(tokio::io::stdin(), tokio::io::stdout())
}

pub async fn write_ssh_identity<IO>(
    io: &mut IO,
    identity: &PairingIdentity,
) -> Result<(), SshPairingError>
where
    IO: AsyncWrite + Unpin,
{
    let frame = identity.encode_to_vec();
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

pub async fn read_ssh_identity<IO>(io: &mut IO) -> Result<PairingIdentity, SshPairingError>
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
    Ok(PairingIdentity::decode(frame.as_slice())?)
}

/// Joins stdin and stdout to the link socket at `socket_path`, carrying a
/// peer's link.
#[cfg(unix)]
pub async fn relay_stdio_to_unix_socket<P>(socket_path: P) -> Result<(), SshPairingError>
where
    P: AsRef<Path>,
{
    let mut stdio = ssh_stdio();
    // Through the short link a long path is bound under, as the listener
    // bound it.
    let mut socket = agent_dir::local_socket::connect(socket_path.as_ref()).await?;
    tokio::io::copy_bidirectional(&mut stdio, &mut socket).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_identity_crosses_as_one_length_prefixed_frame() {
        let (mut near, mut far) = tokio::io::duplex(64 * 1024);
        let identity = PairingIdentity {
            host_id: vec![7; 16],
            pubkey: vec![1; 32],
            name: "laptop".into(),
            expires_at_ms: 0,
        };
        write_ssh_identity(&mut near, &identity).await.unwrap();
        assert_eq!(read_ssh_identity(&mut far).await.unwrap(), identity);
    }

    #[tokio::test]
    async fn an_oversized_frame_is_refused_before_it_is_buffered() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        writer
            .write_all(&((SSH_PAIRING_FRAME_MAX_BYTES as u32) + 1).to_be_bytes())
            .await
            .unwrap();
        assert!(matches!(
            read_ssh_identity(&mut reader).await.unwrap_err(),
            SshPairingError::FrameTooLarge { .. }
        ));
    }
}
