//! Git worktrees amux makes without wt, under the installation's folder by
//! repository then name, each on a branch named after it, made from what
//! the creating folder has checked out.
//!
//! Without the workspaces experiment a worktree is made before its agent's
//! row and never removed ([`GitBackend::make_for`]). With it, each worktree
//! carries `amux-owner` in its private git directory
//! (`<repository>/.git/worktrees/<name>/amux-owner`), holding the id of the
//! host that made it: git removes the marker with the worktree, it is never
//! part of the working tree, and it tells this host's worktrees apart from
//! those another profile of the installation made in the same folder. A
//! worktree has no hooks and nothing holds it but amux, so claiming,
//! starting and stopping one do nothing.

use std::path::{Path, PathBuf};

use daemon_core::workspaces::{Key, Kind, Plan, PlanRefusal, Planned, Removal, Resolved};
use git_facts::{Worktree, WorktreeError};
use uuid::Uuid;

use super::{Environment, Holder, Maker, canonical_beyond, delete_if_empty, git, refused};

/// Where the installation keeps the worktrees it makes, by repository then
/// agent name.
pub const WORKTREES: &str = "worktrees";

/// The marker in a worktree's private git directory naming the host that
/// made it.
pub const OWNER_MARKER: &str = "amux-owner";

/// The worktrees of one host, under the installation's folder.
pub struct GitBackend {
    host: String,
    root: PathBuf,
}

impl GitBackend {
    pub fn new(host: Uuid, root: PathBuf) -> Self {
        Self {
            host: host.to_string(),
            root,
        }
    }

    /// A new worktree of the repository `repository` is in, on a branch for
    /// `agent_name`, made from what `repository` has checked out: what a
    /// create in a new worktree does without the workspaces experiment,
    /// before the agent exists. It carries no owner marker.
    pub async fn make_for(
        &self,
        repository: &Path,
        agent_name: &str,
    ) -> Result<Worktree, WorktreeError> {
        let main = git_facts::repository(repository)
            .await?
            .ok_or_else(|| WorktreeError::NotARepository(repository.to_owned()))?;
        git_facts::add_worktree(repository, &self.destination(&main, agent_name), agent_name).await
    }

    /// Where the worktree named `name` of the repository whose main working
    /// tree is `main` lives.
    fn destination(&self, main: &Path, name: &str) -> PathBuf {
        let folder = main
            .file_name()
            .map_or_else(|| "repository".into(), |name| name.to_os_string());
        self.root.join(folder).join(name)
    }

    /// The private git directory of the worktree at `path`.
    fn git_dir(path: &Path) -> Option<PathBuf> {
        git(path, &["rev-parse", "--absolute-git-dir"])
            .ok()
            .map(PathBuf::from)
    }

    /// Whether the worktree whose private git directory is `git_dir` says
    /// this host made it.
    fn ours(&self, git_dir: &Path) -> bool {
        std::fs::read_to_string(git_dir.join(OWNER_MARKER))
            .is_ok_and(|owner| owner.trim() == self.host)
    }
}

