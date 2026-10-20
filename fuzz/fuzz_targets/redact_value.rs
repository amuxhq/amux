//! Any JSON a report or capture carries: redaction walks it whole and
//! leaves a value that still serializes, never panicking.

#![no_main]

use libfuzzer_sys::fuzz_target;
use redaction::{Redaction, RedactionSummary, redact_value};

fuzz_target!(|bytes: &[u8]| {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return;
    };
    let rules = Redaction {
        home: "/Users/someone".into(),
        extra_paths: vec!["/private/var/folders/xy".into()],
        secret_env: vec!["sk-test-0123456789".into()],
        hostname: Some("desk".into()),
        user: Some("someone".into()),
        extra_personal_identifiers: vec!["someone@example.com".into()],
        personal_identifier_keys: vec!["email".into()],
    };
    redact_value(&mut value, &rules, &mut RedactionSummary::default());
    serde_json::to_vec(&value).unwrap();
});
