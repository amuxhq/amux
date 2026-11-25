//! What the Unix mechanism adds to the process primitives: a pid pinned by
//! its start time, and the census read from the kernel's process table.
//! The helpers the shared roles need from Unix are here too; the shared
//! tests and their plumbing are in `process/tests.rs`.

use std::process::Stdio;

use crate::Pid;
use crate::process::tests::{Driven, eventually, helper, pid};
use crate::process::{Descendant, ProcessRef, descendants, kill_descendants};

/// The roles only Unix has.
pub(crate) fn run_role(name: &str, _rest: &[&str]) {
    match name {
        // Names itself with bytes that aren't UTF-8, as any process may.
        #[cfg(target_os = "linux")]
        "odd-name" => {
            let name = b"am\xffux\xfe\0";
            // SAFETY: PR_SET_NAME reads a NUL-terminated name of at most
            // 16 bytes from a live buffer.
            assert_eq!(unsafe { libc::prctl(libc::PR_SET_NAME, name.as_ptr()) }, 0);
            crate::process::tests::say("named");
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
        // `contained`, in a process whose census can't read the process
        // table, as one where `/proc` can't be opened.
        #[cfg(feature = "async")]
        "census-hidden" => {
            super::table::BLIND.store(true, std::sync::atomic::Ordering::SeqCst);
            crate::process::tests::contained();
        }
        other => panic!("no role {other:?}"),
    }
}

pub(crate) fn close_fd2() {
    // SAFETY: closes this role's descriptor 2, which nothing else uses.
    unsafe {
        libc::close(2);
    }
}

pub(crate) fn write_fd2(bytes: &[u8]) {
    // SAFETY: a write to descriptor 2 from a live buffer.
    unsafe {
        libc::write(2, bytes.as_ptr().cast(), bytes.len());
    }
}

/// Whether some handle still holds `path` open. Unix has no such probe,
/// and dup2 closes what it replaces.
pub(crate) fn held(_path: &std::ffi::OsStr) -> bool {
    false
}

/// Nothing on Unix explains a refused breakaway: none is ever refused.
pub(crate) fn say_why_breakaway_is_refused(_refused: bool) {}

/// A family whose census can't be taken isn't reported ended: what was
/// below it can't be found to kill, or seen gone. Its groups are killed
/// all the same.
#[cfg(feature = "async")]
#[test]
fn kill_others_without_a_census_is_an_error() {
    let mut driven = Driven::start(helper("census-hidden"));
    assert_eq!(driven.expect("contained"), "ok");
    driven.tell("tree");
    let member = ProcessRef::open(pid(&driven.expect("pid"))).unwrap();
    let _sleeper = driven.expect("pid");
    driven.tell("kill");
    let said = driven.expect("killed");
    assert!(said.starts_with("err "), "{said}");
    assert!(eventually(|| !member.alive()), "its group is killed");
}

/// A stop with nothing below it to kill (the common case, a provider
/// that left nothing) reads no process table.
#[test]
fn killing_an_empty_census_reads_no_table() {
    super::table::READS.with(|reads| reads.set(0));
    kill_descendants(&[]);
    assert_eq!(super::table::READS.with(std::cell::Cell::get), 0);
}

/// A process whose name isn't UTF-8 is still found and pinned.
#[cfg(target_os = "linux")]
#[test]
fn a_process_named_in_bytes_that_are_not_utf8_is_found() {
    let mut driven = Driven::start(helper("odd-name"));
    assert_eq!(driven.expect("named"), "");
    let pinned = ProcessRef::open(driven.child.id()).expect("found");
    assert!(pinned.alive());
    let _ = driven.child.kill();
}

/// The fields after the name, whatever bytes and parentheses it holds.
#[cfg(target_os = "linux")]
#[test]
fn a_stat_row_is_read_past_any_name() {
    let mut stat = b"4242 (a) b\xff (c)) S 7 4242 4242 0 -1 0".to_vec();
    stat.extend_from_slice(b" 0 0 0 0 0 0 0 0 20 0 1 0 987654 0 0\n");
    let row = super::table::parse_stat(4242, &stat).expect("a row");
    assert_eq!(
        (row.pid, row.ppid, row.started, row.zombie),
        (4242, 7, 987654, false)
    );
}

#[test]
fn a_process_ref_refuses_a_reused_pid() {
    let mut child = helper("sleep").stdin(Stdio::null()).spawn().unwrap();
    // The ref a reused pid would leave: the same pid, another start time.
    let stale = ProcessRef {
        inner: super::ProcessRef::forged(child.id(), 1),
    };
    assert!(!stale.alive());
    stale.kill().unwrap();
    assert_eq!(
        stale.suspend().unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    assert!(
        child.try_wait().unwrap().is_none(),
        "the pid's new owner lives"
    );
    let pinned = ProcessRef::open(child.id()).unwrap();
    assert!(pinned.alive());
    pinned.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn the_census_finds_a_childs_children() {
    let mut driven = Driven::start(helper("spawner"));
    let sleeper = pid(&driven.expect("pid"));
    let found: Vec<Pid> = descendants(driven.child.id())
        .iter()
        .map(Descendant::pid)
        .collect();
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        assert_eq!(found, vec![sleeper]);
        kill_descendants(&descendants(driven.child.id()));
        let sleeper = ProcessRef::open(sleeper);
        assert!(sleeper.is_err() || eventually(|| !sleeper.as_ref().unwrap().alive()));
    } else {
        assert!(found.is_empty(), "no process table this Unix can read");
        ProcessRef::open(sleeper).unwrap().kill().unwrap();
    }
    driven.child.kill().unwrap();
}
