//! The workspaces agents run in, and the backends behind them.
//!
//! A workspace is where an agent runs: a plain folder, a git worktree this
//! host made, or a wt tree. Which one a folder is in is read from the disk
//! each time ([`Workspaces::resolve`]), never stored: a folder inside a wt
//! tree is in that tree; otherwise one inside a git worktree this host made
//! (its owner marker says so) is in that worktree; otherwise one inside a
//! repository is in a `folder` workspace at the repository's working tree;
//! otherwise it is a `folder` workspace of its own. A folder that no longer
//! exists is a `folder` workspace at its path as written.
//!
//! Each kind with a backend has two halves, as a microVM later will: its
//! [`Maker`] plans, makes, starts and removes it (for a microVM, the
//! provisioner), and its [`Holder`] counts amux's claim beside the holds
//! from outside amux, stops it once nothing holds it and gives the
//! environment agents run in (always the workspace's own daemon). A
//! `folder` workspace has neither: it is never made, started, stopped or
//! removed.
//!
//! "Made by this host" lives with the workspace, so it survives store
//! resets and the experiment being switched off and on: a git worktree
//! carries `amux-owner` in its private git directory, holding the host's
//! id, and a wt tree's owner is `amux:<host id>`. A workspace another host
//! made is hand-made to this one.
//!
//! Everything here blocks on git, the file system and wt's locks: the
//! driver calls it off the core's thread.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use daemon_core::workspaces::{
    Key, Kind, Plan, PlanRefusal, Planned, Removal, Resolved, WorkspaceDone, WorkspaceJob,
};
use uuid::Uuid;

use crate::plain_paths::canonical;

pub(crate) mod git;
#[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
pub(crate) mod wt;

pub use git::WORKTREES;

/// The environment a workspace gives the agents in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Environment {
    pub vars: BTreeMap<String, String>,
    /// Folders ahead of the `PATH` the agent is launched with.
    pub path_prefix: Vec<PathBuf>,
}

impl Environment {
    /// Joins this environment to a provider's, its `PATH` in front of the
    /// one `env` sets, or else the daemon's own.
    pub fn apply(&self, env: &mut HashMap<String, String>) {
        env.extend(self.vars.clone());
        if self.path_prefix.is_empty() {
            return;
        }
        let inherited = env
            .get("PATH")
            .cloned()
            .or_else(|| std::env::var("PATH").ok());
        let mut path: Vec<String> = self
            .path_prefix
            .iter()
            .map(|folder| folder.to_string_lossy().into_owned())
            .collect();
        path.extend(inherited.filter(|inherited| !inherited.is_empty()));
        env.insert("PATH".to_owned(), path.join(":"));
    }
}

/// What makes, starts and removes the workspaces of one kind.
pub(crate) trait Maker: Send + Sync {
    /// A new workspace named `name` of the repository `folder` is in,
    /// checked against what is on this machine without the network; none
    /// when this kind makes no workspace for that repository.
    fn plan(&self, folder: &Path, name: &str) -> Result<Option<Planned>, PlanRefusal>;
    /// Makes the workspace by its plan, owned by this host. A setup it
    /// starts may go on after it returns.
    fn make(&self, workspace: &Key, plan: &Plan) -> Result<(), String>;
    /// Runs its start hook.
    fn start(&self, workspace: &Key) -> Result<(), String>;
    /// Removes it without waiting for anyone: one held, or with changes and
    /// no `discard`, is kept and no longer this host's.
    fn delete(&self, workspace: &Key, discard: bool) -> Removal;
    /// The workspaces of this kind this host made, as they say on disk.
    fn owned(&self) -> Vec<Key>;
}

/// What holds the workspaces of one kind on amux's behalf, beside the holds
/// from outside amux it cannot see.
pub(crate) trait Holder: Send + Sync {
    /// The workspace containing `folder` (canonical), if it is of this
    /// kind, and whether this host made it.
    fn resolve(&self, folder: &Path) -> Option<Resolved>;
    /// Takes amux's claim without waiting: false when the claim must wait,
    /// for a stop hook that runs, through [`Holder::claim`].
    fn try_claim(&self, workspace: &Key) -> Result<bool, String>;
    /// Takes amux's claim, waiting for a stop hook that runs.
    fn claim(&self, workspace: &Key) -> Result<(), String>;
    /// Releases amux's claim.
    fn release(&self, workspace: &Key);
    /// Waits until nothing else holds the workspace, then stops it unless a
    /// person pinned it.
    fn stop_when_unheld(&self, workspace: &Key) -> Result<(), String>;
    /// What its agents run with; none when it gives nothing.
    fn environment(&self, workspace: &Key) -> Result<Option<Environment>, String>;
}

