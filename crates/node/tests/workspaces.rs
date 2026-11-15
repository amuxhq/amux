//! Starting agents in new workspaces under the `workspaces/1` experiment,
//! against real repositories and wt homes in temporary folders: the row is
//! the answer and the workspace is made after it, a git worktree carries
//! this host's marker and a wt tree its owner, a wt tree's environment
//! reaches the provider and its hooks run with its agents, each workspace
//! goes with its last agent, and the experiment's entry rides on the rows
//! to hosts that run it and nowhere else. Without the experiment a new
//! workspace is a plain worktree, as before.

use std::path::{Path, PathBuf};
use std::process::Command;

use prost::Message as _;
use support::synthetic::read_inventory_until;
use support::*;
use wire::experimental::{AgentWorkspace, WorkspaceKind};
use wire::{AgentSpec, CreateAgentRequest, inventory_event};

use crate::support;

const VERSION: &str = "workspaces/1";

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=amux",
            "-c",
            "user.email=amux@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.fsmonitor=false",
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

/// A canonical folder under the installation's scratch directory.
fn place(install: &Install, name: &str) -> PathBuf {
    let path = install.path(name);
    std::fs::create_dir_all(&path).unwrap();
    path.canonicalize().unwrap()
}

/// A repository named `shop` with `feature` checked out, one commit past
/// `main`.
fn repository(install: &Install) -> PathBuf {
    let repo = place(install, "shop");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "start"]);
    git(&repo, &["switch", "-q", "-c", "feature"]);
    std::fs::write(repo.join("feature.txt"), "on feature\n").unwrap();
    git(&repo, &["add", "feature.txt"]);
    git(&repo, &["commit", "-q", "-m", "feature"]);
    repo
}

/// The `.wt.toml` a wt repository commits: an environment, and hooks that
/// record each run in the tree.
const WT_CONFIG: &str = r#"
[env]
SHOP_MODE = "tree-{{name}}"

[lifecycle]
create = "echo created >> .wt/ran"
start = "echo started >> .wt/ran"
stop = "echo stopped >> .wt/ran"
"#;

