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
        // Starts a sleeper that leads a process group of its own, as a
        // provider's tool shell leads a session, and waits.
        "orphaner" => {
            use std::os::unix::process::CommandExt as _;
            #[allow(
                clippy::zombie_processes,
                reason = "the sleeper is the orphan this role leaves for the test to find"
            )]
            let child = helper("sleep")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .expect("the sleeper starts");
            crate::process::tests::say(format!("pid {}", child.id()));
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
        // Starts a sleeper in its own group and exits at once, leaving it
        // there, as a wrapper does that starts the real program.
        "leaver" => {
            #[allow(
                clippy::zombie_processes,
                reason = "the sleeper is what this role leaves behind in its group"
            )]
            let child = helper("sleep")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("the sleeper starts");
            crate::process::tests::say(format!("pid {}", child.id()));
        }
        // Starts two short sleepers and forgets them, as an image that
        // exec'd itself forgets its children, then reaps what it inherited.
        "reaper" => {
            let pids: Vec<u32> = (0..2)
                .map(|_| {
                    #[allow(
                        clippy::zombie_processes,
                        reason = "reap_inherited reaps them, which is what the test checks"
                    )]
                    let child = std::process::Command::new("sleep")
                        .arg("1")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .expect("sleep starts");
                    child.id()
                })
                .collect();
            let found = crate::process::reap_inherited().expect("reaping starts");
            crate::process::tests::say(format!("found {found} {} {}", pids[0], pids[1]));
            std::thread::sleep(std::time::Duration::from_secs(60));
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

/// A mask the launcher left is cleared: the stop signals are heard again.
/// The mask is the calling thread's, so this thread stands in for a
/// process's first.
#[test]
fn unblock_signals_clears_an_inherited_mask() {
    fn blocked(signal: i32) -> bool {
        // SAFETY: reads this thread's mask into a set we own.
        unsafe {
            let mut now = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
            libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), now.as_mut_ptr());
            libc::sigismember(now.as_ptr(), signal) == 1
        }
    }
    // SAFETY: blocks two signals on this thread alone.
    unsafe {
        let mut set = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
        libc::sigemptyset(set.as_mut_ptr());
        libc::sigaddset(set.as_mut_ptr(), libc::SIGTERM);
        libc::sigaddset(set.as_mut_ptr(), libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, set.as_ptr(), std::ptr::null_mut());
    }
    assert!(blocked(libc::SIGTERM) && blocked(libc::SIGINT));
    crate::process::unblock_signals();
    assert!(!blocked(libc::SIGTERM) && !blocked(libc::SIGINT));
}

/// The children an image inherits are found, and each is reaped once it
/// exits rather than left a zombie.
#[test]
fn inherited_children_are_found_and_reaped() {
    let driven = Driven::start(helper("reaper"));
    let said = driven.expect("found");
    let words: Vec<&str> = said.split_whitespace().collect();
    assert!(words[0].parse::<usize>().unwrap() >= 2, "{said}");
    for pid in &words[1..] {
        let pid: i32 = pid.parse().unwrap();
        assert!(
            eventually(|| super::table::row(pid).is_none()),
            "{pid} left unreaped"
        );
    }
}

/// A daemon a member starts in a session of its own leaves the family.
pub(crate) const A_DAEMON_LEAVES_THE_FAMILY: bool = true;

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

