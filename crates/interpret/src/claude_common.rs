//! What Claude in a terminal and Claude through the SDK share: the tool
//! vocabulary, the question and scope shapes, the task list the task tools
//! keep, and the one-line renderings goldens use.

use std::collections::BTreeMap;

use claude_protocol::stream::init::{ModelInfo, SlashCommand};
use claude_protocol::stream::{
    ContentBlock, ImageSourceType, MessageContent, PermissionUpdate, PermissionUpdateDestination,
    ToolResultBody, ToolResultContent,
};
use prost::Message as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{
    Ask, Attachment, BackgroundJob, BlobRef, ClaudeGrant, ClaudeUsage, DecisionOutcome, EditInput,
    HealthState, Limit, NotebookCell, NotebookEditMode, OfferedCommand, OfferedModel,
    OfferedPermission, PatchHunk, Question, QuestionAsk, QuestionOption, Replacement,
    ScopeDestination, ShellInput, TaskList, TaskListEntry, TaskListStatus, ToolCall, ToolClass,
    ToolInput, ToolServer, ToolServerHealth, ToolServerStatus, ToolState, UsageState, attachment,
    codex_grant, tool_decision, tool_input,
};

use crate::{Effect, ItemView};

/// Built-in tools that only look, each by what it looks at: views fold runs
/// of them together and name each by this verb.
pub(crate) const EXPLORATION: &[(&str, ToolClass)] = &[
    ("Read", ToolClass::Read),
    ("NotebookRead", ToolClass::Read),
    ("ReadMcpResourceTool", ToolClass::Read),
    ("Grep", ToolClass::Search),
    ("Glob", ToolClass::Search),
    ("ToolSearch", ToolClass::Search),
    ("LS", ToolClass::List),
    ("ListMcpResourcesTool", ToolClass::List),
    ("WebFetch", ToolClass::Fetch),
    ("WebSearch", ToolClass::WebSearch),
];

/// Tools whose calls are drawn as the task list, never as rows.
pub(crate) const TASK_TOOLS: &[&str] = &["TaskCreate", "TaskUpdate", "TaskGet", "TaskList"];

pub(crate) const QUESTION_TOOL: &str = "AskUserQuestion";
pub(crate) const PLAN_TOOL: &str = "ExitPlanMode";

/// Claude's built-in tools that write or edit files. In plan mode Claude
/// writes only its plan file with them, and the plan's declaration carries
/// the whole plan, so their calls are not drawn.
const FILE_WRITE_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit"];

/// A call that writes a file while Claude plans: drawn as nothing.
pub(crate) fn plan_mode_write(permission: Option<&str>, server: &str, tool: &str) -> bool {
    permission == Some("plan") && server.is_empty() && FILE_WRITE_TOOLS.contains(&tool)
}

/// How the person decided a plan Claude declared, kept on the declaring
/// call; undecided until they do.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Verdict {
    verdict: i32,
    note: Option<String>,
    /// Approved switching: the permission switched to.
    permission: String,
}

impl Default for Verdict {
    fn default() -> Self {
        Self::of(wire::PlanVerdict::Undecided)
    }
}

impl Verdict {
    pub(crate) fn of(verdict: wire::PlanVerdict) -> Self {
        Self {
            verdict: verdict as i32,
            note: None,
            permission: String::new(),
        }
    }

    pub(crate) fn sent_back(note: &str) -> Self {
        Self {
            note: (!note.is_empty()).then(|| note.to_owned()),
            ..Self::of(wire::PlanVerdict::SentBack)
        }
    }

    /// Approved, switching to `permission`.
    pub(crate) fn switching(permission: &str) -> Self {
        Self {
            permission: permission.to_owned(),
            ..Self::of(wire::PlanVerdict::ApprovedSwitching)
        }
    }

    /// Approved, by the permission Claude left plan for: one that acts
    /// without asking first is a switch to it.
    pub(crate) fn approved_for(permission: Option<&str>) -> Self {
        match permission {
            Some(permission) if !matches!(permission, "default" | "plan") => {
                Self::switching(permission)
            }
            _ => Self::of(wire::PlanVerdict::Approved),
        }
    }

    pub(crate) fn undecided(&self) -> bool {
        self.verdict == wire::PlanVerdict::Undecided as i32
    }
}

/// The plan item of an ExitPlanMode call: its text and body. None until
/// the call's input carries the plan, so the plan lands whole.
pub(crate) fn plan_item(input: &str, verdict: &Verdict) -> Option<(String, wire::Plan)> {
    let plan = serde_json::from_str::<PlanInput>(input).ok()?.plan;
    (!plan.is_empty()).then(|| {
        (
            plan,
            wire::Plan {
                verdict: verdict.verdict,
                note: verdict.note.clone(),
                complete: true,
                permission: verdict.permission.clone(),
            },
        )
    })
}

/// The ask item of an AskUserQuestion call: its questions, and how they
/// were answered once they were. None until the call's input carries them.
pub(crate) fn question_item(input: &str, closed: Option<wire::AskClosed>) -> Option<wire::AskItem> {
    let asked = question_ask_text(input);
    if asked.questions.is_empty() {
        return None;
    }
    Some(wire::AskItem {
        ask: Some(wire::ask_item::Ask::Question(asked)),
        closed,
    })
}

/// AskUserQuestion's answers as Claude records them, in the input handed to
/// the tool and in its result: by each question's text, with notes beside
/// them.
#[derive(Deserialize)]
pub(crate) struct AnsweredResult {
    #[serde(default)]
    pub answers: BTreeMap<String, String>,
    #[serde(default)]
    pub annotations: BTreeMap<String, AnswerNote>,
}

/// The note on one answered question.
#[derive(Deserialize)]
pub(crate) struct AnswerNote {
    #[serde(default)]
    pub notes: String,
}

/// The decision Claude's plan asks offer: start as before planning, start
/// switching to `switch` (none when empty), or keep planning.
pub(crate) fn plan_ask(switch: &str) -> wire::PlanAsk {
    let mut choices = vec![wire::PlanChoice::Start as i32];
    if !switch.is_empty() {
        choices.push(wire::PlanChoice::StartSwitching as i32);
    }
    choices.push(wire::PlanChoice::KeepPlanning as i32);
    wire::PlanAsk {
        choices,
        switch_permission: switch.to_owned(),
    }
}

/// The permission Claude's own plan approval switches to, in both modes:
/// never ask where Claude was launched allowing it, else auto mode where
/// Claude offers it, else accept edits.
pub(crate) fn plan_switch(offers_auto: bool, never_ask: bool) -> &'static str {
    if never_ask {
        NEVER_ASK
    } else if offers_auto {
        "auto"
    } else {
        "acceptEdits"
    }
}

/// An RFC 3339 timestamp, as Claude writes them, in milliseconds.
pub(crate) fn timestamp_ms(stamp: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|at| at.timestamp_millis())
}

pub(crate) fn compact_json(value: &Value) -> String {
    if value.is_null() {
        String::new()
    } else {
        value.to_string()
    }
}

/// A message's content as text: the plain text, or its text blocks joined.
pub(crate) fn message_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Blocks(blocks) => blocks_text(blocks),
    }
}

