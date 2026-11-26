//! What the Windows mechanism adds to the process primitives: Job Objects
//! (a job that forbids breakaway, a member the job can end but we can't),
//! the console a background process shares, and the standard handles
//! `file_from_inherited` refuses. The roles they run in, and the helpers
//! the shared roles need from Windows, are here too; the shared tests and
//! their plumbing are in `process/tests.rs`.

use std::process::Stdio;
use std::time::Duration;

use crate::Pid;
use crate::process::tests::{Driven, eventually, helper, members, pid, say};
use crate::process::{
    Detach, Given, ProcessRef, Stream, background, descendants, file_from_inherited, pipe,
    spawn_background,
};

/// The roles only Windows has.
pub(crate) fn run_role(name: &str, _rest: &[&str]) {
    match name {
        "jailer" => jailer(),
        "console" => {
            use windows_sys::Win32::System::Console::GetConsoleProcessList;
            let window = crate::process::console_window();
            // SAFETY: a plain query of this process's console.
            let attached = unsafe {
                let mut list = [0u32; 64];
                GetConsoleProcessList(list.as_mut_ptr(), list.len() as u32)
            };
            say(format!("window {window} attached {attached}"));
        }
        "protected" => {
            protect_self();
            say("protected");
            std::thread::sleep(Duration::from_secs(60));
        }
        "jailed-twice" => {
            jail_self();
            let mut command = helper("sleep");
            background(&mut command, Detach::Session, &std::env::temp_dir());
            let first = spawn_background(&mut command, Given::default()).expect("the first starts");
            let second =
                spawn_background(&mut command, Given::default()).expect("the second starts");
            say(format!(
                "refused {} {}",
                first.breakaway_refused, second.breakaway_refused
            ));
            for mut spawned in [first, second] {
                let _ = spawned.child.kill();
                let _ = spawned.child.wait();
            }
        }
        other => panic!("no role {other:?}"),
    }
}

pub(crate) fn close_fd2() {
    unsafe extern "C" {
        fn _close(fd: i32) -> i32;
    }
    // SAFETY: closes the C runtime's descriptor 2, which nothing else in
    // this role uses.
    unsafe {
        _close(2);
    }
}

pub(crate) fn write_fd2(bytes: &[u8]) {
    unsafe extern "C" {
        fn _write(fd: i32, buffer: *const u8, count: u32) -> i32;
    }
    // SAFETY: a write to the C runtime's descriptor 2 from a live buffer.
    unsafe {
        _write(2, bytes.as_ptr(), bytes.len() as u32);
    }
}

/// Whether some handle still holds `path` open: one that can't be opened
/// with no sharing at all.
pub(crate) fn held(path: &std::ffi::OsStr) -> bool {
    use std::os::windows::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
        .is_err()
}

/// A daemon a member starts can't break away from the family's job.
pub(crate) const A_DAEMON_LEAVES_THE_FAMILY: bool = false;

/// Gives this process an empty DACL, so nothing of the same user can open
/// it to terminate it; its job can still end it.
fn protect_self() {
    use windows_sys::Win32::Security::Authorization::{SE_KERNEL_OBJECT, SetSecurityInfo};
    use windows_sys::Win32::Security::{
        ACL, ACL_REVISION, DACL_SECURITY_INFORMATION, InitializeAcl,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: an ACL in a buffer that outlives the call, set on this
    // process only.
    unsafe {
        let mut acl: ACL = std::mem::zeroed();
        assert!(InitializeAcl(&mut acl, size_of::<ACL>() as u32, ACL_REVISION) != 0);
        let set = SetSecurityInfo(
            GetCurrentProcess(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &acl,
            std::ptr::null(),
        );
        assert_eq!(set, 0, "the DACL is set");
    }
}

/// Puts itself in a job that forbids breakaway, then runs `contained` on
/// its own streams.
fn jailer() {
    jail_self();
    say("jailed");
    let status = helper("contained")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .expect("contained runs");
    std::process::exit(status.code().unwrap_or(1));
}

/// Puts this process in a new job that kills on close and forbids
/// breakaway; its handle lives as long as the process.
fn jail_self() {
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: a new job, set up and joined; its handle lives as long as
    // this process.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        assert!(!job.is_null());
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        assert!(
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) != 0
        );
        assert!(AssignProcessToJobObject(job, GetCurrentProcess()) != 0);
    }
}

/// §6's first risk: an agent whose start fell back into a job that forbids
/// breakaway (here the jailer's, `KILL_ON_JOB_CLOSE` alone) still contains
/// itself, since jobs nest, and still kills its tree. A member's breakaway
/// request is refused by the agent's job too (Git Bash would otherwise take
/// every command out of it), so the member is started inside and dies with
/// the rest.
#[cfg(feature = "async")]
#[test]
fn contain_self_works_inside_a_job_that_forbids_breakaway() {
    let mut driven = Driven::start(helper("jailer"));
    assert_eq!(driven.expect("jailed"), "");
    assert_eq!(driven.expect("contained"), "ok");
    driven.tell("tree");
    let member = ProcessRef::open(pid(&driven.expect("pid"))).unwrap();
    let sleeper = ProcessRef::open(pid(&driven.expect("pid"))).unwrap();
    driven.tell("breakaway");
    let said = driven.expect("pid");
    let daemon = ProcessRef::open(pid(&said)).unwrap();
    assert!(said.ends_with("refused true"), "{said}");
    let found = members(&mut driven);
    assert!(found.contains(&daemon.pid()), "{found:?}");
    driven.tell("kill");
    assert_eq!(driven.expect("killed"), "ok");
    assert!(eventually(|| !member.alive() && !sleeper.alive()));
    assert!(
        eventually(|| !daemon.alive()),
        "the refused breakaway dies too"
    );
}

