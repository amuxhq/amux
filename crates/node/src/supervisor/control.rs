//! `<data_dir>/supervisor.sock`: how the CLI reaches a running supervisor.
//! One request line, one answer line. `check` is `amux update`: check the
//! channel now; `retry` is `amux update --retry`, which may install the
//! build rolled back here again. `stop` is `amux server stop`
//! on Windows, which has no signals; on Unix the CLI signals the pid in the
//! supervisor's lock file instead.

use std::io;
use std::path::Path;
use std::time::Duration;

use agent_dir::local_socket::{self, LocalListener};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::sync::{mpsc, oneshot};

/// The control socket in the data directory.
pub const SUPERVISOR_SOCKET: &str = "supervisor.sock";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Check,
    Retry,
    Stop,
}

impl Request {
    fn word(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Retry => "retry",
            Self::Stop => "stop",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        match word {
            "check" => Some(Self::Check),
            "retry" => Some(Self::Retry),
            "stop" => Some(Self::Stop),
            _ => None,
        }
    }
}

pub(super) struct Asked {
    pub request: Request,
    pub answer: oneshot::Sender<String>,
}

/// Serves the socket until the supervisor exits. A socket that cannot be
/// bound is logged: restarts and updates go on without it.
pub(super) fn serve(data_dir: &Path) -> mpsc::Receiver<Asked> {
    let (sender, receiver) = mpsc::channel(8);
    let path = data_dir.join(SUPERVISOR_SOCKET);
    let mut listener = match LocalListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not bind the control socket");
            return receiver;
        }
    };
    tokio::spawn(async move {
        loop {
            let Ok(stream) = listener.accept().await else {
                return;
            };
            let sender = sender.clone();
            tokio::spawn(async move {
                let (read, mut write) = tokio::io::split(stream);
                let mut line = String::new();
                if BufReader::new(read).read_line(&mut line).await.is_err() {
                    return;
                }
                let answer = match Request::parse(line.trim()) {
                    Some(request) => {
                        let (answer, answered) = oneshot::channel();
                        if sender.send(Asked { request, answer }).await.is_err() {
                            return;
                        }
                        answered
                            .await
                            .unwrap_or_else(|_| "the supervisor is stopping".into())
                    }
                    None => format!("unknown request {:?}", line.trim()),
                };
                let _ = write.write_all(format!("{answer}\n").as_bytes()).await;
                let _ = write.shutdown().await;
            });
        }
    });
    receiver
}

/// Asks the supervisor of the installation at `data_dir`, waiting up to
/// `patience` for the answer.
pub async fn ask(data_dir: &Path, request: Request, patience: Duration) -> io::Result<String> {
    let stream = local_socket::connect(&data_dir.join(SUPERVISOR_SOCKET)).await?;
    let (read, mut write) = tokio::io::split(stream);
    write
        .write_all(format!("{}\n", request.word()).as_bytes())
        .await?;
    let mut line = String::new();
    tokio::time::timeout(patience, BufReader::new(read).read_line(&mut line))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the supervisor did not answer"))??;
    Ok(line.trim_end().to_owned())
}
