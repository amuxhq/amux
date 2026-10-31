//! Releases overlapping on one machine, with the built binaries: agents of
//! the previous build working on across an update to the build under test,
//! a fleet of the previous build staying open through that update,
//! a store the previous build created surviving a rollback before
//! activation, and the generated LaunchAgent restarted under a running
//! turn. The supervisor's own rules (rollback after K failed starts, the
//! rejected build, a crash after go, an interrupted rollback, the daemon
//! exiting when its supervisor dies) are driven in the node crate's
//! supervisor tests.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::BufRead as _;
use std::path::Path;

use provider_fakes::Step;
use support::desk::{Desk, LONG_GRACE_SECS, lock_held, say};
use support::term::Term;
use support::{
    Channel, amux_binary, chat, client, count, created_id, gated_turn, says, texts, turns_ended,
    until,
};

use crate::support;

/// The agent process hosting the directory `dir`, found by the agent id
/// its command line ends with.
fn agent_pid(dir: &Path) -> Option<u32> {
    let id = dir.file_name()?.to_string_lossy().into_owned();
    let output = std::process::Command::new("pgrep")
        .args(["-f", &format!(" agent .*{id}$")])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only probes.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Agents the previous build started keep their process, and their inode,
/// through an update: the new daemon adopts them and they take new turns.
#[tokio::test(flavor = "multi_thread")]
async fn agents_of_the_previous_build_work_on_after_an_update() {
    let running = node::version();
    let previous = version_stamp::previous(running).unwrap();
    let channel = Channel::serve().await;
    let gate_dir = tempfile::tempdir().unwrap();
    let gate = gate_dir.path().join("gate");
    std::fs::write(&gate, b"").unwrap();
    // Two turns: a live agent plays on through one script.
    let turns = [gated_turn(&gate), gated_turn(&gate)].concat();
    let desk = Desk::new(true, LONG_GRACE_SECS, &previous, channel.url(), turns);
    let work = desk.work.to_string_lossy().into_owned();
    desk.run(&["server", "start"]).await;

    let mut agents: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    for (name, agent, interface) in [
        ("terminal", "claude", "native"),
        ("headless", "claude", "amux"),
        ("codex", "codex", "amux"),
    ] {
        let created = desk
            .run(&[
                "new",
                agent,
                "--in",
                interface,
                "--name",
                name,
                "--cwd",
                &work,
                "--prompt",
                "first task",
            ])
            .await;
        agents.insert(name, created_id(&created));
    }
    let mut pids = BTreeMap::new();
    for (name, id) in &agents {
        let dir = desk.agent_dir(id);
        until(&format!("{name} finishes its first turn"), || async {
            turns_ended(&dir, 1)
        })
        .await
        .unwrap();
        pids.insert(*name, agent_pid(&dir).expect("the agent runs"));
    }

    channel.publish(running, std::fs::read(amux_binary()).unwrap());
    let updated = desk.run(&["update"]).await;
    assert!(
        updated.contains(&format!("amux {running} is running.")),
        "{updated}"
    );

    let chats = client(&desk.socket).await;
    for (name, id) in &agents {
        let pid = agent_pid(&desk.agent_dir(id));
        assert_eq!(
            pid,
            Some(pids[name]),
            "{name} kept its process through the update"
        );
        desk.run(&["send", name, "second", "task"]).await;
        until(
            &format!("{name} finishes a turn under the new daemon"),
            || {
                let mut chats = chats.clone();
                async move { says(&chat(&mut chats, id).await, "finished", 2) }
            },
        )
        .await
        .unwrap();
    }
    for name in agents.keys() {
        desk.run(&["stop", name]).await;
    }
}

/// A fleet started from the previous build stays open through an update to
/// the build under test: it reconnects to the new daemon on its own, keeps
/// the chat it had open with the turn that was running, says the running
/// daemon is newer, and a prompt typed into it afterwards is answered.
#[tokio::test(flavor = "multi_thread")]
async fn a_tui_of_the_previous_build_works_on_after_an_update() {
    let running = node::version();
    let previous = version_stamp::previous(running).unwrap();
    let channel = Channel::serve().await;
    let gate_dir = tempfile::tempdir().unwrap();
    let gate = gate_dir.path().join("gate");
    let mut turns = gated_turn(&gate);
    turns.extend([
        Step::Text {
            chunks: vec!["answered after the update".to_owned()],
        },
        Step::TurnEnd,
    ]);
    let desk = Desk::new(true, LONG_GRACE_SECS, &previous, channel.url(), turns);
    let work = desk.work.to_string_lossy().into_owned();
    desk.run(&["server", "start"]).await;
    let agent = created_id(
        &desk
            .run(&[
                "new",
                "claude",
                "--in",
                "amux",
                "--name",
                "helper",
                "--cwd",
                &work,
                "--prompt",
                "first task",
            ])
            .await,
    );

    // The previous build's own binary, as the install had it before the
    // update, opens the fleet and the chat mid-turn.
    let tui = desk.root.path().join("previous-amux");
    std::fs::copy(desk.amux_path(), &tui).unwrap();
    let mut term = Term::run(&desk, &tui, &[], 30, 110);
    term.shows("helper").await;
    term.type_keys(b"\r").await;
    term.shows("started").await;
    term.shows("Working · ").await;
    let daemon = desk.daemon_pid();

    channel.publish(running, std::fs::read(amux_binary()).unwrap());
    let (updated, ()) = tokio::join!(desk.run(&["update"]), async {
        // Drained while the update runs, so the terminal never backs up.
        term.until("the old daemon goes", |_| desk.daemon_pid() != daemon)
            .await;
    });
    assert!(
        updated.contains(&format!("amux {running} is running.")),
        "{updated}"
    );
    say(format!(
        "-- amux update: daemon pid {daemon} -> {}; the {previous} fleet stays open",
        desk.daemon_pid()
    ));

    // The same chat, live again under the new daemon: the running turn
    // finishes into it.
    std::fs::write(&gate, b"").unwrap();
    term.shows("finished").await;
    assert!(
        term.contents().contains("first task"),
        "{}",
        term.contents()
    );
    term.type_keys(b"second task\r").await;
    term.shows("answered after the update").await;
    say(format!(
        "-- the {previous} chat after the update:\n{}",
        term.contents()
    ));

    // Back at the fleet it names the newer daemon.
    term.type_keys(b"\x01s").await;
    term.shows(&format!("amux {running} running · restart to update"))
        .await;
    term.shows("? keys").await;
    say(format!("-- the {previous} fleet:\n{}", term.contents()));
    term.type_keys(b"q").await;
    term.exits().await;

    let mut chats = client(&desk.socket).await;
    let chat = chat(&mut chats, &agent).await;
    assert_eq!(count(&chat, "answered after the update"), 1);
    desk.run(&["stop", "helper"]).await;
}

/// The build under test opens a store the previous build created and is
/// killed before it prepared: the rollback case. The previous build opens
/// the store again and does real work in it.
#[tokio::test(flavor = "multi_thread")]
async fn the_previous_build_reopens_a_store_the_new_one_opened_before_prepared() {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;

    let running = node::version();
    let previous = version_stamp::previous(running).unwrap();
    let gate_dir = tempfile::tempdir().unwrap();
    let gate = gate_dir.path().join("gate");
    std::fs::write(&gate, b"").unwrap();
    let desk = Desk::new(
        false,
        LONG_GRACE_SECS,
        &previous,
        "http://127.0.0.1:1",
        gated_turn(&gate),
    );
    let work = desk.work.to_string_lossy().into_owned();
    desk.run(&["server", "start"]).await;
    let agent = created_id(
        &desk
            .run(&[
                "new",
                "claude",
                "--in",
                "amux",
                "--name",
                "headless",
                "--cwd",
                &work,
                "--prompt",
                "first task",
            ])
            .await,
    );
    let dir = desk.agent_dir(&agent);
    until("the first turn ends", || async { turns_ended(&dir, 1) })
        .await
        .unwrap();
    desk.run(&["stop", "headless"]).await;
    desk.run(&["server", "stop"]).await;

    // The build under test starts on the same store as a supervisor would
    // start it, opens it, looks, says prepared, and dies before go.
    let (daemon_reads, go) = std::io::pipe().unwrap();
    let (prepared, daemon_writes) = std::io::pipe().unwrap();
    let ends = [daemon_reads.as_raw_fd(), daemon_writes.as_raw_fd()];
    let mut command = std::process::Command::new(amux_binary());
    command
        .arg("daemon")
        .env("AMUX_CONFIG", &desk.config)
        .env(node::PIPE_ENV, format!("{},{}", ends[0], ends[1]))
        .env(support::NO_DISCOVERY.0, support::NO_DISCOVERY.1)
        .env_remove("AMUX_LOG")
        .stdin(std::process::Stdio::null());
    // SAFETY: fcntl in the child before exec touches only its descriptors.
    unsafe {
        command.pre_exec(move || {
            for fd in ends {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
            Ok(())
        });
    }
    let mut daemon = command.spawn().unwrap();
    drop((daemon_reads, daemon_writes));
    let said = tokio::task::spawn_blocking(move || {
        let mut line = String::new();
        std::io::BufReader::new(prepared)
            .read_line(&mut line)
            .map(|_| line)
    });
    let said = tokio::time::timeout(support::PATIENCE, said)
        .await
        .expect("the new build prepares")
        .unwrap()
        .unwrap();
    assert_eq!(said.trim(), node::PREPARED);
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    drop(go);
    say(format!(
        "-- amux {running} opened the store and looked, said prepared, and was killed before go"
    ));
    assert!(!lock_held(&desk.data.join(node::INSTALLATION_LOCK)));

    desk.run(&["server", "start"]).await;
    let mut chats = client(&desk.socket).await;
    let before = chat(&mut chats, &agent).await;
    assert_eq!(count(&before, "finished"), 1, "{:?}", texts(&before));
    desk.run(&["resume", "headless", "second", "task"]).await;
    until("the previous build finishes a new turn", || {
        let mut chats = chats.clone();
        let agent = &agent;
        async move { says(&chat(&mut chats, agent).await, "finished", 2) }
    })
    .await
    .unwrap();
    let after = chat(&mut chats, &agent).await;
    for item in &before {
        assert!(after.contains(item), "{item:?} is still in the store");
    }
    desk.run(&["stop", "headless"]).await;
}

/// Unloads a LaunchAgent however the test ends.
struct Loaded {
    domain: String,
}

impl Drop for Loaded {
    fn drop(&mut self) {
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &self.domain])
            .output();
    }
}

