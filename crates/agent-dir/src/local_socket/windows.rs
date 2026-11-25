//! Named pipes: a fresh, randomly named pipe per bind, its name in the file
//! at the socket's path, admitting this user alone.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::future::Future;
use std::io::{self, Read};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_NO_DATA, ERROR_PIPE_BUSY,
    ERROR_SHARING_VIOLATION, GENERIC_READ, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{OWNER_SECURITY_INFORMATION, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_SHARE_READ, FileDispositionInfo,
    FileDispositionInfoEx, SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::CancelIoEx;
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;

use super::Backoff;

/// What every pipe name starts with; 32 lowercase hex digits follow.
const PREFIX: &str = r"\\.\pipe\amux-";

/// Pipe instances a server keeps listening, so clients arriving together
/// find one free while the last is being accepted.
pub(super) const INSTANCES: usize = 4;

/// Names a bind draws before it gives up, each one found taken.
const DRAWS: usize = 8;

/// The longest a connecting client waits between tries.
const MOST_PAUSE: Duration = Duration::from_millis(50);

/// Either end of a connected pipe.
pub enum Stream {
    Server(NamedPipeServer),
    Client(NamedPipeClient),
}

impl Stream {
    fn handle(&self) -> RawHandle {
        match self {
            Stream::Server(server) => server.as_raw_handle(),
            Stream::Client(client) => client.as_raw_handle(),
        }
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Server(server) => Pin::new(server).poll_read(cx, buf),
            Stream::Client(client) => Pin::new(client).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Server(server) => Pin::new(server).poll_write(cx, buf),
            Stream::Client(client) => Pin::new(client).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Server(server) => Pin::new(server).poll_flush(cx),
            Stream::Client(client) => Pin::new(client).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Server(server) => Pin::new(server).poll_shutdown(cx),
            Stream::Client(client) => Pin::new(client).poll_shutdown(cx),
        }
    }
}

/// Cancels what is still pending on the pipe, then closes it. Dropping
/// alone would leave a write the peer has no room for pending, and the
/// handle open with it, so a peer that stopped reading would never see the
/// end. Bytes the peer had room for are already in the pipe and still
/// arrive, then end of file.
pub fn close(stream: Stream) {
    // SAFETY: the handle is the stream's own and open until it drops below;
    // cancelling its pending operations is what the stream's own drop does
    // for reads, and the writes complete as cancelled.
    unsafe { CancelIoEx(stream.handle() as _, std::ptr::null()) };
    drop(stream);
}

/// A pipe instance waiting for its client.
type Listening = Pin<Box<dyn Future<Output = (NamedPipeServer, io::Result<()>)> + Send>>;

fn listen(server: NamedPipeServer) -> Listening {
    Box::pin(async move {
        let connected = server.connect().await;
        (server, connected)
    })
}