/// A member whose parent exits is reparented away, out of every later
/// census: one counted while it was still below is ended all the same, and
/// one never counted escapes, as a provider's tool shell would once the
/// provider finishes on a polite stop.
///
/// The sleeper is seen reparented before the kill, not inferred to be
/// from its parent reading dead: macOS stops answering for an exiting
/// process (`proc_prepareexit`) before it hands the children on
/// (`proc_exit`), so "dead" there comes first. And it is seen alive then,
/// so a sleeper that died on the way (CI run 45886389827, once) says
/// whether `kill_others` ended it.
#[cfg(feature = "async")]
#[test]
fn a_counted_member_reparented_away_still_dies_on_kill_others() {
    for counted in [true, false] {
        let mut driven = Driven::start(helper("contained"));
        assert_eq!(driven.expect("contained"), "ok");
        driven.tell("orphaner");
        let member = ProcessRef::open(pid(&driven.expect("pid"))).unwrap();
        let sleeper = ProcessRef::open(pid(&driven.expect("pid"))).unwrap();
        if counted {
            driven.tell("count");
            assert_eq!(driven.expect("counted"), "");
        }
        member.kill().unwrap();
        assert!(eventually(|| !member.alive()), "the member dies");
        let row = || super::table::row(sleeper.pid() as i32);
        assert!(
            eventually(|| row().is_none_or(|row| row.ppid != member.pid() as i32)),
            "the sleeper is reparented: {:?}",
            row()
        );
        assert!(
            sleeper.alive(),
            "the sleeper outlives its parent: {:?}",
            row()
        );
        driven.tell("kill");
        assert_eq!(driven.expect("killed"), "ok");
        if counted {
            assert!(eventually(|| !sleeper.alive()), "the counted sleeper dies");
        } else {
            assert!(
                sleeper.alive(),
                "an uncounted sleeper is out of reach: {:?}",
                row()
            );
            sleeper.kill().unwrap();
        }
    }
}

/// A child that has exited keeps its row, marked a zombie, until it is
/// reaped: its pid can't be reused until then, so it stays pinned.
#[test]
fn an_exited_child_keeps_its_row_until_reaped() {
    let mut child = helper("exit-after 0 0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let pinned = super::ProcessRef::of_std_child(&child).expect("pinned, exited or not");
    let raw = child.id() as i32;
    assert!(
        eventually(|| super::table::row(raw).is_some_and(|row| row.zombie)),
        "seen exited"
    );
    let row = super::table::row(raw).unwrap();
    assert_eq!(
        (row.ppid, row.started),
        (std::process::id() as i32, pinned.started)
    );
    child.wait().unwrap();
    assert!(super::table::row(raw).is_none(), "gone once reaped");
}

/// A group's leader that has exited, unreaped, still holds its group's id:
/// killing the group ends what the leader left in it.
#[test]
fn a_group_is_killed_after_its_leader_exits() {
    use std::os::unix::process::CommandExt as _;
    let mut command = helper("leaver");
    command.process_group(0);
    let driven = Driven::start(command);
    let leader = ProcessRef::of_std_child(&driven.child).unwrap();
    let left = ProcessRef::open(pid(&driven.expect("pid"))).unwrap();
    let raw = driven.child.id() as i32;
    assert!(
        eventually(|| super::table::row(raw).is_some_and(|row| row.zombie)),
        "the leader exits"
    );
    leader.kill_group().unwrap();
    assert!(eventually(|| !left.alive()), "what it left is killed");
}

/// Both of macOS's rows for a process agree while it runs, so the one an
/// exited process falls back to pins it by the same start time.
#[cfg(target_os = "macos")]
#[test]
fn both_macos_rows_agree_on_a_running_process() {
    let raw = std::process::id() as i32;
    let live = super::table::live_row(raw).expect("proc_pidinfo answers");
    let unreaped = super::table::unreaped_row(raw).expect("sysctl answers");
    assert_eq!(
        (live.ppid, live.started, live.zombie),
        (unreaped.ppid, unreaped.started, unreaped.zombie)
    );
}

/// Another user's process, which `proc_pidinfo` refuses, stays out of the
/// table rather than costing every census a `sysctl` of its own.
#[cfg(target_os = "macos")]
#[test]
fn another_users_process_has_no_macos_row() {
    // SAFETY: geteuid only reads this process's effective user.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    // launchd, root's.
    assert!(super::table::row(1).is_none());
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

/// Asking a child to stop is SIGTERM, which ends one that doesn't catch it.
#[test]
fn asking_a_child_to_stop_sends_sigterm() {
    let mut child = helper("sleep")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let pinned = ProcessRef::of_std_child(&child).unwrap();
    pinned.ask_to_stop().unwrap();
    assert_eq!(pinned.reap().unwrap(), -libc::SIGTERM);
    pinned
        .ask_to_stop()
        .expect("asking what has exited is no error");
    // Reaped already, so this only fails.
    let _ = child.wait();
}
