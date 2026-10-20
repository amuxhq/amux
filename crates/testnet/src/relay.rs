//! The relay a topology declares, and the cloud that hands out its
//! credentials.
//!
//! One [`Relay`] is a production [`CloudLinkServer`] serving its QUIC
//! carrier and its TCP fallback on loopback, beside a stand-in for the
//! cloud: the OAuth token and userinfo endpoints a sign-in calls, the
//! connect call that mints a relay credential for the account's current
//! tier, and the plan read a phone makes (`/api/graphql`). Credentials
//! expire on the net's policy clock. Each host dials the relay's QUIC
//! carrier through its own [`UdpGate`], so a test can take UDP away from
//! one host and give it back.

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use agent_dir::Clock;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use node::harness::{
    AuthenticatedLinkUser, LinkTokenAuthenticator, Tier, relay_quic_client_config_with_roots,
    relay_quic_server_config_from_der,
};
use node::{CloudLinkServer, CloudOptions, RelayIdentity, RelayQuic};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::gate::UdpGate;
use crate::topology::{RelayDecl, TierDecl};

/// The name the relay's certificate is issued to, and the host the cloud
/// names for it.
pub const RELAY_HOST: &str = "localhost";
/// How long a relay credential lives on the policy clock.
pub const CREDENTIAL_TTL: Duration = Duration::from_secs(600);
/// How long the relay gives a QUIC handshake.
const QUIC_HANDSHAKE: Duration = Duration::from_secs(10);

/// One account the cloud knows.
#[derive(Clone, Debug)]
struct Account {
    user: Uuid,
    tier: Tier,
    /// When it last went from Pro to Free, on the policy clock: the moment
    /// its access ended, as the plan read says.
    lapsed_ms: Option<i64>,
    /// Signing in and refreshing fail as for a revoked login.
    revoked: bool,
}

/// A relay credential the cloud minted.
#[derive(Clone, Debug)]
struct Minted {
    user: Uuid,
    expires_ms: i64,
    tier: Tier,
}

#[derive(Default)]
struct CloudState {
    accounts: BTreeMap<String, Account>,
    minted: HashMap<String, Minted>,
    /// Every connect call, by account, in order.
    connects: Vec<String>,
    /// Every credential the relay was shown, in order.
    presented: Vec<String>,
    /// The OAuth client each refresh named, in order.
    refreshed_as: Vec<String>,
    /// How far the relay's clock runs ahead of the hosts'.
    ahead: Duration,
}

struct Cloud {
    state: Mutex<CloudState>,
    clock: Arc<dyn Clock>,
    quic_port: u16,
}

pub struct Relay {
    server: CloudLinkServer,
    host_id: Uuid,
    url: String,
    tcp: SocketAddr,
    quic: SocketAddr,
    quic_client: quinn::ClientConfig,
    /// The relay's self-signed certificate, DER, for a client outside the
    /// net to trust its QUIC carrier by.
    quic_root: Vec<u8>,
    /// The same certificate as PEM, for a daemon process to trust.
    quic_root_pem: String,
    quic_endpoint: quinn::Endpoint,
    cloud: Arc<Cloud>,
    tasks: Vec<JoinHandle<()>>,
}

impl Relay {
    /// Starts the relay and the cloud for `decl`'s accounts, their
    /// credentials timed by `clock`.
    pub async fn start(decl: &RelayDecl, clock: Arc<dyn Clock>) -> std::io::Result<Self> {
        let (quic_server, quic_client, quic_root, quic_root_pem) = quic_configs();
        let quic_endpoint =
            quinn::Endpoint::server(quic_server, SocketAddr::from(([127, 0, 0, 1], 0)))?;
        let quic = quic_endpoint.local_addr()?;
        let mut state = CloudState::default();
        for account in &decl.accounts {
            state.accounts.insert(
                account.name.clone(),
                Account {
                    user: Uuid::new_v4(),
                    tier: account.tier.into(),
                    lapsed_ms: None,
                    revoked: false,
                },
            );
        }
        let cloud = Arc::new(Cloud {
            state: Mutex::new(state),
            clock: clock.clone(),
            quic_port: quic.port(),
        });
        let host_id = Uuid::new_v4();
        let server = CloudLinkServer::new(
            RelayIdentity {
                host_id,
                name: "relay".to_owned(),
                authenticator: Arc::new(Presented(cloud.clone())),
                clock,
            },
            // No story reads a dump the relay's core asks for.
            None,
        );
        let tcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let tcp = tcp_listener.local_addr()?;
        let http = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", http.local_addr()?);
        let tasks = vec![
            server.serve_on_tcp_listener(tcp_listener),
            server.serve_on_quic_endpoint(quic_endpoint.clone(), QUIC_HANDSHAKE),
            tokio::spawn(serve_cloud(http, cloud.clone())),
        ];
        Ok(Self {
            server,
            host_id,
            url,
            tcp,
            quic,
            quic_client,
            quic_root,
            quic_root_pem,
            quic_endpoint,
            cloud,
            tasks,
        })
    }

