//! Any text a report or log carries: redaction returns text with none of
//! the capture's literal secrets left in it, never panicking.

#![no_main]

use libfuzzer_sys::fuzz_target;
use redaction::{Redaction, RedactionSummary, redact_text};

const SECRET: &str = "sk-test-0123456789";

fuzz_target!(|bytes: &[u8]| {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    let rules = Redaction {
        home: "/Users/someone".into(),
        extra_paths: vec!["/private/var/folders/xy".into()],
        secret_env: vec![SECRET.into()],
        hostname: Some("desk".into()),
        user: Some("someone".into()),
        extra_personal_identifiers: vec!["someone@example.com".into()],
        personal_identifier_keys: vec!["email".into()],
    };
    let redacted = redact_text(text, &rules, &mut RedactionSummary::default());
    assert!(
        !redacted.contains(SECRET),
        "{text:?} kept the secret: {redacted:?}"
    );
});
