//! `testnet ctl` from a shell's point of view: an empty net started in a
//! folder grows a host and a link, runs a fake agent, shows the terminal
//! client as a picture, keeps a capture of it all, and shuts down leaving
//! the evidence behind.

use std::path::Path;
use std::process::{Command, Output};

fn ctl(folder: &Path, args: &[&str]) -> Output {
    let output = Command::new(source_tree::bin!("testnet"))
        .arg("ctl")
        .args(args)
        .current_dir(folder)
        // A run's scope or ledger in the caller's environment is not this
        // test's.
        .env_remove("AMUX_QA_SCOPE")
        .env_remove("AMUX_QA_RUN")
        .env_remove("TESTNET_FOLDER")
        .env_remove("TMUX")
        .output()
        .expect("testnet runs");
    assert!(
        output.status.success(),
        "ctl {args:?} failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn said(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Shuts the net down however the test ends, so a failed assertion leaves
/// no daemons or tmux server behind.
struct Down<'a>(&'a Path);

impl Drop for Down<'_> {
    fn drop(&mut self) {
        let _ = Command::new(source_tree::bin!("testnet"))
            .args(["ctl", "down"])
            .current_dir(self.0)
            .output();
    }
}

#[cfg(not(windows))] // out of scope on Windows: the TUI (the terminal client in tmux)
#[test]
fn a_session_grows_a_net_drives_it_and_keeps_the_evidence() {
    let folder = tempfile::tempdir().expect("a folder");
    let folder = folder.path();
    std::fs::write(
        folder.join("hello.json"),
        r#"{"steps": [{"text": {"chunks": ["hello from the fake"]}}, "turn_end"]}"#,
    )
    .expect("a script");

    ctl(folder, &["up"]);
    let down = Down(folder);
    ctl(folder, &["host", "add", "desk"]);
    ctl(folder, &["host", "add", "laptop"]);
    let status = said(&ctl(folder, &["status"]));
    assert!(
        status.contains("agents outlive a stopped daemon by 20s"),
        "the status names each host's agent grace: {status}"
    );
    assert!(said(&ctl(folder, &["link", "desk", "laptop"])).contains("up"));
    let spawned = said(&ctl(
        folder,
        &[
            "spawn",
            "--host",
            "desk",
            "--script",
            "hello.json",
            "--prompt",
            "say hello",
            "--wait",
            "worker",
        ],
    ));
    assert!(spawned.contains("at rest"), "{spawned}");
    let chat = said(&ctl(folder, &["chat", "desk", "worker"]));
    assert!(chat.contains("hello from the fake"), "{chat}");

    // The laptop's terminal client shows the desk's agent once the link
    // has carried it over. Its tmux reads the net's own configuration,
    // whatever the operator's says.
    ctl(folder, &["tui", "open", "laptop"]);
    let conf = std::fs::read_to_string(folder.join(".testnet/tmux.conf"))
        .expect("the net's tmux configuration");
    assert!(conf.contains("extended-keys on"), "{conf}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let screen = said(&ctl(folder, &["tui", "screen", "laptop"]));
        if screen.contains("worker") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the laptop's client never listed worker:\n{screen}"
        );
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let shot = said(&ctl(folder, &["tui", "shot", "laptop"]));
    let png = shot.lines().next().expect("the shot's path");
    assert!(png.ends_with(".png") && Path::new(png).is_file(), "{shot}");
    let frame = std::fs::read_to_string(png.replace(".png", ".frame.json")).expect("its frame");
    assert!(
        frame.contains("worker"),
        "the frame holds the screen's text"
    );

    // A flapping link is whole again once the flap stops: what the desk
    // does next reaches the laptop.
    ctl(
        folder,
        &["flap", "desk", "laptop", "--down", "300ms", "--up", "300ms"],
    );
    std::thread::sleep(std::time::Duration::from_secs(1));
    assert!(said(&ctl(folder, &["flap", "stop"])).contains("restored"));
    ctl(
        folder,
        &[
            "spawn",
            "--host",
            "desk",
            "--script",
            "hello.json",
            "--prompt",
            "again",
            "--wait",
            "second",
        ],
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !said(&ctl(folder, &["fleet", "laptop"])).contains("second") {
        assert!(
            std::time::Instant::now() < deadline,
            "the laptop never heard of the agent made after the flap"
        );
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    let captured = said(&ctl(
        folder,
        &["capture", "lost-row", "--note", "a smoke test's capture"],
    ));
    assert!(captured.contains("captured into"), "{captured}");
    let finding = folder.join("findings/lost-row");
    for file in [
        "finding.md",
        "topology.json",
        "session.jsonl",
        "logs/testnet.log",
    ] {
        assert!(finding.join(file).is_file(), "the capture has {file}");
    }
    let topology = std::fs::read_to_string(finding.join("topology.json")).expect("topology");
    assert!(
        topology.contains("\"laptop\"") && topology.contains("\"desk\""),
        "the capture's topology has the hosts added at run time: {topology}"
    );
    for host in ["desk", "laptop"] {
        let dumps: Vec<_> = std::fs::read_dir(finding.join("dumps").join(host))
            .expect("a dump folder per host")
            .flatten()
            .collect();
        assert_eq!(dumps.len(), 1, "one dump of {host}");
        assert!(dumps[0].path().join("manifest.json").is_file());
    }
    let screens: Vec<String> = std::fs::read_dir(finding.join("screens"))
        .expect("screens")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        screens.iter().filter(|name| name.ends_with(".png")).count() >= 2,
        "the earlier shot and a fresh one: {screens:?}"
    );

    drop(down);
    assert!(
        !folder.join(".testnet/net").exists(),
        "the net's own files go with it"
    );
    let log = std::fs::read_to_string(folder.join("session.jsonl")).expect("the session log");
    assert!(
        log.lines().count() >= 9,
        "every command is in the session log"
    );
    assert!(finding.join("finding.md").is_file(), "the evidence stays");
}

/// A finding.md filled in as the template asks.
fn filled(title: &str, severity: &str) -> String {
    format!(
        "# {title}\n\n- **Tester:** t1\n- **Persona:** commuter\n- **Severity:** {severity}\n\
         - **Label:** undocumented\n- **Surfaces:** terminal and host record disagree\n\
         - **Topology:** desk and laptop linked\n\n## What I did\n\n1. ctl send desk worker hi\n\n\
         ## What I expected\n\nThe row stays.\n\n## What I saw\n\nThe row went.\n\n## Repro\n\n\
         Seen twice.\n\n## Suspected stage\n\n## Note at capture\n\nlost\n"
    )
}

/// `ctl ledger` as tester `tester` of the run in `run`, from its folder,
/// with `finding` as findings/NAME/finding.md first when given.
fn ledger(run: &Path, tester: &str, finding: Option<(&str, &str)>, args: &[&str]) -> Output {
    let folder = run.join(tester);
    if let Some((name, text)) = finding {
        std::fs::create_dir_all(folder.join("findings").join(name)).expect("a finding folder");
        std::fs::write(folder.join("findings").join(name).join("finding.md"), text)
            .expect("a finding");
    }
    std::fs::create_dir_all(&folder).expect("the tester's folder");
    Command::new(source_tree::bin!("testnet"))
        .args(["ctl", "ledger"])
        .args(args)
        .current_dir(&folder)
        .env("AMUX_QA_RUN", run)
        .env("AMUX_QA_TESTER", tester)
        .env_remove("AMUX_QA_SCOPE")
        .env_remove("TESTNET_FOLDER")
        .output()
        .expect("testnet runs")
}

fn refusal(output: &Output) -> String {
    assert!(
        !output.status.success(),
        "expected a refusal, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_finding_names_the_earlier_one_it_duplicates_or_says_it_is_new() {
    let run = tempfile::tempdir().expect("a run folder");
    let run = run.path();
    std::fs::write(run.join("ledger.jsonl"), "").expect("a ledger");
    let lost = filled("A row goes missing after a restart", "wrong state");

    // The run's first finding needs no word on whether it is new.
    assert!(
        ledger(
            run,
            "t1",
            Some(("lost-row", &lost)),
            &["finding", "lost-row"]
        )
        .status
        .success()
    );
    // Recorded once only.
    let again = refusal(&ledger(run, "t1", None, &["finding", "lost-row"]));
    assert!(again.contains("already in the ledger"), "{again}");

    // From then on a finding says which it is, and the refusal lists the
    // run's findings, the most alike first.
    let unsaid = refusal(&ledger(
        run,
        "t2",
        Some(("gone", &filled("The row goes missing", "wrong state"))),
        &["finding", "gone"],
    ));
    assert!(
        unsaid.contains("--duplicate-of") && unsaid.contains("--new"),
        "{unsaid}"
    );
    assert!(
        unsaid.contains("t1/lost-row  [wrong state]  A row goes missing after a restart"),
        "{unsaid}"
    );
    let unknown = refusal(&ledger(
        run,
        "t2",
        None,
        &["finding", "gone", "--duplicate-of", "t9/lost-row"],
    ));
    assert!(unknown.contains("t1/lost-row"), "{unknown}");
    let marked = ledger(
        run,
        "t2",
        None,
        &["finding", "gone", "--duplicate-of", "t1/lost-row"],
    );
    assert!(marked.status.success());
    assert!(
        said(&marked).contains("a duplicate of t1/lost-row — A row goes missing after a restart"),
        "the tester sees what it marked its finding a copy of: {}",
        said(&marked)
    );
    // A duplicate names the original, not another copy.
    let chained = refusal(&ledger(
        run,
        "t3",
        Some(("gone", &filled("Row missing", "wrong state"))),
        &["finding", "gone", "--duplicate-of", "t2/gone"],
    ));
    assert!(chained.contains("--duplicate-of t1/lost-row"), "{chained}");
    assert!(
        ledger(
            run,
            "t3",
            Some(("slow", &filled("Typing lags a second", "visual"))),
            &["finding", "slow", "--new"],
        )
        .status
        .success()
    );

    let findings = said(&ledger(run, "t4", None, &["findings"]));
    assert!(
        findings.contains("2 findings so far in this run (1 more seen again)"),
        "{findings}"
    );
    assert!(
        findings.contains(
            "t1/lost-row  [wrong state]  A row goes missing after a restart\n    seen again by t2/gone\n"
        ),
        "{findings}"
    );
    assert!(
        findings.contains("t3/slow  [visual]  Typing lags a second"),
        "{findings}"
    );
    let read = said(&ledger(run, "t4", None, &["read"]));
    assert!(
        read.contains("t2/gone — The row goes missing (duplicates t1/lost-row)"),
        "{read}"
    );
}

#[test]
fn a_finding_that_skips_the_template_is_refused_with_what_it_lacks() {
    let run = tempfile::tempdir().expect("a run folder");
    let run = run.path();
    std::fs::write(run.join("ledger.jsonl"), "").expect("a ledger");

    let drifted = refusal(&ledger(
        run,
        "t1",
        Some(("rank", &filled("Rows reorder", "high"))),
        &["finding", "rank"],
    ));
    assert!(
        drifted.contains("data loss, broken flow, wrong state, visual, wording")
            && drifted.contains("\"high\""),
        "a severity outside the five words is refused, naming them: {drifted}"
    );

    // The template as capture wrote it, its title the folder's name and
    // nothing filled in.
    let template = include_str!("../../../qa/FINDING.md")
        .replace("{{name}}", "notes")
        .replace("{{tester}}", "t1")
        .replace("{{note}}", "something");
    let unfilled = refusal(&ledger(
        run,
        "t1",
        Some(("notes", &template)),
        &["finding", "notes"],
    ));
    for lack in [
        "still the folder's name",
        "comments --> deleted",
        "`- **Severity:**` filled in",
        "`- **Surfaces:**` filled in",
        "the `## What I saw` section written",
    ] {
        assert!(unfilled.contains(lack), "{lack:?} is named: {unfilled}");
    }

    let shapeless = refusal(&ledger(
        run,
        "t1",
        Some(("prose", "The row went missing.\n")),
        &["finding", "prose"],
    ));
    assert!(
        shapeless.contains("a `# ` title") && shapeless.contains("its `## What I saw` section"),
        "{shapeless}"
    );
    assert_eq!(
        std::fs::read_to_string(run.join("ledger.jsonl")).expect("the ledger"),
        "",
        "nothing refused is recorded"
    );
}

/// Set in a re-run of this test binary that stands in for the process that
/// took a crashed serve's pid: it waits to be killed.
const STRANGER: &str = "TESTNET_CTL_STRANGER";

#[test]
fn down_never_kills_a_process_that_took_a_crashed_nets_pid() {
    if std::env::var_os(STRANGER).is_some() {
        std::thread::sleep(std::time::Duration::from_secs(120));
        return;
    }
    let folder = tempfile::tempdir().expect("a folder");
    let folder = folder.path();
    ctl(folder, &["up"]);
    let down = Down(folder);
    let record_path = folder.join(".testnet/net.json");
    let mut record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&record_path).expect("net.json"))
            .expect("its record");
    let serve = record["pid"].as_u64().expect("serve's pid") as u32;
    // Serve crashes, and another process now has the pid net.json names.
    os::process::ProcessRef::open(serve)
        .expect("serve runs")
        .kill()
        .expect("serve is killed");
    let mut stranger = Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "ctl::down_never_kills_a_process_that_took_a_crashed_nets_pid",
            "--exact",
        ])
        .env(STRANGER, "1")
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("a stranger");
    record["pid"] = stranger.id().into();
    std::fs::write(&record_path, record.to_string()).expect("net.json rewritten");

    let started = std::time::Instant::now();
    ctl(folder, &["down"]);
    assert!(
        stranger.try_wait().expect("its status").is_none(),
        "the process that took the pid is untouched"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "down does not wait on a process that is not the net's"
    );
    let _ = stranger.kill();
    drop(down);
}

