//! What an agent's folder says about its git repository: the branch, the
//! branch it is measured against, how much changed and which files, and the
//! commits a branch holds alone. The one piece of git code the agent process
//! and the daemon share, including the worktree a new agent can start in.
//!
//! One rule picks the base a branch's changes run from, for the fleet row's
//! counts and the host's list of changed files alike ([`Base::Default`]),
//! and one rule says which commits only a branch holds, for a delete's
//! warning and the removal alike ([`branch_own_commits`]).
//!
//! Everything is read by running `git` in the folder, with optional locks
//! off so a read never contends with the person's own git. The person's
//! index is never written: totals that need untracked files counted use a
//! temporary copy of it.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use tokio::io::AsyncWriteExt;

/// How much a comparison changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangeTotals {
    /// Files added, removed or changed, binary ones included.
    pub files: u32,
    /// Lines added and removed, binary files counting none.
    pub added: u32,
    pub removed: u32,
}

/// What a fleet row shows about an agent's repository.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitFacts {
    /// None on a detached head.
    pub branch: Option<String>,
    /// The branch amux made the worktree from when it made one, else the
    /// repository's default branch; None when neither is known.
    pub base_branch: Option<String>,
    /// The working tree against HEAD, untracked files included; always
    /// present in a repository, where an unborn HEAD compares with the
    /// empty tree.
    pub uncommitted: Option<ChangeTotals>,
    /// From where the branch left its base to the working tree, so
    /// uncommitted work counts. On the base branch itself, from where it
    /// left its upstream, so commits not pushed yet count too; with no
    /// upstream, the uncommitted work. None without a base or a commit, or
    /// when the branch shares no history with it.
    pub on_branch: Option<ChangeTotals>,
    /// Where origin points, as [`remote_identity`] writes it; None without
    /// an origin.
    pub remote: Option<String>,
}

/// What a comparison runs from; it always runs to the working tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Comparison {
    /// From HEAD: the work not committed yet.
    Uncommitted,
    /// From where the branch left `base`, so the branch's commits and its
    /// uncommitted work both count.
    OnBranch { base: String },
}

/// What happened to a file between the two ends of a comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Created,
    Deleted,
    Changed,
}

/// One file a comparison found different.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    /// Lines added and removed; none for a binary file.
    pub added: u32,
    pub removed: u32,
    pub change: Change,
    pub binary: bool,
}

/// A comparison of an agent's folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Compared {
    /// The commit checked out; None before the first commit.
    pub head: Option<String>,
    /// For a branch comparison, where the branch left its base.
    pub merge_base: Option<String>,
    pub files: Vec<FileChange>,
    /// Built only when asked for.
    pub patch: Option<Vec<u8>>,
}

/// What [`changes`] compares the working tree with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Base {
    /// HEAD: the work not committed yet.
    Head,
    /// Where the branch left this branch: its commits and its uncommitted
    /// work.
    Branch(String),
    /// The default rule. The base branch is `recorded_branch`, the branch
    /// the agent's worktree was made from as its spec records it, else the
    /// repository's default branch (what `origin/HEAD` names, else
    /// `init.defaultBranch`, else `main` or `master`). The changes run from
    /// where the branch left it; on the base branch itself from where it
    /// left its upstream, so commits not pushed yet count; and with no
    /// upstream, from HEAD.
    Default { recorded_branch: Option<String> },
}

/// The base a comparison actually ran from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsedBase {
    /// The base branch: the one named, or the one the default rule found.
    /// None from HEAD by request, or when the default rule found none.
    pub branch: Option<String>,
    /// On the base branch itself, the upstream the comparison ran from
    /// (`origin/main`); None otherwise, and when there is no upstream.
    pub upstream: Option<String>,
    /// The commit the comparison ran from; None before the first commit,
    /// when it ran from the empty tree.
    pub commit: Option<String>,
}

/// What happened to a file between a base and the working tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Changed,
    Created,
    Deleted,
    /// Moved from [`ChangedFile::old_path`], perhaps changed as well.
    Renamed,
}

/// What a reader may want folded away or left out. Each is set by
/// `.gitattributes` when it says so either way, and otherwise by the
/// heuristic named.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    /// `linguist-generated`, else a header in the file's first lines that
    /// says `@generated` or `DO NOT EDIT`.
    pub generated: bool,
    /// A package manager's lock file, by its name ([`LOCKFILES`]).
    pub lockfile: bool,
    /// `linguist-vendored`, else a folder in its path named one of
    /// [`VENDORED_FOLDERS`].
    pub vendored: bool,
    /// Over [`LARGE_FILE_BYTES`] on either side.
    pub large: bool,
}

/// One file that differs between a base and the working tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    /// Where a renamed file was, at the base.
    pub old_path: Option<String>,
    pub status: Status,
    /// Lines added and removed; none for a binary file.
    pub added: u32,
    pub removed: u32,
    pub binary: bool,
    pub flags: Flags,
}

/// What changed in an agent's folder against a base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changes {
    /// The repository's top folder. git names every file from here, not
    /// from the folder it was asked in, so a file is read at
    /// `root.join(path)` even when the agent works in a subfolder.
    pub root: PathBuf,
    pub base: UsedBase,
    /// The commit checked out; None before the first commit.
    pub head: Option<String>,
    pub files: Vec<ChangedFile>,
}

/// What deleting a workspace would take with it, in git.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteFacts {
    /// The branch checked out; None on a detached head.
    pub branch: Option<String>,
    /// The branch's [`branch_own_commits`]: None on a detached head, before
    /// the first commit, or when they could not be counted.
    pub own_commits: Option<u32>,
    /// Whether the removal takes the branch with it ([`branch_goes`]).
    pub branch_goes: bool,
    /// The working tree against HEAD, untracked files included.
    pub uncommitted: ChangeTotals,
}

/// Names of the lock files package managers write, flagged wherever they
/// sit.
pub const LOCKFILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
    "deno.lock",
    "Gemfile.lock",
    "poetry.lock",
    "Pipfile.lock",
    "uv.lock",
    "pdm.lock",
    "composer.lock",
    "go.sum",
    "go.work.sum",
    "Package.resolved",
    "Podfile.lock",
    "Cartfile.resolved",
    "flake.lock",
    "mix.lock",
    "pubspec.lock",
    "packages.lock.json",
    "gradle.lockfile",
    "MODULE.bazel.lock",
];

