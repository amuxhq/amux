//! Pairing, peers and sign-in: the selected profile's calls on the front
//! door, and the two hidden commands SSH runs on the far host of an SSH
//! pairing (`amux pair-recv`) and of every SSH link after it (`amux relay`).

use std::io::{IsTerminal as _, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use tonic::transport::Channel;
use uuid::Uuid;
use wire::client_service_client::ClientServiceClient;
use wire::profile_service_client::ProfileServiceClient;
use wire::{
    BeginPairRequest, BindProfileRequest, HostEntry, HostRef, PairingIdentity, PeerEntry, PeerVia,
    PendingPairRequest, PendingPairResponse, ProfileBeginPairRequest, ProfileInfo,
    ProfileOperation, ProfilePairingStatusRequest, ProfilePendingPairRequest, ProfileRequest,
    ProfileStartPairingRequest, ProfileTrustSshPeerRequest, ProfileUnpairRequest,
    StartPairingRequest, StartPairingResponse, Trust, TrustSshPeerRequest, begin_pair_request,
    host_ref, peer_reachability, start_pairing_request, start_pairing_response,
};

type Door = ProfileServiceClient<Channel>;

/// The link a pairing QR code carries, which the phone opens.
/// How often a waiting pairing mode asks whether it is still open.
const PAIRING_POLL: Duration = Duration::from_secs(1);

fn operation() -> String {
    Uuid::new_v4().to_string()
}

fn plain(status: tonic::Status) -> anyhow::Error {
    crate::plain(status)
}

/// What `amux pair <target>` names.
#[derive(Debug, PartialEq)]
pub enum Target {
    /// A host found nearby, by name or id.
    Found(String),
    /// A host's direct address.
    Addr(SocketAddr),
    /// An ssh destination: pairing runs `amux pair-recv` there.
    Ssh(String),
}

impl Target {
    pub fn parse(target: &str) -> Result<Target> {
        let target = target.trim();
        if target.is_empty() {
            bail!("name the host to pair with");
        }
        Ok(if let Ok(addr) = target.parse() {
            Target::Addr(addr)
        } else if target.contains('@') {
            Target::Ssh(target.to_owned())
        } else {
            Target::Found(target.to_owned())
        })
    }
}

/// Opens pairing mode on this host and waits until another host pairs, it
/// expires, or the command is ended (Ctrl+C, its terminal closing, a kill),
/// which closes it.
pub async fn wait(mut door: Door, profile: &ProfileInfo, qr: bool, link: bool) -> Result<()> {
    let mode = if qr {
        start_pairing_request::Mode::Qr
    } else {
        start_pairing_request::Mode::Pin
    };
    // Listening before pairing mode opens: a signal between the invitation
    // and the wait would otherwise end the command with pairing left open.
    // One that arrives while it opens is kept and closes it at once.
    let ended = ended()?;
    tokio::pin!(ended);
    let started = door
        .start_pairing(ProfileStartPairingRequest {
            operation_id: operation(),
            profile_id: profile.id.clone(),
            pairing: Some(StartPairingRequest {
                mode: mode as i32,
                ..StartPairingRequest::default()
            }),
        })
        .await
        .map_err(|status| {
            if status.message() == PAIR_MODE_ALREADY_ACTIVE {
                anyhow!(
                    "Pairing is already open on this host; finish it or press Ctrl+C where it runs."
                )
            } else {
                plain(status)
            }
        })?
        .into_inner();
    match invitation(&started, link) {
        Ok(lines) => lines.iter().for_each(|line| println!("{line}")),
        Err(error) => {
            cancel(door, profile).await?;
            return Err(error);
        }
    }
    let ttl = Duration::from_secs(started.ttl_seconds);
    let deadline = tokio::time::Instant::now() + ttl;
    loop {
        tokio::select! {
            () = &mut ended => return cancel(door, profile).await,
            () = tokio::time::sleep(PAIRING_POLL) => {
                let active = door
                    .get_pairing_status(ProfilePairingStatusRequest {
                        profile_id: profile.id.clone(),
                    })
                    .await
                    .map_err(plain)?
                    .into_inner()
                    .active;
                if !active || tokio::time::Instant::now() >= deadline {
                    println!("Pairing mode ended.");
                    return Ok(());
                }
            }
        }
    }
}

/// The daemon's refusal to open pairing mode while it is open already.
const PAIR_MODE_ALREADY_ACTIVE: &str = "PAIR_MODE_ALREADY_ACTIVE";

/// Resolves when the command is asked to end: Ctrl+C, its terminal closing
/// (SIGHUP) or a kill (SIGTERM). Any of them must close pairing mode, or it
/// stays open until it expires and refuses the next `amux pair`.
#[cfg(unix)]
fn ended() -> Result<impl std::future::Future<Output = ()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt()).context("listening for Ctrl+C")?;
    let mut terminate = signal(SignalKind::terminate()).context("listening for SIGTERM")?;
    let mut hangup = signal(SignalKind::hangup()).context("listening for SIGHUP")?;
    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
            _ = hangup.recv() => {}
        }
    })
}

