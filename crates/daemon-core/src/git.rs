//! Keeping the git facts on this host's agents' rows current between the
//! reads each agent makes of its own folder.
//!
//! An agent process reads its folder's facts at start and at each of its
//! turn ends, and its snapshot carries them to the row. Several agents may
//! work in one checkout, and whatever one of them, or a person, commits,
//! stashes or reverts there changes what every other one's row counts: the
//! totals compare the whole folder with HEAD, and nothing says whose changes
//! are whose. So the daemon asks the others to read again when one of them
//! ends a turn, and corrects a row a diff of the folder has just shown to be
//! wrong.

use std::path::Path;

use wire::{CtlFrame, ctl_frame};

use crate::Daemon;
use crate::ids::AgentId;
use crate::io::Output;

impl Daemon {
    /// `agent` ended a turn: every other agent of this host with a live
    /// process and a folder in the same checkout reads its git facts again.
    /// The checkout is judged by path, one folder inside the other or the
    /// same, which costs no git; a folder it wrongly includes, such as a
    /// worktree nested inside another, costs only a read that finds nothing
    /// changed.
    pub(crate) fn turn_ended_beside(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        let Ok(Some(ended)) = self.store.agent(&self.own_key(agent)) else {
            return;
        };
        for other in self.families.connected_agents() {
            if other == agent {
                continue;
            }
            let Ok(Some(row)) = self.store.agent(&self.own_key(other)) else {
                continue;
            };
            if one_checkout(Path::new(&ended.cwd), Path::new(&row.cwd)) {
                read_git(other, out);
            }
        }
    }

    /// A diff of one of this host's agents' folders was just made against
    /// HEAD (`branch` None) or a branch. When the row counts that comparison
    /// differently, the row takes the diff's totals at once, so the marker
    /// every client draws agrees with the changes it opens, and a live agent
    /// reads its facts again, so its next snapshot does not put the old
    /// totals back. A branch other than the row's base is not a comparison
    /// the row counts.
    pub(crate) fn diff_compared(
        &mut self,
        agent_id: &[u8],
        branch: Option<&str>,
        diff: &wire::Diff,
        out: &mut Vec<Output>,
    ) {
        let Ok(Some(mut row)) = self.store.agent_by_id(agent_id) else {
            return;
        };
        if row.agent.host != self.store.own_host() {
            return;
        }
        let Some(git) = row.git.as_mut() else {
            return;
        };
        let counted = match branch {
            None => &mut git.uncommitted,
            Some(branch) if git.base_branch.as_deref() == Some(branch) => &mut git.on_branch,
            Some(_) => return,
        };
        let seen = totals(diff);
        if *counted == Some(seen) {
            return;
        }
        *counted = Some(seen);
        let written = self
            .store
            .set_git(&row.agent, row.git.clone())
            .and_then(|_| self.publish_row(&row.agent, out));
        if let Err(error) = written {
            tracing::warn!(agent = %crate::hex(agent_id), %error, "correcting a row's git totals failed");
            return;
        }
        if let Some(agent) = AgentId::from_slice(agent_id)
            && self.families.connected(agent)
        {
            read_git(agent, out);
        }
    }
}

/// Whether two folders are taken to be in one checkout: the same folder, or
/// one inside the other.
pub(crate) fn one_checkout(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// A diff's changed files counted as an agent's row counts them: every
/// file, and the lines added and removed, a binary file adding none.
fn totals(diff: &wire::Diff) -> wire::ChangeTotals {
    diff.files
        .iter()
        .fold(wire::ChangeTotals::default(), |totals, file| {
            wire::ChangeTotals {
                files: totals.files + 1,
                added: totals.added + file.added,
                removed: totals.removed + file.removed,
            }
        })
}

fn read_git(agent: AgentId, out: &mut Vec<Output>) {
    out.push(Output::WriteControl {
        agent,
        frame: CtlFrame {
            of: Some(ctl_frame::Of::ReadGit(wire::ReadGit {})),
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_share_a_checkout_when_one_holds_the_other() {
        let repo = Path::new("/work/repo");
        assert!(one_checkout(repo, Path::new("/work/repo")));
        assert!(one_checkout(repo, Path::new("/work/repo/crates/a")));
        assert!(one_checkout(Path::new("/work/repo/crates/a"), repo));
        assert!(!one_checkout(repo, Path::new("/work/repo-two")));
        assert!(!one_checkout(repo, Path::new("/work/other")));
    }
}
