//! Snapshots of a working tree, untracked files included, committed into an
//! [`ObjectStore`] without writing anything to the repository: taking one,
//! telling whether the tree has moved since one without writing objects,
//! and dropping those no longer kept.
//!
//! A snapshot starts from a copy of the person's index that keeps its
//! modification time (git rereads an entry written in the same second as
//! the index, judged by the index file's time), so the person's index is
//! never touched. The paths that differ from it are listed and measured
//! before anything is added: a file over the size cap is left out and
//! named, too many paths refuse the snapshot, and the rest are added to the
//! copy with `.gitignore` respected. Its tree is committed with HEAD as the
//! parent and a fixed identity, so no configuration of the person's is
//! needed and nothing personal is stored.
//!
//! Git LFS's filters never run (no git call in this crate runs them). They
//! would write the file's content into `.git/lfs`, and fail the snapshot
//! outright where LFS is configured but not installed. A file LFS tracks is stored as the pointer LFS would
//! commit for it instead, worked out here from the file.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::objects::{self, ObjectStore};
use crate::{GitError, LARGE_FILE_BYTES, Scratch, Status};

/// A snapshot as it was taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The snapshot's commit, by its full object id.
    pub commit: String,
    /// The paths left out of it for being over
    /// [`LARGE_FILE_BYTES`](crate::LARGE_FILE_BYTES), from the repository's
    /// top folder.
    pub omitted: Vec<String>,
    /// A merge or rebase was under way, so conflicted files hold their
    /// markers as content.
    pub merging: bool,
}

/// The most changed and untracked paths a snapshot takes: far past a change
/// a person reviews, and short of a build folder no ignore rule covers yet.
pub const MAX_PATHS: usize = 10_000;

/// How many times a snapshot is tried when a file it listed disappears
/// before git reads it, and how long it waits between tries.
pub const TRIES: u32 = 3;
pub const RETRY_AFTER: Duration = Duration::from_millis(50);

/// What a snapshot takes and how hard it tries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// A file over this many bytes is left out and named in
    /// [`Snapshot::omitted`].
    pub large_file_bytes: u64,
    /// More changed and untracked paths than this refuse the snapshot with
    /// [`GitError::TooManyPaths`].
    pub max_paths: usize,
    /// Tries before a file that keeps disappearing is reported as
    /// [`GitError::Vanished`].
    pub tries: u32,
    pub retry_after: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            large_file_bytes: LARGE_FILE_BYTES,
            max_paths: MAX_PATHS,
            tries: TRIES,
            retry_after: RETRY_AFTER,
        }
    }
}

/// The identity and message every snapshot is committed with.
const AUTHOR: &str = "amux";
const EMAIL: &str = "amux@amux.invalid";
const MESSAGE: &str = "amux review snapshot";

/// The mode git records a submodule or an embedded repository with.
const GITLINK: &str = "160000";

/// What LFS writes at the top of a pointer, naming the version of its spec.
const LFS_VERSION: &str = "version https://git-lfs.github.com/spec/v1";

/// Takes a snapshot of the working tree `cwd` is in, committed at
/// `taken_at`, into `store`. Retried after a pause when a file it listed
/// disappears before git reads it, then [`GitError::Vanished`].
pub async fn take(
    cwd: &Path,
    store: &ObjectStore,
    limits: &Limits,
    taken_at: SystemTime,
) -> Result<Snapshot, GitError> {
    take_with(cwd, store, limits, taken_at, &mut || {}).await
}

/// [`take`], calling `between` once each try after the paths are listed and
/// measured and before git reads them.
async fn take_with(
    cwd: &Path,
    store: &ObjectStore,
    limits: &Limits,
    taken_at: SystemTime,
    between: &mut (dyn FnMut() + Send),
) -> Result<Snapshot, GitError> {
    let repository = Repository::of(cwd).await?;
    std::fs::create_dir_all(store.dir()).map_err(GitError::Scratch)?;
    let mut tries = 0;
    loop {
        tries += 1;
        match attempt(&repository, store, limits, taken_at, between).await? {
            Attempt::Taken(snapshot) => return Ok(snapshot),
            Attempt::Vanished(path) if tries >= limits.tries.max(1) => {
                return Err(GitError::Vanished { path });
            }
            Attempt::Vanished(_) => tokio::time::sleep(limits.retry_after).await,
        }
    }
}