/// A repository named `shop` on `main`, configured for wt, with a bare
/// `origin` whose default branch is `main`.
fn wt_repository(install: &Install) -> PathBuf {
    let repo = place(install, "shop");
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".wt.toml"), WT_CONFIG).unwrap();
    git(&repo, &["add", ".wt.toml"]);
    git(&repo, &["commit", "-q", "-m", "wt"]);
    let origin = install.path("origin.git");
    git(
        &install.path(""),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(
        &repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    git(&repo, &["push", "-q", "-u", "origin", "main"]);
    git(&repo, &["remote", "set-head", "origin", "main"]);
    repo
}

/// What an agent launches with: the experiment on or off, and wt's home in
/// the installation's scratch folder.
fn launch(install: &Install, on: bool) -> node::Launch {
    let mut launch = install.launch("idle", vec![]);
    if on {
        launch.experiments =
            ::settings::experiments::Enabled::of(&[&::settings::experiments::WORKSPACES]);
    }
    launch.wt_home = Some(place(install, "wt-home"));
    launch
}

fn in_new_workspace(cwd: &Path, name: &str) -> CreateAgentRequest {
    CreateAgentRequest {
        new_workspace: true,
        ..create(cwd, name, None)
    }
}

fn entry(agent: &wire::Agent) -> Option<AgentWorkspace> {
    agent
        .experimental
        .get(VERSION)
        .map(|bytes| AgentWorkspace::decode(bytes.as_slice()).unwrap())
}

/// The agent's first spec. The daemon writes it as it launches the
/// process, which may be just after the agent is listed live.
fn spec(install: &Install, agent: &wire::Agent) -> AgentSpec {
    let path = install.agent_dir(id_of(agent)).join("spec.1");
    let deadline = std::time::Instant::now() + PATIENCE;
    let bytes = loop {
        match std::fs::read(&path) {
            Ok(bytes) => break bytes,
            Err(error) if std::time::Instant::now() >= deadline => {
                panic!("{} was never written: {error}", path.display())
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };
    AgentSpec::decode(bytes.as_slice()).unwrap()
}

fn branches(repo: &Path) -> Vec<String> {
    git(
        repo,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

fn canonical(path: &str) -> PathBuf {
    Path::new(path).canonicalize().unwrap()
}

/// Waits for the agent's process to be running.
async fn running(runtime: &node::ProfileRuntime, agent: &wire::Agent) {
    let id = id_of(agent);
    until("the agent to be running", || async {
        if runtime.live().contains(&id) {
            Ok(())
        } else {
            Err("not yet")
        }
    })
    .await
    .unwrap();
}

/// Waits for the agent's row here to satisfy `done`, and returns it.
async fn row_until(
    runtime: &node::ProfileRuntime,
    agent: &wire::Agent,
    what: &str,
    done: impl Fn(&wire::Agent) -> bool,
) -> wire::Agent {
    until(what, || async {
        let row = runtime
            .agent(id_of(agent))
            .await
            .map_err(|error| error.to_string())?;
        if done(&row) {
            Ok(row)
        } else {
            Err(format!("{:?}", entry(&row)))
        }
    })
    .await
    .unwrap()
}

/// Waits for `path` to be gone.
async fn gone(path: &Path) {
    until("the workspace to be removed", || async {
        if path.exists() {
            Err(format!("{} is still there", path.display()))
        } else {
            Ok(())
        }
    })
    .await
    .unwrap();
}

/// What the tree's hooks have recorded.
#[cfg(unix)]
fn ran(tree: &Path) -> String {
    std::fs::read_to_string(tree.join(".wt/ran")).unwrap_or_default()
}

/// Waits for the tree's hooks to have recorded `what` `times` times.
#[cfg(unix)]
async fn hook_ran(tree: &Path, what: &str, times: usize) {
    until(&format!("{what} to run {times} times"), || async {
        let ran = ran(tree);
        if ran.lines().filter(|line| *line == what).count() >= times {
            Ok(())
        } else {
            Err(ran)
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_worktree_is_answered_first_marked_as_this_hosts_and_goes_with_its_agent() {
    let install = Install::new();
    let repo = repository(&install);
    let daemon = install.start("boot-1", launch(&install, true)).await;
    let runtime = runtime(&daemon, &install);

    let agent = runtime
        .spawn(in_new_workspace(&repo, "checkout-fix"), None)
        .await
        .unwrap();
    let expected = install
        .data_dir
        .join(node::WORKTREES)
        .join("shop")
        .join("checkout-fix");
    // The answer is the row, already at its final folder.
    let answered = entry(&agent).expect("the row carries its workspace");
    assert_eq!(answered.kind(), WorkspaceKind::Git);
    assert!(answered.made_by_amux);
    assert_eq!(Path::new(&agent.cwd), Path::new(&answered.id));
    running(&runtime, &agent).await;
    let made = row_until(&runtime, &agent, "the worktree to be made", |row| {
        entry(row).is_some_and(|entry| !entry.creating)
    })
    .await;
    assert_eq!(canonical(&made.cwd), expected.canonicalize().unwrap());
    assert!(expected.join("feature.txt").is_file());
    assert_eq!(
        git(&expected, &["branch", "--show-current"]),
        "checkout-fix"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join(".git/worktrees/checkout-fix/amux-owner")).unwrap(),
        runtime.host().to_string(),
        "the worktree says which host made it"
    );
    assert_eq!(
        spec(&install, &agent).base_branch.as_deref(),
        Some("feature")
    );

    // A name the repository's branches already have gets the next free
    // suffix, branch and folder alike.
    let clash = runtime
        .spawn(in_new_workspace(&repo, "main"), None)
        .await
        .unwrap();
    assert_eq!(clash.name, "main-2");
    running(&runtime, &clash).await;
    row_until(&runtime, &clash, "the second worktree", |row| {
        entry(row).is_some_and(|entry| !entry.creating)
    })
    .await;
    assert!(branches(&repo).contains(&"main-2".to_owned()));
    println!(
        "{} in {}, {} in {}",
        agent.name, agent.cwd, clash.name, clash.cwd
    );

    // Its last agent deleted, a clean worktree goes, and its branch with
    // it, since nothing was committed on it.
    kill_all(&runtime).await;
    runtime.delete(id_of(&agent)).await.unwrap();
    gone(&expected).await;
    until("the empty branch to go", || async {
        if branches(&repo).contains(&"checkout-fix".to_owned()) {
            Err("still there")
        } else {
            Ok(())
        }
    })
    .await
    .unwrap();
    assert!(Path::new(&clash.cwd).is_dir(), "the other agent's stays");

    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dirty_worktree_stays_nobodys_without_consent_and_goes_with_it() {
    let install = Install::new();
    let repo = repository(&install);
    let daemon = install.start("boot-1", launch(&install, true)).await;
    let runtime = runtime(&daemon, &install);
    let made = |name: &'static str| {
        let runtime = runtime.clone();
        let repo = repo.clone();
        async move {
            let agent = runtime
                .spawn(in_new_workspace(&repo, name), None)
                .await
                .unwrap();
            running(&runtime, &agent).await;
            row_until(&runtime, &agent, "the worktree made", |row| {
                entry(row).is_some_and(|entry| !entry.creating)
            })
            .await;
            // Work nobody committed.
            std::fs::write(Path::new(&agent.cwd).join("draft.txt"), "draft\n").unwrap();
            agent
        }
    };

    // A delete that says nothing of the changes, as every client before
    // the warning sends: the worktree stays, with its branch, and is no
    // longer amux's.
    let kept = made("keep-draft").await;
    runtime.kill_agent(id_of(&kept)).await.unwrap();
    runtime.delete(id_of(&kept)).await.unwrap();
    let marker = repo.join(".git/worktrees/keep-draft/amux-owner");
    until("the worktree to be disowned", || async {
        if marker.exists() {
            Err("still marked")
        } else {
            Ok(())
        }
    })
    .await
    .unwrap();
    assert!(
        Path::new(&kept.cwd).join("draft.txt").is_file(),
        "the draft stays"
    );
    assert!(branches(&repo).contains(&"keep-draft".to_owned()));

    // With consent the dirty worktree goes, and its empty branch with it.
    let lost = made("lose-draft").await;
    runtime.kill_agent(id_of(&lost)).await.unwrap();
    runtime.delete_discarding(id_of(&lost), true).await.unwrap();
    gone(Path::new(&lost.cwd)).await;
    until("the empty branch to go", || async {
        if branches(&repo).contains(&"lose-draft".to_owned()) {
            Err("still there")
        } else {
            Ok(())
        }
    })
    .await
    .unwrap();

    drop(runtime);
    daemon.shutdown().await.unwrap();
}

// wt and its backend are Unix-only.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_new_wt_tree_runs_its_agent_in_its_environment_and_its_hooks_with_its_agents() {
    let install = Install::new();
    let repo = wt_repository(&install);
    let wt_home = place(&install, "wt-home");
    let daemon = install.start("boot-1", launch(&install, true)).await;
    let runtime = runtime(&daemon, &install);

    let agent = runtime
        .spawn(in_new_workspace(&repo, "fix-scroll"), None)
        .await
        .unwrap();
    let tree = wt_home.join("trees").join("shop").join("fix-scroll");
    let answered = entry(&agent).expect("the row carries its workspace");
    assert_eq!(answered.kind(), WorkspaceKind::Wt);
    assert!(answered.made_by_amux);
    assert_eq!(Path::new(&answered.id), tree);
    assert_eq!(Path::new(&agent.cwd), tree);
    running(&runtime, &agent).await;
    assert_eq!(
        std::fs::read_to_string(tree.join(".wt/owner")).unwrap(),
        format!("amux:{}", runtime.host())
    );
    // From origin's default branch, since the creating folder was on it.
    assert_eq!(git(&tree, &["branch", "--show-current"]), "fix-scroll");
    assert_eq!(spec(&install, &agent).base_branch.as_deref(), Some("main"));

    // The provider runs in the tree's environment.
    let env = spec(&install, &agent).provider_env;
    assert_eq!(env["SHOP_MODE"], "tree-fix-scroll");
    assert_eq!(env["WT_TREE"], "shop/fix-scroll");
    let bin = tree.join(".wt/bin").to_string_lossy().into_owned();
    assert!(
        env["PATH"].starts_with(&format!("{bin}:")),
        "{}",
        env["PATH"]
    );
    hook_ran(&tree, "created", 1).await;
    hook_ran(&tree, "started", 1).await;
    assert!(!ran(&tree).contains("stopped"));

    // A restart takes its claim again and starts the tree again, the agent
    // still running.
    drop(runtime);
    daemon.shutdown().await.unwrap();
    let daemon = install.start("boot-2", launch(&install, true)).await;
    let runtime = support::runtime(&daemon, &install);
    running(&runtime, &agent).await;
    hook_ran(&tree, "started", 2).await;
    assert!(!ran(&tree).contains("stopped"), "{}", ran(&tree));

    // Its agent stopped, nothing holds the tree and it stops.
    kill_all(&runtime).await;
    hook_ran(&tree, "stopped", 1).await;
    println!("{} in {}: {:?}", agent.name, agent.cwd, ran(&tree));

    // Deleted, its clean tree goes.
    runtime.delete(id_of(&agent)).await.unwrap();
    gone(&tree).await;

    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_experiment_a_new_workspace_is_a_plain_worktree() {
    let install = Install::new();
    let repo = wt_repository(&install);
    let wt_home = place(&install, "wt-home");
    let daemon = install.start("boot-1", launch(&install, false)).await;
    let runtime = runtime(&daemon, &install);

    let agent = runtime
        .spawn(in_new_workspace(&repo, "fix-scroll"), None)
        .await
        .unwrap();
    assert!(entry(&agent).is_none());
    let expected = install
        .data_dir
        .join(node::WORKTREES)
        .join("shop")
        .join("fix-scroll");
    assert_eq!(canonical(&agent.cwd), expected.canonicalize().unwrap());
    assert!(
        !wt_home.join("trees").exists(),
        "wt made nothing without the experiment"
    );
    assert!(
        !repo.join(".git/worktrees/fix-scroll/amux-owner").exists(),
        "nor is the worktree marked"
    );
    running(&runtime, &agent).await;
    assert!(
        !spec(&install, &agent)
            .provider_env
            .contains_key("SHOP_MODE")
    );

    // As before, a delete leaves the worktree.
    kill_all(&runtime).await;
    runtime.delete(id_of(&agent)).await.unwrap();
    assert!(expected.is_dir());

    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// One host of three, each paired with the others.
struct Host {
    install: Install,
    daemon: node::Daemon,
    runtime: std::sync::Arc<node::ProfileRuntime>,
    edge: std::sync::Arc<node::Edge>,
}

impl Host {
    async fn start(on: bool) -> Self {
        let install = Install::new();
        let mut launch = install.launch("idle", vec![]);
        if on {
            launch.experiments =
                ::settings::experiments::Enabled::of(&[&::settings::experiments::WORKSPACES]);
        }
        launch.wt_home = Some(place(&install, "wt-home"));
        let daemon = install.start("boot-1", launch).await;
        let runtime = runtime(&daemon, &install);
        let edge = runtime.edge().unwrap();
        Self {
            install,
            daemon,
            runtime,
            edge,
        }
    }

    /// The agent's row as this host's inventory has it, once it is there.
    async fn sees(&self, origin: &Host, agent: &wire::Agent) -> wire::Agent {
        let host = origin.runtime.host().as_bytes().to_vec();
        until("the row to replicate", || async {
            let mut inventory = self
                .runtime
                .subscribe_inventory()
                .await
                .map_err(|error| error.to_string())?;
            let mut seen = Vec::new();
            read_inventory_until(&mut inventory, &mut seen, "CaughtUp", |seen| {
                seen.last()
                    .is_some_and(|event| matches!(event.of, Some(inventory_event::Of::CaughtUp(_))))
            })
            .await;
            seen.iter()
                .rev()
                .find_map(|event| match &event.of {
                    Some(inventory_event::Of::Agent(row))
                        if row.agent_id == agent.agent_id && row.host_id == host =>
                    {
                        Some(row.clone())
                    }
                    _ => None,
                })
                .ok_or_else(|| "not yet".to_owned())
        })
        .await
        .unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_experiments_entry_reaches_hosts_that_run_it_and_others_pass_rows_on_without_it() {
    let a = Host::start(true).await;
    let b = Host::start(false).await;
    let c = Host::start(true).await;
    // An agent on B, which does not run the experiment.
    let b_folder = place(&b.install, "notes");
    let on_b_own = b
        .runtime
        .spawn(create(&b_folder, "reader", None), None)
        .await
        .unwrap();
    assert_eq!(entry(&on_b_own), None);
    let mut links = Vec::new();
    for (one, other) in [(&b, &c), (&a, &b), (&a, &c)] {
        one.edge.trust(&other.edge).await.unwrap();
        other.edge.trust(&one.edge).await.unwrap();
        links.push(one.edge.link_in_process(&other.edge).unwrap());
    }
    for (one, other) in [(&a, &b), (&a, &c), (&b, &c), (&b, &a), (&c, &a), (&c, &b)] {
        assert!(wait_for_route(&one.edge, other.runtime.host(), PATIENCE).await);
    }

    // An agent on A in a folder of its own: its entry is a folder.
    let folder = place(&a.install, "notes");
    let on_a = a
        .runtime
        .spawn(create(&folder, "scribe", None), None)
        .await
        .unwrap();
    let at_origin = row_until(&a.runtime, &on_a, "A's entry", |row| entry(row).is_some()).await;
    let expected = entry(&at_origin).unwrap();
    assert_eq!(expected.kind(), WorkspaceKind::Folder);
    assert_eq!(Path::new(&expected.id), folder);

    let on_c = c.sees(&a, &on_a).await;
    assert_eq!(
        entry(&on_c),
        Some(expected.clone()),
        "C runs it and keeps it"
    );
    let on_b = b.sees(&a, &on_a).await;
    assert_eq!(entry(&on_b), None, "B does not run it and keeps nothing");
    assert_eq!(on_b.name, on_a.name, "but has the row");

    // B's own agents carry no entry, to anyone.
    assert_eq!(entry(&a.sees(&b, &on_b_own).await), None);
    assert_eq!(entry(&c.sees(&b, &on_b_own).await), None);

    println!("A's entry on C: {expected:?}");

    drop(links);
    let mut installs = Vec::new();
    let mut daemons = Vec::new();
    for host in [a, b, c] {
        kill_all(&host.runtime).await;
        drop((host.runtime, host.edge));
        daemons.push(host.daemon);
        installs.push(host.install);
    }
    let [a, b, c] = <[_; 3]>::try_from(daemons).ok().unwrap();
    let (a, b, c) = tokio::join!(a.shutdown(), b.shutdown(), c.shutdown());
    a.unwrap();
    b.unwrap();
    c.unwrap();
    drop(installs);
}
