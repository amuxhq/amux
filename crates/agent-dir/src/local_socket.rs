//! Local sockets named by a path in the agent directory: Unix domain
//! sockets on Unix, named pipes on Windows.
//!
//! A socket's name is its path (`agents/<id>/ctl.sock`); what the operating
//! system binds is derived from it. On Unix that is the path itself unless
//! it is too long for `sockaddr_un`, which agent directories under a deep
//! data directory easily are; then the socket is reached through a short
//! symbolic link to its directory in a per-user runtime directory, so the
//! file still lives at its path. On Windows the file at the path names a
//! pipe that each bind makes afresh: a pipe lives on while any connection to
//! it is open, so a successor could not take its predecessor's name, just as
//! a Unix bind replaces the file and leaves old connections to finish. A
//! path already in the pipe namespace is that pipe itself.

use std::io;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncRead, AsyncWrite};

/// A connected local socket.
pub trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

pub type LocalStream = Box<dyn Io>;

/// Listens on the socket named by a path; removes it when dropped.
pub struct LocalListener {
    path: PathBuf,
    inner: imp::Listener,
}

impl LocalListener {
    /// Binds `path`, replacing a socket a dead predecessor left behind. The
    /// caller holds the directory's lock, so nothing live owns it.
    pub fn bind(path: &Path) -> io::Result<Self> {
        let path = std::path::absolute(path)?;
        Ok(Self {
            inner: imp::Listener::bind(&path)?,
            path,
        })
    }

