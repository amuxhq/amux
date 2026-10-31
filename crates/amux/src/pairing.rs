//! Pairing, hosts and sign-in: the selected profile's calls on its own
//! socket, a sign-in on the front door, and the two hidden commands SSH
//! runs on the far host of an SSH pairing (`amux internal ssh-pair`) and of
//! every SSH link after it (`amux internal ssh-link`).

use std::io::{IsTerminal as _, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use tonic::transport::Channel;
use uuid::Uuid;
use wire::installation_service_client::InstallationServiceClient;
use wire::profile_admin_service_client::ProfileAdminServiceClient;
use wire::{
    BeginPairRequest, BindProfileRequest, Empty, HostEntry, HostRef, PairingIdentity,
    PairingWindow, PendingPair, RouteKind, StartPairingRequest, TrustSshPeerRequest, UnpairRequest,
    begin_pair_request, pairing_window, peer_reachability, start_pairing_request,
};

type Door = InstallationServiceClient<Channel>;
/// The selected profile's administration, on its own socket.
pub type Admin = ProfileAdminServiceClient<Channel>;

/// How often a waiting pairing mode asks whether it is still open.
const PAIRING_POLL: Duration = Duration::from_secs(1);

fn plain(status: tonic::Status) -> anyhow::Error {
    crate::plain(status)
}

/// Where Pro is bought, for a host only the relay reaches.
fn upgrade_url() -> String {
    format!("{}/account", settings::DEFAULT_CLOUD_URL)
}

/// Whether the profile's relay link is up on the free plan, which carries
/// no connection between hosts: what makes a host only the relay reaches
/// one this machine cannot pair with.
pub fn on_free(profile: &wire::Profile) -> bool {
    use wire::cloud_standing::Standing;
    use wire::relay_link::State;
    matches!(
        profile.cloud.as_ref().and_then(|cloud| cloud.standing.as_ref()),
        Some(Standing::SignedIn(wire::RelayLink {
            state: Some(State::Connected(connected)),
        })) if connected.tier() == wire::Tier::Free
    )
}

/// What `amux pair <target>` names.
#[derive(Debug, PartialEq)]
pub enum Target {
    /// A host nobody paired that this machine knows of, by name or id:
    /// found nearby, or announced by the relay or a paired host.
    Found(String),
    /// A host's direct address.
    Addr(SocketAddr),
    /// An ssh destination: pairing runs `amux internal ssh-pair` there.
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
pub async fn wait(mut admin: Admin, qr: bool, link: bool) -> Result<()> {
    let method = if qr {
        start_pairing_request::Method::Qr(Empty {})
    } else {
        start_pairing_request::Method::Pin(Empty {})
    };
    // Listening before pairing mode opens: a signal between the invitation
    // and the wait would otherwise end the command with pairing left open.
    // One that arrives while it opens is kept and closes it at once.
    let ended = ended()?;
    tokio::pin!(ended);
    let started = admin
        .start_pairing(StartPairingRequest {
            method: Some(method),
            ttl_seconds: None,
        })
        .await
        .map_err(already_open)?
        .into_inner();
    match invitation(&started, link) {
        Ok(lines) => lines.iter().for_each(|line| println!("{line}")),
        Err(error) => {
            cancel(admin).await?;
            return Err(error);
        }
    }
    println!(
        "Pairing mode is open for {}; Ctrl+C closes it.",
        duration(started.ttl_seconds)
    );
    let ttl = Duration::from_secs(started.ttl_seconds);
    let deadline = tokio::time::Instant::now() + ttl;
    loop {
        tokio::select! {
            () = &mut ended => return cancel(admin).await,
            () = tokio::time::sleep(PAIRING_POLL) => {
                let status = admin
                    .get_pairing_status(wire::GetPairingStatusRequest {})
                    .await
                    .map_err(plain)?
                    .into_inner();
                if status.locked_out {
                    bail!(
                        "Pairing mode closed after too many wrong PINs; \
                         run `amux pair` again for a new one."
                    );
                }
                if !status.active || tokio::time::Instant::now() >= deadline {
                    println!("Pairing mode ended.");
                    return Ok(());
                }
            }
        }
    }
}

/// Holds a PIN the person chose open for `ttl` and returns at once: the
/// daemon keeps the window, which every device that presents the PIN pairs
/// through, until it expires, `amux pair --cancel` closes it, or the daemon
/// restarts. For an unattended demo, such as an app reviewer pairing a
/// phone with a machine nobody watches.
pub async fn demo(mut admin: Admin, pin: String, ttl: Duration) -> Result<()> {
    let started = admin
        .start_pairing(StartPairingRequest {
            method: Some(start_pairing_request::Method::DemoPin(pin)),
            ttl_seconds: Some(ttl.as_secs()),
        })
        .await
        .map_err(already_open)?
        .into_inner();
    // The PIN alone: the next steps are for a window someone watches.
    if let Some(pin) = invitation(&started, false)?.first() {
        println!("{pin}");
    }
    println!(
        "Demo pairing is open for {}: this PIN pairs any device that presents it, again and \
         again, until it expires, `amux pair --cancel` closes it, or amux restarts.",
        duration(started.ttl_seconds)
    );
    Ok(())
}

/// The daemon's refusal to open pairing mode while it is open already.
const PAIR_MODE_ALREADY_ACTIVE: &str = "PAIR_MODE_ALREADY_ACTIVE";

/// The refusal does not say which kind of window is open. A PIN or QR
/// window lives as long as the `amux pair` that opened it, but a demo
/// window outlives its command, so the words cover both.
fn already_open(status: tonic::Status) -> anyhow::Error {
    if status.message() == PAIR_MODE_ALREADY_ACTIVE {
        anyhow!(
            "Pairing is already open on this host; finish it or press Ctrl+C where it runs. \
             A demo window has no command running: `amux pair --cancel` closes it."
        )
    } else {
        plain(status)
    }
}

/// Reads a duration as a person writes one: `30d`, `12h`, `45m`, `90s`, or
/// a bare number of seconds.
pub fn parse_duration(input: &str) -> Result<Duration, String> {
    let input = input.trim();
    let (digits, unit) = match input.find(|c: char| !c.is_ascii_digit()) {
        Some(index) => input.split_at(index),
        None => (input, "s"),
    };
    let value: u64 = digits
        .parse()
        .map_err(|_| format!("{input:?} is not a duration; write it like 30d, 12h or 45m"))?;
    let unit = match unit.trim() {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        other => return Err(format!("{other:?} is not a unit; use s, m, h or d")),
    };
    let seconds = value
        .checked_mul(unit)
        .ok_or_else(|| format!("{input:?} is too long"))?;
    if seconds == 0 {
        return Err("a duration must be longer than nothing".to_owned());
    }
    Ok(Duration::from_secs(seconds))
}

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
fn invitation(started: &PairingWindow, link: bool) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    match &started.secret {
        Some(pairing_window::Secret::Pin(pin)) => {
            // What to do next on each kind of device, so the PIN is never
            // left to the docs to explain.
            let name = started
                .identity
                .as_ref()
                .map(|identity| identity.name.as_str())
                .unwrap_or_default();
            lines.push(format!("Pairing PIN: {}", spaced(pin)));
            lines.push(String::new());
            lines.push(format!(
                "On the phone: choose {} and enter the PIN, or run `amux pair --qr` here to scan \
                 instead.",
                if name.is_empty() { "this host" } else { name }
            ));
            let computer = match (name, started.addrs.first()) {
                ("", Some(addr)) => format!("`amux pair {addr}`"),
                ("", None) => "`amux pair <this host>`".to_owned(),
                (name, Some(addr)) => format!(
                    "`{}` (or `amux pair {addr}` if it is not found nearby)",
                    pair_command(name)
                ),
                (name, None) => format!("`{}`", pair_command(name)),
            };
            lines.push(format!(
                "On another computer: run {computer} and enter the PIN."
            ));
        }
        Some(pairing_window::Secret::QrSecret(secret)) => {
            let payload = pair_link(started, secret)?;
            lines.push(terminal_qr(&payload)?);
            lines.push("Scan it with the phone's camera.".to_owned());
            if link {
                lines.push(format!("Pairing link: {payload}"));
            }
        }
        None => bail!("the daemon opened pairing mode without a secret"),
    }
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
    let (count, unit) = match seconds {
        s if s >= 86_400 && s % 86_400 == 0 => (s / 86_400, "day"),
        s if s >= 3_600 && s % 3_600 == 0 => (s / 3_600, "hour"),
        s if s >= 120 && s % 60 == 0 => (s / 60, "minute"),
        s => (s, "second"),
    };
    match count {
        1 => format!("1 {unit}"),
        count => format!("{count} {unit}s"),
    }
}

fn pair_link(started: &PairingWindow, secret: &[u8]) -> Result<String> {
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
pub async fn cancel(mut admin: Admin) -> Result<()> {
    admin
        .cancel_pairing(wire::CancelPairingRequest {})
        .await
        .map_err(plain)?;
    // The terminal may be gone (SIGHUP): saying so must not fail the close.
    let _ = writeln!(std::io::stdout(), "Pairing mode closed.");
    Ok(())
}

/// Pairs with a host this machine knows of or at an address with the PIN
/// its person reads out, or with a host over SSH. A host only the relay
/// reaches is refused before the PIN on the free plan, since no PIN gets a
/// pairing through.
pub async fn pair(
    admin: Admin,
    client: &mut wire::ClientStubs,
    target: Target,
    free: bool,
) -> Result<()> {
    let (host_id, addrs) = match target {
        Target::Ssh(target) => return pair_over_ssh(admin, target).await,
        Target::Addr(addr) => (Vec::new(), vec![addr.to_string()]),
        Target::Found(name) => {
            let (hosts, _) = crate::verbs::inventory(client).await?;
            let host = candidate(&hosts, &name)?;
            if free && host.candidate().is_some_and(wire::Candidate::relay_only) {
                bail!("{}", needs_pro(host.name()));
            }
            (
                host.host_id().to_vec(),
                host.candidate()
                    .map(|candidate| candidate.addrs.clone())
                    .unwrap_or_default(),
            )
        }
    };
    let pin = ask("Pairing PIN shown on the other host: ")?;
    let secret =
        begin_pair_request::Secret::Pin(pin.chars().filter(char::is_ascii_digit).collect());
    confirm(
        admin,
        BeginPairRequest {
            host_id,
            secret: Some(secret),
            addrs,
        },
    )
    .await
}

fn candidate<'a>(hosts: &'a [HostEntry], name: &str) -> Result<&'a HostEntry> {
    let found = hosts
        .iter()
        .filter(|host| host.candidate().is_some())
        .filter(|host| {
            host.name() == name
                || Uuid::from_slice(host.host_id()).is_ok_and(|id| id.to_string() == name)
        })
        .collect::<Vec<_>>();
    match found.as_slice() {
        [host] => Ok(host),
        [] => Err(anyhow!(
            "this machine knows of no unpaired host named {name}; `amux hosts` lists the ones \
             found nearby or announced to it"
        )),
        _ => Err(anyhow!(
            "more than one unpaired host is named {name}; pair by the id `amux hosts` shows"
        )),
    }
}