/// One profile's backends.
pub(crate) struct Workspaces {
    git: git::GitBackend,
    #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
    wt: wt::WtBackend,
}

impl Workspaces {
    /// The backends of the host `host`: git worktrees under `worktrees`, by
    /// repository then name, and wt trees from wt's home at `wt_home`, or
    /// wt's own default (`$WT_HOME`, else `~/.wt`).
    pub fn new(host: Uuid, worktrees: PathBuf, wt_home: Option<PathBuf>) -> Self {
        #[cfg(not(all(unix, not(any(target_os = "ios", target_os = "android")))))]
        let _ = wt_home;
        Self {
            git: git::GitBackend::new(host, worktrees),
            #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
            wt: wt::WtBackend::new(host, wt_home),
        }
    }

    /// The old path's worktree maker, used with the experiment off.
    pub fn worktrees(&self) -> &git::GitBackend {
        &self.git
    }

    fn maker(&self, kind: Kind) -> Option<&dyn Maker> {
        match kind {
            Kind::Folder => None,
            Kind::Git => Some(&self.git),
            #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
            Kind::Wt => Some(&self.wt),
            #[cfg(not(all(unix, not(any(target_os = "ios", target_os = "android")))))]
            Kind::Wt => None,
        }
    }

    fn holder(&self, kind: Kind) -> Option<&dyn Holder> {
        match kind {
            Kind::Folder => None,
            Kind::Git => Some(&self.git),
            #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
            Kind::Wt => Some(&self.wt),
            #[cfg(not(all(unix, not(any(target_os = "ios", target_os = "android")))))]
            Kind::Wt => None,
        }
    }

    /// Plans a new workspace named `name` of the repository `folder` is in:
    /// a wt tree when wt is configured for it, else a git worktree.
    pub fn plan(&self, folder: &Path, name: &str) -> Result<Planned, PlanRefusal> {
        if toplevel(folder).is_none() {
            return Err(refused(
                wire::ErrorCode::FailedPrecondition,
                format!("{} is not in a git repository", folder.display()),
            ));
        }
        #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
        if let Some(planned) = self.wt.plan(folder, name)? {
            return Ok(planned);
        }
        self.git
            .plan(folder, name)?
            .ok_or_else(|| refused(wire::ErrorCode::Internal, "no backend makes it".into()))
    }

    /// The workspace `folder` is in.
    pub fn resolve(&self, folder: &str) -> Resolved {
        let Ok(canonical) = canonical(Path::new(folder)) else {
            return Resolved {
                workspace: Key {
                    kind: Kind::Folder,
                    id: folder.to_owned(),
                },
                made_here: false,
            };
        };
        #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
        if let Some(resolved) = self.wt.resolve(&canonical) {
            return resolved;
        }
        if let Some(resolved) = self.git.resolve(&canonical) {
            return resolved;
        }
        let root = toplevel(&canonical)
            .and_then(|root| crate::plain_paths::canonical(&root).ok())
            .unwrap_or(canonical);
        Resolved {
            workspace: Key {
                kind: Kind::Folder,
                id: root.to_string_lossy().into_owned(),
            },
            made_here: false,
        }
    }

    /// Every workspace this host made, of every kind.
    pub fn owned(&self) -> Vec<Key> {
        let owned = self.git.owned();
        #[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
        let owned = owned.into_iter().chain(self.wt.owned()).collect();
        owned
    }

    pub fn make(&self, workspace: &Key, plan: &Plan) -> Result<(), String> {
        match self.maker(workspace.kind) {
            Some(maker) => maker.make(workspace, plan),
            None => Err(format!("this host makes no {:?} workspace", workspace.kind)),
        }
    }

    pub fn start(&self, workspace: &Key) -> Result<(), String> {
        self.maker(workspace.kind)
            .map_or(Ok(()), |maker| maker.start(workspace))
    }

    pub fn delete(&self, workspace: &Key, discard: bool) -> Removal {
        self.maker(workspace.kind)
            .map_or(Removal::Kept, |maker| maker.delete(workspace, discard))
    }

    pub fn try_claim(&self, workspace: &Key) -> Result<bool, String> {
        self.holder(workspace.kind)
            .map_or(Ok(true), |holder| holder.try_claim(workspace))
    }

    pub fn claim(&self, workspace: &Key) -> Result<(), String> {
        self.holder(workspace.kind)
            .map_or(Ok(()), |holder| holder.claim(workspace))
    }

    pub fn release(&self, workspace: &Key) {
        if let Some(holder) = self.holder(workspace.kind) {
            holder.release(workspace);
        }
    }