/// Folders whose contents are someone else's code, at any depth.
pub const VENDORED_FOLDERS: &[&str] = &["vendor", "third_party", "node_modules"];

/// The size over which a file is large: 1 MiB, on the base side or the
/// working tree's.
pub const LARGE_FILE_BYTES: u64 = 1024 * 1024;

/// How far into a file a generated header is looked for.
const HEADER_LINES: usize = 10;
const HEADER_BYTES: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("running git: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("git {command} failed: {stderr}")]
    Failed { command: String, stderr: String },
    #[error("the temporary index: {0}")]
    Scratch(#[source] std::io::Error),
    #[error("the folder is not in a git repository")]
    NotARepository,
    #[error("the branch shares no history with {0}")]
    NoForkPoint(String),
}

/// A worktree made for an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// The new branch checked out in it.
    pub branch: String,
    /// The branch it was made from: what the repository had checked out.
    pub base_branch: String,
}

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("{} is not in a git repository", .0.display())]
    NotARepository(PathBuf),
    #[error("{} has no branch checked out to start a new one from", .0.display())]
    NoBranch(PathBuf),
    #[error("the repository already has a branch named {0}")]
    BranchExists(String),
    #[error("{0:?} cannot be a branch name")]
    BadBranchName(String),
    #[error("{} already exists", .0.display())]
    DestinationExists(PathBuf),
    #[error("making the worktree: {0}")]
    Git(String),
}

impl From<GitError> for WorktreeError {
    fn from(error: GitError) -> Self {
        Self::Git(error.to_string())
    }
}

/// The repository `cwd` belongs to, named by its main working tree's folder
/// (for a bare repository, the repository's own folder); None when `cwd` is
/// not in one. The same for every worktree of the repository.
pub async fn repository(cwd: &Path) -> Result<Option<PathBuf>, GitError> {
    let Some(common) = answer(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?
    else {
        return Ok(None);
    };
    let common = PathBuf::from(common);
    Ok(Some(match common.file_name() {
        Some(name) if name == ".git" => common.parent().map_or(common.clone(), Path::to_owned),
        _ => common,
    }))
}

/// A new worktree at `destination` on a new branch `branch`, made from
/// whatever `repository` (any folder in it) has checked out. Never removes
/// anything, and fails rather than reuse a branch or a folder.
pub async fn add_worktree(
    repository: &Path,
    destination: &Path,
    branch: &str,
) -> Result<Worktree, WorktreeError> {
    let inside = run(
        repository,
        None,
        &["rev-parse", "--is-inside-work-tree"],
        None,
    )
    .await?;
    if !inside.status.success() || text(&inside.stdout) != "true" {
        return Err(WorktreeError::NotARepository(repository.to_owned()));
    }
    let base_branch = answer(repository, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .await?
        .ok_or_else(|| WorktreeError::NoBranch(repository.to_owned()))?;
    if answer(repository, &["check-ref-format", "--branch", branch])
        .await?
        .is_none()
    {
        return Err(WorktreeError::BadBranchName(branch.to_owned()));
    }
    let named = format!("refs/heads/{branch}");
    if answer(repository, &["rev-parse", "--verify", "--quiet", &named])
        .await?
        .is_some()
    {
        return Err(WorktreeError::BranchExists(branch.to_owned()));
    }
    if destination.exists() {
        return Err(WorktreeError::DestinationExists(destination.to_owned()));
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| WorktreeError::Git(error.to_string()))?;
    }
    let destination_text = destination.to_string_lossy();
    let added = run(
        repository,
        None,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            branch,
            &destination_text,
            "HEAD",
        ],
        None,
    )
    .await?;
    if !added.status.success() {
        return Err(WorktreeError::Git(text(&added.stderr)));
    }
    Ok(Worktree {
        path: destination.to_owned(),
        branch: branch.to_owned(),
        base_branch,
    })
}

/// The facts for `cwd`, or None when it is not inside a git working tree.
/// `recorded_base` is the branch a worktree was made from when amux made
/// it; without one the base is the repository's default branch.
pub async fn facts(cwd: &Path, recorded_base: Option<&str>) -> Result<Option<GitFacts>, GitError> {
    let inside = run(cwd, None, &["rev-parse", "--is-inside-work-tree"], None).await?;
    if !inside.status.success() || text(&inside.stdout) != "true" {
        return Ok(None);
    }
    let branch = answer(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await?;
    let head = answer(cwd, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).await?;
    let base_branch = match recorded_base {
        Some(base) => Some(base.to_owned()),
        None => default_branch(cwd).await?,
    };
    let against = match &head {
        Some(head) => head.clone(),
        None => empty_tree(cwd).await?,
    };
    let uncommitted = Some(totals(cwd, &against).await?);
    let on_branch = match (&base_branch, &head) {
        (Some(base), Some(head)) => match branch_base(cwd, base).await? {
            Some(used) if used.commit.as_deref() == Some(head.as_str()) => uncommitted,
            Some(used) => Some(totals(cwd, used.commit.as_deref().unwrap_or(head)).await?),
            None => None,
        },
        _ => None,
    };
    let remote = answer(cwd, &["config", "--get", "remote.origin.url"])
        .await?
        .map(|url| remote_identity(&url))
        .filter(|remote| !remote.is_empty());
    Ok(Some(GitFacts {
        branch,
        base_branch,
        uncommitted,
        on_branch,
        remote,
    }))
}

/// The working tree a directory is in, as it is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkout {
    /// The working tree's top directory: the nearest one up holding `.git`.
    pub root: PathBuf,
    /// The main clone's top directory, when `root` is a linked worktree of
    /// it; a submodule is a repository of its own.
    pub main: Option<PathBuf>,
    /// Where origin points, as [`remote_identity`] writes it.
    pub remote: Option<String>,
}

/// The working tree `dir` is in, canonical; None outside one. Reads the
/// file system and asks git for the origin: call it off the async workers.
pub fn checkout(dir: &Path) -> Option<Checkout> {
    let (root, main) = working_tree(dir)?;
    let remote = origin(&root);
    Some(Checkout { root, main, remote })
}

