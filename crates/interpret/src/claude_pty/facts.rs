//! Transcript rows, hook payloads and the launch fact, read into state and
//! items. The module docs in `mod.rs` state the inference rules.

use claude_protocol::hooks::{
    self, Elicitation, MessageDisplay, Payload, PermissionRequest, PostToolUse, PostToolUseFailure,
    PreToolUse, SessionStart,
};
use claude_protocol::stream::{
    CompactTrigger, ContentBlock, MessageContent, PermissionMode, PermissionUpdate,
    PermissionUpdateDestination, ToolResultBody,
};
use claude_protocol::transcript::{self, AssistantRow, Attachment, Row, SystemRow, UserRow};
use serde::Deserialize;
use serde_json::Value;
use wire::claude_pty_item::Kind;
use wire::{Ask, BoundaryKind, DecisionOutcome, PermissionAsk, ToolState, Turn, TurnOutcome};

use super::{
    AgentFact, Announced, AskMeta, AskShape, Decision, HookRow, PendingMessage, Running, Slash,
    State, Stream, Subagent, Tool, item_body,
};
use crate::claude_answers::AnswerShape;
use crate::claude_common::{
    AnsweredResult, BackgroundInput, BangInput, JobInput, PLAN_TOOL, QUESTION_TOOL, TASK_TOOLS,
    Verdict, apply_task_tool, blocks_text, compact_json, job_kind, message_text, old_source,
    permission_scopes, plan_ask, plan_mode_write, question_ask, question_ask_text, split_tool_name,
    timestamp_ms, tool_class, tool_input, tool_result_images, tool_result_text,
    without_image_bytes, without_terminal_codes,
};
use crate::{
    Channel, Emit, Fact, ItemDraft, ask_item, is_attach_tool, is_status_tool, status_working_on,
};

/// How terminal Claude words a call the person refused, on the tool result.
const REJECTED: &str = "The user doesn't want to proceed with this tool use.";
const REJECTED_NOTE: &str = "the user said:\n";
const INTERRUPTED: &str = "[Request interrupted by user";
/// The tool that starts a subagent.
const AGENT_TOOL: &str = "Agent";
const TRUST_QUESTION: &str = "Do you trust the files in this folder?";
pub(super) const TRUST_YES: &str = "Trust this folder";
const TRUST_NO: &str = "Exit";

/// The Agent call's immediate result, as far as telling a subagent
/// launched in the background goes.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentLaunch {
    #[serde(default)]
    is_async: bool,
    #[serde(default)]
    agent_id: Option<String>,
}

/// The agent id of a subagent Claude launched in the background, from the
/// Agent call's immediate result.
fn launched_in_background(result: &Value) -> Option<String> {
    AgentLaunch::deserialize(result)
        .ok()
        .filter(|launch| launch.is_async)?
        .agent_id
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShellLaunch {
    #[serde(default)]
    background_task_id: Option<String>,
}

/// The task id of a shell Bash started in the background, from the call's
/// immediate result.
fn shell_in_background(result: &Value) -> Option<String> {
    ShellLaunch::deserialize(result)
        .ok()?
        .background_task_id
        .filter(|id| !id.is_empty())
}

/// The text between `<tag>` and `</tag>`, trimmed.
fn between<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim())
}

/// A user row's text with the pasted blocks unwrapped. Claude 2.1.283
/// records a long paste, which is how amux types a prompt, between
/// `<pasted_content id="…">` and `</pasted_content id="…">` lines, after a
/// blank line; the person sent only what is inside.
fn unwrap_pasted(text: &str) -> String {
    const OPEN: &str = "<pasted_content id=\"";
    let mut out = String::new();
    let mut rest = text;
    let mut unwrapped = false;
    while let Some(at) = rest.find(OPEN) {
        let after = &rest[at + OPEN.len()..];
        let Some((id, inner)) = after.split_once("\">") else {
            break;
        };
        let close = format!("</pasted_content id=\"{id}\">");
        let Some(end) = inner.find(&close) else {
            break;
        };
        out.push_str(&rest[..at]);
        let body = &inner[..end];
        let body = body.strip_prefix('\n').unwrap_or(body);
        out.push_str(body.strip_suffix('\n').unwrap_or(body));
        rest = &inner[end + close.len()..];
        unwrapped = true;
    }
    out.push_str(rest);
    if unwrapped {
        out.trim_matches('\n').to_owned()
    } else {
        out
    }
}

/// The messages from another session a user row shows the model: the body
/// of each `<cross-session-message …>` element (Claude 2.1.240), or the
/// text between the peer preamble and its trust note (2.1.282).
fn peer_message_bodies(text: &str) -> Vec<String> {
    const OPEN: &str = "<cross-session-message";
    const CLOSE: &str = "</cross-session-message>";
    const PREAMBLE: &str = "Another Claude session sent a message:\n";
    const TRUST_NOTE: &str = "\n\nThis came from another Claude session";
    let mut bodies = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        let after = &rest[at..];
        let Some(tag_end) = after.find('>') else {
            break;
        };
        let inner = &after[tag_end + 1..];
        let Some(close) = inner.find(CLOSE) else {
            break;
        };
        bodies.push(inner[..close].trim().to_owned());
        rest = &inner[close + CLOSE.len()..];
    }
    if bodies.is_empty()
        && let Some(message) = text.trim_start().strip_prefix(PREAMBLE)
    {
        let end = message.find(TRUST_NOTE).unwrap_or(message.len());
        bodies.push(message[..end].trim().to_owned());
    }
    bodies
}

impl State {
    pub(super) fn fact(&mut self, emit: &mut Emit, fact: Fact) {
        match fact.channel {
            Channel::Transcript => {
                if let Ok(row) = transcript::decode(&fact.payload) {
                    self.row(emit, &row);
                    self.open_hook_rows(emit);
                }
            }
            Channel::Hook => {
                if let Ok(payload) = hooks::decode(&fact.payload) {
                    self.hook(emit, &payload);
                }
            }
            Channel::Agent => {
                if let Ok(fact) = serde_json::from_slice::<AgentFact>(&fact.payload) {
                    self.agent_fact(emit, fact);
                }
            }
            Channel::Stream | Channel::Rpc | Channel::Tools => {}
        }
    }

    fn agent_fact(&mut self, emit: &mut Emit, fact: AgentFact) {
        let version = match fact {
            AgentFact::Launch { version } => version,
            // Waiting on its trust dialog, Claude takes no prompt; it
            // starts its session once trusted, and that start says so.
            AgentFact::Ready if self.trust_ask().is_some() => return,
            AgentFact::Ready => return self.shared.provider_started(),
            AgentFact::TrustDialog => return self.trust_dialog(emit),
        };
        let provider = &mut self.provider;
        provider.version = (!version.is_empty()).then_some(version);
        provider.relaunched = provider.launches > 0;
        provider.launches += 1;
        self.publish_catalogue(emit);
    }

    /// The permission a plan ask's switch moves to: what the first entry
    /// of Claude's own plan menu switches to. Never ask where Claude was
    /// launched allowing it, else auto mode where its model offers it, else
    /// accept edits.
    fn plan_switch(&self) -> String {
        if self.provider.never_ask {
            return crate::claude_common::NEVER_ASK.to_owned();
        }
        crate::claude_common::plan_switch(self.offers_auto()).to_owned()
    }

