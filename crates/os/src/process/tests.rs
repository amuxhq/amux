//! The process primitives, tested through real processes: the test binary
//! starts itself again in a role (`OS_TEST_ROLE`), and each role says what
//! it sees on lines of its standard output that start with `@os `.

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;

const ROLE: &str = "OS_TEST_ROLE";
/// A path a role is given, which may hold spaces the role's words can't.
pub(crate) const PATH: &str = "OS_TEST_PATH";
const PATH2: &str = "OS_TEST_PATH2";
const WAIT: Duration = Duration::from_secs(20);

/// Not a test: the entry of every role. Does nothing in a plain run.
#[test]
fn role() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let mut words = role.split(' ');
    let name = words.next().unwrap_or_default();
    let rest: Vec<&str> = words.collect();
    run_role(name, &rest);
    std::process::exit(0);
}

/// This test binary in `role`, with no stream set.
pub(crate) fn helper(role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("the test binary"));
    command
        .args(["--exact", "process::tests::role", "--nocapture", "--quiet"])
        .args(["--test-threads", "1"])
        .env(ROLE, role);
    command
}

pub(crate) fn say(line: impl std::fmt::Display) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "@os {line}");
    let _ = out.flush();
}

/// A role started with its stdin and stdout piped, read line by line.
pub(crate) struct Driven {
    pub child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
}

impl Driven {
    pub fn start(mut command: Command) -> Driven {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("the role starts");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped");
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(said) = line.strip_prefix("@os ")
                    && sender.send(said.to_owned()).is_err()
                {
                    break;
                }
            }
        });
        Driven {
            child,
            stdin,
            lines,
        }
    }

    pub fn tell(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin still open");
        writeln!(stdin, "{line}").expect("the role reads");
        stdin.flush().expect("the role reads");
    }

    /// The next line the role says, which must start with `word`; the rest.
    pub fn expect(&self, word: &str) -> String {
        let line = self
            .lines
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("the role said nothing (waiting for {word:?})"));
        let rest = line
            .strip_prefix(word)
            .unwrap_or_else(|| panic!("the role said {line:?}, not {word:?}"));
        rest.trim().to_owned()
    }

    /// Everything the role says until it exits.
    pub fn rest(&self) -> Vec<String> {
        std::iter::from_fn(|| self.lines.recv_timeout(WAIT).ok()).collect()
    }
}

impl Drop for Driven {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pid(text: &str) -> Pid {
    text.split(' ')
        .next()
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("{text:?} names no pid"))
}

/// Until `done`, or `WAIT`.
fn eventually(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}