/// The working tree `dir` is in and, for a linked worktree, the main clone
/// it was made from, read from the file system alone.
pub fn working_tree(dir: &Path) -> Option<(PathBuf, Option<PathBuf>)> {
    let dir = dir.canonicalize().ok()?;
    let root = dir.ancestors().find(|dir| dir.join(".git").exists())?;
    let dot_git = root.join(".git");
    let mut main = None;
    if dot_git.is_file()
        && let Ok(text) = std::fs::read_to_string(&dot_git)
        && let Some(gitdir) = text.trim().strip_prefix("gitdir:")
    {
        // `<repository>/.git/worktrees/<name>`; a submodule's points under
        // `.git/modules` and is a repository of its own.
        let gitdir = root.join(gitdir.trim());
        if let Some(worktrees) = gitdir.parent()
            && worktrees
                .file_name()
                .is_some_and(|name| name == "worktrees")
            && let Some(clone) = worktrees.parent().and_then(Path::parent)
            && let Ok(clone) = clone.canonicalize()
            && clone.join(".git").is_dir()
        {
            main = Some(clone);
        }
    }
    Some((root.to_owned(), main))
}

/// Where the origin of the repository at `cwd` points, as
/// [`remote_identity`] writes it; None outside a repository or without an
/// origin. Blocks on git: call it off the async workers.
pub fn origin(cwd: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| remote_identity(&text(&output.stdout)))
        .filter(|remote| !remote.is_empty())
}

/// A remote's URL as the repository it names, the same for every way of
/// writing it: `host/owner/name`, with no scheme, credentials, port or
/// trailing `.git`, the host in lower case. `git@github.com:amuxhq/amux.git`
/// and `https://token@github.com/amuxhq/amux` are both
/// `github.com/amuxhq/amux`. A local remote is its path.
pub fn remote_identity(url: &str) -> String {
    let url = url.trim();
    let trimmed = |path: &str| {
        let path = path.trim_end_matches(['/', '\\']);
        path.strip_suffix(".git")
            .unwrap_or(path)
            .trim_end_matches(['/', '\\'])
            .to_owned()
    };
    let joined = |authority: &str, path: &str| {
        // Credentials end at the authority's last '@'; a port follows its
        // last ':' (an IPv6 literal keeps its brackets).
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let host = match host.rsplit_once(':') {
            Some((name, port)) if !name.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
                name
            }
            _ => host,
        };
        let path = trimmed(path.trim_start_matches('/'));
        match path.is_empty() {
            true => host.to_ascii_lowercase(),
            false => format!("{}/{path}", host.to_ascii_lowercase()),
        }
    };
    if let Some((scheme, rest)) = url.split_once("://") {
        if scheme.eq_ignore_ascii_case("file") {
            return trimmed(rest);
        }
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        return joined(authority, path);
    }
    // scp-like `user@host:owner/name`: a ':' before any '/'. A drive
    // letter and a slash (`C:\src\amux`, `C:/src/amux`) is a Windows path,
    // as git reads it too.
    if let Some((authority, path)) = url.split_once(':')
        && !authority.contains(['/', '\\'])
        && !authority.is_empty()
        && !is_drive(authority, path)
    {
        return joined(authority, path);
    }
    trimmed(url)
}

/// Whether `letter:` then `rest` starts a Windows path: one ASCII letter,
/// then a slash either way.
fn is_drive(letter: &str, rest: &str) -> bool {
    letter.len() == 1
        && letter.chars().all(|c| c.is_ascii_alphabetic())
        && rest.starts_with(['/', '\\'])
}

/// Compares the working tree of `cwd` with where `comparison` runs from:
/// the list of changed files always, the patch only `with_patch`.
pub async fn compare(
    cwd: &Path,
    comparison: &Comparison,
    with_patch: bool,
) -> Result<Compared, GitError> {
    let inside = run(cwd, None, &["rev-parse", "--is-inside-work-tree"], None).await?;
    if !inside.status.success() || text(&inside.stdout) != "true" {
        return Err(GitError::NotARepository);
    }
    let head = answer(cwd, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).await?;
    let (against, merge_base) = match comparison {
        Comparison::Uncommitted => match &head {
            Some(head) => (head.clone(), None),
            None => (empty_tree(cwd).await?, None),
        },
        Comparison::OnBranch { base } => {
            let fork = match head {
                Some(_) => fork_point(cwd, base).await?,
                None => None,
            };
            let fork = fork.ok_or_else(|| GitError::NoForkPoint(base.clone()))?;
            (fork.clone(), Some(fork))
        }
    };
    let scratch = Scratch::new(cwd).await?;
    let files = scratch.files(&against).await?;
    let patch = match with_patch {
        true => Some(scratch.patch(&against).await?),
        false => None,
    };
    Ok(Compared {
        head,
        merge_base,
        files,
        patch,
    })
}

/// What changed in the whole working tree of the repository `cwd` is in,
/// against `base`, untracked files included, renames found, and the base it
/// ran from. The person's
/// index is not touched and nothing is stored.
pub async fn changes(cwd: &Path, base: &Base) -> Result<Changes, GitError> {
    let inside = run(cwd, None, &["rev-parse", "--is-inside-work-tree"], None).await?;
    if !inside.status.success() || text(&inside.stdout) != "true" {
        return Err(GitError::NotARepository);
    }
    let root = answer(cwd, &["rev-parse", "--show-toplevel"])
        .await?
        .map(PathBuf::from)
        .ok_or(GitError::NotARepository)?;
    // Everything from here runs at the top: git names files from there,
    // and listing untracked files from a subfolder would see only that
    // subfolder, so an agent working in one would get a partial answer.
    let cwd = root.as_path();
    let head = answer(cwd, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).await?;
    let used = match base {
        Base::Head => UsedBase {
            commit: head.clone(),
            ..UsedBase::default()
        },
        Base::Branch(branch) => {
            let fork = match head {
                Some(_) => fork_point(cwd, branch).await?,
                None => None,
            };
            UsedBase {
                branch: Some(branch.clone()),
                upstream: None,
                commit: Some(fork.ok_or_else(|| GitError::NoForkPoint(branch.clone()))?),
            }
        }
        Base::Default { recorded_branch } => {
            let branch = match recorded_branch {
                Some(branch) => Some(branch.clone()),
                None => default_branch(cwd).await?,
            };
            match (branch, &head) {
                (Some(branch), Some(_)) => branch_base(cwd, &branch)
                    .await?
                    .ok_or(GitError::NoForkPoint(branch))?,
                // Before the first commit, or with no base branch to be
                // found, all there is to show is the uncommitted work.
                (branch, _) => UsedBase {
                    branch,
                    upstream: None,
                    commit: head.clone(),
                },
            }
        }
    };
    let against = match &used.commit {
        Some(commit) => commit.clone(),
        None => empty_tree(cwd).await?,
    };
    let scratch = Scratch::new(cwd).await?;
    let raw = scratch.diff(&against, true).await?;
    let files = flagged(cwd, raw).await?;
    Ok(Changes {
        root,
        base: used,
        head,
        files,
    })
}

