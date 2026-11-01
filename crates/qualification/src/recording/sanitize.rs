use redaction::{Redaction, RedactionSummary, redact_text, redact_value};
use serde_json::Value;

use super::IoEvent;

/// Strip secrets, machine paths, and personal identifiers from raw traffic.
pub fn sanitize(io: &mut [IoEvent], rules: &Redaction) -> RedactionSummary {
    let mut summary = RedactionSummary::default();
    for event in io {
        match serde_json::from_str::<Value>(&event.line) {
            Ok(mut frame) => {
                redact_value(&mut frame, rules, &mut summary);
                event.line = serde_json::to_string(&frame)
                    .expect("serializing a serde_json::Value cannot fail");
            }
            Err(_) => event.line = redact_text(&event.line, rules, &mut summary),
        }
    }
    summary
}
