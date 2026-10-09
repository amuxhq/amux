//! The cloud relay the amux binary serves: started the way its deployment
//! starts it, with a self-signed certificate, beside a stand-in cloud that
//! signs connection tokens; a daemon signed in to that cloud links to it.

#![cfg(unix)]

mod support;

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use support::desk::{Desk, GRACE_SECS};
use support::{amux_binary, grpc_channel, until};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use wire::profile_service_client::ProfileServiceClient;
use wire::{BindProfileRequest, Empty, Observed, RelayCarrier};

/// The key the stand-in cloud signs connection tokens with, and the same
/// key's public modulus in its key set.
const KID: &str = "relay-test-key";
const PRIVATE_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQDa10VP9rc+oAG3
+JhkaPK/OJSo2y00s5pUobICMpfWApDpnoEsPJf/3yvRvlIJnQMK+eQNtxSdngFP
1O3P6vgpL0MkB7CAOxbe2WB4LFZ0wHuQzxyO0Bv9YDLvZidNg7FKyxhnARyVK0m0
OFwvW5dn/L6POAxEouadWWHbeyDem1BsOcEAT2spQzqeVKZc2VlZJ2FO/CapGYvi
p7bMOAMbiIPdklfTFRj1eGA4BlLlrw645YEvlo0fCMrVZNYb+sGI1SImVfoToaxl
dcT1lagnKE1+ERL8jPqLcbx66jIOq/Nf5hRJOHuayfjz2uUqfYduumKu5NlryBv+
OiVeQmm3AgMBAAECggEARcoJDKs9XPdiFO1ui/b8EwdUQVVEYV41hW/beN/xlApV
dGtb/mOEhdECBG2RdAdihQmUNNuB85IEERVyka/5XAj6fG8HVp2BeagRH8HkAG+x
+EhUbybnBjK7i6UkO5AX5iZGrfKoztlzM8oVe/TVoA/2JW5WWz0oFl3+2yO1I8gE
4qTcP+iNFgNa2SDu0ALjiDgVUDhap+Rs4R9qd5mxswdGYUfD9oqBcouxGZVPgv6n
Xe66iowrnWfc25bD3swXPmsTBF1lncGPuSHrVwPYBFLb6rSbjtOh8aJf61qqRO/s
w44UIcOAhZ15Qv5I1rbbPHoDiRK1a1VUEpPyWxZ/IQKBgQD8YwovOoSRSnzVorIa
RlstKai7iOE6cFkrEVQUvojJcUNmfW99cMtCrGDkXQTahnpov7m2qRho/oNCPYpr
tECo8vyiMW857CaLVZVQiHO5PvzkdCKqhdR4CNsDYpXCMqBtL0Qgn6WagsU1Qw4X
uj9wgOxtBHoSgufe8rf1hXyLWwKBgQDd+Uo8gvP+rua4g4zkXZzyeucvQC5KJHqM
8YNPdaZ8+cb72yMIp/p3BqPoj2zzyX+uGW7opEwwQjAO5VG5wh+jW6j09s6e5Zes
3IJ55v5f40ioUkpxPaa3EQOSRQfi9EVRVJv6bPidRVCS4rtQMccB3oCxq+iQ8OCj
QAf7PNwV1QKBgCG9N6pSn1Aw7fk9M6PxjdS+wfC3/qvqQvFP8raHNg//1SvJTvMs
9e8mzhkZGkIAQjLolnIFrt6yT2e2hF+bjB1Jxl4ET8Mlf42W1kwawaWc9v+vSscS
9vFI9cZBEpYQYIPYErptvRynqKdTHHotireGdJSqSYtZ9pdGSTNIMfsLAoGAYSHj
MFOFfZ7/ayJ1lsC4GwtY+r41A1CvJ9nPQggTkICkaDVeQT1wRoFrXCrW3F8CNib+
92JdzIhKC1qhxo2B1rQXXQpbJAEHvCbKGZnRGhiVBMLtvFvkBhu12l3Gs7N8WbiS
gKUKrZdVSNFach82HEVHP3ggTrx5MDamx3O8QvkCgYBytDiq72xVjklJLWOsSbBo
X7zVUE2d5y01FsN30UQ4nNdCGmEdvu206B1n3Clel1kepYd9Mn7JQgzrBQQwz5UP
06MxC3IfJVYcFmiZ7Kb4ggeBW1QbUbbsb2Jbuv7wNoPcIAE2c5PwnmgacnhVB58O
qr8VpwUTpFt0PnPahUNCRw==
-----END PRIVATE KEY-----"#;
const MODULUS: &str = "2tdFT_a3PqABt_iYZGjyvziUqNstNLOaVKGyAjKX1gKQ6Z6BLDyX_98r0b5SCZ0DCvnkDbcUnZ4BT9Ttz-r4KS9DJAewgDsW3tlgeCxWdMB7kM8cjtAb_WAy72YnTYOxSssYZwEclStJtDhcL1uXZ_y-jzgMRKLmnVlh23sg3ptQbDnBAE9rKUM6nlSmXNlZWSdhTvwmqRmL4qe2zDgDG4iD3ZJX0xUY9XhgOAZS5a8OuOWBL5aNHwjK1WTWG_rBiNUiJlX6E6GsZXXE9ZWoJyhNfhES_Iz6i3G8euoyDqvzX-YUSTh7msn489rlKn2HbrpiruTZa8gb_jolXkJptw";
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
                serde_json::json!({"keys": [{"kid": KID, "kty": "RSA", "alg": "RS256", "use": "sig", "n": MODULUS, "e": "AQAB"}]}),
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
                    "token": connection_token(self.relay_port),
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

