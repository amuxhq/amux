//! Comparisons between the commits a review names: its snapshots, the base
//! commits they are shown against, and the commits beneath them. Every read
//! runs git in the repository with the review's
//! [`ObjectStore`](crate::objects::ObjectStore) as an alternate, so a
//! snapshot and the history beneath it read as one.
//!
//! What is here: the review's base, by the rule [`changes`](crate::changes)
//! uses; the files and the numbered lines that differ between two commits,
//! renames followed; the line map that carries a comment from the commit
//! that holds its lines to a later one; and "since your last review",
//! limited to what is part of the agent's change.
//!
//! A commit can be gone: the repository can be cloned again, and a rebase
//! followed by garbage collection removes what only an old snapshot still
//! names. Reads then fail with [`CompareError::Missing`], for the caller to
//! fall back to what it stored.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::objects::{ObjectStore, finish};
use crate::{
    ChangedFile, GitError, Later, RawFile, Status, UsedBase, answer, branch_base, default_branch,
    empty_tree as empty_tree_in, flagged, raw_files, succeeded, text,
};

#[derive(Debug, thiserror::Error)]
pub enum CompareError {
    /// The commit (or tree) named, or something it holds, is in neither the
    /// repository nor the snapshot store.
    #[error("{0} can no longer be read: it, or a file it holds, is gone")]
    Missing(String),
    #[error(transparent)]
    Git(#[from] GitError),
}

/// Lines of a file, from 1, both ends included. A range starting at zero
/// stands for the whole file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl LineRange {
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// The whole file.
    pub fn file() -> Self {
        Self { start: 0, end: 0 }
    }

    pub fn is_file(&self) -> bool {
        self.start == 0
    }

    /// A range of lines with `lines` more on each side, never before line 1.
    pub fn widened(&self, lines: u32) -> Self {
        Self {
            start: self.start.saturating_sub(lines).max(1),
            end: self.end.saturating_add(lines),
        }
    }
}

/// The base a review's "all changes" runs from: the rule [`changes`] uses
/// for [`Base::Default`], with the base branch `chosen_branch` when the
/// person picked one, else `recorded_branch`, else the repository's
/// default branch.
///
/// Wherever that rule falls back to HEAD (on the base branch with no
/// upstream, or with no base branch to be found) the review runs from
/// `began_at`, the commit HEAD was on when it began, so the agent's later
/// commits stay in "all changes". `began_at` is None for a review begun
/// before the first commit, which then runs from the empty tree.
///
/// [`changes`]: crate::changes
/// [`Base::Default`]: crate::Base::Default
pub async fn base_for_review(
    cwd: &Path,
    recorded_branch: Option<&str>,
    chosen_branch: Option<&str>,
    began_at: Option<&str>,
) -> Result<UsedBase, GitError> {
    let root = top(cwd).await?;
    let cwd = root.as_path();
    let head = answer(cwd, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).await?;
    let branch = match chosen_branch.or(recorded_branch) {
        Some(branch) => Some(branch.to_owned()),
        None => default_branch(cwd).await?,
    };
    let began = || began_at.map(str::to_owned);
    let (Some(branch), Some(_)) = (&branch, &head) else {
        return Ok(UsedBase {
            branch,
            upstream: None,
            commit: began(),
        });
    };
    let mut used = branch_base(cwd, branch)
        .await?
        .ok_or_else(|| GitError::NoForkPoint(branch.clone()))?;
    // `branch_base` runs from HEAD exactly when the base branch itself is
    // checked out and has no upstream to run from.
    let checked_out = answer(cwd, &["symbolic-ref", "--quiet", "HEAD"]).await?;
    if used.upstream.is_none() && checked_out.as_deref() == Some(&format!("refs/heads/{branch}")) {
        used.commit = began();
    }
    Ok(used)
}

/// The empty tree's id in the repository `cwd` is in: what to compare from
/// when a base is no commit, before the first one.
pub async fn empty_tree(cwd: &Path) -> Result<String, GitError> {
    empty_tree_in(cwd).await
}

/// Each file that differs from `from` to `to` (commits or trees), renames
/// found, with the flags, counts and binary mark [`changes`] gives the
/// working tree's files. Paths are from the repository's top folder.
///
/// [`changes`]: crate::changes
pub async fn files(
    cwd: &Path,
    store: &ObjectStore,
    from: &str,
    to: &str,
) -> Result<Vec<ChangedFile>, CompareError> {
    let root = top(cwd).await?;
    let raw = raw(&root, store, from, to, None).await?;
    match flagged(&root, Later::Commit(store, to), raw).await {
        Ok(files) => Ok(files),
        Err(error) => Err(explained(&root, store, &[from, to], error).await),
    }
}

/// One line of a hunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// The line's number in `from`, from 1; zero for an added line.
    pub old_line: u32,
    /// The line's number in `to`, from 1; zero for a removed line.
    pub new_line: u32,
    /// Without its line ending.
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// One hunk of a unified diff, its lines numbered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// The first line of each side, from 1, and how many lines each side
    /// has; a side with none starts after line `start`, as git writes it.
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<DiffLine>,
}

