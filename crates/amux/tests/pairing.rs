//! `amux pair` holding pairing mode open, as a person runs it: however the
//! command ends (Ctrl+C, its terminal closing, a kill), pairing mode closes
//! with it, so the next `amux pair` opens it again; one run while another
//! holds it open is refused in words.

use std::process::Stdio;

use support::desk::{Desk, LONG_GRACE_SECS, say};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::Child;

use crate::support;

fn desk() -> Desk {
    Desk::new(
        false,
        LONG_GRACE_SECS,
        node::version(),
        "http://127.0.0.1:9/",
        Vec::new(),
    )
}

/// Starts `amux pair` and returns it once it shows the PIN.
async fn open(desk: &Desk) -> (Child, BufReader<tokio::process::ChildStdout>) {
    let mut pair = desk.command(&["pair"]);
    pair.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = pair.spawn().expect("amux pair starts");
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    tokio::time::timeout(support::PATIENCE, async {
        loop {
            line.clear();
            let read = out.read_line(&mut line).await.unwrap();
            assert!(read > 0, "amux pair ended before it showed a PIN");
            if line.starts_with("Pairing PIN:") {
                return;
            }
        }
    })
    .await
    .expect("amux pair shows a PIN");
    (child, out)
}

/// A second `amux pair`, while one holds pairing mode open, is refused in
/// words.
async fn assert_refused(desk: &Desk) {
    let refused = desk.amux(&["pair"]).await;
    assert!(!refused.status.success(), "a second amux pair is refused");
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(
        said.contains(
            "Pairing is already open on this device; finish it or press Ctrl+C where it runs."
        ),
        "{said}"
    );
    assert!(!said.contains("PAIR_MODE_ALREADY_ACTIVE"), "{said}");
    say(format!("a second amux pair: {}", said.trim()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_amux_pair_is_refused_while_one_holds_pairing_mode_open() {
    let desk = desk();
    desk.run(&["server", "start"]).await;
    let (mut child, _out) = open(&desk).await;
    assert_refused(&desk).await;
    child.kill().await.unwrap();
    desk.run(&["server", "stop"]).await;
}

// Unix's own behaviour: SIGTERM, SIGHUP and SIGINT each end `amux pair`.
// On Windows it ends on Ctrl+C or its console closing, console control
// events no test sends (amux sends none; proposal §2.9).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn pairing_mode_closes_however_amux_pair_ends() {
    /// Sends `signal` to a running `amux pair` and returns what it printed as
    /// it closed pairing mode.
    async fn end(
        mut child: Child,
        mut out: BufReader<tokio::process::ChildStdout>,
        signal: libc::c_int,
    ) -> String {
        let pid = child.id().expect("running") as i32;
        // SAFETY: a signal to a process this test started.
        unsafe {
            libc::kill(pid, signal);
        }
        let status = tokio::time::timeout(support::PATIENCE, child.wait())
            .await
            .expect("amux pair ends")
            .unwrap();
        assert!(status.success(), "amux pair ended with {status}");
        let mut rest = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut out, &mut rest)
            .await
            .unwrap();
        rest
    }

    let desk = desk();
    desk.run(&["server", "start"]).await;

    for (signal, name) in [
        (libc::SIGTERM, "SIGTERM"),
        (libc::SIGHUP, "SIGHUP (its terminal closed)"),
        (libc::SIGINT, "Ctrl+C"),
    ] {
        let (child, out) = open(&desk).await;
        let said = end(child, out, signal).await;
        assert!(said.contains("Pairing mode closed."), "{name}: {said:?}");
        say(format!("amux pair ended by {name}: pairing mode closed"));
    }

    // The mode each of those closed opens again at once.
    let (child, out) = open(&desk).await;
    assert_refused(&desk).await;
    end(child, out, libc::SIGTERM).await;

    desk.run(&["server", "stop"]).await;
}