#[cfg(not(unix))]
fn ended() -> Result<impl std::future::Future<Output = ()>> {
    use tokio::signal::windows::{ctrl_c, ctrl_close};
    let mut interrupt = ctrl_c().context("listening for Ctrl+C")?;
    let mut close = ctrl_close().context("listening for the console closing")?;
    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = close.recv() => {}
        }
    })
}

/// What pairing mode shows the other host's person: the PIN and where this
/// host listens, or a QR code the phone scans.
fn invitation(started: &StartPairingResponse, link: bool) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    match &started.secret {
        Some(start_pairing_response::Secret::Pin(pin)) => {
            lines.push(format!("Pairing PIN: {}", spaced(pin)));
            if !started.addrs.is_empty() {
                lines.push(format!(
                    "This host listens at {}.",
                    started.addrs.join(", ")
                ));
            }
        }
        Some(start_pairing_response::Secret::QrSecret(secret)) => {
            let payload = pair_link(started, secret)?;
            lines.push(terminal_qr(&payload)?);
            if link {
                lines.push(format!("Pairing link: {payload}"));
            }
        }
        None => bail!("the daemon opened pairing mode without a secret"),
    }
    lines.push(format!(
        "Pairing mode is open for {}; Ctrl+C closes it.",
        duration(started.ttl_seconds)
    ));
    Ok(lines)
}

/// A PIN as people read it aloud: two groups of three.
fn spaced(pin: &str) -> String {
    if pin.len() == 6 && pin.is_ascii() {
        format!("{} {}", &pin[..3], &pin[3..])
    } else {
        pin.to_owned()
    }
}

fn duration(seconds: u64) -> String {
    match seconds {
        s if s >= 120 && s % 60 == 0 => format!("{} minutes", s / 60),
        s => format!("{s} seconds"),
    }
}

fn pair_link(started: &StartPairingResponse, secret: &[u8]) -> Result<String> {
    let identity = started
        .identity
        .as_ref()
        .ok_or_else(|| anyhow!("the daemon opened pairing mode without its identity"))?;
    let host = Uuid::from_slice(&identity.host_id).context("the pairing host id")?;
    let addrs = started
        .addrs
        .iter()
        .filter_map(|addr| addr.parse().ok())
        .collect::<Vec<SocketAddr>>();
    let json =
        node::encode_qr_pairing_invitation(host, &addrs, started.cloud_url.as_deref(), secret)
            .context("encoding the pairing invitation")?;
    Ok(node::pair_link(&json))
}

fn terminal_qr(payload: &str) -> Result<String> {
    let code = qrcode::QrCode::new(payload.as_bytes()).context("encoding the QR code")?;
    Ok(code
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(true)
        .build())
}

/// Closes this host's pairing mode.
pub async fn cancel(mut door: Door, profile: &ProfileInfo) -> Result<()> {
    door.cancel_pairing(ProfileOperation {
        operation_id: operation(),
        profile_id: profile.id.clone(),
    })
    .await
    .map_err(plain)?;
    // The terminal may be gone (SIGHUP): saying so must not fail the close.
    let _ = writeln!(std::io::stdout(), "Pairing mode closed.");
    Ok(())
}