/// One file's hunks between two commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHunks {
    /// At `to`; for a deleted file, at `from`.
    pub path: String,
    /// At `from`, for a renamed file.
    pub old_path: Option<String>,
    pub status: Status,
    /// A binary file has no hunks.
    pub binary: bool,
    /// The file's object on each side, None where it is not there: what a
    /// read of the whole file, and what is worked out from it, is kept by.
    pub old_blob: Option<String>,
    pub new_blob: Option<String>,
    pub hunks: Vec<Hunk>,
}

impl FileHunks {
    /// The lines its hunks add and remove; none for a binary file.
    pub fn counts(&self) -> (u32, u32) {
        let lines = self.hunks.iter().flat_map(|hunk| &hunk.lines);
        lines.fold((0, 0), |(added, removed), line| match line.kind {
            LineKind::Added => (added + 1, removed),
            LineKind::Removed => (added, removed + 1),
            LineKind::Context => (added, removed),
        })
    }
}

/// The hunks of the files `paths` from `from` to `to`, with `context` lines
/// around each change, renames followed. A file is asked for by its path at
/// `to`, or by its path at `from`: a deleted one, or a renamed one, which is
/// answered under its path at `to`. Files that did not change are left out.
pub async fn hunks(
    cwd: &Path,
    store: &ObjectStore,
    from: &str,
    to: &str,
    paths: &[&str],
    context: u32,
) -> Result<Vec<FileHunks>, CompareError> {
    let root = top(cwd).await?;
    let wanted: HashSet<&str> = paths.iter().copied().collect();
    let diffed = diffed(&root, store, from, to, context, |file| {
        wanted.contains(file.path.as_str())
            || file
                .old_path
                .as_deref()
                .is_some_and(|old| wanted.contains(old))
    })
    .await?;
    Ok(diffed
        .into_iter()
        .map(|(file, hunks)| file_hunks(file, hunks))
        .collect())
}

/// The bytes of `path` at `commit`; None when the commit has no file there
/// (a folder or a submodule included).
pub async fn read(
    cwd: &Path,
    store: &ObjectStore,
    commit: &str,
    path: &str,
) -> Result<Option<Vec<u8>>, CompareError> {
    let root = top(cwd).await?;
    let names = [format!("{commit}^{{tree}}"), format!("{commit}:{path}")];
    let mut read = batch(&root, store, &names).await?.into_iter();
    let (tree, file) = (read.next().flatten(), read.next().flatten());
    if tree.is_none() {
        return Err(CompareError::Missing(commit.to_owned()));
    }
    match file {
        Some((kind, bytes)) => Ok((kind == "blob").then_some(bytes)),
        // No file there, a submodule (its commit is another repository's),
        // or a file whose object is gone: the tree says which.
        None => {
            let listed = store
                .reading(&root, &ls_tree(commit, path))
                .output()
                .await
                .map_err(GitError::Spawn)?;
            let submodule = listed.stdout.starts_with(b"160000 ");
            match listed.status.success() && (listed.stdout.is_empty() || submodule) {
                true => Ok(None),
                false => Err(CompareError::Missing(commit.to_owned())),
            }
        }
    }
}

/// The first `bytes` bytes of each file `paths` at `commit`, in order, read
/// with one git process for them all; None for a path the commit holds no
/// file at (a folder or a submodule), or one git cannot be asked for in its
/// batch (a name holding a line break). What a file's `#!` line is read
/// from, for every changed file at once.
pub async fn heads(
    cwd: &Path,
    store: &ObjectStore,
    commit: &str,
    paths: &[&str],
    bytes: usize,
) -> Result<Vec<Option<Vec<u8>>>, CompareError> {
    let askable: Vec<&str> = paths
        .iter()
        .copied()
        .filter(|path| !path.contains(['\n', '\r']))
        .collect();
    if askable.is_empty() {
        return Ok(vec![None; paths.len()]);
    }
    let root = top(cwd).await?;
    let names: Vec<String> = std::iter::once(format!("{commit}^{{tree}}"))
        .chain(askable.iter().map(|path| format!("{commit}:{path}")))
        .collect();
    let mut read = batch(&root, store, &names).await?.into_iter();
    if read.next().flatten().is_none() {
        return Err(CompareError::Missing(commit.to_owned()));
    }
    let mut found: HashMap<&str, Vec<u8>> = HashMap::new();
    for (path, object) in askable.iter().zip(read) {
        if let Some((kind, mut head)) = object
            && kind == "blob"
        {
            head.truncate(bytes);
            found.insert(path, head);
        }
    }
    Ok(paths.iter().map(|path| found.remove(path)).collect())
}

/// The objects `ids`, whole, in order; None for one git cannot find.
pub async fn blobs(
    cwd: &Path,
    store: &ObjectStore,
    ids: &[&str],
) -> Result<Vec<Option<Vec<u8>>>, CompareError> {
    let root = top(cwd).await?;
    let names: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    Ok(batch(&root, store, &names)
        .await?
        .into_iter()
        .map(|object| object.map(|(_, bytes)| bytes))
        .collect())
}

/// One line of a file, numbered from 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumberedLine {
    pub number: u32,
    /// Without its line ending.
    pub text: String,
}

