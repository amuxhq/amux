//! Carrying terminal Claude's hook payloads to the agent process: the hook
//! command forwards its stdin over a per-session socket and prints what the
//! agent answers, which is how amux decides a permission request or a tool
//! server's form. The agent closes the connection at once for a hook it
//! does not answer, so Claude waits on no hook longer than it takes to
//! deliver it.
//!
//! The payloads themselves are [`claude_protocol::hooks::Payload`].

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use claude_protocol::hooks;
use claude_protocol::hooks::Payload;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

const FORWARD_ENVELOPE_FIELD: &str = "amux_hook_forward_v1";

/// The hook events whose payload the agent answers: Claude waits on the
/// hook command for its decision.
pub const ANSWERED_EVENTS: &[&str] = &["PermissionRequest", "Elicitation"];

/// The variable Claude's hook command reads to find the socket its hook
/// payloads go to.
pub const HOOK_SOCKET_ENV: &str = "CLAUDE_HOOK_SOCKET";

#[derive(Clone, Deserialize, Serialize)]
pub struct MessagingCredentials {
    pub socket_path: PathBuf,
    pub token: String,
}

impl std::fmt::Debug for MessagingCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessagingCredentials")
            .field("socket_path", &self.socket_path)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ForwardedError {
    #[error("forwarded hook bytes are not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("forwarded hook payload: {0}")]
    Payload(#[from] claude_protocol::DecodeError),
    #[error("forwarded hook envelope omitted `{0}`")]
    Missing(&'static str),
    #[error("forwarded hook envelope field `{0}` had the wrong shape")]
    Invalid(&'static str),
}

/// The variables Claude sets on a hook command for its messaging socket.
const MESSAGING_SOCKET_ENV: &str = "CLAUDE_CODE_MESSAGING_SOCKET";
const MESSAGING_TOKEN_ENV: &str = "CLAUDE_CODE_MESSAGING_TOKEN";

/// Splits a forwarded connection's bytes into the hook payload Claude wrote
/// and the messaging credentials the forwarder added, if any. A payload
/// that arrived without the envelope is returned as it came.
pub fn unwrap_forwarded(
    bytes: &[u8],
) -> Result<(Vec<u8>, Option<MessagingCredentials>), ForwardedError> {
    let raw: Value = serde_json::from_slice(bytes)?;
    let Some(envelope) = raw.get(FORWARD_ENVELOPE_FIELD) else {
        return Ok((bytes.to_vec(), None));
    };
    if envelope.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(ForwardedError::Invalid("amux_hook_forward_v1.version"));
    }
    let payload = envelope
        .get("payload")
        .ok_or(ForwardedError::Missing("amux_hook_forward_v1.payload"))?;
    let messaging = envelope
        .get("messaging")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
    Ok((serde_json::to_vec(payload)?, messaging))
}

/// The hook payload in a forwarded connection's bytes, without the
/// messaging credentials the forwarder may have added.
fn parse_forwarded(bytes: &[u8]) -> Result<Payload, ForwardedError> {
    let (payload, _messaging) = unwrap_forwarded(bytes)?;
    Ok(hooks::decode(&payload)?)
}

/// A per-session Unix socket receiving forwarded hook stdin.
pub struct HookReceiver {
    pub path: PathBuf,
    payloads: Mutex<Option<mpsc::Receiver<Payload>>>,
    task: tokio::task::JoinHandle<()>,
}

impl HookReceiver {
    #[cfg(unix)]
    pub fn bind_sync(dir: &Path) -> Result<Self, std::io::Error> {
        use tokio::io::AsyncReadExt;
        use tokio::net::UnixListener;

        std::fs::create_dir_all(dir)?;
        let nonce = Uuid::new_v4().simple().to_string();
        let path = dir.join(format!("h-{}.sock", &nonce[..12]));
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        let listener = UnixListener::from_std(listener)?;
        let (tx, rx) = mpsc::channel(64);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut bytes = Vec::new();
                if stream.read_to_end(&mut bytes).await.is_ok()
                    && let Ok(payload) = parse_forwarded(&bytes)
                    && tx.send(payload).await.is_err()
                {
                    break;
                }
            }
        });
        Ok(Self {
            path,
            payloads: Mutex::new(Some(rx)),
            task,
        })
    }

    #[cfg(not(unix))]
    pub fn bind_sync(_dir: &Path) -> Result<Self, std::io::Error> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Claude hook sockets require Unix",
        ))
    }

    pub fn payloads(&self) -> mpsc::Receiver<Payload> {
        self.payloads
            .lock()
            .expect("hook receiver mutex poisoned")
            .take()
            .expect("hook payload stream already taken")
    }
}

