//! The cloud relay the amux binary serves: started the way its deployment
//! starts it, with a self-signed certificate, beside a stand-in cloud that
//! signs connection tokens; a daemon signed in to that cloud links to it.

#![cfg(unix)]

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use ring::rand::SystemRandom;
use ring::signature::{KeyPair as _, RSA_PKCS1_SHA256, RsaKeyPair, RsaPublicKeyComponents};
use support::desk::{Desk, GRACE_SECS};
use support::{amux_binary, grpc_channel, until};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use wire::installation_service_client::InstallationServiceClient;
use wire::{BindProfileRequest, RelayCarrier};

use crate::support;

/// The id the stand-in cloud's key set names its signing key by.
const KID: &str = "relay-test-key";
include!("../../node/src/auth/test_signing_key.rs");
/// The name the relay's certificate is issued to and tokens are minted for.
const RELAY_HOST: &str = "localhost";
const REFRESH: &str = "refresh-alice";
/// The account's user id: the relay routes by it.
const USER: &str = "5d3f5c1e-6a57-4c38-9d0f-2f3c1b1e7a01";

/// The cloud as a signed-in daemon calls it: the refresh grant, who the
/// account is, a connection token for the relay, and the key set the relay
/// checks that token against.
struct Cloud {
    addr: SocketAddr,
    relay_port: u16,
    /// Every path asked for, in order.
    asked: Mutex<Vec<String>>,
}

impl Cloud {
    async fn serve(relay_port: u16) -> Arc<Cloud> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let cloud = Arc::new(Cloud {
            addr: listener.local_addr().unwrap(),
            relay_port,
            asked: Mutex::new(Vec::new()),
        });
        let serving = cloud.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let cloud = serving.clone();
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
        });
        cloud
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    async fn answer(&self, request: Request<Incoming>) -> Response<Full<Bytes>> {
        let path = request.uri().path().to_owned();
        self.asked.lock().unwrap().push(path.clone());
        let bearer = request
            .headers()
            .get(hyper::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::to_owned);
        let body = request.into_body().collect().await.unwrap().to_bytes();
        let form: HashMap<String, String> = String::from_utf8_lossy(&body)
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        let signed_in = bearer.as_deref() == Some("access-alice");
        match path.as_str() {
            "/.well-known/openid-configuration/jwks" => json(
                StatusCode::OK,
                serde_json::json!({"keys": [{"kid": KID, "kty": "RSA", "alg": "RS256", "use": "sig", "n": public_key("n"), "e": public_key("e")}]}),
            ),
            "/connect/token" if form.get("refresh_token").map(String::as_str) == Some(REFRESH) => {
                json(
                    StatusCode::OK,
                    serde_json::json!({"access_token": "access-alice", "refresh_token": REFRESH, "token_type": "Bearer", "expires_in": 3600}),
                )
            }
            "/connect/userinfo" if signed_in => json(
                StatusCode::OK,
                serde_json::json!({"sub": USER, "name": "Alice", "email": "alice@example.test"}),
            ),
            "/api/connect" if signed_in => json(
                StatusCode::OK,
                serde_json::json!({
                    "host": RELAY_HOST,
                    "port": self.relay_port,
                    "token": connection_token(&self.url(), self.relay_port),
                    "expires_at": "2099-01-01T00:00:00Z",
                    "tier": "pro",
                }),
            ),
            _ => json(
                StatusCode::UNAUTHORIZED,
                serde_json::json!({"error": "invalid_grant"}),
            ),
        }
    }
}

fn json(status: StatusCode, body: serde_json::Value) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap()
}

/// The stand-in cloud's signing key.
fn signing_key() -> RsaKeyPair {
    let body: String = CLOUD_SIGNING_KEY
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    RsaKeyPair::from_pkcs8(&STANDARD.decode(body).unwrap()).unwrap()
}

/// One component of the signing key's public half, as its key set names
/// it: `n`, the modulus, or `e`, the exponent.
fn public_key(component: &str) -> String {
    let public: RsaPublicKeyComponents<Vec<u8>> = signing_key().public_key().into();
    URL_SAFE_NO_PAD.encode(if component == "n" { public.n } else { public.e })
}

/// A connection token for the relay at `RELAY_HOST:port`, as the cloud at
/// `issuer` mints one: RS256, by the key its key set names.
fn connection_token(issuer: &str, port: u16) -> String {
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let header = serde_json::json!({"alg": "RS256", "typ": "JWT", "kid": KID});
    let claims = serde_json::json!({
        "sub": USER,
        "client_id": "relay-test",
        "host": RELAY_HOST,
        "port": port,
        "exp": exp,
        "iss": issuer,
        "aud": "amux_token",
        "tier": "pro",
    });
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let key = signing_key();
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        input.as_bytes(),
        &mut signature,
    )
    .unwrap();
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
}

/// A port free on TCP and UDP both, for the relay's two listeners.
fn free_port() -> u16 {
    loop {
        let tcp = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = tcp.local_addr().unwrap().port();
        if std::net::UdpSocket::bind(("0.0.0.0", port)).is_ok() {
            return port;
        }
    }
}