/// Set in a re-run of this test binary that stands in for an agent of
/// another installation: it names the agent directory whose lock it takes.
const STRANGER_AGENT: &str = "TESTNET_CTL_STRANGER_AGENT";
/// What the stand-in prints once it holds the lock and serves the socket.
const STRANGER_HELD: &str = "testnet-ctl-test: the stranger holds its lock";

/// The stand-in for another installation's agent, as its lock and socket
/// show one: it takes the lock of `dir`, serves its agent socket, says so,
/// and waits for its input to close.
fn hold_a_strangers_lock(dir: &Path) {
    use std::io::Write as _;
    let _lock = agent_dir::lock(dir).unwrap().expect("the lock is free");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    std::fs::create_dir_all(dir.join(agent_dir::PRIVATE)).unwrap();
    let mut listener = {
        let _entered = runtime.enter();
        agent_dir::local_socket::LocalListener::bind(&agent_dir::agent_sock::path(dir)).unwrap()
    };
    runtime.spawn(async move {
        while let Ok(stream) = listener.accept().await {
            drop(stream);
        }
    });
    println!("{STRANGER_HELD}");
    std::io::stdout().flush().unwrap();
    let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut Vec::new());
}

/// The agents a crashed net leaves running are `ctl down`'s to end, found
/// by their locks where the net's hosts keep their agents. Another
/// installation kept in a host's workspace is not the net's: its agent,
/// holding its lock and serving its socket as the net's do, lives on.
#[test]
fn down_ends_the_nets_agents_and_no_other_installations() {
    use std::io::BufRead as _;
    if let Some(dir) = std::env::var_os(STRANGER_AGENT) {
        hold_a_strangers_lock(Path::new(&dir));
        return;
    }
    let folder = tempfile::tempdir().expect("a folder");
    let folder = folder.path();
    std::fs::write(
        folder.join("hello.json"),
        r#"{"steps": [{"text": {"chunks": ["hello from the fake"]}}, "turn_end"]}"#,
    )
    .expect("a script");
    ctl(folder, &["up"]);
    let down = Down(folder);
    ctl(folder, &["host", "add", "desk"]);
    let spawned = said(&ctl(
        folder,
        &[
            "spawn",
            "--host",
            "desk",
            "--script",
            "hello.json",
            "--prompt",
            "say hello",
            "--wait",
            "worker",
        ],
    ));
    assert!(spawned.contains("at rest"), "{spawned}");
    let status: serde_json::Value =
        serde_json::from_slice(&ctl(folder, &["--json", "status"]).stdout).expect("the status");
    let desk = status["hosts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|host| host["name"] == "desk")
        .and_then(|host| host["dir"].as_str())
        .map(std::path::PathBuf::from)
        .expect("the desk's folder");

    // The net's agent, pinned by the lock it holds in the desk's
    // installation.
    let agents: Vec<_> = std::fs::read_dir(desk.join("data").join("profiles"))
        .expect("the desk's profiles")
        .flatten()
        .filter_map(|profile| std::fs::read_dir(profile.path().join("agents")).ok())
        .flatten()
        .flatten()
        .collect();
    let worker = agents
        .iter()
        .find_map(|entry| match agent_dir::lock_holder(&entry.path()) {
            Ok(agent_dir::Holder::Process(process)) => Some(process),
            _ => None,
        })
        .expect("the worker holds its lock");

    // Another installation, laid out as one is, in the desk's workspace.
    let theirs = desk
        .join("work")
        .join("elsewhere")
        .join("data")
        .join("profiles")
        .join("theirs")
        .join("agents")
        .join("theirs");
    std::fs::create_dir_all(&theirs).expect("their agent's folder");
    let mut stranger = Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "ctl::down_ends_the_nets_agents_and_no_other_installations",
            "--exact",
            "--nocapture",
        ])
        .env(STRANGER_AGENT, &theirs)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("a stranger");
    let mut lines = std::io::BufReader::new(stranger.stdout.take().expect("its output")).lines();
    assert!(
        lines
            .by_ref()
            .map_while(Result::ok)
            .any(|line| line.trim() == STRANGER_HELD),
        "the stranger ended without taking its lock: {:?}",
        stranger.wait()
    );
    std::thread::spawn(move || lines.for_each(drop));

    // Serve crashes, so its agents are down's to end: the worker outlives
    // its daemon by the net's 20 seconds of grace.
    let record: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(folder.join(".testnet/net.json")).expect("net.json"),
    )
    .expect("its record");
    let serve = record["pid"].as_u64().expect("serve's pid") as u32;
    os::process::ProcessRef::open(serve)
        .expect("serve runs")
        .kill()
        .expect("serve is killed");
    ctl(folder, &["down"]);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while worker.alive() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let worker_alive = worker.alive();
    let _ = worker.kill();
    let stranger_alive = stranger.try_wait().expect("its status").is_none();
    drop(stranger.stdin.take());
    let _ = stranger.wait();
    drop(down);
    assert!(
        !worker_alive,
        "down left the net's agent {} running",
        worker.pid()
    );
    assert!(stranger_alive, "down ended another installation's agent");
}

