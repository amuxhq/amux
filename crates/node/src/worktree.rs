//! The worktree a new agent can start in: the daemon hands over the
//! repository by path and the agent's name, and takes the folder, branch
//! and base it gets back as given. amux never removes a worktree or its
//! branch.

use std::path::{Path, PathBuf};

use git_facts::{Worktree, WorktreeError};

/// Where the installation keeps the worktrees it makes, by repository then
/// agent name.
pub const WORKTREES: &str = "worktrees";

/// Makes the worktree an agent starts in, before the agent exists: `git
/// worktree add` under the installation's folder,
/// `<root>/<repository>/<agent name>`, on a branch named after the agent.
pub struct GitWorktrees {
    root: PathBuf,
}

impl GitWorktrees {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// A new worktree of the repository `repository` is in, on a branch for
    /// `agent_name`, made from what `repository` has checked out.
    pub async fn make(
        &self,
        repository: &Path,
        agent_name: &str,
    ) -> Result<Worktree, WorktreeError> {
        let main = git_facts::repository(repository)
            .await?
            .ok_or_else(|| WorktreeError::NotARepository(repository.to_owned()))?;
        let folder = main
            .file_name()
            .map_or_else(|| "repository".into(), |name| name.to_os_string());
        let destination = self.root.join(folder).join(agent_name);
        git_facts::add_worktree(repository, &destination, agent_name).await
    }
}