fn run_role(name: &str, rest: &[&str]) {
    match name {
        "sleep" => std::thread::sleep(Duration::from_secs(60)),
        "spawner" => {
            let mut child = helper("sleep")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("the sleeper starts");
            say(format!("pid {}", child.id()));
            let _ = child.wait();
            std::thread::sleep(Duration::from_secs(60));
        }
        "daemonizer" => {
            let mut command = helper("sleep");
            background(&mut command, Detach::Session, &std::env::temp_dir());
            let spawned = spawn_background(&mut command).expect("the daemon starts");
            say(format!(
                "pid {} refused {}",
                spawned.child.id(),
                spawned.breakaway_refused
            ));
        }
        #[cfg(feature = "async")]
        "contained" => contained(),
        #[cfg(windows)]
        "jailer" => jailer(),
        "harden-parent" => {
            if rest.first() == Some(&"1") {
                harden_start();
            }
            let mut command = helper("sleep");
            background(&mut command, Detach::Session, &std::env::temp_dir());
            let spawned = spawn_background(&mut command).expect("the child starts");
            say(format!("pid {}", spawned.child.id()));
        }
        #[cfg(windows)]
        "console" => {
            use windows_sys::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
            // SAFETY: plain queries of this process's console.
            let (window, attached) = unsafe {
                let mut list = [0u32; 64];
                (
                    !GetConsoleWindow().is_null(),
                    GetConsoleProcessList(list.as_mut_ptr(), list.len() as u32),
                )
            };
            say(format!("window {window} attached {attached}"));
        }
        "ticker" => {
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                say("tick");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        "stderr" => {
            if rest.first() == Some(&"vacant") {
                // A process started with standard error closed. Unix:
                // harden_start fills descriptor 2, so the log can't land on
                // it. Windows: it leaves the C runtime's 2 vacant, for the
                // redirect to fill.
                close_fd2();
                harden_start();
            }
            let file = std::fs::OpenOptions::new()
                .append(true)
                .open(std::env::var_os(PATH).expect("a path"))
                .expect("the log opens");
            redirect_stderr(&file).expect("stderr moves");
            drop(file);
            eprintln!("from rust");
            write_fd2(b"from the c runtime\n");
            let status = helper("say-stderr")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .status()
                .expect("the child runs");
            assert!(status.success());
            say("done");
        }
        "say-stderr" => eprintln!("from a child"),
        "stderr-twice" => {
            for path in [PATH, PATH2] {
                let file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(std::env::var_os(path).expect("a path"))
                    .expect("the log opens");
                redirect_stderr(&file).expect("stderr moves");
            }
            say(format!("first held {}", held(&std::env::var_os(PATH).unwrap())));
        }
        #[cfg(windows)]
        "protected" => {
            protect_self();
            say("protected");
            std::thread::sleep(Duration::from_secs(60));
        }
        #[cfg(windows)]
        "jailed-twice" => {
            jail_self();
            let mut command = helper("sleep");
            background(&mut command, Detach::Session, &std::env::temp_dir());
            let first = spawn_background(&mut command).expect("the first starts");
            let second = spawn_background(&mut command).expect("the second starts");
            say(format!(
                "refused {} {}",
                first.breakaway_refused, second.breakaway_refused
            ));
            for mut spawned in [first, second] {
                let _ = spawned.child.kill();
                let _ = spawned.child.wait();
            }
        }
        "inherit" => {
            let raw: u64 = rest[0].parse().expect("a number");
            // SAFETY: the test handed this handle over, and nothing else
            // in this process has it.
            let mut file = unsafe { file_from_inherited(raw) }.expect("the file was handed over");
            file.write_all(b"handed over").expect("the file takes it");
            say("wrote");
        }
        "exec" => {
            let replaced = exec_or_spawn(helper("say exec'd")).expect("the successor starts");
            say(format!("spawned {}", replaced.child.id()));
        }
        "say" => say(format!("said {}", rest.join(" "))),
        #[cfg(feature = "async")]
        "signal" => crate::signal::tests::signal_role(rest),
        other => panic!("no role {other:?}"),
    }
}

#[cfg(unix)]
fn close_fd2() {
    // SAFETY: closes this role's descriptor 2, which nothing else uses.
    unsafe {
        libc::close(2);
    }
}

#[cfg(windows)]
fn close_fd2() {
    unsafe extern "C" {
        fn _close(fd: i32) -> i32;
    }
    // SAFETY: closes the C runtime's descriptor 2, which nothing else in
    // this role uses.
    unsafe {
        _close(2);
    }
}

/// Whether some handle still holds `path` open: one that can't be opened
/// with no sharing at all. Unix has no such probe, and dup2 closes what it
/// replaces.
#[cfg(windows)]
fn held(path: &std::ffi::OsStr) -> bool {
    use std::os::windows::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
        .is_err()
}

#[cfg(unix)]
fn held(_path: &std::ffi::OsStr) -> bool {
    false
}

/// Gives this process an empty DACL, so nothing of the same user can open
/// it to terminate it; its job can still end it.
#[cfg(windows)]
fn protect_self() {
    use windows_sys::Win32::Security::Authorization::{SE_KERNEL_OBJECT, SetSecurityInfo};
    use windows_sys::Win32::Security::{ACL, ACL_REVISION, DACL_SECURITY_INFORMATION, InitializeAcl};
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

#[cfg(unix)]
fn write_fd2(bytes: &[u8]) {
    // SAFETY: a write to descriptor 2 from a live buffer.
    unsafe {
        libc::write(2, bytes.as_ptr().cast(), bytes.len());
    }
}

#[cfg(windows)]
fn write_fd2(bytes: &[u8]) {
    unsafe extern "C" {
        fn _write(fd: i32, buffer: *const u8, count: u32) -> i32;
    }
    // SAFETY: a write to the C runtime's descriptor 2 from a live buffer.
    unsafe {
        _write(2, bytes.as_ptr(), bytes.len() as u32);
    }
}

/// Contains itself, then does what its stdin says: `tree` starts a member
/// that starts a sleeper of its own; `breakaway` a member that starts a
/// sleeper detached as a session and exits; `members`; `kill`.
#[cfg(feature = "async")]
fn contained() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let family = match Family::contain_self() {
            Ok(family) => {
                say("contained ok");
                family
            }
            Err(error) => {
                say(format!("contained err {error}"));
                return;
            }
        };
        let mut kept = Vec::new();
        let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
        let mut line = String::new();
        loop {
            line.clear();
            use tokio::io::AsyncBufReadExt as _;
            if stdin.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let command = line.trim();
            if command == "tree" || command == "breakaway" {
                let role = if command == "tree" {
                    "spawner"
                } else {
                    "daemonizer"
                };
                let mut member = tokio::process::Command::from(helper(role));
                member
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit());
                family.prepare(&mut member);
                let mut child = member.spawn().expect("the member starts");
                let joined = family.join(&child).expect("the member joins");
                let id = child.id().unwrap();
                let mut out = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
                let said = loop {
                    let line = out.next_line().await.unwrap().expect("the member says");
                    if let Some(said) = line.strip_prefix("@os ") {
                        break said.to_owned();
                    }
                };
                if command == "tree" {
                    say(format!("pid {id}"));
                    say(said);
                    kept.push((child, joined));
                } else {
                    // Daemonized: its sleeper's parent has gone.
                    child.wait().await.unwrap();
                    joined.release().await;
                    say(said);
                }
            } else if command == "members" {
                let members: Vec<String> = family
                    .members()
                    .expect("the census reads")
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                say(format!("members {}", members.join(" ")));
            } else if command == "protected" {
                let mut member = tokio::process::Command::from(helper("protected"));
                member
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit());
                family.prepare(&mut member);
                let mut child = member.spawn().expect("the member starts");
                let joined = family.join(&child).expect("the member joins");
                let id = child.id().unwrap();
                let mut out = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
                while let Some(line) = out.next_line().await.unwrap() {
                    if line == "@os protected" {
                        break;
                    }
                }
                say(format!("pid {id}"));
                kept.push((child, joined));
            } else if command == "kill" {
                match family.kill_others() {
                    Ok(()) => say("killed ok"),
                    Err(error) => say(format!("killed err {error}")),
                }
            }
        }
    });
}

