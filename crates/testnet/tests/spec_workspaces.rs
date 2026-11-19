//! Starting an agent in a new workspace from another host, under the
//! `workspaces/1` experiment and without it, across real daemons on a
//! loopback link with real agents on the fakes. The desk holds a
//! repository; the laptop asks for agents there. With the experiment the
//! desk answers with the row at its final folder and makes the workspace
//! after: a wt tree where wt is configured for the repository, a git
//! worktree otherwise, and the laptop's replica carries the workspace.
//! Without it a new workspace is the plain worktree it always was.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use node::Launch;
use prost::Message as _;
use testnet::{HostDecl, Net, NetOptions, PATIENCE, Topology, until};
use wire::amux::staging;
use wire::experimental::{AgentWorkspace, WorkspaceKind};
use wire::inventory_event;

/// A `.wt.toml` with an environment and a start hook that records itself
/// in the tree.
const WT_CONFIG: &str = r#"
[env]
SITE_MODE = "tree-{{name}}"

[lifecycle]
start = "echo started >> .wt/ran"
"#;

/// The desk, holding the repository `site` (configured for wt when `wt`),
/// and the laptop, linked.
fn topology(wt: bool) -> Topology {
    let mut files = std::collections::BTreeMap::from([(
        "site/README.md".to_owned(),
        "# The site\n".to_owned(),
    )]);
    if wt {
        files.insert("site/.wt.toml".to_owned(), WT_CONFIG.to_owned());
    }
    Topology::new()
        .host_decl(HostDecl {
            name: "desk".to_owned(),
            repositories: vec!["site".to_owned()],
            files,
            ..HostDecl::default()
        })
        .host("laptop")
        .link("desk", "laptop")
}

/// Every host runs the experiment when `on`, and each has a wt home of
/// its own under `homes`.
fn options(on: bool, homes: PathBuf) -> NetOptions {
    NetOptions {
        launch: Some(Arc::new(move |host: &str, launch: &mut Launch| {
            if on {
                launch.experiments =
                    settings::experiments::Enabled::of(&[&settings::experiments::WORKSPACES]);
            }
            launch.wt_home = Some(homes.join(host));
        })),
        ..NetOptions::default()
    }
}

/// The laptop asks for an agent named `name` in a new workspace of the
/// desk's repository.
async fn create(net: &Net, name: &str) -> wire::Agent {
    let desk = net.host("desk").unwrap();
    let site = desk.work.with_file_name("repositories").join("site");
    let laptop = net.person("laptop").await.unwrap();
    let desk = laptop.inventory().resolve_host("desk").await.unwrap();
    laptop
        .host(desk.host_id())
        .agents()
        .create(staging::CreateAgentRequest {
            agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
            name: Some(name.to_owned()),
            cwd: site.to_string_lossy().into_owned(),
            agent: Some(staging::create_agent_request::Agent::ClaudeSdk(
                Default::default(),
            )),
            new_workspace: true,
            ..staging::CreateAgentRequest::default()
        })
        .await
        .unwrap()
}

fn entry(agent: &wire::Agent) -> Option<AgentWorkspace> {
    wire::experimental::WORKSPACE.get(&agent.experimental)
}

/// The agent's row as `host`'s inventory lists it, once `done` holds for
/// it.
async fn row_on(
    net: &Net,
    host: &str,
    agent: &wire::Agent,
    what: &str,
    done: impl Fn(&wire::Agent) -> bool,
) -> wire::Agent {
    let runtime = net.runtime(host).unwrap();
    until(what, || async {
        let mut inventory = runtime
            .subscribe_inventory()
            .await
            .map_err(|error| error.to_string())?;
        let mut row = None;
        loop {
            let event = tokio::time::timeout(PATIENCE, inventory.next())
                .await
                .map_err(|_| "the inventory did not catch up".to_owned())?
                .ok_or_else(|| "the inventory ended".to_owned())?;
            match &event.of {
                Some(inventory_event::Of::Agent(listed)) if listed.agent_id == agent.agent_id => {
                    row = Some(listed.clone());
                }
                Some(inventory_event::Of::CaughtUp(_)) => break,
                _ => {}
            }
        }
        let row = row.ok_or_else(|| "not listed yet".to_owned())?;
        if done(&row) {
            Ok(row)
        } else {
            Err(format!("{:?}", entry(&row)))
        }
    })
    .await
    .unwrap()
}

