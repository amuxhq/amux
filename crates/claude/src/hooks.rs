//! Carrying terminal Claude's hook payloads to the agent process: the hook
//! command forwards its stdin over a per-session socket and prints what the
//! agent answers, which is how amux decides a permission request or a tool
//! server's form. The agent closes the connection at once for a hook it
//! does not answer, so Claude waits on no hook longer than it takes to
//! deliver it.
//!
//! The payloads themselves are [`claude_protocol::hooks::Payload`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const FORWARD_ENVELOPE_FIELD: &str = "amux_hook_forward";

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
    #[error("forwarded hook envelope omitted `{0}`")]
    Missing(&'static str),
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
    let payload = envelope
        .get("payload")
        .ok_or(ForwardedError::Missing("amux_hook_forward.payload"))?;
    let messaging = envelope
        .get("messaging")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
    Ok((serde_json::to_vec(payload)?, messaging))
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
    use claude_protocol::hooks::{self, Payload};

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

    #[test]
    fn forwarded_messaging_credentials_stay_out_of_the_payload() {
        use std::io::Read as _;

        let dir = tempfile::Builder::new()
            .prefix("ch")
            .tempdir_in("/tmp")
            .unwrap();
        let path = dir.path().join("h.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let messaging = MessagingCredentials {
            socket_path: PathBuf::from("/runtime/claude.sock"),
            token: "secret".to_string(),
        };
        // The forward waits for the receiving end to close the connection.
        let forwarded =
            std::thread::spawn(move || forward_with_messaging(&session_start(), &path, &messaging));
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        drop(stream);

        let (payload, credentials) = unwrap_forwarded(&bytes).unwrap();
        let received = hooks::decode(&payload).unwrap();
        assert!(matches!(received, Payload::SessionStart(_)));
        assert_eq!(credentials.unwrap().token, "secret");
        let encoded = String::from_utf8(hooks::encode(&received)).unwrap();
        assert!(!encoded.contains(FORWARD_ENVELOPE_FIELD));
        assert!(!encoded.contains("secret"));
        assert_eq!(forwarded.join().unwrap().unwrap(), Vec::<u8>::new());
    }
}