/// Pairs with a host found nearby or at an address with the PIN its person
/// reads out, or with a host over SSH.
pub async fn pair(
    door: Door,
    client: &mut ClientServiceClient<Channel>,
    profile: &ProfileInfo,
    target: Target,
) -> Result<()> {
    let (host_id, addrs) = match target {
        Target::Ssh(target) => return pair_over_ssh(door, profile, target).await,
        Target::Addr(addr) => (Vec::new(), vec![addr.to_string()]),
        Target::Found(name) => {
            let (hosts, _) = crate::verbs::inventory(client).await?;
            let host = candidate(&hosts, &name)?;
            (host.host_id.clone(), host.addrs.clone())
        }
    };
    let pin = ask("Pairing PIN shown on the other host: ")?;
    let secret =
        begin_pair_request::Secret::Pin(pin.chars().filter(char::is_ascii_digit).collect());
    confirm(
        door,
        profile,
        BeginPairRequest {
            host_id,
            secret: Some(secret),
            addrs,
        },
    )
    .await
}

/// Pairs with the host whose QR code's link this is.
pub async fn pair_with_link(door: Door, profile: &ProfileInfo, link: &str) -> Result<()> {
    let invitation = node::parse_pair_link(link).context("the pairing link")?;
    confirm(
        door,
        profile,
        BeginPairRequest {
            host_id: invitation.host_id.as_bytes().to_vec(),
            secret: Some(begin_pair_request::Secret::QrSecret(invitation.secret)),
            addrs: invitation.addrs.iter().map(ToString::to_string).collect(),
        },
    )
    .await
}

fn candidate<'a>(hosts: &'a [HostEntry], name: &str) -> Result<&'a HostEntry> {
    let found = hosts
        .iter()
        .filter(|host| host.trust() == Trust::Candidate)
        .filter(|host| {
            host.name == name
                || Uuid::from_slice(&host.host_id).is_ok_and(|id| id.to_string() == name)
        })
        .collect::<Vec<_>>();
    match found.as_slice() {
        [host] => Ok(host),
        [] => Err(anyhow!(
            "no host named {name} is waiting nearby; `amux peers` lists the hosts found"
        )),
        _ => Err(anyhow!(
            "more than one host nearby is named {name}; pair by the id `amux peers` shows"
        )),
    }
}

async fn confirm(mut door: Door, profile: &ProfileInfo, pairing: BeginPairRequest) -> Result<()> {
    let secret = match pairing.secret {
        Some(begin_pair_request::Secret::QrSecret(_)) => "pairing link",
        _ => "PIN",
    };
    let pending = door
        .begin_pair(ProfileBeginPairRequest {
            operation_id: operation(),
            profile_id: profile.id.clone(),
            pairing: Some(pairing),
        })
        .await
        .map_err(|status| refused(status, secret))?
        .into_inner();
    let peer = door
        .confirm_pair(ProfilePendingPairRequest {
            operation_id: operation(),
            profile_id: profile.id.clone(),
            pairing: Some(PendingPairRequest {
                token: pending.token.clone(),
            }),
        })
        .await
        .map_err(|status| refused(status, secret))?
        .into_inner()
        .peer
        .unwrap_or_default();
    println!("{}", paired(&peer, &pending));
    Ok(())
}

/// A pairing's refusal in words. A wrong secret and a closed pairing
/// window read the same on the wire, so a guesser learns nothing, and so
/// the words name both.
fn refused(status: tonic::Status, secret: &str) -> anyhow::Error {
    if status.code() == tonic::Code::PermissionDenied && status.message() == "INVALID_PIN" {
        return anyhow!(
            "the {secret} did not match, or the other host's pairing window has closed; \
             check the {secret} it shows and try again"
        );
    }
    plain(status)
}

fn paired(peer: &PeerEntry, pending: &PendingPairResponse) -> String {
    let route = match pending.via() {
        PeerVia::Relay => "through the relay",
        PeerVia::Ssh => "over SSH",
        PeerVia::Direct | PeerVia::Unspecified => "on this network",
    };
    format!(
        "Paired with {} ({}) {route}.",
        peer.name,
        host(&peer.host_id)
    )
}

fn host(id: &[u8]) -> String {
    Uuid::from_slice(id).map_or_else(|_| "unknown id".to_owned(), |id| id.to_string())
}

fn ask(prompt: &str) -> Result<String> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("reading the answer")?;
    Ok(answer.trim().to_owned())
}

/// The profile's directory: where its client socket, identity and link
/// socket live, on this machine.
fn profile_dir(profile: &ProfileInfo) -> Result<PathBuf> {
    Path::new(&profile.socket_path)
        .parent()
        .map(Path::to_owned)
        .ok_or_else(|| {
            anyhow!(
                "the profile socket {} has no directory",
                profile.socket_path
            )
        })
}

