//! What named pipes add: names nobody can guess or redirect, an owner
//! checked, patience with a busy or rebinding server.
//!
//! Not here: an elevated daemon with a normal client. It needs an elevated
//! process, which the test environment does not allow; the owner check it
//! rests on is tested (`a_pipe_is_checked_by_its_owner_not_its_server`).

use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

use super::windows::{INSTANCES, Listener, is_amux_pipe, owned_by_me, random_name};
use super::*;

const HEX32: &str = "0123456789abcdef0123456789abcdef";

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
    assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
    assert!(
        waited >= patience,
        "it tried until the deadline: {waited:?}"
    );
    assert!(waited < CONNECT_PATIENCE, "and not much longer: {waited:?}");
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
