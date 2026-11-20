//! A snapshot store: a git object directory of its own, outside any
//! repository, holding the commits a review takes of a working tree.
//!
//! Git reads a snapshot in the repository with the store as an alternate,
//! so a snapshot and the history beneath it read as one. Writes into the
//! store run with it as git's only object directory, and only the commit,
//! which names its parent, also sees the repository's objects: git
//! "freshens" an object it was asked to write and finds already stored, by
//! setting the time of the loose file or the whole pack holding it, and
//! with the repository as an alternate that would touch the repository's
//! files on most snapshots (the empty blob, a tree that did not change).
//! The cost is that the first snapshot stores every tree of the working
//! tree again; later ones share them.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use tokio::io::AsyncWriteExt as _;

use crate::GitError;

/// The snapshot store at one directory.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ObjectStore {
    dir: PathBuf,
}

impl ObjectStore {
    /// The store at `dir`, which need not exist until something is written.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The store's directory: what `GIT_ALTERNATE_OBJECT_DIRECTORIES` names
    /// to read a snapshot in the repository.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Git with `args` in the repository at `cwd`, reading the store's
    /// objects as well as the repository's own: how a snapshot is read. A
    /// store nothing was written to yet is left out, since git complains of
    /// an alternate that is not there. Output and errors are piped, nothing
    /// is on its input, and it is killed if dropped.
    pub fn reading(&self, cwd: &Path, args: &[&str]) -> tokio::process::Command {
        let mut command = crate::command(cwd, None, args);
        if self.dir.is_dir() {
            command.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", alternates(&[&self.dir]));
        }
        command
    }

    /// Git writing into the store and nowhere else, on the temporary
    /// `index` when given: it sees no object but the store's.
    pub(crate) fn writing(
        &self,
        cwd: &Path,
        index: Option<&Path>,
        args: &[&str],
    ) -> tokio::process::Command {
        let mut command = crate::command(cwd, index, args);
        command
            .env("GIT_OBJECT_DIRECTORY", &self.dir)
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES");
        command
    }

    /// Git writing into the store with the repository's `objects` readable
    /// beside it: for the one write that must see the repository, a commit
    /// naming its parent, which is never already stored anywhere.
    pub(crate) fn committing(
        &self,
        cwd: &Path,
        objects: &Path,
        args: &[&str],
    ) -> tokio::process::Command {
        let mut command = crate::command(cwd, None, args);
        command
            .env("GIT_OBJECT_DIRECTORY", &self.dir)
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", alternates(&[objects]));
        command
    }
}

/// `dirs` as git reads `GIT_ALTERNATE_OBJECT_DIRECTORIES`: separated as
/// `PATH` is on this platform, an entry holding the separator (or starting
/// with the quote or comment mark git gives meaning to) quoted as C quotes a
/// string.
pub(crate) fn alternates(dirs: &[&Path]) -> OsString {
    let separator = if cfg!(windows) { ';' } else { ':' };
    let mut joined = OsString::new();
    for (n, dir) in dirs.iter().enumerate() {
        if n > 0 {
            joined.push(separator.to_string());
        }
        let text = dir.as_os_str().to_string_lossy();
        if text.contains(separator) || text.starts_with(['"', '#']) {
            let mut quoted = String::from('"');
            for c in text.chars() {
                if matches!(c, '"' | '\\') {
                    quoted.push('\\');
                }
                quoted.push(c);
            }
            quoted.push('"');
            joined.push(quoted);
        } else {
            joined.push(dir.as_os_str());
        }
    }
    joined
}

/// Runs `command` to the end, with `input` written to its standard input
/// while its output is read, so neither side waits on a full pipe.
pub(crate) async fn finish(
    mut command: tokio::process::Command,
    input: Option<Vec<u8>>,
) -> Result<Output, GitError> {
    let Some(input) = input else {
        return command.output().await.map_err(GitError::Spawn);
    };
    let mut child = command
        .stdin(Stdio::piped())
        .spawn()
        .map_err(GitError::Spawn)?;
    let mut stdin = child.stdin.take().expect("standard input is piped");
    let write = async move {
        let _ = stdin.write_all(&input).await;
    };
    let (_, output) = tokio::join!(write, child.wait_with_output());
    output.map_err(GitError::Spawn)
}

/// [`finish`], failing unless git succeeded; `name` is the git command
/// reported when it did not.
pub(crate) async fn checked(
    name: &str,
    command: tokio::process::Command,
    input: Option<Vec<u8>>,
) -> Result<Vec<u8>, GitError> {
    crate::succeeded(&[name], finish(command, input).await?)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn an_alternate_holding_the_separator_is_quoted() {
        let separator = if cfg!(windows) { ";" } else { ":" };
        let plain = Path::new("/srv/agents/a/review/objects");
        assert_eq!(super::alternates(&[plain]), plain.as_os_str());
        let odd = format!("/srv/a{separator}b/objects");
        assert_eq!(
            super::alternates(&[Path::new(&odd)]),
            format!("\"{odd}\"").as_str()
        );
        assert_eq!(
            super::alternates(&[plain, Path::new("/srv/repo/.git/objects")]),
            format!("{}{separator}/srv/repo/.git/objects", plain.display()).as_str()
        );
    }
}