/// The lines of `path` at `commit` that `ranges` cover, in order, each once;
/// a range past the end stops there. None when the commit has no file
/// there. Read from the commit, never the working tree.
pub async fn lines(
    cwd: &Path,
    store: &ObjectStore,
    commit: &str,
    path: &str,
    ranges: &[LineRange],
) -> Result<Option<Vec<NumberedLine>>, CompareError> {
    let Some(bytes) = read(cwd, store, commit, path).await? else {
        return Ok(None);
    };
    let wanted = |number: u32| {
        ranges
            .iter()
            .any(|range| range.is_file() || (range.start..=range.end).contains(&number))
    };
    Ok(Some(
        split_lines(&bytes)
            .zip(1..)
            .filter(|(_, number)| wanted(*number))
            .map(|(line, number)| NumberedLine {
                number,
                text: line_text(line),
            })
            .collect(),
    ))
}

/// Where a range of lines in one commit is in a later one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mapped {
    /// No change touched the lines; here they are now. The whole file maps
    /// to the whole file.
    Untouched(LineRange),
    /// A change edited or deleted one of the lines, or put lines between
    /// them.
    Touched,
    /// The file is not in the later commit.
    Gone,
}

/// How one file's lines moved from one commit to a later one: one diff,
/// renames followed, as GitHub places review comments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineMap {
    /// The file's path in the later commit; None when it is gone.
    path: Option<String>,
    /// The changes, in order, as `-U0` hunks give them.
    edits: Vec<Edit>,
    /// The file changed in a way with no lines to follow: binary content,
    /// or a change of kind (a file become a link).
    opaque: bool,
}

/// One change between the two sides, as a zero-context hunk writes it: a
/// side with no lines starts after line `start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Edit {
    old_start: u32,
    old_count: u32,
    new_start: u32,
    new_count: u32,
}

impl LineMap {
    /// How `path`, a file of `from`, moved by `to`.
    pub async fn between(
        cwd: &Path,
        store: &ObjectStore,
        from: &str,
        to: &str,
        path: &str,
    ) -> Result<Self, CompareError> {
        let mut maps = Self::for_paths(cwd, store, from, to, &[path]).await?;
        Ok(maps.remove(path).expect("every path asked for is mapped"))
    }

    /// [`between`](Self::between) for each of `paths`, by its path in
    /// `from`, from one diff.
    pub async fn for_paths(
        cwd: &Path,
        store: &ObjectStore,
        from: &str,
        to: &str,
        paths: &[&str],
    ) -> Result<HashMap<String, Self>, CompareError> {
        let root = top(cwd).await?;
        let wanted: HashSet<&str> = paths.iter().copied().collect();
        let at_from = |file: &RawFile| file.old_path.as_deref().unwrap_or(&file.path).to_owned();
        let diffed = diffed(&root, store, from, to, 0, |file| {
            // A path made at `to` was not at `from` to be followed.
            file.status != Status::Created && wanted.contains(at_from(file).as_str())
        })
        .await?;
        let mut maps = HashMap::new();
        for (file, hunks) in diffed {
            let source = at_from(&file);
            if file.status == Status::Created || !wanted.contains(source.as_str()) {
                continue;
            }
            let map = match file.status {
                Status::Deleted => Self::gone(),
                _ => Self {
                    opaque: (file.binary && file.old_id != file.new_id)
                        || kind(&file.old_mode) != kind(&file.new_mode),
                    edits: hunks
                        .iter()
                        .map(|hunk| Edit {
                            old_start: hunk.old_start,
                            old_count: hunk.old_count,
                            new_start: hunk.new_start,
                            new_count: hunk.new_count,
                        })
                        .collect(),
                    path: Some(file.path),
                },
            };
            maps.insert(source, map);
        }
        // What no change named is where it was, if it was there at all.
        let unchanged: Vec<&str> = paths
            .iter()
            .copied()
            .filter(|path| !maps.contains_key(*path))
            .collect();
        if !unchanged.is_empty() {
            let present = present(&root, store, to, &unchanged).await?;
            for path in unchanged {
                let map = match present.contains(path) {
                    true => Self {
                        path: Some(path.to_owned()),
                        edits: Vec::new(),
                        opaque: false,
                    },
                    false => Self::gone(),
                };
                maps.insert(path.to_owned(), map);
            }
        }
        Ok(maps)
    }

    fn gone() -> Self {
        Self {
            path: None,
            edits: Vec::new(),
            opaque: false,
        }
    }

    /// The file's path in the later commit; None when it is gone.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// Where `lines` are in the later commit.
    pub fn map(&self, lines: LineRange) -> Mapped {
        if self.path.is_none() {
            return Mapped::Gone;
        }
        if lines.is_file() {
            return Mapped::Untouched(lines);
        }
        if self.opaque {
            return Mapped::Touched;
        }
        let (first, last) = (lines.start, lines.end.max(lines.start));
        let mut shift: i64 = 0;
        for edit in &self.edits {
            match edit.old_count {
                // Lines put in after line `old_start`: between the range's
                // lines touches it, before it moves it.
                0 if edit.old_start >= last => break,
                0 if edit.old_start >= first => return Mapped::Touched,
                0 => {}
                _ if edit.old_start > last => break,
                _ if edit.old_start + edit.old_count > first => return Mapped::Touched,
                _ => {}
            }
            shift += i64::from(edit.new_count) - i64::from(edit.old_count);
        }
        let moved = |line: u32| u32::try_from(i64::from(line) + shift).unwrap_or(0);
        Mapped::Untouched(LineRange::new(moved(first), moved(last)))
    }

