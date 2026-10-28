//! Codex's app server as one agent hosts it: started listening on a Unix
//! socket in the agent's folder (stdio on Windows), and one client
//! connection to it carrying raw messages.
//!
//! On a socket the server outlives any one client, so Codex's own terminal
//! app can join the same live thread (`codex resume --remote`). Codex's
//! socket speaks WebSocket: an HTTP upgrade, then one JSON-RPC message per
//! text frame. Over stdio each message is one line.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// Where an agent's Codex server listens. `Unix` on macOS and Linux, a
/// socket file in the agent's private folder; `Stdio` on Windows, which has
/// no attach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listen {
    Stdio,
    Unix(PathBuf),
}

impl Listen {
    /// The URL `--listen` and `--remote` take.
    pub fn url(&self) -> String {
        match self {
            Listen::Stdio => "stdio://".to_owned(),
            Listen::Unix(path) => format!("unix://{}", path.display()),
        }
    }
}

/// The arguments that run Codex's own terminal app as one more client of
/// the server on `listen`, on its live thread `thread`:
/// `resume <thread> --remote unix://<socket>`. None over stdio, which has
/// room for one client only.
pub fn attach_args(thread: &str, listen: &Listen) -> Option<Vec<String>> {
    match listen {
        Listen::Stdio => None,
        Listen::Unix(_) => Some(vec![
            "resume".to_owned(),
            thread.to_owned(),
            "--remote".to_owned(),
            listen.url(),
        ]),
    }
}

/// Starts `command` (Codex with its global arguments, folder and
/// environment already set) as `app-server --listen <listen>`, leading a
/// process group of its own on Unix. A socket file a killed server left at
/// the path is removed first so it cannot stop this start; the caller owns
/// the path, so no live server is there.
pub fn spawn_server(mut command: Command, listen: &Listen) -> io::Result<Child> {
    command.args(["app-server", "--listen", &listen.url()]);
    #[cfg(unix)]
    command.process_group(0);
    match listen {
        Listen::Stdio => {
            command.stdin(Stdio::piped()).stdout(Stdio::piped());
        }
        Listen::Unix(path) => {
            remove_stale(path)?;
            command.stdin(Stdio::null()).stdout(Stdio::null());
        }
    }
    command.spawn()
}

/// Ties a server on a socket to the process that started it. Over stdio
/// the server ends at the end of its input, which the starter's death
/// closes; on a socket nothing tells it, so it would serve on with no one
/// to serve. The tether is a shell in the server's process group, reading a
/// pipe only the starter holds: when the starter dies, however it dies, the
/// pipe closes and the shell sends the group SIGTERM. A server that is
/// still running `TETHER_GRACE_SECS` later (one with an approval open
/// ignores SIGTERM, and would hold its thread from every later client) has
/// its group killed. Being in the group, the shell keeps the group's id,
/// and so the server's pid, from being reused while it waits.
#[cfg(unix)]
pub const TETHER_GRACE_SECS: u32 = 5;

#[cfg(unix)]
pub struct Tether {
    shell: Child,
}