/// The trust an SSH pairing establishes, stored through the front door.
struct DoorAdmin {
    door: Door,
    profile: Uuid,
}

#[async_trait::async_trait]
impl node::PairingAdmin for DoorAdmin {
    fn profile_id(&self) -> node::ProfileId {
        self.profile
    }

    async fn pair_ssh_peer(
        &self,
        peer: node::SshPairingPeer,
        target: Option<node::SshTarget>,
    ) -> Result<(), String> {
        self.door
            .clone()
            .trust_ssh_peer(ProfileTrustSshPeerRequest {
                operation_id: operation(),
                profile_id: self.profile.to_string(),
                pairing: Some(TrustSshPeerRequest {
                    peer: Some(PairingIdentity {
                        host_id: peer.host_id.as_bytes().to_vec(),
                        pubkey: peer.pubkey,
                        name: peer.name,
                        expires_at_unix_ms: 0,
                    }),
                    ssh_target: target.map(|target| wire::SshTarget {
                        target: target.target,
                        profile_id: target.profile.to_string(),
                    }),
                }),
            })
            .await
            .map(drop)
            .map_err(|status| status.message().to_owned())
    }
}

async fn admin(door: &Door, profile: &ProfileInfo) -> Result<(DoorAdmin, String)> {
    let name = door
        .clone()
        .get_device_identity(ProfileRequest {
            profile_id: profile.id.clone(),
        })
        .await
        .map_err(plain)?
        .into_inner()
        .name;
    let admin = DoorAdmin {
        door: door.clone(),
        profile: Uuid::parse_str(&profile.id).context("the profile id")?,
    };
    Ok((admin, name))
}

async fn pair_over_ssh(door: Door, profile: &ProfileInfo, target: String) -> Result<()> {
    let (admin, name) = admin(&door, profile).await?;
    let peer = node::pair_via_ssh_target(profile_dir(profile)?, name, target, &admin).await?;
    println!(
        "Paired with {} ({}) over SSH.",
        peer.identity.name, peer.identity.host_id
    );
    Ok(())
}

/// `amux pair-recv`, run by SSH on this host: the identity exchange's far
/// end, over stdin and stdout.
#[cfg(unix)]
pub async fn pair_recv(door: Door, profile: &ProfileInfo) -> Result<()> {
    let (admin, name) = admin(&door, profile).await?;
    node::pair_via_ssh_responder_stdio(profile_dir(profile)?, name, &admin).await?;
    Ok(())
}

/// `amux relay`, run by SSH on this host: stdin and stdout joined to the
/// profile's link socket, carrying a peer's link.
#[cfg(unix)]
pub async fn relay(profile: &ProfileInfo) -> Result<()> {
    let socket = profile_dir(profile)?.join(node::LINK_SOCKET);
    node::relay_stdio_to_unix_socket(socket).await?;
    Ok(())
}

/// The trusted hosts and the hosts found nearby that could be paired.
pub async fn peers(
    mut door: Door,
    client: &mut ClientServiceClient<Channel>,
    profile: &ProfileInfo,
) -> Result<()> {
    let trusted = door
        .list_peers(ProfileRequest {
            profile_id: profile.id.clone(),
        })
        .await
        .map_err(plain)?
        .into_inner()
        .peers;
    let (hosts, _) = crate::verbs::inventory(client).await?;
    print!("{}", peer_table(&trusted, &hosts));
    Ok(())
}

