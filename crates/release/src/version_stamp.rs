//! The version an amux binary reports lives in a fixed-size stamp behind a
//! marker, so a copy of a built binary can be re-stamped with another
//! version without rebuilding it: the previous release in an overlap test
//! is the build under test with a lower stamp. The binary defines the
//! stamp once, with [`stamp`], and reads it with [`read`].

use std::io;
use std::path::Path;

pub const LEN: usize = 64;
const MARK_LEN: usize = 16;
const MARK: [u8; MARK_LEN] = *b"\0amux-stamp-v1\0\0";
// The marker spelled backwards, so that searching for it does not put a
// second copy of it into a binary.
const MARK_REVERSED: [u8; MARK_LEN] = *b"\0\x001v-pmats-xuma\0";

/// The stamp for `version`, for a `static` in the binary.
pub const fn stamp(version: &str) -> [u8; LEN] {
    let mut out = [0u8; LEN];
    let mut i = 0;
    while i < MARK_LEN {
        out[i] = MARK[i];
        i += 1;
    }
    let bytes = version.as_bytes();
    assert!(bytes.len() < LEN - MARK_LEN, "version too long to stamp");
    let mut j = 0;
    while j < bytes.len() {
        out[MARK_LEN + j] = bytes[j];
        j += 1;
    }
    out
}

/// The version in a stamp. Read the binary's static with a volatile read
/// first, so the compiler reads the bytes a re-stamp changed rather than
/// the constant it was built with.
pub fn read(stamp: &[u8; LEN]) -> String {
    let body = &stamp[MARK_LEN..];
    let end = body
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(body.len());
    String::from_utf8_lossy(&body[..end]).into_owned()
}

/// Copies the binary at `from` to `to` with `version` stamped in place of
/// its own. On macOS the copy is signed again ad hoc, since the change
/// invalidates the linker's signature.
pub fn restamp(from: &Path, to: &Path, version: &str) -> io::Result<()> {
    let mark: Vec<u8> = MARK_REVERSED.iter().rev().copied().collect();
    let mut bytes = std::fs::read(from)?;
    let mut found = bytes
        .windows(MARK_LEN)
        .enumerate()
        .filter(|(_, window)| *window == mark.as_slice())
        .map(|(at, _)| at);
    let (Some(at), None) = (found.next(), found.next()) else {
        return Err(io::Error::other(format!(
            "{} does not hold exactly one version stamp",
            from.display()
        )));
    };
    if version.len() >= LEN - MARK_LEN {
        return Err(io::Error::other("version too long to stamp"));
    }
    let body = &mut bytes[at + MARK_LEN..at + LEN];
    body.fill(0);
    body[..version.len()].copy_from_slice(version.as_bytes());
    let _ = std::fs::remove_file(to);
    std::fs::write(to, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(to)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "codesign {} failed",
                to.display()
            )));
        }
    }
    Ok(())
}

/// A version below `version` for playing the release before it: the same
/// numbers with a `-previous` prerelease, which semver orders first. A
/// version that already has a prerelease has no such neighbour, so it
/// needs one named.
pub fn previous(version: &str) -> Option<String> {
    let core = version.split('+').next()?;
    (!core.contains('-') && core.split('.').count() == 3).then(|| format!("{core}-previous"))
}