/// Content blocks' text blocks, joined.
pub(crate) fn blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A tool result as text: the plain text, or its text blocks joined.
pub(crate) fn tool_result_text(content: Option<&ToolResultBody>) -> String {
    match content {
        None => String::new(),
        Some(ToolResultBody::Text(text)) => text.clone(),
        Some(ToolResultBody::Blocks(blocks)) => blocks
            .iter()
            .filter_map(|block| match block {
                ToolResultContent::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// The images in a tool result's content blocks, each as the attachment a
/// tool row carries and the blob write that stores its bytes.
pub(crate) fn tool_result_images(content: Option<&ToolResultBody>) -> Vec<(Attachment, Effect)> {
    let Some(ToolResultBody::Blocks(blocks)) = content else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|block| match block {
            ToolResultContent::Image { source, .. } if source.r#type == ImageSourceType::Base64 => {
                image_blob(&source.media_type, &source.data)
            }
            _ => None,
        })
        .collect()
}

/// A base64 image as the attachment that names its blob and the write that
/// stores it.
fn image_blob(mime: &str, data: &str) -> Option<(Attachment, Effect)> {
    use base64::Engine as _;
    use sha2::Digest as _;

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let hash = sha2::Sha256::digest(&bytes).to_vec();
    let image = Attachment {
        of: Some(attachment::Of::Image(BlobRef {
            hash: hash.clone(),
            name: String::new(),
            mime: mime.to_owned(),
            size: bytes.len() as u64,
        })),
    };
    Some((image, Effect::WriteBlob { hash, bytes }))
}

/// A tool's structured result without the base64 copy of an image it
/// read; the bytes live in the blob its row references.
pub(crate) fn without_image_bytes(result: &Value) -> Value {
    let mut result = result.clone();
    if let Some(file) = result.get_mut("file").and_then(Value::as_object_mut)
        && file
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|mime| mime.starts_with("image/"))
    {
        file.remove("base64");
    }
    result
}

pub(crate) fn task_status(status: &str) -> Option<i32> {
    Some(match status {
        "pending" => TaskListStatus::Pending as i32,
        "in_progress" => TaskListStatus::InProgress as i32,
        "completed" => TaskListStatus::Completed as i32,
        _ => return None,
    })
}

/// `mcp__server__tool` as (server, tool); a built-in tool has no server.
pub(crate) fn split_tool_name(name: &str) -> (String, String) {
    if let Some(rest) = name.strip_prefix("mcp__")
        && let Some((server, tool)) = rest.split_once("__")
    {
        return (server.to_owned(), tool.to_owned());
    }
    (String::new(), name.to_owned())
}

/// The field the agent process adds to a terminal Claude hook payload
/// Claude waits on for a decision, numbering the connection the decision
/// goes back on (`Effect::HookAnswer`).
pub const HOOK_ANSWER_FIELD: &str = "amux_reply";

/// The source of the notebook cell an edit replaces. Claude's request names
/// only the cell, so the host reads the cell from the notebook when Claude
/// asks and adds it to the request's payload under this name.
pub const NOTEBOOK_OLD_SOURCE: &str = "amux_old_source";

/// The notebook cell the host read for an ask, which Claude's input
/// names but does not carry.
pub(crate) fn old_source(extensions: &serde_json::Map<String, Value>) -> &str {
    extensions
        .get(NOTEBOOK_OLD_SOURCE)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

pub(crate) fn same_json(stored: &str, value: &Value) -> bool {
    serde_json::from_str::<Value>(stored).is_ok_and(|stored| stored == *value)
}

/// A task list entry as the task tools maintain it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Task {
    pub id: String,
    pub subject: String,
    pub status: i32,
    pub active_form: String,
}

/// A golden's words for an exit boundary's cause: its value's name and its
/// own words, or nothing for a boundary that is not an exit.
pub(crate) fn describe_cause(boundary: &wire::Boundary) -> String {
    if boundary.cause == 0 && boundary.cause_detail.is_none() {
        return String::new();
    }
    let name = wire::ExitCause::try_from(boundary.cause).map_or_else(
        |_| boundary.cause.to_string(),
        |cause| {
            cause
                .as_str_name()
                .trim_start_matches("EXIT_CAUSE_")
                .to_owned()
        },
    );
    match &boundary.cause_detail {
        Some(detail) => format!(
            " cause={name} detail={}",
            serde_json::Value::String(detail.clone())
        ),
        None => format!(" cause={name}"),
    }
}

pub(crate) fn or_dash(text: &str) -> &str {
    if text.is_empty() { "-" } else { text }
}

pub(crate) fn describe_tool(tool: &ToolCall) -> String {
    let mut text = format!(
        "{}{} {}",
        if tool.server.is_empty() {
            String::new()
        } else {
            format!("{}·", tool.server)
        },
        tool.name,
        ToolState::try_from(tool.state).map_or("?", |state| state.as_str_name())
    );
    text.push_str(describe_class(tool.class));
    if tool.background {
        text.push_str(" background");
    }
    if let Some(subagent) = &tool.subagent {
        text.push_str(&format!(" subagent={}", subagent.tool_count));
        if !subagent.last_tool.is_empty() {
            text.push_str(&format!(" last={}", subagent.last_tool));
        }
        if subagent.finished {
            text.push_str(" finished");
        }
    }
    if let Some(decision) = &tool.decision {
        text.push_str(&format!(
            " decision={}{}{}{}",
            DecisionOutcome::try_from(decision.outcome)
                .map_or("?", |outcome| outcome.as_str_name()),
            describe_grant(decision.granted.as_ref()),
            if decision.note.is_empty() {
                String::new()
            } else {
                format!(" note={}", Value::String(decision.note.clone()))
            },
            if decision.elsewhere { " elsewhere" } else { "" }
        ));
    }
    if let Some(started) = tool.started_at_ms {
        text.push_str(&format!(" started={started}"));
    }
    if let Some(ended) = tool.ended_at_ms {
        text.push_str(&format!(" ended={ended}"));
    }
    if let Some(created) = tool.created {
        text.push_str(if created { " created" } else { " updated" });
    }
    if let Some(line) = tool.line {
        text.push_str(&format!(" line={line}"));
    }
    for attachment in &tool.attachments {
        if let Some(attachment::Of::Image(image)) = &attachment.of {
            text.push_str(&format!(
                " image={}:{}:{}",
                image.mime,
                image.size,
                crate::to_hex(&image.hash[..image.hash.len().min(4)])
            ));
        }
    }
    if let Some(input) = &tool.input {
        text.push_str(&format!(" input={}", describe_input(input)));
    }
    if !tool.patch.is_empty() {
        let lines = tool
            .patch
            .iter()
            .map(|hunk| hunk.lines.len())
            .sum::<usize>();
        text.push_str(&format!(" patch={}/{lines}", tool.patch.len()));
    }
    if !tool.outcome_text.is_empty() {
        text.push_str(&format!(" out={}", clip(&tool.outcome_text, 60)));
    }
    text
}

/// A call's input as goldens show it: its arm and subject, or Claude's
/// JSON for a tool with no arm of its own.
fn describe_input(input: &ToolInput) -> String {
    let arm = match &input.of {
        Some(tool_input::Of::OtherJson(json)) => {
            return clip(&String::from_utf8_lossy(json), 60);
        }
        Some(tool_input::Of::Shell(_)) => "shell",
        Some(tool_input::Of::Edit(edit)) if edit.write => "write",
        Some(tool_input::Of::Edit(edit)) => {
            return format!(
                "edit:{}x{}",
                clip(&input.subject, 60),
                edit.replacements.len()
            );
        }
        Some(tool_input::Of::Subagent(_)) => "subagent",
        Some(tool_input::Of::TaskList(_)) => "task_list",
        Some(tool_input::Of::Web(_)) => "web",
        None => "none",
    };
    format!("{arm}:{}", clip(&input.subject, 60))
}

pub(crate) fn clip(text: &str, chars: usize) -> String {
    let clipped = match text.char_indices().nth(chars) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    };
    Value::String(clipped).to_string()
}

