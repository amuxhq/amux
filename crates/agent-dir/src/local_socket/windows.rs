//! Named pipes: a fresh, randomly named pipe per bind, its name in the file
//! at the socket's path, admitting this user alone.

use std::fs::OpenOptions;
use std::future::Future;
use std::io::{self, Read};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::path::Path;
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
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_LOCK_VIOLATION, ERROR_NO_DATA,
    ERROR_PIPE_BUSY, ERROR_SHARING_VIOLATION, GENERIC_READ, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{OWNER_SECURITY_INFORMATION, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileDispositionInfo, FileDispositionInfoEx, SetFileInformationByHandle,
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
        // reads either the old name or this one. The staging, the patient
        // rename and the cleanup of a stage are os::fs's, shared with every
        // whole-file write.
        os::fs::replace_atomically(path, name.as_bytes())?;
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
            // and a failure to refill never costs this one. The rest keep
            // their order, oldest first, and the refill goes last: a client
            // waiting on an older instance is served before newer ones,
            // however fast they come.
            drop(instances.remove(at));
            // And refilled now, so a client that comes before the next
            // accept finds an instance listening, not every one taken.
            self.refill(cx);
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
    // What the last try met, once there was one.
    let mut last: Option<Waiting> = None;
    loop {
        let tried = last.is_some();
        // A zero deadline is one try, whose read has LEAST_READ; any other
        // read ends by the caller's deadline.
        let read_until = if !tried && patience.is_zero() {
            Instant::now() + LEAST_READ
        } else {
            until
        };
        let named = match read_name(path, read_until).await {
            Ok(named) => named,
            // A retry the deadline cut short: the answer is what the try
            // before it met.
            Err(error)
                if error.kind() == io::ErrorKind::TimedOut
                    && let Some(waiting) = &last =>
            {
                return Err(given_up(path, patience, waiting));
            }
            Err(error) => return Err(error),
        };
        let waiting = match named {
            Named::Name(name) => {
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
                        Waiting::Busy
                    }
                    Err(error) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {
                        Waiting::Gone
                    }
                    Err(error) => return Err(error),
                }
            }
            // No socket at all is no socket, unless one was there a moment
            // ago: a predecessor unbinding before its successor binds.
            Named::Missing if tried => Waiting::Gone,
            Named::Missing => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no socket at {}", path.display()),
                ));
            }
            // A scanner or an indexer holding it a moment.
            Named::Held => Waiting::Held,
        };
        let now = Instant::now();
        if now >= until {
            return Err(given_up(path, patience, &waiting));
        }
        tokio::time::sleep(pause.min(until - now)).await;
        pause = (pause * 2).min(MOST_PAUSE);
        // A try that would start at the deadline is none: what this one
        // met is the answer.
        if Instant::now() >= until {
            return Err(given_up(path, patience, &waiting));
        }
        last = Some(waiting);
    }
}

/// What a connect that ran out of patience on `waiting` says.
fn given_up(path: &Path, patience: Duration, waiting: &Waiting) -> io::Error {
    let (kind, what) = match waiting {
        Waiting::Busy => (
            io::ErrorKind::ResourceBusy,
            "every pipe instance stayed busy",
        ),
        Waiting::Held => (io::ErrorKind::TimedOut, "the name file stayed held"),
        Waiting::Gone => (io::ErrorKind::ConnectionRefused, "no pipe answered"),
    };
    io::Error::new(kind, format!("{}: {what} for {patience:?}", path.display()))
}

/// Why a connect tries again.
enum Waiting {
    Busy,
    Held,
    Gone,
}

/// What reading a name file came to.
enum Named {
    Name(String),
    Missing,
    /// Something else has it open without sharing reads: a moment's wait.
    Held,
}

/// The most of a name file a client reads: a name is 46 bytes, and a file
/// longer than this names no pipe.
const MOST_NAME: u64 = 256;

/// What the one read of a connect with no patience at all is given, so a
/// deadline of zero is one try, not none. Every other read ends by the
/// caller's deadline.
const LEAST_READ: Duration = Duration::from_millis(100);

/// Reads of name files that may be outstanding at once. A read is
/// abandoned at its deadline but its thread waits on, on a stalled disk
/// (a network share gone quiet), so a connect past this many waits for a
/// slot within its own deadline instead of tying up more threads. The
/// threads are our own, not tokio's blocking pool: a runtime waits for
/// its blocking tasks when it shuts down, and a stalled read would hold
/// `amux status` open after its connect gave up.
const MOST_READING: usize = 4;

/// One slot for each read of a name file under way, held by its thread.
static READING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(MOST_READING);

