//! What an agent's working tree changed against a base, and what deleting
//! it would take, as its host answers them. The changes run against HEAD,
//! a named branch, or the default rule from the base branch the agent's
//! spec recorded, and say which base they used; each file carries the
//! language the clients colour it by. The delete preview says whether the
//! workspace goes (its host made it and no agent outside the family works
//! in it), its branch, the branch's own commits by the rule the removal
//! applies, and the uncommitted totals. One agent's turn end has the others
//! in its checkout read their folders' facts again, so every row's totals
//! follow what any of them did there.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use support::synthetic::*;
use support::*;
use wire::experimental::AgentWorkspace;
use wire::get_changes_request::Base;
use wire::{
    ChangedFile, ErrorCode, FileStatus, GetChangesResponse, Lifecycle, PreviewDeleteResponse,
};

use crate::support;

const SEGMENTS: u64 = 1 << 20;

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=amux",
            "-c",
            "user.email=amux@example.invalid",
        ])
        .args(["-c", "commit.gpgsign=false", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// An agent that has exited, its folder `cwd` still there, with a first
/// spec recording `base_branch` as the branch its worktree was made from.
fn exited(install: &Install, cwd: &Path, base_branch: Option<&str>) -> SyntheticAgent {
    exited_child(install, cwd, base_branch, None)
}

/// [`exited`], a child of `parent`.
fn exited_child(
    install: &Install,
    cwd: &Path,
    base_branch: Option<&str>,
    parent: Option<store::AgentKey>,
) -> SyntheticAgent {
    let mut agent = SyntheticAgent::new(install, "coder", SEGMENTS);
    agent.parent = parent;
    let mut row = agent.row(install);
    row.cwd = cwd.to_string_lossy().into_owned();
    row.lifecycle = Lifecycle::Exited as i32;
    row.exit_cause = wire::ExitCause::Finished as i32;
    install.store().put_agent(&row).unwrap();
    let spec = wire::AgentSpec {
        agent_id: agent.id.as_bytes().to_vec(),
        kind: KIND as i32,
        cwd: row.cwd.clone(),
        name: agent.name.clone(),
        incarnation: 1,
        base_branch: base_branch.map(str::to_owned),
        ..wire::AgentSpec::default()
    };
    std::fs::write(
        agent.dir.join("spec.1"),
        prost::Message::encode_to_vec(&spec),
    )
    .unwrap();
    agent
}

/// The work folder on `feature`, which left `main` at the commit returned:
/// on the branch, one commit and a file moved; then an uncommitted change,
/// an untracked file and a script with no extension.
fn feature_branch(work: &Path) -> String {
    git(work, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("kept.txt"), "one\n").unwrap();
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("src/old.rs"), "fn main() {}\n").unwrap();
    git(work, &["add", "."]);
    git(work, &["commit", "-q", "-m", "first"]);
    let fork = git(work, &["rev-parse", "HEAD"]);
    git(work, &["switch", "-q", "-c", "feature"]);
    std::fs::write(work.join("kept.txt"), "one\ntwo\n").unwrap();
    git(work, &["mv", "src/old.rs", "src/moved.rs"]);
    git(work, &["commit", "-q", "-am", "second"]);
    std::fs::write(work.join("kept.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(work.join("deploy"), "#!/bin/sh\necho deployed\n").unwrap();
    fork
}

fn file(changes: &GetChangesResponse, path: &str) -> ChangedFile {
    changes
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("{path} in {:#?}", changes.files))
        .clone()
}

fn paths(changes: &GetChangesResponse) -> Vec<&str> {
    changes
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_answer_each_base_and_say_which_they_used() {
    let install = Install::new();
    let fork = feature_branch(&install.work);
    let head = git(&install.work, &["rev-parse", "HEAD"]);
    let recorded = exited(&install, &install.work, Some("main"));
    // No spec: the default rule finds the repository's own default branch.
    let unrecorded = exited(&install, &install.work, None);
    std::fs::remove_file(unrecorded.dir.join("spec.1")).unwrap();
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);

    let uncommitted = runtime
        .changes(recorded.id, Some(Base::Head(wire::Empty {})))
        .await
        .unwrap();
    println!("against HEAD: {uncommitted:#?}");
    assert_eq!(uncommitted.base_branch, "", "it ran from HEAD");
    assert_eq!(uncommitted.base_commit, head);
    assert_eq!(paths(&uncommitted), ["deploy", "kept.txt"]);
    let kept = file(&uncommitted, "kept.txt");
    assert_eq!((kept.added, kept.removed), (1, 0));
    assert_eq!(kept.status, FileStatus::Changed as i32);
    let deploy = file(&uncommitted, "deploy");
    assert_eq!(deploy.status, FileStatus::Created as i32);
    assert_eq!(deploy.language, "bash", "by its #! line");

    let on_branch = runtime
        .changes(recorded.id, Some(Base::Branch("main".into())))
        .await
        .unwrap();
    println!("against main: {on_branch:#?}");
    assert_eq!(on_branch.base_branch, "main");
    assert_eq!(on_branch.base_commit, fork);
    assert_eq!(paths(&on_branch), ["deploy", "kept.txt", "src/moved.rs"]);
    assert_eq!((file(&on_branch, "kept.txt").added), 2);
    let moved = file(&on_branch, "src/moved.rs");
    assert_eq!(moved.status, FileStatus::Renamed as i32);
    assert_eq!(moved.old_path.as_deref(), Some("src/old.rs"));
    assert_eq!(moved.language, "rust");

    // The default rule: from where the branch left the base branch the
    // spec recorded, or with none recorded, the repository's own.
    for agent in [&recorded, &unrecorded] {
        assert_eq!(
            runtime.changes(agent.id, None).await.unwrap(),
            on_branch,
            "{}",
            agent.dir.display()
        );
    }

    let refused = runtime
        .changes(recorded.id, Some(Base::Branch("nowhere".into())))
        .await
        .unwrap_err()
        .to_wire();
    assert_eq!(
        refused.code,
        ErrorCode::FailedPrecondition as i32,
        "{refused:?}"
    );
    let refused = runtime
        .changes(recorded.id, Some(Base::Branch(String::new())))
        .await
        .unwrap_err()
        .to_wire();
    assert_eq!(
        refused.code,
        ErrorCode::InvalidArgument as i32,
        "{refused:?}"
    );
    let missing = runtime
        .changes(uuid::Uuid::new_v4(), None)
        .await
        .unwrap_err()
        .to_wire();
    assert_eq!(missing.code, ErrorCode::NotFound as i32);

    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// On the base branch itself, the default rule runs from where the branch
/// left its upstream, and says so: what is not pushed yet is what changed.
#[tokio::test(flavor = "multi_thread")]
async fn on_the_base_branch_the_changes_run_from_its_upstream() {
    let install = Install::new();
    let origin = install.path("origin.git");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "-q", "--bare", "-b", "main"]);
    let work = install.work.clone();
    git(&work, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("kept.txt"), "one\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "pushed"]);
    git(
        &work,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    git(&work, &["push", "-q", "-u", "origin", "main"]);
    let pushed = git(&work, &["rev-parse", "HEAD"]);
    std::fs::write(work.join("kept.txt"), "one\ntwo\n").unwrap();
    git(&work, &["commit", "-q", "-am", "not pushed"]);
    let agent = exited(&install, &work, Some("main"));
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);

    let changes = runtime.changes(agent.id, None).await.unwrap();
    assert_eq!(changes.base_branch, "origin/main");
    assert_eq!(changes.base_commit, pushed);
    assert_eq!(paths(&changes), ["kept.txt"]);

    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// A repository named `shop` with `feature` checked out, one commit past
/// `main`.
fn shop(install: &Install) -> PathBuf {
    let repo = install.path("shop");
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "start"]);
    git(&repo, &["switch", "-q", "-c", "feature"]);
    std::fs::write(repo.join("feature.txt"), "on feature\n").unwrap();
    git(&repo, &["add", "feature.txt"]);
    git(&repo, &["commit", "-q", "-m", "feature"]);
    repo
}

fn launch(install: &Install, workspaces: bool) -> node::Launch {
    let mut launch = install.launch("idle", vec![]);
    if workspaces {
        launch.experiments =
            ::settings::experiments::Enabled::of(&[&::settings::experiments::WORKSPACES]);
    }
    launch
}

fn entry(agent: &wire::Agent) -> Option<AgentWorkspace> {
    wire::experimental::WORKSPACE.get(&agent.experimental)
}

/// Waits for `agent`'s row to say its workspace is made, and returns it.
async fn made(runtime: &node::ProfileRuntime, agent: &wire::Agent) -> wire::Agent {
    until("the workspace to be made", || async {
        let row = runtime
            .agent(id_of(agent))
            .await
            .map_err(|error| error.to_string())?;
        match entry(&row) {
            Some(entry) if !entry.creating => Ok(row),
            other => Err(format!("{other:?}")),
        }
    })
    .await
    .unwrap()
}

/// Waits for every agent in `agents` to carry its workspace on its row.
async fn members(runtime: &node::ProfileRuntime, agents: &[&wire::Agent]) {
    for agent in agents {
        made(runtime, agent).await;
    }
}

async fn preview(runtime: &node::ProfileRuntime, agent: &wire::Agent) -> PreviewDeleteResponse {
    runtime.preview_delete(id_of(agent)).await.unwrap()
}

fn totals(files: u32, added: u32, removed: u32) -> Option<wire::ChangeTotals> {
    Some(wire::ChangeTotals {
        files,
        added,
        removed,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn the_preview_says_what_a_delete_would_take() {
    let install = Install::new();
    let repo = shop(&install);
    let daemon = install.start("boot-1", launch(&install, true)).await;
    let runtime = runtime(&daemon, &install);

    let lead = runtime
        .spawn(
            wire::CreateAgentRequest {
                new_workspace: true,
                ..create(&repo, "checkout-fix", None)
            },
            None,
        )
        .await
        .unwrap();
    let lead = made(&runtime, &lead).await;
    let tree = PathBuf::from(&lead.cwd);
    assert_eq!(
        preview(&runtime, &lead).await,
        PreviewDeleteResponse {
            removes_workspace: true,
            branch: Some("checkout-fix".into()),
            own_commits: Some(0),
            uncommitted: totals(0, 0, 0),
        },
        "a clean new worktree, and its branch, go with it"
    );

    // A commit of its own, and work not committed.
    std::fs::write(tree.join("feature.txt"), "on feature\nfixed\n").unwrap();
    git(&tree, &["commit", "-q", "-am", "fix"]);
    std::fs::write(tree.join("feature.txt"), "on feature\nfixed\nagain\n").unwrap();
    std::fs::write(tree.join("notes.txt"), "a\nb\n").unwrap();
    let dirty = preview(&runtime, &lead).await;
    println!("with work: {dirty:?}");
    assert_eq!(
        dirty,
        PreviewDeleteResponse {
            removes_workspace: true,
            branch: Some("checkout-fix".into()),
            own_commits: Some(1),
            uncommitted: totals(2, 3, 0),
        }
    );

    // Its child working there goes with it, so the workspace still does;
    // the child's own delete leaves its parent there.
    let child = runtime
        .spawn(create(&tree, "child", None), Some(id_of(&lead)))
        .await
        .unwrap();
    members(&runtime, &[&child]).await;
    assert!(preview(&runtime, &lead).await.removes_workspace);
    assert!(!preview(&runtime, &child).await.removes_workspace);

    // An agent outside the family keeps it.
    let other = runtime
        .spawn(create(&tree, "other", None), None)
        .await
        .unwrap();
    members(&runtime, &[&other]).await;
    let shared = preview(&runtime, &lead).await;
    assert!(!shared.removes_workspace, "{shared:?}");
    assert_eq!(
        shared.own_commits,
        Some(1),
        "what the folder holds is said all the same"
    );

    // A plain folder is no workspace this host made.
    let plain = runtime
        .spawn(create(&repo, "plain", None), None)
        .await
        .unwrap();
    members(&runtime, &[&plain]).await;
    let plain = preview(&runtime, &plain).await;
    assert!(!plain.removes_workspace);
    assert_eq!(plain.branch.as_deref(), Some("feature"));

    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// Without the experiment a delete leaves a new worktree, so the preview
/// says it stays.
#[tokio::test(flavor = "multi_thread")]
async fn without_the_experiment_no_workspace_goes() {
    let install = Install::new();
    let repo = shop(&install);
    let daemon = install.start("boot-1", launch(&install, false)).await;
    let runtime = runtime(&daemon, &install);
    let agent = runtime
        .spawn(
            wire::CreateAgentRequest {
                new_workspace: true,
                ..create(&repo, "checkout-fix", None)
            },
            None,
        )
        .await
        .unwrap();
    let preview = preview(&runtime, &agent).await;
    assert!(!preview.removes_workspace, "{preview:?}");
    assert_eq!(preview.branch.as_deref(), Some("checkout-fix"));

    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// GetChanges as each entry serves it: a person is answered on the
/// profile's socket and from a paired host; an agent on its tools socket,
/// or carried from a paired host, only about its own children.
#[tokio::test(flavor = "multi_thread")]
async fn every_entry_answers_a_person_and_an_agent_only_about_its_children() {
    use node::harness::{Calling, Served, served_as};
    use store::AgentKey;
    use wire::GetChangesRequest;

    let install = Install::new();
    feature_branch(&install.work);
    let here = host(&install);
    let paired = uuid::Uuid::new_v4();
    let remote = uuid::Uuid::new_v4();
    let work = install.work.clone();
    let lead = exited(&install, &work, Some("main"));
    let coder = exited_child(&install, &work, Some("main"), Some(lead.key(&install)));
    // A child an agent of the paired host made here.
    let made_from_afar = AgentKey::new(paired.as_bytes().to_vec(), remote.as_bytes().to_vec());
    let helper = exited_child(&install, &work, Some("main"), Some(made_from_afar));
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);

    let ask = |served, calling, agent: &SyntheticAgent| {
        let mut client = wire::review_service_client(served_as(&runtime, served, calling));
        let request = GetChangesRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            base: None,
        };
        async move {
            client
                .get_changes(request)
                .await
                .map(drop)
                .map_err(|refused| refused.code())
        }
    };
    let agent = |host, agent: &SyntheticAgent| Calling::Agent {
        host,
        agent: agent.id,
        name: agent.name.clone(),
    };
    let denied = Err(tonic::Code::PermissionDenied);
    for target in [&lead, &coder, &helper] {
        let person = Calling::PersonHere(analytics::Client::Terminal);
        assert_eq!(ask(Served::Profile, person, target).await, Ok(()));
        let person = Calling::PersonAt(paired, analytics::Client::Ios);
        assert_eq!(ask(Served::Link, person, target).await, Ok(()));
    }
    let tools = Served::Tools(lead.id);
    assert_eq!(ask(tools, agent(here, &lead), &coder).await, Ok(()));
    assert_eq!(ask(tools, agent(here, &lead), &lead).await, denied);
    assert_eq!(ask(tools, agent(here, &lead), &helper).await, denied);
    let remote = Calling::Agent {
        host: paired,
        agent: remote,
        name: "remote".into(),
    };
    assert_eq!(ask(Served::Link, remote.clone(), &helper).await, Ok(()));
    assert_eq!(ask(Served::Link, remote, &coder).await, denied);

    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// A turn's end, its snapshot carrying no git facts.
fn turn_ended() -> wire::Step {
    let mut step = snapshot(wire::Phase::Idle, &[], 4_000);
    step.turn_end = Some(wire::TurnEnd {
        turn_id: 1,
        outcome: wire::TurnOutcome::Completed as i32,
        ..wire::TurnEnd::default()
    });
    step
}

/// Agents sharing a checkout all count it against HEAD, so one agent's turn
/// end, which may have committed or reverted, has every other live agent
/// whose folder is the same, or one inside the other, read its facts again.
/// An agent in another folder is left alone, and so is the one whose turn
/// ended: it reads at its own turn end.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_end_has_the_others_in_its_checkout_read_their_facts_again() {
    let install = Install::new();
    git(&install.work, &["init", "-q", "-b", "main"]);
    let inside = install.work.join("crates");
    std::fs::create_dir_all(&inside).unwrap();
    let elsewhere = install.path("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut ending = SyntheticAgent::new(&install, "ending", SEGMENTS);
    let mut beside = SyntheticAgent::new(&install, "beside", SEGMENTS);
    let mut nested = SyntheticAgent::new(&install, "nested", SEGMENTS).in_folder(inside);
    let mut apart = SyntheticAgent::new(&install, "apart", SEGMENTS).in_folder(elsewhere);
    let agents = [&mut ending, &mut beside, &mut nested, &mut apart];
    for agent in agents {
        agent.register(&install);
        agent.go_live();
    }
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);
    // An idle snapshot from each, ingested only after its Hello on the same
    // connection: once every row shows it, the daemon knows each is live.
    for agent in [&mut ending, &mut beside, &mut nested, &mut apart] {
        agent.append(&snapshot(wire::Phase::Idle, &[], 2_000));
        agent.nudge().await;
    }
    for agent in [&ending, &beside, &nested, &apart] {
        until("each agent's idle snapshot", || async {
            let phase = runtime.agent(agent.id).await.unwrap().phase();
            (phase == wire::Phase::Idle)
                .then_some(())
                .ok_or_else(|| format!("{phase:?}"))
        })
        .await
        .unwrap();
    }
    assert_eq!(beside.git_reads_asked(), 0, "nothing has ended yet");

    ending.append(&turn_ended());
    ending.nudge().await;
    until("the agents in the checkout to be asked", || async {
        let asked = (beside.git_reads_asked(), nested.git_reads_asked());
        (asked == (1, 1))
            .then_some(())
            .ok_or_else(|| format!("{asked:?}"))
    })
    .await
    .unwrap();
    holds_for(
        "the agent elsewhere and the one that ended to be left alone",
        Duration::from_millis(300),
        || async { apart.git_reads_asked() == 0 && ending.git_reads_asked() == 0 },
    )
    .await
    .unwrap();

    for agent in [&mut ending, &mut beside, &mut nested, &mut apart] {
        agent.die().await;
    }
    drop(runtime);
    daemon.shutdown().await.unwrap();
}