enum Attempt {
    Taken(Snapshot),
    /// A file listed was gone when git came to read it.
    Vanished(String),
}

/// What a snapshot needs of the repository.
struct Repository {
    /// The working tree's top folder, where every git call runs.
    root: PathBuf,
    /// This working tree's own git folder, where a merge or rebase leaves
    /// its state.
    git_dir: PathBuf,
    /// The repository's object directory.
    objects: PathBuf,
    /// The commit checked out; None before the first commit.
    head: Option<String>,
}

impl Repository {
    async fn of(cwd: &Path) -> Result<Self, GitError> {
        let output = crate::run(
            cwd,
            None,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--absolute-git-dir",
                "--git-path",
                "objects",
            ],
            None,
        )
        .await?;
        if !output.status.success() {
            return Err(GitError::NotARepository);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = text.lines().map(PathBuf::from);
        let (Some(root), Some(git_dir), Some(objects)) = (lines.next(), lines.next(), lines.next())
        else {
            return Err(GitError::NotARepository);
        };
        let head = crate::answer(
            &root,
            &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        )
        .await?;
        Ok(Self {
            root,
            git_dir,
            objects,
            head,
        })
    }

    /// Whether a merge, rebase, cherry-pick or revert is under way.
    fn merging(&self) -> bool {
        [
            "MERGE_HEAD",
            "REBASE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-merge",
            "rebase-apply",
        ]
        .iter()
        .any(|name| self.git_dir.join(name).exists())
    }
}