#[cfg(unix)]
impl Tether {
    /// Tethers the server `server` leads, which must not have been reaped.
    pub fn new(server: &Child) -> io::Result<Tether> {
        let group = server
            .id()
            .ok_or_else(|| io::Error::other("the server has already been reaped"))?;
        // The shell ignores the SIGTERM it sends its own group.
        let script = format!(
            r#"trap '' TERM; read _; kill -TERM "-$1" 2>/dev/null; i=0
while kill -0 "$1" 2>/dev/null; do
  if [ "$i" -ge {TETHER_GRACE_SECS} ]; then kill -KILL "-$1" 2>/dev/null; exit 0; fi
  sleep 1; i=$((i + 1))
done"#
        );
        let shell = Command::new("/bin/sh")
            .args(["-c", &script, "codex-tether"])
            .arg(group.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(group as i32)
            .spawn()?;
        Ok(Tether { shell })
    }

    /// Lets go once the server has exited: whatever it left in its group
    /// is sent SIGTERM, and the shell is reaped.
    pub async fn release(mut self) {
        self.shell.stdin.take();
        let _ = self.shell.wait().await;
    }
}

fn remove_stale(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("Codex exited before it took a client ({0})")]
    Exited(std::process::ExitStatus),
    #[error("Codex was not listening on {path} after {wait:?}: {source}")]
    NotListening {
        path: PathBuf,
        wait: Duration,
        source: io::Error,
    },
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// One client of a Codex server: whole messages in and out, as bytes.
pub struct Connection {
    sender: Sender,
    receiver: Receiver,
}

impl Connection {
    /// Connects to the server `child` runs. Over stdio that is its pipes;
    /// on a socket it waits up to `wait` for the server to listen, and
    /// gives up early if the server exits.
    pub async fn connect(
        listen: &Listen,
        child: &mut Child,
        wait: Duration,
    ) -> Result<Connection, ConnectError> {
        match listen {
            Listen::Stdio => {
                let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
                    return Err(io::Error::other("Codex's stdio is not piped").into());
                };
                Ok(Connection {
                    sender: Sender(SenderOf::Stdio(Some(stdin))),
                    receiver: Receiver(ReceiverOf::Stdio(BufReader::new(stdout))),
                })
            }
            Listen::Unix(path) => socket::connect(path, child, wait).await,
        }
    }

    pub async fn send(&mut self, message: &[u8]) -> io::Result<()> {
        self.sender.send(message).await
    }

    /// The next message the server sent; None once the connection ends.
    pub async fn next(&mut self) -> Option<io::Result<Vec<u8>>> {
        self.receiver.next().await
    }

    /// The writing and reading halves, to drive from separate tasks.
    pub fn into_split(self) -> (Sender, Receiver) {
        (self.sender, self.receiver)
    }
}

/// The writing half of a [`Connection`].
pub struct Sender(SenderOf);

enum SenderOf {
    Stdio(Option<ChildStdin>),
    #[cfg(unix)]
    Socket(socket::Sink),
}

impl Sender {
    /// Sends one message.
    pub async fn send(&mut self, message: &[u8]) -> io::Result<()> {
        match &mut self.0 {
            SenderOf::Stdio(stdin) => {
                let stdin = stdin.as_mut().ok_or_else(closed)?;
                stdin.write_all(message).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await
            }
            #[cfg(unix)]
            SenderOf::Socket(sink) => socket::send(sink, message).await,
        }
    }

    /// Ends this client's side: end of input over stdio, which ends a
    /// stdio server; a close frame on a socket, which the server answers
    /// by closing the connection and which leaves it serving others.
    pub async fn close(&mut self) -> io::Result<()> {
        match &mut self.0 {
            SenderOf::Stdio(stdin) => {
                stdin.take();
                Ok(())
            }
            #[cfg(unix)]
            SenderOf::Socket(sink) => socket::close(sink).await,
        }
    }
}

/// The reading half of a [`Connection`].
pub struct Receiver(ReceiverOf);

enum ReceiverOf {
    Stdio(BufReader<ChildStdout>),
    #[cfg(unix)]
    Socket(socket::Stream),
}

impl Receiver {
    /// The next message the server sent; None once the connection ends.
    pub async fn next(&mut self) -> Option<io::Result<Vec<u8>>> {
        match &mut self.0 {
            ReceiverOf::Stdio(stdout) => {
                let mut line = Vec::new();
                match stdout.read_until(b'\n', &mut line).await {
                    Ok(0) => None,
                    Ok(_) => {
                        if line.last() == Some(&b'\n') {
                            line.pop();
                            if line.last() == Some(&b'\r') {
                                line.pop();
                            }
                        }
                        Some(Ok(line))
                    }
                    Err(error) => Some(Err(error)),
                }
            }
            #[cfg(unix)]
            ReceiverOf::Socket(stream) => socket::next(stream).await,
        }
    }
}

fn closed() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "the connection to Codex is closed",
    )
}

#[cfg(unix)]
mod socket {
    /// The largest message or frame read from Codex.
    const MAX_MESSAGE: usize = 128 * 1024 * 1024;

    use std::io;
    use std::path::Path;
    use std::time::Duration;

    use futures_util::stream::{SplitSink, SplitStream};
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::UnixStream;
    use tokio::process::Child;
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

