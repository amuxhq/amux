//! What amux takes as a host's name.
//!
//! A host's name is shown wherever hosts are listed: in `amux hosts`, the
//! terminal app and the phone. Names arrive from configuration, the
//! system, pairing, local discovery and every link's handshake, and a
//! terminal acts on control sequences in what it prints. So a name is any
//! printable text, as a person names a machine ("Jordan’s MacBook Pro",
//! "desk (2)"), without control characters or the invisible formatting
//! ones (bidirectional overrides, zero-width characters) that make a name
//! read as something it is not.

use std::fmt;

/// The longest name, in characters.
pub const MAX_HOST_NAME_CHARS: usize = 64;

/// What [`HostName::lossy`] gives a name left empty.
const UNNAMED: &str = "unknown";

/// A host's name: printable, trimmed, between one and
/// [`MAX_HOST_NAME_CHARS`] characters.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HostName(String);

/// Why text is not a host name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostNameError {
    Empty,
    TooLong,
    /// A control or invisible formatting character.
    Unprintable(char),
}

impl fmt::Display for HostNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a host name must not be empty"),
            Self::TooLong => write!(
                f,
                "a host name must be at most {MAX_HOST_NAME_CHARS} characters"
            ),
            Self::Unprintable(found) => write!(
                f,
                "a host name must not contain control or invisible characters (found U+{:04X})",
                u32::from(*found)
            ),
        }
    }
}

impl std::error::Error for HostNameError {}

impl HostName {
    /// `name` without the whitespace around it, when that is a host name.
    pub fn parse(name: &str) -> Result<Self, HostNameError> {
        let name = name.trim();
        if let Some(found) = name.chars().find(|c| unprintable(*c)) {
            return Err(HostNameError::Unprintable(found));
        }
        if name.is_empty() {
            return Err(HostNameError::Empty);
        }
        if name.chars().count() > MAX_HOST_NAME_CHARS {
            return Err(HostNameError::TooLong);
        }
        Ok(Self(name.to_owned()))
    }

    /// The host name nearest `name`, for a name no one typed for amux (the
    /// system's, a phone's): what cannot be in one is dropped and the rest
    /// is cut to [`MAX_HOST_NAME_CHARS`], so a host always has a name to
    /// start with.
    pub fn lossy(name: &str) -> Self {
        let kept: String = name.chars().filter(|c| !unprintable(*c)).collect();
        let cut: String = kept.trim().chars().take(MAX_HOST_NAME_CHARS).collect();
        Self::parse(&cut).unwrap_or_else(|_| Self(UNNAMED.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for HostName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for HostName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<HostName> for String {
    fn from(name: HostName) -> Self {
        name.0
    }
}

impl PartialEq<str> for HostName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for HostName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// A control character (C0, DEL, C1) or an invisible formatting one
/// (Unicode's general category Cf).
fn unprintable(c: char) -> bool {
    c.is_control() || format_character(c)
}

/// Unicode's format characters (general category Cf), as of Unicode 16.
/// The set changes rarely, and only by additions in scripts' own blocks.
fn format_character(c: char) -> bool {
    const FORMAT: &[(u32, u32)] = &[
        (0x00AD, 0x00AD),
        (0x0600, 0x0605),
        (0x061C, 0x061C),
        (0x06DD, 0x06DD),
        (0x070F, 0x070F),
        (0x0890, 0x0891),
        (0x08E2, 0x08E2),
        (0x180E, 0x180E),
        (0x200B, 0x200F),
        (0x202A, 0x202E),
        (0x2060, 0x2064),
        (0x2066, 0x206F),
        (0xFEFF, 0xFEFF),
        (0xFFF9, 0xFFFB),
        (0x110BD, 0x110BD),
        (0x110CD, 0x110CD),
        (0x13430, 0x1343F),
        (0x1BCA0, 0x1BCA3),
        (0x1D173, 0x1D17A),
        (0xE0001, 0xE0001),
        (0xE0020, 0xE007F),
    ];
    let c = u32::from(c);
    FORMAT
        .iter()
        .any(|(first, last)| (*first..=*last).contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_as_people_write_one_is_taken() {
        for name in [
            "desk",
            "Jordan’s MacBook Pro",
            "Jordan's MacBook",
            "MacBook Pro (2)",
            "build-01.lan",
            "Ordinateur de Zoé",
            "株式会社のMac",
            "🚀 rocket",
        ] {
            assert_eq!(HostName::parse(name).unwrap(), name);
        }
        assert_eq!(HostName::parse("  desk \n").unwrap(), "desk");
    }

    #[test]
    fn control_and_invisible_characters_are_refused() {
        for (name, found) in [
            ("desk\u{1b}[2J", '\u{1b}'),
            ("desk\u{7f}", '\u{7f}'),
            ("desk\u{9b}31m", '\u{9b}'),
            ("desk\tlaptop", '\t'),
            ("desk\u{202e}potkal", '\u{202e}'),
            ("de\u{200b}sk", '\u{200b}'),
            ("desk\u{2066}", '\u{2066}'),
            ("desk\u{feff}", '\u{feff}'),
            ("desk\u{e0041}", '\u{e0041}'),
        ] {
            assert_eq!(
                HostName::parse(name),
                Err(HostNameError::Unprintable(found)),
                "{name:?}"
            );
        }
    }

    #[test]
    fn a_name_is_one_to_sixty_four_characters() {
        assert_eq!(HostName::parse(""), Err(HostNameError::Empty));
        assert_eq!(HostName::parse("   "), Err(HostNameError::Empty));
        let longest = "é".repeat(MAX_HOST_NAME_CHARS);
        assert_eq!(HostName::parse(&longest).unwrap(), longest.as_str());
        assert_eq!(
            HostName::parse(&format!("{longest}a")),
            Err(HostNameError::TooLong)
        );
    }

    #[test]
    fn a_system_name_is_made_one_rather_than_refused() {
        let long = "a".repeat(MAX_HOST_NAME_CHARS + 10);
        assert_eq!(HostName::lossy(&long).as_str().len(), MAX_HOST_NAME_CHARS);
        assert_eq!(HostName::lossy("desk\u{202e}\u{1b}top"), "desktop");
        assert_eq!(HostName::lossy("\u{200b}\n"), UNNAMED);
        // A cut that ends in a space leaves none.
        let spaced = format!("{} b", "a".repeat(MAX_HOST_NAME_CHARS - 1));
        assert_eq!(
            HostName::lossy(&spaced).as_str(),
            "a".repeat(MAX_HOST_NAME_CHARS - 1)
        );
    }
}