/// What deleting the workspace at `cwd` would take with it: its branch,
/// the branch's own commits by the rule the removal applies, and its
/// uncommitted work. None outside a git working tree.
pub async fn delete_facts(cwd: &Path) -> Result<Option<DeleteFacts>, GitError> {
    let inside = run(cwd, None, &["rev-parse", "--is-inside-work-tree"], None).await?;
    if !inside.status.success() || text(&inside.stdout) != "true" {
        return Ok(None);
    }
    let branch = answer(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await?;
    let head = answer(cwd, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).await?;
    let against = match &head {
        Some(head) => head.clone(),
        None => empty_tree(cwd).await?,
    };
    let uncommitted = totals(cwd, &against).await?;
    let own_commits = own_commits(cwd).await?;
    Ok(Some(DeleteFacts {
        branch,
        own_commits,
        branch_goes: branch_goes(own_commits),
        uncommitted,
    }))
}

/// How many commits the branch `cwd` has checked out holds that no other
/// ref does: [`branch_own_commits`] for that branch. None on a detached
/// head, or before the first commit.
pub async fn own_commits(cwd: &Path) -> Result<Option<u32>, GitError> {
    let Some(branch) = answer(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await? else {
        return Ok(None);
    };
    let cwd = cwd.to_owned();
    tokio::task::spawn_blocking(move || branch_own_commits(&cwd, &branch))
        .await
        .map_err(|error| GitError::Spawn(std::io::Error::other(error)))?
}

/// How many commits `branch` of the repository `repository` (any folder in
/// it) holds that no other ref does, its own upstream and the stash aside:
/// what deleting the branch would lose. Zero exactly when its tip is
/// reachable from another ref. None when there is no such branch.
///
/// The one statement of the rule: a workspace's removal deletes the branch
/// by it ([`branch_goes`]) and a delete's warning counts by it, so the
/// warning cannot promise what the removal does not do. It blocks, since
/// the removal runs on a thread of its own; [`own_commits`] is its async
/// face.
pub fn branch_own_commits(repository: &Path, branch: &str) -> Result<Option<u32>, GitError> {
    let own = format!("refs/heads/{branch}");
    if blocking::answer(repository, &["rev-parse", "--verify", "--quiet", &own])?.is_none() {
        return Ok(None);
    }
    let upstream = blocking::answer(repository, &["for-each-ref", "--format=%(upstream)", &own])?;
    let refs = blocking::checked(repository, &["for-each-ref", "--format=%(refname)"], None)?;
    // Every other ref, negated, read from standard input: a repository may
    // have more refs than an argument list holds. The upstream does not
    // keep the commits: deleting the branch loses them here all the same.
    let others: String = text(&refs)
        .lines()
        .filter(|name| *name != own && Some(*name) != upstream.as_deref() && *name != "refs/stash")
        .map(|name| format!("^{name}\n"))
        .collect();
    let count = blocking::checked(
        repository,
        &["rev-list", "--count", &own, "--stdin"],
        Some(others.into_bytes()),
    )?;
    Ok(text(&count).parse().ok())
}

/// Whether a workspace's removal takes its branch with it, from the
/// branch's [`branch_own_commits`]: only when it has none, and never when
/// they could not be counted.
pub fn branch_goes(own_commits: Option<u32>) -> bool {
    own_commits == Some(0)
}

/// The repository's default branch: what `origin/HEAD` names, else the
/// configured default for new repositories, else `main` or `master`,
/// whichever exists.
async fn default_branch(cwd: &Path) -> Result<Option<String>, GitError> {
    if let Some(remote) = answer(
        cwd,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .await?
    {
        return Ok(Some(
            remote
                .strip_prefix("origin/")
                .map_or(remote.clone(), str::to_owned),
        ));
    }
    let configured = answer(cwd, &["config", "--get", "init.defaultBranch"]).await?;
    for candidate in configured
        .iter()
        .map(String::as_str)
        .chain(["main", "master"])
    {
        if branch_ref(cwd, candidate).await?.is_some() {
            return Ok(Some(candidate.to_owned()));
        }
    }
    Ok(None)
}

/// The ref a base branch is read from: the local branch, else origin's,
/// else a remote's branch named with its remote (`origin/main`), which is
/// how the default rule reports an upstream. So every name the rule
/// reports as the base can be asked for again and finds the same commit.
async fn branch_ref(cwd: &Path, branch: &str) -> Result<Option<String>, GitError> {
    for name in [
        format!("refs/heads/{branch}"),
        format!("refs/remotes/origin/{branch}"),
        format!("refs/remotes/{branch}"),
    ] {
        if answer(cwd, &["rev-parse", "--verify", "--quiet", &name])
            .await?
            .is_some()
        {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// Where HEAD left `base`: their merge base. None when the base does not
/// exist or shares no history with HEAD.
async fn fork_point(cwd: &Path, base: &str) -> Result<Option<String>, GitError> {
    let Some(base) = branch_ref(cwd, base).await? else {
        return Ok(None);
    };
    answer(cwd, &["merge-base", &base, "HEAD"]).await
}

/// Where the default rule starts the changes of the commit checked out,
/// given the base branch `base`: from where the branch left `base`; and
/// with `base` itself checked out, from where it left its upstream, so
/// commits not yet pushed count, and with no upstream, from HEAD. None when
/// `base` does not exist or shares no history with HEAD.
///
/// Which branch is checked out decides the second step, not where HEAD
/// sits. A branch of its own that has not left `base` yet has no commits of
/// its own, though its merge base is HEAD as on `base` itself: asking its
/// upstream would count `base`'s commits since it was pushed as the
/// agent's.
async fn branch_base(cwd: &Path, base: &str) -> Result<Option<UsedBase>, GitError> {
    let Some(fork) = fork_point(cwd, base).await? else {
        return Ok(None);
    };
    let mut used = UsedBase {
        branch: Some(base.to_owned()),
        upstream: None,
        commit: Some(fork),
    };
    let checked_out = answer(cwd, &["symbolic-ref", "--quiet", "HEAD"]).await?;
    if checked_out.as_deref() != Some(format!("refs/heads/{base}").as_str()) {
        return Ok(Some(used));
    }
    // A branch with no upstream, or one that is gone, has none to ask.
    let upstream = answer(
        cwd,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .await?;
    if let Some(upstream) = upstream
        && let Some(commit) = answer(cwd, &["merge-base", &upstream, "HEAD"]).await?
    {
        let short = upstream
            .strip_prefix("refs/remotes/")
            .or_else(|| upstream.strip_prefix("refs/heads/"))
            .unwrap_or(&upstream)
            .to_owned();
        used.upstream = Some(short);
        used.commit = Some(commit);
    }
    Ok(Some(used))
}

/// The empty tree's id in this repository's hash.
async fn empty_tree(cwd: &Path) -> Result<String, GitError> {
    let output = checked(cwd, None, &["hash-object", "-t", "tree", "--stdin"]).await?;
    Ok(text(&output))
}

/// `against` compared with the working tree, untracked files included.
async fn totals(cwd: &Path, against: &str) -> Result<ChangeTotals, GitError> {
    let scratch = Scratch::new(cwd).await?;
    Ok(sum(&scratch.files(against).await?))
}

fn sum(files: &[FileChange]) -> ChangeTotals {
    files
        .iter()
        .fold(ChangeTotals::default(), |totals, file| ChangeTotals {
            files: totals.files + 1,
            added: totals.added + file.added,
            removed: totals.removed + file.removed,
        })
}

/// The scratch index's name inside its folder.
const INDEX: &str = "index";

/// A temporary copy of the person's index with intent-to-add entries for
/// the untracked files, so a comparison with the working tree counts them
/// as added and the person's own index is never touched. Removed on drop.
///
/// The copy lives in a folder only this user can enter: the index names
/// every file in the repository, the system temporary folder is shared on
/// Linux, and git rewrites the copy through a lock file of its own whose
/// permissions follow the umask, not the copy's.
struct Scratch<'a> {
    cwd: &'a Path,
    dir: tempfile::TempDir,
}

impl<'a> Scratch<'a> {
    async fn new(cwd: &'a Path) -> Result<Self, GitError> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("amux-git-facts-");
        #[cfg(unix)]
        builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700));
        let dir = builder.tempdir().map_err(GitError::Scratch)?;
        let own = text(
            &checked(
                cwd,
                None,
                &["rev-parse", "--path-format=absolute", "--git-path", "index"],
            )
            .await?,
        );
        // The bytes, not the file: a copy would carry the index's own
        // permissions with it. The modification time comes too: git reads
        // again any entry written in the same second as the index file, and
        // judges that by the file's time, so a fresh time would hide an
        // edit made in that second.
        match std::fs::File::open(&own) {
            Ok(mut from) => {
                let mut to =
                    std::fs::File::create_new(dir.path().join(INDEX)).map_err(GitError::Scratch)?;
                std::io::copy(&mut from, &mut to).map_err(GitError::Scratch)?;
                let written = from
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .map_err(GitError::Scratch)?;
                to.set_modified(written).map_err(GitError::Scratch)?;
            }
            // A repository nothing was ever added to has no index yet.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(GitError::Scratch(error)),
        }
        let scratch = Self { cwd, dir };
        let untracked = scratch
            .checked(&["ls-files", "--others", "--exclude-standard", "-z"])
            .await?;
        // The paths go to git on its standard input as ls-files wrote them:
        // a folder's untracked paths can together pass the platform's
        // command-line limit, and are not always valid UTF-8.
        if !untracked.is_empty() {
            checked_with_input(
                cwd,
                Some(&scratch.index()),
                &[
                    "--literal-pathspecs",
                    "add",
                    "--intent-to-add",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                ],
                untracked,
            )
            .await?;
        }
        Ok(scratch)
    }

    fn index(&self) -> std::path::PathBuf {
        self.dir.path().join(INDEX)
    }

    async fn checked(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        checked(self.cwd, Some(&self.index()), args).await
    }

    /// Each file that differs between `against` and the working tree,
    /// with renames as one file each.
    async fn files(&self, against: &str) -> Result<Vec<FileChange>, GitError> {
        Ok(self
            .diff(against, false)
            .await?
            .into_iter()
            .map(|file| FileChange {
                path: file.path,
                added: file.added,
                removed: file.removed,
                change: match file.status {
                    Status::Created => Change::Created,
                    Status::Deleted => Change::Deleted,
                    Status::Changed | Status::Renamed => Change::Changed,
                },
                binary: file.binary,
            })
            .collect())
    }

    /// Each file that differs between `against` and the working tree, with
    /// renames found when `renames`.
    async fn diff(&self, against: &str, renames: bool) -> Result<Vec<RawFile>, GitError> {
        let raw = self
            .checked(&[
                "diff",
                "--raw",
                "--numstat",
                "-z",
                "--no-abbrev",
                if renames {
                    "--find-renames"
                } else {
                    "--no-renames"
                },
                "--no-ext-diff",
                "--no-textconv",
                against,
                "--",
            ])
            .await?;
        Ok(raw_files(&raw))
    }

    /// The patch from `against` to the working tree, with full object ids
    /// on its index lines: each file's identity.
    async fn patch(&self, against: &str) -> Result<Vec<u8>, GitError> {
        self.checked(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-renames",
            "--full-index",
            against,
            "--",
        ])
        .await
    }
}

/// One file of `git diff --raw --numstat`, as git wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RawFile {
    path: String,
    old_path: Option<String>,
    status: Status,
    /// Modes as git writes them (`100644`, `120000` for a link, `160000`
    /// for a submodule, `000000` for a side that is not there).
    old_mode: String,
    new_mode: String,
    /// The base side's object id.
    old_id: String,
    added: u32,
    removed: u32,
    binary: bool,
}

/// Reads `git diff --raw --numstat -z`: first a raw record per file
/// (`:modes ids STATUS NUL path NUL`, or with two paths, old then new, for
/// a rename), then a numstat record per file in the same order (`added TAB
/// removed TAB path NUL`, or `added TAB removed TAB NUL old NUL new NUL`
/// for a rename; `-` for both counts on a binary file).
fn raw_files(output: &[u8]) -> Vec<RawFile> {
    let mut fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(String::from_utf8_lossy);
    let mut files: Vec<RawFile> = Vec::new();
    let mut counted = 0;
    while let Some(field) = fields.next() {
        if let Some(raw) = field.strip_prefix(':') {
            let parts: Vec<&str> = raw.split(' ').collect();
            let letter = parts.get(4).and_then(|status| status.chars().next());
            let first = fields
                .next()
                .map(|path| path.into_owned())
                .unwrap_or_default();
            let (old_path, path) = match letter {
                Some('R' | 'C') => (
                    Some(first),
                    fields
                        .next()
                        .map(|path| path.into_owned())
                        .unwrap_or_default(),
                ),
                _ => (None, first),
            };
            files.push(RawFile {
                path,
                old_path: old_path.filter(|_| letter == Some('R')),
                status: match letter {
                    Some('A') => Status::Created,
                    Some('D') => Status::Deleted,
                    Some('R') => Status::Renamed,
                    // A copy is not asked for; read as the new file it is.
                    Some('C') => Status::Created,
                    _ => Status::Changed,
                },
                old_mode: parts.first().copied().unwrap_or_default().to_owned(),
                new_mode: parts.get(1).copied().unwrap_or_default().to_owned(),
                old_id: parts.get(2).copied().unwrap_or_default().to_owned(),
                added: 0,
                removed: 0,
                binary: false,
            });
            continue;
        }
        let mut parts = field.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if path.is_empty() {
            // A rename's two paths follow; the raw record has them.
            fields.next();
            fields.next();
        }
        if let Some(file) = files.get_mut(counted) {
            file.binary = added == "-" && removed == "-";
            file.added = added.parse().unwrap_or(0);
            file.removed = removed.parse().unwrap_or(0);
        }
        counted += 1;
    }
    files
}

/// `files` with their flags, read from the repository's top folder.
async fn flagged(root: &Path, files: Vec<RawFile>) -> Result<Vec<ChangedFile>, GitError> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let attributes = attributes(root, &files).await?;
    let base_sizes = base_sizes(root, &files).await?;
    let regular = |mode: &str| mode.starts_with("100");
    let mut flagged = Vec::with_capacity(files.len());
    // The base side of deleted files, read for their headers in one go.
    let mut deleted_heads = Vec::new();
    for file in files {
        let (generated, vendored) = attributes.get(&file.path).copied().unwrap_or_default();
        let base_size = base_sizes.get(&file.old_id).copied();
        let tree_size = match file.status {
            Status::Deleted => None,
            _ => std::fs::symlink_metadata(root.join(&file.path))
                .ok()
                .filter(std::fs::Metadata::is_file)
                .map(|meta| meta.len()),
        };
        let large = base_size
            .max(tree_size)
            .is_some_and(|size| size > LARGE_FILE_BYTES);
        let readable = !file.binary && !large;
        let header = match generated {
            Some(_) => false,
            None if !readable => false,
            None if file.status == Status::Deleted => {
                if regular(&file.old_mode) {
                    deleted_heads.push((flagged.len(), file.old_id.clone()));
                }
                false
            }
            None => regular(&file.new_mode) && generated_header(&read_head(&root.join(&file.path))),
        };
        flagged.push(ChangedFile {
            flags: Flags {
                generated: generated.unwrap_or(header),
                lockfile: is_lockfile(&file.path),
                vendored: vendored.unwrap_or_else(|| is_vendored(&file.path)),
                large,
            },
            path: file.path,
            old_path: file.old_path,
            status: file.status,
            added: file.added,
            removed: file.removed,
            binary: file.binary,
        });
    }
    if !deleted_heads.is_empty() {
        let ids: Vec<&str> = deleted_heads.iter().map(|(_, id)| id.as_str()).collect();
        let contents = objects(root, &ids).await?;
        for ((at, _), content) in deleted_heads.iter().zip(contents) {
            flagged[*at].flags.generated = generated_header(&content);
        }
    }
    Ok(flagged)
}

/// Whether `path` is a lock file, by its name.
fn is_lockfile(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    LOCKFILES.contains(&name)
}

/// Whether a folder on `path` is one of [`VENDORED_FOLDERS`].
fn is_vendored(path: &str) -> bool {
    let mut folders = path.split('/').rev().skip(1);
    folders.any(|folder| VENDORED_FOLDERS.contains(&folder))
}

/// Whether the first lines of a file say it is generated.
fn generated_header(head: &[u8]) -> bool {
    let head = &head[..head.len().min(HEADER_BYTES)];
    String::from_utf8_lossy(head)
        .lines()
        .take(HEADER_LINES)
        .any(|line| line.contains("@generated") || line.contains("DO NOT EDIT"))
}

/// The first bytes of a file in the working tree; none when it cannot be
/// read.
fn read_head(path: &Path) -> Vec<u8> {
    use std::io::Read as _;
    let mut head = Vec::new();
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(HEADER_BYTES as u64).read_to_end(&mut head);
    }
    head
}