    use super::{ConnectError, Connection, Receiver, ReceiverOf, Sender, SenderOf};

    pub type Sink = SplitSink<WebSocketStream<UnixStream>, Message>;
    pub type Stream = SplitStream<WebSocketStream<UnixStream>>;

    /// How often a server that is not listening yet is tried again.
    const RETRY: Duration = Duration::from_millis(10);

    pub async fn connect(
        path: &Path,
        child: &mut Child,
        wait: Duration,
    ) -> Result<Connection, ConnectError> {
        let deadline = tokio::time::Instant::now() + wait;
        let stream = loop {
            match UnixStream::connect(path).await {
                Ok(stream) => break stream,
                Err(source) => {
                    if let Some(status) = child.try_wait()? {
                        return Err(ConnectError::Exited(status));
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(ConnectError::NotListening {
                            path: path.to_owned(),
                            wait,
                            source,
                        });
                    }
                    tokio::time::sleep(RETRY).await;
                }
            }
        };
        // The URL's host and path are what Codex's own clients send; the
        // server reads neither for a client connection. One message can
        // carry a turn's screenshots, past the library's 16 MiB frame and
        // 64 MiB message defaults.
        let config = WebSocketConfig::default()
            .max_frame_size(Some(MAX_MESSAGE))
            .max_message_size(Some(MAX_MESSAGE));
        let (socket, _) =
            tokio_tungstenite::client_async_with_config("ws://localhost/rpc", stream, Some(config))
                .await
                .map_err(io::Error::other)?;
        let (sink, stream) = socket.split();
        Ok(Connection {
            sender: Sender(SenderOf::Socket(sink)),
            receiver: Receiver(ReceiverOf::Socket(stream)),
        })
    }

    pub async fn send(sink: &mut Sink, message: &[u8]) -> io::Result<()> {
        let text = std::str::from_utf8(message)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        sink.send(Message::text(text))
            .await
            .map_err(io::Error::other)
    }

    pub async fn close(sink: &mut Sink) -> io::Result<()> {
        sink.close().await.map_err(io::Error::other)
    }

    pub async fn next(stream: &mut Stream) -> Option<io::Result<Vec<u8>>> {
        loop {
            match stream.next().await? {
                Ok(Message::Text(text)) => return Some(Ok(text.as_bytes().to_vec())),
                Ok(Message::Binary(bytes)) => return Some(Ok(bytes.to_vec())),
                Ok(Message::Close(_)) => return None,
                Ok(_) => {}
                Err(error) => return Some(Err(io::Error::other(error))),
            }
        }
    }
}

#[cfg(not(unix))]
mod socket {
    use std::path::Path;
    use std::time::Duration;

    use tokio::process::Child;

    use super::{ConnectError, Connection};