/// One try at a snapshot, on a fresh copy of the index.
async fn attempt(
    repository: &Repository,
    store: &ObjectStore,
    limits: &Limits,
    taken_at: SystemTime,
    between: &mut (dyn FnMut() + Send),
) -> Result<Attempt, GitError> {
    let root = repository.root.as_path();
    let scratch = Scratch::copied(root).await?;
    let index = scratch.index();
    let git = |args: &[&str]| store.writing(root, Some(&index), args);
    // Entries whose file was only touched read as unchanged, so they are
    // neither counted nor stored again. Its status says whether any entry
    // still differs, which the listing below finds anyway.
    objects::finish(git(&["update-index", "-q", "--refresh"]), None).await?;
    let changed =
        listed(objects::checked("ls-files", git(&["ls-files", "-z", "--modified"]), None).await?);
    let untracked = listed(
        objects::checked(
            "ls-files",
            git(&["ls-files", "-z", "--others", "--exclude-standard"]),
            None,
        )
        .await?,
    );
    let unmerged = unmerged(&git).await?;
    let count = changed.len() + untracked.len();
    if count > limits.max_paths {
        return Err(GitError::TooManyPaths {
            count,
            cap: limits.max_paths,
        });
    }
    let untracked = committed_repositories(root, untracked).await?;
    let lfs = lfs_paths(root, &index, changed.iter().chain(&untracked)).await?;

    let mut omitted = Vec::new();
    // What `git add` takes, and of those the ones there when measured.
    let mut added: Vec<&[u8]> = Vec::new();
    let mut present: Vec<&[u8]> = Vec::new();
    let mut pointed: Vec<(&[u8], std::fs::Metadata)> = Vec::new();
    for (path, tracked) in changed
        .iter()
        .map(|path| (path, true))
        .chain(untracked.iter().map(|path| (path, false)))
    {
        let meta = match std::fs::symlink_metadata(on_disk(root, path)) {
            Ok(meta) => meta,
            // A tracked file deleted: the add records its removal.
            Err(_) if tracked => {
                added.push(path);
                continue;
            }
            // An untracked file gone since it was listed is simply not there.
            Err(_) => continue,
        };
        if meta.is_file() && lfs.contains(path) {
            pointed.push((path, meta));
            continue;
        }
        if left_out(&meta, limits, false, unmerged.contains(path)) {
            omitted.push(lossy(path));
            continue;
        }
        added.push(path);
        present.push(path);
    }
    between();

    if !added.is_empty() {
        let output = objects::finish(
            git(&[
                "--literal-pathspecs",
                "add",
                "--all",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ]),
            Some(nul_separated(&added)),
        )
        .await?;
        if !output.status.success() {
            if let Some(gone) = present
                .iter()
                .find(|path| std::fs::symlink_metadata(on_disk(root, path)).is_err())
            {
                return Ok(Attempt::Vanished(lossy(gone)));
            }
            crate::succeeded(&["add"], output)?;
        }
    }
    if !pointed.is_empty() {
        let mut pointers = Vec::with_capacity(pointed.len());
        for (path, meta) in &pointed {
            match pointer(&on_disk(root, path)).await {
                Ok(pointer) => pointers.push((pointer, mode(meta))),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Attempt::Vanished(lossy(path)));
                }
                Err(error) => return Err(GitError::Scratch(error)),
            }
        }
        let ids = blob_ids(
            &scratch,
            |args| git(args),
            pointers.iter().map(|(pointer, _)| pointer.as_slice()),
            true,
        )
        .await?;
        let mut entries = Vec::new();
        for (((path, _), (_, mode)), id) in pointed.iter().zip(&pointers).zip(&ids) {
            entries.extend_from_slice(format!("{mode} {id}\t").as_bytes());
            entries.extend_from_slice(path);
            entries.push(0);
        }
        objects::checked(
            "update-index",
            git(&["update-index", "-z", "--index-info"]),
            Some(entries),
        )
        .await?;
    }

    // The trees of files the store does not hold are written all the same:
    // their blobs are in the repository, which this write does not see.
    let tree =
        text(&objects::checked("write-tree", git(&["write-tree", "--missing-ok"]), None).await?);
    let date = format!(
        "@{} +0000",
        taken_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
    let mut args = vec!["commit-tree", tree.as_str(), "--no-gpg-sign", "-m", MESSAGE];
    if let Some(head) = &repository.head {
        args.extend(["-p", head.as_str()]);
    }
    let mut commit = store.committing(root, &repository.objects, &args);
    commit
        .env("GIT_AUTHOR_NAME", AUTHOR)
        .env("GIT_AUTHOR_EMAIL", EMAIL)
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_NAME", AUTHOR)
        .env("GIT_COMMITTER_EMAIL", EMAIL)
        .env("GIT_COMMITTER_DATE", &date);
    let commit = text(&objects::checked("commit-tree", commit, None).await?);
    Ok(Attempt::Taken(Snapshot {
        commit,
        omitted,
        merging: repository.merging(),
    }))
}

/// Whether a file a snapshot measured is left out of it: a regular file
/// over the size cap, unless LFS tracks it (it is stored as its pointer) or
/// it is in conflict (its markers are what a merge under way needs seen).
fn left_out(meta: &std::fs::Metadata, limits: &Limits, lfs: bool, unmerged: bool) -> bool {
    meta.is_file() && meta.len() > limits.large_file_bytes && !lfs && !unmerged
}

/// The paths in conflict in the index `git` runs on.
async fn unmerged(
    git: &impl Fn(&[&str]) -> tokio::process::Command,
) -> Result<HashSet<Vec<u8>>, GitError> {
    Ok(
        objects::checked("ls-files", git(&["ls-files", "-z", "--unmerged"]), None)
            .await?
            .split(|byte| *byte == 0)
            .filter_map(|record| {
                let tab = record.iter().position(|byte| *byte == b'\t')?;
                Some(record[tab + 1..].to_vec())
            })
            .collect(),
    )
}

