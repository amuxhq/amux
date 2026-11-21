//! What every request says about the device, and whether and where a build
//! sends at all.

use serde_json::{Value, json};

/// Names the base URL a build sends to instead of its account service, in
/// any build: how a person tries the uploader against a server of their own.
pub const URL_ENV: &str = "AMUX_ANALYTICS_URL";

/// The convention a person sets to ask every tool not to send telemetry.
pub const DO_NOT_TRACK_ENV: &str = "DO_NOT_TRACK";

/// Where events go, once a build sends them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// Every profile's events go here: the [`URL_ENV`] override.
    Fixed(String),
    /// Each profile's events go to the account service it is bound to, and
    /// an unbound profile's to the default one.
    Accounts,
}

impl Endpoint {
    /// Whether a build sends, and where. `shipped` is whether this is a
    /// published build; development builds, tests and test networks are
    /// not, so they never send unless [`URL_ENV`] names a server.
    /// `DO_NOT_TRACK` wins over everything.
    pub fn resolve(shipped: bool) -> Option<Endpoint> {
        Self::resolve_from(
            shipped,
            std::env::var(DO_NOT_TRACK_ENV).ok().as_deref(),
            std::env::var(URL_ENV).ok().as_deref(),
        )
    }

    fn resolve_from(
        shipped: bool,
        do_not_track: Option<&str>,
        url: Option<&str>,
    ) -> Option<Endpoint> {
        if do_not_track_set(do_not_track) {
            return None;
        }
        match url.map(str::trim).filter(|url| !url.is_empty()) {
            Some(url) => Some(Endpoint::Fixed(base(url).to_owned())),
            None if shipped => Some(Endpoint::Accounts),
            None => None,
        }
    }

    /// Where a profile's events go: its account service's, else
    /// `default`, unless an override names one place for all.
    pub fn base<'a>(&'a self, service: Option<&'a str>, default: &'a str) -> &'a str {
        match self {
            Endpoint::Fixed(url) => url,
            Endpoint::Accounts => base(service.unwrap_or(default)),
        }
    }

    /// Where events go, said to a person: `amux config telemetry` prints it.
    pub fn describe(&self, default: &str) -> String {
        match self {
            Endpoint::Fixed(url) => format!("{} (from {URL_ENV})", events_url(url)),
            Endpoint::Accounts => format!(
                "{} or the account service a profile is signed in to",
                events_url(base(default))
            ),
        }
    }
}

/// Whether `DO_NOT_TRACK` asks for nothing to be sent: set to anything but
/// empty or `0`.
pub fn do_not_track() -> bool {
    do_not_track_set(std::env::var(DO_NOT_TRACK_ENV).ok().as_deref())
}

fn do_not_track_set(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.trim().is_empty() && value.trim() != "0")
}

fn base(url: &str) -> &str {
    url.trim_end_matches('/')
}

/// The events' endpoint under a base URL.
pub fn events_url(base: &str) -> String {
    format!("{base}/api/events")
}

/// The device a request comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub platform: Platform,
    pub os_version: Option<String>,
    pub arch: &'static str,
    /// This build's version.
    pub version: String,
    pub channel: Channel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
    Ios,
    Android,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "ios") {
            Platform::Ios
        } else if cfg!(target_os = "android") {
            Platform::Android
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Other
        }
    }

    /// As the contract spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::MacOs => "macOS",
            Platform::Linux => "Linux",
            Platform::Windows => "Windows",
            Platform::Ios => "ios",
            Platform::Android => "android",
            Platform::Other => "other",
        }
    }
}

/// The release channel a build follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Preview,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Preview => "preview",
        }
    }
}

impl Context {
    /// This device, running build `version` on `channel`.
    pub fn detect(version: &str, channel: Channel) -> Self {
        Self {
            platform: Platform::current(),
            os_version: os_version().map(|version| token(&version)),
            arch: std::env::consts::ARCH,
            version: token(version),
            channel,
        }
    }

    pub fn to_json(&self) -> Value {
        let mut context = json!({
            "platform": self.platform.as_str(),
            "arch": self.arch,
            "version": self.version,
            "channel": self.channel.as_str(),
        });
        if let Some(os_version) = &self.os_version {
            context["os_version"] = json!(os_version);
        }
        context
    }
}

