//! What named pipes add: names nobody can guess or redirect, an owner
//! checked, patience with a busy or rebinding server.
//!
//! Not here: an elevated daemon with a normal client, which needs two
//! processes at two integrity levels. (Under Buck the tests run in the
//! build worker's elevated process, where a pipe left to the token's
//! default owner is owned by Administrators: that is how the legacy tests
//! below see an elevated earlier build.) The owner check a normal client
//! makes is tested (`a_pipe_is_checked_by_its_owner_not_its_server`).

use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

use super::super::*;
use super::{INSTANCES, Listener, is_amux_pipe, owned_by_me, random_name};

const HEX32: &str = "0123456789abcdef0123456789abcdef";

/// Name files whose reads stall, and for how long: a share gone quiet,
/// for the reads of one path alone, so tests running beside are untouched.
static STALLED: std::sync::Mutex<Vec<(std::path::PathBuf, Duration)>> =
    std::sync::Mutex::new(Vec::new());

/// Called by each read of a name file before it reads.
pub(super) fn stall(path: &Path) {
    let stall = STALLED
        .lock()
        .unwrap()
        .iter()
        .find(|(stalled, _)| stalled == path)
        .map(|(_, stall)| *stall);
    if let Some(stall) = stall {
        std::thread::sleep(stall);
    }
}

fn stall_reads_of(path: &Path, stall: Duration) {
    STALLED.lock().unwrap().push((path.to_owned(), stall));
}

/// A read of the name file that stalls is given up at the deadline, and
/// the runtime that asked shuts down at once, not when the read ends.
#[test]
fn a_stalled_name_file_read_does_not_hold_its_runtime_open() {
    let root = tempfile::tempdir().unwrap();
    let path = std::path::absolute(root.path().join("ctl.sock")).unwrap();
    std::fs::write(&path, random_name()).unwrap();
    stall_reads_of(&path, Duration::from_secs(3));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime
        .block_on(connect_within(&path, Duration::from_millis(200)))
        .err()
        .expect("the read stalls");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
    let started = Instant::now();
    drop(runtime);
    let took = started.elapsed();
    assert!(took < Duration::from_secs(1), "the runtime shut down in {took:?}");
}

/// Within a few seconds, or the test fails rather than hangs.
async fn soon<T>(what: &str, future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .unwrap_or_else(|_| panic!("{what} did not happen within 5s"))
}

#[test]
fn only_an_amux_pipe_name_is_opened() {
    let new = format!(r"\\.\pipe\amux-{HEX32}");
    assert!(is_amux_pipe(&new));
    assert!(
        is_amux_pipe(r"\\.\pipe\amux-0123456789abcdef-4242-0"),
        "what earlier builds wrote, which an agent they started still serves"
    );
    for refused in [
        format!(r"\\server\pipe\amux-{HEX32}"),
        format!(r"\\localhost\pipe\amux-{HEX32}"),
        format!(r"\\?\pipe\amux-{HEX32}"),
        format!(r"//./pipe/amux-{HEX32}"),
        format!(r"\\.\PIPE\amux-{HEX32}"),
        format!(r"\\.\pipe\amux-{}", HEX32.to_uppercase()),
        format!(r"\\.\pipe\amux-{}", &HEX32[1..]),
        format!(r"\\.\pipe\amux-{HEX32}0"),
        format!(r"\\.\pipe\amux-{HEX32}\x"),
        format!(r"\\.\pipe\amux-{HEX32}{}", "\n"),
        format!(r" \\.\pipe\amux-{HEX32}"),
        r"\\.\COM1".to_owned(),
        r"\\.\pipe\other".to_owned(),
        r"\\.\pipe\amux-".to_owned(),
        r"\\.\pipe\amux-0123456789abcdef-4242".to_owned(),
        r"\\.\pipe\amux-0123456789abcdef-4242-0-1".to_owned(),
        r"\\.\pipe\amux-0123456789abcdef-x-0".to_owned(),
        r"\\.\pipe\amux-0123456789abcdef--0".to_owned(),
        String::new(),
    ] {
        assert!(!is_amux_pipe(&refused), "{refused:?} is refused");
    }
}

