//! `os::paths` on Unix: the process `PATH` and the execute bit.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

pub(super) fn current_path() -> OsString {
    std::env::var_os("PATH").unwrap_or_default()
}

pub(super) fn from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(bytes))
}

pub(super) fn login_shell() -> Option<PathBuf> {
    shell_of(std::env::var_os("SHELL"))
}

fn shell_of(shell: Option<OsString>) -> Option<PathBuf> {
    shell.filter(|shell| !shell.is_empty()).map(PathBuf::from)
}

/// A Unix user's `PATH` is whatever their shell's profile builds; an
/// install script edits that file itself.
pub(super) fn remove_from_user_path(_dir: &Path) -> std::io::Result<bool> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "a Unix user's PATH is their shell profile's",
    ))
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
    fn a_path_from_bytes_keeps_bytes_that_are_not_utf8() {
        let bytes = b"dir/am\xffux";
        assert_eq!(from_bytes(bytes).as_os_str().as_bytes(), bytes);
    }

    #[test]
    fn the_login_shell_is_shell_when_it_names_one() {
        assert_eq!(
            shell_of(Some("/bin/zsh".into())),
            Some(PathBuf::from("/bin/zsh"))
        );
        assert_eq!(shell_of(Some(OsString::new())), None);
        assert_eq!(shell_of(None), None);
    }

    #[test]
    fn the_current_path_is_the_process_path() {
        assert_eq!(current_path(), std::env::var_os("PATH").unwrap_or_default());
    }
}
