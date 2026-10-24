//! What a headless Claude session costs on the wire: a recording replayed
//! through the interpreter, and the encoded size of what it journals.
//!
//! Sizes are of the records as the interpreter writes them, before the
//! owning daemon stamps the agent id, order and revision on them. Offline
//! and deterministic: the same recording gives the same numbers.

use std::path::Path;

use interpret::claude_sdk::ClaudeSdk;
use prost::Message as _;
use serde::Serialize;
use wire::Step;
use wire::claude_sdk_item::Kind;

/// The numbers for one recording.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WireSize {
    pub snapshots: Snapshots,
    /// Every tool call, in the order it first appeared.
    pub commands: Vec<CommandOutput>,
}

/// Every snapshot the replay journaled, each replacing the last whole.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Snapshots {
    pub count: usize,
    pub largest_bytes: usize,
    /// The middle snapshot by size; the lower of the two middle ones when
    /// the count is even, so it is always a size some snapshot had.
    pub median_bytes: usize,
}

/// What one tool call's item cost: each time it was sent whole, and each
/// append to its text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CommandOutput {
    pub key: String,
    pub tool: String,
    /// The shell command, for a call that has one.
    pub command: Option<String>,
    pub background: bool,
    pub items: usize,
    pub appends: usize,
    /// Encoded bytes of those items and appends together.
    pub bytes: usize,
}

/// Replays a headless Claude `io.jsonl` recording through its interpreter
/// and measures what was journaled.
pub fn wire_size(recording: &Path) -> Result<WireSize, String> {
    let recording = std::path::absolute(recording)
        .map_err(|error| format!("{}: {error}", recording.display()))?;
    // The interpreter's replay reads a fixture that names the recording.
    let fixture = tempfile::Builder::new()
        .suffix(".json")
        .tempfile()
        .map_err(|error| format!("a fixture file: {error}"))?;
    let named = serde_json::json!({
        "spec": { "created_at_ms": 0 },
        "recording": { "path": recording, "format": "claude_sdk_io" },
    });
    std::fs::write(fixture.path(), named.to_string())
        .map_err(|error| format!("a fixture file: {error}"))?;
    let steps: Vec<Step> = interpret::replay::<ClaudeSdk>(fixture.path())?
        .into_iter()
        .map(|replayed| replayed.step)
        .collect();
    Ok(measure(&steps))
}

/// Measures journaled steps: snapshot sizes, and per tool call the bytes
/// its items and appends took.
pub fn measure(steps: &[Step]) -> WireSize {
    let mut sizes: Vec<usize> = steps
        .iter()
        .filter_map(|step| step.snapshot.as_ref())
        .map(|snapshot| snapshot.encoded_len())
        .collect();
    sizes.sort_unstable();
    let snapshots = Snapshots {
        count: sizes.len(),
        largest_bytes: sizes.last().copied().unwrap_or(0),
        median_bytes: sizes
            .get(sizes.len().saturating_sub(1) / 2)
            .copied()
            .unwrap_or(0),
    };

    let mut commands: Vec<CommandOutput> = Vec::new();
    for step in steps {
        for item in &step.items {
            let Some(call) = tool_call(&item.body) else {
                continue;
            };
            let at = match commands.iter().position(|command| command.key == item.key) {
                Some(at) => at,
                None => {
                    commands.push(CommandOutput {
                        key: item.key.clone(),
                        tool: String::new(),
                        command: None,
                        background: false,
                        items: 0,
                        appends: 0,
                        bytes: 0,
                    });
                    commands.len() - 1
                }
            };
            let command = &mut commands[at];
            // The newest item says what the call is.
            command.tool = call.name.clone();
            command.command = shell_command(&call);
            command.background = call.background;
            command.items += 1;
            command.bytes += item.encoded_len();
        }
        for append in &step.appends {
            if let Some(command) = commands
                .iter_mut()
                .find(|command| command.key == append.key)
            {
                command.appends += 1;
                command.bytes += append.encoded_len();
            }
        }
    }
    WireSize {
        snapshots,
        commands,
    }
}

fn tool_call(body: &[u8]) -> Option<wire::ToolCall> {
    match wire::ClaudeSdkItem::decode(body).ok()?.kind? {
        Kind::Tool(call) => Some(call),
        _ => None,
    }
}

fn shell_command(call: &wire::ToolCall) -> Option<String> {
    let input = call.input.as_ref()?;
    matches!(input.of, Some(wire::tool_input::Of::Shell(_))).then(|| input.subject.clone())
}