#[test]
fn names_are_amuxs_and_never_repeat() {
    let names: std::collections::BTreeSet<String> = (0..1000).map(|_| random_name()).collect();
    assert_eq!(names.len(), 1000);
    assert!(names.iter().all(|name| is_amux_pipe(name)));
}

/// A name file pointing at another host is refused before anything is
/// opened: nothing is sent to that host, these credentials least of all.
#[tokio::test]
async fn a_file_naming_a_pipe_on_another_host_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    std::fs::write(&path, format!(r"\\amux-test.invalid\pipe\amux-{HEX32}")).unwrap();
    let started = Instant::now();
    let error = connect(&path).await.err().expect("refused");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "no network was tried"
    );
}

/// A file redirected to a pipe that is not amux's is refused even when that
/// pipe is this user's own and would answer.
#[tokio::test]
async fn a_file_naming_a_pipe_not_amuxs_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    let name = format!(r"\\.\pipe\squat-{}", &random_name()[14..]);
    let _squatter = ServerOptions::new().create(&name).unwrap();
    std::fs::write(&path, &name).unwrap();
    let error = connect(&path).await.err().expect("refused");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{error}");
}

/// A name someone else created first is never joined: the bind draws
/// another, and the file names that.
#[tokio::test]
async fn a_squatted_name_is_never_joined() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    let squatted = random_name();
    let _squatter = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&squatted)
        .unwrap();

    let error = Listener::bind_drawing(&path, [squatted.clone()])
        .err()
        .expect("the squatted name alone is refused");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
    assert!(!path.exists(), "and nothing was published");

    let fresh = random_name();
    let inner = Listener::bind_drawing(&path, [squatted.clone(), fresh.clone()]).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), fresh);
    let mut listener = LocalListener {
        path: path.clone(),
        inner,
    };
    let (served, client) = tokio::join!(listener.accept(), connect(&path));
    let (mut served, mut client) = (served.unwrap(), client.unwrap());
    client.write_all(b"?").await.unwrap();
    let mut byte = [0u8; 1];
    served.read_exact(&mut byte).await.unwrap();
    assert_eq!(&byte, b"?", "the client reached the fresh pipe");
}

/// The client asks who owns the pipe it opened. This user's own pipe
/// passes, with its server's pid; an object owned by someone else (a
/// system file, owned by TrustedInstaller) fails, which is what a pipe
/// another user made would do.
#[tokio::test]
async fn a_pipe_is_checked_by_its_owner_not_its_server() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    let mut listener = LocalListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let mut stream = listener.accept().await.unwrap();
        stream.write_all(b"ok").await.unwrap();
        listener
    });
    let (mut client, pid) = connect_to_process(&path).await.unwrap();
    assert_eq!(pid, Some(std::process::id()));
    let mut answer = [0u8; 2];
    client.read_exact(&mut answer).await.unwrap();
    assert_eq!(&answer, b"ok");
    drop(server.await.unwrap());

    let name = std::fs::read_to_string(root.path().join("ctl.sock"));
    assert!(name.is_err(), "unbound");

    let system = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap()
        .join(r"System32\kernel32.dll");
    const READ_CONTROL: u32 = 0x0002_0000;
    let theirs = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL)
        .open(&system)
        .unwrap();
    assert!(
        !owned_by_me(theirs.as_raw_handle()).unwrap(),
        "not this user's"
    );
    // Not a new file of this user's as the other half: a file takes its
    // creator's default owner, which need not be the user (an elevated
    // process's is Administrators, and under the test runner it was not
    // the user either). That is why a pipe names its owner in its SDDL;
    // the pipe's own owner is checked in `the_file_names_the_listening_pipe`.
}