/// Whether the working tree `cwd` is in differs from `snapshot`, taken
/// with `limits`; untracked files count. A path the snapshot `omitted` has
/// moved only once a snapshot would no longer leave it out (it shrank under
/// the cap, went, or no longer differs from the index), since what it holds
/// is not in the snapshot to compare. Nothing is written to the repository
/// or the store: untracked files are marked in a copy of the index without
/// being stored, and the one object marking writes (the empty blob) goes to
/// a folder of its own that is then removed.
pub async fn moved(
    cwd: &Path,
    store: &ObjectStore,
    limits: &Limits,
    snapshot: &str,
    omitted: &[String],
) -> Result<bool, GitError> {
    let repository = Repository::of(cwd).await?;
    let root = repository.root.as_path();
    let scratch = Scratch::copied(root).await?;
    let index = scratch.index();
    let throwaway = ObjectStore::at(scratch.dir.path().join("objects"));
    std::fs::create_dir_all(throwaway.dir()).map_err(GitError::Scratch)?;
    let git = |args: &[&str]| throwaway.writing(root, Some(&index), args);
    objects::finish(git(&["update-index", "-q", "--refresh"]), None).await?;
    let untracked = committed_repositories(
        root,
        listed(
            objects::checked(
                "ls-files",
                git(&["ls-files", "-z", "--others", "--exclude-standard"]),
                None,
            )
            .await?,
        ),
    )
    .await?;
    if !omitted.is_empty()
        && !still_left_out(root, &index, &git, limits, omitted, &untracked).await?
    {
        return Ok(true);
    }
    if !untracked.is_empty() {
        let refs: Vec<&[u8]> = untracked.iter().map(Vec::as_slice).collect();
        objects::checked(
            "add",
            git(&[
                "--literal-pathspecs",
                "add",
                "--intent-to-add",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ]),
            Some(nul_separated(&refs)),
        )
        .await?;
    }
    let mut args = vec![
        "diff",
        "--raw",
        "-z",
        "--no-abbrev",
        "--no-renames",
        "--ignore-submodules=dirty",
    ];
    args.extend(crate::DIFF_OPTIONS);
    args.extend([snapshot, "--"]);
    let mut diff = crate::command(root, Some(&index), &args);
    diff.env("GIT_OBJECT_DIRECTORY", throwaway.dir()).env(
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        objects::alternates(&[&repository.objects, store.dir()]),
    );
    let raw = objects::checked("diff", diff, None).await?;
    let omitted: HashSet<&str> = omitted.iter().map(String::as_str).collect();
    let mut differ = Vec::new();
    for file in crate::raw_files(&raw) {
        if omitted.contains(file.path.as_str()) {
            continue;
        }
        // A submodule or embedded repository reads as differing whenever
        // the copy of the index does not hold its commit, as when it was
        // only marked; it has moved only when its checkout is not the
        // snapshot's commit.
        if file.status == Status::Changed && file.old_mode == GITLINK && file.new_mode == GITLINK {
            let inner = on_disk(root, file.path.as_bytes());
            let head = crate::answer(
                &inner,
                &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
            )
            .await?;
            if head.as_deref() == Some(file.old_id.as_str()) {
                continue;
            }
        }
        differ.push(file);
    }
    if differ.is_empty() {
        return Ok(false);
    }
    let paths: Vec<Vec<u8>> = differ
        .iter()
        .map(|file| file.path.as_bytes().to_vec())
        .collect();
    let lfs = lfs_paths(root, &index, paths.iter()).await?;
    // With LFS's filters off, git compares a file LFS tracks by its content
    // with the pointer the snapshot holds, so those differ always; what
    // counts is whether the file's pointer is still the snapshot's.
    let mut pointers = Vec::new();
    for file in &differ {
        if !lfs.contains(file.path.as_bytes()) || file.status == Status::Deleted {
            return Ok(true);
        }
        match std::fs::symlink_metadata(on_disk(root, file.path.as_bytes())) {
            Ok(meta) if meta.is_file() => {}
            _ => return Ok(true),
        }
        match pointer(&on_disk(root, file.path.as_bytes())).await {
            Ok(pointer) => pointers.push(pointer),
            Err(_) => return Ok(true),
        }
    }
    let ids = blob_ids(
        &scratch,
        |args| git(args),
        pointers.iter().map(Vec::as_slice),
        false,
    )
    .await?;
    Ok(differ.iter().zip(&ids).any(|(file, id)| file.old_id != *id))
}

