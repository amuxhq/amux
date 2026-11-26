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