/// What Bash's input says about where it runs.
#[derive(Deserialize)]
pub(crate) struct BackgroundInput {
    #[serde(default)]
    pub run_in_background: bool,
}

/// Which limit a usage window Claude names is: five-hour, weekly, or weekly
/// for one model, named as Claude's own labels name it. None for a window
/// amux does not recognise.
pub(crate) fn claude_limit(name: &str) -> Option<(Limit, Option<&'static str>)> {
    Some(match name {
        "five_hour" => (Limit::FiveHour, None),
        "seven_day" => (Limit::Weekly, None),
        "seven_day_opus" => (Limit::Weekly, Some("Opus")),
        "seven_day_sonnet" => (Limit::Weekly, Some("Sonnet")),
        "seven_day_overage_included" => (Limit::Weekly, Some("Fable")),
        _ => return None,
    })
}

/// Claude's usage as goldens print it: the overall state, then each window
/// as its limit (Claude's own name when unrecognised), use and state.
pub(crate) fn describe_claude_usage(usage: &ClaudeUsage) -> String {
    if usage.state() == UsageState::Unknown && usage.windows.is_empty() {
        return "?".into();
    }
    let mut text = crate::shared::describe_usage_state(usage.state).to_owned();
    for window in &usage.windows {
        text.push_str(&crate::shared::describe_usage_window(
            window.limit(),
            window.model.as_deref(),
            || format!("{}?", window.provider_name),
            window.meter,
        ));
    }
    text
}

/// The input of the Bash call a `!command` is recorded as: what Claude's
/// own Bash tool takes, the command alone.
#[derive(Serialize)]
pub(crate) struct BangInput<'a> {
    pub command: &'a str,
}

/// What a call that started a background job ran: Bash's command, or the
/// description of an Agent call's task.
#[derive(Deserialize)]
pub(crate) struct JobInput {
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

impl JobInput {
    /// The job's command from the call's input as the interpreter keeps it.
    pub(crate) fn command(input: &str) -> String {
        serde_json::from_str::<Self>(input)
            .ok()
            .and_then(|input| input.command.or(input.description))
            .unwrap_or_default()
    }
}

/// The kind of background job Claude's name for a task's type says: its
/// stream's `task_type` (`local_bash`, `local_agent`) or its Stop hook's
/// `type` (`shell`, `subagent`).
pub(crate) fn job_kind(task_type: &str) -> wire::BackgroundKind {
    match task_type {
        "local_bash" | "shell" => wire::BackgroundKind::Shell,
        "local_agent" | "remote_agent" | "subagent" => wire::BackgroundKind::Subagent,
        other if other.contains("monitor") => wire::BackgroundKind::Monitor,
        "" => wire::BackgroundKind::Unspecified,
        _ => wire::BackgroundKind::Other,
    }
}

/// Claude's background jobs by its task id: the call that started each, as
/// far as Claude has said, and which of them Claude lists as running. A job
/// is published from when its call says it started until Claude's next list
/// leaves it out or Claude says it ended.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Jobs {
    jobs: BTreeMap<String, Job>,
    /// The task ids of Claude's newest list, in its order.
    listed: Vec<String>,
    /// How many lists Claude has stated.
    lists: u64,
    /// Whether anything is known about the jobs: a list stated, or a job
    /// launched. Before either they are unknown.
    known: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Job {
    step: String,
    command: String,
    started_at_ms: i64,
    /// The number of lists stated before the job's call said it started:
    /// a job launched after the newest list is kept until the next one.
    launched_after: u64,
    kind: i32,
    /// A stop was sent and Claude has not said the job ended.
    stopping: bool,
    /// Claude's refusal of the last stop.
    stop_error: String,
}

impl Jobs {
    /// The call keyed `step` started task `task_id`, of `kind`, at `at_ms`.
    pub(crate) fn launched(
        &mut self,
        task_id: &str,
        step: &str,
        command: String,
        at_ms: i64,
        kind: wire::BackgroundKind,
    ) {
        let job = self.jobs.entry(task_id.to_owned()).or_default();
        job.step = step.to_owned();
        if !command.is_empty() {
            job.command = command;
        }
        job.started_at_ms = at_ms;
        job.launched_after = self.lists;
        if kind != wire::BackgroundKind::Unspecified {
            job.kind = kind as i32;
        }
        self.known = true;
    }

    /// Claude's list of the tasks running now, as (task id, description,
    /// kind): a task no call is known to have started is shown by its
    /// description, from now. Returns the jobs to publish.
    pub(crate) fn listed(
        &mut self,
        tasks: impl IntoIterator<Item = (String, String, wire::BackgroundKind)>,
        now_ms: i64,
    ) -> Vec<BackgroundJob> {
        self.listed.clear();
        for (task_id, description, kind) in tasks {
            let job = self.jobs.entry(task_id.clone()).or_insert_with(|| Job {
                started_at_ms: now_ms,
                launched_after: self.lists,
                ..Job::default()
            });
            if job.command.is_empty() {
                job.command = description;
            }
            if kind != wire::BackgroundKind::Unspecified {
                job.kind = kind as i32;
            }
            self.listed.push(task_id);
        }
        let (listed, lists) = (&self.listed, self.lists);
        self.jobs
            .retain(|id, job| listed.contains(id) || job.launched_after == lists);
        self.lists += 1;
        self.known = true;
        self.published()
    }

    /// Whether task `task_id` is a job still running, as far as Claude
    /// has said.
    pub(crate) fn running(&self, task_id: &str) -> bool {
        self.jobs.contains_key(task_id)
    }

    /// A stop was sent for task `task_id`. Returns the jobs to publish;
    /// None when no job has that id.
    pub(crate) fn stop_sent(&mut self, task_id: &str) -> Option<Vec<BackgroundJob>> {
        let job = self.jobs.get_mut(task_id)?;
        job.stopping = true;
        job.stop_error.clear();
        Some(self.published())
    }

    /// Claude refused to stop task `task_id`, saying `error`; the job runs
    /// on. Returns the jobs to publish when it is still one.
    pub(crate) fn stop_refused(
        &mut self,
        task_id: &str,
        error: String,
    ) -> Option<Vec<BackgroundJob>> {
        let job = self.jobs.get_mut(task_id)?;
        job.stopping = false;
        job.stop_error = error;
        Some(self.published())
    }