pub struct Listener {
    name: String,
    /// A mutex only so the listener is `Sync`, as it always was (the
    /// futures are `Send` alone); reached through `get_mut`, never locked.
    listening: Mutex<Vec<Listening>>,
    backoff: Backoff,
    /// While making an instance fails: when to try again.
    retry: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl Listener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        Self::bind_drawing(path, std::iter::repeat_with(random_name).take(DRAWS))
    }

    /// Binds `path` to the first of `names` nobody holds yet.
    pub(super) fn bind_drawing(
        path: &Path,
        names: impl IntoIterator<Item = String>,
    ) -> io::Result<Self> {
        let mut taken = None;
        for name in names {
            // First instance: a name someone else already holds is refused,
            // never joined.
            match create(ServerOptions::new().first_pipe_instance(true), &name) {
                Ok(first) => return Self::publish(path, name, first),
                Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
                    tracing::warn!(%name, "a pipe name was taken already; drawing another");
                    taken = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(taken.unwrap_or_else(|| io::Error::other("no pipe name to bind")))
    }

    /// Fills the listening instances, then names the pipe in the file.
    fn publish(path: &Path, name: String, first: NamedPipeServer) -> io::Result<Self> {
        let mut listening = vec![listen(first)];
        for _ in 1..INSTANCES {
            match create(&mut ServerOptions::new(), &name) {
                Ok(server) => listening.push(listen(server)),
                // Accepting makes the rest.
                Err(error) => {
                    tracing::warn!(%error, %name, "making a pipe instance failed");
                    break;
                }
            }
        }
        // Named once the pipe listens, and replaced whole, so a client
        // reads either the old name or this one.
        static STAGED: AtomicU64 = AtomicU64::new(0);
        let mut staged = OsString::from(path.as_os_str());
        staged.push(format!(
            ".{}-{}.tmp",
            std::process::id(),
            STAGED.fetch_add(1, Ordering::Relaxed)
        ));
        let staged = PathBuf::from(staged);
        let published =
            std::fs::write(&staged, &name).and_then(|()| os::fs::rename_patiently(&staged, path));
        if let Err(error) = published {
            let _ = std::fs::remove_file(&staged);
            return Err(error);
        }
        Ok(Self {
            name,
            listening: Mutex::new(listening),
            backoff: Backoff::new(),
            retry: None,
        })
    }

    pub async fn accept(&mut self) -> Stream {
        std::future::poll_fn(|cx| self.poll_accept(cx)).await
    }

    fn poll_accept(&mut self, cx: &mut Context<'_>) -> Poll<Stream> {
        loop {
            self.refill(cx);
            let instances = listening(&mut self.listening);
            let ready = instances.iter_mut().enumerate().find_map(|(at, waiting)| {
                match waiting.as_mut().poll(cx) {
                    Poll::Ready(done) => Some((at, done)),
                    Poll::Pending => None,
                }
            });
            let Some((at, (server, connected))) = ready else {
                return Poll::Pending;
            };
            // Its place is refilled before the next connection is awaited,
            // and a failure to refill never costs this one.
            drop(instances.swap_remove(at));
            match connected {
                Ok(()) => return Poll::Ready(Stream::Server(server)),
                // The client came and went before it was accepted: an
                // instance used up, nothing more.
                Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA as i32) => {}
                Err(error) => {
                    tracing::warn!(%error, name = %self.name, "a pipe instance failed to connect");
                    self.back_off();
                }
            }
        }
    }

    /// Makes instances until [`INSTANCES`] listen, unless it is backing
    /// off after a failure; a failure starts (or lengthens) the backoff,
    /// whose timer wakes `cx`.
    fn refill(&mut self, cx: &mut Context<'_>) {
        while listening(&mut self.listening).len() < INSTANCES {
            if let Some(retry) = self.retry.as_mut() {
                if retry.as_mut().poll(cx).is_pending() {
                    return;
                }
                self.retry = None;
            }
            match create(&mut ServerOptions::new(), &self.name) {
                Ok(server) => {
                    self.backoff.reset();
                    listening(&mut self.listening).push(listen(server));
                }
                Err(error) => {
                    tracing::warn!(%error, name = %self.name, "making a pipe instance failed; retrying");
                    self.back_off();
                }
            }
        }
    }

    fn back_off(&mut self) {
        if self.retry.is_none() {
            self.retry = Some(Box::pin(tokio::time::sleep(self.backoff.pause())));
        }
    }

    /// Removes the file at `path` if it still names this listener's pipe.
    /// It is opened so nothing can rename over it or delete it, read, and
    /// deleted through the same handle, so a successor's file, written
    /// before or after, survives.
    pub fn unbind(&self, path: &Path) {
        let started = Instant::now();
        let mut pause = Duration::from_millis(5);
        let mut file = loop {
            match OpenOptions::new()
                .access_mode(GENERIC_READ | DELETE)
                .share_mode(FILE_SHARE_READ)
                .open(path)
            {
                Ok(file) => break file,
                Err(error)
                    if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32)
                        && started.elapsed() < os::fs::SHARING_PATIENCE =>
                {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(Duration::from_millis(100));
                }
                Err(_) => return,
            }
        };
        let mut named = String::new();
        if file.read_to_string(&mut named).is_err() || named != self.name {
            return;
        }
        if let Err(error) = delete_open(&file) {
            tracing::warn!(%error, path = %path.display(), "removing a pipe's name file failed");
        }
    }
}

