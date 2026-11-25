//! `os::fs` on Unix: modes, fsync and POSIX renames.

use std::fs::{self, DirBuilder, File, Metadata, OpenOptions, Permissions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

use super::{Create, Flush, Publish, VolumeCheck};

/// Unix paths have no verbatim form.
pub(super) const VERBATIM_PATHS: bool = false;

pub(super) fn create_private_dir(dir: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    // An existing folder keeps its mode through `create`.
    fs::set_permissions(dir, Permissions::from_mode(0o700))
}

pub(super) fn make_private(path: &Path) -> io::Result<()> {
    let meta = fs::metadata(path)?;
    // A folder, and a program, which keeps its owner's execute bit.
    let mode = if meta.is_dir() || meta.permissions().mode() & 0o111 != 0 {
        0o700
    } else {
        0o600
    };
    fs::set_permissions(path, Permissions::from_mode(mode))
}

pub(super) fn private_open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).mode(0o600);
    options
}

pub(super) fn current_user_sid() -> io::Result<String> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "a user SID is a Windows notion",
    ))
}

pub(super) fn create_private(path: &Path, how: Create) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).mode(0o600);
    match how {
        Create::New => options.write(true).create_new(true),
        Create::Truncate => options.write(true).create(true).truncate(true),
        Create::Append => options.append(true).create(true),
    };
    let file = options.open(path)?;
    // A file that was there keeps its mode through `open`.
    file.set_permissions(Permissions::from_mode(0o600))?;
    Ok(file)
}

pub(super) fn create_temp(temp: &Path) -> io::Result<File> {
    let mut options = private_open_options();
    options.create_new(true);
    options.open(temp)
}

/// Writes and syncs the temporary file, closes it, and puts it in place.
pub(super) fn finish_durably(
    mut file: File,
    temp: &Path,
    path: &Path,
    bytes: &[u8],
    publish: Publish,
    flush: Flush,
) -> io::Result<()> {
    file.write_all(bytes)?;
    if flush == Flush::Yes {
        file.sync_all()?;
    }
    drop(file);
    match publish {
        Publish::Replace => fs::rename(temp, path),
        Publish::New => publish_new(temp, path),
    }
}

/// Puts `temp` at `path` unless something is there: a hard link, then the
/// temporary name's removal.
#[cfg(not(target_os = "android"))]
fn publish_new(temp: &Path, path: &Path) -> io::Result<()> {
    fs::hard_link(temp, path)?;
    let _ = fs::remove_file(temp);
    Ok(())
}

/// Android's SELinux policy denies `link(2)` on app data to untrusted apps,
/// so the temporary file is renamed with `RENAME_NOREPLACE`, which is as
/// atomic. Bionic has exported `renameat2` since API 30, below the app's
/// minimum SDK.
#[cfg(target_os = "android")]
fn publish_new(temp: &Path, path: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let nul = |_| io::Error::new(io::ErrorKind::InvalidInput, "a path holds a NUL");
    let from = CString::new(temp.as_os_str().as_bytes()).map_err(nul)?;
    let to = CString::new(path.as_os_str().as_bytes()).map_err(nul)?;
    // SAFETY: both strings are NUL-terminated and outlive the call;
    // AT_FDCWD resolves them against the working directory.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE as _,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(super) fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

pub(super) fn remove_file(path: &Path) -> io::Result<()> {
    fs::remove_file(path)
}

pub(super) fn remove_dir_all(path: &Path) -> io::Result<()> {
    fs::remove_dir_all(path)
}

/// Unix has no sharing violations; a busy mount point and a folder another
/// process is filling are what waiting can fix.
pub(super) fn is_held(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ResourceBusy | io::ErrorKind::DirectoryNotEmpty
    )
}

pub(super) fn make_executable(path: &Path) -> io::Result<()> {
    let mode = fs::metadata(path)?.permissions().mode();
    let executable = mode | ((mode & 0o444) >> 2);
    fs::set_permissions(path, Permissions::from_mode(executable))
}

pub(super) fn is_executable(_path: &Path, meta: &Metadata) -> bool {
    meta.is_file() && meta.permissions().mode() & 0o111 != 0
}

pub(super) fn volume_supports_root(_path: &Path) -> io::Result<VolumeCheck> {
    Ok(VolumeCheck::Suitable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn private_folders_and_files_are_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, Permissions::from_mode(0o755)).unwrap();
        super::super::create_private_dir(&root).unwrap();
        assert_eq!(mode(&root), 0o700, "an existing folder is made private");

        let durable = root.join("device.key");
        super::super::write_durably(&durable, b"key").unwrap();
        assert_eq!(mode(&durable), 0o600);

        let program = root.join("tool");
        fs::write(&program, b"").unwrap();
        fs::set_permissions(&program, Permissions::from_mode(0o755)).unwrap();
        super::super::make_private(&program).unwrap();
        assert_eq!(mode(&program), 0o700, "a program stays runnable");

        let file = super::super::private_open_options()
            .create_new(true)
            .open(root.join("journal"))
            .unwrap();
        drop(file);
        assert_eq!(mode(&root.join("journal")), 0o600);
    }

    #[test]
    fn a_user_sid_is_unsupported() {
        assert_eq!(
            super::super::current_user_sid().unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn making_a_file_executable_follows_its_read_bits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool");
        fs::write(&path, b"").unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o640)).unwrap();
        super::super::make_executable(&path).unwrap();
        assert_eq!(mode(&path), 0o750);
    }

    /// Unix's half of the private-file promise; Windows reads back the DACL
    /// in `windows_tests.rs`.
    #[test]
    fn a_private_file_is_0600_and_an_existing_one_becomes_so() {
        let dir = tempfile::tempdir().unwrap();
        for how in [Create::Truncate, Create::Append] {
            let path = dir.path().join(format!("{how:?}"));
            fs::write(&path, b"x").unwrap();
            fs::set_permissions(&path, Permissions::from_mode(0o644)).unwrap();
            drop(create_private(&path, how).unwrap());
            assert_eq!(mode(&path), 0o600, "{how:?}");
        }
        let new = dir.path().join("new");
        drop(create_private(&new, Create::New).unwrap());
        assert_eq!(mode(&new), 0o600);
    }
}