impl Maker for GitBackend {
    fn plan(&self, folder: &Path, name: &str) -> Result<Option<Planned>, PlanRefusal> {
        let main = git(
            folder,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .map(PathBuf::from)
        .map_err(|error| refused(wire::ErrorCode::FailedPrecondition, error))?;
        let main = match main.file_name() {
            Some(git) if git == ".git" => main.parent().map_or(main.clone(), Path::to_owned),
            _ => main,
        };
        let base_branch =
            git(folder, &["symbolic-ref", "--quiet", "--short", "HEAD"]).map_err(|_| {
                refused(
                    wire::ErrorCode::FailedPrecondition,
                    format!(
                        "{} has no branch checked out to start a new one from",
                        folder.display()
                    ),
                )
            })?;
        if git(folder, &["check-ref-format", "--branch", name]).is_err() {
            return Err(refused(
                wire::ErrorCode::FailedPrecondition,
                format!("{name:?} cannot be a branch name"),
            ));
        }
        let named = format!("refs/heads/{name}");
        if git(folder, &["rev-parse", "--verify", "--quiet", &named]).is_ok() {
            return Err(PlanRefusal::Taken(format!(
                "the repository already has a branch named {name}"
            )));
        }
        let path = self.destination(&main, name);
        if path.exists() {
            return Err(PlanRefusal::Taken(format!(
                "{} already exists",
                path.display()
            )));
        }
        Ok(Some(Planned {
            workspace: Key {
                kind: Kind::Git,
                id: canonical_beyond(&path).to_string_lossy().into_owned(),
            },
            plan: Plan {
                repository: folder.to_string_lossy().into_owned(),
                name: name.to_owned(),
                branch: name.to_owned(),
                from: None,
            },
            base_branch: Some(base_branch),
        }))
    }

    fn make(&self, workspace: &Key, plan: &Plan) -> Result<(), String> {
        let path = Path::new(&workspace.id);
        let made = (|| {
            if path.exists() {
                return Err(format!("{} already exists", path.display()));
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let path_text = path.to_string_lossy();
            git(
                Path::new(&plan.repository),
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    &plan.branch,
                    &path_text,
                    plan.from.as_deref().unwrap_or("HEAD"),
                ],
            )?;
            let git_dir = Self::git_dir(path)
                .ok_or_else(|| format!("{} has no git directory", path.display()))?;
            std::fs::write(git_dir.join(OWNER_MARKER), &self.host)
                .map_err(|error| format!("marking it this host's: {error}"))
        })();
        made.map_err(|error| format!("making the worktree: {error}"))
    }

    fn start(&self, _: &Key) -> Result<(), String> {
        Ok(())
    }

    fn delete(&self, workspace: &Key, discard: bool) -> Removal {
        let path = Path::new(&workspace.id);
        if !path.is_dir() {
            return Removal::Removed;
        }
        let (Some(git_dir), Ok(common)) = (
            Self::git_dir(path),
            git(
                path,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            ),
        ) else {
            return Removal::Kept;
        };
        if !self.ours(&git_dir) {
            return Removal::Kept;
        }
        // Commands run from the repository's main working tree: git will
        // not remove the worktree it runs in.
        let common = PathBuf::from(common);
        let main = common.parent().map_or(common.clone(), Path::to_owned);
        let branch = git(path, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
        // Explicit flags, as wt's own check has, since a repository's
        // `status.showUntrackedFiles=no` would hide files the forced removal
        // then deletes. Ignored files are not changes: build output goes.
        let dirty = git(
            path,
            &[
                "status",
                "--porcelain",
                "--untracked-files=normal",
                "--ignore-submodules=none",
            ],
        )
        .map(|status| !status.is_empty());
        if dirty != Ok(false) && !discard {
            // Left on disk for the person, and no longer this host's.
            if let Err(error) = std::fs::remove_file(git_dir.join(OWNER_MARKER)) {
                tracing::warn!(id = %workspace.id, %error, "disowning a worktree failed");
            }
            return Removal::Kept;
        }
        let path_text = path.to_string_lossy();
        // Clean but for ignored files, such as build output, or changes the
        // delete agreed to lose: forcing loses nothing else.
        if let Err(error) = git(&main, &["worktree", "remove", "--force", &path_text]) {
            tracing::warn!(id = %workspace.id, %error, "removing a worktree failed");
            return Removal::Kept;
        }
        if let Some(branch) = branch
            && let Err(error) = delete_if_empty(&main, &branch)
        {
            tracing::warn!(id = %workspace.id, %error, "deleting a worktree's empty branch failed");
        }
        Removal::Removed
    }

    fn owned(&self) -> Vec<Key> {
        let mut owned = Vec::new();
        let Ok(repositories) = std::fs::read_dir(&self.root) else {
            return owned;
        };
        for repository in repositories.flatten() {
            let Ok(worktrees) = std::fs::read_dir(repository.path()) else {
                continue;
            };
            for worktree in worktrees.flatten() {
                let path = worktree.path();
                // A worktree's `.git` is a file naming its private git
                // directory.
                let Ok(pointer) = std::fs::read_to_string(path.join(".git")) else {
                    continue;
                };
                let Some(git_dir) = pointer.trim().strip_prefix("gitdir:") else {
                    continue;
                };
                if self.ours(Path::new(git_dir.trim()))
                    && let Ok(canonical) = path.canonicalize()
                {
                    owned.push(Key {
                        kind: Kind::Git,
                        id: canonical.to_string_lossy().into_owned(),
                    });
                }
            }
        }
        owned
    }
}

impl Holder for GitBackend {
    fn resolve(&self, folder: &Path) -> Option<Resolved> {
        let git_dir = Self::git_dir(folder)?;
        if !self.ours(&git_dir) {
            return None;
        }
        let root = super::toplevel(folder)?.canonicalize().ok()?;
        Some(Resolved {
            workspace: Key {
                kind: Kind::Git,
                id: root.to_string_lossy().into_owned(),
            },
            made_here: true,
        })
    }

    fn try_claim(&self, _: &Key) -> Result<bool, String> {
        Ok(true)
    }

    fn claim(&self, _: &Key) -> Result<(), String> {
        Ok(())
    }

    fn release(&self, _: &Key) {}

    fn stop_when_unheld(&self, _: &Key) -> Result<(), String> {
        Ok(())
    }

    fn environment(&self, _: &Key) -> Result<Option<Environment>, String> {
        Ok(None)
    }
}