/// Puts itself in a job that forbids breakaway, then runs `contained` on
/// its own streams.
#[cfg(windows)]
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
#[cfg(windows)]
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

/// Starts `contained` and its tree; the member's pid, the sleeper's.
#[cfg(feature = "async")]
fn contained_tree(command: Command) -> (Driven, Pid, Pid) {
    let mut driven = Driven::start(command);
    assert_eq!(driven.expect("contained"), "ok");
    driven.tell("tree");
    let member = pid(&driven.expect("pid"));
    let sleeper = pid(&driven.expect("pid"));
    (driven, member, sleeper)
}

fn members(driven: &mut Driven) -> Vec<Pid> {
    driven.tell("members");
    driven
        .expect("members")
        .split(' ')
        .filter(|word| !word.is_empty())
        .map(|word| word.parse().unwrap())
        .collect()
}

#[cfg(feature = "async")]
#[test]
fn a_grandchild_of_a_contained_process_dies_on_kill_others() {
    let (mut driven, member, sleeper) = contained_tree(helper("contained"));
    let (member_ref, sleeper_ref) = (
        ProcessRef::open(member).unwrap(),
        ProcessRef::open(sleeper).unwrap(),
    );
    let found = members(&mut driven);
    assert!(found.contains(&member), "{member} in {found:?}");
    assert!(found.contains(&sleeper), "{sleeper} in {found:?}");
    driven.tell("kill");
    assert_eq!(driven.expect("killed"), "ok");
    assert!(eventually(|| !member_ref.alive()), "the member dies");
    assert!(eventually(|| !sleeper_ref.alive()), "its child dies");
    assert_eq!(members(&mut driven), Vec::<Pid>::new());
    // The contained process itself lives on.
    assert!(driven.child.try_wait().unwrap().is_none());
}

#[cfg(feature = "async")]
#[test]
fn members_die_with_the_process_that_contains_them() {
    let (mut driven, member, sleeper) = contained_tree(helper("contained"));
    let (member_ref, sleeper_ref) = (
        ProcessRef::open(member).unwrap(),
        ProcessRef::open(sleeper).unwrap(),
    );
    driven.child.kill().unwrap();
    driven.child.wait().unwrap();
    // Unix: the tether's SIGTERM, at once. Windows: the job's last handle
    // closing.
    assert!(eventually(|| !member_ref.alive()), "the member dies");
    assert!(eventually(|| !sleeper_ref.alive()), "its child dies");
}

#[cfg(feature = "async")]
#[test]
fn a_member_that_breaks_away_survives() {
    let mut driven = Driven::start(helper("contained"));
    assert_eq!(driven.expect("contained"), "ok");
    driven.tell("breakaway");
    let said = driven.expect("pid");
    let daemon = pid(&said);
    let daemon_ref = ProcessRef::open(daemon).unwrap();
    let refused = said.ends_with("refused true");
    #[cfg(windows)]
    eprintln!(
        "this test process is in a job: {}; breakaway refused: {refused}",
        windows::in_job(None)
    );
    assert!(
        !refused,
        "the test runner's job forbids breakaway, so this can't be seen here"
    );
    let found = members(&mut driven);
    assert!(!found.contains(&daemon), "{daemon} not in {found:?}");
    driven.tell("kill");
    assert_eq!(driven.expect("killed"), "ok");
    driven.child.kill().unwrap();
    driven.child.wait().unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert!(daemon_ref.alive(), "the daemon lives on");
    daemon_ref.kill().unwrap();
}