    /// Whether any change touched `lines`, or the file is gone: what says a
    /// definition, or the lines around a comment, changed.
    pub fn touches(&self, lines: LineRange) -> bool {
        !matches!(self.map(lines), Mapped::Untouched(_))
    }

    /// The lines of the later commit that a change wrote, in order. Lines
    /// only deleted leave none.
    pub fn changed(&self) -> Vec<LineRange> {
        self.edits
            .iter()
            .filter(|edit| edit.new_count > 0)
            .map(|edit| LineRange::new(edit.new_start, edit.new_start + edit.new_count - 1))
            .collect()
    }
}

/// A snapshot a review showed and the base commit its "all changes" ran
/// from (the empty tree's id before the first commit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown<'a> {
    pub base: &'a str,
    pub snapshot: &'a str,
}

/// "Since your last review": the hunks from the `last` round's snapshot to
/// `now`'s, with `context` lines around each change, limited to what is
/// part of the agent's change. `paths` are files at `now`'s snapshot (a
/// deleted one by its path at the last round's); None asks for every file
/// that differs, so a list of files and their counts can come from the
/// same comparison as their hunks.
///
/// After a merge or rebase from a moved base, what arrived from the base
/// differs from the last round but is no part of the change, so it goes. A
/// hunk stays when one of its changes sits on the lines of a change the
/// agent's work holds now (`now`'s all changes), or on the lines of one it
/// held at the last round (that round's all changes): the agent undoing its
/// own change, a line put back or a file it made and then deleted, is
/// shown, though all changes no longer lists it. A file stays when either
/// lists it; one whose hunks all go goes with them, and one either lists
/// with nothing to compare since the round (a binary file, a rename alone)
/// stays whole. A file the agent only renamed changed no line, so no hunk
/// of it since the round is the agent's; one renamed since the round whose
/// hunks all go stays as the rename alone.
pub async fn since_last(
    cwd: &Path,
    store: &ObjectStore,
    last: Shown<'_>,
    now: Shown<'_>,
    paths: Option<&[&str]>,
    context: u32,
) -> Result<Vec<FileHunks>, CompareError> {
    let root = top(cwd).await?;
    let wanted: Option<HashSet<&str>> = paths.map(|paths| paths.iter().copied().collect());
    let asked = |file: &RawFile| {
        wanted
            .as_ref()
            .is_none_or(|wanted| wanted.contains(file.path.as_str()))
    };
    let since: Vec<FileHunks> = diffed(&root, store, last.snapshot, now.snapshot, context, asked)
        .await?
        .into_iter()
        .map(|(file, hunks)| file_hunks(file, hunks))
        .collect();
    if since.is_empty() {
        return Ok(since);
    }
    // Each file as the agent's work holds it now, by its path now, and as
    // it held it at the last round, by its path then.
    let at_now: HashSet<&str> = since.iter().map(|file| file.path.as_str()).collect();
    let at_last: HashSet<&str> = since.iter().map(old_path).collect();
    let all = diffed(&root, store, now.base, now.snapshot, context, |file| {
        at_now.contains(file.path.as_str())
    })
    .await?;
    let then = diffed(&root, store, last.base, last.snapshot, context, |file| {
        at_last.contains(file.path.as_str())
    })
    .await?;
    Ok(limited(since, &all, &then))
}

/// [`since_last`]'s rule over the three comparisons: `since` from the last
/// round's snapshot to now's, `all` from now's base to now's snapshot, and
/// `then` from the last round's base to its snapshot.
fn limited(
    since: Vec<FileHunks>,
    all: &[(RawFile, Vec<Hunk>)],
    then: &[(RawFile, Vec<Hunk>)],
) -> Vec<FileHunks> {
    let by_path = |files: &'_ [(RawFile, Vec<Hunk>)]| -> HashMap<String, Vec<(u32, u32)>> {
        files
            .iter()
            .map(|(file, hunks)| {
                let spans = hunks
                    .iter()
                    .flat_map(|hunk| change_spans(hunk, Side::New))
                    .collect();
                (file.path.clone(), spans)
            })
            .collect()
    };
    let (all, then) = (by_path(all), by_path(then));
    let overlaps = |spans: &[(u32, u32)], (first, last): (u32, u32)| {
        spans
            .iter()
            .any(|&(start, end)| first <= end && start <= last)
    };
    since
        .into_iter()
        .filter_map(|mut file| {
            let now = all.get(file.path.as_str());
            let held = then.get(old_path(&file));
            if now.is_none() && held.is_none() {
                return None;
            }
            // Nothing to compare (a binary file, a rename alone) stays
            // whole. A side that changed no line (the agent only renamed the
            // file) holds no change for a hunk to sit on, so it keeps none:
            // a hunk such a file has since the round came from elsewhere.
            if file.hunks.is_empty() {
                return Some(file);
            }
            let (now, held) = (
                now.map_or(&[][..], Vec::as_slice),
                held.map_or(&[][..], Vec::as_slice),
            );
            file.hunks.retain(|hunk| {
                change_spans(hunk, Side::New).any(|span| overlaps(now, span))
                    || change_spans(hunk, Side::Old).any(|span| overlaps(held, span))
            });
            // A file renamed since the round stays, as the rename alone,
            // when every line that changed in it came from elsewhere.
            (!file.hunks.is_empty() || file.old_path.is_some()).then_some(file)
        })
        .collect()
}

