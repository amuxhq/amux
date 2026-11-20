//! What the reviews experiment leaves outside the store, removed when the
//! daemon opens a profile without it: each own agent's snapshot store. The
//! store's tables, and the entries on this host's own rows, go with the
//! experiment by the store's own rules as it opens.
//!
//! A snapshot store holds every file of the working tree when it was taken,
//! untracked ones included, which can be secrets: with nothing left to name
//! its commits it must not stay behind. Unsent drafts go with it, as the
//! experiment's rules accept.

use std::io;
use std::path::Path;

use crate::install::AGENTS;
use crate::runtime::{AgentId, Launch};

/// The profile at `dir` opens under `launch`, its store open.
pub(crate) fn on_open(dir: &Path, store: &store::Sqlite, launch: &Launch) {
    if launch.experiments.is_on(&settings::experiments::REVIEWS) {
        return;
    }
    let rows = match store.agents() {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "listing agents to remove their review stores failed");
            return;
        }
    };
    let agents = dir.join(AGENTS);
    let mut removed = 0;
    for row in rows.iter().filter(|row| row.agent.host == store.own_host()) {
        let Ok(agent) = AgentId::from_slice(&row.agent.agent) else {
            continue;
        };
        match remove(&agents.join(agent.to_string())) {
            Ok(true) => removed += 1,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, %agent, "removing an agent's review store failed");
            }
        }
    }
    if removed > 0 {
        tracing::info!(
            removed,
            "removed the review stores the reviews experiment left"
        );
    }
}

/// Removes the review folder of the agent whose directory is `agent_dir`,
/// answering whether there was one. A link in its place is removed itself:
/// nothing it points at is touched.
fn remove(agent_dir: &Path) -> io::Result<bool> {
    let folder = agent_dir.join(daemon_core::reviews::REVIEW);
    match std::fs::symlink_metadata(&folder) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&folder).map(|()| true),
        // Windows removes a link to a directory as a directory, and doing
        // so leaves what it names.
        Ok(meta) if meta.is_symlink() && cfg!(windows) => std::fs::remove_file(&folder)
            .or_else(|_| std::fs::remove_dir(&folder))
            .map(|()| true),
        Ok(_) => std::fs::remove_file(&folder).map(|()| true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_link_inside_the_review_folder_is_removed_not_followed() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        let folder = agent.join(daemon_core::reviews::REVIEW);
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(folder.join("objects")).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("kept"), "kept").unwrap();
        std::os::unix::fs::symlink(&elsewhere, folder.join("objects").join("info")).unwrap();

        assert!(remove(&agent).unwrap());
        assert!(!folder.exists());
        assert!(agent.is_dir(), "the agent's directory stays");
        assert_eq!(std::fs::read(elsewhere.join("kept")).unwrap(), b"kept");
    }
}