fn made(entry: Option<AgentWorkspace>) -> bool {
    entry.is_some_and(|entry| !entry.creating)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_worktree_from_another_host_is_answered_first_and_replicated_with_its_workspace() {
    let homes = tempfile::tempdir().unwrap();
    let net = Net::start_with(topology(false), options(true, homes.path().to_owned()))
        .await
        .unwrap();

    let agent = create(&net, "fix-header").await;
    let answered = entry(&agent).expect("the answer carries the workspace");
    assert_eq!(answered.kind(), WorkspaceKind::Git);
    assert!(answered.made_by_amux);
    let worktree = net
        .host("desk")
        .unwrap()
        .data_dir
        .join(node::WORKTREES)
        .join("site")
        .join("fix-header");
    let on_desk = row_on(&net, "desk", &agent, "the worktree made", |row| {
        made(entry(row))
    })
    .await;
    assert_eq!(
        Path::new(&agent.cwd),
        worktree.canonicalize().unwrap(),
        "answered at its final folder"
    );
    assert!(worktree.join("README.md").is_file());
    let on_laptop = row_on(&net, "laptop", &agent, "the replica's workspace", |row| {
        made(entry(row))
    })
    .await;
    assert_eq!(entry(&on_laptop), entry(&on_desk));
    println!("{} in {}: {:?}", agent.name, agent.cwd, entry(&on_laptop));

    net.shutdown().await.unwrap();
}

/// The desk runs the experiment and the laptop does not: the laptop still
/// keeps and serves the workspace the desk's row says, so a client of the
/// laptop warns that deleting the agent takes its worktree.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_without_the_experiment_keeps_a_replicas_workspace_for_its_clients() {
    let homes = tempfile::tempdir().unwrap();
    let homes = homes.path().to_owned();
    let options = NetOptions {
        launch: Some(Arc::new(move |host: &str, launch: &mut Launch| {
            if host == "desk" {
                launch.experiments =
                    settings::experiments::Enabled::of(&[&settings::experiments::WORKSPACES]);
            }
            launch.wt_home = Some(homes.join(host));
        })),
        ..NetOptions::default()
    };
    let net = Net::start_with(topology(false), options).await.unwrap();
    assert!(
        !net.runtime("laptop")
            .unwrap()
            .launch()
            .experiments
            .is_on(&settings::experiments::WORKSPACES)
    );

    let agent = create(&net, "fix-sidebar").await;
    let on_desk = row_on(&net, "desk", &agent, "the worktree made", |row| {
        made(entry(row))
    })
    .await;
    let on_laptop = row_on(&net, "laptop", &agent, "the replica's workspace", |row| {
        made(entry(row))
    })
    .await;
    assert_eq!(on_laptop.experimental, on_desk.experimental);
    assert_eq!(entry(&on_laptop).unwrap().kind(), WorkspaceKind::Git);

    let mut fleet = ui_state::FleetState::new();
    fleet.update(ui_state::FleetMsg::Event(Box::new(wire::InventoryEvent {
        of: Some(inventory_event::Of::Agent(on_laptop.clone())),
    })));
    assert!(
        fleet
            .delete_removes_workspace(&ui_state::agent_key(&on_laptop))
            .is_some(),
        "the laptop's client warns that the worktree goes"
    );

    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_wt_tree_from_another_host_runs_its_agent_and_starts_the_tree() {
    let homes = tempfile::tempdir().unwrap();
    let net = Net::start_with(topology(true), options(true, homes.path().to_owned()))
        .await
        .unwrap();

    let agent = create(&net, "fix-footer").await;
    let answered = entry(&agent).expect("the answer carries the workspace");
    assert_eq!(answered.kind(), WorkspaceKind::Wt);
    let tree = homes
        .path()
        .canonicalize()
        .unwrap()
        .join("desk")
        .join("trees")
        .join("site")
        .join("fix-footer");
    assert_eq!(Path::new(&agent.cwd), tree);
    let on_laptop = row_on(&net, "laptop", &agent, "the tree made", |row| {
        made(entry(row))
    })
    .await;
    assert_eq!(entry(&on_laptop).unwrap().kind(), WorkspaceKind::Wt);
    until("the start hook to run", || async {
        let ran = std::fs::read_to_string(tree.join(".wt/ran")).unwrap_or_default();
        if ran.contains("started") {
            Ok(())
        } else {
            Err(ran)
        }
    })
    .await
    .unwrap();
    let desk = net.host("desk").unwrap();
    let spec = std::fs::read(
        node::profile_dir(&desk.data_dir, desk.profile)
            .join(node::AGENTS)
            .join(uuid::Uuid::from_slice(&agent.agent_id).unwrap().to_string())
            .join("spec.1"),
    )
    .unwrap();
    let spec = wire::AgentSpec::decode(spec.as_slice()).unwrap();
    assert_eq!(spec.provider_env["SITE_MODE"], "tree-fix-footer");
    println!("{} in {}", agent.name, agent.cwd);

    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_experiment_a_new_workspace_from_another_host_is_a_plain_worktree() {
    let homes = tempfile::tempdir().unwrap();
    let net = Net::start_with(topology(true), options(false, homes.path().to_owned()))
        .await
        .unwrap();

    let agent = create(&net, "fix-nav").await;
    assert_eq!(entry(&agent), None);
    let worktree = net
        .host("desk")
        .unwrap()
        .data_dir
        .join(node::WORKTREES)
        .join("site")
        .join("fix-nav");
    assert_eq!(
        Path::new(&agent.cwd).canonicalize().unwrap(),
        worktree.canonicalize().unwrap()
    );
    assert!(!homes.path().join("desk").join("trees").exists());
    let on_laptop = row_on(&net, "laptop", &agent, "the replica", |_| true).await;
    assert_eq!(entry(&on_laptop), None);

    net.shutdown().await.unwrap();
}

/// The laptop deletes an agent on the desk whose worktree has work nobody
/// committed, saying `discard`.
async fn delete_from_laptop(net: &Net, agent: &wire::Agent, discard: bool) {
    net.person("laptop")
        .await
        .unwrap()
        .agent(agent)
        .delete(discard)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delete_from_another_host_discards_a_worktrees_changes_only_when_it_says_so() {
    let homes = tempfile::tempdir().unwrap();
    let net = Net::start_with(topology(false), options(true, homes.path().to_owned()))
        .await
        .unwrap();
    let mut agents = Vec::new();
    for name in ["keep-draft", "lose-draft"] {
        let agent = create(&net, name).await;
        row_on(&net, "desk", &agent, "the worktree made", |row| {
            made(entry(row))
        })
        .await;
        std::fs::write(Path::new(&agent.cwd).join("draft.txt"), "draft\n").unwrap();
        agents.push(agent);
    }
    let made = agents;

    delete_from_laptop(&net, &made[0], false).await;
    delete_from_laptop(&net, &made[1], true).await;
    until("the discarded worktree to go", || async {
        if Path::new(&made[1].cwd).exists() {
            Err("still there")
        } else {
            Ok(())
        }
    })
    .await
    .unwrap();
    assert!(
        Path::new(&made[0].cwd).join("draft.txt").is_file(),
        "a delete that did not say so keeps the work"
    );
    let site = net
        .host("desk")
        .unwrap()
        .work
        .with_file_name("repositories")
        .join("site");
    let marker = site.join(".git/worktrees/keep-draft/amux-owner");
    until("the kept worktree to be no longer amux's", || async {
        if marker.exists() {
            Err("still marked")
        } else {
            Ok(())
        }
    })
    .await
    .unwrap();

    net.shutdown().await.unwrap();
}
