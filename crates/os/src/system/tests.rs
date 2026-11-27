use super::*;

/// Every desktop system says its version, which starts with a number:
/// Windows' `10.0.<build>.<revision>`, macOS's product version, Linux's
/// kernel release.
#[test]
fn the_system_says_its_version() {
    let version = version().expect("a version");
    assert!(
        version.starts_with(|c: char| c.is_ascii_digit()) && version.contains('.'),
        "{version}"
    );
    if std::env::consts::OS == "windows" {
        let parts: Vec<&str> = version.split('.').collect();
        assert!(
            (3..=4).contains(&parts.len()) && parts.iter().all(|part| part.parse::<u32>().is_ok()),
            "{version}"
        );
    }
}

#[test]
fn a_host_name_loses_its_mdns_suffix() {
    assert_eq!(without_mdns_suffix("Adas-MacBook.local"), "Adas-MacBook");
    assert_eq!(without_mdns_suffix("build-host"), "build-host");
}

/// Every desktop system names itself, in text a host name could be, with
/// no mDNS suffix; on Linux, the kernel's host name.
#[test]
fn the_system_names_this_computer() {
    let name = computer_name().expect("a name");
    assert!(!name.trim().is_empty() && !name.contains('\0'), "{name:?}");
    assert!(!name.ends_with(".local"), "{name}");
    if let Ok(kernel) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let kernel = kernel.trim();
        assert_eq!(name, kernel.strip_suffix(".local").unwrap_or(kernel));
    }
}
