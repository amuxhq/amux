//! The cell a notebook edit replaces. Claude's request to edit a notebook
//! names the cell and its new source, not what is there now, so an approver
//! reading amux's card saw only added lines. When Claude asks, the host
//! reads the cell from the notebook and adds its source to the request's
//! payload under [`interpret::NOTEBOOK_OLD_SOURCE`]; the card then shows
//! it as the lines removed.

use std::path::Path;

use serde_json::Value;

const TOOL: &str = "NotebookEdit";

/// A terminal Claude hook payload, with the cell its notebook edit replaces
/// when it is a permission request for one. Any other payload is returned
/// as it came.
pub(crate) fn annotate_hook(payload: Vec<u8>) -> Vec<u8> {
    if !mentions_tool(&payload) {
        return payload;
    }
    let Ok(Value::Object(mut fields)) = serde_json::from_slice::<Value>(&payload) else {
        return payload;
    };
    if fields.get("hook_event_name").and_then(Value::as_str) != Some("PermissionRequest")
        || fields.get("tool_name").and_then(Value::as_str) != Some(TOOL)
    {
        return payload;
    }
    let cwd = fields
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(old) = fields
        .get("tool_input")
        .and_then(|input| old_source(input, Path::new(cwd)))
    else {
        return payload;
    };
    fields.insert(
        interpret::NOTEBOOK_OLD_SOURCE.to_owned(),
        Value::String(old),
    );
    serde_json::to_vec(&fields).unwrap_or(payload)
}

/// A line of Claude's stream-json output, with the cell its notebook edit
/// replaces when it is a request to use the notebook tool. Any other line
/// is returned as it came.
pub(crate) fn annotate_stream(line: String, cwd: &Path) -> String {
    if !mentions_tool(line.as_bytes()) {
        return line;
    }
    let Ok(Value::Object(mut frame)) = serde_json::from_str::<Value>(&line) else {
        return line;
    };
    if frame.get("type").and_then(Value::as_str) != Some("control_request") {
        return line;
    }
    let Some(Value::Object(request)) = frame.get_mut("request") else {
        return line;
    };
    if request.get("subtype").and_then(Value::as_str) != Some("can_use_tool")
        || request.get("tool_name").and_then(Value::as_str) != Some(TOOL)
    {
        return line;
    }
    let Some(old) = request
        .get("input")
        .and_then(|input| old_source(input, cwd))
    else {
        return line;
    };
    request.insert(
        interpret::NOTEBOOK_OLD_SOURCE.to_owned(),
        Value::String(old),
    );
    serde_json::to_string(&frame).unwrap_or(line)
}

fn mentions_tool(bytes: &[u8]) -> bool {
    bytes
        .windows(TOOL.len())
        .any(|window| window == TOOL.as_bytes())
}

/// The source of the cell an edit replaces or deletes, read from its
/// notebook. Nothing for an insert, or when the notebook or the cell cannot
/// be read. A cell is named by its id, or as Claude names cells that have
/// none, "cell-N" counting from 0.
fn old_source(input: &Value, cwd: &Path) -> Option<String> {
    let field = |name: &str| input.get(name).and_then(Value::as_str).unwrap_or_default();
    if field("edit_mode") == "insert" {
        return None;
    }
    let cell_id = field("cell_id");
    let path = cwd.join(field("notebook_path"));
    let notebook: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let cells = notebook.get("cells")?.as_array()?;
    let cell = cells
        .iter()
        .find(|cell| cell.get("id").and_then(Value::as_str) == Some(cell_id))
        .or_else(|| {
            let index: usize = cell_id.strip_prefix("cell-")?.parse().ok()?;
            cells.get(index)
        })?;
    Some(match cell.get("source")? {
        Value::String(source) => source.clone(),
        Value::Array(lines) => lines.iter().filter_map(Value::as_str).collect(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn notebook(dir: &Path) {
        let cells = json!({"cells": [
            {"cell_type": "code", "source": ["print(1)"]},
            {"cell_type": "code", "id": "b2", "source": "x = 2\nprint(x)"},
        ]});
        std::fs::write(dir.join("n.ipynb"), cells.to_string()).unwrap();
    }

    fn old_of(payload: &[u8], at: &[&str]) -> Option<String> {
        let mut value: Value = serde_json::from_slice(payload).unwrap();
        for key in at {
            value = value.get(*key)?.clone();
        }
        value
            .get(interpret::NOTEBOOK_OLD_SOURCE)
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    #[test]
    fn a_notebook_edit_request_carries_the_cell_it_replaces() {
        let dir = tempfile::tempdir().unwrap();
        notebook(dir.path());
        let hook = |cell: &str, mode: &str| {
            json!({
                "hook_event_name": "PermissionRequest",
                "cwd": dir.path(),
                "tool_name": "NotebookEdit",
                "tool_input": {"notebook_path": "n.ipynb", "cell_id": cell,
                    "new_source": "print(10)", "edit_mode": mode},
            })
            .to_string()
            .into_bytes()
        };
        assert_eq!(
            old_of(&annotate_hook(hook("cell-0", "replace")), &[]).as_deref(),
            Some("print(1)")
        );
        assert_eq!(
            old_of(&annotate_hook(hook("b2", "delete")), &[]).as_deref(),
            Some("x = 2\nprint(x)")
        );
        assert_eq!(old_of(&annotate_hook(hook("b2", "insert")), &[]), None);
        let other = br#"{"hook_event_name":"PreToolUse","tool_name":"NotebookEdit"}"#.to_vec();
        assert_eq!(annotate_hook(other.clone()), other);

        let line = json!({
            "type": "control_request",
            "request_id": "r1",
            "request": {"subtype": "can_use_tool", "tool_name": "NotebookEdit",
                "tool_use_id": "t1",
                "input": {"notebook_path": dir.path().join("n.ipynb"), "cell_id": "cell-1",
                    "new_source": "print(20)"}},
        })
        .to_string();
        let annotated = annotate_stream(line, Path::new("/elsewhere"));
        assert_eq!(
            old_of(annotated.as_bytes(), &["request"]).as_deref(),
            Some("x = 2\nprint(x)")
        );
    }
}