    /// Claude said task `task_id` ended. Returns the jobs to publish when
    /// it was one.
    pub(crate) fn ended(&mut self, task_id: &str) -> Option<Vec<BackgroundJob>> {
        let was = self.jobs.remove(task_id).is_some();
        self.listed.retain(|id| id != task_id);
        was.then(|| self.published())
    }

    /// The jobs to publish when what is known about them changed; None
    /// while nothing is.
    pub(crate) fn current(&self) -> Option<Vec<BackgroundJob>> {
        self.known.then(|| self.published())
    }

    /// Claude's newest list in its order, then the jobs launched since it,
    /// by when they started.
    fn published(&self) -> Vec<BackgroundJob> {
        let mut launched: Vec<_> = self
            .jobs
            .iter()
            .filter(|(id, job)| job.launched_after == self.lists && !self.listed.contains(id))
            .collect();
        launched.sort_by_key(|(id, job)| (job.started_at_ms, (*id).clone()));
        self.listed
            .iter()
            .filter_map(|id| Some((id, self.jobs.get(id)?)))
            .chain(launched)
            .map(|(id, job)| BackgroundJob {
                item_key: job.step.clone(),
                command: job.command.clone(),
                started_at_ms: job.started_at_ms,
                kind: job.kind,
                key: id.clone(),
                stopping: job.stopping,
                stop_error: job.stop_error.clone(),
            })
            .collect()
    }

    /// The provider exited: its jobs are gone.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

/// ExitPlanMode's input: the plan put to the person.
#[derive(Deserialize)]
struct PlanInput {
    #[serde(default)]
    pub plan: String,
}

/// AskUserQuestion's input: the questions put to the person.
#[derive(Deserialize)]
pub(crate) struct QuestionInput {
    #[serde(default)]
    pub questions: Vec<QuestionItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuestionItem {
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default)]
    pub options: Vec<QuestionOptionItem>,
}

#[derive(Deserialize)]
pub(crate) struct QuestionOptionItem {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub preview: String,
}

/// The question form AskUserQuestion's input describes.
pub(crate) fn question_ask(input: &Value) -> QuestionAsk {
    questions_asked(
        QuestionInput::deserialize(input)
            .map(|input| input.questions)
            .unwrap_or_default(),
    )
}

/// The same, from the input kept as text on its call.
pub(crate) fn question_ask_text(input: &str) -> QuestionAsk {
    questions_asked(
        serde_json::from_str::<QuestionInput>(input)
            .map(|input| input.questions)
            .unwrap_or_default(),
    )
}

fn questions_asked(questions: Vec<QuestionItem>) -> QuestionAsk {
    let questions = questions
        .into_iter()
        .map(|question| Question {
            header: question.header,
            question: question.question,
            multi_select: question.multi_select,
            options: question
                .options
                .into_iter()
                .map(|option| QuestionOption {
                    label: option.label,
                    description: option.description,
                    preview: option.preview,
                    recommended: false,
                })
                .collect(),
            allow_other: true,
            secret: false,
        })
        .collect();
    QuestionAsk {
        questions,
        provider_dialog: false,
    }
}

/// What a decision granted, as goldens print it.
pub(crate) fn describe_grant(granted: Option<&tool_decision::Granted>) -> String {
    let words = |words: &[String]| format!("{words:?}");
    match granted {
        None => String::new(),
        Some(tool_decision::Granted::Claude(grant)) => {
            let mut text = format!(
                " grant=claude(destination={}",
                or_dash(&grant.destination_word)
            );
            if !grant.rules.is_empty() {
                text.push_str(&format!(" rules={}", words(&grant.rules)));
            }
            if !grant.directories.is_empty() {
                text.push_str(&format!(" directories={}", words(&grant.directories)));
            }
            if let Some(mode) = &grant.mode {
                text.push_str(&format!(" mode={mode}"));
            }
            text.push(')');
            text
        }
        Some(tool_decision::Granted::Codex(grant)) => match &grant.of {
            Some(codex_grant::Of::Session(_)) => " grant=session".into(),
            Some(codex_grant::Of::Always(_)) => " grant=always".into(),
            Some(codex_grant::Of::CommandPrefix(prefix)) => {
                format!(" grant=prefix{}", words(&prefix.words))
            }
            Some(codex_grant::Of::NetworkHosts(hosts)) => {
                format!(" grant=hosts{}", words(&hosts.hosts))
            }
            None => " grant=?".into(),
        },
    }
}

/// Where a suggestion would be saved: amux's value for it, and Claude's
/// word, which a client shows for a destination it does not know.
fn destination(suggestion: &PermissionUpdate) -> (ScopeDestination, String) {
    let typed = match suggestion {
        PermissionUpdate::AddRules { destination, .. }
        | PermissionUpdate::ReplaceRules { destination, .. }
        | PermissionUpdate::RemoveRules { destination, .. }
        | PermissionUpdate::SetMode { destination, .. }
        | PermissionUpdate::AddDirectories { destination, .. }
        | PermissionUpdate::RemoveDirectories { destination, .. } => Some(destination),
        PermissionUpdate::Unknown(_) => None,
    };
    let destination = match typed {
        Some(PermissionUpdateDestination::Session) => ScopeDestination::Session,
        Some(PermissionUpdateDestination::LocalSettings) => ScopeDestination::LocalSettings,
        Some(PermissionUpdateDestination::ProjectSettings) => ScopeDestination::ProjectSettings,
        Some(PermissionUpdateDestination::UserSettings) => ScopeDestination::UserSettings,
        Some(PermissionUpdateDestination::CliArg) | None => ScopeDestination::Unspecified,
    };
    let word = suggestion.destination().unwrap_or_default().to_owned();
    (destination, word)
}

/// A finished Write's result: Claude says whether it made the file.
#[derive(Deserialize)]
struct WriteResult {
    #[serde(rename = "type")]
    kind: String,
}

/// A finished edit's result: the hunks of the change it made.
#[derive(Deserialize)]
struct EditResult {
    #[serde(rename = "structuredPatch", default)]
    structured_patch: Vec<Hunk>,
}

#[derive(Deserialize)]
struct Hunk {
    #[serde(rename = "oldStart", default)]
    old_start: u32,
    #[serde(rename = "newStart")]
    new_start: u32,
    #[serde(default)]
    lines: Vec<String>,
}

/// What a finished file write or edit states on its step. Read from
/// Claude's result, which says each only once the change is made.
#[derive(Debug, Default, PartialEq)]
pub struct FileStep {
    /// Whether a write made the file, else replaced one.
    pub created: Option<bool>,
    /// The line the first change landed on.
    pub line: Option<u32>,
    pub patch: Vec<PatchHunk>,
}

pub fn file_step(name: &str, server: &str, result: &str) -> FileStep {
    if !server.is_empty() || result.is_empty() {
        return FileStep::default();
    }
    let created = match name {
        "Write" => serde_json::from_str::<WriteResult>(result)
            .ok()
            .and_then(|write| match write.kind.as_str() {
                "create" => Some(true),
                "update" => Some(false),
                _ => None,
            }),
        _ => None,
    };
    let hunks = match name {
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            serde_json::from_str::<EditResult>(result)
                .map(|edit| edit.structured_patch)
                .unwrap_or_default()
        }
        _ => Vec::new(),
    };
    let line = match name {
        "Write" | "Edit" | "MultiEdit" if created != Some(true) => hunks
            .iter()
            .find_map(|hunk| crate::shared::first_change(hunk.new_start, &hunk.lines)),
        _ => None,
    };
    FileStep {
        created,
        line,
        patch: hunks
            .into_iter()
            .map(|hunk| PatchHunk {
                old_start: hunk.old_start,
                new_start: hunk.new_start,
                lines: hunk.lines,
            })
            .collect(),
    }
}