#[test]
fn a_background_process_has_no_window() {
    let folder = tempfile::tempdir().unwrap();
    let out = folder.path().join("out");
    let mut command = helper("console");
    background(&mut command, Detach::Group, &std::env::temp_dir());
    let given = Given {
        stdout: Stream::File(std::fs::File::create(&out).unwrap()),
        ..Given::default()
    };
    let mut child = spawn_background(&mut command, given).unwrap().child;
    child.wait().unwrap();
    let said = std::fs::read_to_string(&out).unwrap();
    let line = said
        .lines()
        .find_map(|line| line.strip_prefix("@os "))
        .unwrap_or_else(|| panic!("the child said nothing: {said}"));
    let attached: u32 = line.rsplit(' ').next().unwrap().parse().unwrap();
    assert!(line.starts_with("window false"), "{line}");
    assert!(
        attached >= 1,
        "it has a console, which its children share: {line}"
    );
}

/// The D3 finding: a process holding an inheritable handle it was never
/// told of (a desktop app's own pipe, passed down to amux unknowingly)
/// starts a background child, which doesn't get it, so the pipe's reader
/// sees its end when the process exits, though the child runs on. Every
/// child would inherit it from a start that names no handle list.
#[test]
fn a_background_child_inherits_only_what_it_is_given() {
    let (mut unknown, passed) = pipe().unwrap();
    let (said, out) = pipe().unwrap();
    // The parent is given the pipe's write end as a handle it isn't told
    // of; it starts a sleeper in the background and exits.
    let mut command = helper("harden-parent 1");
    background(&mut command, Detach::Session, &std::env::temp_dir());
    let given = Given {
        stdout: Stream::File(out),
        files: &[&passed],
        ..Given::default()
    };
    let mut parent = spawn_background(&mut command, given).unwrap().child;
    drop(passed);
    parent.wait().unwrap();
    let line = std::io::BufRead::lines(std::io::BufReader::new(said))
        .map_while(Result::ok)
        .find_map(|line| line.strip_prefix("@os pid ").map(str::to_owned))
        .expect("the parent names its child");
    let child = ProcessRef::open(pid(&line)).unwrap();
    let (sender, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(std::io::Read::read(&mut unknown, &mut [0; 1]));
    });
    let ended = ended.recv_timeout(Duration::from_secs(10));
    let alive = child.alive();
    child.kill().unwrap();
    assert!(
        matches!(ended, Ok(Ok(0) | Err(_))),
        "the pipe ends with the parent, not with its child: {ended:?}"
    );
    assert!(alive, "the child runs on");
}

/// The job, not a census, holds a child's children on Windows: the census
/// finds none.
#[test]
fn the_census_leaves_a_childs_children_to_the_job() {
    let mut driven = Driven::start(helper("spawner"));
    let sleeper = pid(&driven.expect("pid"));
    let found: Vec<Pid> = descendants(driven.child.id())
        .iter()
        .map(crate::process::Descendant::pid)
        .collect();
    assert!(found.is_empty(), "the job takes the census on Windows");
    ProcessRef::open(sleeper).unwrap().kill().unwrap();
    driven.child.kill().unwrap();
}

/// Windows has no exec, so a process inherits no children to reap, even
/// while it has children of its own.
#[test]
fn nothing_is_inherited_to_reap() {
    let mut child = helper("sleep")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    crate::process::unblock_signals();
    assert_eq!(crate::process::reap_inherited().unwrap(), 0);
    child.kill().unwrap();
    let _ = child.wait();
}

/// Beside what `file_from_inherited_refuses_what_is_no_inherited_file`
/// refuses everywhere: one of our own standard handles.
#[test]
fn file_from_inherited_refuses_our_own_standard_handle() {
    use std::os::windows::io::AsRawHandle as _;
    let stderr = std::io::stderr().as_raw_handle() as u64;
    // SAFETY: refused, as one of our standard handles, or as null
    // where there is none.
    let error = unsafe { file_from_inherited(stderr) }.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

/// A member that can't be opened to be ended is an error, not "done".
#[cfg(feature = "async")]
#[test]
fn kill_others_reports_a_member_it_cannot_end() {
    // An enabled SeDebug opens any process whatever its DACL, and the
    // helpers inherit this token: on GitHub's runner the member was ended.
    let disabled = crate::testing::faults::hold_to_acls().unwrap();
    eprintln!("disabled to be held to the DACLs: {disabled:?}");
    let mut driven = Driven::start(helper("contained"));
    assert_eq!(driven.expect("contained"), "ok");
    driven.tell("protected");
    let protected = pid(&driven.expect("pid"));
    assert!(members(&mut driven).contains(&protected));
    driven.tell("kill");
    let said = driven.expect("killed");
    assert!(said.starts_with("err "), "{said}");
    // Its job still ends it.
    driven.child.kill().unwrap();
    driven.child.wait().unwrap();
}

/// A breakaway the job refused is asked for again on the command's next
/// start, and reported again.
#[test]
fn a_refused_breakaway_is_tried_again_on_the_next_start() {
    let driven = Driven::start(helper("jailed-twice"));
    assert_eq!(driven.expect("refused"), "true true");
}

/// Windows has no signal to ask a process to stop with: the caller is told
/// so, and the process runs on.
#[test]
fn asking_a_process_to_stop_is_unsupported() {
    let mut child = helper("sleep")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let pinned = ProcessRef::of_std_child(&child).unwrap();
    let error = pinned.ask_to_stop().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported, "{error}");
    assert!(pinned.alive());
    pinned.kill().unwrap();
    assert_eq!(pinned.reap().unwrap(), 1, "TerminateProcess's exit code");
    let _ = child.wait();
}