    pub fn stop_when_unheld(&self, workspace: &Key) -> Result<(), String> {
        self.holder(workspace.kind)
            .map_or(Ok(()), |holder| holder.stop_when_unheld(workspace))
    }

    pub fn environment(&self, workspace: &Key) -> Result<Option<Environment>, String> {
        self.holder(workspace.kind)
            .map_or(Ok(None), |holder| holder.environment(workspace))
    }
}

/// Runs one of the core's workspace jobs, blocking, and says what it came
/// to. A claim taken here is one that waited for a stop hook.
pub(crate) fn run(backends: &Workspaces, job: WorkspaceJob) -> WorkspaceDone {
    match job {
        WorkspaceJob::Plan { folder, name } => {
            WorkspaceDone::Planned(backends.plan(Path::new(&folder), &name))
        }
        WorkspaceJob::Resolve { folder } => WorkspaceDone::Resolved {
            resolved: backends.resolve(&folder),
            folder,
        },
        WorkspaceJob::Owned => WorkspaceDone::Owned(backends.owned()),
        WorkspaceJob::Make { workspace, plan } => WorkspaceDone::Made {
            result: backends.make(&workspace, &plan),
            workspace,
        },
        WorkspaceJob::TakeClaim { workspace } => WorkspaceDone::Claimed {
            result: backends.claim(&workspace),
            workspace,
        },
        WorkspaceJob::ReleaseClaim { workspace } => {
            backends.release(&workspace);
            WorkspaceDone::Claimed {
                workspace,
                result: Ok(()),
            }
        }
        WorkspaceJob::Start { workspace } => WorkspaceDone::Started {
            result: backends.start(&workspace),
            workspace,
        },
        WorkspaceJob::StopWhenUnheld { workspace } => WorkspaceDone::Stopped {
            result: backends.stop_when_unheld(&workspace),
            workspace,
        },
        WorkspaceJob::Delete { workspace, discard } => WorkspaceDone::Deleted {
            removal: backends.delete(&workspace, discard),
            workspace,
        },
    }
}

/// A refusal the caller of a create reads.
pub(crate) fn refused(code: wire::ErrorCode, message: String) -> PlanRefusal {
    PlanRefusal::Refused(wire::Error {
        code: code as i32,
        message: format!("starting in a new workspace: {message}"),
        details: Vec::new(),
    })
}

/// Runs git in `cwd`, its output trimmed, or what it said on failure.
pub(crate) fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|error| format!("running git: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        let said = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(format!("git {}: {said}", args.join(" ")))
    }
}

/// The working tree `folder` is in, when it is in one.
pub(crate) fn toplevel(folder: &Path) -> Option<PathBuf> {
    git(folder, &["rev-parse", "--show-toplevel"])
        .ok()
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
}

/// `path` with its longest existing ancestor made canonical, so a folder
/// about to be made is named as it will be once it exists.
pub(crate) fn canonical_beyond(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut ancestor = path;
    loop {
        if let Ok(canonical) = canonical(ancestor) {
            return rest
                .iter()
                .rev()
                .fold(canonical, |path, part| path.join(part));
        }
        match (ancestor.parent(), ancestor.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                ancestor = parent;
            }
            _ => return path.to_owned(),
        }
    }
}

/// What `folder` has checked out, and where a branch made from it starts:
/// the branch it is on, and the commit a new branch starts from when that
/// is not the repository's default branch, which a new branch instead
/// takes as freshly fetched. A repository with no `origin` has no default
/// to fetch, so a new branch starts from the folder's commit.
#[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
pub(crate) fn branch_start(folder: &Path) -> (Option<String>, Option<String>) {
    let branch = git(folder, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    let default = git(
        folder,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .ok()
    .and_then(|origin| origin.strip_prefix("origin/").map(str::to_owned));
    if branch.is_some() && branch == default {
        return (branch, None);
    }
    let head = git(
        folder,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
    )
    .ok();
    (branch, head)
}

/// Deletes `branch` from the repository at `main` when it has no commits of
/// its own, by the rule a delete's warning also counts with
/// ([`git_facts::branch_own_commits`]): its tip is reachable from another
/// ref. Neither git nor wt records where a branch started, and the base
/// moves, so reachability is the only test that works.
pub(crate) fn delete_if_empty(main: &Path, branch: &str) -> Result<bool, String> {
    let own = git_facts::branch_own_commits(main, branch).map_err(|error| error.to_string())?;
    if !git_facts::branch_goes(own) {
        return Ok(false);
    }
    git(main, &["branch", "-D", branch])?;
    Ok(true)
}

#[cfg(test)]
mod tests;
