//! Every integration suite of the `claude-protocol` crate, one module each, in one
//! test binary: one link per crate instead of one per suite.

mod frames;
mod pty_corpus;
mod stream_corpus;

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The Claude corpus both corpus suites read.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../claude-specs/fixtures")
}

/// Where two JSON values differ, as paths with what each side holds.
fn differ(path: &str, written: &Value, encoded: &Value, out: &mut Vec<String>) {
    match (written, encoded) {
        (Value::Object(a), Value::Object(b)) => {
            for key in a.keys().chain(b.keys().filter(|key| !a.contains_key(*key))) {
                let inner = format!("{path}.{key}");
                match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => differ(&inner, x, y, out),
                    (x, y) => out.push(format!("{inner} written {x:?} encoded {y:?}")),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (index, (x, y)) in a.iter().zip(b).enumerate() {
                differ(&format!("{path}[{index}]"), x, y, out);
            }
        }
        (a, b) if a != b => out.push(format!("{path} written {a} encoded {b}")),
        _ => {}
    }
}
