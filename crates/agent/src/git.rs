//! The repository facts of the agent's folder, read as a job off the
//! agent's step whenever its core asks: at start and at each turn end.

use std::path::Path;

/// Reads the folder's facts once. `base` is the branch the agent's worktree
/// was made from, when its host made one. Outside a repository, or without
/// git, there are none.
pub(crate) async fn read(cwd: &Path, base: Option<&str>) -> Option<wire::Git> {
    match git_facts::facts(cwd, base).await {
        Ok(read) => read.map(|facts| to_wire(cwd, facts)),
        Err(error) => {
            // Without git the row shows no branch; nothing else depends on
            // it.
            eprintln!("amux agent: reading git facts: {error}");
            None
        }
    }
}

fn to_wire(cwd: &Path, facts: git_facts::GitFacts) -> wire::Git {
    let totals = |totals: Option<git_facts::ChangeTotals>| {
        totals.map(|totals| wire::ChangeTotals {
            files: totals.files,
            added: totals.added,
            removed: totals.removed,
        })
    };
    let text = |path: std::path::PathBuf| path.to_string_lossy().into_owned();
    // The working tree the folder is in, so a client matches the agent to
    // the folders its host offers by root, not by guessing from paths.
    let checkout = git_facts::working_tree(cwd).map(|(root, main)| wire::Checkout {
        root: text(root),
        main: main.map(text),
        remote: facts.remote,
    });
    wire::Git {
        checkout: checkout.map(Box::new),
        branch: facts.branch,
        base_branch: facts.base_branch,
        uncommitted: totals(facts.uncommitted),
        on_branch: totals(facts.on_branch),
    }
}