/// §6's first risk: an agent whose start fell back into a job that forbids
/// breakaway (here the jailer's, `KILL_ON_JOB_CLOSE` alone) still contains
/// itself, since jobs nest, and still kills its tree. A member's breakaway
/// request is not refused: it leaves the agent's job, which allows it, and
/// stays in the outer one, which doesn't, so it ends when that job does.
#[cfg(all(windows, feature = "async"))]
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
    assert!(said.ends_with("refused false"), "{said}");
    let found = members(&mut driven);
    assert!(!found.contains(&daemon.pid()), "{found:?}");
    driven.tell("kill");
    assert_eq!(driven.expect("killed"), "ok");
    assert!(eventually(|| !member.alive() && !sleeper.alive()));
    assert!(daemon.alive(), "the member that broke away lives on");
    // The jailer's job ending takes it: it never left that one.
    driven.child.kill().unwrap();
    driven.child.wait().unwrap();
    assert!(
        eventually(|| !daemon.alive()),
        "the outer job's end takes it"
    );
}

#[cfg(feature = "async")]
#[test]
fn a_process_ref_survives_its_process_exiting() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut child = helper("sleep")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let pinned = ProcessRef::of_std_child(&child).unwrap();
    assert_eq!(pinned.pid(), child.id());
    assert!(pinned.alive());
    child.kill().unwrap();
    child.wait().unwrap();
    drop(child);
    assert!(!pinned.alive());
    runtime
        .block_on(async { tokio::time::timeout(WAIT, pinned.exited()).await })
        .expect("exited resolves")
        .unwrap();
    pinned.kill().expect("killing what has exited is no error");
    assert_eq!(
        pinned.suspend().unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
}

#[cfg(feature = "async")]
#[test]
fn a_process_ref_waits_for_a_live_process() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut child = tokio::process::Command::from(helper("sleep"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let pinned = ProcessRef::of_child(&child).unwrap();
        let waiting = tokio::time::timeout(Duration::from_millis(300), pinned.exited()).await;
        assert!(waiting.is_err(), "a live process hasn't exited");
        pinned.kill().unwrap();
        tokio::time::timeout(WAIT, pinned.exited())
            .await
            .expect("exited resolves once killed")
            .unwrap();
        let _ = child.wait().await;
    });
}