    pub async fn connect(
        path: &Path,
        _child: &mut Child,
        _wait: Duration,
    ) -> Result<Connection, ConnectError> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "Codex is hosted over stdio on this platform, not on {}",
                path.display()
            ),
        )
        .into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use tokio::net::UnixListener;
    use tokio::process::Command;
    use tokio_tungstenite::tungstenite::Message;

    use super::{ConnectError, Connection, Listen, Tether, spawn_server};

    const WAIT: Duration = Duration::from_secs(10);

    /// A server that waits: something to give `connect` while a test
    /// plays the server itself.
    fn idle_server(listen: &Listen) -> tokio::process::Child {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30", "sh"]).kill_on_drop(true);
        spawn_server(command, listen).unwrap()
    }

    #[tokio::test]
    async fn a_socket_left_by_a_killed_server_does_not_stop_the_next() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("codex.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists(), "the dead server's socket is left behind");
        let listen = Listen::Unix(path.clone());
        let mut child = idle_server(&listen);
        assert!(!path.exists(), "the stale socket is gone before the start");

        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let Some(Ok(Message::Text(asked))) = socket.next().await else {
                panic!("a text frame");
            };
            socket
                .send(Message::text(format!(r#"{{"echo":{asked}}}"#)))
                .await
                .unwrap();
            // The client's close is answered, and the connection ends.
            while let Some(Ok(_)) = socket.next().await {}
        });
        let connection = Connection::connect(&listen, &mut child, WAIT)
            .await
            .unwrap();
        let (mut sender, mut receiver) = connection.into_split();
        sender.send(br#"{"id":1}"#).await.unwrap();
        assert_eq!(
            receiver.next().await.unwrap().unwrap(),
            br#"{"echo":{"id":1}}"#
        );
        sender.close().await.unwrap();
        assert!(receiver.next().await.is_none(), "the server closed in turn");
        server.await.unwrap();
    }

    /// A resumed thread's answer or a turn's item can carry screenshots well
    /// past the websocket library's 16 MiB default frame; a 20 MiB one is
    /// read whole.
    #[tokio::test]
    async fn a_frame_past_sixteen_mebibytes_is_read() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("codex.sock");
        let listen = Listen::Unix(path.clone());
        let mut child = idle_server(&listen);
        let listener = UnixListener::bind(&path).unwrap();
        let big = format!(r#"{{"blob":"{}"}}"#, "a".repeat(20 * 1024 * 1024));
        let sent = big.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.send(Message::text(sent)).await.unwrap();
            while let Some(Ok(_)) = socket.next().await {}
        });
        let connection = Connection::connect(&listen, &mut child, WAIT)
            .await
            .unwrap();
        let (mut sender, mut receiver) = connection.into_split();
        let read = receiver.next().await.expect("a frame").expect("read whole");
        assert_eq!(read.len(), big.len());
        sender.close().await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_server_that_exits_before_listening_is_reported_at_once() {
        let folder = tempfile::tempdir().unwrap();
        let listen = Listen::Unix(folder.path().join("codex.sock"));
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 3", "sh"]);
        let mut child = spawn_server(command, &listen).unwrap();
        let started = std::time::Instant::now();
        match Connection::connect(&listen, &mut child, WAIT).await {
            Err(ConnectError::Exited(status)) => assert_eq!(status.code(), Some(3)),
            Err(other) => panic!("{other}"),
            Ok(_) => panic!("nothing listens"),
        }
        assert!(started.elapsed() < WAIT, "not left to time out");
    }

    /// Killing the starter, which is what closes the tether's pipe, asks
    /// the server's whole group to finish.
    #[tokio::test]
    async fn a_tethered_server_finishes_when_its_starter_dies() {
        let folder = tempfile::tempdir().unwrap();
        let listen = Listen::Unix(folder.path().join("codex.sock"));
        let started = folder.path().join("started");
        let mut command = Command::new("/bin/sh");
        // A child in the group that would outlive the server alone.
        command.args(["-c", r#"sleep 30 & : > "$1"; wait"#, "sh"]);
        command.arg(&started);
        let mut server = spawn_server(command, &listen).unwrap();
        let group = server.id().unwrap() as i32;
        let tether = Tether::new(&server).unwrap();
        // A signal sent while a process forks can miss the new child.
        tokio::time::timeout(WAIT, async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the server's child starts");
        // The starter dying is its end of the pipe closing.
        let Tether { mut shell } = tether;
        drop(shell.stdin.take());
        let status = tokio::time::timeout(WAIT, server.wait())
            .await
            .expect("the server is asked to finish")
            .unwrap();
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(15)
        );
        let _ = shell.wait().await;
        let gone = tokio::time::timeout(WAIT, async {
            while group_lives(group) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(gone.is_ok(), "nothing of the group lives on");
    }

    fn group_lives(group: i32) -> bool {
        nix::sys::signal::killpg(nix::unistd::Pid::from_raw(group), None).is_ok()
    }

    #[tokio::test]
    async fn stdio_is_one_message_per_line() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            r#"read line; printf '%s\r\n' "$line"; printf 'two\n'"#,
        ]);
        let mut child = spawn_server(command, &Listen::Stdio).unwrap();
        let mut connection = Connection::connect(&Listen::Stdio, &mut child, WAIT)
            .await
            .unwrap();
        connection.send(b"one").await.unwrap();
        assert_eq!(connection.next().await.unwrap().unwrap(), b"one");
        assert_eq!(connection.next().await.unwrap().unwrap(), b"two");
        assert!(connection.next().await.is_none());
    }
}
