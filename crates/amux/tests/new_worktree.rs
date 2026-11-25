//! `amux new --worktree`: an agent started from the shell in a new worktree
//! of its folder's repository, with the workspaces experiment on and off.

use std::path::{Path, PathBuf};

use support::desk::{Desk, LONG_GRACE_SECS, say};
use support::{created_id, until};

use crate::support;

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
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

/// A desk whose work folder is a repository on `main`, with the workspaces
/// experiment on or off, and amux started with a wt home of its own, so no
/// wt configuration of the machine's reaches it.
async fn desk(workspaces: bool) -> Desk {
    let desk = Desk::new(
        false,
        LONG_GRACE_SECS,
        node::version(),
        "http://127.0.0.1:9/",
        Vec::new(),
    );
    git(&desk.work, &["init", "-q", "-b", "main"]);
    std::fs::write(desk.work.join("README"), "shop\n").unwrap();
    git(&desk.work, &["add", "README"]);
    git(&desk.work, &["commit", "-q", "-m", "start"]);
    if workspaces {
        let mut config = std::fs::read_to_string(&desk.config).unwrap();
        config.push_str("\n[experiments]\nenabled = [\"workspaces\"]\n");
        std::fs::write(&desk.config, config).unwrap();
    }
    let wt_home = desk.root.path().join("wt-home");
    let started = tokio::time::timeout(
        support::PATIENCE,
        desk.command(&["server", "start"])
            .env("WT_HOME", &wt_home)
            .output(),
    )
    .await
    .expect("amux server start finishes")
    .unwrap();
    assert!(
        started.status.success(),
        "amux server start: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    desk
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    windows,
    ignore = "windows: `amux server start` prints Started but never finishes: the daemon keeps its stdout pipe open"
)]
async fn amux_new_with_worktree_starts_its_agent_in_a_new_worktree() {
    for workspaces in [true, false] {
        say(format!(
            "with the workspaces experiment {}",
            if workspaces { "on" } else { "off" }
        ));
        let desk = desk(workspaces).await;
        let work = desk.work.canonicalize().unwrap();
        let work_text = work.to_string_lossy().into_owned();
        let created = desk
            .run(&[
                "new",
                "claude",
                "--in",
                "amux",
                "--worktree",
                "--name",
                "fix-scroll",
                "--cwd",
                &work_text,
            ])
            .await;
        created_id(&created);
        // The answer names the worktree, which is the agent's folder.
        let at = created
            .split(" in a new worktree at ")
            .nth(1)
            .and_then(|rest| rest.rsplit_once(": "))
            .map(|(path, _)| PathBuf::from(path))
            .unwrap_or_else(|| panic!("no worktree named in: {created}"));
        assert_ne!(at, work, "the agent works in its checkout");
        // The answer comes before the worktree is made, and git writes the
        // worktree's `.git` file before the rest of its records: it is made
        // once git reads it as one.
        until("the worktree to be made", || {
            let at = at.clone();
            async move {
                let read = std::process::Command::new("git")
                    .args(["-c", "core.fsmonitor=false", "rev-parse", "--git-dir"])
                    .current_dir(&at)
                    .output();
                match read {
                    Ok(output) if output.status.success() => Ok(()),
                    _ => Err(format!("{} is not made", at.display())),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(git(&at, &["branch", "--show-current"]), "fix-scroll");
        assert_eq!(
            git(&work, &["branch", "--show-current"]),
            "main",
            "the checkout is untouched"
        );
        let listing = desk.run(&["ls"]).await;
        assert!(listing.contains("fix-scroll"), "{listing}");
    }
}
