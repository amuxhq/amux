//! Starting an agent in a new worktree. The create request only says yes;
//! the agent's host makes the worktree under its own folder, by repository
//! then agent name, on a branch named after the agent from what the chosen
//! folder has checked out, and the agent's git facts measure it from there.
//! A folder that is not a repository, or a name a branch already has, fails
//! the create with nothing made. Nothing ever removes the worktree or its
//! branch.

use std::path::{Path, PathBuf};
use std::process::Command;

use prost::Message as _;
use support::synthetic::{quiet_launch, read_until};
use support::*;
use wire::agent_service_server::AgentService as _;
use wire::{AgentSpec, CreateAgentRequest, SessionEvent, session_event};

use crate::support;

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=amux",
            "-c",
            "user.email=amux@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A repository named `shop` with `feature` checked out, one commit past
/// `main`.
fn repository(install: &Install) -> PathBuf {
    let repo = install.path("shop");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "start"]);
    git(&repo, &["switch", "-q", "-c", "feature"]);
    std::fs::write(repo.join("feature.txt"), "on feature\n").unwrap();
    git(&repo, &["add", "feature.txt"]);
    git(&repo, &["commit", "-q", "-m", "feature"]);
    repo
}

fn in_worktree(cwd: &Path, name: Option<&str>) -> CreateAgentRequest {
    CreateAgentRequest {
        name: name.map(str::to_owned),
        new_worktree: true,
        ..create(cwd, "", None)
    }
}

fn branches(repo: &Path) -> Vec<String> {
    let listed = git(
        repo,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    );
    listed.lines().map(str::to_owned).collect()
}

fn spec(install: &Install, agent: &wire::Agent) -> AgentSpec {
    let bytes = std::fs::read(install.agent_dir(id_of(agent)).join("spec.1")).unwrap();
    AgentSpec::decode(bytes.as_slice()).unwrap()
}

fn same_place(a: &Path, b: &Path) -> bool {
    a.canonicalize().unwrap() == b.canonicalize().unwrap()
}