/// Text as the contract's token alphabet allows: lowercase letters, digits
/// and `_.:-`, at most 64 of them.
fn token(text: &str) -> String {
    text.trim()
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

/// The operating system's version: the product version on Apple systems
/// and Android, the kernel release on Linux. Windows reports none.
fn os_version() -> Option<String> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let name = c"kern.osproductversion";
        let mut buf = [0u8; 64];
        let mut len = buf.len();
        // SAFETY: `buf` and `len` describe a writable buffer that outlives
        // the call; the name is a NUL-terminated string.
        let status = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 {
            return None;
        }
        let text = &buf[..len.min(buf.len())];
        let text = text.split(|byte| *byte == 0).next().unwrap_or_default();
        Some(String::from_utf8_lossy(text).into_owned()).filter(|text| !text.is_empty())
    }
    // The Android release ("14"), not the kernel's, which says nothing of
    // the system a person has.
    #[cfg(target_os = "android")]
    {
        let mut value = [0 as std::ffi::c_char; libc::PROP_VALUE_MAX as usize];
        // SAFETY: the name is NUL-terminated, and `value` holds the
        // PROP_VALUE_MAX bytes the call may write, NUL included.
        let len = unsafe {
            libc::__system_property_get(c"ro.build.version.release".as_ptr(), value.as_mut_ptr())
        };
        if len <= 0 {
            return None;
        }
        // SAFETY: the call NUL-terminates what it writes.
        let release = unsafe { std::ffi::CStr::from_ptr(value.as_ptr()) };
        Some(release.to_string_lossy().into_owned()).filter(|text| !text.is_empty())
    }
    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    ))]
    {
        // SAFETY: uname fills the zeroed struct it is handed.
        let mut name: libc::utsname = unsafe { std::mem::zeroed() };
        if unsafe { libc::uname(&mut name) } != 0 {
            return None;
        }
        // SAFETY: uname NUL-terminates each field.
        let release = unsafe { std::ffi::CStr::from_ptr(name.release.as_ptr()) };
        Some(release.to_string_lossy().into_owned()).filter(|text| !text.is_empty())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: &str = "https://amux.sh";

    #[test]
    fn a_development_build_sends_nowhere() {
        assert_eq!(Endpoint::resolve_from(false, None, None), None);
    }

    #[test]
    fn a_shipped_build_sends_to_each_account_service() {
        let endpoint = Endpoint::resolve_from(true, None, None).unwrap();
        assert_eq!(endpoint.base(None, DEFAULT), DEFAULT);
        assert_eq!(
            endpoint.base(Some("https://staging.amux.sh/"), DEFAULT),
            "https://staging.amux.sh"
        );
    }

    #[test]
    fn the_override_sends_any_build_to_one_place() {
        let endpoint = Endpoint::resolve_from(false, None, Some("http://localhost:5000/")).unwrap();
        assert_eq!(endpoint, Endpoint::Fixed("http://localhost:5000".into()));
        assert_eq!(
            endpoint.base(Some(DEFAULT), DEFAULT),
            "http://localhost:5000"
        );
        assert_eq!(Endpoint::resolve_from(false, None, Some("  ")), None);
    }

    #[test]
    fn do_not_track_wins_over_everything() {
        for set in ["1", "true", "yes"] {
            assert_eq!(
                Endpoint::resolve_from(true, Some(set), Some("http://x")),
                None
            );
        }
        for unset in ["", "0", " 0 "] {
            assert!(Endpoint::resolve_from(true, Some(unset), None).is_some());
        }
    }

    #[test]
    fn context_values_are_tokens() {
        assert_eq!(token(" 15.5 (Build 24F74)"), "15.5__build_24f74_");
        let context = Context::detect("0.8.0", Channel::Preview);
        let json = context.to_json();
        assert_eq!(json["version"], "0.8.0");
        assert_eq!(json["channel"], "preview");
        assert_eq!(json["arch"], std::env::consts::ARCH);
    }

    #[test]
    fn platform_names_match_contract() {
        assert_eq!(Platform::MacOs.as_str(), "macOS");
        assert_eq!(Platform::Linux.as_str(), "Linux");
        assert_eq!(Platform::Windows.as_str(), "Windows");
        assert_eq!(Platform::Ios.as_str(), "ios");
        assert_eq!(Platform::Android.as_str(), "android");
        assert_eq!(Platform::Other.as_str(), "other");
    }
}