/// The daemon dials TLS too once QUIC has had a moment, and on a busy
/// machine that dial can win; here it fails, so the link says QUIC works.
/// Which carrier wins the race is the race's own unit tests'.
#[tokio::test(flavor = "multi_thread")]
async fn a_signed_in_daemon_links_to_the_relay_over_quic() {
    assert_eq!(link_through_relay(true).await, RelayCarrier::Quic);
}

/// With no QUIC listener where the token points, the daemon's fallback
/// dials the relay's TLS-over-TCP carrier.
#[tokio::test(flavor = "multi_thread")]
async fn a_signed_in_daemon_links_to_the_relay_over_tls_when_quic_is_away() {
    assert_eq!(link_through_relay(false).await, RelayCarrier::Tcp);
}

/// Starts the relay as its deployment does, signs a daemon in to the
/// stand-in cloud, waits for its cloud link to connect through the relay,
/// and says which carrier it took. `quic` puts the relay's QUIC listener on
/// the port tokens name and has the daemon's TLS dial fail; otherwise the
/// QUIC listener is on another port.
async fn link_through_relay(quic: bool) -> RelayCarrier {
    let dir = tempfile::tempdir().unwrap();
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec![RELAY_HOST.into()]).unwrap();
    let cert_path = dir.path().join("fullchain.pem");
    let key_path = dir.path().join("privkey.pem");
    std::fs::write(&cert_path, cert.pem()).unwrap();
    std::fs::write(&key_path, signing_key.serialize_pem()).unwrap();

    let port = free_port();
    // A port the probe let go of can come straight back from it, so the
    // QUIC listener's own port is taken until it is another one.
    let udp_port = if quic {
        port
    } else {
        std::iter::repeat_with(free_port)
            .find(|other| *other != port)
            .unwrap()
    };
    let cloud = Cloud::serve(port).await;
    // The deployment's configuration and command line, word for word.
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "host_name = \"{RELAY_HOST}\"\ntcp_port = {port}\nudp_port = {udp_port}\n\
             cloud_url = \"{}\"\ntls_cert = \"fullchain.pem\"\ntls_key = \"privkey.pem\"\n\
             state_dir = \"state\"\n",
            cloud.url()
        ),
    )
    .unwrap();
    let mut relay = tokio::process::Command::new(amux_binary())
        .arg("relay")
        .env("AMUX_CONFIG", &config)
        .env("AMUX_LOG", dir.path().join("relay.log"))
        .env_remove("STATE_DIRECTORY")
        .env("HOME", dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the relay starts");
    let mut said = BufReader::new(relay.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(Duration::from_secs(30), said.next_line())
        .await
        .expect("the relay says it is listening")
        .unwrap()
        .expect("a line");
    println!("{line}");
    assert!(line.contains(&format!("TCP 0.0.0.0:{port}")), "{line}");

    let desk = Desk::new(
        false,
        GRACE_SECS,
        node::version(),
        "http://127.0.0.1:1",
        vec![],
    );
    let mut start = desk.command(&["server", "start"]);
    start.env("AMUX_CLOUD_TLS_CA", &cert_path);
    if quic {
        start.env("AMUX_TEST_RELAY_TCP_BLOCKED", "1");
    }
    let started = start.output().await.unwrap();
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let mut profiles = InstallationServiceClient::new(grpc_channel(&desk.socket).await.unwrap());
    let profile = profiles
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .unwrap()
        .into_inner()
        .profiles
        .remove(0);
    profiles
        .bind_profile(BindProfileRequest {
            profile_id: Some(profile.id.clone()),
            cloud_url: cloud.url(),
            staged_refresh_token: REFRESH.into(),
            adopt_non_pristine: true,
            client_id: String::new(),
        })
        .await
        .expect("the profile signs in");

    until("the daemon's cloud link to reach the relay", || {
        let mut profiles = profiles.clone();
        let profile = &profile;
        async move {
            let listed = profiles
                .list_profiles(wire::ListProfilesRequest {})
                .await
                .unwrap()
                .into_inner()
                .profiles;
            let listed = listed.iter().find(|p| p.id == profile.id);
            listed
                .and_then(wire::Profile::relay_connected)
                .map(|_| ())
                .ok_or_else(|| format!("cloud {:?}", listed.map(|p| &p.cloud)))
        }
    })
    .await
    .unwrap();
    let linked = profiles
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .unwrap()
        .into_inner()
        .profiles
        .into_iter()
        .find(|p| p.id == profile.id)
        .unwrap();
    println!("profile {} {:?}", linked.label, linked.cloud);
    // The relay checked the token against the cloud's keys: it fetched them.
    assert!(
        cloud
            .asked
            .lock()
            .unwrap()
            .iter()
            .any(|path| path == "/.well-known/openid-configuration/jwks"),
        "the relay never fetched the cloud's signing keys"
    );
    drop(desk);
    relay.kill().await.unwrap();
    linked
        .relay_connected()
        .expect("the profile's relay link is connected")
        .carrier()
}
