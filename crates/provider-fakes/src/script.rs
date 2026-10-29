//! The authored script a fake provider plays.
//!
//! A script is the model's side of a session: what it says, which tools it
//! calls, what it asks, and when it stops. How the provider reacts to the
//! host — handshakes, echoes of what was sent, queueing and folding of
//! messages written mid-turn, answers to control requests — is the fake's
//! protocol engine, not the script, so every script gets the same provider
//! behaviour.
//!
//! Steps run in order. When the fake is idle it waits for the next prompt
//! (or injected message) and then plays steps until a [`Step::TurnEnd`]. An
//! [`Step::Ask`] blocks until the host answers it. A [`Step::WaitFor`]
//! blocks until a file exists, so a test can hold a turn open while it
//! kills a daemon; a [`Step::Pause`] keeps it busy for a while. When the steps run out the fake stays idle until its
//! input closes, then exits 0; [`Step::Exit`] ends the process early.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The environment variable naming the script a fake plays.
pub const SCRIPT_ENV: &str = "AMUX_FAKE_SCRIPT";
/// The environment variable naming a recording a fake plays back verbatim.
pub const PLAYBACK_ENV: &str = "AMUX_FAKE_PLAYBACK";
/// The environment variable naming a file a stdio fake appends every line
/// its host writes to, so a test can see exactly what the provider got.
pub const INPUT_LOG_ENV: &str = "AMUX_FAKE_INPUT_LOG";

/// Start `sh -c command` in a session of its own and leave it running.
pub fn spawn_detached(command: &str) {
    let mut shell = std::process::Command::new("sh");
    shell
        .args(["-c", command])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: setsid is async-signal-safe and touches nothing shared.
        unsafe {
            shell.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    if let Err(error) = shell.spawn() {
        eprintln!("spawn {command}: {error}");
    }
}

/// Append one host line to the input log, when the environment names one.
pub fn log_input(line: &str) {
    use std::io::Write as _;

    let Some(path) = std::env::var_os(INPUT_LOG_ENV) else {
        return;
    };
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{line}"));
    if let Err(error) = appended {
        eprintln!("input log {}: {error}", Path::new(&path).display());
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    #[serde(default)]
    pub steps: Vec<Step>,
    /// The model the session reports when the launch names none.
    #[serde(default)]
    pub model: Option<String>,
    /// The models the session offers: headless Claude's initialize answer,
    /// Codex's `model/list`. None scripted offers the session's model with
    /// efforts low, medium and high.
    #[serde(default)]
    pub models: Vec<OfferedModel>,
    /// The commands the session offers: headless Claude's slash commands,
    /// Codex's skills.
    #[serde(default)]
    pub commands: Vec<OfferedCommand>,
    /// Terminal Claude offers auto mode. Its permission menu offers to
    /// switch to it, as 2.1.283 does in manual mode: "Yes, and switch to
    /// auto mode" sits third and No fourth. Its plan menu starts in it, as
    /// 2.1.296 does: "Yes, and use auto mode" first, "Yes, manually approve
    /// edits" second, and no entry that accepts edits.
    #[serde(default)]
    pub offers_auto_mode: bool,
    /// Terminal Claude has not been told to trust its working directory:
    /// its first screen asks, with No preselected, and No or Escape exits
    /// with code 1.
    #[serde(default)]
    pub untrusted_folder: bool,
    /// The tool servers headless Claude reports at the start of each turn,
    /// with their state (`connected`, `failed`, `needs-auth`).
    #[serde(default)]
    pub servers: Vec<ServerState>,
    /// The context the session reports in use, in tokens, on each message
    /// (headless Claude) or at each turn's end (Codex).
    #[serde(default)]
    pub context_tokens: Option<u64>,
    /// How long the provider takes between a message's chunks, where it
    /// streams them, as a model writing does.
    #[serde(default)]
    pub chunk_ms: u64,
    /// How long each completed turn says it took (headless Claude's result
    /// `duration_ms`, which a chat shows as "Worked 6m"); 1 ms when unset.
    #[serde(default)]
    pub turn_ms: Option<u64>,
    /// File tools change the files they name, as the real tools do: a
    /// Write writes its content, an Edit replaces its old string, a Codex
    /// patch writes each file it adds. Off by default, so a script's paths
    /// need not exist.
    #[serde(default)]
    pub edit_files: bool,
    /// The version `--version` prints and the handshake reports; the
    /// fake's own when unset.
    #[serde(default)]
    pub version: Option<String>,
    /// The provider is signed out of its account: headless Claude's
    /// initialize answer names no account, Codex's `account/read` none.
    #[serde(default)]
    pub signed_out: bool,
    /// Codex asked to finish (SIGTERM) takes this many milliseconds to exit
    /// once its running turn has ended, still holding its thread, as a real
    /// server can take seconds. None by default: it exits at once.
    #[serde(default)]
    pub exit_ms: u64,
}

/// A tool server as headless Claude reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerState {
    pub name: String,
    pub status: String,
}

/// A model a scripted session offers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfferedModel {
    pub value: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: String,
    /// Its effort levels, in order.
    #[serde(default)]
    pub efforts: Vec<String>,
    #[serde(default)]
    pub default_effort: Option<String>,
    /// The model id an alias stands for, as headless Claude reports it
    /// ("default" stands for "claude-opus-5[1m]").
    #[serde(default)]
    pub resolved_model: Option<String>,
}