/// A file's path at the earlier side of its comparison.
fn old_path(file: &FileHunks) -> &str {
    file.old_path.as_deref().unwrap_or(&file.path)
}

/// A side of a comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Old,
    New,
}

/// Where each run of changed lines in `hunk` sits on `side`, both ends
/// included: the lines it wrote there (added on the new side, removed on
/// the old), or for a run with none on that side, the lines either side of
/// where they would be.
fn change_spans(hunk: &Hunk, side: Side) -> impl Iterator<Item = (u32, u32)> + '_ {
    let (start, count, own) = match side {
        Side::Old => (hunk.old_start, hunk.old_count, LineKind::Removed),
        Side::New => (hunk.new_start, hunk.new_count, LineKind::Added),
    };
    let number = |line: &DiffLine| match side {
        Side::Old => line.old_line,
        Side::New => line.new_line,
    };
    let mut spans = Vec::new();
    // The side's last line before the run, and the run's own lines so far.
    // A side with no lines in the hunk starts after line `start`.
    let mut before = match count {
        0 => start,
        _ => start.saturating_sub(1),
    };
    let mut run: Option<Option<(u32, u32)>> = None;
    let mut close = |run: &mut Option<Option<(u32, u32)>>, before: u32| {
        if let Some(lines) = run.take() {
            spans.push(lines.unwrap_or((before, before + 1)));
        }
    };
    for line in &hunk.lines {
        if line.kind == LineKind::Context {
            close(&mut run, before);
            before = number(line);
            continue;
        }
        let lines = run.get_or_insert(None);
        if line.kind == own {
            let at = number(line);
            *lines = Some(match *lines {
                Some((first, _)) => (first, at),
                None => (at, at),
            });
        }
    }
    close(&mut run, before);
    spans.into_iter()
}

/// The repository's top folder, where git names every file from and where
/// a path given to git is read from.
async fn top(cwd: &Path) -> Result<PathBuf, GitError> {
    answer(cwd, &["rev-parse", "--show-toplevel"])
        .await?
        .map(PathBuf::from)
        .ok_or(GitError::NotARepository)
}

/// The files that differ from `from` to `to`, limited to `paths` when
/// given, as `git diff --raw --numstat` lists them.
async fn raw(
    root: &Path,
    store: &ObjectStore,
    from: &str,
    to: &str,
    paths: Option<&[&str]>,
) -> Result<Vec<RawFile>, CompareError> {
    let mut args = vec![
        "--literal-pathspecs",
        "diff",
        "--raw",
        "--numstat",
        "-z",
        "--no-abbrev",
        "--find-renames",
    ];
    // Nothing the person's configuration adds that would change what is
    // read, here or in the patch below.
    args.extend(crate::DIFF_OPTIONS);
    args.extend([from, to, "--"]);
    args.extend(paths.unwrap_or_default());
    let listed = read_git(root, store, &[from, to], &args).await?;
    Ok(raw_files(&listed))
}

/// Most bytes of paths one git call is given as arguments: far inside what
/// every platform takes on a command line (Windows' 32,767 characters the
/// least), so a comparison of thousands of long paths is read in several
/// calls rather than refused by the system.
pub(crate) const PATHSPEC_BYTES: usize = 16 * 1024;

/// `paths` in runs whose bytes, one separator each, stay within
/// [`PATHSPEC_BYTES`]; a path longer than that alone is a run of its own.
/// Each item's paths stay in one run.
fn runs<'a, T>(items: &'a [T], paths: impl Fn(&'a T) -> Vec<&'a str>) -> Vec<Vec<&'a T>> {
    let mut runs: Vec<Vec<&T>> = Vec::new();
    let mut bytes = 0;
    for item in items {
        let size: usize = paths(item).iter().map(|path| path.len() + 1).sum();
        match runs.last_mut() {
            Some(run) if bytes + size <= PATHSPEC_BYTES => {
                run.push(item);
                bytes += size;
            }
            _ => {
                runs.push(vec![item]);
                bytes = size;
            }
        }
    }
    runs
}