fn peer_table(trusted: &[PeerEntry], hosts: &[HostEntry]) -> String {
    let mut rows = vec![[
        "HOST".to_owned(),
        "ID".into(),
        "REACHED".into(),
        "PAIRED".into(),
    ]];
    let mut paired = trusted.to_vec();
    paired.sort_by(|a, b| a.name.cmp(&b.name));
    for peer in &paired {
        let reached = match hosts.iter().find(|host| host.host_id == peer.host_id) {
            Some(host) if host.presence() == wire::Presence::Online => match host.via() {
                wire::HostVia::Relay => "online, through the relay",
                wire::HostVia::Ssh => "online, over SSH",
                _ => "online, on this network",
            },
            Some(_) => "away",
            None => "not listed",
        };
        let ssh = peer.reachabilities.iter().find_map(|way| match &way.kind {
            Some(peer_reachability::Kind::SshTarget(target)) => Some(target.target.clone()),
            _ => None,
        });
        rows.push([
            peer.name.clone(),
            host(&peer.host_id),
            match ssh {
                Some(target) => format!("{reached} (ssh {target})"),
                None => reached.to_owned(),
            },
            "yes".to_owned(),
        ]);
    }
    let mut found = hosts
        .iter()
        .filter(|host| host.trust() == Trust::Candidate)
        .collect::<Vec<_>>();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    for candidate in found {
        let name = if candidate.name.contains(char::is_whitespace) {
            format!("'{}'", candidate.name)
        } else {
            candidate.name.clone()
        };
        rows.push([
            candidate.name.clone(),
            host(&candidate.host_id),
            "found nearby".to_owned(),
            format!("no: amux pair {name}"),
        ]);
    }
    if rows.len() == 1 {
        return "No paired hosts, and none found nearby. `amux pair` opens pairing mode here.\n"
            .to_owned();
    }
    let widths: Vec<usize> = (0..4)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for row in rows {
        let line = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:width$}"))
            .collect::<Vec<_>>()
            .join("  ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Stops trusting a paired host, asking first unless forced.
pub async fn unpair(mut door: Door, profile: &ProfileInfo, peer: &str, force: bool) -> Result<()> {
    let reference = HostRef {
        of: Some(match Uuid::parse_str(peer) {
            Ok(id) => host_ref::Of::HostId(id.as_bytes().to_vec()),
            Err(_) => host_ref::Of::Name(peer.to_owned()),
        }),
    };
    let entry = door
        .get_peer(wire::ProfileGetPeerRequest {
            profile_id: profile.id.clone(),
            peer: Some(reference.clone()),
        })
        .await
        .map_err(plain)?
        .into_inner()
        .peer
        .unwrap_or_default();
    if !force {
        if !std::io::stdin().is_terminal() {
            bail!("unpairing asks first; pass --force to unpair without asking");
        }
        let answer = ask(&format!(
            "Stop trusting {} ({})? Its agents leave this fleet. [y/N] ",
            entry.name,
            host(&entry.host_id)
        ))?;
        if !matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes") {
            println!("Still paired.");
            return Ok(());
        }
    }
    let removed = door
        .unpair(ProfileUnpairRequest {
            operation_id: operation(),
            profile_id: profile.id.clone(),
            peer: Some(reference),
            reason: "user".to_owned(),
        })
        .await
        .map_err(plain)?
        .into_inner()
        .removed_peer
        .unwrap_or_default();
    println!("Unpaired {} ({}).", removed.name, host(&removed.host_id));
    Ok(())
}

/// Signs a profile in: the account service's device sign-in in the
/// browser, then the daemon binds the profile with the refresh token it
/// gave. A profile that already has agents or paired hosts is adopted only
/// when the person says so.
pub async fn login(mut door: Door, profile: Option<&ProfileInfo>, cloud_url: &str) -> Result<()> {
    let refresh = node::run_device_flow(cloud_url)
        .await
        .context("signing in")?;
    let mut request = BindProfileRequest {
        operation_id: operation(),
        profile_id: profile.map(|profile| profile.id.clone()),
        cloud_url: cloud_url.to_owned(),
        staged_refresh_token: refresh,
        adopt_non_pristine: false,
        client_id: String::new(),
    };
    let bound = match door.bind_profile(request.clone()).await {
        Err(status)
            if status.code() == tonic::Code::FailedPrecondition
                && status.message().contains("adopting") =>
        {
            let answer = ask(
                "This profile already has agents or paired hosts. Sign it in to this account \
                 anyway? [y/N] ",
            )?;
            if !matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes") {
                println!("Not signed in.");
                return Ok(());
            }
            request.adopt_non_pristine = true;
            request.operation_id = operation();
            door.bind_profile(request).await
        }
        other => other,
    }
    .map_err(plain)?
    .into_inner();
    let who = [&bound.account_name, &bound.email]
        .into_iter()
        .find(|who| !who.is_empty())
        .cloned()
        .unwrap_or_else(|| "your account".to_owned());
    println!("Signed {} in as {who}.", bound.label);
    Ok(())
}