impl OfferedModel {
    /// The one model a script that names none offers.
    pub fn standard(value: &str) -> Self {
        Self {
            value: value.to_owned(),
            display_name: None,
            description: "The scripted model".into(),
            efforts: ["low", "medium", "high"].map(str::to_owned).to_vec(),
            default_effort: Some("medium".into()),
            resolved_model: None,
        }
    }

    /// What the session offers: the scripted models, or the standard one.
    pub fn offered(scripted: &[Self], model: &str) -> Vec<Self> {
        if scripted.is_empty() {
            vec![Self::standard(model)]
        } else {
            scripted.to_vec()
        }
    }

    pub fn display_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.value)
    }
}

/// A command a scripted session offers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfferedCommand {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub argument_hint: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// Streamed assistant text: one message, delivered chunk by chunk where
    /// the provider streams.
    Text { chunks: Vec<String> },
    /// Reasoning before the next step.
    Thinking { text: String },
    /// A tool call that runs without asking.
    Tool(Tool),
    /// Something the host must answer before the turn goes on.
    Ask(Ask),
    /// Hold the turn until this file exists.
    WaitFor { path: PathBuf },
    /// Stay busy for this long, as a model at work does.
    Pause { ms: u64 },
    /// Start `sh -c command` as a session of its own, as Claude's and
    /// Codex's tool shells are, and go on without waiting for it: what a
    /// long command leaves running under the provider, out of reach of a
    /// kill of the provider's own process group.
    Spawn { command: String },
    /// Finish the turn successfully.
    TurnEnd,
    /// Exit the provider process with this code, mid-turn or not.
    Exit { code: i32 },
    /// The account's usage limits as the provider reports them (headless
    /// Claude, Codex).
    Usage(Usage),
    /// The provider's credential is refused, and the turn fails on it:
    /// headless Claude says so in an error message, without retrying, and
    /// ends the turn; Codex reconnects a few times and fails the turn.
    AuthFailed { message: String },
    /// Play `steps` this many times in a row, as if written out: a flood
    /// of messages without a script the size of the flood. `{pass}` in the
    /// text or thinking of a step directly inside becomes the pass's
    /// number, counted from 1, so a watcher can tell the messages apart.
    Repeat {
        times: usize,
        steps: Vec<Step>,
        /// The passes already played, as the repeat unrolls.
        #[serde(skip)]
        played: usize,
    },
}