/// What `.gitattributes` says of each file's `linguist-generated` and
/// `linguist-vendored`: Some either way when it says, None when it does
/// not.
async fn attributes(
    cwd: &Path,
    files: &[RawFile],
) -> Result<std::collections::HashMap<String, (Option<bool>, Option<bool>)>, GitError> {
    let mut input = Vec::new();
    for file in files {
        input.extend_from_slice(file.path.as_bytes());
        input.push(0);
    }
    let output = checked_with_input(
        cwd,
        None,
        &[
            "check-attr",
            "-z",
            "--stdin",
            "linguist-generated",
            "linguist-vendored",
        ],
        input,
    )
    .await?;
    let mut said = std::collections::HashMap::<String, (Option<bool>, Option<bool>)>::new();
    let fields: Vec<_> = output.split(|byte| *byte == 0).collect();
    for [path, attribute, value] in fields.as_chunks::<3>().0 {
        let value = match *value {
            b"set" | b"true" => Some(true),
            b"unset" | b"false" => Some(false),
            _ => None,
        };
        let entry = said
            .entry(String::from_utf8_lossy(path).into_owned())
            .or_default();
        match *attribute {
            b"linguist-generated" => entry.0 = value,
            b"linguist-vendored" => entry.1 = value,
            _ => {}
        }
    }
    Ok(said)
}

