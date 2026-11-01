//! Recordings played back verbatim, for conformance.
//!
//! A live recording is the provider's side of a real
//! session: every line or byte run it wrote on each channel the host reads,
//! and every one the host wrote to it, in the order they happened. Played
//! back, a fake binary becomes that provider for one of its processes: it
//! writes each recorded output where the real one did and checks each host
//! input against the recording byte for byte, so the recording is a script
//! the fake can play and the fake's transports are the provider's.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// One channel a provider process speaks on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// What the host wrote: stdin lines, or bytes typed into the terminal.
    Input,
    /// What the provider wrote to stdout or its terminal.
    Output,
    /// A row the provider appended to its session transcript.
    Transcript,
    /// A payload the provider handed a hook command on stdin.
    Hook,
    /// The provider process ended here with the exit code in the bytes,
    /// as when a provider dies mid-turn. Host-side fixtures write it as
    /// `{"dir":"exit","line":"<code>"}`; recorded corpora end with the
    /// host closing the provider's input instead.
    Exit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub channel: Channel,
    pub bytes: Vec<u8>,
}

/// One provider process of a recording.
#[derive(Clone, Debug)]
pub struct Process {
    pub transport: String,
    pub argv: Vec<String>,
    pub events: Vec<Event>,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} line {line}: {reason}")]
    Malformed {
        path: PathBuf,
        line: usize,
        reason: String,
    },
    #[error("{0} has no transport {1:?}")]
    NoTransport(PathBuf, String),
}

/// Rows the recording harness wrote into the transcript stream as markers,
/// never written by the provider.
const HARNESS_ROW_PREFIX: &str = "amux.";

/// The processes of a recording, in the order they ran.
pub fn load(dir: &Path) -> Result<Vec<Process>, LoadError> {
    let io = dir.join("io.jsonl");
    let text = std::fs::read_to_string(&io).map_err(|source| LoadError::Read {
        path: io.clone(),
        source,
    })?;
    let spawn = dir.join("spawn.jsonl");
    let spawns = std::fs::read_to_string(&spawn).unwrap_or_default();
    let mut processes: Vec<Process> = Vec::new();
    for (index, line) in spawns
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        let malformed = |reason: String| LoadError::Malformed {
            path: spawn.clone(),
            line: index + 1,
            reason,
        };
        let value: Value =
            serde_json::from_str(line).map_err(|error| malformed(error.to_string()))?;
        let argv = value
            .get("argv")
            .or_else(|| value.get("args"))
            .and_then(Value::as_array)
            .map(|args| {
                args.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        processes.push(Process {
            transport: transport_of(&value),
            argv,
            events: Vec::new(),
        });
    }
    for (index, line) in text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        let malformed = |reason: String| LoadError::Malformed {
            path: io.clone(),
            line: index + 1,
            reason,
        };
        let value: Value =
            serde_json::from_str(line).map_err(|error| malformed(error.to_string()))?;
        let dir = value.get("dir").and_then(Value::as_str).unwrap_or_default();
        let recorded = value
            .get("line")
            .and_then(Value::as_str)
            .ok_or_else(|| malformed("no line".into()))?;
        let transport = transport_of(&value);
        let (channel, process) = match transport.as_str() {
            _ if dir == "exit" => (
                Channel::Exit,
                if transport == "stdio" { "stdio" } else { "pty" },
            ),
            // A terminal recording's transports are channels of one process.
            "pty" => (
                if dir == "stdin" {
                    Channel::Input
                } else {
                    Channel::Output
                },
                "pty",
            ),
            "transcript" => (Channel::Transcript, "pty"),
            "hook" => (Channel::Hook, "pty"),
            other => (
                if dir == "stdin" {
                    Channel::Input
                } else {
                    Channel::Output
                },
                other,
            ),
        };
        let bytes = match recorded.strip_prefix("hex:") {
            Some(hex) => decode_hex(hex).ok_or_else(|| malformed("bad hex".into()))?,
            None => recorded.as_bytes().to_vec(),
        };
        let bytes = if channel == Channel::Exit {
            if recorded.trim().parse::<i32>().is_err() {
                return Err(malformed("an exit code that is not a number".into()));
            }
            bytes
        } else if channel == Channel::Transcript {
            let row = transcript_row(recorded).ok_or_else(|| malformed("bad row".into()))?;
            let kind = serde_json::from_str::<Value>(row)
                .ok()
                .and_then(|row| row.get("type")?.as_str().map(str::to_owned));
            if kind.is_some_and(|kind| kind.starts_with(HARNESS_ROW_PREFIX)) {
                continue;
            }
            row.as_bytes().to_vec()
        } else {
            bytes
        };
        let event = Event { channel, bytes };
        match processes.iter_mut().find(|p| p.transport == process) {
            Some(found) => found.events.push(event),
            None => processes.push(Process {
                transport: process.to_owned(),
                argv: Vec::new(),
                events: vec![event],
            }),
        }
    }
    processes.retain(|process| !process.events.is_empty());
    Ok(processes)
}

