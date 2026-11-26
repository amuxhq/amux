use std::fs::File;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use super::{Deny, Linked};

/// The mode `path` had before the fault.
#[derive(Debug)]
pub(super) struct Restore(u32);

impl Restore {
    pub(super) fn apply(self, path: &Path) -> io::Result<()> {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(self.0))
    }
}

pub(super) fn deny(path: &Path, what: Deny) -> io::Result<Restore> {
    let mode = std::fs::metadata(path)?.permissions().mode() & 0o7777;
    let denied = match what {
        Deny::Read => mode & !0o444,
        Deny::Write => mode & !0o222,
        Deny::All => 0,
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(denied))?;
    Ok(Restore(mode))
}

pub(super) fn hold_exclusively(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Unix has no share modes: an open never excludes another",
    ))
}

pub(super) fn is_sharing_violation(_error: &io::Error) -> bool {
    false
}

pub(super) fn link_dir(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

pub(super) fn remove_link(link: &Path) -> io::Result<()> {
    std::fs::remove_file(link)
}

pub(super) fn symlink(target: &Path, link: &Path) -> io::Result<Linked> {
    std::os::unix::fs::symlink(target, link).map(|()| Linked::Made)
}

pub(super) fn hold_to_acls() -> io::Result<Vec<&'static str>> {
    Ok(Vec::new())
}
