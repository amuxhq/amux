//! The repository facts of the agent's folder, read as a job off the
//! agent's step whenever its core asks: at start and at each turn end.

use std::path::Path;

/// Reads the folder's facts once. `base` is the branch the agent's worktree
/// was made from, when its host made one. Outside a repository, or without
/// git, there are none.
pub(crate) async fn read(cwd: &Path, base: Option<&str>) -> Option<wire::Git> {
    match git_facts::facts(cwd, base).await {
        Ok(read) => read.map(to_wire),
        Err(error) => {
            // Without git the row shows no branch; nothing else depends on
            // it.
            eprintln!("amux agent: reading git facts: {error}");
            None
        }
    }
}

fn to_wire(facts: git_facts::GitFacts) -> wire::Git {
    let totals = |totals: Option<git_facts::ChangeTotals>| {
        totals.map(|totals| wire::ChangeTotals {
            files: totals.files,
            added: totals.added,
            removed: totals.removed,
        })
    };
    wire::Git {
        branch: facts.branch,
        base_branch: facts.base_branch,
        uncommitted: totals(facts.uncommitted),
        on_branch: totals(facts.on_branch),
        remote: facts.remote,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn git(cwd: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=amux",
                "-c",
                "user.email=amux@example.invalid",
            ])
            .args(["-c", "commit.gpgsign=false"])
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

    fn totals(files: u32, added: u32, removed: u32) -> Option<wire::ChangeTotals> {
        Some(wire::ChangeTotals {
            files,
            added,
            removed,
        })
    }

    /// An agent working on its base branch shows on its row what is not
    /// pushed yet: its commits since the upstream and its uncommitted work.
    /// With no upstream, the uncommitted work alone.
    #[tokio::test]
    async fn on_the_base_branch_the_row_counts_what_is_not_pushed() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("start.txt"), "start\n").unwrap();
        git(dir, &["add", "start.txt"]);
        git(dir, &["commit", "-q", "-m", "start"]);
        std::fs::write(dir.join("draft.txt"), "draft\n").unwrap();

        let row = super::read(dir, None).await.unwrap();
        assert_eq!(row.base_branch.as_deref(), Some("main"));
        assert_eq!(row.uncommitted, totals(1, 1, 0));
        assert_eq!(row.on_branch, row.uncommitted, "no upstream");

        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
        git(
            dir,
            &["remote", "add", "origin", &remote.path().to_string_lossy()],
        );
        git(dir, &["push", "-q", "-u", "origin", "main"]);
        std::fs::write(dir.join("unpushed.txt"), "one\ntwo\n").unwrap();
        git(dir, &["add", "unpushed.txt"]);
        git(dir, &["commit", "-q", "-m", "not pushed"]);

        let row = super::read(dir, None).await.unwrap();
        assert_eq!(row.branch.as_deref(), Some("main"));
        assert_eq!(row.uncommitted, totals(1, 1, 0));
        assert_eq!(row.on_branch, totals(2, 3, 0), "the unpushed commit counts");
    }
}