async fn confirm(mut admin: Admin, pairing: BeginPairRequest) -> Result<()> {
    let pending = admin
        .begin_pair(pairing)
        .await
        .map_err(refused)?
        .into_inner();
    let peer = admin
        .confirm_pair(wire::ConfirmPairRequest {
            token: pending.token.clone(),
        })
        .await
        .map_err(refused)?
        .into_inner();
    println!("{}", paired(&peer, &pending));
    Ok(())
}

/// A pairing's refusal in words. A wrong secret and a closed pairing
/// window read the same on the wire, so a guesser learns nothing, and so
/// the words name both.
fn refused(status: tonic::Status) -> anyhow::Error {
    if status.code() == tonic::Code::PermissionDenied && status.message() == "PAIRING_LOCKED" {
        return anyhow!(
            "the other host closed its pairing window after too many wrong PINs; \
             run `amux pair` there again for a new PIN"
        );
    }
    if status.code() == tonic::Code::PermissionDenied && status.message() == "INVALID_PIN" {
        return anyhow!(
            "the PIN did not match, or the other host's pairing window has closed; \
             check the PIN it shows and try again"
        );
    }
    if client::RpcError::from(status.clone()).reason() == Some(wire::ErrorReason::PaymentRequired) {
        return anyhow!("{}", needs_pro("the other host"));
    }
    plain(status)
}

