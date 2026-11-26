//! This binary's own version, stamped so a copy can report another. The
//! manifest, its signature, the keys a build trusts and the target it was
//! built for are the [`release`] crate, shared with the release tool and
//! the installer.

use std::sync::OnceLock;

pub use release::*;

#[used]
static STAMP: [u8; release::version_stamp::LEN] =
    release::version_stamp::stamp(env!("CARGO_PKG_VERSION"));

/// This binary's version, as stamped; [`restamp`]
/// makes a copy of a built binary report another.
pub fn version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        // SAFETY: reading a static; volatile so the compiler reads the
        // bytes a re-stamp changed rather than the constant it was built
        // with.
        let stamp = unsafe { std::ptr::read_volatile(&STAMP) };
        release::version_stamp::read(&stamp)
    })
}

/// Copies the binary at `from` to `to` with `version` stamped in place of
/// its own, as a program that runs: executable, and signed again where the
/// change invalidates the linker's signature (ad hoc, on macOS).
pub fn restamp(from: &std::path::Path, to: &std::path::Path, version: &str) -> std::io::Result<()> {
    let mut bytes = std::fs::read(from)?;
    release::version_stamp::restamp(&mut bytes, version)
        .map_err(|error| std::io::Error::other(format!("{}: {error}", from.display())))?;
    let _ = std::fs::remove_file(to);
    std::fs::write(to, &bytes)?;
    os::fs::make_executable(to)?;
    os::fs::sign_ad_hoc(to)
}
