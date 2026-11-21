//! Keeping the git facts on this host's agents' rows current between the
//! reads each agent makes of its own folder.
//!
//! An agent process reads its folder's facts at start and at each of its
//! turn ends, and its snapshot carries them to the row. Several agents may
//! work in one checkout, and whatever one of them, or a person, commits,
//! stashes or reverts there changes what every other one's row counts: the
//! totals compare the whole folder with HEAD, and nothing says whose changes
//! are whose. So the daemon asks the others to read again when one of them
//! ends a turn.

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
}

/// Whether two folders are taken to be in one checkout: the same folder, or
/// one inside the other.
pub(crate) fn one_checkout(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
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
