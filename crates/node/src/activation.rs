//! The supervisor pipe: the one thing a daemon knows about being
//! supervised.
//!
//! The daemon writes `prepared` once it has opened its stores and looked
//! at every agent directory without writing, and waits for `go`. Before
//! `go` nothing it did needs undoing if the supervisor rolls the binary
//! back; after it the daemon is ordinary running. End of file on the pipe
//! means the supervisor is gone, and the daemon shuts down so that whoever
//! restarts the supervisor gets a fresh daemon under it. A daemon with no
//! pipe has no supervisor and goes at once.

use std::io;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

pub const PREPARED: &str = "prepared";
pub const GO: &str = "go";

type Reader = BufReader<Box<dyn AsyncRead + Send + Unpin>>;

pub struct ActivationPipe {
    reader: Reader,
    writer: Box<dyn AsyncWrite + Send + Unpin>,
}

impl ActivationPipe {
    pub fn new(
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
    ) -> Self {
        Self {
            reader: BufReader::new(Box::new(reader)),
            writer: Box::new(writer),
        }
    }

    /// Writes `prepared` and waits for `go`.
    pub async fn activate(&mut self) -> Result<(), ActivationError> {
        self.writer
            .write_all(format!("{PREPARED}\n").as_bytes())
            .await
            .map_err(ActivationError::Io)?;
        self.writer.flush().await.map_err(ActivationError::Io)?;
        let mut line = String::new();
        if self
            .reader
            .read_line(&mut line)
            .await
            .map_err(ActivationError::Io)?
            == 0
        {
            return Err(ActivationError::SupervisorGone);
        }
        match line.trim_end() {
            GO => Ok(()),
            other => Err(ActivationError::Unexpected(other.to_owned())),
        }
    }

    /// Resolves when the supervisor is gone: end of file, or a read error.
    /// Anything it writes after `go` is ignored.
    pub async fn closed(mut self) {
        let mut line = String::new();
        loop {
            line.clear();
            match self.reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    }
}

/// The pipe's ends as the supervisor named them in the environment, taken
/// before the runtime starts.
#[derive(Debug)]
pub struct InheritedPipe {
    read: std::fs::File,
    write: std::fs::File,
}

/// Names the pipe's two ends, `<read>,<write>`: descriptors on Unix, handle
/// values on Windows. Only a supervisor sets it.
pub const PIPE_ENV: &str = "AMUX_SUPERVISOR_PIPE";

impl InheritedPipe {
    /// Takes the pipe a supervisor handed this process, if one did, and
    /// removes it from the environment so nothing the daemon starts sees it
    /// or inherits its ends.
    ///
    /// # Safety
    ///
    /// Call before any other thread exists: it changes the environment.
    pub unsafe fn take() -> io::Result<Option<Self>> {
        let Some(value) = std::env::var_os(PIPE_ENV) else {
            return Ok(None);
        };
        // SAFETY: the caller promises no other thread reads the
        // environment yet.
        unsafe { std::env::remove_var(PIPE_ENV) };
        let value = value.to_string_lossy();
        let bad = || io::Error::other(format!("{PIPE_ENV}={value} is not <read>,<write>"));
        let (read, write) = value.split_once(',').ok_or_else(bad)?;
        let read: u64 = read.parse().map_err(|_| bad())?;
        let write: u64 = write.parse().map_err(|_| bad())?;
        Ok(Some(Self {
            read: own(read)?,
            write: own(write)?,
        }))
    }

    /// The pipe, on the current runtime.
    pub fn into_pipe(self) -> io::Result<ActivationPipe> {
        #[cfg(unix)]
        {
            let read = tokio::net::unix::pipe::Receiver::from_file(self.read)?;
            let write = tokio::net::unix::pipe::Sender::from_file(self.write)?;
            Ok(ActivationPipe::new(read, write))
        }
        #[cfg(not(unix))]
        {
            Ok(ActivationPipe::new(
                tokio::fs::File::from_std(self.read),
                tokio::fs::File::from_std(self.write),
            ))
        }
    }
}

/// Owns an end the supervisor handed over, closed on exec so the daemon's
/// own children never hold it.
#[cfg(unix)]
fn own(raw: u64) -> io::Result<std::fs::File> {
    use std::os::fd::{FromRawFd as _, RawFd};
    let fd = RawFd::try_from(raw).map_err(io::Error::other)?;
    // SAFETY: fcntl on a descriptor this process was handed.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the supervisor handed the descriptor to this process alone.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(windows)]
fn own(raw: u64) -> io::Result<std::fs::File> {
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, RawHandle};

    use windows_sys::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};
    // SAFETY: the supervisor handed the handle to this process alone.
    let file = unsafe { std::fs::File::from_raw_handle(raw as RawHandle) };
    // SAFETY: clearing a flag on a handle this process owns.
    if unsafe { SetHandleInformation(file.as_raw_handle() as HANDLE, HANDLE_FLAG_INHERIT, 0) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn own(_raw: u64) -> io::Result<std::fs::File> {
    Err(io::Error::other("no supervisor pipe on this platform"))
}

#[derive(Debug, thiserror::Error)]
pub enum ActivationError {
    #[error("the supervisor went away before activation")]
    SupervisorGone,
    #[error("the supervisor answered {0:?}, not go")]
    Unexpected(String),
    #[error("the supervisor pipe: {0}")]
    Io(io::Error),
}
