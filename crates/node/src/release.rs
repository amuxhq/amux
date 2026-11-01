//! What a build knows about releases: its own version, the target it was
//! built for and the keys releases are signed with. The manifest itself,
//! its signature and the choice of build to install are the [`release`]
//! crate, shared with the release tool.

use std::sync::OnceLock;

pub use release::*;

/// The target triple this binary was built for; manifests are keyed by it.
pub const TARGET: &str = env!("AMUX_TARGET");

/// The public half of the key debug builds trust for a test feed when the
/// build names no release key. Its private half is in the test suites; no
/// release build trusts it.
pub const TEST_RELEASE_KEY: [u8; 32] = [
    0x48, 0x9e, 0xa5, 0x33, 0xf0, 0x7a, 0x72, 0xdb, 0x76, 0xa9, 0x9c, 0xf4, 0x7b, 0xf4, 0x00, 0x8f,
    0xcc, 0xee, 0x4b, 0x8f, 0x20, 0x05, 0xb4, 0xcc, 0xa0, 0x83, 0x5d, 0xf1, 0x04, 0xa4, 0x7e, 0xa9,
];

/// The keys a manifest read from `manifest_url` may be signed with, or None
/// when this build installs nothing from there. Compiled in from
/// `AMUX_RELEASE_PUBLIC_KEYS` (comma-separated, 64 hex digits each) at build
/// time: the key releases are signed with and the backup kept offline. A
/// release build without them never installs anything. A debug build
/// without them trusts [`TEST_RELEASE_KEY`], and only from a feed on this
/// machine's loopback, where the test suites serve theirs: the test key's
/// private half is committed, so trusting it from amux.sh would let anyone
/// able to write there sign what every development build installs.
pub fn feed_keys(manifest_url: &str) -> Option<Vec<[u8; 32]>> {
    match option_env!("AMUX_RELEASE_PUBLIC_KEYS") {
        Some(list) => Some(parse_keys(list).expect(
            "AMUX_RELEASE_PUBLIC_KEYS is comma-separated keys of 64 hex digits at build time",
        )),
        None if cfg!(debug_assertions) && is_loopback(manifest_url) => Some(vec![TEST_RELEASE_KEY]),
        None => None,
    }
}

/// Whether `url` names a host on this machine's loopback.
fn is_loopback(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}

/// Whether this is a published build: optimised and built by the release
/// workflow, which names the release keys. Development builds, test binaries
/// and test networks are not, and send no product analytics.
pub fn is_published() -> bool {
    option_env!("AMUX_RELEASE_PUBLIC_KEYS").is_some() && !cfg!(debug_assertions)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_loopback_feed_is_a_test_feed() {
        for url in [
            "http://127.0.0.1:4000/releases/stable.json",
            "http://localhost/stable.json",
            "http://[::1]:9/stable.json",
        ] {
            assert!(is_loopback(url), "{url}");
        }
        for url in [
            "https://amux.sh/releases/stable.json",
            "http://10.0.0.1/stable.json",
            "http://localhost.amux.sh/stable.json",
            "not a url",
        ] {
            assert!(!is_loopback(url), "{url}");
        }
    }

    /// A development build follows no feed but a test's own: the test key
    /// that build trusts is public.
    #[test]
    fn a_development_build_trusts_nothing_from_amux_sh() {
        if option_env!("AMUX_RELEASE_PUBLIC_KEYS").is_some() {
            return;
        }
        let production = format!("{}/releases/stable.json", settings::DEFAULT_CLOUD_URL);
        assert_eq!(feed_keys(&production), None);
        if cfg!(debug_assertions) {
            assert_eq!(
                feed_keys("http://127.0.0.1:9/stable.json"),
                Some(vec![TEST_RELEASE_KEY])
            );
        }
    }
}
