//! Claude Code messaging-socket client.

use std::path::Path;

#[cfg(unix)]
use claude_protocol::messaging::{Auth, UserMessage};

#[derive(Debug, thiserror::Error)]
pub enum MessagingError {
    #[error("Claude messaging socket I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Claude messaging sockets require Unix")]
    Unsupported,
}

pub struct MessagingSocket {
    #[cfg(unix)]
    stream: tokio::net::UnixStream,
}

impl MessagingSocket {
    #[cfg(unix)]
    pub async fn connect(path: &Path, token: &str) -> Result<Self, MessagingError> {
        use tokio::io::AsyncWriteExt;

        let mut stream = tokio::net::UnixStream::connect(path).await?;
        let auth = serde_json::to_vec(&Auth::new(token)).map_err(std::io::Error::other)?;
        stream.write_all(&auth).await?;
        stream.write_all(b"\n").await?;
        Ok(Self { stream })
    }

    #[cfg(not(unix))]
    pub async fn connect(_path: &Path, _token: &str) -> Result<Self, MessagingError> {
        Err(MessagingError::Unsupported)
    }

    #[cfg(unix)]
    pub async fn send(&mut self, text: &str) -> Result<(), MessagingError> {
        use tokio::io::AsyncWriteExt;

        let message = serde_json::to_vec(&UserMessage::new(text)).map_err(std::io::Error::other)?;
        self.stream.write_all(&message).await?;
        self.stream.write_all(b"\n").await?;
        self.stream.shutdown().await?;
        Ok(())
    }

    #[cfg(not(unix))]
    pub async fn send(&mut self, _text: &str) -> Result<(), MessagingError> {
        Err(MessagingError::Unsupported)
    }
}

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