/// Why a host only the relay reaches cannot be paired on the free plan,
/// and what pairs it.
fn needs_pro(name: &str) -> String {
    format!(
        "{name} can be reached only through the relay, and this account is on the free plan, \
         which carries no connection between hosts. Pro pairs through the relay: {}. On the \
         same network, or through a host both are paired with, pairing works on any plan.",
        upgrade_url()
    )
}

fn paired(peer: &wire::Peer, pending: &PendingPair) -> String {
    let route = match pending.via() {
        RouteKind::Relay => "through the relay",
        _ => "on this network",
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
fn profile_dir(profile: &wire::Profile) -> Result<PathBuf> {
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

/// This profile's identity as its daemon holds it, for the far end of an
/// SSH pairing.
async fn own_identity(admin: &Admin) -> Result<PairingIdentity> {
    let own = admin
        .clone()
        .get_device_identity(wire::GetDeviceIdentityRequest {})
        .await
        .map_err(plain)?
        .into_inner();
    Ok(PairingIdentity {
        host_id: own.host_id,
        pubkey: own.pubkey,
        name: own.name,
        expires_at_ms: 0,
    })
}

/// Trusts `peer`, whose identity SSH carried; `target` is where this host
/// reaches it, when this host ran the SSH.
async fn trust_ssh_peer(
    admin: &Admin,
    peer: PairingIdentity,
    target: Option<String>,
) -> Result<()> {
    admin
        .clone()
        .trust_ssh_peer(TrustSshPeerRequest {
            peer: Some(peer),
            ssh_target: target.map(|target| wire::SshTarget { target }),
        })
        .await
        .map_err(plain)?;
    Ok(())
}

async fn pair_over_ssh(admin: Admin, target: String) -> Result<()> {
    let own = own_identity(&admin).await?;
    let peer = node::exchange_over_ssh(&target, &own).await?;
    trust_ssh_peer(&admin, peer.clone(), Some(target)).await?;
    println!(
        "Paired with {} ({}) over SSH.",
        peer.name,
        host(&peer.host_id)
    );
    Ok(())
}

/// `amux internal ssh-pair`, run by SSH on this host: the identity exchange's far
/// end, over stdin and stdout. It trusts the near end before answering, so
/// the near end trusts only a host that has trusted it.
#[cfg(unix)]
pub async fn pair_recv(admin: Admin) -> Result<()> {
    let own = own_identity(&admin).await?;
    let mut stdio = node::ssh_stdio();
    let peer = node::read_ssh_identity(&mut stdio).await?;
    if peer.host_id == own.host_id || peer.pubkey == own.pubkey {
        // The near end learns it reached itself only from the answer.
        node::write_ssh_identity(&mut stdio, &own).await?;
        bail!("SSH pairing peer is this device");
    }
    trust_ssh_peer(&admin, peer, None).await?;
    node::write_ssh_identity(&mut stdio, &own).await?;
    Ok(())
}

/// `amux internal ssh-link`, run by SSH on this host: stdin and stdout joined to the
/// profile's link socket, carrying a peer's link.
#[cfg(unix)]
pub async fn relay(profile: &wire::Profile) -> Result<()> {
    let socket = profile_dir(profile)?.join(node::LINK_SOCKET);
    node::relay_stdio_to_unix_socket(socket).await?;
    Ok(())
}

/// The trusted hosts and the hosts that could be paired: found nearby, or
/// announced by the relay or a paired host.
pub async fn hosts(mut admin: Admin, client: &mut wire::ClientStubs, free: bool) -> Result<()> {
    let trusted = admin
        .list_peers(wire::ListPeersRequest {})
        .await
        .map_err(plain)?
        .into_inner()
        .peers;
    let (hosts, _) = crate::verbs::inventory(client).await?;
    print!("{}", peer_table(&trusted, &hosts, free));
    Ok(())
}

/// The PAIRED column of a host this machine cannot use until it pairs.
fn pair_again(name: &str) -> String {
    format!("no: {}", pair_command(name))
}

/// The command that pairs with the host of this name, quoted for a shell.
fn pair_command(name: &str) -> String {
    if name.contains(char::is_whitespace) {
        format!("amux pair '{name}'")
    } else {
        format!("amux pair {name}")
    }
}

fn peer_table(trusted: &[wire::Peer], hosts: &[HostEntry], free: bool) -> String {
    let mut rows = vec![[
        "HOST".to_owned(),
        "ID".into(),
        "REACHED".into(),
        "PAIRED".into(),
    ]];
    let mut paired = trusted.to_vec();
    paired.sort_by(|a, b| a.name.cmp(&b.name));
    for peer in &paired {
        let listed = hosts.iter().find(|host| host.host_id() == peer.host_id);
        // Trust is each side's own: this machine still trusts a host that
        // no longer trusts it, which only pairing again undoes.
        let revoked = listed.is_some_and(|host| {
            matches!(host.reach(), Some(wire::trusted::Reach::Offline(offline)) if offline.revoked)
        });
        let reached = match listed {
            Some(_) if revoked => "no longer trusts this machine",
            Some(host) => match host.online() {
                Some(wire::RouteKind::Relay) => "online, through the relay",
                Some(wire::RouteKind::Ssh) => "online, over SSH",
                Some(_) => "online, on this network",
                // The daemon lists a host away only when the relay alone
                // sees it and the account is on the free plan.
                None if host.away() => "away: the free plan does not relay between hosts",
                None => "away",
            },
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
            if revoked {
                pair_again(&peer.name)
            } else {
                "yes".to_owned()
            },
        ]);
    }
    let mut found = hosts
        .iter()
        .filter(|host| host.candidate().is_some())
        .collect::<Vec<_>>();
    found.sort_by(|a, b| a.name().cmp(b.name()));
    for entry in found {
        let Some(candidate) = entry.candidate() else {
            continue;
        };
        let reached = if candidate.nearby() {
            "found nearby".to_owned()
        } else if let Some(through) = candidate.through_paired().next() {
            format!("through {}", through.name)
        } else {
            "on your account, through the relay".to_owned()
        };
        rows.push([
            entry.name().to_owned(),
            host(entry.host_id()),
            reached,
            if free && candidate.relay_only() {
                format!("no: Pro pairs it through the relay, {}", upgrade_url())
            } else {
                pair_again(entry.name())
            },
        ]);
    }
    if rows.len() == 1 {
        return "No paired hosts, and none found nearby or announced. `amux pair` opens pairing \
                mode here.\n"
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
pub async fn unpair(mut admin: Admin, peer: &str, force: bool) -> Result<()> {
    let reference = match Uuid::parse_str(peer) {
        Ok(id) => HostRef::id(id.as_bytes().to_vec()),
        Err(_) => HostRef::named(peer),
    };
    let entry = admin
        .get_peer(wire::GetPeerRequest {
            peer: Some(reference.clone()),
        })
        .await
        .map_err(plain)?
        .into_inner();
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
    let removed = admin
        .unpair(UnpairRequest {
            peer: Some(reference),
            reason: "user".to_owned(),
        })
        .await
        .map_err(plain)?
        .into_inner();
    println!("Unpaired {} ({}).", removed.name, host(&removed.host_id));
    Ok(())
}

/// Signs a profile in: the account service's device sign-in in the
/// browser, then the daemon binds the profile with the refresh token it
/// gave. A profile that already has agents or paired hosts is adopted only
/// when the person says so.
pub async fn login(mut door: Door, profile: Option<&wire::Profile>, cloud_url: &str) -> Result<()> {
    let refresh = node::run_device_flow(cloud_url)
        .await
        .context("signing in")?;
    let mut request = BindProfileRequest {
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
pub async fn logout(mut admin: Admin, profile: &wire::Profile) -> Result<()> {
    admin
        .sign_out(wire::SignOutRequest {})
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
        let refusal = refused(tonic::Status::permission_denied("INVALID_PIN")).to_string();
        assert_eq!(
            refusal,
            "the PIN did not match, or the other host's pairing window has closed; \
             check the PIN it shows and try again"
        );
        assert!(!refusal.contains("INVALID_PIN"));
        // Locked out after too many wrong PINs: said so, with the way on.
        let locked = refused(tonic::Status::permission_denied("PAIRING_LOCKED")).to_string();
        assert!(
            locked.contains("too many wrong PINs") && locked.contains("amux pair"),
            "{locked}"
        );
        // Any other refusal reads as it did.
        assert_eq!(
            refused(tonic::Status::permission_denied("not allowed")).to_string(),
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
        let started = PairingWindow {
            identity: Some(PairingIdentity {
                host_id: host.as_bytes().to_vec(),
                ..PairingIdentity::default()
            }),
            ttl_seconds: 300,
            addrs: vec!["10.0.0.2:7420".into()],
            cloud_url: None,
            secret: Some(pairing_window::Secret::QrSecret(vec![9; 32])),
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
    }

    #[test]
    fn a_pin_reads_in_two_groups_with_what_each_device_does_next() {
        let started = PairingWindow {
            identity: Some(PairingIdentity {
                name: "den mac".into(),
                ..PairingIdentity::default()
            }),
            ttl_seconds: 300,
            addrs: vec!["10.0.0.2:7420".into()],
            secret: Some(pairing_window::Secret::Pin("482913".into())),
            ..PairingWindow::default()
        };
        assert_eq!(
            invitation(&started, false).unwrap(),
            vec![
                "Pairing PIN: 482 913".to_owned(),
                String::new(),
                "On the phone: choose den mac and enter the PIN, or run `amux pair --qr` here \
                 to scan instead."
                    .to_owned(),
                "On another computer: run `amux pair 'den mac'` (or `amux pair 10.0.0.2:7420` \
                 if it is not found nearby) and enter the PIN."
                    .to_owned(),
            ]
        );
    }

    #[test]
    fn a_window_reads_in_its_largest_whole_unit() {
        assert_eq!(duration(300), "5 minutes");
        assert_eq!(duration(90), "90 seconds");
        assert_eq!(duration(3_600), "1 hour");
        assert_eq!(duration(30 * 86_400), "30 days");
    }

    #[test]
    fn a_demo_duration_parses_its_unit_and_refuses_garbage() {
        assert_eq!(parse_duration("45m"), Ok(Duration::from_secs(2_700)));
        assert_eq!(parse_duration("12h"), Ok(Duration::from_secs(43_200)));
        assert_eq!(parse_duration("30d"), Ok(Duration::from_secs(30 * 86_400)));
        assert_eq!(parse_duration("90"), Ok(Duration::from_secs(90)));
        assert!(parse_duration("0d").is_err());
        assert!(parse_duration("3w").is_err());
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn hosts_lists_trusted_hosts_then_hosts_found_nearby() {
        let online = HostEntry::trusted_host(
            vec![1; 16],
            "studio",
            wire::trusted::Reach::Online(wire::RouteKind::Relay as i32),
        );
        let nearby =
            HostEntry::candidate_host(vec![2; 16], "den mac", vec!["192.168.1.9:4000".into()]);
        let trusted = [wire::Peer {
            host_id: vec![1; 16],
            name: "studio".into(),
            ..wire::Peer::default()
        }];
        let table = peer_table(&trusted, &[online, nearby], false);
        let lines: Vec<&str> = table.lines().collect();
        assert!(lines[0].starts_with("HOST"), "{table}");
        assert!(lines[1].starts_with("studio"), "{table}");
        assert!(lines[1].contains("online, through the relay"), "{table}");
        assert!(lines[2].starts_with("den mac"), "{table}");
        assert!(lines[2].ends_with("no: amux pair 'den mac'"), "{table}");
    }

    /// A host the daemon lists away is one the relay refused for the
    /// account's free plan: the table says so, not a bare "away".
    #[test]
    fn a_peer_away_on_the_free_plan_names_the_plan() {
        let away = HostEntry::trusted_host(
            vec![1; 16],
            "plap",
            wire::trusted::Reach::Away(wire::Empty {}),
        );
        let trusted = [wire::Peer {
            host_id: vec![1; 16],
            name: "plap".into(),
            ..wire::Peer::default()
        }];
        let table = peer_table(&trusted, &[away], false);
        assert!(
            table.contains("away: the free plan does not relay between hosts"),
            "{table}"
        );
    }

    /// A host that unpaired this machine is still trusted here, but the
    /// table says it no longer trusts this machine and how to pair again,
    /// not a bare "away" beside "yes".
    #[test]
    fn a_peer_that_no_longer_trusts_this_machine_says_to_pair_again() {
        let revoked = HostEntry::trusted_host(
            vec![1; 16],
            "desk",
            wire::trusted::Reach::Offline(wire::Offline {
                revoked: true,
                ..Default::default()
            }),
        );
        let trusted = [wire::Peer {
            host_id: vec![1; 16],
            name: "desk".into(),
            ..wire::Peer::default()
        }];
        let table = peer_table(&trusted, &[revoked], false);
        let row = table.lines().nth(1).unwrap();
        assert!(row.contains("no longer trusts this machine"), "{table}");
        assert!(row.ends_with("no: amux pair desk"), "{table}");
    }

    fn neighbor(name: &str, cloud_relay: bool) -> wire::Host {
        wire::Host {
            host_id: name.as_bytes().to_vec(),
            name: name.into(),
            capabilities: Some(wire::Capabilities {
                cloud_relay,
                kinds: Vec::new(),
            }),
            ..wire::Host::default()
        }
    }

    /// A host nobody paired that the neighbors `by` say they reach.
    fn announced(name: &str, by: Vec<wire::Host>) -> HostEntry {
        HostEntry {
            standing: Some(wire::host_entry::Standing::Candidate(wire::Candidate {
                addrs: Vec::new(),
                announced_by: by,
            })),
            ..HostEntry::candidate_host(vec![3; 16], name, Vec::new())
        }
    }

    /// A host only the relay announces pairs on Pro; on the free plan the
    /// table says Pro is what pairs it, and where to get it.
    #[test]
    fn a_host_only_the_relay_announces_names_pro_on_the_free_plan() {
        let nova = announced("nova", vec![neighbor("s1", true)]);
        let pro = peer_table(&[], std::slice::from_ref(&nova), false);
        let row = pro.lines().nth(1).unwrap();
        assert!(row.contains("on your account, through the relay"), "{pro}");
        assert!(row.ends_with("no: amux pair nova"), "{pro}");

        let free = peer_table(&[], &[nova], true);
        let row = free.lines().nth(1).unwrap();
        assert!(
            row.ends_with("no: Pro pairs it through the relay, https://amux.sh/account"),
            "{free}"
        );
    }

    /// A paired host announcing a host forwards a pairing on any plan, so
    /// the table names it and the command, never Pro.
    #[test]
    fn a_host_a_paired_host_announces_pairs_through_it_on_any_plan() {
        let nas = announced("nas", vec![neighbor("studio", false), neighbor("s1", true)]);
        let table = peer_table(&[], &[nas], true);
        let row = table.lines().nth(1).unwrap();
        assert!(row.contains("through studio"), "{table}");
        assert!(row.ends_with("no: amux pair nas"), "{table}");
    }

    #[test]
    fn a_free_relay_link_reads_as_the_free_plan() {
        let profile = |tier: wire::Tier| wire::Profile {
            cloud: Some(wire::CloudStanding {
                standing: Some(wire::cloud_standing::Standing::SignedIn(wire::RelayLink {
                    state: Some(wire::relay_link::State::Connected(wire::RelayConnected {
                        tier: tier as i32,
                        carrier: wire::RelayCarrier::Quic as i32,
                    })),
                })),
            }),
            ..wire::Profile::default()
        };
        assert!(on_free(&profile(wire::Tier::Free)));
        assert!(!on_free(&profile(wire::Tier::Pro)));
        assert!(
            !on_free(&wire::Profile::default()),
            "unbound is not the free plan"
        );
    }
}
