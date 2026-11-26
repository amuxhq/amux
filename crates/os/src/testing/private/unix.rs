use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

pub(super) fn owner_only(path: &Path) -> io::Result<bool> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o077 == 0)
}

pub(super) fn open_to_others(path: &Path) -> io::Result<()> {
    let meta = std::fs::metadata(path)?;
    let opened = if meta.is_dir() { 0o055 } else { 0o044 };
    let mode = meta.permissions().mode() & 0o7777;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | opened))
}