    /// A terminal offers nothing a program can read: its models and
    /// commands are what the host's Claude offers, and its permissions only
    /// its own cycle key reaches. Published at launch, since Claude starts
    /// its session only once a prompt arrives.
    fn publish_catalogue(&mut self, emit: &mut Emit) {
        let offered = self.provider.offered.clone().unwrap_or_default();
        let auto_models = offered
            .permissions
            .iter()
            .find(|permission| permission.value == "auto")
            .map(|auto| auto.models.clone());
        let permissions = crate::claude_common::permissions(
            if offered.permissions.is_empty() {
                None
            } else {
                Some(auto_models.as_deref().unwrap_or_default())
            },
            self.provider.never_ask,
            false,
        );
        self.shared.set_catalogue(
            emit,
            wire::Catalogue {
                models: offered.models,
                commands: offered.commands,
                permissions,
                ..Default::default()
            },
        );
    }

    pub(super) fn exited(&mut self, emit: &mut Emit, exit: wire::IncarnationEnd) {
        emit.end_incarnation(exit.clone());
        self.running.clear();
        self.close_all_unknown(emit);
        self.boundary(emit, BoundaryKind::Exited, exit);
        self.shared.provider_exited();
        self.jobs.clear();
        self.local_turn = false;
    }

    // --- hooks -----------------------------------------------------------