/// Whether a snapshot taken now would leave out every path in `omitted`,
/// as [`attempt`] decides it: each still differs from the index `git` runs
/// on (refreshed) or is among the `untracked`, and is [`left_out`].
async fn still_left_out(
    root: &Path,
    index: &Path,
    git: &impl Fn(&[&str]) -> tokio::process::Command,
    limits: &Limits,
    omitted: &[String],
    untracked: &[Vec<u8>],
) -> Result<bool, GitError> {
    let changed =
        listed(objects::checked("ls-files", git(&["ls-files", "-z", "--modified"]), None).await?);
    // Named by the snapshot as text; found again by the bytes git lists.
    let listed: std::collections::HashMap<String, &Vec<u8>> = changed
        .iter()
        .chain(untracked)
        .map(|path| (lossy(path), path))
        .collect();
    let mut paths = Vec::with_capacity(omitted.len());
    for path in omitted {
        match listed.get(path) {
            Some(path) => paths.push(*path),
            None => return Ok(false),
        }
    }
    let lfs = lfs_paths(root, index, paths.iter().copied()).await?;
    let unmerged = unmerged(git).await?;
    Ok(paths.iter().all(|path| {
        std::fs::symlink_metadata(on_disk(root, path)).is_ok_and(|meta| {
            left_out(&meta, limits, lfs.contains(*path), unmerged.contains(*path))
        })
    }))
}

/// Removes from `store` every object no snapshot in `keep` reaches, using
/// the repository `cwd` is in for its object format. A commit in `keep`
/// the store does not hold keeps nothing. Answers how many objects went.
pub async fn drop<S: AsRef<str>>(
    cwd: &Path,
    store: &ObjectStore,
    keep: impl IntoIterator<Item = S>,
) -> Result<usize, GitError> {
    if !store.dir().is_dir() {
        return Ok(0);
    }
    let repository = Repository::of(cwd).await?;
    let root = repository.root.as_path();
    // The store alone: what it holds is what is walked, and the history
    // beneath each snapshot, in the repository, is never read.
    let git = |args: &[&str]| store.writing(root, None, args);
    let mut asked = String::new();
    for commit in keep {
        asked.push_str(commit.as_ref());
        asked.push('\n');
    }
    let mut reachable = HashSet::new();
    if !asked.is_empty() {
        let held = objects::checked(
            "cat-file",
            git(&["cat-file", "--batch-check=%(objectname) %(objecttype)"]),
            Some(asked.into_bytes()),
        )
        .await?;
        let held: String = text(&held)
            .lines()
            .filter_map(|line| line.strip_suffix(" commit"))
            .map(|id| format!("{id}\n"))
            .collect();
        if !held.is_empty() {
            let walked = objects::checked(
                "rev-list",
                git(&[
                    "rev-list",
                    "--objects",
                    "--no-walk",
                    "--missing=allow-any",
                    "--stdin",
                ]),
                Some(held.into_bytes()),
            )
            .await?;
            reachable.extend(
                text(&walked)
                    .lines()
                    .filter_map(|line| line.split(' ').next())
                    .map(str::to_owned),
            );
        }
    }
    let mut removed = 0;
    for fanout in std::fs::read_dir(store.dir()).map_err(GitError::Scratch)? {
        let fanout = fanout.map_err(GitError::Scratch)?;
        let prefix = fanout.file_name().to_string_lossy().into_owned();
        if prefix.len() != 2 || !is_hex(&prefix) || !fanout.path().is_dir() {
            continue;
        }
        for object in std::fs::read_dir(fanout.path()).map_err(GitError::Scratch)? {
            let object = object.map_err(GitError::Scratch)?;
            let rest = object.file_name().to_string_lossy().into_owned();
            // Git's own temporary files, perhaps of a write under way, are
            // not objects yet.
            if !is_hex(&rest) || reachable.contains(&format!("{prefix}{rest}")) {
                continue;
            }
            std::fs::remove_file(object.path()).map_err(GitError::Scratch)?;
            removed += 1;
        }
        // Emptied folders go too; one still holding an object stays.
        let _ = std::fs::remove_dir(fanout.path());
    }
    Ok(removed)
}