/// The files `keep` picks from those that differ from `from` to `to`, each
/// with its hunks at `context` lines, in git's order: the list first, over
/// the whole tree so a renamed file is found by either path, then the patch
/// of just those files. When every file is picked the patch is the whole
/// tree's, naming no path; otherwise it names both paths of each picked
/// file, in calls of at most [`PATHSPEC_BYTES`] of them.
async fn diffed(
    root: &Path,
    store: &ObjectStore,
    from: &str,
    to: &str,
    context: u32,
    keep: impl Fn(&RawFile) -> bool,
) -> Result<Vec<(RawFile, Vec<Hunk>)>, CompareError> {
    let listed = raw(root, store, from, to, None).await?;
    let every = listed.len();
    let picked: Vec<RawFile> = listed.into_iter().filter(|file| keep(file)).collect();
    if picked.is_empty() {
        return Ok(Vec::new());
    }
    if picked.len() == every {
        let sections = patch(root, store, from, to, context, &[]).await?;
        return paired(picked, sections);
    }
    let mut found: HashMap<String, (RawFile, Vec<Hunk>)> = HashMap::new();
    for run in runs(&picked, |file| {
        std::iter::once(file.path.as_str())
            .chain(file.old_path.as_deref())
            .collect()
    }) {
        let mut pathspec: Vec<&str> = Vec::new();
        for file in &run {
            pathspec.push(&file.path);
            pathspec.extend(file.old_path.as_deref());
        }
        pathspec.sort_unstable();
        pathspec.dedup();
        // The list again, for exactly the files the patch shows, in its
        // order: asking by both paths can bring in another file at one of
        // them, which is kept from the run that picked it.
        let listed = raw(root, store, from, to, Some(&pathspec)).await?;
        let sections = patch(root, store, from, to, context, &pathspec).await?;
        let own: HashSet<&str> = run.iter().map(|file| file.path.as_str()).collect();
        for (file, hunks) in paired(listed, sections)? {
            if own.contains(file.path.as_str()) {
                found.insert(file.path.clone(), (file, hunks));
            }
        }
    }
    Ok(picked
        .iter()
        .filter_map(|file| found.remove(&file.path))
        .collect())
}

/// The patch from `from` to `to` at `context` lines, of `pathspec` or with
/// none of the whole tree: each file's hunks, in the patch's order.
async fn patch(
    root: &Path,
    store: &ObjectStore,
    from: &str,
    to: &str,
    context: u32,
    pathspec: &[&str],
) -> Result<Vec<Vec<Hunk>>, CompareError> {
    let unified = format!("--unified={context}");
    let mut args = vec![
        "--literal-pathspecs",
        "diff",
        &unified,
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--find-renames",
    ];
    args.extend(crate::DIFF_OPTIONS);
    args.extend([from, to, "--"]);
    args.extend(pathspec.iter().copied());
    let patch = read_git(root, store, &[from, to], &args).await?;
    Ok(sections(&patch)
        .into_iter()
        .map(|section| section.iter().flat_map(|part| parse_hunks(part)).collect())
        .collect())
}

/// Each listed file with the hunks of its section of the patch, which
/// lists the same files in the same order.
fn paired(
    listed: Vec<RawFile>,
    sections: Vec<Vec<Hunk>>,
) -> Result<Vec<(RawFile, Vec<Hunk>)>, CompareError> {
    if sections.len() != listed.len() {
        return Err(GitError::Failed {
            command: "diff".to_owned(),
            stderr: format!(
                "the patch shows {} files where the list has {}",
                sections.len(),
                listed.len()
            ),
        }
        .into());
    }
    Ok(listed.into_iter().zip(sections).collect())
}

fn file_hunks(file: RawFile, hunks: Vec<Hunk>) -> FileHunks {
    let side = |id: &str, there: bool| (there && hashed(id)).then(|| id.to_owned());
    FileHunks {
        old_blob: side(&file.old_id, file.status != Status::Created),
        new_blob: side(&file.new_id, file.status != Status::Deleted),
        path: file.path,
        old_path: file.old_path,
        status: file.status,
        binary: file.binary,
        hunks,
    }
}

/// Whether git hashed an object for a side: all zeros is a side not there.
fn hashed(id: &str) -> bool {
    !id.is_empty() && id.bytes().any(|byte| byte != b'0')
}

/// What a file mode says the entry is (`10` a file, `12` a link, `16` a
/// submodule), so a change of kind can be told from a change of mode.
fn kind(mode: &str) -> &str {
    mode.get(..2).unwrap_or(mode)
}

/// What git writes for `args`, reading the store as well, its failure
/// explained when one of `commits`, or something one holds, is gone.
async fn read_git(
    root: &Path,
    store: &ObjectStore,
    commits: &[&str],
    args: &[&str],
) -> Result<Vec<u8>, CompareError> {
    let ran = finish(store.reading(root, args), None).await?;
    match succeeded(args, ran) {
        Ok(output) => Ok(output),
        Err(error) => Err(explained(root, store, commits, error).await),
    }
}

/// `error` as [`CompareError::Missing`] when one of `commits` cannot be read
/// whole: it is gone, or a folder or file it holds is.
async fn explained(
    root: &Path,
    store: &ObjectStore,
    commits: &[&str],
    error: GitError,
) -> CompareError {
    for commit in commits {
        if !readable(root, store, commit).await.unwrap_or(true) {
            return CompareError::Missing((*commit).to_owned());
        }
    }
    error.into()
}

