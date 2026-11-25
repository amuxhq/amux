//! Unix domain sockets, reached through a short link when the path is too
//! long for `sockaddr_un`.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{fs, io};

use tokio::net::{UnixListener, UnixStream};

use super::Backoff;

pub type Stream = UnixStream;

pub struct Listener {
    socket: UnixListener,
    /// The bound socket file's device and inode: unbinding removes the
    /// file at the path only while it is still this one.
    file: (u64, u64),
    backoff: Backoff,
}

impl Listener {
    /// Binds under the socket's lock, which [`Listener::unbind`] takes
    /// too, so a predecessor's unbind can't fall between this bind and
    /// its check of what is at the path.
    pub fn bind(path: &Path) -> io::Result<Self> {
        let address = address(path)?;
        let _locked = lock_socket(path)?;
        match fs::remove_file(&address) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let socket = UnixListener::bind(&address)?;
        let meta = fs::symlink_metadata(&address)?;
        Ok(Self {
            socket,
            file: (meta.dev(), meta.ino()),
            backoff: Backoff::new(),
        })
    }

    pub async fn accept(&mut self) -> Stream {
        loop {
            match self.socket.accept().await {
                Ok((stream, _)) => {
                    self.backoff.reset();
                    return stream;
                }
                // Out of descriptors, a connection reset before it was
                // taken: nothing waiting can't mend.
                Err(error) => {
                    let pause = self.backoff.pause();
                    tracing::warn!(%error, ?pause, "accepting on a local socket failed; retrying");
                    tokio::time::sleep(pause).await;
                }
            }
        }
    }

    /// Removes the socket file at `path` if it is still the one this
    /// listener bound; a successor's stays. The check and the removal hold
    /// the socket's lock, as a bind does, so no successor binds between
    /// them, in this process or another.
    pub fn unbind(&self, path: &Path) {
        self.unbind_between(path, || {});
    }

    /// [`Listener::unbind`], running `between` after the check and before
    /// the removal: a test's way in.
    fn unbind_between(&self, path: &Path, between: impl FnOnce()) {
        // A socket that can't be locked (its directory has gone) has
        // nothing to keep.
        let Ok(_locked) = lock_socket(path) else {
            return;
        };
        if fs::symlink_metadata(path).is_ok_and(|meta| (meta.dev(), meta.ino()) == self.file) {
            between();
            let _ = fs::remove_file(path);
        }
    }
}

/// The lock of the socket `path`, held while the returned file is open:
/// `flock` on `<path>.lock` beside it, which every bind and unbind of the
/// path takes, in any process. Each holds it only for a removal and a
/// bind. A file open for writing, not the directory: on NFS `flock` is a
/// byte-range lock on the server, and an exclusive one needs a descriptor
/// that can write, which a directory's never is. The file stays, since a
/// lock on a file one holder removed and another made anew would keep
/// nobody out.
fn lock_socket(path: &Path) -> io::Result<fs::File> {
    use std::os::fd::AsRawFd;

    let mut lock = path.as_os_str().to_owned();
    lock.push(".lock");
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock)?;
    loop {
        // SAFETY: the descriptor is `file`'s, open for the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(file);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// The peer's pid comes from the socket itself: `SO_PEERCRED` on Linux,
/// `LOCAL_PEEREPID` on macOS.
pub async fn connect(path: &Path, patience: Duration) -> io::Result<(Stream, Option<u32>)> {
    let address = address(path)?;
    let stream = tokio::time::timeout(patience, UnixStream::connect(&address))
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{} did not answer within {patience:?}", path.display()),
            )
        })?
        .map_err(|error| match error.kind() {
            // Linux: the listener's backlog is full. It is there, and busy.
            io::ErrorKind::WouldBlock => io::Error::new(
                io::ErrorKind::ResourceBusy,
                format!("{} is too busy to take a connection: {error}", path.display()),
            ),
            _ => error,
        })?;
    let pid = stream
        .peer_cred()
        .ok()
        .and_then(|cred| cred.pid())
        .and_then(|pid| u32::try_from(pid).ok());
    Ok((stream, pid))
}

/// `shutdown(Both)`, so the peer reads end of file even if another copy of
/// the descriptor lives on, then the socket closed.
pub fn close(stream: Stream) {
    if let Ok(stream) = stream.into_std() {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
}

/// The path to bind or dial for the socket named `path`.
pub fn address(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().as_bytes().len() < limit() {
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
    if address.as_os_str().as_bytes().len() >= limit() {
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

/// FNV-1a: stable across processes and versions, which is all a derived
/// address needs.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
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
    if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a private directory", dir.display()),
        ));
    }
    Ok(())
}

