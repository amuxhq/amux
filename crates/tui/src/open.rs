//! Opening an attachment in the app the system picks for it.
//!
//! The bytes live with the agent, possibly on another host, so they are
//! written to a fresh private folder under the attachment's own name, which
//! is what the system chooses the app by.

use std::io;
use std::path::{Path, PathBuf};

/// Kinds that only ever show their content. `open` runs a `.command` file
/// and launches a `.app`, and an agent chooses an attachment's name, so
/// anything not listed here is revealed in its folder for the person to
/// decide.
const SHOWN_DIRECTLY: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "heic", "bmp", "tif", "tiff", "pdf", "txt", "md", "log",
    "json", "csv", "tsv", "diff", "patch",
];

/// What to do with a saved attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opening {
    /// Hand it to the app the system picks.
    Open,
    /// Show it in its folder, for files that could run.
    Reveal,
}

pub fn opening(name: &str) -> Opening {
    let shown = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|extension| SHOWN_DIRECTLY.contains(&extension.as_str()));
    if shown {
        Opening::Open
    } else {
        Opening::Reveal
    }
}

/// An attachment's name as a single safe file name: the last part of
/// whatever path it holds, without control characters or leading dots.
pub fn file_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let kept: String = last.chars().filter(|c| !c.is_control()).collect();
    let kept = kept.trim_start_matches('.').trim();
    if kept.is_empty() {
        "attachment".to_owned()
    } else {
        kept.to_owned()
    }
}

/// Writes the bytes into a new private folder, answering the file's path.
/// The folder outlives this call, since the app reading it starts later;
/// the system clears its temporary folders in time.
pub fn save(name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let folder = tempfile::Builder::new()
        .prefix("amux-attachment-")
        .tempdir()?
        .keep();
    let path = folder.join(file_name(name));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_and_documents_open_and_anything_that_could_run_is_revealed() {
        assert_eq!(opening("shot.PNG"), Opening::Open);
        assert_eq!(opening("report.pdf"), Opening::Open);
        for name in ["run.command", "Thing.app", "setup.sh", "page.html", "plain"] {
            assert_eq!(opening(name), Opening::Reveal, "{name}");
        }
    }

    #[test]
    fn a_name_cannot_leave_the_folder_or_hide() {
        assert_eq!(file_name("../../etc/passwd"), "passwd");
        assert_eq!(file_name("..\\up\\shot.png"), "shot.png");
        assert_eq!(file_name(".hidden.png"), "hidden.png");
        assert_eq!(file_name("../"), "attachment");
        assert_eq!(file_name("a\u{7}b.png"), "ab.png");
    }

    #[test]
    fn saving_writes_the_bytes_under_the_name() {
        let path = save("../shot.png", b"bytes").unwrap();
        assert_eq!(path.file_name().unwrap(), "shot.png");
        assert_eq!(std::fs::read(&path).unwrap(), b"bytes");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