/// Whether every folder and file `commit` holds can be read.
async fn readable(root: &Path, store: &ObjectStore, commit: &str) -> Result<bool, GitError> {
    let tree = format!("{commit}^{{tree}}");
    let args = ["ls-tree", "-r", "-z", "--full-tree", &tree];
    let listed = finish(store.reading(root, &args), None).await?;
    if !listed.status.success() {
        return Ok(false);
    }
    let mut ids = String::new();
    for entry in listed.stdout.split(|byte| *byte == 0) {
        // `mode type id TAB path`; a submodule's commit is another
        // repository's.
        let entry = String::from_utf8_lossy(entry);
        let mut fields = entry.split(['\t', ' ']);
        if let (Some(_), Some("blob"), Some(id)) = (fields.next(), fields.next(), fields.next()) {
            ids.push_str(id);
            ids.push('\n');
        }
    }
    let args = ["cat-file", "--batch-check"];
    let checked = succeeded(
        &args,
        finish(store.reading(root, &args), Some(ids.into_bytes())).await?,
    )?;
    Ok(!text(&checked)
        .lines()
        .any(|line| line.ends_with(" missing")))
}

/// Which of `paths` `commit` holds, asked [`PATHSPEC_BYTES`] of paths at a
/// time.
async fn present(
    root: &Path,
    store: &ObjectStore,
    commit: &str,
    paths: &[&str],
) -> Result<HashSet<String>, CompareError> {
    let mut held = HashSet::new();
    for run in runs(paths, |path| vec![*path]) {
        let mut args = vec![
            "--literal-pathspecs",
            "ls-tree",
            "-z",
            "--full-tree",
            "--name-only",
            commit,
            "--",
        ];
        args.extend(run.into_iter().copied());
        let listed = read_git(root, store, &[commit], &args).await?;
        held.extend(
            listed
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .map(|path| String::from_utf8_lossy(path).into_owned()),
        );
    }
    Ok(held)
}

fn ls_tree<'a>(commit: &'a str, path: &'a str) -> [&'a str; 7] {
    [
        "--literal-pathspecs",
        "ls-tree",
        "-z",
        "--full-tree",
        commit,
        "--",
        path,
    ]
}

/// The objects `names` name, whole, each with its type; None for one git
/// cannot find.
async fn batch(
    root: &Path,
    store: &ObjectStore,
    names: &[String],
) -> Result<Vec<Option<(String, Vec<u8>)>>, GitError> {
    let input: String = names.iter().map(|name| format!("{name}\n")).collect();
    let args = ["cat-file", "--batch"];
    let read = succeeded(
        &args,
        finish(store.reading(root, &args), Some(input.into_bytes())).await?,
    )?;
    // Each object is `id type size LF`, its bytes, then LF; one git cannot
    // find is `name missing LF`, or `name ambiguous LF`.
    let mut objects = Vec::with_capacity(names.len());
    let mut rest = read.as_slice();
    while objects.len() < names.len() {
        let Some(end) = rest.iter().position(|byte| *byte == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&rest[..end]).into_owned();
        rest = &rest[end + 1..];
        let mut fields = header.rsplitn(3, ' ');
        let (size, kind) = (fields.next(), fields.next());
        match size.and_then(|size| size.parse::<usize>().ok()) {
            Some(size) if rest.len() > size => {
                objects.push(Some((
                    kind.unwrap_or_default().to_owned(),
                    rest[..size].to_vec(),
                )));
                rest = &rest[size + 1..];
            }
            _ => objects.push(None),
        }
    }
    objects.resize(names.len(), None);
    Ok(objects)
}

/// A patch cut into its files, each the `diff --git` sections git wrote
/// for it: one, or two for a file that changed kind, which git shows as a
/// deletion and then a creation under the same header.
fn sections(patch: &[u8]) -> Vec<Vec<&[u8]>> {
    let mut sections: Vec<(&[u8], Vec<&[u8]>)> = Vec::new();
    let mut starts = Vec::new();
    let mut at = 0;
    for line in patch.split_inclusive(|byte| *byte == b'\n') {
        if line.starts_with(b"diff --git ") {
            starts.push(at);
        }
        at += line.len();
    }
    starts.push(patch.len());
    for pair in starts.windows(2) {
        let section = &patch[pair[0]..pair[1]];
        let header = section
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default();
        match sections.last_mut() {
            Some((last, parts)) if *last == header => parts.push(section),
            _ => sections.push((header, vec![section])),
        }
    }
    sections.into_iter().map(|(_, parts)| parts).collect()
}

