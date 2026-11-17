//! Canonical paths in the form programs and people take.

use std::io;
use std::path::{Path, PathBuf};

/// `path` made canonical, in the form programs take: [`plain`].
pub(crate) fn canonical(path: &Path) -> io::Result<PathBuf> {
    path.canonicalize().map(plain)
}

/// A canonical path in the form programs take as a working directory.
/// Windows writes a canonical drive path verbatim (`\\?\C:\...`), a form
/// that `cmd.exe`, git and many tools an agent runs refuse or print back
/// oddly; the same folder as `C:\...` is plain. A network share stays
/// verbatim.
pub(crate) fn plain(folder: PathBuf) -> PathBuf {
    #[cfg(windows)]
    if let Some(drive) = folder.to_str().and_then(|path| path.strip_prefix(r"\\?\"))
        && drive.as_bytes().get(1) == Some(&b':')
    {
        return PathBuf::from(drive);
    }
    folder
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_drive_path_loses_its_verbatim_prefix_and_a_share_keeps_it() {
        assert_eq!(
            plain(PathBuf::from(r"\\?\C:\Users\me\shop")),
            PathBuf::from(r"C:\Users\me\shop")
        );
        let share = PathBuf::from(r"\\?\UNC\server\share\shop");
        assert_eq!(plain(share.clone()), share);
    }
}