/// The paths `git ls-files -z` wrote, each once.
fn listed(output: Vec<u8>) -> Vec<Vec<u8>> {
    let mut seen = BTreeSet::new();
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .filter(|path| seen.insert(path.to_vec()))
        .map(<[u8]>::to_vec)
        .collect()
}

/// `untracked` without the repositories inside the working tree that have
/// no commit checked out: git can record an embedded repository only by
/// its commit, and fails the whole add on one without.
async fn committed_repositories(
    root: &Path,
    untracked: Vec<Vec<u8>>,
) -> Result<Vec<Vec<u8>>, GitError> {
    let mut kept = Vec::with_capacity(untracked.len());
    for path in untracked {
        if path.ends_with(b"/") {
            let inner = on_disk(root, &path);
            let committed = crate::answer(
                &inner,
                &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
            )
            .await?;
            if committed.is_none() {
                continue;
            }
        }
        kept.push(path);
    }
    Ok(kept)
}

/// Which of `paths` LFS's filter is set on by `.gitattributes`.
async fn lfs_paths<'p>(
    root: &Path,
    index: &Path,
    paths: impl Iterator<Item = &'p Vec<u8>>,
) -> Result<HashSet<Vec<u8>>, GitError> {
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path);
        input.push(0);
    }
    if input.is_empty() {
        return Ok(HashSet::new());
    }
    let output = objects::checked(
        "check-attr",
        crate::command(
            root,
            Some(index),
            &["check-attr", "-z", "--stdin", "filter"],
        ),
        Some(input),
    )
    .await?;
    let fields: Vec<&[u8]> = output.split(|byte| *byte == 0).collect();
    Ok(fields
        .as_chunks::<3>()
        .0
        .iter()
        .filter(|[_, _, value]| *value == b"lfs")
        .map(|[path, _, _]| path.to_vec())
        .collect())
}

/// The pointer LFS commits for the file at `path`: its spec's version, the
/// SHA-256 of the content and its size, each on a line; an empty file is
/// its own pointer. Read off the async workers, since the file may be large.
async fn pointer(path: &Path) -> std::io::Result<Vec<u8>> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        use std::fmt::Write as _;
        use std::io::Read as _;

        use sha2::Digest as _;
        let mut file = std::fs::File::open(&path)?;
        let mut hasher = sha2::Sha256::new();
        let mut buffer = vec![0; 64 * 1024];
        let mut size = 0u64;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            size += read as u64;
        }
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut oid = String::with_capacity(64);
        for byte in hasher.finalize() {
            let _ = write!(oid, "{byte:02x}");
        }
        Ok(format!("{LFS_VERSION}\noid sha256:{oid}\nsize {size}\n").into_bytes())
    })
    .await
    .map_err(std::io::Error::other)?
}