/// The hunks of one `diff --git` section, their lines numbered.
fn parse_hunks(section: &[u8]) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    // Lines each side of the current hunk still has to come, and the next
    // number on each side.
    let (mut old_left, mut new_left) = (0u32, 0u32);
    let (mut old_line, mut new_line) = (0u32, 0u32);
    for line in split_lines(section) {
        if old_left == 0 && new_left == 0 {
            if let Some(hunk) = hunk_header(line) {
                (old_left, new_left) = (hunk.old_count, hunk.new_count);
                old_line = hunk.old_start.max(1);
                new_line = hunk.new_start.max(1);
                hunks.push(hunk);
            }
            continue;
        }
        let Some(hunk) = hunks.last_mut() else {
            continue;
        };
        let (kind, rest) = match line.first() {
            Some(b'+') => (LineKind::Added, &line[1..]),
            Some(b'-') => (LineKind::Removed, &line[1..]),
            Some(b' ') => (LineKind::Context, &line[1..]),
            // `\ No newline at end of file` belongs to the line before.
            Some(b'\\') => continue,
            // An empty context line, as `diff.suppressBlankEmpty` writes it.
            None => (LineKind::Context, line),
            Some(_) => continue,
        };
        let (old, new) = match kind {
            LineKind::Context => (old_line, new_line),
            LineKind::Removed => (old_line, 0),
            LineKind::Added => (0, new_line),
        };
        if old != 0 {
            old_line += 1;
            old_left = old_left.saturating_sub(1);
        }
        if new != 0 {
            new_line += 1;
            new_left = new_left.saturating_sub(1);
        }
        hunk.lines.push(DiffLine {
            kind,
            old_line: old,
            new_line: new,
            text: line_text(rest),
        });
    }
    hunks
}

/// `@@ -old_start[,old_count] +new_start[,new_count] @@ …`, a count left
/// out being one.
fn hunk_header(line: &[u8]) -> Option<Hunk> {
    let line = std::str::from_utf8(line.strip_prefix(b"@@ -")?).ok()?;
    let (ranges, _) = line.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let side = |range: &str| -> Option<(u32, u32)> {
        match range.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((range.parse().ok()?, 1)),
        }
    };
    let ((old_start, old_count), (new_start, new_count)) = (side(old)?, side(new)?);
    Some(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        lines: Vec::new(),
    })
}

/// A file's lines without their endings; a last line with no ending is a
/// line all the same.
fn split_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes
        .split(|byte| *byte == b'\n')
        .filter(move |_| !bytes.is_empty())
}

/// A line's text, a carriage return before its ending dropped with it.
fn line_text(line: &[u8]) -> String {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    String::from_utf8_lossy(line).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(old_start: u32, old_count: u32, new_start: u32, new_count: u32) -> Edit {
        Edit {
            old_start,
            old_count,
            new_start,
            new_count,
        }
    }

    fn map(edits: Vec<Edit>) -> LineMap {
        LineMap {
            path: Some("f".into()),
            edits,
            opaque: false,
        }
    }

    #[test]
    fn a_range_moves_by_what_changed_before_it_and_is_touched_by_what_changed_in_it() {
        // Two lines put in after line 2, line 10 edited, lines 20–21
        // deleted.
        let map = map(vec![
            hunk(2, 0, 3, 2),
            hunk(10, 1, 12, 1),
            hunk(20, 2, 21, 0),
        ]);
        let range = |start, end| LineRange::new(start, end);
        assert_eq!(map.map(range(1, 2)), Mapped::Untouched(range(1, 2)));
        assert_eq!(map.map(range(2, 3)), Mapped::Touched);
        assert_eq!(map.map(range(3, 9)), Mapped::Untouched(range(5, 11)));
        assert_eq!(map.map(range(9, 10)), Mapped::Touched);
        assert_eq!(map.map(range(11, 19)), Mapped::Untouched(range(13, 21)));
        assert_eq!(map.map(range(21, 21)), Mapped::Touched);
        assert_eq!(map.map(range(22, 30)), Mapped::Untouched(range(22, 30)));
        assert_eq!(
            map.map(LineRange::file()),
            Mapped::Untouched(LineRange::file())
        );
        assert_eq!(map.changed(), vec![range(3, 4), range(12, 12)]);
        assert!(map.touches(range(5, 6).widened(3)));
        assert!(!map.touches(range(5, 6).widened(2)));
    }

    #[test]
    fn lines_put_in_at_the_top_move_everything_and_touch_nothing() {
        let map = map(vec![hunk(0, 0, 1, 3)]);
        assert_eq!(
            map.map(LineRange::new(1, 4)),
            Mapped::Untouched(LineRange::new(4, 7))
        );
    }

    #[test]
    fn hunks_are_read_with_their_lines_numbered() {
        let section = b"diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n\
            @@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n\\ No newline at end of file\n\
            @@ -9 +9,0 @@\n-z\n";
        let hunks = parse_hunks(section);
        assert_eq!(hunks.len(), 2);
        let numbers: Vec<_> = hunks[0]
            .lines
            .iter()
            .map(|line| (line.kind, line.old_line, line.new_line, line.text.as_str()))
            .collect();
        assert_eq!(
            numbers,
            [
                (LineKind::Context, 1, 1, "a"),
                (LineKind::Removed, 2, 0, "b"),
                (LineKind::Added, 0, 2, "B"),
                (LineKind::Context, 3, 3, "c"),
            ]
        );
        assert_eq!((hunks[1].old_start, hunks[1].old_count), (9, 1));
        assert_eq!((hunks[1].new_start, hunks[1].new_count), (9, 0));
        assert_eq!(hunks[1].lines[0].old_line, 9);
    }
}