    /// The production relay server, for what it can say about its links.
    pub fn server(&self) -> &CloudLinkServer {
        &self.server
    }

    /// The relay's own host id: what a stream addressed to the relay names.
    pub fn host_id(&self) -> Uuid {
        self.host_id
    }

    /// The cloud a sign-in names.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The relay's plaintext TCP carrier, which a client outside the net
    /// dials instead of where the account service names it.
    pub fn tcp(&self) -> SocketAddr {
        self.tcp
    }

    /// The relay's QUIC carrier, which a client outside the net dials
    /// instead of where the account service names it, trusting
    /// [`Relay::quic_root`].
    pub fn quic(&self) -> SocketAddr {
        self.quic
    }

    /// The relay's self-signed certificate, DER.
    pub fn quic_root(&self) -> &[u8] {
        &self.quic_root
    }

    /// The relay's self-signed certificate, PEM: what a debug daemon process
    /// is told to trust (`AMUX_CLOUD_TLS_CA`) to reach the relay where the
    /// account service names it.
    pub fn quic_root_pem(&self) -> &str {
        &self.quic_root_pem
    }

    /// The refresh token a person signing in as `account` hands the daemon.
    pub fn login(account: &str) -> String {
        format!("refresh-{account}")
    }

    /// What the relay knows `account` by.
    pub fn user(&self, account: &str) -> Option<Uuid> {
        self.cloud
            .state
            .lock()
            .unwrap()
            .accounts
            .get(account)
            .map(|known| known.user)
    }

    /// Adds an account the net did not declare.
    pub fn add_account(&self, account: &str, tier: Tier) -> Uuid {
        let user = Uuid::new_v4();
        self.cloud.state.lock().unwrap().accounts.insert(
            account.to_owned(),
            Account {
                user,
                tier,
                lapsed_ms: None,
                revoked: false,
            },
        );
        user
    }

    /// What `account` buys from its next credential and its next plan
    /// read on.
    pub fn set_tier(&self, account: &str, tier: Tier) {
        let now = self.cloud.clock.now_ms();
        if let Some(known) = self.cloud.state.lock().unwrap().accounts.get_mut(account) {
            known.lapsed_ms = match (known.tier, tier) {
                (Tier::Pro, Tier::Free) => Some(now),
                (_, Tier::Pro) => None,
                _ => known.lapsed_ms,
            };
            known.tier = tier;
        }
    }

    /// What `account` buys now.
    pub fn tier(&self, account: &str) -> Option<Tier> {
        self.cloud
            .state
            .lock()
            .unwrap()
            .accounts
            .get(account)
            .map(|known| known.tier)
    }

    /// Revokes `account`'s login: the token endpoint refuses its refresh
    /// token and the connect call its access token, as for a person who
    /// signed out everywhere.
    pub fn revoke(&self, account: &str) {
        if let Some(known) = self.cloud.state.lock().unwrap().accounts.get_mut(account) {
            known.revoked = true;
        }
    }

    /// Runs the relay's clock `ahead` of the hosts': each credential minted
    /// from now on lapses at the relay that much sooner than the expiry its
    /// host is told, as for a host whose clock is behind.
    pub fn run_ahead(&self, ahead: Duration) {
        self.cloud.state.lock().unwrap().ahead = ahead;
    }