/// The size of each file's base side, by its object id, for the files
/// that have one.
async fn base_sizes(
    cwd: &Path,
    files: &[RawFile],
) -> Result<std::collections::HashMap<String, u64>, GitError> {
    let mut input = String::new();
    for file in files {
        if file.status != Status::Created && file.old_mode.starts_with("100") {
            input.push_str(&file.old_id);
            input.push('\n');
        }
    }
    if input.is_empty() {
        return Ok(Default::default());
    }
    let output = checked_with_input(
        cwd,
        None,
        &["cat-file", "--batch-check=%(objectname) %(objectsize)"],
        input.into_bytes(),
    )
    .await?;
    Ok(text(&output)
        .lines()
        .filter_map(|line| {
            let (id, size) = line.split_once(' ')?;
            Some((id.to_owned(), size.parse().ok()?))
        })
        .collect())
}

/// The first bytes of each object in `ids`, in order, up to the header
/// window; empty for one git cannot find. Read as `cat-file --batch` writes
/// them, so only the window of each is ever held: the rest of an object is
/// dropped as it arrives.
async fn objects(cwd: &Path, ids: &[&str]) -> Result<Vec<Vec<u8>>, GitError> {
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _};

    let args = ["cat-file", "--batch"];
    let input: String = ids.iter().map(|id| format!("{id}\n")).collect();
    let mut child = command(cwd, None, &args)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(GitError::Spawn)?;
    let mut stdin = child.stdin.take().expect("standard input is piped");
    let mut stdout = tokio::io::BufReader::new(child.stdout.take().expect("output is piped"));
    let mut stderr = child.stderr.take().expect("errors are piped");
    // Written while the output is read, so neither side waits on a full
    // pipe; closing it when done is what lets git finish.
    let write = async move {
        let _ = stdin.write_all(input.as_bytes()).await;
    };
    let read = async {
        // Each object is `id type size LF`, its bytes, then LF; one missing
        // is `id missing LF`.
        let mut contents = Vec::with_capacity(ids.len());
        let mut header = Vec::new();
        while contents.len() < ids.len() {
            header.clear();
            if stdout.read_until(b'\n', &mut header).await? == 0 {
                break;
            }
            let header = String::from_utf8_lossy(&header);
            let header = header.trim_end();
            let size = header
                .rsplit(' ')
                .next()
                .and_then(|size| size.parse::<u64>().ok())
                .filter(|_| !header.ends_with(" missing"));
            let Some(size) = size else {
                contents.push(Vec::new());
                continue;
            };
            let window = size.min(HEADER_BYTES as u64);
            let mut head = Vec::with_capacity(window as usize);
            (&mut stdout).take(window).read_to_end(&mut head).await?;
            tokio::io::copy(
                &mut (&mut stdout).take(size - window + 1),
                &mut tokio::io::sink(),
            )
            .await?;
            contents.push(head);
        }
        // Drained, so git never waits on a pipe no one reads.
        tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await?;
        Ok::<_, std::io::Error>(contents)
    };
    let mut errors = Vec::new();
    let (_, read, _) = tokio::join!(write, read, stderr.read_to_end(&mut errors));
    let status = child.wait().await.map_err(GitError::Spawn)?;
    succeeded(
        &args,
        Output {
            status,
            stdout: Vec::new(),
            stderr: errors,
        },
    )?;
    let mut contents = read.map_err(GitError::Spawn)?;
    contents.resize(ids.len(), Vec::new());
    Ok(contents)
}