/// Signs a profile out; its agents and paired hosts stay.
pub async fn logout(mut door: Door, profile: &ProfileInfo) -> Result<()> {
    door.logout_profile(ProfileOperation {
        operation_id: operation(),
        profile_id: profile.id.clone(),
    })
    .await
    .map_err(plain)?;
    println!(
        "Signed {} out; its agents and paired hosts stay.",
        profile.label
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_pin_is_refused_in_words() {
        let refusal = refused(tonic::Status::permission_denied("INVALID_PIN"), "PIN").to_string();
        assert_eq!(
            refusal,
            "the PIN did not match, or the other host's pairing window has closed; \
             check the PIN it shows and try again"
        );
        assert!(!refusal.contains("INVALID_PIN"));
        // Any other refusal reads as it did.
        assert_eq!(
            refused(tonic::Status::permission_denied("not allowed"), "PIN").to_string(),
            "not allowed"
        );
    }

    #[test]
    fn a_pair_target_is_an_address_an_ssh_destination_or_a_name() {
        assert_eq!(
            Target::parse("192.168.1.4:7420").unwrap(),
            Target::Addr("192.168.1.4:7420".parse().unwrap())
        );
        assert_eq!(
            Target::parse("me@studio.local").unwrap(),
            Target::Ssh("me@studio.local".into())
        );
        assert_eq!(
            Target::parse(" den mac ").unwrap(),
            Target::Found("den mac".into())
        );
        assert!(Target::parse("  ").is_err());
    }

    #[test]
    fn a_qr_invitation_round_trips_through_its_link() {
        let host = Uuid::from_bytes([3; 16]);
        let started = StartPairingResponse {
            identity: Some(PairingIdentity {
                host_id: host.as_bytes().to_vec(),
                ..PairingIdentity::default()
            }),
            ttl_seconds: 300,
            addrs: vec!["10.0.0.2:7420".into()],
            cloud_url: None,
            secret: Some(start_pairing_response::Secret::QrSecret(vec![9; 32])),
        };
        let lines = invitation(&started, true).unwrap();
        let link = lines
            .iter()
            .find_map(|line| line.strip_prefix("Pairing link: "))
            .unwrap();
        let parsed = node::parse_pair_link(link).unwrap();
        assert_eq!(parsed.host_id, host);
        assert_eq!(parsed.secret, vec![9; 32]);
        assert_eq!(parsed.addrs, vec!["10.0.0.2:7420".parse().unwrap()]);
        assert!(lines.last().unwrap().contains("5 minutes"));
    }

    #[test]
    fn a_pin_reads_in_two_groups() {
        let started = StartPairingResponse {
            ttl_seconds: 300,
            addrs: vec!["10.0.0.2:7420".into()],
            secret: Some(start_pairing_response::Secret::Pin("482913".into())),
            ..StartPairingResponse::default()
        };
        assert_eq!(
            invitation(&started, false).unwrap(),
            vec![
                "Pairing PIN: 482 913".to_owned(),
                "This host listens at 10.0.0.2:7420.".to_owned(),
                "Pairing mode is open for 5 minutes; Ctrl+C closes it.".to_owned(),
            ]
        );
    }

    #[test]
    fn peers_list_trusted_hosts_then_hosts_found_nearby() {
        let online = HostEntry {
            host_id: vec![1; 16],
            name: "studio".into(),
            trust: Trust::Trusted as i32,
            presence: wire::Presence::Online as i32,
            via: wire::HostVia::Relay as i32,
            ..HostEntry::default()
        };
        let nearby = HostEntry {
            host_id: vec![2; 16],
            name: "den mac".into(),
            trust: Trust::Candidate as i32,
            presence: wire::Presence::Online as i32,
            ..HostEntry::default()
        };
        let trusted = [PeerEntry {
            host_id: vec![1; 16],
            name: "studio".into(),
            ..PeerEntry::default()
        }];
        let table = peer_table(&trusted, &[online, nearby]);
        let lines: Vec<&str> = table.lines().collect();
        assert!(lines[0].starts_with("HOST"), "{table}");
        assert!(lines[1].starts_with("studio"), "{table}");
        assert!(lines[1].contains("online, through the relay"), "{table}");
        assert!(lines[2].starts_with("den mac"), "{table}");
        assert!(lines[2].ends_with("no: amux pair 'den mac'"), "{table}");
    }
}
