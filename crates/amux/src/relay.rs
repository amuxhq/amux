//! `amux server start --cloud`: the cloud relay a deployment runs.
//!
//! The relay accepts every host of an account over QUIC and a TLS-over-TCP
//! fallback, checks each host's connection token against the cloud's
//! signing keys, and forwards streams between the account's hosts. It is
//! not an installation: it has no profiles, no agents, no supervisor and no
//! updater, and it runs in the foreground under whatever service manager
//! started it.
//!
//! The configuration is the file `--config` (or `AMUX_CONFIG`) names:
//!
//! ```yaml
//! host_name: s1.amux.sh   # the name connection tokens are minted for
//! tcp_port: 9001          # the TLS listener; tokens name this port
//! udp_port: 9001          # the QUIC listener; the TCP port when absent
//! cloud_url: https://amux.sh
//! reports_dir: /var/lib/amux-relay/reports  # beside this file when absent
//! ```
//!
//! The certificate and key are PEM files named by `AMUX_TLS_CERT` and
//! `AMUX_TLS_KEY`, or by `tls_cert` and `tls_key` in the file. Other keys an
//! older relay configuration carried are ignored. A dump the relay's core
//! asks for (a broken invariant, a step over its budget) is a text file in
//! `reports_dir`; the default is `reports` in the configuration file's
//! directory.

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
const CERT_ENV: &str = "AMUX_TLS_CERT";
const KEY_ENV: &str = "AMUX_TLS_KEY";
const LOG_ENV: &str = "AMUX_LOG";

#[derive(Debug, Deserialize)]
struct RelayConfig {
    host_name: String,
    tcp_port: u16,
    #[serde(default)]
    udp_port: Option<u16>,
    #[serde(default = "default_cloud_url")]
    cloud_url: String,
    #[serde(default)]
    tls_cert: Option<PathBuf>,
    #[serde(default)]
    tls_key: Option<PathBuf>,
    #[serde(default)]
    reports_dir: Option<PathBuf>,
}

fn default_cloud_url() -> String {
    "https://amux.sh".into()
}

fn config_path(path: Option<&Path>) -> Result<PathBuf> {
    path.map(Path::to_owned).ok_or_else(|| {
        anyhow!("the relay needs its configuration: pass --config or set AMUX_CONFIG")
    })
}

fn pem(env: &str, configured: Option<&Path>, what: &str) -> Result<Vec<u8>> {
    let path = std::env::var_os(env)
        .map(PathBuf::from)
        .or_else(|| configured.map(Path::to_owned))
        .ok_or_else(|| anyhow!("the relay needs its TLS {what}: set {env}"))?;
    std::fs::read(&path).with_context(|| format!("reading the TLS {what} {}", path.display()))
}

/// The log file: `AMUX_LOG`, else the per-user state directory.
fn log_path() -> PathBuf {
    if let Some(path) = std::env::var_os(LOG_ENV) {
        return PathBuf::from(path);
    }
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir);
    state.join("amux").join("amux.log")
}

pub fn run(config: Option<&Path>) -> Result<()> {
    let path = config_path(config)?;
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let config: RelayConfig =
        serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if config.host_name.is_empty() {
        bail!("{}: host_name must name the relay", path.display());
    }
    let cert = pem(CERT_ENV, config.tls_cert.as_deref(), "certificate")?;
    let key = pem(KEY_ENV, config.tls_key.as_deref(), "key")?;
    crate::server::log_to(&log_path())?;
    let reports = reports_dir(&path, config.reports_dir.as_deref());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(config, reports, cert, key))
}

/// Where the relay writes the dumps its core asks for: the configured
/// directory, else `reports` beside the configuration file.
fn reports_dir(config: &Path, configured: Option<&Path>) -> PathBuf {
    configured.map_or_else(
        || {
            config
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("reports")
        },
        Path::to_owned,
    )
}

async fn serve(config: RelayConfig, reports: PathBuf, cert: Vec<u8>, key: Vec<u8>) -> Result<()> {
    let tls = node::create_tls_acceptor(&cert, &key).context("loading the TLS certificate")?;
    let quic =
        node::relay_quic_server_config(&cert, &key).context("loading the TLS certificate")?;
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
    let endpoint = quinn::Endpoint::server(quic, udp_addr)
        .with_context(|| format!("listening on UDP {udp_addr}"))?;
    tracing::info!(addr = %udp_addr, "listening for cloud QUIC carriers");
    println!(
        "amux relay {} listening on TCP {tcp_addr} and UDP {udp_addr}, tokens from {}",
        config.host_name, config.cloud_url
    );
    let tcp_task = server.serve_on_tls_tcp_listener(listener, tls, HANDSHAKE_TIMEOUT);
    let quic_task = server.serve_on_quic_endpoint(endpoint.clone(), HANDSHAKE_TIMEOUT);
    crate::server::terminated().await;
    tracing::info!("relay stopping");
    server
        .send_link_close_to_all(wire::pb::LinkCloseReason::UserShutdown)
        .await;
    tokio::time::sleep(CLOSE_FLUSH).await;
    endpoint.close(quinn::VarInt::from_u32(0), b"relay shutting down");
    tcp_task.abort();
    quic_task.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dumps_go_beside_the_configuration_unless_it_names_a_directory() {
        let config = Path::new("/etc/amux/relay.yaml");
        assert_eq!(
            reports_dir(config, None),
            Path::new("/etc/amux").join("reports")
        );
        let named = Path::new("/var/lib/amux-relay/reports");
        assert_eq!(reports_dir(config, Some(named)), named);
    }
}