/// A tool call. The class picks a tool of that class when no name is given:
/// exploration reads, consequential runs a command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    #[serde(default)]
    pub name: Option<String>,
    pub class: ToolClass,
    /// The call's input in the provider's terms; a default for the class
    /// otherwise.
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub outcome: Outcome,
    /// The call runs until this file exists, as a long command does, so a
    /// test can write to the provider while a tool is running. A command
    /// run in the background returns at once and its job runs until then.
    #[serde(default)]
    pub wait_for: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolClass {
    Exploration,
    Consequential,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    #[serde(default)]
    pub output: String,
    /// The tool ran and failed.
    #[serde(default)]
    pub error: bool,
    /// The command prints `output` this many times as it runs, `{piece}`
    /// in it numbered from 1: a long build's log without a script the size
    /// of the log. Codex streams each piece as it is printed; the others
    /// return the pieces joined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pieces: Option<usize>,
    /// The call runs this long before it returns, so its row shows a
    /// believable duration; at once when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
}

impl Outcome {
    /// What the command prints, a piece at a time.
    pub fn printed(&self) -> impl Iterator<Item = String> + '_ {
        let pieces = self.pieces;
        (1..=pieces.unwrap_or(1)).map(move |piece| match pieces {
            Some(_) => self.output.replace("{piece}", &piece.to_string()),
            None => self.output.clone(),
        })
    }

    /// Everything the command printed.
    pub fn text(&self) -> String {
        self.printed().collect()
    }

    /// When a call started now returns.
    pub fn until(&self) -> tokio::time::Instant {
        tokio::time::Instant::now() + std::time::Duration::from_millis(self.ms.unwrap_or(0))
    }
}

/// Every kind of ask the providers raise. Not every provider raises every
/// kind; a script asking a provider for one it cannot raise fails to load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Ask {
    /// Permission to make this call; allowed, it runs with its outcome.
    Permission(Tool),
    /// Multiple-choice questions.
    Question { questions: Vec<Question> },
    /// A plan to approve before the work starts.
    Plan { markdown: String },
    /// A tool server's form.
    Form {
        server: String,
        message: String,
        schema: Schema,
    },
    /// A tool server's request to open a link.
    Link {
        server: String,
        message: String,
        url: String,
    },
    /// Wider access for the rest of the turn or session (Codex).
    Grant { reason: String, paths: Vec<String> },
    /// A tool server's form, or link when `link` is set, that terminal
    /// Claude shows in its own terminal while the server's `tool` call
    /// runs. It stays open until the interrupt key cancels it, or until
    /// `wait_for` exists, when it counts as answered there and the call
    /// returns `output`.
    ToolServerDialog {
        server: String,
        tool: String,
        #[serde(default)]
        link: bool,
        #[serde(default)]
        wait_for: Option<PathBuf>,
        #[serde(default)]
        output: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
    /// Codex: an answer may be typed instead of picked.
    #[serde(default)]
    pub other: bool,
    /// Codex: the answer is a secret.
    #[serde(default)]
    pub secret: bool,
}

/// A tool server's form schema, kept exactly as written: a parsed JSON
/// value would sort its keys, and the order of its properties is the order
/// the form asks for them.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Schema(pub Box<serde_json::value::RawValue>);

impl Schema {
    pub fn text(&self) -> &str {
        self.0.get()
    }

    /// The schema on one line, its keys in their order: whitespace outside
    /// strings dropped.
    pub fn compact(&self) -> String {
        let mut out = String::with_capacity(self.text().len());
        let (mut quoted, mut escaped) = (false, false);
        for c in self.text().chars() {
            if quoted {
                out.push(c);
                match (escaped, c) {
                    (true, _) => escaped = false,
                    (false, '\\') => escaped = true,
                    (false, '"') => quoted = false,
                    _ => {}
                }
            } else if c == '"' {
                quoted = true;
                out.push(c);
            } else if !c.is_whitespace() {
                out.push(c);
            }
        }
        out
    }
}

impl PartialEq for Schema {
    fn eq(&self, other: &Schema) -> bool {
        self.text() == other.text()
    }
}