    fn hook(&mut self, emit: &mut Emit, payload: &Payload) {
        let Some(common) = payload.common() else {
            return;
        };
        // While Claude's own view shows another conversation, only its
        // next session start concerns this agent. A hook that conversation
        // waits on is let go at once, so Claude shows its own dialog.
        if self.provider.switched_away
            && !matches!(payload, Payload::SessionStart(_))
            && self.provider.session.as_deref() != Some(common.session_id.as_str())
        {
            let held = match payload {
                Payload::PermissionRequest(request) => hook_number(&request.extensions),
                Payload::Elicitation(elicitation) => hook_number(&elicitation.extensions),
                _ => None,
            };
            if let Some(id) = held {
                emit.effect(crate::Effect::HookAnswer {
                    id,
                    answer: Vec::new(),
                });
            }
            return;
        }
        if let Some(mode) = &common.permission_mode {
            self.permission_seen(emit, mode.as_str());
        }
        if let Some(agent) = common.agent_id.as_deref().filter(|agent| !agent.is_empty())
            && matches!(
                payload,
                Payload::PreToolUse(_)
                    | Payload::PermissionRequest(_)
                    | Payload::PostToolUse(_)
                    | Payload::PostToolUseFailure(_)
                    | Payload::MessageDisplay(_)
            )
        {
            return self.subagent_hook(emit, agent, payload);
        }
        match payload {
            Payload::SessionStart(start) => self.session_start(emit, start),
            Payload::SessionEnd(_) => self.close_all_unknown(emit),
            Payload::UserPromptSubmit(_) => {
                self.between_turns = false;
                self.shared.provider_started();
            }
            Payload::PreToolUse(call) => {
                let (server, name) = split_tool_name(&call.tool_name);
                if server.is_empty() && name == PLAN_TOOL {
                    self.hook_rows.push(HookRow {
                        id: call.tool_use_id.clone(),
                        name: call.tool_name.clone(),
                        input: call.tool_input.clone(),
                    });
                    self.open_hook_rows(emit);
                }
                self.pre_tool_use(call)
            }
            Payload::PermissionRequest(request) => self.permission_request(emit, request),
            Payload::PostToolUse(PostToolUse {
                tool_use_id,
                duration_ms,
                ..
            }) => {
                self.call_ended(tool_use_id);
                if let Some(duration) = duration_ms {
                    self.ran_for(emit, tool_use_id, *duration);
                }
            }
            Payload::PostToolUseFailure(PostToolUseFailure { tool_use_id, .. }) => {
                self.call_ended(tool_use_id)
            }
            // Auto mode's refusal names its rule; the call's row says it
            // was refused.
            Payload::PermissionDenied(denied) => {
                let id = denied.tool_use_id.clone();
                match self.tools.get_mut(&id) {
                    Some(tool) => {
                        tool.decision = Some(Decision::refused(&denied.reason));
                        self.emit_tool(emit, &id);
                    }
                    None => {
                        self.refused.insert(id, denied.reason.clone());
                    }
                }
            }
            Payload::Elicitation(elicitation) => self.elicitation(emit, elicitation),
            Payload::MessageDisplay(display) => self.display(emit, display),
            Payload::PostModelSwitch(switch) if !switch.to_model.is_empty() => {
                self.provider.model = Some(switch.to_model.clone());
            }
            Payload::PostModelSwitch(_) | Payload::Notification(_) => {}
            Payload::Stop(stop) => {
                let background: Vec<String> = stop
                    .background_tasks
                    .iter()
                    .flatten()
                    .map(|task| task.id.clone())
                    .collect();
                if let Some(tasks) = &stop.background_tasks {
                    let now = self.shared.now_ms();
                    let jobs = self.jobs.listed(
                        tasks.iter().map(|task| {
                            (
                                task.id.clone(),
                                task.description.clone().unwrap_or_default(),
                                job_kind(&task.r#type),
                            )
                        }),
                        now,
                    );
                    self.shared.set_jobs(jobs);
                }
                self.running.clear();
                // A subagent Claude still runs in the background keeps the
                // dialog it raised open in Claude's own view: so does amux.
                for key in self.open_ask_keys() {
                    let in_background = self
                        .asks
                        .get(&key)
                        .and_then(|meta| meta.agent.as_ref())
                        .is_some_and(|agent| background.contains(agent));
                    if !in_background {
                        self.close(emit, &key, Decision::unknown());
                    }
                }
            }
            // A subagent finished: it leaves the background list, and a
            // dialog it left open is over.
            Payload::SubagentStop(stop) => {
                let Some(agent) = stop.common.agent_id.clone() else {
                    return;
                };
                if let Some(jobs) = self.jobs.ended(&agent) {
                    self.shared.set_jobs(jobs);
                }
                for key in self.open_ask_keys() {
                    if self.asks.get(&key).and_then(|meta| meta.agent.as_ref()) == Some(&agent) {
                        self.close(emit, &key, Decision::unknown());
                    }
                }
            }
            Payload::Unknown(_) => {}
        }
    }

    fn session_start(&mut self, emit: &mut Emit, start: &SessionStart) {
        self.shared.provider_started();
        self.running.clear();
        // Claude starts its session only once its folder is trusted: No
        // exits.
        self.close_trust_answered_elsewhere(emit);
        self.close_all_unknown(emit);
        let common = &start.common;
        // An agent is its conversation. A resume in Claude's own view of a
        // conversation that is not this agent's moved its terminal to
        // another agent's: nothing of it is followed or typed into, until
        // Claude resumes this one again. A clear is this agent starting
        // afresh, in a new session.
        let ours = self.provider.session.clone();
        if start.source == "resume"
            && !common.session_id.is_empty()
            && ours.as_ref().is_some_and(|ours| *ours != common.session_id)
        {
            self.provider.switched_away = true;
            return;
        }
        self.provider.switched_away = false;
        if !common.session_id.is_empty() {
            self.provider.session = Some(common.session_id.clone());
        }
        if let Some(model) = start.model.as_deref().filter(|model| !model.is_empty()) {
            self.provider.model = Some(model.to_owned());
        }
        self.announced.clear();
        self.finish_streams(emit);
        let path = common.transcript_path.to_string_lossy();
        if !path.is_empty() && self.provider.transcript.as_deref() != Some(&*path) {
            self.provider.transcript = Some(path.clone().into_owned());
            emit.effect(crate::Effect::FollowTranscript {
                path: path.into_owned(),
            });
        }
        let kind = if std::mem::take(&mut self.provider.relaunched) {
            BoundaryKind::Restarted
        } else {
            match start.source.as_str() {
                "resume" => BoundaryKind::Resumed,
                "clear" => BoundaryKind::Cleared,
                "compact" => BoundaryKind::Compacted,
                _ => BoundaryKind::Started,
            }
        };
        if matches!(kind, BoundaryKind::Cleared | BoundaryKind::Compacted) {
            self.context_tokens = None;
            self.end_local_turn();
        }
        if kind == BoundaryKind::Cleared {
            self.shared.steers_lost();
            if self.tasks.is_some() {
                self.tasks = Some(Vec::new());
            }
        }
        self.boundary(emit, kind, wire::IncarnationEnd::default());
    }

    /// A call inside a subagent: a step on its Agent row, and an ask it
    /// raises or answers. What a subagent writes is not drawn as it goes:
    /// its Agent call's result carries its answer.
    fn subagent_hook(&mut self, emit: &mut Emit, agent: &str, payload: &Payload) {
        let row = self.agent_row(agent);
        match payload {
            Payload::PermissionRequest(request) => self.permission_request(emit, request),
            Payload::PreToolUse(call) => {
                self.announce(call, Some(agent));
                if let Some(subagent) = row
                    .as_ref()
                    .and_then(|id| self.tools.get_mut(id))
                    .and_then(|tool| tool.subagent.as_mut())
                {
                    subagent.tool_count += 1;
                    subagent.last_tool = split_tool_name(&call.tool_name).1;
                }
            }
            Payload::PostToolUse(PostToolUse { tool_use_id, .. })
            | Payload::PostToolUseFailure(PostToolUseFailure { tool_use_id, .. }) => {
                self.call_ended(tool_use_id);
                let answered = self
                    .asks
                    .iter()
                    .find(|(_, meta)| meta.call.as_deref() == Some(tool_use_id.as_str()))
                    .map(|(key, _)| key.clone());
                if let Some(key) = answered
                    && self.shared.asks().get(&key).is_some()
                {
                    self.close(emit, &key, Decision::elsewhere(DecisionOutcome::Allowed));
                }
            }
            _ => {}
        }
        if let Some(id) = row {
            self.emit_tool(emit, &id);
        }
    }

    /// The Agent call a subagent belongs to. A background subagent is known
    /// by the id its launch returned; a foreground one's calls come while its
    /// Agent call runs, so the newest running Agent call not yet claimed.
    fn agent_row(&mut self, agent: &str) -> Option<String> {
        if let Some(id) = self.agents.get(agent) {
            return self.tools.contains_key(id).then(|| id.clone());
        }
        let claimed = self.agents.values().cloned().collect::<Vec<_>>();
        let id = self
            .tools
            .iter()
            .filter(|(id, tool)| tool.subagent.is_some() && !tool.finished && !claimed.contains(id))
            .max_by_key(|(_, tool)| tool.seq)
            .map(|(id, _)| id.clone())?;
        self.agents.insert(agent.to_owned(), id.clone());
        Some(id)
    }

    /// The Agent call returned before its subagent finished: the row stays
    /// running until the task notification.
    fn agent_launched(&mut self, id: &str, result: Option<&Value>) -> bool {
        let Some(agent) = result.and_then(launched_in_background) else {
            return false;
        };
        let Some(tool) = self.tools.get_mut(id) else {
            return false;
        };
        if tool.subagent.is_none() {
            return false;
        }
        tool.awaiting_notification = true;
        tool.background = true;
        let (command, at_ms) = (JobInput::command(&tool.input), tool.at_ms);
        self.jobs
            .launched(&agent, id, command, at_ms, wire::BackgroundKind::Subagent);
        if let Some(jobs) = self.jobs.current() {
            self.shared.set_jobs(jobs);
        }
        self.agents.insert(agent, id.to_owned());
        true
    }

    /// Claude asks the person whether a call may run, or asks them a
    /// question or to review a plan: an ask answered through its hook,
    /// pointing at the call its PreToolUse hook announced.
    fn permission_request(&mut self, emit: &mut Emit, request: &PermissionRequest) {
        let name = request.tool_name.as_str();
        let input = &request.tool_input;
        let (server, tool) = split_tool_name(name);
        let mut offered = request.permission_suggestions.clone().unwrap_or_default();
        let (body, shape) = match tool.as_str() {
            QUESTION_TOOL if server.is_empty() => {
                let question = question_ask(input);
                let shape = AnswerShape::Question(
                    question
                        .questions
                        .iter()
                        .map(|question| {
                            (
                                question.question.clone(),
                                question
                                    .options
                                    .iter()
                                    .map(|option| option.label.clone())
                                    .collect(),
                                question.multi_select,
                            )
                        })
                        .collect(),
                );
                (wire::ask::Body::Question(question), shape)
            }
            PLAN_TOOL if server.is_empty() => {
                let switch = self.plan_switch();
                (
                    wire::ask::Body::Plan(plan_ask(&switch)),
                    AnswerShape::Plan { switch },
                )
            }
            _ => {
                // Claude's own menu offers to switch to auto mode in
                // manual mode, after the scope choices: so does the card.
                if self.menu_offers_auto() {
                    offered.push(PermissionUpdate::SetMode {
                        mode: PermissionMode::Auto,
                        destination: PermissionUpdateDestination::Session,
                    });
                }
                (
                    wire::ask::Body::Permission(PermissionAsk {
                        tool_name: tool.clone(),
                        input: Some(tool_input(
                            &tool,
                            &server,
                            input,
                            old_source(&request.extensions),
                        )),
                        scopes: permission_scopes(&offered),
                        reason: String::new(),
                        description: String::new(),
                        deny_stops: false,
                        deny_can_stop: true,
                        server: server.clone(),
                    }),
                    AnswerShape::Permission,
                )
            }
        };
        let agent = request
            .common
            .agent_id
            .clone()
            .filter(|agent| !agent.is_empty());
        let call = self.announced_call(name, input, agent.as_deref());
        let key = self.new_ask(AskMeta {
            seq: 0,
            tool_name: name.to_owned(),
            input: input.to_string(),
            suggestions: offered
                .iter()
                .map(|update| {
                    serde_json::to_value(update)
                        .expect("a permission change serializes")
                        .to_string()
                })
                .collect(),
            shape: AskShape::Answer(shape),
            call: call.clone(),
            bound: None,
            closed: None,
            agent: agent.clone(),
            hook: hook_number(&request.extensions),
        });
        let item_key = match &agent {
            // Shown on the subagent's Agent row, which takes no decision.
            Some(agent) => self.agent_row(agent),
            None => None,
        };
        self.shared.open_ask(Ask {
            key,
            item_key: item_key.unwrap_or_default(),
            body: Some(body),
            opened_at_ms: self.shared.now_ms(),
        });
        // Claude writes the gated call's row at the decision, which points
        // the ask at it; a plan's row is already open from its PreToolUse.
        if let Some(id) = call.filter(|id| agent.is_none() && self.tools.contains_key(id)) {
            self.bind_asks_for(emit, &id);
        }
    }

    /// Records an ask under a new key.
    fn new_ask(&mut self, mut meta: AskMeta) -> String {
        meta.seq = self.next_seq();
        self.next_ask += 1;
        let key = format!("ask:{}", self.next_ask);
        self.asks.insert(key.clone(), meta);
        key
    }

    /// Claude's first screen asks whether its folder is trusted, and
    /// exits on No. It is asked here as a question with those two answers.
    fn trust_dialog(&mut self, emit: &mut Emit) {
        if self
            .asks
            .values()
            .any(|meta| matches!(meta.shape, AskShape::Trust))
        {
            return;
        }
        let key = self.new_ask(AskMeta {
            seq: 0,
            tool_name: String::new(),
            input: String::new(),
            suggestions: Vec::new(),
            shape: AskShape::Trust,
            call: None,
            bound: None,
            closed: None,
            agent: None,
            hook: None,
        });
        let option = |label: &str, description: &str| wire::QuestionOption {
            label: label.to_owned(),
            description: description.to_owned(),
            ..Default::default()
        };
        let ask = Ask {
            item_key: ask_item::key(&key),
            key,
            body: Some(wire::ask::Body::Question(wire::QuestionAsk {
                questions: vec![wire::Question {
                    header: "Folder".to_owned(),
                    question: TRUST_QUESTION.to_owned(),
                    multi_select: false,
                    options: vec![
                        option(TRUST_YES, "Claude can read, edit and run files here"),
                        option(TRUST_NO, "Claude exits without starting"),
                    ],
                    allow_other: false,
                    secret: false,
                }],
                provider_dialog: true,
            })),
            opened_at_ms: self.shared.now_ms(),
        };
        self.shared.open_ask(ask.clone());
        self.emit_ask_item(emit, &ask, None);
    }

    /// A tool server asks the person to fill in a form or open a link,
    /// while one of its calls runs. Claude shows nothing while its hook
    /// waits, so the ask is answered through amux; its own item records
    /// it, as headless Claude's does.
    fn elicitation(&mut self, emit: &mut Emit, elicitation: &Elicitation) {
        let server = elicitation.mcp_server_name.clone();
        let message = elicitation.message.clone();
        let (body, shape) = match &elicitation.url {
            Some(url) if elicitation.mode.as_deref() == Some("url") => (
                wire::ask::Body::Link(wire::LinkAsk {
                    server,
                    message,
                    url: url.clone(),
                }),
                AnswerShape::Link,
            ),
            _ => (
                wire::ask::Body::Form(wire::FormAsk {
                    server,
                    message,
                    schema_json: elicitation
                        .requested_schema
                        .as_ref()
                        .map(|schema| schema.to_string().into_bytes())
                        .unwrap_or_default(),
                }),
                AnswerShape::Form,
            ),
        };
        // A tool server asks while one of its calls runs; that call's
        // result ends the dialog. A call only a PreToolUse hook announced
        // has its row on the way: the ask's item waits for that row, so it
        // lands below the call and the rows written before it.
        let landed = self
            .tools
            .iter()
            .filter(|(_, tool)| !tool.finished && !tool.awaiting_notification)
            .max_by_key(|(_, tool)| (!tool.server.is_empty(), tool.seq))
            .map(|(id, _)| id.clone());
        let announced = self.running.last().map(|running| running.id.clone());
        let awaits_row = landed.is_none() && announced.is_some();
        let call = landed.or(announced);
        let key = self.new_ask(AskMeta {
            seq: 0,
            tool_name: String::new(),
            input: String::new(),
            suggestions: Vec::new(),
            shape: AskShape::Answer(shape),
            call,
            bound: None,
            closed: None,
            agent: None,
            hook: hook_number(&elicitation.extensions),
        });
        let ask = Ask {
            item_key: if awaits_row {
                String::new()
            } else {
                ask_item::key(&key)
            },
            key,
            body: Some(body),
            opened_at_ms: self.shared.now_ms(),
        };
        self.shared.open_ask(ask.clone());
        if !awaits_row {
            self.emit_ask_item(emit, &ask, None);
        }
    }

    /// Calls their hooks opened: Claude's own row of a plan, or of a call
    /// waiting on the person, may come only once it is decided, and it must
    /// show while it waits. Hooks arrive ahead of the transcript, so such a
    /// call waits for the row of the prompt that began its turn, to read
    /// below it.
    fn open_hook_rows(&mut self, emit: &mut Emit) {
        if !self.shared.awaiting_reflection().is_empty() {
            return;
        }
        for call in std::mem::take(&mut self.hook_rows) {
            self.tool_seen(emit, &call.id, &call.name, &call.input, None, "");
        }
    }

    /// A call its PreToolUse hook announced: an ask Claude raises next is
    /// raised for it.
    fn announce(&mut self, call: &PreToolUse, agent: Option<&str>) {
        self.announced.push(Announced {
            id: call.tool_use_id.clone(),
            name: call.tool_name.clone(),
            input: call.tool_input.to_string(),
            agent: agent.map(str::to_owned),
        });
    }

    /// Lines of the reply Claude is drawing. The first batch of a reply
    /// opens its item; the transcript row the reply becomes takes it over.
    /// Claude draws replies in the order it writes their rows, so a reply
    /// whose row came first is one already drawn.
    fn display(&mut self, emit: &mut Emit, display: &MessageDisplay) {
        // Hooks and rows travel apart: the reply's last batch can come after
        // the row that ended its turn.
        if self.between_turns {
            return;
        }
        self.shared.turn_started();
        if let Some(stream) = self
            .streams
            .iter()
            .find(|stream| stream.message_id == display.message_id)
        {
            if !stream.landed && !display.delta.is_empty() {
                let key = stream.key.clone();
                self.shared.append(emit, &key, &display.delta);
            }
            return;
        }
        let landed = self.unstreamed > 0;
        self.unstreamed = self.unstreamed.saturating_sub(1);
        let key = format!("stream:{}", display.message_id);
        self.streams.push(Stream {
            message_id: display.message_id.clone(),
            key: key.clone(),
            landed,
        });
        if !landed {
            self.shared.item(
                emit,
                ItemDraft {
                    key,
                    text: display.delta.clone(),
                    body: item_body(Kind::Message(wire::Text { complete: false })),
                    ..Default::default()
                },
            );
        }
    }

    /// The key a reply's text row is drawn under: the stream Claude drew
    /// it in, when there was one.
    fn streamed_key(&mut self, uuid: &str) -> String {
        match self.streams.iter_mut().find(|stream| !stream.landed) {
            Some(stream) => {
                stream.landed = true;
                stream.key.clone()
            }
            None => {
                self.unstreamed += 1;
                uuid.to_owned()
            }
        }
    }

    /// The turn is over: a reply drawn without a row (the turn was cut
    /// short) ends as it was drawn.
    fn finish_streams(&mut self, emit: &mut Emit) {
        for stream in std::mem::take(&mut self.streams) {
            if stream.landed {
                continue;
            }
            if let Some(open) = self.shared.open_item(&stream.key).cloned() {
                self.shared.item(
                    emit,
                    ItemDraft {
                        key: stream.key,
                        text: open.text,
                        body: item_body(Kind::Message(wire::Text { complete: true })),
                        complete: true,
                        ..Default::default()
                    },
                );
            }
        }
        self.unstreamed = 0;
    }

    /// A call starts. Its row follows within a second or so, and by
    /// seconds while a background task runs; until then the activity line
    /// names it from here. The model is working, whether or not the row
    /// of the prompt that began the turn landed yet.
    fn pre_tool_use(&mut self, call: &PreToolUse) {
        self.between_turns = false;
        self.shared.turn_started();
        self.announce(call, None);
        let (server, name) = split_tool_name(&call.tool_name);
        // Drawn elsewhere, as its subagent's work, or not at all.
        if is_status_tool(&server, &name)
            || (server.is_empty() && (TASK_TOOLS.contains(&name.as_str()) || name == AGENT_TOOL))
            || plan_mode_write(self.provider.permission_mode.as_deref(), &server, &name)
        {
            return;
        }
        self.running.push(Running {
            id: call.tool_use_id.clone(),
            name,
            since_ms: self.shared.now_ms(),
        });
    }

    /// A call's PostToolUse hook or its result row: it no longer runs.
    fn call_ended(&mut self, id: &str) {
        self.running.retain(|running| running.id != id);
        self.announced.retain(|call| call.id != id);
    }

    /// A call's PostToolUse hook says how long it ran: it began running
    /// that long before now, after any wait for the person's approval,
    /// which its row's time includes. Claude writes the call's result row
    /// after the hook, and that row carries the time; only a result that
    /// came first is drawn again for it.
    fn ran_for(&mut self, emit: &mut Emit, id: &str, duration_ms: u64) {
        let started = self.shared.now_ms() - i64::try_from(duration_ms).unwrap_or(i64::MAX);
        match self.tools.get_mut(id) {
            Some(tool) => {
                tool.started_at_ms = Some(started.max(tool.at_ms));
                if tool.finished {
                    self.emit_tool(emit, id);
                }
            }
            None => {
                self.ran_from.insert(id.to_owned(), started);
            }
        }
    }

    // --- tools -----------------------------------------------------------

    /// A call's tool_use row: the call opens, running.
    fn tool_seen(
        &mut self,
        emit: &mut Emit,
        id: &str,
        name: &str,
        input: &Value,
        at_ms: Option<i64>,
        message_id: &str,
    ) {
        if id.is_empty() {
            return;
        }
        let (server, tool_name) = split_tool_name(name);
        if !self.tools.contains_key(id) {
            let hidden = is_status_tool(&server, &tool_name)
                || (server.is_empty() && TASK_TOOLS.contains(&tool_name.as_str()))
                || plan_mode_write(
                    self.provider.permission_mode.as_deref(),
                    &server,
                    &tool_name,
                );
            let plan = (server.is_empty() && tool_name == PLAN_TOOL).then(Verdict::default);
            let question = server.is_empty() && tool_name == QUESTION_TOOL;
            if is_status_tool(&server, &tool_name)
                && let Some(working_on) = status_working_on(input.to_string().as_bytes())
            {
                self.shared.set_working_on(working_on);
            }
            let seq = self.next_seq();
            let class = tool_class(&server, &tool_name);
            let subagent = (server.is_empty() && tool_name == AGENT_TOOL).then(Subagent::default);
            self.tools.insert(
                id.to_owned(),
                Tool {
                    seq,
                    at_ms: at_ms.unwrap_or_else(|| self.shared.now_ms()),
                    name: tool_name,
                    server,
                    input: input.to_string(),
                    state: ToolState::Running as i32,
                    outcome_text: String::new(),
                    outcome_json: String::new(),
                    class: class as i32,
                    background: BackgroundInput::deserialize(input)
                        .is_ok_and(|input| input.run_in_background),
                    decision: None,
                    ended_at_ms: None,
                    started_at_ms: self
                        .ran_from
                        .remove(id)
                        .map(|started| started.max(at_ms.unwrap_or_else(|| self.shared.now_ms()))),
                    message_id: message_id.to_owned(),
                    finished: false,
                    hidden,
                    subagent,
                    awaiting_notification: false,
                    images: Vec::new(),
                    attached: Vec::new(),
                    plan,
                    question,
                    asked: None,
                    emitted: Vec::new(),
                },
            );
        } else if let Some(tool) = self.tools.get_mut(id)
            && tool.message_id.is_empty()
        {
            // A call its hook opened: its row names the message.
            tool.message_id = message_id.to_owned();
        }
        if let Some(reason) = self.refused.remove(id)
            && let Some(tool) = self.tools.get_mut(id)
        {
            tool.decision = Some(Decision::refused(&reason));
        }
        self.emit_tool(emit, id);
        self.bind_asks_for(emit, id);
        self.emit_dialog_items_for(emit, id);
    }

    /// One tool_result block of a user row; `result` is the row's
    /// structured result of the call.
    fn tool_result(
        &mut self,
        emit: &mut Emit,
        id: &str,
        content: Option<&ToolResultBody>,
        is_error: bool,
        result: Option<&Value>,
        at_ms: Option<i64>,
    ) {
        let id = id.to_owned();
        let output = tool_result_text(content);
        let rejected = is_error && output.starts_with(REJECTED);
        self.call_ended(&id);
        self.close_dialogs_for_tool(emit, &id);
        // The launch metadata of a background subagent is for the model.
        if self.agent_launched(&id, result) {
            self.close_for_tool(emit, &id, DecisionOutcome::Allowed);
            return self.emit_tool(emit, &id);
        }
        // A shell started in the background: Claude lists it by this id at
        // the turn's end.
        if let Some(task) = result.and_then(shell_in_background)
            && let Some(tool) = self.tools.get(&id)
        {
            let (command, at_ms) = (JobInput::command(&tool.input), tool.at_ms);
            self.jobs
                .launched(&task, &id, command, at_ms, wire::BackgroundKind::Shell);
            if let Some(jobs) = self.jobs.current() {
                self.shared.set_jobs(jobs);
            }
        }
        let Some(tool) = self.tools.get_mut(&id) else {
            return;
        };
        let outcome = if rejected {
            DecisionOutcome::Denied
        } else {
            DecisionOutcome::Allowed
        };
        let note = match output.find(REJECTED_NOTE) {
            Some(at) if rejected => output[at + REJECTED_NOTE.len()..].trim().to_owned(),
            _ => String::new(),
        };
        // An ask a fact closed without saying how: the result says.
        if let Some(decision) = &mut tool.decision
            && decision.outcome == DecisionOutcome::Unknown as i32
        {
            *decision = Decision::elsewhere(outcome);
            decision.note = note.clone();
        }
        let denied = tool
            .decision
            .as_ref()
            .is_some_and(|decision| decision.outcome == DecisionOutcome::Denied as i32);
        tool.finished = true;
        tool.state = if rejected
            && (denied
                || self
                    .asks
                    .values()
                    .any(|meta| meta.call.as_deref() == Some(id.as_str())))
        {
            ToolState::Denied as i32
        } else if rejected {
            ToolState::Cancelled as i32
        } else if is_error {
            ToolState::Failed as i32
        } else {
            ToolState::Succeeded as i32
        };
        tool.outcome_text = output.clone();
        // amux's attach tool answers with the element, naming the blob by
        // this host's path: the step carries the attachment instead.
        if is_attach_tool(&tool.server, &tool.name) {
            let (text, attached) = crate::shared::parse_reply(output.clone());
            if !attached.is_empty() {
                tool.outcome_text = text;
                tool.attached = attached;
            }
        }
        let images = tool_result_images(content);
        if !images.is_empty() {
            tool.images = Vec::new();
            for (image, write) in images {
                tool.images.push(image);
                emit.effect(write);
            }
        }
        if let Some(result) = result.filter(|result| !result.is_null() && tool.attached.is_empty())
        {
            tool.outcome_json = compact_json(&without_image_bytes(result));
        }
        if tool.ended_at_ms.is_none() {
            tool.ended_at_ms = at_ms.or(Some(self.shared.now_ms()));
        }
        for key in self.open_ask_keys() {
            let meta = self.asks.get(&key);
            if meta.is_some_and(|meta| meta.agent.is_none())
                && meta.and_then(|meta| meta.call.as_deref()) == Some(id.as_str())
            {
                let mut decision = Decision::elsewhere(outcome);
                decision.note = note.clone();
                self.close(emit, &key, decision);
            }
        }
        // A question answered in Claude's own terminal, or closed by a fact
        // before its answers showed: Claude's own record says how.
        if let Some(tool) = self.tools.get_mut(&id).filter(|tool| tool.question)
            && tool
                .asked
                .as_ref()
                .is_none_or(|closed| closed.outcome == wire::AskOutcome::Dismissed as i32)
        {
            tool.asked = Some(match result.map(AnsweredResult::deserialize) {
                Some(Ok(answered)) if !is_error && !answered.answers.is_empty() => {
                    ask_item::recorded_by_claude(
                        &question_ask_text(&tool.input),
                        &answered.answers,
                        &answered.annotations,
                    )
                }
                _ => ask_item::dismissed(),
            });
        }
        self.task_tool(&id, result.unwrap_or(&Value::Null));
        self.emit_tool(emit, &id);
    }

    fn task_tool(&mut self, id: &str, result: &Value) {
        let Some(tool) = self.tools.get(id) else {
            return;
        };
        if !tool.server.is_empty() {
            return;
        }
        let input = serde_json::from_str::<Value>(&tool.input).unwrap_or(Value::Null);
        let name = tool.name.clone();
        apply_task_tool(&mut self.tasks, &name, &input, result);
    }

    // --- rows ------------------------------------------------------------

    fn row(&mut self, emit: &mut Emit, row: &Row) {
        if self.provider.version.is_none()
            && let Some(version) = row.version().filter(|version| !version.is_empty())
        {
            self.provider.version = Some(version.to_owned());
        }
        if self.provider.session.is_none()
            && let Some(session) = row.session_id().filter(|session| !session.is_empty())
        {
            self.provider.session = Some(session.to_owned());
        }
        let at_ms = row.timestamp().and_then(timestamp_ms);
        match row {
            Row::User(user) => {
                self.shared.provider_started();
                self.user_row(emit, user, at_ms);
            }
            Row::Assistant(assistant) => {
                self.shared.provider_started();
                self.assistant_row(emit, assistant, at_ms);
            }
            Row::System(system) => self.system_row(emit, system, at_ms),
            Row::PermissionMode(mode) => {
                let mode = mode.permission_mode.as_str();
                if !mode.is_empty() {
                    self.permission_seen(emit, mode);
                }
            }
            Row::Attachment(row) => {
                if let Attachment::QueuedCommand(queued) = &row.attachment
                    && matches!(queued.command_mode.as_deref(), None | Some("" | "prompt"))
                {
                    let prompt = unwrap_pasted(&message_text(&queued.prompt));
                    self.joined_prompt(emit, row.envelope.uuid.clone(), prompt, at_ms);
                }
            }
            // Everything else is bookkeeping for Claude's own interface:
            // titles, modes, file history, queue operations, reminders.
            _ => {}
        }
    }

    fn user_row(&mut self, emit: &mut Emit, row: &UserRow, at_ms: Option<i64>) {
        let uuid = row.envelope.uuid.clone();
        let whole = unwrap_pasted(&message_text(&row.message.content));
        if !peer_message_bodies(&whole).is_empty() {
            return self.reflected_messages(&whole);
        }
        if row.is_compact_summary == Some(true) {
            self.shared.item(
                emit,
                ItemDraft {
                    key: uuid,
                    text: whole,
                    body: item_body(Kind::CompactSummary(wire::CompactSummary {})),
                    at_ms,
                    complete: true,
                    ..Default::default()
                },
            );
            return;
        }
        if row.is_meta == Some(true) {
            return;
        }
        // Claude's own message to the model, never a person's prompt.
        if row
            .origin
            .as_ref()
            .is_some_and(|origin| origin.kind == "task-notification")
            || row.prompt_source.as_deref() == Some("system")
        {
            return self.task_notification(emit, &whole, at_ms);
        }
        if let MessageContent::Blocks(blocks) = &row.message.content {
            let mut results = false;
            for block in blocks {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    ..
                } = block
                {
                    results = true;
                    self.tool_result(
                        emit,
                        tool_use_id,
                        content.as_ref(),
                        is_error.unwrap_or(false),
                        row.tool_use_result.as_ref(),
                        at_ms,
                    );
                }
            }
            if results {
                return;
            }
        }
        self.user_text(emit, uuid, whole, row.origin.is_some(), at_ms);
    }