/// Unix's own machinery: the short links a long path is reached through,
/// and the inode unbinding checks. The promises they keep are tested on
/// every OS in `tests.rs`.
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::local_socket::{
        LocalListener, connect, forget_short_addresses, unix_address, unix_private_address,
    };

    #[test]
    fn an_agents_short_addresses_go_with_its_directory() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("d".repeat(60)).join("e".repeat(60));
        let private = agent.join(crate::PRIVATE);
        fs::create_dir_all(&private).unwrap();
        let link = unix_address(&agent.join("ctl.sock")).unwrap();
        let link = link.parent().unwrap().to_owned();
        let real = unix_private_address(&private.join("messaging.sock")).unwrap();
        let real = real.parent().unwrap().to_owned();
        assert!(fs::symlink_metadata(&link).is_ok());
        assert!(real.is_dir());

        forget_short_addresses(&agent);
        assert!(fs::symlink_metadata(&link).is_err(), "the link is gone");
        assert!(!real.exists(), "the private directory is gone");
    }

    /// The lock is taken through a descriptor that can write, which an
    /// exclusive `flock` on NFS needs, and on a file that stays.
    #[tokio::test]
    async fn the_lock_is_a_file_open_for_writing_beside_the_socket() {
        use std::os::fd::AsRawFd;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ctl.sock");
        let held = lock_socket(&path).unwrap();
        // SAFETY: asks after the flags of a descriptor `held` owns.
        let flags = unsafe { libc::fcntl(held.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::O_ACCMODE, libc::O_RDONLY, "it can write");
        assert!(root.path().join("ctl.sock.lock").is_file());
        drop(held);
        let listener = Listener::bind(&path).unwrap();
        listener.unbind(&path);
        assert!(
            root.path().join("ctl.sock.lock").is_file(),
            "the lock stays"
        );
    }

    /// A listener whose backlog is full is there, and busy: a connect
    /// says so (`ResourceBusy`), not that nothing answers, so a daemon
    /// starting doesn't take a live one's socket for free.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_listener_with_a_full_backlog_is_busy() {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ctl.sock");
        let address = address(&path).unwrap();
        // SAFETY: a new socket, owned at once; bound to a path in our own
        // folder and listening with the shortest backlog, never accepting.
        let listener = unsafe {
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
            assert!(fd >= 0);
            let listener = OwnedFd::from_raw_fd(fd);
            let mut name: libc::sockaddr_un = std::mem::zeroed();
            name.sun_family = libc::AF_UNIX as libc::sa_family_t;
            for (to, from) in name.sun_path.iter_mut().zip(address.as_os_str().as_bytes()) {
                *to = *from as libc::c_char;
            }
            let size = size_of::<libc::sockaddr_un>() as libc::socklen_t;
            assert_eq!(libc::bind(fd, (&raw const name).cast(), size), 0);
            assert_eq!(libc::listen(listener.as_raw_fd(), 0), 0);
            listener
        };
        let mut held = Vec::new();
        let error = loop {
            match crate::local_socket::connect_within(&path, Duration::from_secs(1)).await {
                Ok(stream) => held.push(stream),
                Err(error) => break error,
            }
            assert!(held.len() < 64, "the backlog never filled");
        };
        assert_eq!(error.kind(), io::ErrorKind::ResourceBusy, "{error}");
        drop(listener);
    }

    /// The socket file is the socket: a file put at the path since is
    /// another inode, and the predecessor's unbind leaves it.
    #[tokio::test]
    async fn unbinding_leaves_a_file_that_is_not_this_listeners() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ctl.sock");
        let first = LocalListener::bind(&path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"someone else's").unwrap();
        drop(first);
        assert_eq!(fs::read(&path).unwrap(), b"someone else's");
    }

    /// The race: the predecessor checks that the file is its own, a
    /// successor binds, and only then does the predecessor remove what is
    /// at the path. The socket's lock holds the successor's bind back
    /// until the removal is done, so the successor's socket survives.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_successor_binding_inside_an_unbind_keeps_its_socket() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ctl.sock");
        let first = Listener::bind(&path).unwrap();
        let runtime = tokio::runtime::Handle::current();
        let mut successor = None;
        first.unbind_between(&path, || {
            let path = path.clone();
            successor = Some(std::thread::spawn(move || {
                let _entered = runtime.enter();
                Listener::bind(&path).unwrap()
            }));
            // Time for the successor to bind, were it not held back.
            std::thread::sleep(Duration::from_millis(200));
        });
        let mut second = successor.unwrap().join().unwrap();
        assert!(
            fs::symlink_metadata(&path).is_ok(),
            "the successor's socket is at the path"
        );
        let (served, client) = tokio::join!(second.accept(), connect(&path));
        drop(served);
        client.expect("the successor answers");
        drop(first);
    }
}
