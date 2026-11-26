//! How fake-codex's clients reach it: one host over stdio, or any number of
//! clients on a Unix socket.
//!
//! Codex's socket speaks WebSocket: an HTTP upgrade, then one JSON-RPC
//! message per text frame. A client that writes JSON lines without the
//! upgrade is disconnected, as Codex does. Both transports deliver the same
//! [`Event`]s, so the server and playback treat a stdio host as the one
//! client that ever connects.

use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, BufReader, Stdout};
use tokio::sync::mpsc;

use crate::lines::Out;

pub type ClientId = u64;

/// What a transport tells the server about its clients.
pub enum Event {
    Opened(ClientId, Writer),
    /// One message the client wrote, as written.
    Frame(ClientId, String),
    Closed(ClientId),
    /// A socket server was asked to finish (SIGTERM, or another of the
    /// OS's stops). Codex then finishes
    /// what is running and exits; for a stdio host the end of its input
    /// says the same.
    Terminated,
}

/// Where `app-server --listen` was told to serve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listen {
    Stdio,
    Unix(PathBuf),
}

impl Listen {
    /// The `--listen` URL of an `app-server` command line; stdio when it
    /// names none.
    pub fn from_args(args: &[String]) -> Result<Self, String> {
        let Some(at) = args.iter().position(|arg| arg == "--listen") else {
            return Ok(Self::Stdio);
        };
        let url = args.get(at + 1).ok_or("--listen needs a URL".to_owned())?;
        Self::from_url(url)
    }

    pub fn from_url(url: &str) -> Result<Self, String> {
        if url == "stdio://" {
            return Ok(Self::Stdio);
        }
        match url.strip_prefix("unix://") {
            // Codex's default socket lives in its home; tests name theirs.
            Some("") => Err("fake-codex serves a socket only at a path it is given".into()),
            Some(path) => Ok(Self::Unix(PathBuf::from(path))),
            None => Err(format!("fake-codex cannot listen on {url}")),
        }
    }
}

/// Writes whole messages to one client.
pub enum Writer {
    Lines(Out<Stdout>),
    Socket(socket::Sink),
}

impl Writer {
    pub async fn write(&mut self, text: &str) -> std::io::Result<()> {
        match self {
            Writer::Lines(out) => out.raw(text.as_bytes()).await,
            Writer::Socket(sink) => socket::write(sink, text).await,
        }
    }
}

/// Start serving: the events of every client that connects.
pub async fn serve(listen: &Listen) -> std::io::Result<mpsc::UnboundedReceiver<Event>> {
    let (tx, events) = mpsc::unbounded_channel();
    match listen {
        Listen::Stdio => {
            tokio::spawn(stdio(tx));
        }
        // Not on Windows, whose hosts keep Codex on stdio.
        Listen::Unix(path) => socket::serve(path, tx).await?,
    }
    Ok(events)
}

async fn stdio(tx: mpsc::UnboundedSender<Event>) {
    if tx
        .send(Event::Opened(
            0,
            Writer::Lines(Out::new(tokio::io::stdout())),
        ))
        .is_err()
    {
        return;
    }
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if tx.send(Event::Frame(0, line)).is_err() {
            return;
        }
    }
    let _ = tx.send(Event::Closed(0));
}

pub use socket::{Connection, connect};

mod socket {
    use std::path::Path;
    use std::time::Duration;

    use futures_util::stream::SplitSink;
    use futures_util::{SinkExt, StreamExt};
    use tokio::sync::mpsc;
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::Message;

    use super::{Event, Writer};
    use crate::platform::{SocketListener, SocketStream};

    pub type Sink = SplitSink<WebSocketStream<SocketStream>, Message>;

    /// A client's side of the socket.
    pub type Connection = WebSocketStream<SocketStream>;

    /// Connects to a server on `path` as Codex's own clients do, waiting
    /// up to `wait` for it to start listening.
    pub async fn connect(path: &Path, wait: Duration) -> std::io::Result<Connection> {
        let deadline = tokio::time::Instant::now() + wait;
        let stream = loop {
            match crate::platform::connect(path).await {
                Ok(stream) => break stream,
                Err(error) if tokio::time::Instant::now() >= deadline => return Err(error),
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        };
        let (socket, _) = tokio_tungstenite::client_async("ws://localhost/rpc", stream)
            .await
            .map_err(std::io::Error::other)?;
        Ok(socket)
    }

    pub async fn write(sink: &mut Sink, text: &str) -> std::io::Result<()> {
        sink.send(Message::text(text))
            .await
            .map_err(std::io::Error::other)
    }

    /// Binds the socket, replacing one a dead server left, and accepts
    /// clients until the process ends or is asked to finish.
    pub async fn serve(path: &Path, tx: mpsc::UnboundedSender<Event>) -> std::io::Result<()> {
        if std::fs::symlink_metadata(path).is_ok() {
            std::fs::remove_file(path)?;
        }
        let listener = SocketListener::bind(path)?;
        let terminated = tx.clone();
        tokio::spawn(async move {
            if os::signal::terminated().await.is_ok() {
                let _ = terminated.send(Event::Terminated);
            }
        });
        tokio::spawn(async move {
            let mut next = 0;
            while let Ok(stream) = listener.accept().await {
                tokio::spawn(client(next, stream, tx.clone()));
                next += 1;
            }
        });
        Ok(())
    }

    async fn client(id: super::ClientId, stream: SocketStream, tx: mpsc::UnboundedSender<Event>) {
        // A client that skips the upgrade is dropped here, unanswered.
        let Ok(socket) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        let (sink, mut frames) = socket.split();
        if tx.send(Event::Opened(id, Writer::Socket(sink))).is_err() {
            return;
        }
        while let Some(Ok(message)) = frames.next().await {
            match message {
                Message::Text(text) => {
                    if tx.send(Event::Frame(id, text.as_str().to_owned())).is_err() {
                        return;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = tx.send(Event::Closed(id));
    }
}