    /// The text of a user row that is neither a tool result nor Claude's
    /// own: an interruption, a slash command or its output, or a prompt.
    /// `origin` is whether the row names who sent it.
    fn user_text(
        &mut self,
        emit: &mut Emit,
        uuid: String,
        whole: String,
        origin: bool,
        at_ms: Option<i64>,
    ) {
        let trimmed = whole.trim_start();
        if trimmed.starts_with(INTERRUPTED) {
            return self.interrupted(emit, uuid, at_ms);
        }
        if trimmed.starts_with("<local-command-caveat>") {
            return;
        }
        if trimmed.starts_with("<bash-input>") {
            let command = between(trimmed, "bash-input").unwrap_or_default();
            return self.bang_input(emit, uuid, command, at_ms);
        }
        if trimmed.starts_with("<bash-stdout>") || trimmed.starts_with("<bash-stderr>") {
            let stdout =
                without_terminal_codes(between(trimmed, "bash-stdout").unwrap_or_default());
            let stderr =
                without_terminal_codes(between(trimmed, "bash-stderr").unwrap_or_default());
            return self.bang_output(emit, stdout, stderr, at_ms);
        }
        if trimmed.starts_with("<command-name>") {
            let command = between(trimmed, "command-name").unwrap_or_default();
            let args = between(trimmed, "command-args").unwrap_or_default();
            let same = self
                .slash
                .as_ref()
                .is_some_and(|slash| slash.command == command && slash.output.is_none());
            if !same {
                self.slash_command(emit, uuid, command.to_owned(), args.to_owned(), at_ms);
            }
            return;
        }
        if trimmed.starts_with("<local-command-stdout>")
            || trimmed.starts_with("<local-command-stderr>")
        {
            let output = without_terminal_codes(
                between(trimmed, "local-command-stdout")
                    .or_else(|| between(trimmed, "local-command-stderr"))
                    .unwrap_or_default(),
            );
            if let Some(slash) = &mut self.slash {
                slash.output = Some(output);
                self.emit_slash(emit);
            }
            self.end_local_turn();
            return;
        }
        // A slash command as typed: Claude records it without a prompt
        // origin before it runs.
        if trimmed.starts_with('/') && !origin {
            let (command, args) = trimmed
                .split_once(char::is_whitespace)
                .unwrap_or((trimmed, ""));
            return self.slash_command(
                emit,
                uuid,
                command.to_owned(),
                args.trim().to_owned(),
                at_ms,
            );
        }
        self.prompt_row(emit, uuid, whole, at_ms);
    }