    /// Every connect call so far, by account.
    pub fn connects(&self) -> Vec<String> {
        self.cloud.state.lock().unwrap().connects.clone()
    }

    /// The OAuth client each refresh-token exchange named, in order.
    pub fn refreshed_as(&self) -> Vec<String> {
        self.cloud.state.lock().unwrap().refreshed_as.clone()
    }

    /// Every credential the relay was shown so far, in Hello or Reauth.
    pub fn presented(&self) -> Vec<String> {
        self.cloud.state.lock().unwrap().presented.clone()
    }

    /// Which hosts `account` holds relay links from, and how many each.
    pub async fn links(&self, account: &str) -> Vec<(Uuid, usize)> {
        match self.user(account) {
            Some(user) => self.server.user_links(user).await,
            None => Vec::new(),
        }
    }

    /// The cloud options a host dials this relay with: TCP straight to the
    /// relay, QUIC through `gate`.
    pub fn cloud_options(&self, gate: &UdpGate) -> CloudOptions {
        CloudOptions {
            relay_tcp: Some(self.tcp),
            relay_quic: Some(RelayQuic {
                addr: gate.addr(),
                client: self.quic_client.clone(),
            }),
            ..CloudOptions::default()
        }
    }

    /// A gate in front of the relay's QUIC carrier.
    pub async fn gate(&self) -> std::io::Result<UdpGate> {
        UdpGate::start(self.quic).await
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.quic_endpoint
            .close(quinn::VarInt::from_u32(0), b"relay stopping");
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl From<TierDecl> for Tier {
    fn from(tier: TierDecl) -> Self {
        match tier {
            TierDecl::Pro => Tier::Pro,
            TierDecl::Free => Tier::Free,
        }
    }
}

/// The relay's check of a credential: one the cloud minted, until its
/// expiry.
struct Presented(Arc<Cloud>);

#[tonic::async_trait]
impl LinkTokenAuthenticator for Presented {
    async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<AuthenticatedLinkUser, tonic::Status> {
        let mut state = self.0.state.lock().unwrap();
        state.presented.push(token.to_owned());
        let minted = state
            .minted
            .get(token)
            .cloned()
            .ok_or_else(|| tonic::Status::unauthenticated("unknown credential"))?;
        Ok(AuthenticatedLinkUser {
            user_id: minted.user,
            client_id: "cli".to_owned(),
            expires_at: SystemTime::UNIX_EPOCH + Duration::from_millis(minted.expires_ms as u64),
            tier: minted.tier,
        })
    }
}

async fn serve_cloud(listener: tokio::net::TcpListener, cloud: Arc<Cloud>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let cloud = cloud.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let cloud = cloud.clone();
                async move { Ok::<_, Infallible>(cloud.answer(request).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}

impl Cloud {
    async fn answer(&self, request: hyper::Request<Incoming>) -> hyper::Response<Full<Bytes>> {
        let path = request.uri().path().to_owned();
        let bearer = request
            .headers()
            .get(hyper::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .and_then(|token| token.strip_prefix("access-"))
            .map(str::to_owned);
        let body = request
            .into_body()
            .collect()
            .await
            .map(|body| body.to_bytes())
            .unwrap_or_default();
        let form = String::from_utf8_lossy(&body).into_owned();
        match path.as_str() {
            "/connect/token" => {
                let client = form
                    .split('&')
                    .find_map(|pair| pair.strip_prefix("client_id="))
                    .unwrap_or_default()
                    .to_owned();
                self.state.lock().unwrap().refreshed_as.push(client);
                let account = form
                    .split('&')
                    .find_map(|pair| pair.strip_prefix("refresh_token="))
                    .and_then(|token| token.strip_prefix("refresh-"))
                    .filter(|account| self.signed_in(account));
                match account {
                    Some(account) => json(serde_json::json!({
                        "access_token": format!("access-{account}"),
                        "token_type": "bearer",
                        "expires_in": 3600,
                        "refresh_token": format!("refresh-{account}"),
                    })),
                    None => status(400, serde_json::json!({ "error": "invalid_grant" })),
                }
            }
            "/connect/userinfo" => match bearer.filter(|account| self.signed_in(account)) {
                Some(account) => json(serde_json::json!({
                    "sub": account,
                    "name": account,
                    "email": format!("{account}@example.com"),
                })),
                None => status(401, serde_json::json!({})),
            },
            "/api/connect" => match bearer.filter(|account| self.signed_in(account)) {
                Some(account) => json(self.mint(&account)),
                None => status(401, serde_json::json!({})),
            },
            // The plan read a phone makes. Whatever the query asks, the
            // answer is the account's access: `pro`, when it ended, and why
            // it has it, the way the real account service words it.
            "/api/graphql" => match bearer.filter(|account| self.signed_in(account)) {
                Some(account) => json(serde_json::json!({
                    "data": {"me": {"access": self.access(&account)}},
                })),
                None => status(401, serde_json::json!({})),
            },
            _ => status(404, serde_json::json!({})),
        }
    }

    fn signed_in(&self, account: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .accounts
            .get(account)
            .is_some_and(|known| !known.revoked)
    }

    /// An account's access as the plan read answers it. A Pro account was
    /// given its access (the net knows no purchases); one moved from Pro to
    /// Free had that access end when it moved; one that was always Free
    /// has none.
    fn access(&self, account: &str) -> serde_json::Value {
        let known = self.state.lock().unwrap().accounts[account].clone();
        let at = |ms: i64| {
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
                .expect("a time in range")
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        };
        match (known.tier, known.lapsed_ms) {
            (Tier::Pro, _) => serde_json::json!({
                "pro": true, "until": null, "grant": {"__typename": "Granted"},
            }),
            (Tier::Free, Some(lapsed)) => serde_json::json!({
                "pro": false, "until": at(lapsed), "grant": {"__typename": "Granted"},
            }),
            (Tier::Free, None) => serde_json::json!({"pro": false, "until": null, "grant": null}),
        }
    }

    fn mint(&self, account: &str) -> serde_json::Value {
        let mut state = self.state.lock().unwrap();
        state.connects.push(account.to_owned());
        let known = state.accounts[account].clone();
        let token = format!("relay-{account}-{}", state.connects.len());
        let expires_ms = self.clock.now_ms() + CREDENTIAL_TTL.as_millis() as i64;
        let lapses_ms = expires_ms - state.ahead.as_millis() as i64;
        state.minted.insert(
            token.clone(),
            Minted {
                user: known.user,
                expires_ms: lapses_ms,
                tier: known.tier,
            },
        );
        let expires_at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(expires_ms)
            .expect("a credential expiry in range");
        serde_json::json!({
            "host": RELAY_HOST,
            "port": self.quic_port,
            "token": token,
            "expires_at": expires_at.to_rfc3339(),
            "tier": match known.tier {
                Tier::Pro => "pro",
                Tier::Free => "free",
            },
        })
    }
}

fn json(body: serde_json::Value) -> hyper::Response<Full<Bytes>> {
    status(200, body)
}

fn status(code: u16, body: serde_json::Value) -> hyper::Response<Full<Bytes>> {
    hyper::Response::builder()
        .status(code)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("a response")
}

/// A self-signed certificate for the relay's QUIC carrier, a client
/// configuration that trusts only it, and the certificate itself, as DER
/// and as PEM.
fn quic_configs() -> (quinn::ServerConfig, quinn::ClientConfig, Vec<u8>, String) {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec![RELAY_HOST.to_owned()])
            .expect("a relay certificate");
    let der = rustls::pki_types::CertificateDer::from(cert.der().as_ref().to_vec());
    let server = relay_quic_server_config_from_der(
        vec![der.clone()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
            signing_key.serialize_der(),
        )),
    )
    .expect("the relay's QUIC server configuration");
    let mut roots = rustls::RootCertStore::empty();
    roots.add(der).expect("the relay certificate as a root");
    let client =
        relay_quic_client_config_with_roots(roots).expect("the relay's QUIC client configuration");
    (server, client, cert.der().as_ref().to_vec(), cert.pem())
}
