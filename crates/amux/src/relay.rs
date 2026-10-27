//! `amux server start --cloud`: the cloud relay a deployment runs.
//!
//! The relay accepts every host of an account over QUIC and a TLS-over-TCP
//! fallback, checks each host's connection token against the cloud's
//! signing keys, and forwards packets between the account's paid hosts. It is
//! not an installation: it has no profiles, no agents, no supervisor and no
//! updater, and it runs in the foreground under whatever service manager
//! started it.
//!
//! The configuration is the TOML file `--config` (or `AMUX_CONFIG`) names;
//! a key it does not know is refused:
//!
//! ```toml
//! host_name = "s1.amux.sh"   # the name connection tokens are minted for
//! tcp_port = 9001            # the TLS listener; tokens name this port
//! udp_port = 9001            # the QUIC listener; the TCP port when absent
//! cloud_url = "https://amux.sh"
//! tls_cert = "/etc/amux-relay/tls/fullchain.pem"
//! tls_key = "/etc/amux-relay/tls/privkey.pem"
//! state_dir = "/var/lib/amux-relay"
//! ```
//!
//! Relative paths are resolved beside the file. `state_dir` holds the
//! session tickets the relay issues (`tickets/`) and the dumps its core asks
//! for (`reports/`, the newest kept); it defaults to the directory systemd
//! names in `STATE_DIRECTORY`, else the file's own. The relay logs to
//! standard error, where its service manager keeps and rotates it;
//! `AMUX_LOG` names a file instead.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use node::{CloudLinkServer, JwtCloudLinkAuthenticator, RelayIdentity};
use serde::Deserialize;

/// How long a carrier gets to finish its TLS handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long link-close notices get to reach hosts before the listeners go.
const CLOSE_FLUSH: Duration = Duration::from_millis(500);
const LOG_ENV: &str = "AMUX_LOG";
/// Where systemd tells a unit with `StateDirectory=` its state lives.
const STATE_DIRECTORY_ENV: &str = "STATE_DIRECTORY";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayConfig {
    host_name: String,
    tcp_port: u16,
    #[serde(default)]
    udp_port: Option<u16>,
    #[serde(default = "default_cloud_url")]
    cloud_url: String,
    tls_cert: PathBuf,
    tls_key: PathBuf,
    #[serde(default)]
    state_dir: Option<PathBuf>,
}

fn default_cloud_url() -> String {
    settings::DEFAULT_CLOUD_URL.into()
}

fn config_path(path: Option<&Path>) -> Result<PathBuf> {
    path.map(Path::to_owned).ok_or_else(|| {
        anyhow!("the relay needs its configuration: pass --config or set AMUX_CONFIG")
    })
}

fn read_pem(path: &Path, what: &str) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("reading the TLS {what} {}", path.display()))
}

pub fn run(config: Option<&Path>) -> Result<()> {
    let path = config_path(config)?;
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let config: RelayConfig =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if config.host_name.is_empty() {
        bail!("{}: host_name must name the relay", path.display());
    }
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let cert = read_pem(&base.join(&config.tls_cert), "certificate")?;
    let key = read_pem(&base.join(&config.tls_key), "key")?;
    match std::env::var_os(LOG_ENV) {
        Some(file) => crate::server::log_to(Path::new(&file))?,
        None => crate::server::log_to_stderr(),
    }
    let state = state_dir(
        base,
        config.state_dir.as_deref(),
        std::env::var_os(STATE_DIRECTORY_ENV).as_deref(),
    );
    // The session tickets the relay issued outlive it, so a restarted relay
    // still resumes the hosts that held them. A directory the relay cannot
    // write costs each host one full handshake after a restart, not the
    // relay.
    let tickets_dir = state.join(node::TICKETS_DIR);
    let tickets =
        node::SessionTickets::open(&tickets_dir, node::RELAY_TICKETS).unwrap_or_else(|error| {
            tracing::warn!(
                %error,
                dir = %tickets_dir.display(),
                "session tickets will not outlive this run"
            );
            node::SessionTickets::in_memory(node::RELAY_TICKETS)
        });
    let reports = state.join(node::REPORTS);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(config, reports, cert, key, tickets))
}