    fn slash_command(
        &mut self,
        emit: &mut Emit,
        key: String,
        command: String,
        args: String,
        at_ms: Option<i64>,
    ) {
        let input_id = if self.shared.awaiting_reflection().is_empty() {
            Vec::new()
        } else {
            // The command amux typed: it runs as a turn of its own until
            // its output row.
            self.local_turn = true;
            self.shared.turn_started();
            self.shared.reflect_prompt().unwrap_or_default()
        };
        self.slash = Some(Slash {
            key,
            command,
            args,
            input_id,
            at_ms: at_ms.unwrap_or_else(|| self.shared.now_ms()),
            output: None,
        });
        self.emit_slash(emit);
    }

    /// A `!command` typed in Claude's own view (or through amux): Claude
    /// runs it in a shell with no model turn and records it as user rows in
    /// its own markup. It is a shell step.
    fn bang_input(&mut self, emit: &mut Emit, key: String, command: &str, at_ms: Option<i64>) {
        // One typed through amux leaves the prompts awaiting reflection.
        let oldest = self.shared.awaiting_reflection().first().cloned();
        if let Some(oldest) = oldest
            && self
                .submitted
                .iter()
                .any(|prompt| prompt.id == oldest && prompt.text.trim_start().starts_with('!'))
        {
            self.shared.reflect_prompt();
        }
        let input = serde_json::to_value(BangInput { command }).expect("a command serializes");
        self.tool_seen(emit, &key, "Bash", &input, at_ms, "");
        self.bang = Some(key);
    }

