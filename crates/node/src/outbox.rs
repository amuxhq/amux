//! The two outboxes the daemon drains: deliveries, which tell a parent a
//! turn of its child's ended or the child's incarnation exited, and
//! notifications, which push what an agent wants to the person's devices.
//!
//! The daemon core drains both outboxes: it hands each deliveries row to
//! the parent and asks for each push as a job. What is here is where the
//! push job hands its push, and a way to wait for a deliveries pass.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub use daemon_core::DrainReport;
pub use daemon_core::payload::{Push, PushRecipient};
use uuid::Uuid;

use crate::auth::{AuthError, CredentialProvider};
use crate::runtime::ProfileRuntime;
use crate::seal;

#[derive(Debug, thiserror::Error)]
#[error("sending a push: {0}")]
pub struct PushError(pub String);

pub type PushFuture<'a> = Pin<Box<dyn Future<Output = Result<(), PushError>> + Send + 'a>>;

/// The account service a profile is bound to, and how to speak for it.
#[derive(Clone)]
pub struct PushCloud {
    pub url: String,
    pub credentials: Arc<dyn CredentialProvider>,
}

/// Where pushes go. The daemon never talks to a push service or holds a
/// device token. `cloud` is None for a profile with no signed-in account.
pub trait PushSender: Send + Sync + 'static {
    fn send<'a>(&'a self, push: &'a Push, cloud: Option<PushCloud>) -> PushFuture<'a>;
}

/// Sends nothing: what a host that runs no agents starts with, having
/// nothing to say.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopSender;

impl PushSender for NoopSender {
    fn send<'a>(&'a self, push: &'a Push, _cloud: Option<PushCloud>) -> PushFuture<'a> {
        tracing::debug!(revision = push.revision, "a push with nowhere to go");
        Box::pin(async { Ok(()) })
    }
}

/// How long a push may take. Past it the send fails and is tried again: the
/// row stays marked as being sent until a send ends, so one that hung on a
/// sleeping network would never be retried.
const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Seals each push to every host that wants it and posts them to the
/// account's endpoint, which finds each host's device and calls its platform
/// push service. The endpoint sees only which host gets what ciphertext.
#[derive(Clone, Debug)]
pub struct HttpSender {
    client: reqwest::Client,
}

impl HttpSender {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(SEND_TIMEOUT)
                .build()
                .unwrap_or_default(),
        }
    }
}

impl Default for HttpSender {
    fn default() -> Self {
        Self::new()
    }
}