/// The git facts the agent's snapshot reports once it has read them.
async fn reported_git(runtime: &node::ProfileRuntime, agent: &wire::Agent) -> wire::Git {
    let mut subscription = until("a subscription to the new agent", || {
        runtime.subscribe_session(&agent.agent_id, 10)
    })
    .await
    .unwrap();
    let git = |event: &SessionEvent| match &event.of {
        Some(session_event::Of::Snapshot(snapshot)) => snapshot.git.clone(),
        _ => None,
    };
    let mut seen = Vec::new();
    read_until(
        &mut subscription,
        &mut seen,
        "the agent's git facts",
        |seen| seen.iter().any(|event| git(event).is_some()),
    )
    .await;
    seen.iter().rev().find_map(git).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_starts_in_a_worktree_on_its_own_branch_from_what_was_checked_out() {
    let install = Install::new();
    let repo = repository(&install);
    let daemon = install
        .start("boot-1", install.launch("idle", vec![]))
        .await;
    let runtime = runtime(&daemon, &install);

    let agent = runtime
        .spawn(in_worktree(&repo, Some("checkout-fix")), None)
        .await
        .unwrap();
    let expected = install
        .data_dir
        .join(node::WORKTREES)
        .join("shop")
        .join("checkout-fix");
    assert!(
        same_place(Path::new(&agent.cwd), &expected),
        "{}",
        agent.cwd
    );
    assert!(expected.join("feature.txt").is_file());
    assert_eq!(
        git(&expected, &["branch", "--show-current"]),
        "checkout-fix"
    );
    assert_eq!(
        git(&repo, &["branch", "--show-current"]),
        "feature",
        "the chosen folder keeps its own checkout"
    );
    assert_eq!(
        spec(&install, &agent).base_branch.as_deref(),
        Some("feature")
    );
    let reported = reported_git(&runtime, &agent).await;
    assert_eq!(reported.branch.as_deref(), Some("checkout-fix"));
    assert_eq!(reported.base_branch.as_deref(), Some("feature"));
    println!(
        "{} in {} on {:?} from {:?}",
        agent.name, agent.cwd, reported.branch, reported.base_branch
    );

    // An agent created without a name gets a word pair, and its branch is
    // that pair.
    let unnamed = runtime.spawn(in_worktree(&repo, None), None).await.unwrap();
    assert!(node::word_pairs().any(|pair| pair == unnamed.name));
    assert!(branches(&repo).contains(&unnamed.name));
    assert!(same_place(
        Path::new(&unnamed.cwd),
        &install
            .data_dir
            .join(node::WORKTREES)
            .join("shop")
            .join(&unnamed.name)
    ));

    // A rename leaves the branch and the folder alone.
    let renamed = runtime
        .rename(id_of(&agent), "checkout-redesign")
        .await
        .unwrap();
    assert_eq!(renamed.name, "checkout-redesign");
    assert!(branches(&repo).contains(&"checkout-fix".to_owned()));
    assert!(!branches(&repo).contains(&"checkout-redesign".to_owned()));
    assert_eq!(runtime.agent(id_of(&agent)).await.unwrap().cwd, agent.cwd);

    // Stopping and deleting an agent leaves its worktree and branch.
    kill_all(&runtime).await;
    runtime.delete(id_of(&agent)).await.unwrap();
    assert!(expected.join("feature.txt").is_file());
    assert!(branches(&repo).contains(&"checkout-fix".to_owned()));

    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_outside_a_repository_or_a_taken_branch_fails_the_create() {
    let install = Install::new();
    let repo = repository(&install);
    let daemon = install
        .start("boot-1", install.launch("idle", vec![]))
        .await;
    let runtime = runtime(&daemon, &install);
    let worktrees = install.data_dir.join(node::WORKTREES);

    let outside = install.path("notes");
    std::fs::create_dir_all(&outside).unwrap();
    let refused = runtime
        .spawn(in_worktree(&outside, Some("tidy")), None)
        .await
        .unwrap_err();
    let wire = refused.to_wire();
    assert_eq!(wire.code, wire::ErrorCode::FailedPrecondition as i32);
    assert!(
        wire.message.contains("is not in a git repository"),
        "{}",
        wire.message
    );
    println!("not a repository: {}", wire.message);

    let refused = runtime
        .spawn(in_worktree(&repo, Some("main")), None)
        .await
        .unwrap_err();
    let wire = refused.to_wire();
    assert_eq!(wire.code, wire::ErrorCode::AlreadyExists as i32);
    assert_eq!(
        wire.message,
        "starting in a new worktree: the repository already has a branch named main"
    );
    println!("taken branch: {}", wire.message);

    assert!(
        runtime
            .read_store(|store| store.agents().unwrap().is_empty())
            .await
            .unwrap(),
        "no agent is made"
    );
    assert!(
        !worktrees.exists() || std::fs::read_dir(worktrees.join("shop")).unwrap().count() == 0,
        "no worktree is made"
    );
    assert_eq!(branches(&repo), vec!["feature", "main"]);

    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_created_from_another_host_gets_its_worktree_on_its_own_host() {
    let desk = Install::new();
    let repo = repository(&desk);
    let desk_daemon = desk.start("boot-1", desk.launch("idle", vec![])).await;
    let desk_runtime = runtime(&desk_daemon, &desk);
    let laptop = Install::new();
    let laptop_daemon = laptop.start("boot-1", quiet_launch()).await;
    let laptop_runtime = runtime(&laptop_daemon, &laptop);
    let (desk_edge, laptop_edge) = (desk_runtime.edge().unwrap(), laptop_runtime.edge().unwrap());
    desk_edge.trust(&laptop_edge).await.unwrap();
    laptop_edge.trust(&desk_edge).await.unwrap();
    let link = laptop_edge.link_in_process(&desk_edge).unwrap();
    assert!(wait_for_route(&laptop_edge, desk_runtime.host(), PATIENCE).await);
    let from_laptop = node::ClientApi::new(&laptop_runtime, None);
    let on_desk = |name: &str| CreateAgentRequest {
        host: Some(wire::HostRef {
            of: Some(wire::host_ref::Of::HostId(
                desk_runtime.host().as_bytes().to_vec(),
            )),
        }),
        ..in_worktree(&repo, Some(name))
    };

    let agent = from_laptop
        .create_agent(tonic::Request::new(on_desk("from-laptop")))
        .await
        .unwrap()
        .into_inner();
    let expected = desk
        .data_dir
        .join(node::WORKTREES)
        .join("shop")
        .join("from-laptop");
    assert!(
        same_place(Path::new(&agent.cwd), &expected),
        "{}",
        agent.cwd
    );
    assert!(!laptop.data_dir.join(node::WORKTREES).exists());
    assert_eq!(spec(&desk, &agent).base_branch.as_deref(), Some("feature"));
    let reported = reported_git(&desk_runtime, &agent).await;
    assert_eq!(reported.branch.as_deref(), Some("from-laptop"));
    assert_eq!(reported.base_branch.as_deref(), Some("feature"));

    // The desk's refusal reaches the laptop as the desk said it. A taken
    // agent name would only earn a suffix, so the clash is with a branch
    // the repository already has.
    let refused = from_laptop
        .create_agent(tonic::Request::new(on_desk("main")))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), tonic::Code::AlreadyExists, "{refused:?}");
    assert!(
        refused
            .message()
            .contains("the repository already has a branch named main"),
        "{refused:?}"
    );
    println!(
        "from the laptop: {} in {}; again: {}",
        agent.name,
        agent.cwd,
        refused.message()
    );

    drop(link);
    kill_all(&desk_runtime).await;
    drop((desk_runtime, laptop_runtime, desk_edge, laptop_edge));
    desk_daemon.shutdown().await.unwrap();
    laptop_daemon.shutdown().await.unwrap();
}