/// `testnet ctl up` and the `serve` it starts are for a person to drive by
/// hand, as well as for tests: outside Buck's test runner, which only
/// tests run under, an empty net still comes up, answers and goes down.
#[test]
fn a_net_comes_up_outside_the_test_runner() {
    let folder = tempfile::tempdir().expect("a folder");
    let folder = folder.path();
    let by_hand = |args: &[&str]| {
        Command::new(source_tree::bin!("testnet"))
            .arg("ctl")
            .args(args)
            .current_dir(folder)
            .env_remove(os::testing::TEST_RUNNER_ENV)
            .env_remove("AMUX_QA_SCOPE")
            .env_remove("AMUX_QA_RUN")
            .env_remove("TESTNET_FOLDER")
            .env_remove("TMUX")
            .output()
            .expect("testnet runs")
    };
    let up = by_hand(&["up"]);
    let down = Down(folder);
    assert!(
        up.status.success(),
        "ctl up failed outside the runner:\n{}{}",
        said(&up),
        String::from_utf8_lossy(&up.stderr)
    );
    let status = by_hand(&["status"]);
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    drop(down);
}

#[test]
fn a_qa_tester_is_refused_fake_agents() {
    let run = tempfile::tempdir().expect("a run folder");
    // A topology may start a QA run's net only when all of it is real.
    std::fs::write(
        run.path().join("topology.json"),
        r#"{"hosts": [{"name": "desk", "provider": "fake"}]}"#,
    )
    .expect("a topology");
    for args in [
        &["host", "add", "desk", "--provider", "fake"][..],
        &["host", "add", "desk", "--script", "hello.json"],
        &["spawn", "--host", "desk", "--provider", "fake", "worker"],
        &[
            "spawn",
            "--host",
            "desk",
            "--script",
            "hello.json",
            "worker",
        ],
        &["up", "topology.json"],
    ] {
        let refused = Command::new(source_tree::bin!("testnet"))
            .arg("ctl")
            .args(args)
            .current_dir(run.path())
            .env("AMUX_QA_RUN", run.path())
            .output()
            .expect("testnet runs");
        assert!(!refused.status.success(), "{args:?} is refused");
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("real agents only"),
            "{args:?}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
}

#[test]
fn a_session_named_in_the_environment_is_found_from_any_folder() {
    let session = tempfile::tempdir().expect("a session folder");
    let session = session.path();
    std::fs::write(
        session.join("session.jsonl"),
        r#"{"time":"2026-10-12T09:00:00+00:00","args":["status"],"ok":true,"reply":"net at somewhere","files":[]}"#,
    )
    .expect("a session log");
    let elsewhere = session.join("findings/lost-row");
    std::fs::create_dir_all(&elsewhere).expect("a folder inside it");
    let run = |args: &[&str], named: bool| {
        let mut command = Command::new(source_tree::bin!("testnet"));
        command
            .arg("ctl")
            .args(args)
            .current_dir(&elsewhere)
            .env_remove("AMUX_QA_RUN")
            .env_remove("AMUX_QA_SCOPE")
            .env_remove("TESTNET_FOLDER");
        if named {
            command.env("TESTNET_FOLDER", session);
        }
        command.output().expect("testnet runs")
    };

    let log = run(&["log"], true);
    assert!(
        said(&log).contains("ctl status"),
        "the named session's log, read from a folder inside it: {}",
        String::from_utf8_lossy(&log.stderr)
    );
    assert!(
        !run(&["log"], false).status.success(),
        "without the name, the current folder is the session"
    );
    let status = run(&["status"], true);
    let refused = String::from_utf8_lossy(&status.stderr);
    assert!(
        refused.contains(&format!("no net in {}", session.display())),
        "{refused}"
    );
}
