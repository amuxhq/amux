//! Provider-neutral pseudo-terminal process hosting.
//!
//! A spawned process owns one output stream and runs in its own process group.
//! Cloning [`PtyHandle`] shares input, resize, and process identity; callers
//! that need output fan-out own that policy above this crate.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
pub use portable_pty::ExitStatus;
use portable_pty::{CommandBuilder, MasterPty, native_pty_system};
use tokio::sync::{mpsc, watch};

#[cfg(unix)]
mod unix;

const IO_CHANNEL_CAPACITY: usize = 256;

/// What to run and where.
#[derive(Clone, Debug)]
pub struct PtySpawn {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    pub env_remove: Vec<OsString>,
    pub size: PtySize,
}

/// Terminal geometry in character cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for PtySize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

/// A PTY spawn or I/O failure.
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("failed to spawn PTY process: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("PTY process exited: {0:?}")]
    Exited(ExitStatus),
    #[error("PTY I/O failed: {0}")]
    Io(#[source] std::io::Error),
}

/// A running child and its independently usable exit monitor.
pub struct PtyProcess {
    pub handle: PtyHandle,
    pub exit: ExitMonitor,
}

struct Shared {
    input_tx: mpsc::Sender<Bytes>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    current_size: Mutex<PtySize>,
    output: Mutex<Option<mpsc::Receiver<Bytes>>>,
    exit_rx: watch::Receiver<Option<ExitStatus>>,
    pid: u32,
    #[cfg(not(unix))]
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
}

/// Cheaply cloned access to one PTY process.
#[derive(Clone)]
pub struct PtyHandle {
    shared: Arc<Shared>,
}

impl PtyHandle {
    /// Queue all bytes for writing to the PTY.
    pub async fn write(&self, bytes: &[u8]) -> Result<(), PtyError> {
        if let Some(status) = self.shared.exit_rx.borrow().clone() {
            return Err(PtyError::Exited(status));
        }
        self.shared
            .input_tx
            .send(Bytes::copy_from_slice(bytes))
            .await
            .map_err(|_| {
                self.shared
                    .exit_rx
                    .borrow()
                    .clone()
                    .map(PtyError::Exited)
                    .unwrap_or_else(|| {
                        PtyError::Io(std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "PTY writer is closed",
                        ))
                    })
            })
    }

    /// Resize the terminal and notify the foreground process group.
    pub fn resize(&self, size: PtySize) -> Result<(), PtyError> {
        let mut current = self
            .shared
            .current_size
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if *current == size {
            return Ok(());
        }
        let master = self
            .shared
            .master
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(master) = master.as_ref() else {
            return self.exited_error();
        };
        master
            .resize(portable_size(size))
            .map_err(anyhow_to_io)
            .map_err(PtyError::Io)?;
        *current = size;
        Ok(())
    }

    /// Take the process's single owned output stream.
    ///
    /// A second call returns an already-closed receiver.
    pub fn output(&self) -> mpsc::Receiver<Bytes> {
        self.shared
            .output
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
            .unwrap_or_else(|| {
                let (_tx, rx) = mpsc::channel(1);
                rx
            })
    }

    /// Return the child process id, which is also its process-group id on Unix.
    pub fn pid(&self) -> u32 {
        self.shared.pid
    }

    /// Signal the entire hosted process group without requiring an async runtime.
    pub fn signal_process_group(&self, signal: ProcessGroupSignal) -> Result<(), PtyError> {
        if self.shared.exit_rx.borrow().is_some() {
            return Ok(());
        }

        #[cfg(unix)]
        {
            let signal = match signal {
                ProcessGroupSignal::Terminate => libc::SIGTERM,
                ProcessGroupSignal::Kill => libc::SIGKILL,
            };
            unix::signal_group(self.pid(), signal).map_err(PtyError::Io)
        }

        #[cfg(not(unix))]
        {
            let _ = signal;
            self.shared
                .killer
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .kill()
                .map_err(PtyError::Io)
        }
    }

    fn exited_error(&self) -> Result<(), PtyError> {
        Err(self
            .shared
            .exit_rx
            .borrow()
            .clone()
            .map(PtyError::Exited)
            .unwrap_or_else(|| {
                PtyError::Io(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "PTY master is closed",
                ))
            }))
    }
}

/// Observes one child exit without owning or killing it.
#[derive(Clone)]
pub struct ExitMonitor {
    rx: watch::Receiver<Option<ExitStatus>>,
}

impl ExitMonitor {
    /// Wait until the child exits. Repeated calls return the same status.
    pub async fn wait(&mut self) -> ExitStatus {
        loop {
            if let Some(status) = self.rx.borrow().clone() {
                return status;
            }
            if self.rx.changed().await.is_err() {
                return ExitStatus::with_signal("exit monitor closed");
            }
        }
    }