/// What a request needs beyond the sealed copies: an id that stays the same
/// across retries, so the push service replaces rather than repeats, and that
/// says nothing of the agent.
fn push_id(push: &Push) -> String {
    let mut named = push.agent_id.clone();
    named.extend_from_slice(&push.revision.to_be_bytes());
    let digest = ring::digest::digest(&ring::digest::SHA256, &named);
    digest.as_ref()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn request_body(push: &Push) -> Result<serde_json::Value, PushError> {
    let recipients = push
        .recipients
        .iter()
        .map(|recipient| {
            let host_id = Uuid::from_slice(&recipient.host_id)
                .map_err(|error| PushError(error.to_string()))?;
            let sealed = seal::seal_push(push, &recipient.push_key)
                .map_err(|error| PushError(error.to_string()))?;
            Ok(serde_json::json!({ "host_id": host_id.to_string(), "sealed": sealed }))
        })
        .collect::<Result<Vec<_>, PushError>>()?;
    Ok(serde_json::json!({ "id": push_id(push), "recipients": recipients }))
}

impl PushSender for HttpSender {
    fn send<'a>(&'a self, push: &'a Push, cloud: Option<PushCloud>) -> PushFuture<'a> {
        Box::pin(async move {
            let Some(cloud) = cloud else {
                return Ok(());
            };
            let token = match cloud.credentials.access_token().await {
                Ok(token) => token,
                // Signed out: nobody to push to, now or on a retry.
                Err(AuthError::Unauthenticated | AuthError::AccountMismatch) => return Ok(()),
                Err(error) => return Err(PushError(error.to_string())),
            };
            let response = self
                .client
                .post(format!("{}/api/push", cloud.url.trim_end_matches('/')))
                .bearer_auth(&token.bearer)
                .json(&request_body(push)?)
                .send()
                .await
                .map_err(|error| PushError(error.to_string()))?;
            let status = response.status();
            if status == reqwest::StatusCode::UNAUTHORIZED {
                cloud.credentials.invalidate(&token);
                return Err(PushError("the endpoint refused the token".to_owned()));
            }
            if status.is_server_error() {
                return Err(PushError(format!("the endpoint answered {status}")));
            }
            // Any other refusal, such as an account without Pro, says the
            // same on every retry.
            if !status.is_success() {
                tracing::info!(%status, "the endpoint declined a push");
            }
            Ok(())
        })
    }
}

impl ProfileRuntime {
    /// What a pass over the deliveries outbox delivered, dropped as stale
    /// and kept: the pass under way, else one begun now. The core runs one
    /// whenever a row may have become deliverable; tests call it too.
    pub async fn drain_deliveries(&self) -> DrainReport {
        self.driver().drain().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use prost::Message as _;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    use super::*;
    use crate::auth::AccessToken;

    struct Signed {
        refused: Mutex<u32>,
    }

    #[async_trait]
    impl CredentialProvider for Signed {
        async fn access_token(&self) -> Result<AccessToken, AuthError> {
            Ok(AccessToken {
                bearer: "the-token".to_owned(),
                expires_at: None,
                tier: None,
            })
        }

        fn invalidate(&self, _token: &AccessToken) {
            *self.refused.lock().unwrap() += 1;
        }
    }

    struct SignedOut;

    #[async_trait]
    impl CredentialProvider for SignedOut {
        async fn access_token(&self) -> Result<AccessToken, AuthError> {
            Err(AuthError::Unauthenticated)
        }

        fn invalidate(&self, _token: &AccessToken) {}
    }

    /// Answers one request with `status` and hands back what it was sent.
    async fn serve_once(status: u16) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let serving = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut read = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                let count = stream.read(&mut buffer).await.unwrap();
                read.extend_from_slice(&buffer[..count]);
                let text = String::from_utf8_lossy(&read);
                let Some((head, body)) = text.split_once("\r\n\r\n") else {
                    continue;
                };
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length: ")?
                            .parse()
                            .ok()
                    })
                    .unwrap_or(0_usize);
                if body.len() >= length {
                    break;
                }
            }
            let answer = format!("HTTP/1.1 {status} X\r\ncontent-length: 0\r\n\r\n");
            stream.write_all(answer.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&read).into_owned()
        });
        (url, serving)
    }

    fn push() -> Push {
        Push {
            host_id: vec![1; 16],
            agent_id: vec![2; 16],
            revision: 3,
            name: "asker".to_owned(),
            notice: wire::Notice {
                of: Some(wire::notice::Of::Blocked(wire::BlockedNotice {
                    why: Some(wire::blocked_notice::Why::Failed("stopped".to_owned())),
                })),
            }
            .encode_to_vec(),
            recipients: vec![PushRecipient {
                host_id: vec![4; 16],
                push_key: push_key(),
            }],
        }
    }

    fn push_key() -> Vec<u8> {
        use hpke::{Kem as _, Serializable as _};
        let (_, public) = hpke::kem::X25519HkdfSha256::gen_keypair();
        public.to_bytes().to_vec()
    }

    fn cloud(url: String, credentials: impl CredentialProvider) -> Option<PushCloud> {
        Some(PushCloud {
            url,
            credentials: Arc::new(credentials),
        })
    }

    #[tokio::test]
    async fn a_push_posts_one_sealed_copy_per_recipient_with_the_accounts_token() {
        let (url, serving) = serve_once(202).await;
        let signed = Signed {
            refused: Mutex::new(0),
        };
        let push = push();
        HttpSender::new()
            .send(&push, cloud(url, signed))
            .await
            .unwrap();
        let request = serving.await.unwrap();
        assert!(request.starts_with("POST /api/push "), "{request}");
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer the-token")
        );
        let body: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["id"], push_id(&push));
        assert_eq!(body["id"].as_str().unwrap().len(), 32);
        let recipients = body["recipients"].as_array().unwrap();
        assert_eq!(recipients.len(), 1);
        assert_eq!(
            recipients[0]["host_id"],
            "04040404-0404-0404-0404-040404040404"
        );
        assert!(!request.contains("stopped"), "the text is sealed");
    }

    #[tokio::test]
    async fn a_server_failure_is_retried_and_a_refusal_is_final() {
        let sender = HttpSender::new();
        let signed = || Signed {
            refused: Mutex::new(0),
        };
        let (url, serving) = serve_once(503).await;
        assert!(sender.send(&push(), cloud(url, signed())).await.is_err());
        serving.await.unwrap();
        let (url, serving) = serve_once(403).await;
        assert!(sender.send(&push(), cloud(url, signed())).await.is_ok());
        serving.await.unwrap();
    }

    #[tokio::test]
    async fn a_refused_token_is_given_up_and_the_push_retried() {
        let credentials = Arc::new(Signed {
            refused: Mutex::new(0),
        });
        let (url, serving) = serve_once(401).await;
        let cloud = PushCloud {
            url,
            credentials: credentials.clone(),
        };
        assert!(HttpSender::new().send(&push(), Some(cloud)).await.is_err());
        serving.await.unwrap();
        assert_eq!(*credentials.refused.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn signed_out_there_is_nobody_to_push_to() {
        let sender = HttpSender::new();
        assert!(sender.send(&push(), None).await.is_ok());
        let cloud = cloud("http://127.0.0.1:1".to_owned(), SignedOut);
        assert!(sender.send(&push(), cloud).await.is_ok());
    }
}