    pub async fn accept(&mut self) -> io::Result<LocalStream> {
        self.inner.accept().await
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LocalListener {
    fn drop(&mut self) {
        imp::unbind(&self.path);
    }
}

/// Connects to the socket named by `path`.
pub async fn connect(path: &Path) -> io::Result<LocalStream> {
    imp::connect(&std::path::absolute(path)?).await
}

/// Connects to the socket named by `path`, with the id of the process
/// that answered, as the operating system names it, when it can.
///
/// The process on the other end is the one serving the socket, whoever
/// started it, so a daemon that adopted an agent from an earlier daemon
/// still knows which process to end when the agent does not answer.
pub async fn connect_to_process(path: &Path) -> io::Result<(LocalStream, Option<u32>)> {
    imp::connect_to_process(&std::path::absolute(path)?).await
}

/// The path a Unix socket named `path` is bound and dialled at: the path
/// itself, or one through a short link when it is too long.
#[cfg(unix)]
pub fn unix_address(path: &Path) -> io::Result<PathBuf> {
    imp::address(&std::path::absolute(path)?)
}

/// The path to hand a program that binds the socket itself and insists its
/// directory be a real, private one (terminal Claude's messaging socket):
/// the path itself when it fits, else the same name in a real directory,
/// only this user's, in the per-user runtime directory. Nothing but the
/// program's own announcement of it leads there, so it needs no link.
#[cfg(unix)]
pub fn unix_private_address(path: &Path) -> io::Result<PathBuf> {
    imp::private_address(&std::path::absolute(path)?)
}

/// Removes the short addresses made for sockets in `dir` and its private
/// directory, which go with the directory itself: the link to each, and
/// the real directory a program binding its own socket was given. Nothing
/// to do on Windows, where a pipe ends with its last handle.
pub fn forget_short_addresses(dir: &Path) {
    #[cfg(unix)]
    if let Ok(dir) = std::path::absolute(dir) {
        imp::forget(&dir);
        imp::forget(&dir.join(crate::PRIVATE));
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// FNV-1a: stable across processes and versions, which is all a derived
/// address needs.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(unix)]
mod imp {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::{fs, io};

    use tokio::net::{UnixListener, UnixStream};

    use super::{LocalStream, fnv};

    pub struct Listener(UnixListener);

    impl Listener {
        pub fn bind(path: &Path) -> io::Result<Self> {
            let address = address(path)?;
            match fs::remove_file(&address) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            Ok(Self(UnixListener::bind(address)?))
        }

        pub async fn accept(&mut self) -> io::Result<LocalStream> {
            let (stream, _) = self.0.accept().await?;
            Ok(Box::new(stream))
        }
    }

    pub fn unbind(path: &Path) {
        let _ = fs::remove_file(path);
    }

    pub async fn connect(path: &Path) -> io::Result<LocalStream> {
        Ok(Box::new(UnixStream::connect(address(path)?).await?))
    }

    /// The peer's pid comes from the socket itself: `SO_PEERCRED` on Linux,
    /// `LOCAL_PEEREPID` on macOS.
    pub async fn connect_to_process(path: &Path) -> io::Result<(LocalStream, Option<u32>)> {
        let stream = UnixStream::connect(address(path)?).await?;
        let pid = stream
            .peer_cred()
            .ok()
            .and_then(|cred| cred.pid())
            .and_then(|pid| u32::try_from(pid).ok());
        Ok((Box::new(stream), pid))
    }

    /// The path to bind or dial for the socket named `path`.
    pub fn address(path: &Path) -> io::Result<PathBuf> {
        // SAFETY: sockaddr_un is plain old data; zeroed is a valid value.
        let limit = unsafe { std::mem::zeroed::<libc::sockaddr_un>() }
            .sun_path
            .len();
        if path.as_os_str().as_bytes().len() < limit {
            return Ok(path.to_owned());
        }
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} names no socket", path.display()),
            ));
        };
        let link = runtime_dir()?.join(format!("{:016x}", fnv(dir.as_os_str().as_bytes())));
        loop {
            match fs::read_link(&link) {
                Ok(target) if target == dir => break,
                Ok(_) => fs::remove_file(&link)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match std::os::unix::fs::symlink(dir, &link) {
                        Ok(()) => break,
                        // Someone else made it first; check what they made.
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            }
        }
        let address = link.join(name);
        if address.as_os_str().as_bytes().len() >= limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("no short address fits {}", path.display()),
            ));
        }
        Ok(address)
    }

    pub fn private_address(path: &Path) -> io::Result<PathBuf> {
        if path.as_os_str().as_bytes().len() < limit() {
            return Ok(path.to_owned());
        }
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} names no socket", path.display()),
            ));
        };
        let short = runtime_dir()?.join(format!("{:016x}.d", fnv(dir.as_os_str().as_bytes())));
        private_dir(&short)?;
        let address = short.join(name);
        if address.as_os_str().as_bytes().len() >= limit() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("no short address fits {}", path.display()),
            ));
        }
        Ok(address)
    }

    /// Removes the short link to `dir` and its private short directory, if
    /// either was ever made.
    pub fn forget(dir: &Path) {
        let Ok(runtime) = runtime_dir() else { return };
        let hash = fnv(dir.as_os_str().as_bytes());
        let link = runtime.join(format!("{hash:016x}"));
        if fs::read_link(&link).is_ok_and(|target| target == dir) {
            let _ = fs::remove_file(&link);
        }
        let _ = fs::remove_dir_all(runtime.join(format!("{hash:016x}.d")));
    }

    fn limit() -> usize {
        // SAFETY: sockaddr_un is plain old data; zeroed is a valid value.
        unsafe { std::mem::zeroed::<libc::sockaddr_un>() }
            .sun_path
            .len()
    }

    /// A directory only this user can write, for the short links.
    fn runtime_dir() -> io::Result<PathBuf> {
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        let dir = std::env::temp_dir().join(format!("amux-{uid}"));
        private_dir(&dir)?;
        Ok(dir)
    }

    /// Makes `dir` if it is missing, and refuses it unless it is a real
    /// directory this user owns that no one else can use.
    fn private_dir(dir: &Path) -> io::Result<()> {
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        match fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = fs::symlink_metadata(dir)?;
        if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not a private directory", dir.display()),
            ));
        }
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    use super::{LocalStream, fnv};

    /// ERROR_PIPE_BUSY: every instance is taken; another comes shortly.
    const PIPE_BUSY: i32 = 231;

    /// Binds made by this process, so no two of its pipes share a name.
    static BINDS: AtomicU64 = AtomicU64::new(0);

    pub struct Listener {
        name: String,
        next: NamedPipeServer,
    }

    impl Listener {
        pub fn bind(path: &Path) -> io::Result<Self> {
            let name = direct(path).unwrap_or_else(|| pipe_name(path));
            // First instance: a name someone else already holds is refused,
            // never joined.
            let next = create(ServerOptions::new().first_pipe_instance(true), &name)?;
            if direct(path).is_some() {
                return Ok(Self { name, next });
            }
            // Named once the pipe listens, and replaced whole, so a client
            // reads either the old name or this one.
            let mut staged = path.as_os_str().to_owned();
            staged.push(format!(".{}-{}", std::process::id(), bind_number()));
            std::fs::write(&staged, &name)?;
            std::fs::rename(&staged, path)?;
            Ok(Self { name, next })
        }

        pub async fn accept(&mut self) -> io::Result<LocalStream> {
            self.next.connect().await?;
            let fresh = create(&mut ServerOptions::new(), &self.name)?;
            let connected = std::mem::replace(&mut self.next, fresh);
            Ok(Box::new(connected))
        }
    }

    pub fn unbind(path: &Path) {
        if direct(path).is_none() {
            let _ = std::fs::remove_file(path);
        }
    }

    pub async fn connect(path: &Path) -> io::Result<LocalStream> {
        Ok(Box::new(open(path).await?.0))
    }

    /// The peer's pid comes from the pipe: the process that created the
    /// server end this client opened.
    pub async fn connect_to_process(path: &Path) -> io::Result<(LocalStream, Option<u32>)> {
        let (client, pid) = open(path).await?;
        Ok((Box::new(client), Some(pid)))
    }

    /// A pipe served by this user's own process, and that process's id.
    /// Pipe names are predictable, so another user on the machine could
    /// create one first and answer as amux: a pipe whose server runs as
    /// anyone else is refused.
    async fn open(path: &Path) -> io::Result<(NamedPipeClient, u32)> {
        use std::os::windows::io::AsRawHandle;

        let name = match direct(path) {
            Some(name) => name,
            None => std::fs::read_to_string(path)?,
        };
        let client = loop {
            match ClientOptions::new().open(&name) {
                Ok(client) => break client,
                Err(error) if error.raw_os_error() == Some(PIPE_BUSY) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error),
            }
        };
        let mut pid = 0u32;
        // SAFETY: the handle is the open pipe client's, and `pid` outlives
        // the call.
        let known = unsafe {
            windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId(
                client.as_raw_handle() as _,
                &mut pid,
            )
        } != 0;
        if !known {
            return Err(io::Error::last_os_error());
        }
        if !user::runs_as_me(pid)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{name} is served by another user's process"),
            ));
        }
        Ok((client, pid))
    }

    /// A server end that admits this user alone, local clients only.
    fn create(options: &mut ServerOptions, name: &str) -> io::Result<NamedPipeServer> {
        let mut attributes = user::only_me()?;
        options.reject_remote_clients(true);
        // SAFETY: the attributes and the descriptor they point at outlive
        // the call, which copies what it needs.
        unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast()) }
    }

    /// A path already in the pipe namespace (the front door's default,
    /// `\\.\pipe\amux-<user>`) is that pipe; no file can stand there.
    fn direct(path: &Path) -> Option<String> {
        let path = path.to_str()?;
        path.get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(r"\\.\pipe\"))
            .then(|| path.to_owned())
    }

    fn bind_number() -> u64 {
        BINDS.fetch_add(1, Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(super) fn runs_as_me_for_test(pid: u32) -> io::Result<bool> {
        user::runs_as_me(pid)
    }

    /// Who this process runs as, and the pipe security that follows.
    mod user {
        use std::io;
        use std::sync::OnceLock;

        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
        use windows_sys::Win32::Security::Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::{
            GetLengthSid, GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        };
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        /// This user's SID, and a security descriptor granting it alone
        /// full access. Read once: a process's user never changes.
        struct Me {
            sid: Vec<u8>,
            descriptor: usize,
        }

        fn me() -> io::Result<&'static Me> {
            static ME: OnceLock<Result<Me, String>> = OnceLock::new();
            ME.get_or_init(|| {
                // SAFETY: the pseudo handle needs no closing.
                let (sid, text) =
                    sid_of(unsafe { GetCurrentProcess() }).map_err(|error| error.to_string())?;
                // Protected, so nothing is inherited; this user may do
                // anything; nobody else is named, so nobody else may open it.
                let sddl: Vec<u16> = format!("D:P(A;;GA;;;{text})")
                    .encode_utf16()
                    .chain([0])
                    .collect();
                let mut descriptor = std::ptr::null_mut();
                // SAFETY: `sddl` is NUL-terminated; the descriptor is
                // allocated by the call and kept for the process's life.
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
                Ok(Me {
                    sid,
                    descriptor: descriptor as usize,
                })
            })
            .as_ref()
            .map_err(|error| io::Error::other(format!("reading this user's identity: {error}")))
        }

        /// Security attributes admitting this user alone.
        pub(super) fn only_me() -> io::Result<SECURITY_ATTRIBUTES> {
            Ok(SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: me()?.descriptor as *mut _,
                bInheritHandle: 0,
            })
        }

        /// Whether process `pid` runs as this user. One that cannot be
        /// asked, another user's, is not.
        pub(super) fn runs_as_me(pid: u32) -> io::Result<bool> {
            let me = me()?;
            // SAFETY: a failed open returns null, checked; an opened
            // handle is closed below.
            let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            if process.is_null() {
                return Ok(false);
            }
            let theirs = sid_of(process);
            // SAFETY: the handle was opened above and is not used again.
            unsafe { CloseHandle(process) };
            Ok(theirs.is_ok_and(|(sid, _)| sid == me.sid))
        }

        /// The user a process runs as: its SID's bytes, and its string form.
        fn sid_of(process: HANDLE) -> io::Result<(Vec<u8>, String)> {
            let mut token = std::ptr::null_mut();
            // SAFETY: `token` outlives the call; closed below once opened.
            if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let read = token_user(token);
            // SAFETY: the token was opened above and is not used again.
            unsafe { CloseHandle(token) };
            read
        }

        fn token_user(token: HANDLE) -> io::Result<(Vec<u8>, String)> {
            let mut needed = 0u32;
            // SAFETY: a size query: no buffer, the size written to `needed`.
            unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
            // u64s, so the TOKEN_USER read out of it is aligned.
            let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
            // SAFETY: the buffer holds at least `needed` bytes.
            let read = unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    needed,
                    &mut needed,
                )
            };
            if read == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the call wrote a TOKEN_USER at the buffer's start, and
            // the SID it points at lies within the buffer, which lives on.
            let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
            // SAFETY: `sid` is a valid SID while `buffer` lives.
            let bytes = unsafe {
                std::slice::from_raw_parts(sid.cast::<u8>(), GetLengthSid(sid) as usize).to_vec()
            };
            let mut text = std::ptr::null_mut();
            // SAFETY: `sid` is valid; the string is allocated by the call and
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
            Ok((bytes, string))
        }
    }

    /// Unique among live pipes: the path's hash for whoever reads a pipe
    /// list, then this process and its count of binds.
    fn pipe_name(path: &Path) -> String {
        let wide: Vec<u8> = path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect();
        format!(
            r"\\.\pipe\amux-{:016x}-{}-{}",
            fnv(&wide),
            std::process::id(),
            bind_number()
        )
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// A pipe this process serves, under the descriptor admitting only this
    /// user, is opened by this user and its server is found to be this
    /// user's own.
    #[tokio::test]
    async fn a_pipe_admits_its_own_user_and_its_server_is_checked() {
        assert!(imp::runs_as_me_for_test(std::process::id()).unwrap());
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ctl.sock");
        let mut listener = LocalListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            stream.write_all(b"ok").await.unwrap();
        });
        let (mut client, pid) = connect_to_process(&path).await.unwrap();
        assert_eq!(pid, Some(std::process::id()));
        let mut answer = [0u8; 2];
        client.read_exact(&mut answer).await.unwrap();
        assert_eq!(&answer, b"ok");
        server.await.unwrap();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn a_socket_whose_path_is_too_long_still_lives_at_its_path() {
        let root = tempfile::tempdir().unwrap();
        let deep = root.path().join("d".repeat(60)).join("e".repeat(60));
        std::fs::create_dir_all(&deep).unwrap();
        let path = deep.join("ctl.sock");
        let mut listener = LocalListener::bind(&path).unwrap();
        assert!(path.exists(), "the socket file is in its own directory");
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            stream.write_all(b"hello").await.unwrap();
            listener
        });
        let mut client = connect(&path).await.unwrap();
        let mut got = [0; 5];
        client.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hello");
        drop(server.await.unwrap());
        assert!(!path.exists(), "dropping the listener removes the socket");
    }

    /// Terminal Claude binds its messaging socket itself and refuses a
    /// directory reached through a symbolic link.
    #[test]
    fn a_socket_another_program_binds_gets_a_real_private_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let deep = root.path().join("d".repeat(60)).join("e".repeat(60));
        std::fs::create_dir_all(&deep).unwrap();
        let path = deep.join("messaging.sock");
        let address = unix_private_address(&path).unwrap();
        assert_ne!(address, path, "the path is too long to bind");
        assert_eq!(address.file_name(), path.file_name());
        let dir = std::fs::symlink_metadata(address.parent().unwrap()).unwrap();
        assert!(dir.is_dir(), "a real directory, not a link");
        assert_eq!(dir.permissions().mode() & 0o777, 0o700);
        assert_eq!(unix_private_address(&path).unwrap(), address, "stable");
        std::os::unix::net::UnixListener::bind(&address).unwrap();
        std::fs::remove_file(&address).unwrap();

        let short = root.path().join("m.sock");
        assert_eq!(unix_private_address(&short).unwrap(), short);
    }

    #[test]
    fn an_agents_short_addresses_go_with_its_directory() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("d".repeat(60)).join("e".repeat(60));
        let private = agent.join(crate::PRIVATE);
        std::fs::create_dir_all(&private).unwrap();
        let link = unix_address(&agent.join("ctl.sock")).unwrap();
        let link = link.parent().unwrap().to_owned();
        let real = unix_private_address(&private.join("messaging.sock")).unwrap();
        let real = real.parent().unwrap().to_owned();
        assert!(std::fs::symlink_metadata(&link).is_ok());
        assert!(real.is_dir());

        forget_short_addresses(&agent);
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "the link is gone"
        );
        assert!(!real.exists(), "the private directory is gone");
    }
}

