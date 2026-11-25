//! What a local socket promises, on every OS.

use std::time::{Duration, Instant};

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

/// Both ends of one connection: the listener's and the client's.
async fn pair(listener: &mut LocalListener) -> (LocalStream, LocalStream) {
    let path = listener.path().to_owned();
    let (served, client) = tokio::join!(listener.accept(), connect(&path));
    (served.unwrap(), client.unwrap())
}

/// Within a few seconds, or the test fails rather than hangs.
async fn soon<T>(what: &str, future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .unwrap_or_else(|_| panic!("{what} did not happen within 5s"))
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

/// A predecessor dropped after its successor bound leaves the successor's
/// file, and clients keep reaching the successor.
#[tokio::test]
async fn a_successors_file_survives_its_predecessors_unbind() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    let first = LocalListener::bind(&path).unwrap();
    let mut second = LocalListener::bind(&path).unwrap();
    drop(first);
    assert!(path.exists(), "the successor's file is still there");
    let (_served, mut client) = soon("a connection", pair(&mut second)).await;
    drop(second);
    assert!(!path.exists(), "the successor's own unbind removes it");
    client.write_all(b"x").await.unwrap();
}

#[tokio::test]
async fn a_socket_whose_path_is_too_long_for_unix_still_lives_at_its_path() {
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

/// A client that connects and leaves before the server accepts it (a probe
/// asking whether anyone is there) costs the accept loop nothing: the next
/// client is served.
#[tokio::test]
async fn a_client_that_leaves_before_it_is_accepted_does_not_end_accepting() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    let mut listener = LocalListener::bind(&path).unwrap();
    for _ in 0..3 {
        drop(connect(&path).await.unwrap());
    }
    tokio::spawn(async move {
        loop {
            let mut stream = listener.accept().await.expect("accepting never fails");
            tokio::spawn(async move {
                let _ = stream.write_all(b"!").await;
            });
        }
    });
    let answer = soon("an answer", async {
        let mut client = connect(&path).await.unwrap();
        let mut byte = [0u8; 1];
        client.read_exact(&mut byte).await.unwrap();
        byte
    })
    .await;
    assert_eq!(&answer, b"!");
}

#[tokio::test]
async fn nothing_at_the_path_is_an_error_at_once() {
    let root = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let error = connect(&root.path().join("none.sock")).await.err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
    assert!(
        started.elapsed() < CONNECT_PATIENCE,
        "no waiting for nothing"
    );
}

/// The I1 shape: the write half is kept to write on and the read half is in
/// another task's loop. Closing through a closer ends both here, and the
/// peer, which reads what was written before, then reads the end.
#[tokio::test]
async fn closing_a_split_connection_ends_it_for_the_peer_and_both_halves() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    let mut listener = LocalListener::bind(&path).unwrap();
    let (served, mut client) = soon("a connection", pair(&mut listener)).await;

    let closer = served.closer();
    let (mut reader, mut writer) = tokio::io::split(served);
    let reading = tokio::spawn(async move {
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).await.map(|_| rest)
    });
    writer.write_all(b"last").await.unwrap();
    // Let the reader wait on the pipe before it is closed under it.
    tokio::time::sleep(Duration::from_millis(50)).await;
    closer.close();

    let read = soon("the reader's end", reading).await.unwrap();
    assert_eq!(read.unwrap(), b"", "the reader here sees the end");
    let error = writer
        .write_all(b"more")
        .await
        .expect_err("nothing more is written");
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);

    let mut got = Vec::new();
    soon("the peer's end", client.read_to_end(&mut got))
        .await
        .unwrap();
    assert_eq!(got, b"last", "what was written arrives, then the end");
    closer.close();
}

/// A client's close is the server's end of file too.
#[tokio::test]
async fn closing_a_client_ends_it_for_the_server() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    let mut listener = LocalListener::bind(&path).unwrap();
    let (mut served, client) = soon("a connection", pair(&mut listener)).await;
    client.close();
    let mut got = Vec::new();
    soon("the server's end", served.read_to_end(&mut got))
        .await
        .unwrap();
    assert!(got.is_empty());
}

/// A closer outliving its stream keeps nothing open and does nothing.
#[tokio::test]
async fn a_closer_does_not_hold_its_connection_open() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    let mut listener = LocalListener::bind(&path).unwrap();
    let (served, mut client) = soon("a connection", pair(&mut listener)).await;
    let closer = served.closer();
    drop(served);
    let mut got = Vec::new();
    soon("the peer's end", client.read_to_end(&mut got))
        .await
        .unwrap();
    closer.close();
}

/// Terminal Claude binds its messaging socket itself and refuses a
/// directory reached through a symbolic link.
#[cfg(unix)]
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

#[cfg(unix)]
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

/// On Unix the socket file is the socket: a file a successor bound at the
/// path is another inode, and the predecessor's unbind leaves it.
#[cfg(unix)]
#[tokio::test]
async fn unbinding_leaves_a_file_that_is_not_this_listeners() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ctl.sock");
    let first = LocalListener::bind(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"someone else's").unwrap();
    drop(first);
    assert_eq!(std::fs::read(&path).unwrap(), b"someone else's");
}
