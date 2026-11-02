//! A record's structure without its content, for a stage whose code never
//! reads content: the daemon core. Every value is treated by the name of
//! the field that holds it, from one table, never by what its text looks
//! like:
//!
//! - an identifier the core compares (a host's or an agent's name, a
//!   folder, a branch, a token) becomes a stable alias such as `host-1`
//!   or `path-3`, so values that were equal stay equal and no others do;
//! - an id, a key, a version or a setting is kept as it is;
//! - everything else (prompts, output, error text) becomes zero bytes of
//!   its length, so every size and offset holds, and zero bytes compress
//!   to almost nothing.
//!
//! Text that is only whitespace is kept: it holds nothing, and the core
//! treats it as empty.

use std::collections::HashMap;

/// What becomes of a string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Text {
    /// Kept as it is.
    Kept,
    /// Replaced by an alias of the class, such as `host`.
    Alias(&'static str),
    /// Zero bytes of its length.
    Blank,
}

/// What becomes of a bytes field that is not a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bytes {
    Kept,
    Blank,
}

/// String fields by name, qualified by the message or type that holds them
/// where one name means different things; a value a variant holds with no
/// field name of its own is named by its variant (`Ssh`). A field not here
/// is blanked.
const TEXT: &[(&str, Text)] = &[
    // Hosts.
    ("Host.name", Text::Alias("host")),
    ("PairingIdentity.name", Text::Alias("host")),
    ("Candidate.name", Text::Alias("host")),
    ("TrustEntry.name", Text::Alias("host")),
    ("host", Text::Alias("host")),
    ("hosts", Text::Alias("host")),
    ("network_hosts", Text::Alias("host")),
    ("udp_blocked", Text::Alias("host")),
    ("ssh", Text::Alias("ssh")),
    ("Ssh", Text::Alias("ssh")),
    ("target", Text::Alias("ssh")),
    // Agents, files and the accounts they belong to.
    ("name", Text::Alias("name")),
    ("display_name", Text::Alias("name")),
    ("given", Text::Alias("name")),
    ("to", Text::Alias("name")),
    ("from", Text::Alias("name")),
    ("account", Text::Alias("account")),
    ("account_name", Text::Alias("account")),
    ("email", Text::Alias("account")),
    ("auth_token", Text::Alias("token")),
    ("staged_refresh_token", Text::Alias("token")),
    // Folders and branches.
    ("cwd", Text::Alias("path")),
    ("path", Text::Alias("path")),
    ("repo", Text::Alias("path")),
    ("dir", Text::Alias("path")),
    ("recent", Text::Alias("path")),
    ("replicas", Text::Alias("path")),
    ("agents", Text::Alias("path")),
    ("directories", Text::Alias("path")),
    ("roots", Text::Alias("path")),
    ("grant_root", Text::Alias("path")),
    ("install_path", Text::Alias("path")),
    ("socket_path", Text::Alias("path")),
    ("report_path", Text::Alias("path")),
    ("dump_path", Text::Alias("path")),
    ("removed_dirs", Text::Alias("path")),
    ("pinned", Text::Alias("path")),
    ("branch", Text::Alias("branch")),
    ("branches", Text::Alias("branch")),
    ("base_branch", Text::Alias("branch")),
    // Ids and keys the core matches rows by.
    ("id", Text::Kept),
    ("key", Text::Kept),
    ("item_key", Text::Kept),
    ("ask_key", Text::Kept),
    ("parent_key", Text::Kept),
    ("last_message_key", Text::Kept),
    ("check", Text::Kept),
    // The network's addresses, which the core parses and dials.
    ("addr", Text::Kept),
    ("addrs", Text::Kept),
    ("direct", Text::Kept),
    ("observed", Text::Kept),
    ("public_addr", Text::Kept),
    ("observed_addr", Text::Kept),
    // A pairing's secret: the conversation it opens is replayed, and its
    // randomness is in the record already.
    ("pin", Text::Kept),
    ("demo_pin", Text::Kept),
    ("WindowMethod.Demo", Text::Kept),
    // Versions and settings, which are ours, not the person's.
    ("version", Text::Kept),
    ("provider_version", Text::Kept),
    ("provider_name", Text::Kept),
    ("model", Text::Kept),
    ("models", Text::Kept),
    ("model_name", Text::Kept),
    ("resolved_model", Text::Kept),
    ("effort", Text::Kept),
    ("efforts", Text::Kept),
    ("default_effort", Text::Kept),
    ("mode", Text::Kept),
    ("permission", Text::Kept),
    ("approval_policy", Text::Kept),
    ("mime", Text::Kept),
];