#[cfg(test)]
mod rebind {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    async fn echo_once(listener: &mut LocalListener) -> LocalStream {
        let mut stream = listener.accept().await.unwrap();
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await.unwrap();
        stream.write_all(&byte).await.unwrap();
        stream
    }

    async fn ask(stream: &mut LocalStream, byte: u8) -> u8 {
        stream.write_all(&[byte]).await.unwrap();
        let mut answer = [0u8; 1];
        stream.read_exact(&mut answer).await.unwrap();
        answer[0]
    }

    /// A successor binds the path while a connection to its predecessor is
    /// still open: new clients reach the successor, and the old connection
    /// stays whole until its ends close it.
    #[tokio::test]
    async fn a_path_rebinds_while_a_predecessors_connection_is_open() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ctl.sock");

        let mut first = LocalListener::bind(&path).unwrap();
        let (served, mut old) = tokio::join!(echo_once(&mut first), async {
            let mut client = connect(&path).await.unwrap();
            assert_eq!(ask(&mut client, 1).await, 1);
            client
        });
        drop(first);

        let mut second = LocalListener::bind(&path).expect("the path rebinds");
        let (_new_served, _new) = tokio::join!(echo_once(&mut second), async {
            let mut client = connect(&path).await.unwrap();
            assert_eq!(ask(&mut client, 2).await, 2);
            client
        });

        let mut served = served;
        let mut byte = [0u8; 1];
        old.write_all(&[3]).await.unwrap();
        served.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte, [3], "the old connection still carries bytes");
    }
}