#[cfg(unix)]
#[test]
fn a_process_ref_refuses_a_reused_pid() {
    let mut child = helper("sleep").stdin(Stdio::null()).spawn().unwrap();
    // The ref a reused pid would leave: the same pid, another start time.
    let stale = ProcessRef {
        inner: unix::ProcessRef::forged(child.id(), 1),
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
fn opening_no_process_is_not_found() {
    let error = ProcessRef::open(0x7FFF_FFF0).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error}");
}

#[test]
fn suspend_stops_a_process_until_resume() {
    let driven = Driven::start(helper("ticker"));
    let ticks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let pinned = ProcessRef::of_std_child(&driven.child).unwrap();
    let counting = ticks.clone();
    let lines = std::thread::spawn({
        let driven = driven;
        move || {
            while driven.lines.recv_timeout(WAIT).is_ok() {
                counting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            drop(driven);
        }
    });
    assert!(eventually(|| ticks
        .load(std::sync::atomic::Ordering::SeqCst)
        > 5));
    pinned.suspend().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let stopped = ticks.load(std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        ticks.load(std::sync::atomic::Ordering::SeqCst),
        stopped,
        "a suspended process says nothing"
    );
    pinned.resume().unwrap();
    assert!(
        eventually(|| ticks.load(std::sync::atomic::Ordering::SeqCst) > stopped + 5),
        "it runs again"
    );
    pinned.kill().unwrap();
    lines.join().unwrap();
}

/// The S1 shape: a process whose stdout is a pipe starts a background
/// child and exits; the pipe's reader must see the end then, not when the
/// child does.
#[test]
fn harden_start_keeps_a_background_child_off_our_pipes() {
    let mut parent = helper("harden-parent 1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = parent.stdout.take().unwrap();
    let (sender, ended) = mpsc::channel();
    std::thread::spawn(move || {
        let mut said = Vec::new();
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(rest) = line.strip_prefix("@os pid ") {
                said.push(rest.to_owned());
            }
        }
        let _ = sender.send(said);
    });
    parent.wait().unwrap();
    let said = ended.recv_timeout(Duration::from_secs(10));
    let said = said.expect("the pipe ends when the parent exits, though its child runs on");
    let child = ProcessRef::open(pid(&said[0])).unwrap();
    assert!(child.alive(), "the child runs on");
    child.kill().unwrap();
}

#[cfg(windows)]
#[test]
fn a_background_process_has_no_window() {
    let mut command = helper("console");
    background(&mut command, Detach::Group, &std::env::temp_dir());
    command.stdout(Stdio::piped());
    let child = spawn_background(&mut command).unwrap().child;
    let output = child.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&output.stdout);
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

#[test]
fn redirect_stderr_moves_rust_the_c_runtime_and_children() {
    let folder = tempfile::tempdir().unwrap();
    let log = folder.path().join("log");
    std::fs::write(&log, "").unwrap();
    let mut command = helper("stderr");
    command.env(PATH, &log);
    let driven = Driven::start(command);
    assert_eq!(driven.expect("done"), "");
    drop(driven);
    let written = std::fs::read_to_string(&log).unwrap();
    for line in ["from rust", "from the c runtime", "from a child"] {
        assert!(written.contains(line), "{line:?} in {written:?}");
    }
}

#[test]
fn a_file_handed_to_a_child_round_trips() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("handed");
    let file = std::fs::File::create(&path).unwrap();
    set_inheritable(&file, true).unwrap();
    let driven = Driven::start(helper(&format!("inherit {}", raw_for_child(&file))));
    assert_eq!(driven.expect("wrote"), "");
    set_inheritable(&file, false).unwrap();
    drop(driven);
    drop(file);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "handed over");
}

#[test]
fn exec_or_spawn_runs_the_successor_on_our_streams() {
    let driven = Driven::start(helper("exec"));
    let said: Vec<String> = driven.rest();
    assert!(said.iter().any(|line| line == "said exec'd"), "{said:?}");
    // Unix replaced the role's image; Windows started a successor beside it.
    assert_eq!(
        said.iter().any(|line| line.starts_with("spawned ")),
        cfg!(windows),
        "{said:?}"
    );
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
        assert!(found.is_empty(), "the job takes the census on Windows");
        ProcessRef::open(sleeper).unwrap().kill().unwrap();
    }
    driven.child.kill().unwrap();
}

/// A descriptor 2 the C runtime had closed is filled, not closed again.
#[test]
fn redirect_stderr_fills_a_vacant_descriptor_2() {
    let folder = tempfile::tempdir().unwrap();
    let log = folder.path().join("with a space");
    std::fs::write(&log, "").unwrap();
    let mut command = helper("stderr vacant");
    command.env(PATH, &log);
    let driven = Driven::start(command);
    assert_eq!(driven.expect("done"), "");
    drop(driven);
    let written = std::fs::read_to_string(&log).unwrap();
    for line in ["from rust", "from the c runtime", "from a child"] {
        assert!(written.contains(line), "{line:?} in {written:?}");
    }
}

/// A second redirect lets go of the first log.
#[test]
fn a_second_redirect_closes_the_first_log() {
    let folder = tempfile::tempdir().unwrap();
    let (first, second) = (folder.path().join("first"), folder.path().join("second"));
    std::fs::write(&first, "").unwrap();
    std::fs::write(&second, "").unwrap();
    let mut command = helper("stderr-twice");
    command.env(PATH, &first).env(PATH2, &second);
    let driven = Driven::start(command);
    assert_eq!(driven.expect("first held"), "false");
}

#[test]
fn file_from_inherited_refuses_what_is_no_inherited_file() {
    for raw in [u64::MAX, 2] {
        // SAFETY: neither value is taken: both are refused before a File
        // is made.
        let error = unsafe { file_from_inherited(raw) }.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{raw}");
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;
        let stderr = std::io::stderr().as_raw_handle() as u64;
        // SAFETY: refused, as one of our standard handles, or as null
        // where there is none.
        let error = unsafe { file_from_inherited(stderr) }.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}

/// A member that can't be opened to be ended is an error, not "done".
#[cfg(all(windows, feature = "async"))]
#[test]
fn kill_others_reports_a_member_it_cannot_end() {
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
#[cfg(windows)]
#[test]
fn a_refused_breakaway_is_tried_again_on_the_next_start() {
    let driven = Driven::start(helper("jailed-twice"));
    assert_eq!(driven.expect("refused"), "true true");
}