/// Every instance taken, nobody accepting: four clients get in, and a
/// fifth is told so by its deadline instead of waiting forever.
#[tokio::test]
async fn a_busy_server_is_given_up_on_by_the_deadline() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    let _listener = LocalListener::bind(&path).unwrap();
    let mut held = Vec::new();
    for n in 0..INSTANCES {
        let started = Instant::now();
        let client = connect_within(&path, Duration::ZERO)
            .await
            .unwrap_or_else(|error| panic!("client {n} finds an instance listening: {error}"));
        assert!(started.elapsed() < Duration::from_millis(500));
        held.push(client);
    }
    let patience = Duration::from_millis(300);
    let started = Instant::now();
    let error = connect_within(&path, patience).await.err().expect("busy");
    let waited = started.elapsed();
    // Busy, not gone: a daemon starting must not take this one's name.
    assert_eq!(error.kind(), io::ErrorKind::ResourceBusy, "{error}");
    assert!(
        waited >= patience,
        "it tried until the deadline: {waited:?}"
    );
    assert!(waited < CONNECT_PATIENCE, "and not much longer: {waited:?}");
}

/// Many dials at once, each to a listener of its own: more than the
/// reads of name files allowed at once wait for a slot within their
/// deadline, rather than failing at once.
#[tokio::test]
async fn dials_past_the_reads_allowed_at_once_wait_for_a_slot() {
    let root = tempfile::tempdir().unwrap();
    let mut listeners = Vec::new();
    let mut dials = tokio::task::JoinSet::new();
    for n in 0..4 * super::MOST_READING {
        let path = root.path().join(format!("{n}.sock"));
        listeners.push(LocalListener::bind(&path).unwrap());
        dials.spawn(async move { (n, connect_within(&path, CONNECT_PATIENCE).await) });
    }
    let mut reached = 0;
    while let Some(dialled) = soon("every dial", dials.join_next()).await {
        let (n, dialled) = dialled.unwrap();
        dialled.unwrap_or_else(|error| panic!("dial {n} reaches its listener: {error}"));
        reached += 1;
    }
    assert_eq!(reached, listeners.len());
}

/// A file naming a pipe that is gone (its listener ended, a successor is
/// starting) is read again until the successor's name is there.
#[tokio::test]
async fn a_client_waits_for_a_successor_and_reads_its_name_afresh() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    std::fs::write(&path, random_name()).unwrap();

    let patience = Duration::from_millis(200);
    let started = Instant::now();
    let error = connect_within(&path, patience).await.err().expect("gone");
    assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused, "{error}");
    assert!(started.elapsed() >= patience);

    let successor = {
        let path = path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let mut listener = LocalListener::bind(&path).unwrap();
            let mut stream = listener.accept().await.unwrap();
            stream.write_all(b"s").await.unwrap();
            listener
        })
    };
    let mut client = connect(&path).await.expect("the successor answers");
    let mut byte = [0u8; 1];
    client.read_exact(&mut byte).await.unwrap();
    assert_eq!(&byte, b"s");
    drop(successor.await.unwrap());
}

/// Publishing the name waits out a brief hold on the file (a scanner, an
/// indexer); a hold that outlasts the patience fails the bind and leaves no
/// staged file behind.
#[tokio::test]
async fn binding_waits_out_a_brief_hold_and_leaves_nothing_after_a_long_one() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    std::fs::write(&path, "an old name").unwrap();
    let hold = |for_how_long: Duration| {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(for_how_long);
            drop(file);
        })
    };

    let held = hold(Duration::from_millis(200));
    let listener = LocalListener::bind(&path).expect("the bind waits");
    held.join().unwrap();
    assert!(is_amux_pipe(&std::fs::read_to_string(&path).unwrap()));
    drop(listener);

    std::fs::write(&path, "an old name").unwrap();
    let held = hold(os::fs::SHARING_PATIENCE * 3);
    assert!(
        LocalListener::bind(&path).is_err(),
        "a long hold fails the bind"
    );
    held.join().unwrap();
    let left: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, ["ctl.sock"], "no staged file is left");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "an old name");
}

/// The file names the pipe the listener serves, which a client opened by
/// hand finds this user's, admitting local clients alone.
#[tokio::test]
async fn the_file_names_the_listening_pipe() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    let _listener = LocalListener::bind(&path).unwrap();
    let name = std::fs::read_to_string(&path).unwrap();
    assert!(is_amux_pipe(&name) && name.len() == 14 + 32, "{name}");
    let client = ClientOptions::new().open(&name).unwrap();
    assert!(owned_by_me(client.as_raw_handle()).unwrap());
}

