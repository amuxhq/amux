//! Claude Code messaging-socket client: the Unix socket terminal Claude
//! binds, which Windows, hosting no terminal Claude, has none of.

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum MessagingError {
    #[error("Claude messaging socket I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Claude messaging sockets require Unix")]
    Unsupported,
}

pub struct MessagingSocket {
    // out of scope on Windows: terminal Claude, whose messaging socket this is
    #[cfg(not(windows))]
    stream: tokio::net::UnixStream,
}

// Each method's gate returns where terminal Claude is hosted; only Windows
// reaches the `Unsupported` after it.
#[allow(unreachable_code, unused_variables)]
impl MessagingSocket {
    /// Connects to the socket at `path` and authenticates with `token`.
    pub async fn connect(path: &Path, token: &str) -> Result<Self, MessagingError> {
        // out of scope on Windows: terminal Claude
        #[cfg(not(windows))]
        return Ok(Self {
            stream: unix::connect(path, token).await?,
        });
        Err(MessagingError::Unsupported)
    }

    /// Sends `text` as the person's message and ends the connection.
    pub async fn send(&mut self, text: &str) -> Result<(), MessagingError> {
        // out of scope on Windows: terminal Claude
        #[cfg(not(windows))]
        return unix::send(&mut self.stream, text).await;
        Err(MessagingError::Unsupported)
    }
}

// out of scope on Windows: terminal Claude, whose messaging socket this is
#[cfg(not(windows))]
mod unix {
    use std::path::Path;

    use claude_protocol::messaging::{Auth, UserMessage};
    use tokio::io::AsyncWriteExt as _;
    use tokio::net::UnixStream;

    use super::MessagingError;

    pub(super) async fn connect(path: &Path, token: &str) -> Result<UnixStream, MessagingError> {
        let mut stream = UnixStream::connect(path).await?;
        let auth = serde_json::to_vec(&Auth::new(token)).map_err(std::io::Error::other)?;
        stream.write_all(&auth).await?;
        stream.write_all(b"\n").await?;
        Ok(stream)
    }

    pub(super) async fn send(stream: &mut UnixStream, text: &str) -> Result<(), MessagingError> {
        let message = serde_json::to_vec(&UserMessage::new(text)).map_err(std::io::Error::other)?;
        stream.write_all(&message).await?;
        stream.write_all(b"\n").await?;
        stream.shutdown().await?;
        Ok(())
    }
}

// out of scope on Windows: terminal Claude, whose messaging socket this is.
#[cfg(all(test, unix))]
mod tests {
    use tokio::io::AsyncBufReadExt;
    use tokio::net::UnixListener;

    use super::*;

    #[tokio::test]
    async fn sends_auth_then_user_message() {
        let dir = tempfile::Builder::new()
            .prefix("cm")
            .tempdir_in("/tmp")
            .unwrap();
        let path = dir.path().join("message.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut lines = tokio::io::BufReader::new(stream).lines();
            let auth = lines.next_line().await.unwrap().unwrap();
            let message = lines.next_line().await.unwrap().unwrap();
            (auth, message)
        });
        let mut socket = MessagingSocket::connect(&path, "secret").await.unwrap();
        socket.send("hello").await.unwrap();
        let (auth, message) = server.await.unwrap();
        assert_eq!(auth, r#"{"token":"secret","type":"auth"}"#);
        assert_eq!(
            message,
            r#"{"message":{"content":"hello","role":"user"},"type":"user"}"#
        );
    }
}