/// Where a frame carries a form schema, written out as the schema's own
/// text when the frame is sent.
pub const SCHEMA_SLOT: &str = "__fake_schema__";

/// One option of a question: its label, or the label with a description
/// and a preview (headless and terminal Claude).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum QuestionOption {
    Label(String),
    Full {
        label: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        preview: Option<String>,
    },
}

impl From<&str> for QuestionOption {
    fn from(label: &str) -> Self {
        QuestionOption::Label(label.to_owned())
    }
}

impl QuestionOption {
    pub fn label(&self) -> &str {
        match self {
            QuestionOption::Label(label) | QuestionOption::Full { label, .. } => label,
        }
    }

    /// Its description; the label where none is given.
    pub fn description(&self) -> &str {
        match self {
            QuestionOption::Full {
                description: Some(description),
                ..
            } => description,
            option => option.label(),
        }
    }

    pub fn preview(&self) -> Option<&str> {
        match self {
            QuestionOption::Full { preview, .. } => preview.as_deref(),
            QuestionOption::Label(_) => None,
        }
    }
}

/// Usage limits, as both providers report them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    /// `allowed`, `allowed_warning` (near a limit) or `rejected` (a limit
    /// reached), in headless Claude's words.
    pub status: String,
    pub windows: Vec<UsageWindow>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageWindow {
    /// `five_hour` or `seven_day`.
    pub name: String,
    /// How much of it is used, out of 100.
    pub used_percent: f64,
    /// When it resets, in seconds from now.
    pub resets_in_s: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error("reading script {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parsing script {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("{provider} cannot raise a {ask} ask")]
    Unsupported {
        provider: &'static str,
        ask: &'static str,
    },
    #[error("{provider} cannot play a {step} step")]
    UnsupportedStep {
        provider: &'static str,
        step: &'static str,
    },
}

impl Script {
    pub fn load(path: &Path) -> Result<Self, ScriptError> {
        let bytes = std::fs::read(path).map_err(|source| ScriptError::Read {
            path: path.to_owned(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| ScriptError::Parse {
            path: path.to_owned(),
            source,
        })
    }

    /// Refuse asks this provider has no way to raise, and steps it has no
    /// way to play (`plays` names the provider-specific ones it can).
    pub fn check(
        &self,
        provider: &'static str,
        raises: &[&str],
        plays: &[&str],
    ) -> Result<(), ScriptError> {
        check_steps(&self.steps, provider, raises, plays)
    }
}

impl Ask {
    pub fn kind(&self) -> &'static str {
        match self {
            Ask::Permission(_) => "permission",
            Ask::Question { .. } => "question",
            Ask::Plan { .. } => "plan",
            Ask::Form { .. } => "form",
            Ask::Link { .. } => "link",
            Ask::Grant { .. } => "grant",
            Ask::ToolServerDialog { .. } => "tool_server_dialog",
        }
    }
}

impl Tool {
    pub fn is_exploration(&self) -> bool {
        self.class == ToolClass::Exploration
    }
}

fn check_steps(
    steps: &[Step],
    provider: &'static str,
    raises: &[&str],
    plays: &[&str],
) -> Result<(), ScriptError> {
    for step in steps {
        match step {
            Step::Ask(ask) if !raises.contains(&ask.kind()) => {
                return Err(ScriptError::Unsupported {
                    provider,
                    ask: ask.kind(),
                });
            }
            Step::Repeat { steps, .. } => check_steps(steps, provider, raises, plays)?,
            step => {
                if let Some(kind) = step.provider_kind()
                    && !plays.contains(&kind)
                {
                    return Err(ScriptError::UnsupportedStep {
                        provider,
                        step: kind,
                    });
                }
            }
        }
    }
    Ok(())
}

impl Step {
    /// The name of a step only some providers can play.
    pub fn provider_kind(&self) -> Option<&'static str> {
        match self {
            Step::Usage(_) => Some("usage"),
            Step::AuthFailed { .. } => Some("auth_failed"),
            _ => None,
        }
    }
}

/// The next step to play, with repeats unrolled one pass at a time so a
/// long repeat never sits in memory written out. Never returns a
/// [`Step::Repeat`].
pub fn next_step(steps: &mut std::collections::VecDeque<Step>) -> Option<Step> {
    loop {
        match steps.pop_front()? {
            Step::Repeat {
                times,
                steps: body,
                played,
            } => {
                if times == 0 || body.is_empty() {
                    continue;
                }
                let pass = (played + 1).to_string();
                if times > 1 {
                    steps.push_front(Step::Repeat {
                        times: times - 1,
                        steps: body.clone(),
                        played: played + 1,
                    });
                }
                for step in body.into_iter().rev() {
                    steps.push_front(numbered(step, &pass));
                }
            }
            step => return Some(step),
        }
    }
}

/// A step of a repeat's pass, with `{pass}` in its words made the pass's
/// number.
fn numbered(step: Step, pass: &str) -> Step {
    match step {
        Step::Text { chunks } => Step::Text {
            chunks: chunks
                .into_iter()
                .map(|chunk| chunk.replace("{pass}", pass))
                .collect(),
        },
        Step::Thinking { text } => Step::Thinking {
            text: text.replace("{pass}", pass),
        },
        step => step,
    }
}

/// Where a scripted pause stands: polled rather than watched so it works the
/// same on every platform and filesystem.
pub async fn wait_for(path: &Path) {
    while !path.exists() {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
mod repeat_tests {
    use std::collections::VecDeque;

    use super::{Step, next_step};

    fn text(text: &str) -> Step {
        Step::Text {
            chunks: vec![text.to_owned()],
        }
    }

    #[test]
    fn a_repeat_plays_as_if_written_out_and_parses_from_json() {
        let steps: Vec<Step> = serde_json::from_str(
            r#"[{"repeat": {"times": 3, "steps": [{"text": {"chunks": ["a"]}}, {"repeat": {"times": 2, "steps": [{"text": {"chunks": ["b"]}}]}}]}}, "turn_end"]"#,
        )
        .unwrap();
        let mut queue: VecDeque<Step> = steps.into();
        let mut played = Vec::new();
        while let Some(step) = next_step(&mut queue) {
            played.push(step);
        }
        let mut expected = Vec::new();
        for _ in 0..3 {
            expected.extend([text("a"), text("b"), text("b")]);
        }
        expected.push(Step::TurnEnd);
        assert_eq!(played, expected);
    }

    #[test]
    fn a_repeat_numbers_its_passes_where_its_text_asks() {
        let steps: Vec<Step> = serde_json::from_str(
            r#"[{"repeat": {"times": 2, "steps": [{"text": {"chunks": ["message {pass}"]}}, {"thinking": {"text": "on {pass}"}}, {"repeat": {"times": 2, "steps": [{"text": {"chunks": ["{pass}"]}}]}}]}}]"#,
        )
        .unwrap();
        let mut queue: VecDeque<Step> = steps.into();
        let mut played = Vec::new();
        while let Some(step) = next_step(&mut queue) {
            played.push(step);
        }
        let thinking = |text: &str| Step::Thinking {
            text: text.to_owned(),
        };
        assert_eq!(
            played,
            [
                text("message 1"),
                thinking("on 1"),
                text("1"),
                text("2"),
                text("message 2"),
                thinking("on 2"),
                text("1"),
                text("2"),
            ]
        );
    }
}

#[cfg(test)]
mod schema_tests {
    use super::Schema;

    #[test]
    fn a_schema_keeps_its_key_order_on_one_line() {
        let schema: Schema = serde_json::from_str(
            "{\n  \"properties\": {\n    \"team\": {\"title\": \"Team name\"},\n    \"\\\"quoted\\\" key\": {}\n  }\n}",
        )
        .unwrap();
        assert_eq!(
            schema.compact(),
            r#"{"properties":{"team":{"title":"Team name"},"\"quoted\" key":{}}}"#
        );
    }
}