/// Bytes fields that are kept; any other is blanked unless it is a
/// message, which is walked.
const BYTES_KEPT: &[&str] = &[
    "id",
    "hash",
    "agent",
    "agent_ids",
    "dst",
    "pubkey",
    "token",
    "qr_secret",
    // The pairing exchange's own values: the core checks each against
    // what it derived from the recorded randomness.
    "spake2_message",
    "key_confirmation",
    "sealed_identity",
];

/// How the string in `field` of `holder` (a message's or a type's short
/// name, or empty) is treated.
pub fn text(holder: &str, field: &str) -> Text {
    let qualified = format!("{holder}.{field}");
    TEXT.iter()
        .find(|(name, _)| *name == qualified)
        .or_else(|| TEXT.iter().find(|(name, _)| *name == field))
        .map_or(Text::Blank, |(_, text)| *text)
}

/// How the bytes in `field` are treated.
pub fn bytes(field: &str) -> Bytes {
    if field.ends_with("_id") || BYTES_KEPT.contains(&field) {
        Bytes::Kept
    } else {
        Bytes::Blank
    }
}

/// Zero bytes of `text`'s length; text that is only whitespace is kept.
pub fn blank(text: &str) -> String {
    if text.trim().is_empty() {
        return text.to_owned();
    }
    "\0".repeat(text.len())
}

/// The aliases of one record: each distinct value gets the next number of
/// the class it was first seen under, and keeps it wherever it appears,
/// whatever field holds it. A value ending in `-<number>` keeps that
/// suffix on its stem's alias, so a name and the name the core derived
/// from it by suffix stay related.
pub struct Aliases {
    of: HashMap<String, String>,
    counts: HashMap<&'static str, u32>,
    kept: fn(&str) -> bool,
}

impl Aliases {
    /// Aliases that keep every value `kept` says is not the person's, such
    /// as a name the core itself made up.
    pub fn new(kept: fn(&str) -> bool) -> Aliases {
        Aliases {
            of: HashMap::new(),
            counts: HashMap::new(),
            kept,
        }
    }

    pub fn alias(&mut self, class: &'static str, value: &str) -> String {
        if value.is_empty() || (self.kept)(value) {
            return value.to_owned();
        }
        if let Some((stem, number)) = value.rsplit_once('-')
            && !stem.is_empty()
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
        {
            return format!("{}-{number}", self.alias(class, stem));
        }
        if let Some(alias) = self.of.get(value) {
            return alias.clone();
        }
        let count = self.counts.entry(class).or_default();
        *count += 1;
        let alias = format!("{class}-{count}");
        self.of.insert(value.to_owned(), alias.clone());
        alias
    }

    /// The string in `field` of `holder`, as the table treats it.
    pub fn text(&mut self, holder: &str, field: &str, value: &str) -> String {
        match text(holder, field) {
            Text::Kept => value.to_owned(),
            Text::Alias(class) => self.alias(class, value),
            Text::Blank => blank(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_values_alias_alike_and_keep_a_derived_suffix() {
        let mut aliases = Aliases::new(|name| name == "quiet-otter");
        let desk = aliases.alias("host", "desk zebra");
        assert_eq!(desk, "host-1");
        assert_eq!(aliases.alias("name", "desk zebra"), desk, "one map");
        assert_eq!(aliases.alias("name", "worker"), "name-1");
        assert_eq!(aliases.alias("name", "worker-2"), "name-1-2");
        assert_eq!(aliases.alias("name", "quiet-otter"), "quiet-otter");
        assert_eq!(aliases.alias("name", ""), "");
    }

    #[test]
    fn the_table_decides_by_field_name() {
        let mut aliases = Aliases::new(|_| false);
        assert_eq!(aliases.text("Host", "name", "desk"), "host-1");
        assert_eq!(aliases.text("Agent", "cwd", "/src/zebra"), "path-1");
        assert_eq!(aliases.text("Item", "text", "a zebra"), "\0".repeat(7));
        assert_eq!(aliases.text("Item", "text", "  \n"), "  \n");
        assert_eq!(aliases.text("Item", "key", "toolu_1"), "toolu_1");
        assert_eq!(
            aliases.text("Error", "message", "Zebra failed"),
            "\0".repeat(12)
        );
        assert_eq!(
            aliases.text("Error", "message", "PAIRING_LOCKED"),
            "\0".repeat(14),
            "a refusal is told apart by its typed reason, never its words"
        );
        assert_eq!(bytes("agent_id"), Bytes::Kept);
        assert_eq!(bytes("body"), Bytes::Blank);
    }
}
