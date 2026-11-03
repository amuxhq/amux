//! This binary's own version, stamped so a copy can report another. The
//! manifest, its signature, the keys a build trusts and the target it was
//! built for are the [`release`] crate, shared with the release tool and
//! the installer.

use std::sync::OnceLock;

pub use release::*;

#[used]
static STAMP: [u8; release::version_stamp::LEN] =
    release::version_stamp::stamp(env!("CARGO_PKG_VERSION"));

/// This binary's version, as stamped; `release::version_stamp::restamp`
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

pub use release::version_stamp::restamp;