/// The generated LaunchAgent runs amux supervise; restarting the service
/// under a running turn leaves the agent's process alone, and the turn
/// finishes into the store.
#[tokio::test(flavor = "multi_thread")]
async fn restarting_the_launch_agent_leaves_the_agent_running() {
    if std::process::Command::new("launchctl")
        .arg("help")
        .output()
        .is_err()
    {
        eprintln!("skipped: this machine has no launchctl, so no LaunchAgent to restart");
        return;
    }
    let gate_dir = tempfile::tempdir().unwrap();
    let gate = gate_dir.path().join("gate");
    let desk = Desk::new(
        true,
        LONG_GRACE_SECS,
        node::version(),
        "http://127.0.0.1:1",
        gated_turn(&gate),
    );
    let work = desk.work.to_string_lossy().into_owned();
    let path = format!(
        "{}:{}",
        desk.bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let binary = desk.amux_path();
    let log = desk.data.join("supervisor.log");
    let item = node::supervisor::login::LoginItem {
        binary: &binary,
        config: Some(&desk.config),
        path: &path,
        log: &log,
    };
    // The generated unit under a label of its own, with the fakes' script
    // and the scripted local network in its environment.
    let label = format!("sh.amux.supervise.test.{}", std::process::id());
    let plist = item
        .launch_agent()
        .replace(node::supervisor::login::LAUNCH_AGENT_LABEL, &label)
        .replace(
            "\t\t<key>PATH</key>",
            &format!(
                "\t\t<key>{}</key>\n\t\t<string>{}</string>\n\t\t<key>CLAUDE_CONFIG_DIR</key>\n\t\t<string>{}</string>\n\t\t<key>{}</key>\n\t\t<string>{}</string>\n\t\t<key>PATH</key>",
                provider_fakes::SCRIPT_ENV,
                desk.root.path().join("script.json").display(),
                desk.root.path().join("claude").display(),
                support::NO_DISCOVERY.0,
                support::NO_DISCOVERY.1,
            ),
        );
    // launchd opens the log before amux runs, so its directory must exist,
    // as `amux init` makes sure.
    std::fs::create_dir_all(&desk.data).unwrap();
    let file = desk.root.path().join(format!("{label}.plist"));
    std::fs::write(&file, plist).unwrap();
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let status = std::process::Command::new("launchctl")
        .args(["bootstrap", &format!("gui/{uid}")])
        .arg(&file)
        .status()
        .unwrap();
    assert!(status.success(), "launchctl bootstrap failed");
    let _loaded = Loaded {
        domain: format!("gui/{uid}/{label}"),
    };
    // What `amux init` runs next: launchd defers a conditional job's first
    // spawn otherwise.
    let status = std::process::Command::new("launchctl")
        .args(["kickstart", &format!("gui/{uid}/{label}")])
        .status()
        .unwrap();
    assert!(status.success(), "launchctl kickstart failed");
    until("launchd's amux answers", || async {
        std::os::unix::net::UnixStream::connect(&desk.socket)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .await
    .unwrap();
    let supervisor = desk
        .supervisor_pid()
        .expect("launchd started amux supervise");

    let agent = created_id(
        &desk
            .run(&[
                "new",
                "claude",
                "--in",
                "amux",
                "--name",
                "headless",
                "--cwd",
                &work,
                "--prompt",
                "first task",
            ])
            .await,
    );
    let chats = client(&desk.socket).await;
    until("the agent is mid-turn", || {
        let mut chats = chats.clone();
        let agent = &agent;
        async move { says(&chat(&mut chats, agent).await, "started", 1) }
    })
    .await
    .unwrap();
    let dir = desk.agent_dir(&agent);
    let pid = agent_pid(&dir).expect("the agent runs");

    let status = std::process::Command::new("launchctl")
        .args(["kickstart", "-k", &format!("gui/{uid}/{label}")])
        .status()
        .unwrap();
    assert!(status.success(), "launchctl kickstart failed");
    until("launchd restarts amux supervise", || async {
        let now = desk.supervisor_pid();
        now.is_some_and(|now| now != supervisor)
            .then_some(())
            .ok_or_else(|| format!("supervisor pid {now:?}, was {supervisor}"))
    })
    .await
    .unwrap();
    say(format!(
        "-- launchctl kickstart -k restarted amux supervise (pid {supervisor} -> {}) mid-turn",
        desk.supervisor_pid().unwrap()
    ));
    assert!(alive(pid), "the agent outlived the service restart");
    assert_eq!(agent_pid(&dir), Some(pid), "the same agent process");

    std::fs::write(&gate, b"").unwrap();
    until("the daemon answers again", || async {
        std::os::unix::net::UnixStream::connect(&desk.socket)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .await
    .unwrap();
    let chats = client(&desk.socket).await;
    until("the turn finishes into the store", || {
        let mut chats = chats.clone();
        let agent = &agent;
        async move { says(&chat(&mut chats, agent).await, "finished", 1) }
    })
    .await
    .unwrap();
    assert_eq!(agent_pid(&dir), Some(pid));
    desk.run(&["stop", "headless"]).await;
}