/// The exit code an exit event carries.
pub fn exit_code(event: &Event) -> i32 {
    String::from_utf8_lossy(&event.bytes)
        .trim()
        .parse()
        .unwrap_or(1)
}

/// The recorded process with this transport id.
pub fn process(dir: &Path, transport: &str) -> Result<Process, LoadError> {
    load(dir)?
        .into_iter()
        .find(|process| process.transport == transport)
        .ok_or_else(|| LoadError::NoTransport(dir.to_owned(), transport.to_owned()))
}

/// The playback selector a fake reads: `<recording dir>#<transport id>`.
pub fn selector(dir: &Path, transport: &str) -> String {
    format!("{}#{transport}", dir.display())
}

pub fn parse_selector(selector: &str) -> (PathBuf, String) {
    match selector.rsplit_once('#') {
        Some((dir, transport)) => (PathBuf::from(dir), transport.to_owned()),
        None => (PathBuf::from(selector), String::new()),
    }
}

/// The row's own bytes out of the recorder's `{"path":…,"row":…}` wrapper,
/// cut from the text so the provider's number formatting survives.
fn transcript_row(line: &str) -> Option<&str> {
    let wrapper: Value = serde_json::from_str(line).ok()?;
    let path = serde_json::to_string(wrapper.get("path")?).ok()?;
    line.strip_prefix(&format!("{{\"path\":{path},\"row\":"))?
        .strip_suffix('}')
}

fn transport_of(value: &Value) -> String {
    value
        .get("transport_id")
        .and_then(Value::as_str)
        .unwrap_or("stdio")
        .to_owned()
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(hex.get(index..index + 2)?, 16).ok())
        .collect()
}

/// Where a Claude session's transcript lives, as Claude lays it out: the
/// configuration directory's `projects/<cwd with every non-alphanumeric
/// character replaced by '-'>/<session id>.jsonl`.
pub fn transcript_path(config_dir: &Path, cwd: &Path, session: &str) -> PathBuf {
    let slug: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    config_dir
        .join("projects")
        .join(slug)
        .join(format!("{session}.jsonl"))
}

/// A recorded hook payload as a fake hands it over on this machine: a
/// transcript path the recorder sanitised (as a machine path, or as a
/// numbered transcript by newer recorders) becomes the file the fake writes
/// that session's rows to, so a host following the path reads them.
pub fn localize_hook(payload: &[u8], transcript: &Path) -> Vec<u8> {
    const KEY: &str = r#""transcript_path":""#;
    let Ok(text) = std::str::from_utf8(payload) else {
        return payload.to_vec();
    };
    let Some(start) = text.find(KEY).map(|at| at + KEY.len()) else {
        return payload.to_vec();
    };
    let Some(end) = text[start..].find('"').map(|len| start + len) else {
        return payload.to_vec();
    };
    let recorded = &text[start..end];
    let sanitised = recorded == "<MACHINE_PATH>"
        || recorded
            .strip_prefix("<TRANSCRIPT_")
            .and_then(|rest| rest.strip_suffix('>'))
            .is_some_and(|number| number.bytes().all(|byte| byte.is_ascii_digit()));
    if !sanitised {
        return payload.to_vec();
    }
    let local = Value::String(transcript.display().to_string()).to_string();
    format!("{}{local}{}", &text[..start - 1], &text[end + 1..]).into_bytes()
}

/// Every hook payload of a recorded process, localized as the fake hands
/// them over when it plays in `cwd` with `config` as Claude's directory.
pub fn local_hooks(events: &[Event], config: &Path, cwd: &Path) -> Vec<Vec<u8>> {
    let mut session = String::from("unnamed");
    let mut hooks = Vec::new();
    for event in events {
        match event.channel {
            Channel::Hook | Channel::Transcript => {
                if let Some(named) = row_session(&event.bytes) {
                    session = named;
                }
            }
            _ => continue,
        }
        if event.channel == Channel::Hook {
            hooks.push(localize_hook(
                &event.bytes,
                &transcript_path(config, cwd, &session),
            ));
        }
    }
    hooks
}

/// Claude's configuration directory: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn claude_config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude")
        })
}

/// The session a transcript row belongs to, where it names one.
pub fn row_session(row: &[u8]) -> Option<String> {
    let row: Value = serde_json::from_slice(row).ok()?;
    row.get("sessionId")
        .or_else(|| row.get("session_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A recording's transcript rows grouped the way a fake writes them: each
/// row goes to the file of the last session any row or hook payload named.
pub fn transcript_files(events: &[Event]) -> Vec<(String, Vec<Vec<u8>>)> {
    let mut files: Vec<(String, Vec<Vec<u8>>)> = Vec::new();
    let mut current = String::from("unnamed");
    for event in events {
        match event.channel {
            Channel::Hook | Channel::Transcript => {
                if let Some(session) = row_session(&event.bytes) {
                    current = session;
                }
            }
            _ => continue,
        }
        if event.channel != Channel::Transcript {
            continue;
        }
        match files.iter_mut().find(|(session, _)| *session == current) {
            Some((_, rows)) => rows.push(event.bytes.clone()),
            None => files.push((current.clone(), vec![event.bytes.clone()])),
        }
    }
    files
}