/// A name file a scanner holds without sharing reads is waited for within
/// the deadline, not failed at once.
#[tokio::test]
async fn a_name_file_held_a_moment_is_waited_for() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    let mut listener = LocalListener::bind(&path).unwrap();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        drop(held);
    });
    let started = Instant::now();
    let accepting = tokio::spawn(async move {
        let served = listener.accept().await;
        (listener, served)
    });
    let client = soon("an answer", connect(&path)).await;
    client.expect("connected once the file was let go");
    assert!(started.elapsed() >= Duration::from_millis(250));
    release.join().unwrap();
    let (_listener, served) = accepting.await.unwrap();
    served.unwrap();

    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let patience = Duration::from_millis(200);
    let started = Instant::now();
    let error = connect_within(&path, patience)
        .await
        .err()
        .expect("held throughout");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
    assert!(
        started.elapsed() < CONNECT_PATIENCE,
        "{:?}",
        started.elapsed()
    );
    drop(held);
}

/// A client reads no more of the file than a name could be.
#[tokio::test]
async fn a_name_file_too_long_for_a_name_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    std::fs::write(&path, format!(r"\\.\pipe\amux-{}", "a".repeat(1 << 20))).unwrap();
    let error = connect(&path).await.err().expect("refused");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{error}");
}

/// A pipe server end made as earlier builds made it: the old name form, and
/// a DACL admitting this user but no owner named, so the owner is the
/// token's default (Administrators, for an elevated process).
fn legacy_pipe(name: &str) -> tokio::net::windows::named_pipe::NamedPipeServer {
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;

    let sid = os::fs::current_user_sid().unwrap();
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
        .encode_utf16()
        .chain([0])
        .collect();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: `sddl` is NUL-terminated; the descriptor is leaked, as the
    // old builds' was.
    assert_ne!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        },
        0
    );
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // SAFETY: the attributes and their descriptor outlive the call.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, (&raw mut attributes).cast())
    }
    .unwrap()
}

/// A daemon adopting an agent an earlier build started opens that agent's
/// pipe, whoever the token made its owner, as long as the process serving
/// it runs as this user. (Under Buck the tests run elevated, so the owner
/// here is Administrators: the elevated case, not only the plain one.)
#[tokio::test]
async fn an_earlier_builds_pipe_is_reached() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    let name = format!(
        r"\\.\pipe\amux-{}-{}-0",
        &random_name()[14..30],
        std::process::id()
    );
    assert!(is_amux_pipe(&name), "{name}");
    let server = legacy_pipe(&name);
    std::fs::write(&path, &name).unwrap();
    let probe = ClientOptions::new().open(&name).unwrap();
    let owner = super::owner_of(probe.as_raw_handle()).unwrap();
    eprintln!("an earlier build's pipe is owned here by {owner}");
    drop(probe);
    server.disconnect().unwrap();

    let accepted = async {
        server.connect().await.unwrap();
        server
    };
    let ((_client, pid), _server) =
        tokio::join!(async { connect_to_process(&path).await.unwrap() }, accepted);
    assert_eq!(pid, Some(std::process::id()));
}

/// The same old descriptor under a name of this build's form is held to
/// the strict rule: only an owner that is this user passes.
#[tokio::test]
async fn a_new_name_is_held_to_its_owner() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    let name = random_name();
    let server = legacy_pipe(&name);
    std::fs::write(&path, &name).unwrap();
    let probe = ClientOptions::new().open(&name).unwrap();
    let mine = owned_by_me(probe.as_raw_handle()).unwrap();
    drop(probe);
    server.disconnect().unwrap();
    let accepting = tokio::spawn(async move {
        let _ = server.connect().await;
        server
    });
    let reached = soon("an answer", connect(&path)).await;
    accepting.abort();
    if mine {
        eprintln!("the default owner here is the user: nothing to refuse");
    } else {
        let error = reached.err().expect("refused: not owned by this user");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
    }
}