/// A call's input as the views draw it, read once here so no view parses
/// Claude's JSON. `old_source` is a notebook cell's text as it stands,
/// which the host reads when Claude asks to edit it (Claude names only the
/// cell); a call carries none.
pub fn tool_input(name: &str, server: &str, input: &Value, old_source: &str) -> ToolInput {
    let of = if !server.is_empty() {
        tool_input::Of::OtherJson(input.to_string().into_bytes())
    } else {
        match name {
            "Bash" => tool_input::Of::Shell(ShellInput {
                description: read::<BashInput>(input).description,
            }),
            "Edit" | "MultiEdit" => {
                let edit = read::<EditCall>(input);
                let edits = edit.edits.unwrap_or_else(|| vec![edit.one]);
                tool_input::Of::Edit(EditInput {
                    replacements: edits
                        .into_iter()
                        .map(|edit| Replacement {
                            old: edit.old_string,
                            new: edit.new_string,
                        })
                        .collect(),
                    ..Default::default()
                })
            }
            "Write" => tool_input::Of::Edit(EditInput {
                replacements: vec![Replacement {
                    old: String::new(),
                    new: read::<WriteCall>(input).content,
                }],
                write: true,
                ..Default::default()
            }),
            "NotebookEdit" => {
                let edit = read::<NotebookEditCall>(input);
                let mode = match edit.edit_mode.as_str() {
                    "insert" => NotebookEditMode::Insert,
                    "delete" => NotebookEditMode::Delete,
                    _ => NotebookEditMode::Replace,
                };
                tool_input::Of::Edit(EditInput {
                    replacements: vec![Replacement {
                        old: old_source.to_owned(),
                        new: if mode == NotebookEditMode::Delete {
                            String::new()
                        } else {
                            edit.new_source
                        },
                    }],
                    write: false,
                    cell: Some(NotebookCell {
                        id: edit.cell_id,
                        mode: mode as i32,
                        cell_type: edit.cell_type,
                    }),
                })
            }
            "Agent" | "Task" => tool_input::Of::Subagent(wire::Empty {}),
            "TodoWrite" | "TaskCreate" | "TaskUpdate" | "TaskList" | "TaskGet" => {
                tool_input::Of::TaskList(wire::Empty {})
            }
            "WebFetch" | "WebSearch" => tool_input::Of::Web(wire::Empty {}),
            _ => tool_input::Of::OtherJson(input.to_string().into_bytes()),
        }
    };
    ToolInput {
        subject: subject(input),
        of: Some(of),
    }
}

/// A built-in tool's input as its fields; one Claude wrote some other way
/// reads as empty.
fn read<T: Default + for<'de> Deserialize<'de>>(input: &Value) -> T {
    T::deserialize(input).unwrap_or_default()
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct BashInput {
    description: String,
}

/// Edit's one replacement, or MultiEdit's list of them.
#[derive(Default, Deserialize)]
#[serde(default)]
struct EditCall {
    edits: Option<Vec<OneEdit>>,
    #[serde(flatten)]
    one: OneEdit,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct OneEdit {
    old_string: String,
    new_string: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct WriteCall {
    content: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct NotebookEditCall {
    cell_id: String,
    new_source: String,
    cell_type: String,
    edit_mode: String,
}

/// The fields that say what a call is about, in the order a row prefers
/// them.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Named {
    file_path: String,
    pattern: String,
    url: String,
    query: String,
    path: String,
    command: String,
    description: String,
    notebook_path: String,
}

/// What a call names: the first of the fields that say what it is about,
/// else its first string argument.
fn subject(input: &Value) -> String {
    let named = read::<Named>(input);
    [
        named.file_path,
        named.pattern,
        named.url,
        named.query,
        named.path,
        named.command,
        named.description,
        named.notebook_path,
    ]
    .into_iter()
    .find(|value| !value.is_empty())
    .unwrap_or_else(|| crate::shared::first_string(input))
}

/// Every scope choice Claude offered with a permission request, in order.
pub(crate) fn permission_scopes(suggestions: &[PermissionUpdate]) -> Vec<ClaudeGrant> {
    suggestions
        .iter()
        .map(|suggestion| {
            let (rules, directories, mode) = match suggestion {
                PermissionUpdate::AddRules { rules, .. }
                | PermissionUpdate::ReplaceRules { rules, .. }
                | PermissionUpdate::RemoveRules { rules, .. } => (
                    rules
                        .iter()
                        .map(|rule| match &rule.rule_content {
                            Some(content) => format!("{}({content})", rule.tool_name),
                            None => rule.tool_name.clone(),
                        })
                        .collect(),
                    Vec::new(),
                    None,
                ),
                PermissionUpdate::AddDirectories { directories, .. }
                | PermissionUpdate::RemoveDirectories { directories, .. } => {
                    (Vec::new(), directories.clone(), None)
                }
                PermissionUpdate::SetMode { mode, .. } => {
                    (Vec::new(), Vec::new(), Some(mode.as_str().to_owned()))
                }
                PermissionUpdate::Unknown(_) => (Vec::new(), Vec::new(), None),
            };
            let (destination, destination_word) = destination(suggestion);
            ClaudeGrant {
                rules,
                directories,
                mode,
                destination: destination as i32,
                destination_word,
            }
        })
        .collect()
}

/// TaskCreate's input.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskCreateInput {
    #[serde(default)]
    subject: String,
    #[serde(default)]
    active_form: String,
}

/// TaskCreate's result: the task it made.
#[derive(Deserialize)]
struct TaskCreated {
    task: CreatedTask,
}

#[derive(Deserialize)]
struct CreatedTask {
    id: String,
}

/// TaskUpdate's input: the task and what changes; a status of `deleted`
/// removes it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskUpdateInput {
    #[serde(default)]
    task_id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    active_form: Option<String>,
}

/// TaskUpdate's result.
#[derive(Deserialize)]
struct TaskUpdated {
    #[serde(default)]
    success: Option<bool>,
}

/// A task tool's call and result applied to the task list. Upserts by
/// task id, so two reports of the same result change nothing twice.
pub(crate) fn apply_task_tool(
    tasks: &mut Option<Vec<Task>>,
    name: &str,
    input: &Value,
    result: &Value,
) {
    if !TASK_TOOLS.contains(&name) {
        return;
    }
    let tasks = tasks.get_or_insert_with(Vec::new);
    match name {
        "TaskCreate" => {
            let Ok(created) = TaskCreated::deserialize(result) else {
                return;
            };
            let task_id = created.task.id;
            if tasks.iter().any(|task| task.id == task_id) {
                return;
            }
            let (subject, active_form) = TaskCreateInput::deserialize(input)
                .map(|input| (input.subject, input.active_form))
                .unwrap_or_default();
            tasks.push(Task {
                id: task_id,
                subject,
                status: TaskListStatus::Pending as i32,
                active_form,
            });
        }
        "TaskUpdate" => {
            if TaskUpdated::deserialize(result).is_ok_and(|result| result.success == Some(false)) {
                return;
            }
            let Ok(input) = TaskUpdateInput::deserialize(input) else {
                return;
            };
            if input.status.as_deref() == Some("deleted") {
                tasks.retain(|task| task.id != input.task_id);
                return;
            }
            let Some(task) = tasks.iter_mut().find(|task| task.id == input.task_id) else {
                return;
            };
            if let Some(status) = input.status.as_deref().and_then(task_status) {
                task.status = status;
            }
            if let Some(subject) = input.subject {
                task.subject = subject;
            }
            if let Some(active) = input.active_form {
                task.active_form = active;
            }
        }
        _ => {}
    }
}

/// The snapshot's task list: unknown until a task tool ran.
pub(crate) fn task_list(tasks: &Option<Vec<Task>>) -> TaskList {
    match tasks {
        None => crate::unknown::task_list(),
        Some(tasks) => TaskList {
            known: true,
            entries: tasks
                .iter()
                .map(|task| TaskListEntry {
                    id: task.id.clone(),
                    subject: task.subject.clone(),
                    status: task.status,
                    active_form: task.active_form.clone(),
                })
                .collect(),
        },
    }
}

/// How a call is drawn: a built-in tool that only looks takes its verb, and
/// anything else may change the world.
pub(crate) fn tool_class(server: &str, name: &str) -> ToolClass {
    if server.is_empty()
        && let Some((_, class)) = EXPLORATION.iter().find(|(tool, _)| *tool == name)
    {
        *class
    } else {
        ToolClass::Consequential
    }
}

/// A class as goldens show it: nothing for a consequential call, the verb
/// for one that only looks.
pub(crate) fn describe_class(class: i32) -> &'static str {
    match ToolClass::try_from(class) {
        Ok(ToolClass::Read) => " read",
        Ok(ToolClass::Search) => " search",
        Ok(ToolClass::List) => " list",
        Ok(ToolClass::Fetch) => " fetch",
        Ok(ToolClass::WebSearch) => " web-search",
        Ok(ToolClass::Look) => " look",
        Ok(ToolClass::Unspecified | ToolClass::Consequential) | Err(_) => "",
    }
}