/// A git query's one-line answer, or None when git says no (exit status
/// non-zero), as `--quiet` lookups do for something that is not there.
async fn answer(cwd: &Path, args: &[&str]) -> Result<Option<String>, GitError> {
    let output = run(cwd, None, args, None).await?;
    Ok(output
        .status
        .success()
        .then(|| text(&output.stdout))
        .filter(|answer| !answer.is_empty()))
}

/// Standard output of a git command that must succeed.
async fn checked(cwd: &Path, index: Option<&Path>, args: &[&str]) -> Result<Vec<u8>, GitError> {
    succeeded(args, run(cwd, index, args, None).await?)
}

/// [`checked`], with `input` written to the command's standard input.
async fn checked_with_input(
    cwd: &Path,
    index: Option<&Path>,
    args: &[&str],
    input: Vec<u8>,
) -> Result<Vec<u8>, GitError> {
    succeeded(args, run(cwd, index, args, Some(input)).await?)
}

fn succeeded(args: &[&str], output: Output) -> Result<Vec<u8>, GitError> {
    if !output.status.success() {
        return Err(GitError::Failed {
            command: args
                .iter()
                .find(|arg| !arg.starts_with('-'))
                .copied()
                .unwrap_or_default()
                .to_owned(),
            stderr: text(&output.stderr),
        });
    }
    Ok(output.stdout)
}

async fn run(
    cwd: &Path,
    index: Option<&Path>,
    args: &[&str],
    input: Option<Vec<u8>>,
) -> Result<Output, GitError> {
    let mut command = command(cwd, index, args);
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let Some(input) = input else {
        return command.output().await.map_err(GitError::Spawn);
    };
    let mut child = command.spawn().map_err(GitError::Spawn)?;
    let mut stdin = child.stdin.take().expect("standard input is piped");
    // Written while the output is read, so neither side waits on a full
    // pipe; a git that stops reading early says why on its way out.
    let write = async move {
        let _ = stdin.write_all(&input).await;
    };
    let (_, output) = tokio::join!(write, child.wait_with_output());
    output.map_err(GitError::Spawn)
}