/// Deletes the open `file`: its name goes at once where the volume allows
/// (POSIX semantics), else when the handle closes, just after.
fn delete_open(file: &std::fs::File) -> io::Result<()> {
    let handle = file.as_raw_handle() as _;
    let posix = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    };
    // SAFETY: the handle is open with DELETE access; the struct outlives
    // the call, which reads `size_of` bytes of it.
    let done = unsafe {
        SetFileInformationByHandle(
            handle,
            FileDispositionInfoEx,
            (&raw const posix).cast(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    } != 0;
    if done {
        return Ok(());
    }
    let plain = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: as above.
    let done = unsafe {
        SetFileInformationByHandle(
            handle,
            FileDispositionInfo,
            (&raw const plain).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } != 0;
    if done {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Opens the pipe named in the file at `path`, reading the file afresh on
/// each try, until `patience` runs out; see `connect_within`.
pub async fn connect(path: &Path, patience: Duration) -> io::Result<(Stream, Option<u32>)> {
    let until = Instant::now() + patience;
    let mut pause = Duration::from_millis(5);
    let mut tried = false;
    loop {
        let waiting = match std::fs::read_to_string(path) {
            Ok(name) => {
                if !is_amux_pipe(&name) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{} names {name:?}, which is no amux pipe", path.display()),
                    ));
                }
                match ClientOptions::new().open(&name) {
                    Ok(client) => {
                        let pid = check(&client, &name)?;
                        return Ok((Stream::Client(client), Some(pid)));
                    }
                    Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                        io::ErrorKind::TimedOut
                    }
                    Err(error) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {
                        io::ErrorKind::ConnectionRefused
                    }
                    Err(error) => return Err(error),
                }
            }
            // No socket at all is no socket, unless one was there a moment
            // ago: a predecessor unbinding before its successor binds.
            Err(error) if error.kind() == io::ErrorKind::NotFound && tried => {
                io::ErrorKind::ConnectionRefused
            }
            Err(error) => return Err(error),
        };
        tried = true;
        let now = Instant::now();
        if now >= until {
            let what = match waiting {
                io::ErrorKind::TimedOut => "every pipe instance stayed busy",
                _ => "no pipe answered",
            };
            return Err(io::Error::new(
                waiting,
                format!("{}: {what} for {patience:?}", path.display()),
            ));
        }
        tokio::time::sleep(pause.min(until - now)).await;
        pause = (pause * 2).min(MOST_PAUSE);
    }
}

/// Whether `name` is a pipe an amux bind makes: `\\.\pipe\amux-` and 32
/// lowercase hex digits, or the form earlier builds made,
/// `\\.\pipe\amux-<16 hex>-<pid>-<count>`, which a daemon still meets in
/// an agent an earlier build started. Nothing else: no other host (a UNC
/// name would hand this user's credentials to it), no device, no other
/// pipe.
pub(super) fn is_amux_pipe(name: &str) -> bool {
    let hex = |part: &str, length: usize| {
        part.len() == length && part.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    };
    let number =
        |part: &str| (1..=20).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit());
    let Some(rest) = name.strip_prefix(PREFIX) else {
        return false;
    };
    if hex(rest, 32) {
        return true;
    }
    let mut parts = rest.split('-');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(hash), Some(pid), Some(count), None) if hex(hash, 16) && number(pid) && number(count)
    )
}

