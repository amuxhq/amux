//! The process primitives, tested through real processes: the test binary
//! starts itself again in a role (`OS_TEST_ROLE`), and each role says what
//! it sees on lines of its standard output that start with `@os `.
//!
//! What holds on every OS is here; what one OS's mechanism adds (Job
//! Objects, the console, a reused pid) is in that OS's `tests` module
//! beside its code, with its roles and the helpers only it has.

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;

const ROLE: &str = "OS_TEST_ROLE";
/// A path a role is given, which may hold spaces the role's words can't.
pub(crate) const PATH: &str = "OS_TEST_PATH";
const PATH2: &str = "OS_TEST_PATH2";
pub(crate) const WAIT: Duration = Duration::from_secs(20);

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

pub(crate) fn pid(text: &str) -> Pid {
    text.split(' ')
        .next()
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("{text:?} names no pid"))
}

/// Until `done`, or `WAIT`.
pub(crate) fn eventually(mut done: impl FnMut() -> bool) -> bool {
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
        "harden-parent" => {
            if rest.first() == Some(&"1") {
                harden_start();
            }
            let mut command = helper("sleep");
            background(&mut command, Detach::Session, &std::env::temp_dir());
            let spawned = spawn_background(&mut command).expect("the child starts");
            say(format!("pid {}", spawned.child.id()));
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
                imp::tests::close_fd2();
                harden_start();
            }
            let file = std::fs::OpenOptions::new()
                .append(true)
                .open(std::env::var_os(PATH).expect("a path"))
                .expect("the log opens");
            redirect_stderr(&file).expect("stderr moves");
            drop(file);
            eprintln!("from rust");
            imp::tests::write_fd2(b"from the c runtime\n");
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
            say(format!(
                "first held {}",
                imp::tests::held(&std::env::var_os(PATH).unwrap())
            ));
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
        "exit-after" => {
            std::thread::sleep(Duration::from_millis(rest[0].parse().expect("ms")));
            std::process::exit(rest[1].parse().expect("a code"));
        }
        // Starts a child, then hands it to a successor it replaces itself
        // with, as a supervisor does after an update.
        "bequeath" => {
            // Waited for by the successor, not here.
            #[expect(clippy::zombie_processes)]
            let child = helper("exit-after 1000 7")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .expect("the child starts");
            let pinned = ProcessRef::of_std_child(&child).expect("the child is pinned");
            let handle = pinned.for_successor().expect("a handle for the successor");
            let raw = match &handle {
                Some(file) => {
                    set_inheritable(file, true).expect("the handle is inheritable");
                    raw_for_child(file).to_string()
                }
                None => "-".to_owned(),
            };
            exec_or_spawn(helper(&format!("heir {} {raw}", child.id())))
                .expect("the successor starts");
            say("bequeathed");
        }
        "heir" => {
            let pid: Pid = rest[0].parse().expect("a pid");
            let handle = (rest[1] != "-").then(|| rest[1].parse().expect("a handle"));
            // SAFETY: the bequeathing role handed this handle over, and
            // nothing else in this process has it.
            let inherited = unsafe { ProcessRef::inherited(pid, handle) }.expect("taken over");
            assert_eq!(inherited.pid(), pid);
            say(format!("reaped {}", inherited.reap().expect("reaped")));
        }
        #[cfg(feature = "async")]
        "signal" => crate::signal::tests::signal_role(rest),
        // A role only one OS has.
        other => imp::tests::run_role(other, rest),
    }
}

