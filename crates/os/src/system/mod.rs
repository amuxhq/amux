//! What the operating system is: its version, as a person would name it.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(test)]
mod tests;

/// The operating system's version: the product version on macOS and iOS
/// (`15.1`), the Android release (`14`), the kernel release on Linux and
/// every other Unix, and on Windows `major.minor.build.revision`
/// (`10.0.26100.2605`; Windows 11 is 10.0 from build 22000). `None` when
/// the system won't say.
pub fn version() -> Option<String> {
    imp::version().filter(|version| !version.is_empty())
}