/// `\\.\pipe\amux-` and 128 bits no other process can guess, so nobody can
/// take a pipe's name before its bind does.
///
/// The bits come from std's `RandomState`, whose keys the OS's random
/// source draws: SipHash under a secret key is a keyed pseudorandom
/// function, so its output is as unguessable as the key. Two hashers, two
/// keys, 64 bits each.
pub(super) fn random_name() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    static DRAWN: AtomicU64 = AtomicU64::new(0);
    let drawn = DRAWN.fetch_add(1, Ordering::Relaxed);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let half = |side: u8| {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u8(side);
        hasher.write_u64(drawn);
        hasher.write_u32(std::process::id());
        hasher.write_u128(now);
        hasher.finish()
    };
    format!("{PREFIX}{:016x}{:016x}", half(0), half(1))
}

/// The pipe a client opened is owned by this user, and the id of the
/// process serving it.
///
/// The owner is what a server can't fake: another user can't create a pipe
/// owned by this one, while this user's own daemon, elevated or not, makes
/// every pipe owned by this user (its SDDL names the owner). The server's
/// process token is no test: an elevated daemon's pipes would fail it.
fn check(client: &NamedPipeClient, name: &str) -> io::Result<u32> {
    if !owned_by_me(client.as_raw_handle())? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{name} is owned by another user"),
        ));
    }
    let mut pid = 0u32;
    // SAFETY: the handle is the open pipe client's, and `pid` outlives
    // the call.
    if unsafe { GetNamedPipeServerProcessId(client.as_raw_handle() as _, &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
}

/// Whether the object open at `handle` (with `READ_CONTROL`) is owned by
/// this user.
pub(super) fn owned_by_me(handle: RawHandle) -> io::Result<bool> {
    let mut owner = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: the handle is open; the call allocates the descriptor `owner`
    // points into, freed below once the owner is read.
    let status = unsafe {
        GetSecurityInfo(
            handle as _,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let mut text = std::ptr::null_mut();
    // SAFETY: `owner` points into the live descriptor; the string is
    // allocated by the call and freed below once copied.
    let converted = unsafe { ConvertSidToStringSidW(owner, &mut text) } != 0;
    let owner = if converted {
        // SAFETY: the call wrote a NUL-terminated UTF-16 string.
        let owner = unsafe {
            let length = (0..).take_while(|&at| *text.add(at) != 0).count();
            String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
        };
        // SAFETY: allocated by ConvertSidToStringSidW, not used again.
        unsafe { LocalFree(text.cast()) };
        Ok(owner)
    } else {
        Err(io::Error::last_os_error())
    };
    // SAFETY: allocated by GetSecurityInfo; `owner` is not read again.
    unsafe { LocalFree(descriptor) };
    Ok(owner? == os::fs::current_user_sid()?)
}

/// A server end that admits this user alone, local clients only.
fn create(options: &mut ServerOptions, name: &str) -> io::Result<NamedPipeServer> {
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: only_me()? as *mut _,
        bInheritHandle: 0,
    };
    options.reject_remote_clients(true);
    // SAFETY: the attributes and the descriptor they point at outlive
    // the call, which copies what it needs.
    unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast()) }
}

/// A security descriptor owned by this user, granting it alone full
/// access, protected, so nothing is inherited and nobody else is named.
/// Made once and kept for the process's life: a process's user never
/// changes.
fn only_me() -> io::Result<usize> {
    static DESCRIPTOR: OnceLock<Result<usize, String>> = OnceLock::new();
    DESCRIPTOR
        .get_or_init(|| {
            let sid = os::fs::current_user_sid().map_err(|error| error.to_string())?;
            let sddl: Vec<u16> = format!("O:{sid}D:P(A;;GA;;;{sid})")
                .encode_utf16()
                .chain([0])
                .collect();
            let mut descriptor = std::ptr::null_mut();
            // SAFETY: `sddl` is NUL-terminated; the descriptor is allocated
            // by the call and kept for the process's life.
            let made = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            };
            if made == 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            Ok(descriptor as usize)
        })
        .clone()
        .map_err(|error| io::Error::other(format!("making the pipe's security: {error}")))
}

/// Nothing to forget: a pipe ends with its last handle.
pub fn forget(_dir: &Path) {}

fn listening(listening: &mut Mutex<Vec<Listening>>) -> &mut Vec<Listening> {
    listening
        .get_mut()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