/// Where the relay keeps its tickets and reports: the configured directory
/// (resolved beside the configuration), else the one systemd names, else
/// the configuration's own directory.
fn state_dir(base: &Path, configured: Option<&Path>, systemd: Option<&std::ffi::OsStr>) -> PathBuf {
    match (configured, systemd) {
        (Some(dir), _) => base.join(dir),
        // systemd lists one directory per StateDirectory= entry,
        // colon-separated; the relay names one.
        (None, Some(dirs)) => dirs
            .to_str()
            .and_then(|dirs| dirs.split(':').next())
            .map_or_else(|| PathBuf::from(dirs), PathBuf::from),
        (None, None) => base.to_owned(),
    }
}

async fn serve(
    config: RelayConfig,
    reports: PathBuf,
    cert: Vec<u8>,
    key: Vec<u8>,
    tickets: Arc<node::SessionTickets>,
) -> Result<()> {
    let tls = node::create_tls_acceptor(&cert, &key).context("loading the TLS certificate")?;
    let quic = node::relay_quic_server_config(&cert, &key, &tickets)
        .context("loading the TLS certificate")?;
    let clock: Arc<dyn node::Clock> = Arc::new(agent_dir::SystemClock);
    let server = CloudLinkServer::new(
        RelayIdentity {
            host_id: uuid::Uuid::new_v4(),
            name: config.host_name.clone(),
            authenticator: Arc::new(JwtCloudLinkAuthenticator::new(
                &config.cloud_url,
                config.host_name.clone(),
                config.tcp_port,
                clock.clone(),
            )),
            clock,
        },
        Some(reports),
    );
    let tcp_addr = SocketAddr::from(([0, 0, 0, 0], config.tcp_port));
    let listener = tokio::net::TcpListener::bind(tcp_addr)
        .await
        .with_context(|| format!("listening on TCP {tcp_addr}"))?;
    tracing::info!(addr = %tcp_addr, "listening for cloud TLS carriers");
    let udp_addr = SocketAddr::from(([0, 0, 0, 0], config.udp_port.unwrap_or(config.tcp_port)));
    let udp = std::net::UdpSocket::bind(udp_addr)
        .with_context(|| format!("listening on UDP {udp_addr}"))?;
    tracing::info!(addr = %udp_addr, "listening for cloud QUIC carriers");
    println!(
        "amux relay {} listening on TCP {tcp_addr} and UDP {udp_addr}, tokens from {}",
        config.host_name, config.cloud_url
    );
    let serving = server
        .serve_tls(udp, listener, tls, quic, HANDSHAKE_TIMEOUT)
        .context("serving the relay")?;
    crate::server::terminated().await;
    tracing::info!("relay stopping");
    server
        .send_link_close_to_all(wire::pb::LinkCloseReason::UserShutdown)
        .await;
    tokio::time::sleep(CLOSE_FLUSH).await;
    serving.close();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_goes_where_the_configuration_says_else_where_systemd_says() {
        let base = Path::new("/etc/amux-relay");
        let systemd = std::ffi::OsStr::new("/var/lib/amux-relay");
        assert_eq!(state_dir(base, None, None), base);
        assert_eq!(
            state_dir(base, None, Some(systemd)),
            Path::new("/var/lib/amux-relay")
        );
        assert_eq!(
            state_dir(base, Some(Path::new("state")), Some(systemd)),
            base.join("state")
        );
        assert_eq!(
            state_dir(base, Some(Path::new("/srv/relay")), None),
            Path::new("/srv/relay")
        );
    }

    #[test]
    fn a_key_the_relay_does_not_know_is_refused() {
        let text = "host_name = \"r\"\ntcp_port = 1\ntls_cert = \"c\"\ntls_key = \"k\"\n";
        assert!(toml::from_str::<RelayConfig>(text).is_ok());
        let old = format!("{text}reports_dir = \"/r\"\n");
        assert!(toml::from_str::<RelayConfig>(&old).is_err());
    }
}
