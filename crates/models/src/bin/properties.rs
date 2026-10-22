//! Lists every property of the protocol's models, one JSON object a line:
//! `{"machine": ..., "kind": "always" | "sometimes" | "eventually",
//! "sentence": ...}`. The contracts check reads it to find the property a
//! `model` contract names.

fn main() {
    for listed in models::properties() {
        let kind = match listed.kind {
            models::PropertyKind::Always => "always",
            models::PropertyKind::Sometimes => "sometimes",
            models::PropertyKind::Eventually => "eventually",
        };
        println!(
            "{}",
            serde_json::json!({
                "machine": listed.model,
                "kind": kind,
                "sentence": listed.sentence,
            })
        );
    }
}