/// The blob ids of `contents`, in order, written into the object directory
/// `git` writes to when `write`.
async fn blob_ids<'c>(
    scratch: &Scratch<'_>,
    git: impl Fn(&[&str]) -> tokio::process::Command,
    contents: impl Iterator<Item = &'c [u8]>,
    write: bool,
) -> Result<Vec<String>, GitError> {
    let folder = scratch.dir.path().join("pointers");
    std::fs::create_dir_all(&folder).map_err(GitError::Scratch)?;
    let mut paths = String::new();
    for (n, content) in contents.enumerate() {
        let file = folder.join(n.to_string());
        std::fs::write(&file, content).map_err(GitError::Scratch)?;
        paths.push_str(&file.to_string_lossy());
        paths.push('\n');
    }
    let mut args = vec!["hash-object", "--no-filters", "--stdin-paths"];
    if write {
        args.push("-w");
    }
    let ids = objects::checked("hash-object", git(&args), Some(paths.into_bytes())).await?;
    Ok(text(&ids).lines().map(str::to_owned).collect())
}

/// The mode git records for a regular file.
fn mode(meta: &std::fs::Metadata) -> &'static str {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o111 != 0 {
            return "100755";
        }
    }
    let _ = meta;
    "100644"
}

fn nul_separated(paths: &[&[u8]]) -> Vec<u8> {
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path);
        input.push(0);
    }
    input
}

/// Where a path git names from the top folder is on disk.
fn on_disk(root: &Path, path: &[u8]) -> PathBuf {
    #[cfg(unix)]
    let path = <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(path).to_owned();
    #[cfg(not(unix))]
    let path = std::ffi::OsString::from(String::from_utf8_lossy(path).into_owned());
    root.join(path)
}

fn lossy(path: &[u8]) -> String {
    String::from_utf8_lossy(path).into_owned()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

fn is_hex(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::time::SystemTime;

    use super::{Limits, take_with};
    use crate::GitError;
    use crate::objects::ObjectStore;

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
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
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("kept.txt"), "kept\n").unwrap();
        git(dir.path(), &["add", "kept.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "start"]);
        dir
    }

    /// A file listed and measured, then gone before git reads it, fails
    /// that try; the next lists the folder again and succeeds without it.
    #[tokio::test]
    async fn a_file_gone_before_the_add_is_tried_again_without_it() {
        let repo = repository();
        let store_dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::at(store_dir.path().join("objects"));
        std::fs::write(repo.path().join("brief.txt"), "brief\n").unwrap();
        std::fs::write(repo.path().join("stays.txt"), "stays\n").unwrap();
        let mut tries = 0;
        let path = repo.path().to_owned();
        let snapshot = take_with(
            repo.path(),
            &store,
            &Limits::default(),
            SystemTime::now(),
            &mut || {
                tries += 1;
                let _ = std::fs::remove_file(path.join("brief.txt"));
            },
        )
        .await
        .unwrap();
        assert_eq!(tries, 2);
        let alternate = store.dir().to_str().unwrap();
        let files = Command::new("git")
            .args(["ls-tree", "-r", "--name-only", &snapshot.commit])
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", alternate)
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(files.stdout).unwrap(),
            "kept.txt\nstays.txt\n"
        );
    }

    /// A folder where a listed file is gone by the add on every try is
    /// reported, naming the file, after the tries the limits allow.
    #[tokio::test]
    async fn a_file_gone_on_every_try_is_reported() {
        let repo = repository();
        let store_dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::at(store_dir.path().join("objects"));
        std::fs::write(repo.path().join("gone-1.txt"), "1\n").unwrap();
        let mut tries = 0;
        let path = repo.path().to_owned();
        let error = take_with(
            repo.path(),
            &store,
            &Limits::default(),
            SystemTime::now(),
            &mut || {
                tries += 1;
                std::fs::remove_file(path.join(format!("gone-{tries}.txt"))).unwrap();
                std::fs::write(path.join(format!("gone-{}.txt", tries + 1)), "next\n").unwrap();
            },
        )
        .await
        .unwrap_err();
        assert_eq!(tries, 3);
        assert!(
            matches!(&error, GitError::Vanished { path } if path == "gone-3.txt"),
            "{error:?}"
        );
    }
}
