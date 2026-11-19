//! A delete of an agent in a workspace its host made, as each kind of
//! caller asks for it: the workspace goes with its last agent, and its
//! uncommitted work goes only with the consent the delete carries.

use std::path::{Path, PathBuf};
use std::process::Command;

use support::*;
use wire::amux::staging;

use crate::host_acts::{EVERYONE, Host, Setup, create};
use crate::support;

fn git(cwd: &Path, args: &[&str]) {
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
}

/// A repository with one commit on `main`.
fn repository(install: &Install) -> PathBuf {
    let repo = install.path("shop");
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "start"]);
    repo
}

/// Waits for the agent's workspace to be made, and returns its folder.
async fn made(host: &Host, agent: uuid::Uuid) -> PathBuf {
    until("the workspace to be made", || async {
        let row = host
            .runtime
            .agent(agent)
            .await
            .map_err(|error| error.to_string())?;
        match wire::experimental::WORKSPACE.get(&row.experimental) {
            Some(entry) if !entry.creating && Path::new(&row.cwd).is_dir() => {
                Ok(PathBuf::from(row.cwd))
            }
            other => Err(format!("{other:?}")),
        }
    })
    .await
    .unwrap()
}

/// A delete removes the workspace its host made with its last agent. With
/// uncommitted work there, the workspace goes only when the delete carries
/// the person's consent; without it the work stays on disk, no longer
/// amux's. Every caller that may delete the agent is held to it alike.
#[tokio::test(flavor = "multi_thread")]
async fn a_delete_takes_uncommitted_work_only_with_consent() {
    let setup = Setup::new();
    let repo = repository(&setup.install);
    let mut launch = setup.install.launch("idle", vec![]);
    launch.experiments =
        ::settings::experiments::Enabled::of(&[&::settings::experiments::WORKSPACES]);
    let wt_home = setup.install.path("wt-home");
    std::fs::create_dir_all(&wt_home).unwrap();
    launch.wt_home = Some(wt_home);
    let host = setup
        .start_with(move |options| options.launch = launch)
        .await;

    for who in EVERYONE {
        for consent in [false, true] {
            let name = format!(
                "{}-{}",
                format!("{who:?}").to_lowercase(),
                if consent { "lose" } else { "keep" }
            );
            let agent = host
                .agents(who)
                .create_agent(staging::CreateAgentRequest {
                    new_workspace: true,
                    ..create(&repo, &name, None)
                })
                .await
                .unwrap_or_else(|status| panic!("{who:?}: {status:?}"))
                .into_inner()
                .agent
                .unwrap();
            let id = uuid::Uuid::from_slice(&agent.agent_id).unwrap();
            let tree = made(&host, id).await;
            std::fs::write(tree.join("draft.txt"), "work nobody committed\n").unwrap();
            host.runtime.kill_agent(id).await.unwrap();

            host.agents(who)
                .delete_agent(staging::DeleteAgentRequest {
                    agent_id: id.as_bytes().to_vec(),
                    discard_workspace_changes: consent,
                })
                .await
                .unwrap_or_else(|status| panic!("{who:?} deleting: {status:?}"));
            let marker = repo.join(".git/worktrees").join(&name).join("amux-owner");
            until("the workspace to be let go", || async {
                match (consent, tree.exists(), marker.exists()) {
                    (true, false, _) | (false, true, false) => Ok(()),
                    state => Err(format!("{state:?}")),
                }
            })
            .await
            .unwrap_or_else(|why| panic!("{who:?} consent {consent}: {why}"));
            if !consent {
                assert!(tree.join("draft.txt").is_file(), "{who:?}: the work stays");
            }
        }
    }
    host.stop().await;
}
