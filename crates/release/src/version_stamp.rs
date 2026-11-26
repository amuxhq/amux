//! The version an amux binary reports lives in a fixed-size stamp behind a
//! marker, so a copy of a built binary can be re-stamped with another
//! version without rebuilding it: the previous release in an overlap test
//! is the build under test with a lower stamp. The binary defines the
//! stamp once, with [`stamp`], and reads it with [`read`].

use std::io;

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

/// Stamps `version` into a binary's bytes in place of its own. Writing the
/// copy, and making it a program that runs, is `node::release::restamp`'s:
/// this crate takes in no OS code, since a core reads its manifests.
pub fn restamp(binary: &mut [u8], version: &str) -> io::Result<()> {
    let mark: Vec<u8> = MARK_REVERSED.iter().rev().copied().collect();
    let mut found = binary
        .windows(MARK_LEN)
        .enumerate()
        .filter(|(_, window)| *window == mark.as_slice())
        .map(|(at, _)| at);
    let (Some(at), None) = (found.next(), found.next()) else {
        return Err(io::Error::other(
            "it does not hold exactly one version stamp",
        ));
    };
    if version.len() >= LEN - MARK_LEN {
        return Err(io::Error::other("version too long to stamp"));
    }
    let body = &mut binary[at + MARK_LEN..at + LEN];
    body.fill(0);
    body[..version.len()].copy_from_slice(version.as_bytes());
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