/// Reads the name file at `path` on a thread of its own, giving up at
/// `until`, the wait for a slot among [`MOST_READING`] included.
async fn read_name(path: &Path, until: Instant) -> io::Result<Named> {
    let slot = match tokio::time::timeout_at(until.into(), READING.acquire()).await {
        Ok(Ok(slot)) => slot,
        Ok(Err(closed)) => return Err(io::Error::other(closed)),
        Err(_) => {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "{}: earlier reads of socket files have not finished",
                    path.display()
                ),
            ));
        }
    };
    let file = path.to_owned();
    let (answer, reading) = tokio::sync::oneshot::channel();
    // Detached: nothing waits for it but this connect, which may stop.
    std::thread::Builder::new()
        .name("amux-name-file".into())
        .spawn(move || {
            let _slot = slot;
            #[cfg(test)]
            tests::stall(&file);
            let _ = answer.send(read_name_now(&file));
        })?;
    let late = || {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("{}: reading it did not finish in time", path.display()),
        )
    };
    match tokio::time::timeout_at(until.into(), reading).await {
        // An answer seen only after the deadline (this task ran late, and
        // a timeout looks at what it waits on before the clock) is late.
        Ok(Ok(_)) if Instant::now() > until => Err(late()),
        Ok(Ok(named)) => named,
        Ok(Err(_)) => Err(io::Error::other(format!(
            "{}: the read ended without an answer",
            path.display()
        ))),
        Err(_) => Err(late()),
    }
}

fn read_name_now(path: &Path) -> io::Result<Named> {
    let held = |error: &io::Error| {
        [ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION]
            .iter()
            .any(|&code| error.raw_os_error() == Some(code as i32))
    };
    let file = match OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Named::Missing),
        Err(error) if held(&error) => return Ok(Named::Held),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    match file.take(MOST_NAME + 1).read_to_end(&mut bytes) {
        Ok(_) => {}
        Err(error) if held(&error) => return Ok(Named::Held),
        Err(error) => return Err(error),
    }
    if bytes.len() as u64 > MOST_NAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is too long to name a pipe", path.display()),
        ));
    }
    String::from_utf8(bytes).map(Named::Name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} names no pipe: it is not text", path.display()),
        )
    })
}

/// Whether `name` is a pipe an amux bind makes: `\\.\pipe\amux-` and 32
/// lowercase hex digits. Nothing else: no other host (a UNC name would
/// hand this user's credentials to it), no device, no other pipe.
pub(super) fn is_amux_pipe(name: &str) -> bool {
    name.strip_prefix(PREFIX).is_some_and(|rest| {
        rest.len() == 32 && rest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
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

/// The pipe a client opened is this user's, and the id of the process
/// serving it.
///
/// An amux pipe names this user as its owner (its SDDL does), elevated or
/// not, and the owner is what a server can't fake: another user can't
/// create a pipe owned by this one. So the owner alone decides.
fn check(client: &NamedPipeClient, name: &str) -> io::Result<u32> {
    let mut pid = 0u32;
    // SAFETY: the handle is the open pipe client's, and `pid` outlives
    // the call.
    if unsafe { GetNamedPipeServerProcessId(client.as_raw_handle() as _, &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let owner = owner_of(client.as_raw_handle())?;
    let me = os::fs::current_user_sid()?;
    if owner == me {
        return Ok(pid);
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{name} is owned by {owner}, not by this user ({me})"),
    ))
}

/// Whether the object open at `handle` (with `READ_CONTROL`) is owned by
/// this user.
#[cfg(test)]
pub(super) fn owned_by_me(handle: RawHandle) -> io::Result<bool> {
    Ok(owner_of(handle)? == os::fs::current_user_sid()?)
}

/// A SID as its string form.
fn sid_text(sid: *mut std::ffi::c_void) -> io::Result<String> {
    let mut text = std::ptr::null_mut();
    // SAFETY: `sid` is a valid SID; the string is allocated by the call and
    // freed below once copied.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call wrote a NUL-terminated UTF-16 string.
    let string = unsafe {
        let length = (0..).take_while(|&at| *text.add(at) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
    };
    // SAFETY: allocated by ConvertSidToStringSidW, not used again.
    unsafe { LocalFree(text.cast()) };
    Ok(string)
}

/// The owner of the object open at `handle` (with `READ_CONTROL`), as a
/// SID string.
fn owner_of(handle: RawHandle) -> io::Result<String> {
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
    // `owner` points into the live descriptor.
    let text = sid_text(owner);
    // SAFETY: allocated by GetSecurityInfo; `owner` is not read again.
    unsafe { LocalFree(descriptor) };
    text
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

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