    /// The `!command`'s output: its step ends with what it wrote.
    fn bang_output(&mut self, emit: &mut Emit, stdout: String, stderr: String, at_ms: Option<i64>) {
        let Some(key) = self.bang.take() else {
            return;
        };
        let now = self.shared.now_ms();
        let Some(tool) = self.tools.get_mut(&key) else {
            return;
        };
        tool.finished = true;
        tool.state = ToolState::Succeeded as i32;
        tool.outcome_text = match (stdout.is_empty(), stderr.is_empty()) {
            (_, true) => stdout,
            (true, false) => stderr,
            (false, false) => format!("{stdout}\n{stderr}"),
        };
        tool.ended_at_ms = at_ms.or(Some(now));
        self.emit_tool(emit, &key);
    }

    fn end_local_turn(&mut self) {
        if std::mem::take(&mut self.local_turn) {
            self.shared.turn_abandoned();
        }
    }

    /// A person's prompt, typed in the terminal or submitted through amux.
    /// A prompt sent into a turn that had no tool boundary left lands here
    /// too, as a turn of its own after that turn ended: an ordinary prompt.
    fn prompt_row(&mut self, emit: &mut Emit, key: String, text: String, at_ms: Option<i64>) {
        self.between_turns = false;
        // A prompt amux submitted went in with no ask open, so its
        // reflection proves nothing about an ask open now: the row can land
        // after the hook that opened one, since rows are read by polling.
        // Any other prompt means the call under the ask is over.
        if self.shared.awaiting_reflection().is_empty() {
            self.close_all_unknown(emit);
        }
        // The submission began a turn, but another may have run and ended
        // since: a background task's notification answered first.
        self.shared.turn_started();
        let input_id = match self.shared.reflect_prompt() {
            Some(input_id) => input_id,
            None => self
                .steered_entry(&text)
                .map(|entry| entry.input_id)
                .unwrap_or_default(),
        };
        self.local_turn = false;
        // A message from another agent went in labelled; its row is the
        // words alone, as an agent message's row is.
        let text = self
            .submitted
            .iter()
            .find(|prompt| {
                prompt.id == input_id
                    && !prompt.words.is_empty()
                    && prompt.text.trim() == text.trim()
            })
            .map_or(text, |prompt| prompt.words.clone());
        // Attachments go in as their elements: the row holds the
        // attachments at their places, not the elements' text.
        let (text, attachments) = crate::shared::parse_reply(text);
        self.shared.item(
            emit,
            ItemDraft {
                key,
                text,
                attachments,
                input_id,
                body: item_body(Kind::Prompt(wire::Prompt {})),
                at_ms,
                complete: true,
            },
        );
    }