    /// Return the exit status without waiting, when available.
    pub fn status(&self) -> Option<ExitStatus> {
        self.rx.borrow().clone()
    }
}

/// Process-group termination policy.
#[derive(Clone, Copy, Debug)]
pub enum Terminate {
    Graceful { grace: Duration },
    Kill,
}

/// A signal delivered synchronously to the hosted process group.
#[derive(Clone, Copy, Debug)]
pub enum ProcessGroupSignal {
    Terminate,
    Kill,
}

/// Spawn a process on a new PTY in its own process group.
pub fn spawn(spec: PtySpawn) -> Result<PtyProcess, PtyError> {
    #[cfg(target_os = "macos")]
    unix::prepare_fork();
    let pair = native_pty_system()
        .openpty(portable_size(spec.size))
        .map_err(anyhow_to_io)
        .map_err(PtyError::Spawn)?;

    let mut command = CommandBuilder::new(&spec.command);
    for arg in &spec.args {
        command.arg(arg);
    }
    command.cwd(&spec.cwd);
    for key in &spec.env_remove {
        command.env_remove(key);
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }

    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(anyhow_to_io)
        .map_err(PtyError::Spawn)?;
    let pid = child.process_id().ok_or_else(|| {
        PtyError::Spawn(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "PTY child has no process id",
        ))
    })?;
    #[cfg(not(unix))]
    let killer = child.clone_killer();
    drop(pair.slave);

    let master = pair.master;
    let mut reader = master
        .try_clone_reader()
        .map_err(anyhow_to_io)
        .map_err(PtyError::Spawn)?;
    let mut writer = master
        .take_writer()
        .map_err(anyhow_to_io)
        .map_err(PtyError::Spawn)?;
    let (input_tx, mut input_rx) = mpsc::channel::<Bytes>(IO_CHANNEL_CAPACITY);
    let (output_tx, output_rx) = mpsc::channel::<Bytes>(IO_CHANNEL_CAPACITY);
    let (exit_tx, exit_rx) = watch::channel(None);

    #[cfg(windows)]
    let mut cursor_request = CursorRequest::answering(input_tx.clone());
    tokio::task::spawn_blocking(move || {
        let mut buffer = [0; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    #[cfg(windows)]
                    let bytes = Bytes::from(cursor_request.take(&buffer[..read]));
                    #[cfg(not(windows))]
                    let bytes = Bytes::copy_from_slice(&buffer[..read]);
                    if !bytes.is_empty() && output_tx.blocking_send(bytes).is_err() {
                        return;
                    }
                }
            }
        }
        #[cfg(windows)]
        {
            let held = cursor_request.rest();
            if !held.is_empty() {
                let _ = output_tx.blocking_send(Bytes::from(held));
            }
        }
    });

    tokio::task::spawn_blocking(move || {
        while let Some(bytes) = input_rx.blocking_recv() {
            if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                break;
            }
        }
    });

    let shared = Arc::new(Shared {
        input_tx,
        master: Mutex::new(Some(master)),
        current_size: Mutex::new(spec.size),
        output: Mutex::new(Some(output_rx)),
        exit_rx: exit_rx.clone(),
        pid,
        #[cfg(not(unix))]
        killer: Mutex::new(killer),
    });
    let wait_shared = shared.clone();
    tokio::task::spawn_blocking(move || {
        let status = child
            .wait()
            .unwrap_or_else(|error| ExitStatus::with_signal(&format!("wait failed: {error}")));
        wait_shared
            .master
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        exit_tx.send_replace(Some(status));
    });

    Ok(PtyProcess {
        handle: PtyHandle { shared },
        exit: ExitMonitor { rx: exit_rx },
    })
}

/// ConPTY's question when it opens: where is the cursor?
#[cfg(any(windows, test))]
const CURSOR_REQUEST: &[u8] = b"\x1b[6n";
/// The answer for a terminal that starts blank: row 1, column 1.
#[cfg(any(windows, test))]
const CURSOR_HOME: &[u8] = b"\x1b[1;1R";

