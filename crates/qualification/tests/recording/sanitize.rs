//! Sanitizing a capture redacts the recording machine's paths, secrets and
//! identifiers wherever they nest, and keeps package and version names.

use std::path::PathBuf;

use qualification::recording::{IoDirection, IoEvent, sanitize};
use redaction::{
    EMAIL_PLACEHOLDER, IDENTIFIER_PLACEHOLDER, PATH_PLACEHOLDER, Redaction, RedactionSummary,
    SECRET_PLACEHOLDER, redact_text,
};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

fn event(line: Value) -> IoEvent {
    IoEvent {
        us: 0,
        direction: IoDirection::Read,
        line: serde_json::to_string(&line).unwrap(),
        transport_id: None,
        session_id: None,
    }
}

#[test]
fn sanitize_redacts_nested_values_and_reports_each_category() {
    let mut io = vec![
        event(serde_json::json!({
            "type": "control_request",
            "api_key": "sk-ant-secret",
            "cwd": "/Users/alice/project",
            "message": {
                "note": "email alice@example.com and token exact-secret",
                "paths": ["/srv/operator/project", "relative/file"]
            },
            "installationId": "2b93020b-f38b-47de-ae2f-d9885611b5f0",
            "serverName": "Alices-Laptop.local"
        })),
        event(serde_json::json!({
            "type": "system",
            "text": "read /tmp/work/file with Bearer abc123"
        })),
    ];
    let rules = Redaction {
        home: PathBuf::from("/Users/alice"),
        extra_paths: vec![PathBuf::from("/srv/operator")],
        secret_env: vec!["exact-secret".to_string()],
        hostname: None,
        user: None,
        extra_personal_identifiers: Vec::new(),
        personal_identifier_keys: vec!["installationId".into(), "serverName".into()],
    };

    let summary = sanitize(&mut io, &rules);
    let sanitized = io
        .iter()
        .map(|event| event.line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!sanitized.contains("sk-ant-secret"));
    assert!(!sanitized.contains("alice@example.com"));
    assert!(!sanitized.contains("exact-secret"));
    assert!(!sanitized.contains("/Users/alice"));
    assert!(!sanitized.contains("/srv/operator"));
    assert!(!sanitized.contains("/tmp/work"));
    assert!(!sanitized.contains("abc123"));
    assert!(sanitized.contains(SECRET_PLACEHOLDER));
    assert!(sanitized.contains(EMAIL_PLACEHOLDER));
    assert!(sanitized.contains(IDENTIFIER_PLACEHOLDER));
    assert!(sanitized.contains(PATH_PLACEHOLDER));
    assert_eq!(
        summary,
        RedactionSummary {
            secrets: 3,
            machine_paths: 3,
            personal_identifiers: 3,
        }
    );

    let repeated = io.clone();
    assert_eq!(sanitize(&mut io, &rules), RedactionSummary::default());
    assert_eq!(io, repeated, "sanitization must be idempotent");
}

#[test]
fn redact_text_removes_local_and_extra_identifiers() {
    let rules = Redaction {
        hostname: Some("alices-laptop.local".to_string()),
        user: Some("alice".to_string()),
        extra_personal_identifiers: vec!["daily-driver".to_string()],
        ..Redaction::default()
    };
    let mut summary = RedactionSummary::default();

    let redacted = redact_text(
        "alice captured this on alices-laptop.local as daily-driver",
        &rules,
        &mut summary,
    );

    assert_eq!(
        redacted,
        "<REDACTED_IDENTIFIER> captured this on <REDACTED_IDENTIFIER> as <REDACTED>__"
    );
    assert_eq!(summary.personal_identifiers, 3);
    assert_eq!(
        UnicodeWidthStr::width("<REDACTED>__"),
        UnicodeWidthStr::width("daily-driver")
    );
}

#[test]
fn sanitize_scopes_identifier_keys_to_the_capture_and_exact_field_name() {
    let mut io = vec![event(serde_json::json!({
        "remoteControl": {
            "serverName": "Alices-Laptop.local"
        },
        "mcp": {
            "server_name": "spec"
        }
    }))];
    let rules = Redaction {
        personal_identifier_keys: vec!["serverName".into()],
        ..Redaction::default()
    };

    assert_eq!(
        sanitize(&mut io, &rules),
        RedactionSummary {
            personal_identifiers: 1,
            ..RedactionSummary::default()
        }
    );
    let sanitized: Value = serde_json::from_str(&io[0].line).unwrap();
    assert_eq!(
        sanitized["remoteControl"]["serverName"],
        IDENTIFIER_PLACEHOLDER
    );
    assert_eq!(sanitized["mcp"]["server_name"], "spec");
}

#[test]
fn sanitize_redacts_bridge_session_identifiers() {
    let mut io = vec![event(serde_json::json!({
        "path": "<MACHINE_PATH>",
        "row": {
            "type": "bridge-session",
            "bridgeSessionId": "private-bridge-id",
            "ownerAccountUuid": "private-account-id",
            "ownerOrganizationUuid": "private-organization-id",
            "sessionId": "replay-session-id"
        }
    }))];

    assert_eq!(
        sanitize(&mut io, &Redaction::default()),
        RedactionSummary {
            personal_identifiers: 3,
            ..RedactionSummary::default()
        }
    );
    let sanitized: Value = serde_json::from_str(&io[0].line).unwrap();
    assert_eq!(sanitized["row"]["bridgeSessionId"], IDENTIFIER_PLACEHOLDER);
    assert_eq!(sanitized["row"]["ownerAccountUuid"], IDENTIFIER_PLACEHOLDER);
    assert_eq!(
        sanitized["row"]["ownerOrganizationUuid"],
        IDENTIFIER_PLACEHOLDER
    );
    assert_eq!(sanitized["row"]["sessionId"], "replay-session-id");
}

#[test]
fn sanitize_preserves_package_and_version_identifiers() {
    let mut io = vec![event(serde_json::json!({
        "values": [
            "agent-sdk@0.3.247",
            "stripe@claude-plugins-official",
            "rust-analyzer-lsp@1.0.0"
        ]
    }))];

    assert_eq!(
        sanitize(&mut io, &Redaction::default()),
        RedactionSummary::default()
    );
    assert!(io[0].line.contains("agent-sdk@0.3.247"));
}