    /// A prompt typed while a turn ran that Claude folded into that turn at
    /// a tool boundary: a queued-command attachment, with no user row. Its
    /// item is the prompt, marked steered; one sent through amux carries
    /// its input id.
    fn joined_prompt(&mut self, emit: &mut Emit, key: String, text: String, at_ms: Option<i64>) {
        let entry = self.steered_entry(&text);
        let (input_id, attachments) = match entry {
            Some(entry) => (entry.input_id, entry.attachments),
            None => (self.folded_prompt(&text).unwrap_or_default(), Vec::new()),
        };
        self.shared.item(
            emit,
            ItemDraft {
                key,
                text,
                attachments,
                input_id,
                body: item_body(Kind::Steer(wire::Steer {})),
                at_ms,
                complete: true,
            },
        );
    }

    /// A prompt amux typed while Claude's own turn ran, as when a
    /// background task's notification began one first: Claude folds it
    /// into that turn instead of reflecting it as a prompt row. It is
    /// reflected here when its text is the oldest awaiting reflection, so
    /// later reflections keep their own input ids.
    fn folded_prompt(&mut self, text: &str) -> Option<Vec<u8>> {
        let oldest = self.shared.awaiting_reflection().first()?;
        let prompt = self.submitted.iter().find(|prompt| &prompt.id == oldest)?;
        if prompt.text.trim() != text.trim() {
            return None;
        }
        self.shared.reflect_prompt()
    }

    /// The steered queue entry a reflection with this text stands for.
    /// Claude takes its own queue in order and echoes no id, so the entry
    /// with the same text wins and the oldest one otherwise.
    fn steered_entry(&mut self, text: &str) -> Option<wire::QueuedInput> {
        let text = text.trim();
        self.shared
            .steer_reflected(|entry| entry.text.trim() == text)
            .or_else(|| self.shared.steer_reflected(|_| true))
    }

    /// Agent messages that arrived on the messaging socket, as Claude
    /// showed them to the model: the consumption point.
    fn reflected_messages(&mut self, text: &str) {
        // Claude answers a message from its socket in a turn of its own.
        self.shared.turn_started();
        for body in peer_message_bodies(text) {
            // The files the message carries were handed over as elements
            // naming this host's copies; as positioned text the body reads
            // as the message was queued.
            let body = attachments::parse(&body).positioned.text;
            let body = body.trim();
            // Claude shows the text it was handed; a body of the words
            // alone, without amux's label, is the same message.
            if let Some(at) = self
                .messages
                .iter()
                .position(|message| message.text.trim() == body)
                .or_else(|| {
                    self.messages
                        .iter()
                        .position(|message| message.words.trim() == body)
                })
            {
                let PendingMessage { id, .. } = self.messages.remove(at);
                self.shared.message_consumed(&id);
            }
        }
    }

    /// A background task reported to the model. For a subagent it names the
    /// Agent call that launched it and carries its answer; the model answers
    /// the notification in a turn of its own.
    fn task_notification(&mut self, emit: &mut Emit, text: &str, at_ms: Option<i64>) {
        self.close_all_unknown(emit);
        self.local_turn = false;
        self.shared.turn_started();
        // The task it reports on is over: it leaves the background list.
        if let Some(task) = between(text, "task-id")
            && let Some(jobs) = self.jobs.ended(task)
        {
            self.shared.set_jobs(jobs);
        }
        let Some(id) = between(text, "tool-use-id") else {
            return;
        };
        let Some(tool) = self.tools.get_mut(id) else {
            return;
        };
        if !tool.awaiting_notification || tool.finished {
            return;
        }
        let result = text
            .find("<result>")
            .and_then(|start| {
                let body = &text[start + "<result>".len()..];
                body.rfind("</result>").map(|end| body[..end].trim())
            })
            .unwrap_or_default();
        tool.finished = true;
        tool.state = match between(text, "status") {
            Some("completed") => ToolState::Succeeded,
            Some("failed") => ToolState::Failed,
            _ => ToolState::Cancelled,
        } as i32;
        tool.outcome_text = result.to_owned();
        tool.ended_at_ms = at_ms.or(Some(self.shared.now_ms()));
        let id = id.to_owned();
        self.emit_tool(emit, &id);
    }

    fn interrupted(&mut self, emit: &mut Emit, key: String, at_ms: Option<i64>) {
        self.running.clear();
        self.close_all_unknown(emit);
        // Claude hands prompts still in its own queue back to its composer.
        self.shared.steers_lost();
        // A subagent in the background outlives the interrupted turn.
        let running = self
            .tools
            .iter()
            .filter(|(_, tool)| !tool.finished && !tool.awaiting_notification)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in running {
            if let Some(tool) = self.tools.get_mut(&id) {
                tool.finished = true;
                tool.state = ToolState::Cancelled as i32;
            }
            self.emit_tool(emit, &id);
        }
        self.shared.item(
            emit,
            ItemDraft {
                key: key.clone(),
                body: item_body(Kind::Interruption(wire::Interruption {})),
                at_ms,
                complete: true,
                ..Default::default()
            },
        );
        self.end_turn(emit, TurnOutcome::Interrupted, at_ms, None);
        self.interruption = Some(key);
    }