/// Git with `args` in `cwd`, on the scratch `index` when given: its output
/// and errors piped, nothing on its input, and killed if dropped.
fn command(cwd: &Path, index: Option<&Path>, args: &[&str]) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .kill_on_drop(true);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    command
}

/// Git run on the calling thread, for the callers that are on a thread of
/// their own rather than in a runtime.
mod blocking {
    use std::io::Write as _;
    use std::path::Path;
    use std::process::{Command, Output, Stdio};

    use super::{GitError, succeeded, text};

    /// [`super::answer`], blocking.
    pub(super) fn answer(cwd: &Path, args: &[&str]) -> Result<Option<String>, GitError> {
        let output = run(cwd, args, None)?;
        Ok(output
            .status
            .success()
            .then(|| text(&output.stdout))
            .filter(|answer| !answer.is_empty()))
    }

    /// [`super::checked`], blocking, with `input` on standard input.
    pub(super) fn checked(
        cwd: &Path,
        args: &[&str],
        input: Option<Vec<u8>>,
    ) -> Result<Vec<u8>, GitError> {
        succeeded(args, run(cwd, args, input)?)
    }

    fn run(cwd: &Path, args: &[&str], input: Option<Vec<u8>>) -> Result<Output, GitError> {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(cwd)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_OPTIONAL_LOCKS", "0");
        let mut child = command.spawn().map_err(GitError::Spawn)?;
        let Some(input) = input else {
            return child.wait_with_output().map_err(GitError::Spawn);
        };
        let mut stdin = child.stdin.take().expect("standard input is piped");
        // Written on a thread of its own while the output is read, so
        // neither side waits on a full pipe.
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let _ = stdin.write_all(&input);
            });
            child.wait_with_output().map_err(GitError::Spawn)
        })
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_way_of_writing_a_remote_names_one_repository() {
        for url in [
            "git@github.com:amuxhq/amux.git",
            "https://github.com/amuxhq/amux",
            "https://x-access-token:secret@github.com/amuxhq/amux.git/",
            "ssh://git@GitHub.com:22/amuxhq/amux.git",
            "git://github.com/amuxhq/amux",
        ] {
            assert_eq!(
                super::remote_identity(url),
                "github.com/amuxhq/amux",
                "{url}"
            );
        }
        assert_eq!(super::remote_identity("/srv/git/amux.git"), "/srv/git/amux");
        assert_eq!(
            super::remote_identity("file:///srv/git/amux"),
            "/srv/git/amux"
        );
        assert_eq!(
            super::remote_identity("https://gitlab.example.com/group/sub/tool.git"),
            "gitlab.example.com/group/sub/tool"
        );
        assert_eq!(
            super::remote_identity(r"C:\Users\ada\src\amux.git"),
            r"C:\Users\ada\src\amux"
        );
        assert_eq!(super::remote_identity("C:/src/amux"), "C:/src/amux");
    }

    /// Objects are read in order, each to the header window and no more,
    /// however large: one far past the window does not shift the next, an
    /// empty one and one git cannot find read empty, and a short one whole.
    #[tokio::test]
    async fn objects_are_read_to_the_header_window_in_order() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str], input: Option<&[u8]>| {
            use std::io::Write as _;
            let mut child = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            if let Some(input) = input {
                stdin.write_all(input).unwrap();
            }
            drop(stdin);
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "git {args:?}");
            super::text(&output.stdout)
        };
        git(&["init", "-q"], None);
        let blob = |fill: u8, size: usize, first: &[u8]| {
            let mut bytes = vec![fill; size];
            bytes[..first.len()].copy_from_slice(first);
            git(&["hash-object", "-w", "--stdin"], Some(&bytes))
        };
        let large = 3 * 1024 * 1024;
        let first = blob(b'a', large, b"// @generated\n");
        let second = blob(b'b', large + 1, b"plain\n");
        let empty = blob(b'c', 0, b"");
        let short = blob(b'd', 12, b"short");
        let missing = "0".repeat(first.len());
        let third = blob(b'e', large - 1, b"// DO NOT EDIT\n");
        let ids = [&first, &second, &empty, &missing, &short, &third];
        let ids: Vec<&str> = ids.iter().map(|id| id.as_str()).collect();

        let read = super::objects(repo.path(), &ids).await.unwrap();
        let lengths: Vec<_> = read.iter().map(Vec::len).collect();
        let window = super::HEADER_BYTES;
        assert_eq!(lengths, [window, window, 0, 0, 12, window]);
        assert!(read[0].starts_with(b"// @generated\n"));
        assert!(read[1].starts_with(b"plain\n") && read[1][6..].iter().all(|b| *b == b'b'));
        assert!(read[4].starts_with(b"short") && read[4][5..].iter().all(|b| *b == b'd'));
        assert!(read[5].starts_with(b"// DO NOT EDIT\n"));
        assert!(read[5][15..].iter().all(|b| *b == b'e'));
        let generated: Vec<_> = read
            .iter()
            .map(|head| super::generated_header(head))
            .collect();
        assert_eq!(generated, [true, false, false, false, false, true]);
    }

    /// The scratch index holds the repository's file names, so it is made
    /// where only this user can read it, whatever the index's own mode.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_scratch_index_is_private_to_this_user() {
        use std::os::unix::fs::PermissionsExt;
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap()
                .status;
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(repo.path().join("secret-plans.txt"), "x\n").unwrap();
        git(&["add", "secret-plans.txt"]);
        let own = repo.path().join(".git/index");
        std::fs::set_permissions(&own, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(repo.path().join("untracked.txt"), "y\n").unwrap();

        let scratch = super::Scratch::new(repo.path()).await.unwrap();
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(scratch.dir.path()), 0o700);
        assert!(scratch.index().is_file());
        assert_ne!(
            std::fs::read(&own).unwrap(),
            std::fs::read(scratch.index()).unwrap()
        );
    }
}