/// Open asks as goldens show them.
pub(crate) fn describe_asks(asks: &[Ask]) -> String {
    asks.iter()
        .map(|ask| {
            let body = match &ask.body {
                Some(wire::ask::Body::Permission(permission)) => format!(
                    "permission:{}{}{}{}",
                    permission.tool_name,
                    if permission.server.is_empty() {
                        String::new()
                    } else {
                        format!("@{}", permission.server)
                    },
                    permission
                        .scopes
                        .iter()
                        .map(|grant| {
                            format!(
                                "/{}{}{}",
                                grant.destination_word,
                                grant
                                    .mode
                                    .as_ref()
                                    .map(|mode| format!(" mode={mode}"))
                                    .unwrap_or_default(),
                                grant
                                    .rules
                                    .iter()
                                    .chain(&grant.directories)
                                    .map(|rule| format!(" {rule}"))
                                    .collect::<String>()
                            )
                        })
                        .collect::<String>(),
                    [
                        (!permission.reason.is_empty()).then(|| format!(
                            " reason={}",
                            Value::String(permission.reason.clone())
                        )),
                        (!permission.description.is_empty()).then(|| format!(
                            " description={}",
                            Value::String(permission.description.clone())
                        )),
                        permission
                            .deny_can_stop
                            .then(|| " deny-can-stop".to_owned()),
                        permission.deny_stops.then(|| " deny-stops".to_owned()),
                        permission
                            .default_to_no
                            .then(|| " default-to-no".to_owned()),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<String>()
                ),
                Some(wire::ask::Body::Question(question)) => format!(
                    "question:{}",
                    question
                        .questions
                        .iter()
                        .map(|question| format!(
                            "{}{}{}",
                            question.options.len(),
                            if question.multi_select { "m" } else { "" },
                            if question.allow_other { "+other" } else { "" }
                        ))
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                Some(wire::ask::Body::Plan(plan)) => crate::shared::describe_plan_ask(plan),
                Some(wire::ask::Body::Form(form)) => format!(
                    "form:{} {}",
                    form.server,
                    Value::String(form.message.clone())
                ),
                Some(wire::ask::Body::Link(link)) => format!(
                    "link:{} {} {}",
                    link.server,
                    Value::String(link.message.clone()),
                    link.url
                ),
                Some(wire::ask::Body::Unanswerable(unanswerable)) => {
                    format!("unanswerable:{}", unanswerable.reason)
                }
                None => "none".into(),
            };
            format!(
                "{}->{} {}",
                ask.key,
                if ask.item_key.is_empty() {
                    "-"
                } else {
                    &ask.item_key
                },
                body
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// A task list as goldens show it.
pub(crate) fn describe_tasks(tasks: &TaskList) -> String {
    if !tasks.known {
        return "?".into();
    }
    format!(
        "[{}]",
        tasks
            .entries
            .iter()
            .map(|entry| format!(
                "{}:{}:{}",
                entry.id,
                TaskListStatus::try_from(entry.status).map_or("?", |status| status.as_str_name()),
                Value::String(entry.subject.clone())
            ))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Offered models as `value{effort/effort*}`, the default effort starred.
pub(crate) fn describe_models(models: &[OfferedModel]) -> String {
    models
        .iter()
        .map(|model| {
            let efforts = model
                .efforts
                .iter()
                .map(|effort| {
                    if model.default_effort.as_ref() == Some(effort) {
                        format!("{effort}*")
                    } else {
                        effort.clone()
                    }
                })
                .collect::<Vec<_>>();
            if efforts.is_empty() {
                model.value.clone()
            } else {
                format!("{}{{{}}}", model.value, efforts.join("/"))
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Offered commands by count: the lists are long, and the clients' goldens
/// show them in full.
pub(crate) fn describe_commands(commands: &[OfferedCommand]) -> String {
    commands.len().to_string()
}

/// Claude's never-ask permission, reachable only when Claude was launched
/// allowing it.
pub(crate) const NEVER_ASK: &str = "bypassPermissions";

/// Whether launch arguments let Claude reach its never-ask permission.
pub(crate) fn allows_never_ask(args: &[String]) -> bool {
    args.iter().enumerate().any(|(at, arg)| {
        arg == "--dangerously-skip-permissions"
            || arg == "--allow-dangerously-skip-permissions"
            || arg == "--permission-mode=bypassPermissions"
            || (arg == "--permission-mode"
                && args.get(at + 1).is_some_and(|mode| mode == NEVER_ASK))
    })
}

/// The value of `--flag value` or `--flag=value` in launch arguments. A
/// flag given more than once takes its last value, as Claude's command
/// line does: amux appends the effective model after one the person
/// passed.
pub(crate) fn launch_arg(args: &[String], flag: &str) -> Option<String> {
    args.iter().enumerate().rev().find_map(|(index, arg)| {
        if arg == flag {
            args.get(index + 1).cloned()
        } else {
            arg.strip_prefix(flag)?.strip_prefix('=').map(str::to_owned)
        }
    })
}

/// The permission launch arguments start Claude in, when they name one.
/// `--dangerously-skip-permissions` wins over any mode, as on Claude's own
/// command line; otherwise the last `--permission-mode` does, since amux
/// appends the chosen permission after one the person passed.
pub(crate) fn launched_permission(args: &[String]) -> Option<String> {
    if args
        .iter()
        .any(|arg| arg == "--dangerously-skip-permissions")
    {
        return Some(NEVER_ASK.to_owned());
    }
    launch_arg(args, "--permission-mode")
}

/// Claude's text as a wire field carries it: without the colour, cursor
/// and link codes a terminal program writes (CSI and OSC sequences, in
/// their escaped and their one-character C1 forms, and the other escapes)
/// or any other control character but a newline and a tab. Every field
/// made of Claude's words goes through this one cleaner.
pub(crate) fn without_terminal_codes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    // CSI: parameters and intermediates, then one final byte.
    let csi = |chars: &mut std::iter::Peekable<std::str::Chars>| {
        for c in chars.by_ref() {
            if ('\u{40}'..='\u{7e}').contains(&c) {
                break;
            }
        }
    };
    // OSC: up to BEL or ST, in either form.
    let osc = |chars: &mut std::iter::Peekable<std::str::Chars>| {
        while let Some(c) = chars.next() {
            if c == '\u{7}' || c == '\u{9c}' {
                break;
            }
            if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                chars.next();
                break;
            }
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => csi(&mut chars),
                Some(']') => osc(&mut chars),
                // A two-character escape.
                _ => {}
            },
            '\u{9b}' => csi(&mut chars),
            '\u{9d}' => osc(&mut chars),
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// The models an initialize answer says take Claude's auto permission, by
/// the value Claude lists ("haiku") and the model it resolves to
/// ("claude-haiku-5-5"), which is how Claude's rows and hooks name the
/// running model.
pub(crate) fn auto_models(models: &[ModelInfo]) -> Vec<String> {
    let mut named = Vec::new();
    for model in models
        .iter()
        .filter(|model| model.supports_auto_mode == Some(true))
    {
        for name in std::iter::once(&model.value).chain(model.resolved_model.as_ref()) {
            if !named.contains(name) {
                named.push(name.clone());
            }
        }
    }
    named
}

/// The permissions Claude offers, ask first. Auto names the models that
/// take it when Claude listed its models (None: unknown, so unrestricted),
/// and is left out when none does; never-ask is listed only when it can be
/// reached. Terminal Claude's can only be cycled through, so they are
/// listed as not settable.
pub(crate) fn permissions(
    auto_models: Option<&[String]>,
    never_ask: bool,
    settable: bool,
) -> Vec<OfferedPermission> {
    let permission = |value: &str, display_name: &str| OfferedPermission {
        value: value.to_owned(),
        display_name: display_name.to_owned(),
        settable,
        ..OfferedPermission::default()
    };
    let mut offered = vec![
        OfferedPermission {
            normal: true,
            ..permission("default", "Ask")
        },
        permission("acceptEdits", "Accept edits"),
        permission("plan", "Plan"),
    ];
    match auto_models {
        Some([]) => {}
        models => offered.push(OfferedPermission {
            models: models.unwrap_or_default().to_vec(),
            ..permission("auto", "Auto")
        }),
    }
    if never_ask {
        offered.push(OfferedPermission {
            never_asks: true,
            ..permission(NEVER_ASK, "Never ask")
        });
    }
    offered
}

/// The models an initialize answer lists, as headless Claude answers it.
pub(crate) fn offered_models(models: &[ModelInfo]) -> Vec<OfferedModel> {
    models
        .iter()
        .map(|model| OfferedModel {
            value: model.value.clone(),
            display_name: model.display_name.clone(),
            description: model.description.clone(),
            efforts: model.supported_effort_levels.clone().unwrap_or_default(),
            // Claude names no default effort per model.
            default_effort: None,
            resolved_model: model.resolved_model.clone().unwrap_or_default(),
        })
        .collect()
}

/// The commands an initialize answer lists. A plugin's command is named
/// `plugin:command`; the plugin is its source.
pub(crate) fn offered_commands(commands: &[SlashCommand]) -> Vec<OfferedCommand> {
    commands
        .iter()
        .map(|command| OfferedCommand {
            name: command.name.clone(),
            description: command.description.clone(),
            argument_hint: command.argument_hint.clone(),
            source: command
                .name
                .split_once(':')
                .map(|(plugin, _)| plugin.to_owned())
                .unwrap_or_default(),
        })
        .collect()
}

/// A Claude model id read as a name when no offered model names it:
/// "claude-opus-4-1-20250805" reads "Opus 4.1" and "claude-opus-5[1m]"
/// reads "Opus 5": the family prefix, a trailing date and a bracketed
/// variant go. An id of no recognisable shape stays as it is.
pub(crate) fn tidy_model(id: &str) -> String {
    let base = match id.split_once('[') {
        Some((base, _)) if id.ends_with(']') && !base.is_empty() => base,
        _ => id,
    };
    let mut parts: Vec<&str> = base.split('-').filter(|part| !part.is_empty()).collect();
    if parts
        .last()
        .is_some_and(|last| last.len() == 8 && last.chars().all(|c| c.is_ascii_digit()))
    {
        parts.pop();
    }
    if parts.first() == Some(&"claude") {
        parts.remove(0);
    }
    crate::shared::tidy_model_parts(id, Vec::new(), &parts)
}

/// A model's display name in a golden: quoted, or `?` while unknown.
pub(crate) fn describe_model_name(name: Option<&str>) -> String {
    name.map_or("?".to_owned(), |name| {
        serde_json::Value::String(name.to_owned()).to_string()
    })
}

/// Tool-server health with amux's own server listed as off: where an
/// organization manages Claude's MCP servers, Claude takes none added on
/// its command line, so the agent launched it without amux's, and the
/// person reads why amux's tools are missing.
pub(crate) fn amux_tools_withheld(mut health: ToolServerHealth) -> ToolServerHealth {
    health.servers.push(ToolServer {
        name: crate::AMUX_TOOL_SERVER.to_owned(),
        status: ToolServerStatus::Failed as i32,
        error:
            "Your organization's MCP policy allows no other tool servers, so amux's tools are off."
                .to_owned(),
    });
    health.state = HealthState::Degraded as i32;
    health
}

/// A Claude item as the emission goldens print it.
pub(crate) fn describe_item(body: &[u8]) -> ItemView {
    use wire::claude_item::Kind;
    let item = wire::ClaudeItem::decode(body).unwrap_or_default();
    let (arm, complete, text) = match item.kind {
        None => ("none", true, String::new()),
        Some(Kind::Prompt(_)) => ("prompt", true, String::new()),
        Some(Kind::Steer(_)) => ("steer", true, String::new()),
        Some(Kind::Message(text)) => ("message", text.complete, String::new()),
        Some(Kind::Thinking(thinking)) => ("thinking", thinking.complete, String::new()),
        Some(Kind::Tool(tool)) => ("tool", true, describe_tool(&tool)),
        Some(Kind::Ask(item)) => ("ask", true, crate::ask_item::describe(&item)),
        Some(Kind::Plan(plan)) => ("plan", plan.complete, crate::shared::describe_plan(&plan)),
        Some(Kind::Task(task)) => (
            "task",
            true,
            format!(
                "{} {} {} tools={} last={} tokens={}",
                task.task_id,
                wire::TaskState::try_from(task.state).map_or("?", |state| state.as_str_name()),
                Value::String(task.description),
                task.tool_count,
                or_dash(&task.last_tool),
                task.tokens,
            ),
        ),
        Some(Kind::Turn(turn)) => (
            "turn",
            true,
            format!(
                "#{} {} started={}",
                turn.turn_id,
                wire::TurnOutcome::try_from(turn.outcome)
                    .map_or("?", |outcome| outcome.as_str_name()),
                turn.started_at_ms,
            ),
        ),
        Some(Kind::Status(status)) => (
            "status",
            true,
            format!(
                "{} {}",
                wire::SdkStatus::try_from(status.status).map_or("?", |status| status.as_str_name()),
                status.word
            ),
        ),
        Some(Kind::Boundary(boundary)) => (
            "boundary",
            true,
            format!(
                "{}{}",
                wire::BoundaryKind::try_from(boundary.kind).map_or("?", |kind| kind.as_str_name()),
                describe_cause(&boundary)
            ),
        ),
        Some(Kind::CompactSummary(_)) => ("compact_summary", true, String::new()),
        Some(Kind::Interruption(_)) => ("interruption", true, String::new()),
        Some(Kind::AgentMessage(message)) => (
            "agent_message",
            true,
            crate::shared::describe_agent_message(&message),
        ),
        Some(Kind::ApiError(error)) => (
            "api_error",
            true,
            format!(
                "{} {} retry={} {}/{}{}",
                error.error_kind,
                Value::String(error.message),
                error.will_retry,
                error.attempt,
                error.max_attempts,
                error
                    .retry_at_ms
                    .map_or(String::new(), |at| format!(" at={at}"))
            ),
        ),
        Some(Kind::Slash(slash)) => (
            "slash",
            true,
            format!("{} args={}", slash.command, Value::String(slash.args)),
        ),
        Some(Kind::Unrecognized(unrecognized)) => (
            "unrecognized",
            true,
            format!(
                "{} {}",
                unrecognized.fact_type,
                Value::String(unrecognized.summary)
            ),
        ),
        Some(Kind::ModelSwitch(switch)) => (
            "model_switch",
            true,
            format!(
                "{} -> {} {}",
                switch.from,
                switch.to,
                Value::String(switch.reason)
            ),
        ),
        Some(Kind::Compaction(compaction)) => (
            "compaction",
            true,
            format!(
                "{}→{}{}",
                compaction
                    .tokens_before
                    .map_or("?".to_owned(), |tokens| tokens.to_string()),
                compaction
                    .tokens_after
                    .map_or("?".to_owned(), |tokens| tokens.to_string()),
                if compaction.automatic { " auto" } else { "" }
            ),
        ),
    };
    ItemView {
        arm: arm.into(),
        complete,
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claude's command line takes the last of a repeated flag, and amux
    /// appends the effective model after any the person passed: the model
    /// that runs is the last one named.
    #[test]
    fn a_repeated_launch_flag_reads_as_its_last_value() {
        let args = |of: &[&str]| of.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            launch_arg(&args(&["--model", "sonnet", "--model", "opus"]), "--model").as_deref(),
            Some("opus")
        );
        assert_eq!(
            launch_arg(
                &args(&["--model=sonnet", "--effort", "high", "--model", "opus"]),
                "--model"
            )
            .as_deref(),
            Some("opus")
        );
        assert_eq!(
            launch_arg(&args(&["--model", "opus", "--model=haiku"]), "--model").as_deref(),
            Some("haiku")
        );
        assert_eq!(launch_arg(&args(&["--effort", "high"]), "--model"), None);
    }

    /// amux appends the chosen permission after any `--permission-mode`
    /// the person passed, and Claude starts in the last one named.
    #[test]
    fn a_repeated_permission_mode_reads_as_its_last_value() {
        let args = |of: &[&str]| of.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            launched_permission(&args(&[
                "--permission-mode",
                "plan",
                "--permission-mode",
                "acceptEdits"
            ]))
            .as_deref(),
            Some("acceptEdits")
        );
        assert_eq!(
            launched_permission(&args(&[
                "--permission-mode=plan",
                "--permission-mode",
                "auto"
            ]))
            .as_deref(),
            Some("auto")
        );
        assert_eq!(
            launched_permission(&args(&["--dangerously-skip-permissions"])).as_deref(),
            Some(NEVER_ASK)
        );
        assert_eq!(launched_permission(&args(&["--model", "opus"])), None);
    }

    /// The cell source the host read is what a notebook edit replaces; a
    /// deleted cell is replaced by nothing.
    #[test]
    fn a_notebook_edit_replaces_the_cell_the_host_read() {
        let input = serde_json::json!({"notebook_path": "n.ipynb", "new_source": "print(10)"});
        let replacements = |input: &Value, old_source: &str| match tool_input(
            "NotebookEdit",
            "",
            input,
            old_source,
        )
        .of
        {
            Some(tool_input::Of::Edit(edit)) => edit
                .replacements
                .into_iter()
                .map(|replacement| (replacement.old, replacement.new))
                .collect::<Vec<_>>(),
            other => panic!("not an edit: {other:?}"),
        };
        assert_eq!(
            replacements(&input, "print(1)"),
            [("print(1)".to_owned(), "print(10)".to_owned())]
        );
        let deleted = serde_json::json!({"notebook_path": "n.ipynb", "edit_mode": "delete"});
        assert_eq!(
            replacements(&deleted, "print(1)"),
            [("print(1)".to_owned(), String::new())]
        );
    }

    #[test]
    fn a_finished_write_says_whether_it_made_the_file_and_an_edit_where_it_landed() {
        let created =
            r#"{"type":"create","filePath":"/w/a.txt","content":"hi","structuredPatch":[]}"#;
        let facts = |name, server, result| {
            let step = file_step(name, server, result);
            (step.created, step.line)
        };
        assert_eq!(facts("Write", "", created), (Some(true), None));
        let updated = r#"{"type":"update","filePath":"/w/a.txt","structuredPatch":[{"oldStart":3,"oldLines":3,"newStart":3,"newLines":3,"lines":[" a","-b","+c"]}]}"#;
        assert_eq!(facts("Write", "", updated), (Some(false), Some(4)));
        let edit = r#"{"filePath":"/w/a.rs","oldString":"b","newString":"c","structuredPatch":[{"oldStart":40,"oldLines":7,"newStart":40,"newLines":7,"lines":[" x"," y"," z","-b","+c"," p"]}]}"#;
        assert_eq!(facts("Edit", "", edit), (None, Some(43)));
        assert_eq!(
            file_step("Edit", "", edit).patch,
            [PatchHunk {
                old_start: 40,
                new_start: 40,
                lines: [" x", " y", " z", "-b", "+c", " p"]
                    .map(str::to_owned)
                    .to_vec(),
            }]
        );
        assert_eq!(file_step("Edit", "server", edit), FileStep::default());
        assert_eq!(file_step("Read", "", edit), FileStep::default());
    }
}
