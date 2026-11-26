//! `os::paths` on Unix: the process `PATH` and the execute bit.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

pub(super) fn current_path() -> OsString {
    std::env::var_os("PATH").unwrap_or_default()
}

/// Unix names a variable by its exact spelling.
pub(super) fn same_variable(key: &str, name: &str) -> bool {
    key == name
}

/// `PATHEXT` is Windows' own: here it is ignored.
pub(super) fn find_program(
    name: &OsStr,
    path: &OsStr,
    _pathext: Option<&OsStr>,
) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    let runnable = |candidate: &Path| {
        std::fs::metadata(candidate).is_ok_and(|meta| crate::fs::is_executable(candidate, &meta))
    };
    if name.as_bytes().contains(&b'/') {
        return super::first_runnable([PathBuf::from(name)], runnable);
    }
    super::first_runnable(
        super::folders(path).map(|folder| folder.join(name)),
        runnable,
    )
}

#[cfg(test)]
mod tests {
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn a_program_is_the_first_on_path_with_an_execute_bit() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(first.join("tool"), b"").unwrap();
        fs::set_permissions(first.join("tool"), Permissions::from_mode(0o644)).unwrap();
        fs::write(second.join("tool"), b"").unwrap();
        fs::set_permissions(second.join("tool"), Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([&first, &second]).unwrap();
        assert_eq!(
            find_program(OsStr::new("tool"), &path, None),
            Some(second.join("tool"))
        );
        fs::set_permissions(first.join("tool"), Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            find_program(OsStr::new("tool"), &path, None),
            Some(first.join("tool"))
        );
        fs::create_dir(first.join("dir")).unwrap();
        assert_eq!(
            find_program(OsStr::new("dir"), &path, None),
            None,
            "a folder never runs"
        );
    }

    #[test]
    fn the_current_path_is_the_process_path() {
        assert_eq!(current_path(), std::env::var_os("PATH").unwrap_or_default());
    }
}