/// ConPTY's opening cursor-position request, answered by the host.
///
/// portable-pty creates every pseudoconsole with
/// `PSEUDOCONSOLE_INHERIT_CURSOR`, so ConPTY opens by writing a
/// cursor-position request and then draws nothing the child writes until
/// an answer arrives on its input. Nothing downstream can answer it
/// properly: an agent running unattended has no terminal at all, and an
/// attached client would answer with the cursor of its own screen, not the
/// hosted one's. The hosted terminal starts blank, so the host answers
/// "row 1, column 1", which leaves ConPTY where it would be had it not
/// asked, and takes the request out of the output. Only that first request
/// is ConPTY's own: it answers its child's cursor queries itself, from its
/// screen, so everything after it passes through untouched.
#[cfg(any(windows, test))]
struct CursorRequest {
    /// The terminal's input, until the request is answered.
    answer: Option<mpsc::Sender<Bytes>>,
    /// Output that may be the start of a request split across reads.
    held: Vec<u8>,
}

#[cfg(any(windows, test))]
impl CursorRequest {
    fn answering(input: mpsc::Sender<Bytes>) -> Self {
        Self {
            answer: Some(input),
            held: Vec::new(),
        }
    }

    /// `chunk` as the hosted terminal's output: without ConPTY's request,
    /// which is answered as it is found.
    fn take(&mut self, chunk: &[u8]) -> Vec<u8> {
        if self.answer.is_none() {
            return chunk.to_vec();
        }
        let mut bytes = std::mem::take(&mut self.held);
        bytes.extend_from_slice(chunk);
        if let Some(at) = bytes
            .windows(CURSOR_REQUEST.len())
            .position(|window| window == CURSOR_REQUEST)
        {
            bytes.drain(at..at + CURSOR_REQUEST.len());
            if let Some(answer) = self.answer.take() {
                let _ = answer.blocking_send(Bytes::from_static(CURSOR_HOME));
            }
            return bytes;
        }
        let partial = (1..CURSOR_REQUEST.len())
            .rev()
            .find(|&len| bytes.ends_with(&CURSOR_REQUEST[..len]))
            .unwrap_or(0);
        self.held = bytes.split_off(bytes.len() - partial);
        bytes
    }

    /// Output held back when the terminal closed.
    fn rest(self) -> Vec<u8> {
        self.held
    }
}

/// End the entire process group and return its exit status.
pub async fn terminate(process: &PtyProcess, policy: Terminate) -> ExitStatus {
    if let Some(status) = process.exit.status() {
        return status;
    }

    #[cfg(unix)]
    {
        match policy {
            Terminate::Kill => {
                let _ = process
                    .handle
                    .signal_process_group(ProcessGroupSignal::Kill);
            }
            Terminate::Graceful { grace } => {
                let _ = process
                    .handle
                    .signal_process_group(ProcessGroupSignal::Terminate);
                let mut monitor = process.exit.clone();
                if let Ok(status) = tokio::time::timeout(grace, monitor.wait()).await {
                    return status;
                }
                let _ = process
                    .handle
                    .signal_process_group(ProcessGroupSignal::Kill);
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = policy;
        let _ = process
            .handle
            .shared
            .killer
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .kill();
    }

    let mut monitor = process.exit.clone();
    monitor.wait().await
}

fn portable_size(size: PtySize) -> portable_pty::PtySize {
    portable_pty::PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn anyhow_to_io(error: anyhow::Error) -> std::io::Error {
    match error.downcast::<std::io::Error>() {
        Ok(error) => error,
        Err(error) => std::io::Error::other(error),
    }
}

#[cfg(test)]
mod cursor_request_tests {
    use super::*;

    fn request() -> (CursorRequest, mpsc::Receiver<Bytes>) {
        let (input, answers) = mpsc::channel(4);
        (CursorRequest::answering(input), answers)
    }

    #[test]
    fn the_first_request_is_answered_from_home_and_taken_out() {
        let (mut request, mut answers) = request();
        assert_eq!(request.take(b"\x1b[?25l\x1b[6n"), b"\x1b[?25l");
        assert_eq!(answers.try_recv().unwrap(), CURSOR_HOME);
        assert_eq!(request.take(b"drawn\x1b[6n"), b"drawn\x1b[6n");
        assert!(answers.try_recv().is_err(), "answered once");
    }

    #[test]
    fn a_request_split_across_reads_is_still_found() {
        let (mut request, mut answers) = request();
        assert_eq!(request.take(b"\x1b"), b"");
        assert_eq!(request.take(b"[6"), b"");
        assert!(answers.try_recv().is_err());
        assert_eq!(request.take(b"nafter"), b"after");
        assert_eq!(answers.try_recv().unwrap(), CURSOR_HOME);
    }

    #[test]
    fn held_output_that_was_no_request_is_kept() {
        let (mut request, _answers) = request();
        assert_eq!(request.take(b"a\x1b["), b"a");
        assert_eq!(request.take(b"mb\x1b"), b"\x1b[mb");
        assert_eq!(request.rest(), b"\x1b");
    }
}