/// Contains itself, then does what its stdin says: `tree` starts a member
/// that starts a sleeper of its own; `breakaway` a member that starts a
/// sleeper detached as a session and exits; `orphaner` a member whose
/// sleeper leaves its group (a role only Unix has); `count`; `members`;
/// `kill`.
#[cfg(feature = "async")]
pub(crate) fn contained() {
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
            if command == "tree" || command == "orphaner" || command == "breakaway" {
                let role = match command {
                    "tree" => "spawner",
                    "orphaner" => "orphaner",
                    _ => "daemonizer",
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
                if command != "breakaway" {
                    say(format!("pid {id}"));
                    say(said);
                    kept.push((child, joined));
                } else {
                    // Daemonized: its sleeper's parent has gone.
                    child.wait().await.unwrap();
                    joined.release().await;
                    say(said);
                }
            } else if command == "count" {
                family.count();
                say("counted");
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

pub(crate) fn members(driven: &mut Driven) -> Vec<Pid> {
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

/// `kill_others` answers once nothing it killed runs, not when the kills
/// are sent: what the caller does next (reuse the folder, start another)
/// meets nothing of the old family.
#[cfg(feature = "async")]
#[test]
fn kill_others_returns_once_no_member_runs() {
    let (mut driven, member, sleeper) = contained_tree(helper("contained"));
    driven.tell("tree");
    let (second, second_sleeper) = (pid(&driven.expect("pid")), pid(&driven.expect("pid")));
    let refs: Vec<ProcessRef> = [member, sleeper, second, second_sleeper]
        .into_iter()
        .map(|pid| ProcessRef::open(pid).unwrap())
        .collect();
    driven.tell("kill");
    assert_eq!(driven.expect("killed"), "ok");
    for (n, pinned) in refs.iter().enumerate() {
        assert!(!pinned.alive(), "process {n} ({}) still runs", pinned.pid());
    }
    assert_eq!(members(&mut driven), Vec::<Pid>::new());
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

/// A member that starts a daemon (detached as a session) and exits. On
/// Unix the daemon leaves the family, as a `setsid` leaves a process
/// group, and lives on. On Windows the family's job forbids breakaway (Git
/// Bash breaks every command away from a job that allows it), so the
/// daemon is started inside, stays a member, and ends with the family.
#[cfg(feature = "async")]
#[test]
fn a_daemon_a_member_starts_leaves_the_family_only_on_unix() {
    let leaves = imp::tests::A_DAEMON_LEAVES_THE_FAMILY;
    let mut driven = Driven::start(helper("contained"));
    assert_eq!(driven.expect("contained"), "ok");
    driven.tell("breakaway");
    let said = driven.expect("pid");
    let daemon = pid(&said);
    let daemon_ref = ProcessRef::open(daemon).unwrap();
    assert_eq!(said.ends_with("refused true"), !leaves, "{said}");
    let found = members(&mut driven);
    assert_eq!(found.contains(&daemon), !leaves, "{daemon} in {found:?}");
    driven.tell("kill");
    assert_eq!(driven.expect("killed"), "ok");
    driven.child.kill().unwrap();
    driven.child.wait().unwrap();
    if leaves {
        std::thread::sleep(Duration::from_millis(1500));
        assert!(daemon_ref.alive(), "the daemon lives on");
        daemon_ref.kill().unwrap();
    } else {
        assert!(
            eventually(|| !daemon_ref.alive()),
            "the daemon ends with the family"
        );
    }
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
}

/// A child is reaped with its exit code, by this process and by a
/// successor it is handed to: the image an exec replaces this one with on
/// Unix, a successor started with the handle on Windows.
#[test]
fn a_child_is_reaped_here_and_by_a_successor_it_is_handed_to() {
    let mut child = helper("exit-after 0 7")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let pinned = ProcessRef::of_std_child(&child).unwrap();
    assert_eq!(pinned.reap().unwrap(), 7);
    // Reaped already on Unix, where this only fails.
    let _ = child.wait();

    let driven = Driven::start(helper("bequeath"));
    let said = driven.rest();
    assert!(said.iter().any(|line| line == "reaped 7"), "{said:?}");
}

/// What a successor is handed must fit: on Unix no handle, on Windows an
/// open one that names the pid.
#[test]
fn a_handover_that_does_not_fit_is_refused() {
    // SAFETY: refused before anything is taken.
    let error = unsafe { ProcessRef::inherited(0x7FFF_FFF0, Some(u64::MAX)) }.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{error}");
}