    fn end_turn(
        &mut self,
        emit: &mut Emit,
        outcome: TurnOutcome,
        at_ms: Option<i64>,
        duration_ms: Option<i64>,
    ) {
        self.local_turn = false;
        self.running.clear();
        self.finish_streams(emit);
        self.between_turns = true;
        // A subagent's calls may outlive the turn, in the background.
        self.announced.retain(|call| call.agent.is_some());
        let Some(turn) = self.shared.turn_ended(emit, outcome) else {
            return;
        };
        let at_ms = at_ms.unwrap_or_else(|| self.shared.now_ms());
        let started_at_ms = duration_ms.map_or(turn.started_at_ms, |duration| at_ms - duration);
        self.shared.item(
            emit,
            ItemDraft {
                key: format!("turn:{}", turn.id),
                body: item_body(Kind::Turn(Turn {
                    turn_id: turn.id,
                    outcome: outcome as i32,
                    started_at_ms,
                })),
                at_ms: Some(at_ms),
                complete: true,
                ..Default::default()
            },
        );
        // A finished turn's calls and asks cannot change any more, but for
        // subagents still running in the background.
        self.tools
            .retain(|_, tool| tool.awaiting_notification && !tool.finished);
        let tools = &self.tools;
        self.agents.retain(|_, id| tools.contains_key(id));
        let open = self.open_ask_keys();
        self.asks.retain(|key, _| open.contains(key));
    }

    fn assistant_row(&mut self, emit: &mut Emit, row: &AssistantRow, at_ms: Option<i64>) {
        self.local_turn = false;
        // The model is answering, whether or not a prompt row said so.
        self.shared.turn_started();
        let uuid = row.envelope.uuid.clone();
        let message = &row.message;
        let message_id = message.id.clone();
        if !message.model.is_empty() && message.model != "<synthetic>" {
            self.provider.model = Some(message.model.clone());
        }
        if let Some(effort) = row.effort.as_ref().filter(|effort| !effort.is_empty()) {
            self.provider.effort = Some(effort.clone());
        }
        let usage = &message.usage;
        let tokens = usage.input_tokens
            + usage.cache_creation_input_tokens.unwrap_or(0)
            + usage.cache_read_input_tokens.unwrap_or(0);
        if tokens > 0 {
            self.context_tokens = Some(tokens);
        }
        // A row from a later message proves an asked call is over.
        for key in self.open_ask_keys() {
            let later = self
                .asks
                .get(&key)
                .and_then(|meta| meta.call.as_ref().filter(|_| meta.agent.is_none()))
                .and_then(|call| self.tools.get(call))
                .is_some_and(|tool| {
                    !message_id.is_empty()
                        && !tool.message_id.is_empty()
                        && tool.message_id != message_id
                });
            if later {
                self.close(emit, &key, Decision::unknown());
            }
        }
        // The model writes again only once the form's call returned.
        for (key, call) in self.dialog_asks() {
            let later = match call.map(|call| self.tools.get(&call)) {
                Some(Some(tool)) => !message_id.is_empty() && tool.message_id != message_id,
                // The call's own row has not landed yet: this may be it.
                Some(None) => false,
                None => true,
            };
            if later {
                self.close_ask_item(emit, &key, ask_item::dismissed());
            }
        }
        if row.is_api_error_message == Some(true) {
            self.shared.item(
                emit,
                ItemDraft {
                    key: uuid,
                    body: item_body(Kind::ApiError(wire::ApiError {
                        error_kind: row.error.clone().unwrap_or_default(),
                        message: blocks_text(&message.content),
                        ..Default::default()
                    })),
                    at_ms,
                    complete: true,
                    ..Default::default()
                },
            );
            return;
        }
        let blocks = &message.content;
        let several = blocks.len() > 1;
        for (index, block) in blocks.iter().enumerate() {
            let key = if several && index > 0 {
                format!("{uuid}#{index}")
            } else {
                uuid.clone()
            };
            match block {
                ContentBlock::Text { text, .. } => {
                    let key = self.streamed_key(&key);
                    let (text, attachments) = crate::shared::parse_reply(text.clone());
                    self.shared.item(
                        emit,
                        ItemDraft {
                            key: key.clone(),
                            text,
                            attachments,
                            body: item_body(Kind::Message(wire::Text { complete: true })),
                            at_ms,
                            complete: true,
                            ..Default::default()
                        },
                    );
                    self.shared.note_message(&key);
                }
                ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => {
                    self.shared.item(
                        emit,
                        ItemDraft {
                            key,
                            text: match block {
                                ContentBlock::Thinking { thinking, .. } => thinking.clone(),
                                _ => String::new(),
                            },
                            body: item_body(Kind::Thinking(wire::Thinking { complete: true })),
                            at_ms,
                            complete: true,
                            ..Default::default()
                        },
                    )
                }
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => {
                    self.tool_seen(emit, id, name, input, at_ms, &message_id);
                }
                other => self.shared.item(
                    emit,
                    ItemDraft {
                        key,
                        body: item_body(Kind::Unrecognized(wire::Unrecognized {
                            fact_type: format!("assistant/{}", other.kind()),
                            summary: String::new(),
                        })),
                        at_ms,
                        complete: true,
                        ..Default::default()
                    },
                ),
            }
        }
    }

    fn system_row(&mut self, emit: &mut Emit, row: &SystemRow, at_ms: Option<i64>) {
        match row {
            SystemRow::TurnDuration(turn) => {
                if turn.envelope.parent_uuid.is_some()
                    && turn.envelope.parent_uuid == self.interruption
                {
                    return;
                }
                self.close_all_unknown(emit);
                let duration = turn.duration_ms.map(|duration| duration as i64);
                self.end_turn(emit, TurnOutcome::Completed, at_ms, duration);
            }
            SystemRow::CompactBoundary(boundary) => {
                let metadata = &boundary.compact_metadata;
                let after = metadata.post_tokens;
                if after.is_some() {
                    self.context_tokens = after;
                }
                self.shared.item(
                    emit,
                    ItemDraft {
                        key: boundary.envelope.uuid.clone(),
                        body: item_body(Kind::Compaction(wire::Compaction {
                            tokens_before: metadata.pre_tokens,
                            tokens_after: after,
                            automatic: metadata.trigger == CompactTrigger::Auto,
                        })),
                        at_ms,
                        complete: true,
                        ..Default::default()
                    },
                );
            }
            SystemRow::LocalCommand(command) => {
                let whole = unwrap_pasted(&command.content);
                self.user_text(emit, command.envelope.uuid.clone(), whole, false, at_ms);
            }
            SystemRow::StopHookSummary(_) | SystemRow::Unknown(_) => {}
        }
    }
}

/// The hook Claude waits on for this payload's answer, by the number the
/// agent process wrote into it.
fn hook_number(extensions: &serde_json::Map<String, Value>) -> Option<u64> {
    extensions
        .get(crate::HOOK_ANSWER_FIELD)
        .and_then(Value::as_u64)
}

#[cfg(test)]
mod tests {
    use super::unwrap_pasted;

    #[test]
    fn pasted_blocks_read_as_what_was_pasted() {
        assert_eq!(
            unwrap_pasted(
                "\n\n<pasted_content id=\"c9c7\">\nfirst line\n\nsecond line\n</pasted_content id=\"c9c7\">\n"
            ),
            "first line\n\nsecond line"
        );
        assert_eq!(
            unwrap_pasted(
                "look at this\n\n<pasted_content id=\"a1\">\nlog\n</pasted_content id=\"a1\">\nand fix it"
            ),
            "look at this\n\nlog\nand fix it"
        );
        // A tag with no matching close is the person's own text.
        let typed = "<pasted_content id=\"x\">\nhalf";
        assert_eq!(unwrap_pasted(typed), typed);
        let mismatched = "<pasted_content id=\"x\">\nhalf\n</pasted_content id=\"y\">";
        assert_eq!(unwrap_pasted(mismatched), mismatched);
        assert_eq!(unwrap_pasted("\n  plain\n"), "\n  plain\n");
    }
}