/// A connection token for the relay at `RELAY_HOST:port`, as the cloud
/// mints one.
fn connection_token(port: u16) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(KID.into());
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    jsonwebtoken::encode(
        &header,
        &serde_json::json!({
            "sub": USER,
            "client_id": "relay-test",
            "host": RELAY_HOST,
            "port": port,
            "exp": exp,
            "aud": "amux_token",
            "tier": "pro",
        }),
        &EncodingKey::from_rsa_pem(PRIVATE_KEY.as_bytes()).unwrap(),
    )
    .unwrap()
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
/// the port tokens name; otherwise on another one.
async fn link_through_relay(quic: bool) -> RelayCarrier {
    let dir = tempfile::tempdir().unwrap();
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec![RELAY_HOST.into()]).unwrap();
    let cert_path = dir.path().join("fullchain.pem");
    let key_path = dir.path().join("privkey.pem");
    std::fs::write(&cert_path, cert.pem()).unwrap();
    std::fs::write(&key_path, signing_key.serialize_pem()).unwrap();

    let port = free_port();
    let udp_port = if quic { port } else { free_port() };
    let cloud = Cloud::serve(port).await;
    // The deployment's configuration and command line, word for word.
    let config = dir.path().join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "host_name: {RELAY_HOST}\ntcp_port: {port}\nudp_port: {udp_port}\ncloud_url: {}\n",
            cloud.url()
        ),
    )
    .unwrap();
    let mut relay = tokio::process::Command::new(amux_binary())
        .args(["server", "start", "--cloud", "--foreground"])
        .env("AMUX_CONFIG", &config)
        .env("AMUX_TLS_CERT", &cert_path)
        .env("AMUX_TLS_KEY", &key_path)
        .env("AMUX_LOG", dir.path().join("relay.log"))
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
    let started = desk
        .command(&["server", "start"])
        .env("AMUX_CLOUD_TLS_CA", &cert_path)
        .output()
        .await
        .unwrap();
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let mut profiles = ProfileServiceClient::new(grpc_channel(&desk.socket).await.unwrap());
    let profile = profiles
        .list_profiles(Empty {})
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
                .list_profiles(Empty {})
                .await
                .unwrap()
                .into_inner()
                .profiles;
            let observed = listed
                .iter()
                .find(|p| p.id == profile.id)
                .map(|p| p.observed());
            (observed == Some(Observed::Connected))
                .then_some(())
                .ok_or_else(|| format!("observed {observed:?}"))
        }
    })
    .await
    .unwrap();
    let linked = profiles
        .list_profiles(Empty {})
        .await
        .unwrap()
        .into_inner()
        .profiles
        .into_iter()
        .find(|p| p.id == profile.id)
        .unwrap();
    println!(
        "profile {} {:?} over {:?}",
        linked.label,
        linked.observed(),
        linked.relay_carrier()
    );
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
    linked.relay_carrier()
}
