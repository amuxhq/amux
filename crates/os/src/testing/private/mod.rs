//! Who may open a file or folder, read back, and opened up the way an
//! older build or a person may have left it: what a test of
//! [`crate::fs::make_private`] and its callers needs on every OS, with no
//! `cfg` of its own.
//!
//! | | Unix | Windows |
//! |---|---|---|
//! | [`owner_only`] | no group or other bit in the mode | every entry of the DACL that grants anything grants it to this user, SYSTEM or Administrators |
//! | [`open_to_others`] | group and other may read (and enter a folder) | an entry granting Users read (and, on a folder, passed on to what it holds) |

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

/// Whether only this user (and, on Windows, the system and its
/// administrators) may open `path`.
pub fn owner_only(path: &Path) -> io::Result<bool> {
    imp::owner_only(path)
}

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

    #[test]
    fn an_opened_folder_is_not_owner_only_until_it_is_made_private() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("key"), b"x").unwrap();
        open_to_others(&root).unwrap();
        open_to_others(&root.join("key")).unwrap();
        assert!(!owner_only(&root).unwrap());
        assert!(!owner_only(&root.join("key")).unwrap());
        crate::fs::make_private(&root).unwrap();
        crate::fs::make_private(&root.join("key")).unwrap();
        assert!(owner_only(&root).unwrap());
        assert!(owner_only(&root.join("key")).unwrap());
    }
}
