//! What an agent's folder says about its git repository: the branch, the
//! branch it is measured against, and how much changed. The one piece of git
//! code the agent process and the daemon share, including the worktree a new
//! agent can start in.
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
    /// uncommitted work counts; on the base branch itself that is the
    /// uncommitted work. None without a base or a commit, or when the
    /// branch shares no history with it.
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
        (Some(base), Some(head)) => match fork_point(cwd, base).await? {
            // On the base branch itself, or a branch that has not left
            // it, only the uncommitted work is on the branch.
            Some(fork) if fork == *head => uncommitted,
            Some(fork) => Some(totals(cwd, &fork).await?),
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

/// How many commits the branch `cwd` has checked out holds that no other
/// ref does, its own upstream and the stash aside: what deleting the branch
/// would lose. Zero exactly when its tip is reachable from another ref,
/// which is when amux deletes a workspace's branch with it. None on a
/// detached head, or before the first commit.
pub async fn own_commits(cwd: &Path) -> Result<Option<u32>, GitError> {
    let Some(branch) = answer(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await? else {
        return Ok(None);
    };
    let own = format!("refs/heads/{branch}");
    if answer(cwd, &["rev-parse", "--verify", "--quiet", &own])
        .await?
        .is_none()
    {
        return Ok(None);
    }
    let upstream = answer(cwd, &["for-each-ref", "--format=%(upstream)", &own]).await?;
    let refs = checked(cwd, None, &["for-each-ref", "--format=%(refname)"]).await?;
    // Every other ref, negated, read from standard input: a repository may
    // have more refs than an argument list holds.
    let others: String = text(&refs)
        .lines()
        .filter(|name| *name != own && Some(*name) != upstream.as_deref() && *name != "refs/stash")
        .map(|name| format!("^{name}\n"))
        .collect();
    let count = checked_with_input(
        cwd,
        None,
        &["rev-list", "--count", &own, "--stdin"],
        others.into_bytes(),
    )
    .await?;
    Ok(text(&count).parse().ok())
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

/// The ref a base branch is read from: the local branch, else origin's.
async fn branch_ref(cwd: &Path, branch: &str) -> Result<Option<String>, GitError> {
    for name in [
        format!("refs/heads/{branch}"),
        format!("refs/remotes/origin/{branch}"),
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
        // permissions with it.
        match std::fs::File::open(&own) {
            Ok(mut from) => {
                let mut to =
                    std::fs::File::create_new(dir.path().join(INDEX)).map_err(GitError::Scratch)?;
                std::io::copy(&mut from, &mut to).map_err(GitError::Scratch)?;
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

    /// Each file that differs between `against` and the working tree.
    async fn files(&self, against: &str) -> Result<Vec<FileChange>, GitError> {
        let raw = self
            .checked(&[
                "diff",
                "--raw",
                "--numstat",
                "-z",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                against,
                "--",
            ])
            .await?;
        Ok(file_changes(&raw))
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

/// Reads `git diff --raw --numstat -z --no-renames`: first a raw record per
/// file (`:modes ids STATUS NUL path NUL`), then a numstat record per file
/// in the same order (`added TAB removed TAB path NUL`, `-` for both counts
/// on a binary file).
fn file_changes(output: &[u8]) -> Vec<FileChange> {
    let mut fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut changes: Vec<Change> = Vec::new();
    let mut files = Vec::new();
    while let Some(field) = fields.next() {
        let field = String::from_utf8_lossy(field);
        if let Some(raw) = field.strip_prefix(':') {
            let _path = fields.next();
            changes.push(match raw.rsplit(' ').next() {
                Some("A") => Change::Created,
                Some("D") => Change::Deleted,
                _ => Change::Changed,
            });
            continue;
        }
        let mut parts = field.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let binary = added == "-" && removed == "-";
        files.push(FileChange {
            path: path.to_owned(),
            added: added.parse().unwrap_or(0),
            removed: removed.parse().unwrap_or(0),
            change: changes.get(files.len()).copied().unwrap_or(Change::Changed),
            binary,
        });
    }
    files
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
    let mut command = tokio::process::Command::new("git");
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
        .env("GIT_OPTIONAL_LOCKS", "0")
        .kill_on_drop(true);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
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
