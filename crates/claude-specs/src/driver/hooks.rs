//! Receiving terminal Claude's forwarded hook payloads during a recording:
//! the hook command `claude-probe __pty-hook` forwards each payload over a
//! per-session socket, as amux's own hook command does.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use claude::hooks::unwrap_forwarded;
use claude_protocol::hooks::{self, Payload};
use tokio::sync::mpsc;
use uuid::Uuid;

/// The hook payload in a forwarded connection's bytes, without the
/// messaging credentials the forwarder may have added.
fn parse_forwarded(bytes: &[u8]) -> Option<Payload> {
    let (payload, _messaging) = unwrap_forwarded(bytes).ok()?;
    hooks::decode(&payload).ok()
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
                    && let Some(payload) = parse_forwarded(&bytes)
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