impl Drop for HookReceiver {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Forward hook stdin to a session socket and wait for what the agent
/// answers, which is empty unless it decides the hook.
#[cfg(unix)]
pub fn forward(stdin: &[u8], socket: &Path) -> Result<Vec<u8>, std::io::Error> {
    use std::io::{Read, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    stream.write_all(stdin)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer)?;
    Ok(answer)
}

/// Forward hook stdin and the provider's per-session messaging credentials.
///
/// [`HookReceiver`] and [`unwrap_forwarded`] remove the transport envelope,
/// so the credential token never reaches the payload.
#[cfg(unix)]
pub fn forward_with_messaging(
    stdin: &[u8],
    socket: &Path,
    messaging: &MessagingCredentials,
) -> Result<Vec<u8>, std::io::Error> {
    let payload: Value = serde_json::from_slice(stdin)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let envelope = serde_json::json!({
        FORWARD_ENVELOPE_FIELD: {
            "version": 1,
            "payload": payload,
            "messaging": messaging,
        }
    });
    let encoded = serde_json::to_vec(&envelope)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    forward(&encoded, socket)
}

#[cfg(not(unix))]
pub fn forward_with_messaging(
    _stdin: &[u8],
    _socket: &Path,
    _messaging: &MessagingCredentials,
) -> Result<Vec<u8>, std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Claude hook sockets require Unix",
    ))
}

/// What a hook command does: forward its stdin to `socket`, with the
/// messaging credentials Claude put in its environment when there are any,
/// and return the agent's answer for the command to print.
pub fn forward_from_env(stdin: &[u8], socket: &Path) -> Result<Vec<u8>, std::io::Error> {
    let socket_path = std::env::var_os(MESSAGING_SOCKET_ENV);
    let token = std::env::var(MESSAGING_TOKEN_ENV).ok();
    match (socket_path, token) {
        (Some(socket_path), Some(token)) if !token.is_empty() => forward_with_messaging(
            stdin,
            socket,
            &MessagingCredentials {
                socket_path: PathBuf::from(socket_path),
                token,
            },
        ),
        _ => forward(stdin, socket),
    }
}

#[cfg(not(unix))]
pub fn forward(_stdin: &[u8], _socket: &Path) -> Result<Vec<u8>, std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Claude hook sockets require Unix",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn session_start() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp",
            "source": "startup"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn receiver_keeps_messaging_credentials_out_of_the_payload() {
        let dir = tempfile::Builder::new()
            .prefix("ch")
            .tempdir_in("/tmp")
            .unwrap();
        let receiver = HookReceiver::bind_sync(dir.path()).unwrap();
        let mut payloads = receiver.payloads();
        let messaging = MessagingCredentials {
            socket_path: PathBuf::from("/runtime/claude.sock"),
            token: "secret".to_string(),
        };

        // The forward waits for the receiver to close the connection, which
        // runs on this test's one thread.
        let path = receiver.path.clone();
        let forwarded = tokio::task::spawn_blocking(move || {
            forward_with_messaging(&session_start(), &path, &messaging)
        });
        let received = tokio::time::timeout(std::time::Duration::from_secs(1), payloads.recv())
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(received, Payload::SessionStart(_)));
        let encoded = String::from_utf8(hooks::encode(&received)).unwrap();
        assert!(!encoded.contains(FORWARD_ENVELOPE_FIELD));
        assert!(!encoded.contains("secret"));
        assert!(!format!("{received:?}").contains("secret"));
        assert_eq!(forwarded.await.unwrap().unwrap(), Vec::<u8>::new());
    }
}
