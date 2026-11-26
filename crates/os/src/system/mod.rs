//! What the operating system is: its version, as a person would name it,
//! and the name it gives this computer.

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

/// The name this computer goes by for a person: on macOS its Computer Name
/// (System Settings, General, Sharing), which a Mac keeps apart from its
/// network host name, and otherwise, or when macOS won't say, the host
/// name without the `.local` suffix mDNS gives it. Windows' host name is
/// its device name, `GetComputerNameExW`'s DNS host name in the case it
/// was given. macOS runs `scutil` for its Computer Name. `None` when the
/// system won't say, or says it in bytes that are no text.
pub fn computer_name() -> Option<String> {
    imp::computer_name()
        .or_else(|| imp::host_name().map(|host_name| without_mdns_suffix(&host_name).to_owned()))
        .filter(|name| !name.is_empty())
}

/// A host name without the `.local` suffix mDNS gives it.
fn without_mdns_suffix(host_name: &str) -> &str {
    host_name.strip_suffix(".local").unwrap_or(host_name)
}
