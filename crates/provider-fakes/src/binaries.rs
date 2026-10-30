//! Where the binaries the integration suites start were built: `amux`, the
//! fake providers, `fake-amux` and the agent's install-path stand-in.
//!
//! No test builds them. A test that ran cargo itself would queue on cargo's
//! lock on the target directory behind every other test process doing the
//! same, and could build with features of its own and relink a binary
//! another test is running. The nextest setup script
//! (`scripts/test-binaries.sh`) builds them once before any test starts and
//! names their directory in [`ENV`]; a caller that built them itself, such
//! as a sanitizer or release run, names its own.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Names the directory holding the built binaries.
pub const ENV: &str = "TESTNET_BINARIES";

/// The directory [`ENV`] names. A process that is not a test (a driver such
/// as `testnet serve` or `perf`) without it takes the directory it runs
/// from, where its recipe built the binaries beside it.
///
/// # Panics
///
/// In a test process [`ENV`] does not name: the binaries there could be
/// missing or older than the source under test.
pub fn dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        if let Some(dir) = std::env::var_os(ENV) {
            return dir.into();
        }
        let exe = std::env::current_exe().expect("this executable's path");
        let dir = exe.parent().expect("an executable's directory");
        // target/<profile>/deps/<test binary>
        assert!(
            dir.file_name().and_then(|name| name.to_str()) != Some("deps"),
            "{ENV} is unset: run the tests with `cargo nextest run` (or a `just` test \
             recipe), whose setup script builds amux and the fakes and sets it"
        );
        dir.to_owned()
    })
}

/// The built binary `name`, with the platform's executable suffix.
pub fn path(name: &str) -> PathBuf {
    dir().join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}
