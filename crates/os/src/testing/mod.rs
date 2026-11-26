//! What tests need of the OS and production code does not, built only for
//! this crate's tests and with the `testing` feature, which test-support
//! crates turn on.

pub mod faults;
pub mod pipe;
pub mod private;

/// Whether the file or folder at `path` is owner-only in its own right, as
/// `os::fs` leaves what it makes private, so a test needs no `cfg` to say
/// so: on Unix no permission bit for the group or others; on Windows a
/// protected DACL (inheriting nothing from its folder) whose every allowing
/// entry is for this user, SYSTEM or Administrators. Something private only
/// by inheritance from a private folder is not: what makes it private is
/// where it is.
pub fn is_private(path: &std::path::Path) -> std::io::Result<bool> {
    crate::fs::is_private(path)
}

/// The variable Buck's test runner (`build/test-runner.py`) sets for every
/// test it runs.
pub const TEST_RUNNER_ENV: &str = "AMUX_TEST_RUNNER";

/// Readies a test that starts amux processes (`what`), the first call of the
/// test support that starts them.
///
/// Refuses, on Windows, to go on outside Buck's test runner: there those
/// processes open windows on the desktop of whoever runs the test by hand
/// (`cargo test`), where Buck runs it in a non-interactive task. Then, on
/// every OS, hardens this test process's own handles as every amux `main`
/// does ([`crate::process::harden_start`]): on Windows a child otherwise
/// inherits the runner's output pipe, whatever its `Stdio` says, and one
/// that outlives the test (a daemon restarted in place, which Buck's job
/// ends with the test) would hold it, which the runner fails as a leak.
///
/// # Panics
///
/// On Windows, when [`TEST_RUNNER_ENV`] is unset.
pub fn require_test_runner(what: &str) {
    if cfg!(windows) && std::env::var_os(TEST_RUNNER_ENV).is_none() {
        panic!(
            "{what}: run this through `just test-windows` (Buck's test runner): on Windows it \
             starts amux processes that would open windows on the desktop"
        );
    }
    crate::process::harden_start();
}
