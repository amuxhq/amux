//! A file or folder opened up to other users, as an older build or a
//! person may have left one, for a test of [`crate::fs::make_private`] and
//! its callers on every OS, with no `cfg` of its own; what reads back
//! whether something is private is [`crate::testing::is_private`].
//!
//! Unix: group and other may read (and enter a folder). Windows: an entry
//! granting Users read, which a folder passes on to what it holds.

use std::io;
use std::path::Path;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

/// Lets other users of the machine read `path`, as a file or folder made
/// before amux kept its files private, or outside its user's profile on
/// Windows, may be: a folder passes the grant on to what it holds. It
/// lasts until the file goes or is made private.
pub fn open_to_others(path: &Path) -> io::Result<()> {
    imp::open_to_others(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::is_private;

    #[test]
    fn an_opened_folder_is_not_private_until_it_is_made_private() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        crate::fs::create_private_dir(&root).unwrap();
        crate::fs::write_durably(&root.join("key"), b"x").unwrap();
        open_to_others(&root).unwrap();
        open_to_others(&root.join("key")).unwrap();
        assert!(!is_private(&root).unwrap());
        assert!(!is_private(&root.join("key")).unwrap());
        crate::fs::make_private(&root).unwrap();
        crate::fs::make_private(&root.join("key")).unwrap();
        assert!(is_private(&root).unwrap());
        assert!(is_private(&root.join("key")).unwrap());
    }
}
