//! Codex: one `codex app-server` per agent, spoken to over JSON-RPC in
//! WebSocket text frames on a Unix socket (on Windows, over stdio). The
//! facts are the messages the server writes: notifications about
//! the thread, its turns and their items, requests that ask the person
//! something, and responses to the requests this interpreter sent.
//!
//! The agent process performs the handshake (`initialize`, then
//! `thread/start` or `thread/resume`); the interpreter reads the thread from
//! that response or from `thread/started`, and from then on writes every
//! request and response itself with ids it chooses (`amux-<n>`), so each
//! acknowledgement is matched to what it acknowledges.
//!
//! What the agent offers comes from the server, never from a client
//! catalogue: once the handshake's thread answer arrives, the interpreter
//! asks `model/list` (following `nextCursor` to the last page; hidden
//! models left out) and `skills/list` once per server, and the snapshot
//! carries the models with their reasoning efforts and the skills as
//! commands. A refused or missing answer leaves its list empty.
//!
//! An agent message goes to Codex with `thread/inject_items`. Codex reports
//! nothing about an injected item, so the interpreter writes the
//! agent-message item itself. When the message leaves the pending set
//! depends on whether a running turn drains injected items or parks them
//! for the next turn: [`CODEX_INJECT_CONSUMPTION`].

mod facts;
mod recording;

use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;

use codex_protocol::client::{
    Answer, ElicitationAction, ElicitationResponse, GrantScope, InjectItemsParams, InjectedContent,
    InjectedItem, InjectedMessage, PermissionsApprovalResponse, ThreadIdParams,
    TurnInterruptParams, TurnStartParams, TurnSteerParams, UserInputResponse,
};
use codex_protocol::items::{TextContent, UserInput};
use codex_protocol::server::{
    CommandDecision, FileSystemPermissions, NetworkPermissions, PermissionProfile,
};
use codex_protocol::thread::{
    ApprovalPolicy, ApprovalsReviewer, AskForApproval, CollaborationMode, CollaborationSettings,
    ModeKind, ReasoningEffort, SandboxMode,
};
use codex_protocol::{ClientMessage, ClientRequest, ClientResponse, RequestId, RpcError};
use prost::Message as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{
    AgentSpec, AskClosed, AskItem, BackgroundJob, CodexAnswer, CodexAsk, CodexGrant, CodexItem,
    CodexSnapshot, CodexUsage, ContextMeter, Decision, DecisionOutcome, Envelope, EnvelopeKind,
    FormAction, Input, KindInput, OfferedCommand, OfferedMode, OfferedModel, OfferedPermission,
    QueuedInput, Refusal, Step, TaskList, TaskListEntry, ToolDecision, ToolServerHealth, Work,
    codex_answer, codex_ask, codex_grant, codex_input, codex_item, sender, tool_decision, work,
};

use crate::claude_common::{clip, describe_tasks, or_dash};
use crate::{
    Checkpoint, Effect, Emit, Event, FixtureInput, Interpreter, ItemDraft, ItemView, RedactTarget,
    Shared, SnapshotView, Stepped, agent_message_body, agent_message_key, ask_item, codex_input,
    human, serde_pb, unknown,
};

/// When an agent message injected into Codex counts as consumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InjectConsumption {
    /// A running turn does not see injected items; they wait for the next
    /// turn. At turn end, while messages are parked, the interpreter starts
    /// an empty turn, and the messages leave the pending set at that turn's
    /// acknowledgement.
    ParkedUntilNextTurn,
    /// A running turn samples again with injected items in context before
    /// it ends, so a message injected mid-turn leaves the pending set at the
    /// inject's acknowledgement and no turn is started for it. An inject
    /// acknowledged after its turn ended was recorded into an idle thread,
    /// so it is kicked as an idle inject is.
    DrainedMidTurn,
}

/// Decided by probing codex-cli 0.157.0: a running turn drains items
/// injected into it and answers them under the same turn id. An inject into
/// an idle thread still needs an empty turn to be answered, and is consumed
/// at that turn's acknowledgement under either arm.
pub const CODEX_INJECT_CONSUMPTION: InjectConsumption = InjectConsumption::DrainedMidTurn;

/// Which consumption arm an interpreter type runs.
pub trait Arm {
    const CONSUMPTION: InjectConsumption;
}

/// The arm the probe decided.
pub struct Probed;
impl Arm for Probed {
    const CONSUMPTION: InjectConsumption = CODEX_INJECT_CONSUMPTION;
}

/// The parked arm, whatever the probe decided; for its own goldens.
pub struct Parked;
impl Arm for Parked {
    const CONSUMPTION: InjectConsumption = InjectConsumption::ParkedUntilNextTurn;
}

/// The drained arm, whatever the probe decided; for its own goldens.
pub struct Drained;
impl Arm for Drained {
    const CONSUMPTION: InjectConsumption = InjectConsumption::DrainedMidTurn;
}

/// The Codex interpreter with a fixed consumption arm.
pub struct CodexWith<A: Arm>(PhantomData<A>);

/// The interpreter for kind `codex`.
pub type Codex = CodexWith<Probed>;

/// A request this interpreter sent, awaiting its response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Request {
    /// `turn/start`. Agent messages that leave the pending set at its
    /// acknowledgement ride along: a kick, or the next turn in the parked
    /// arm.
    Turn {
        #[serde(with = "serde_pb::bytes_vec")]
        consumes: Vec<Vec<u8>>,
    },
    /// `turn/steer` for the queued prompt with this input id.
    Steer {
        #[serde(with = "serde_pb::bytes")]
        input_id: Vec<u8>,
    },
    Interrupt,
    Compact,
    Inject {
        #[serde(with = "serde_pb::bytes")]
        envelope_id: Vec<u8>,
        /// A turn was running when it was sent.
        during_turn: bool,
        /// That turn ended before the acknowledgement came.
        turn_over: bool,
    },
    /// A page of `model/list`, with the models earlier pages listed.
    Models {
        page: u32,
        #[serde(with = "serde_pb::msgs")]
        listed: Vec<OfferedModel>,
    },
    /// `thread/name/set`.
    Name,
    /// `thread/read` with turns, to have the thread written to disk.
    Persist,
    /// `skills/list`.
    Skills,
    /// A page of `thread/backgroundTerminals/list`, with the jobs earlier
    /// pages listed.
    Jobs {
        page: u32,
        #[serde(with = "serde_pb::msgs")]
        listed: Vec<BackgroundJob>,
    },
}

/// What answering an open ask needs beyond the wire ask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct AskMeta {
    /// The server's request id, as it sent it.
    id: String,
    method: String,
    /// For approvals: the response each offered decision sends, in the
    /// order of the ask's decisions.
    responses: Vec<String>,
    /// For questions: each question's id and option labels.
    questions: Vec<(String, Vec<String>)>,
    /// When it opened, for the item of an ask that is the work.
    at_ms: i64,
}

/// A unit of work as last emitted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WorkState {
    at_ms: i64,
    #[serde(with = "serde_pb::opt_msg")]
    work: Option<Work>,
    text: String,
    turn: String,
}

/// A text item streaming from Codex: a message, a working note, a plan or
/// reasoning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Streamed {
    Message,
    WorkingNote,
    Plan,
    Reasoning(Vec<String>),
}

/// The plan Codex proposed last, and how it was decided.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Proposed {
    /// Its item's key: Codex's item id.
    key: String,
    turn: String,
    at_ms: i64,
    /// Whole once the item completed; until then the open item's.
    text: String,
    complete: bool,
    verdict: i32,
    note: Option<String>,
}

/// What a plan ask's implement choice sends, as Codex's own app does.
const IMPLEMENT: &str = "Implement the plan.";

/// The key amux gives the plan ask it opens on a plan item.
fn plan_ask_key(item: &str) -> String {
    format!("plan:{item}")
}

/// Overrides the next `turn/start` carries; Codex keeps them for later
/// turns.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Overrides {
    model: Option<String>,
    effort: Option<Option<String>>,
    /// A permission's value; its settings go out.
    permission: Option<String>,
    /// A mode's value: Codex's collaboration mode.
    mode: Option<String>,
}

/// A permission Codex offers: amux's name for one combination of Codex's
/// own settings. Settings that match none read as custom.
pub struct NamedPermission {
    pub value: &'static str,
    pub display_name: &'static str,
    pub approval_policy: &'static str,
    pub sandbox: &'static str,
    /// Who answers approvals: the person, or Codex's reviewer model.
    pub approvals_reviewer: &'static str,
    normal: bool,
    never_asks: bool,
}

const fn named(
    value: &'static str,
    display_name: &'static str,
    approval_policy: &'static str,
    sandbox: &'static str,
    approvals_reviewer: &'static str,
) -> NamedPermission {
    NamedPermission {
        value,
        display_name,
        approval_policy,
        sandbox,
        approvals_reviewer,
        normal: false,
        never_asks: false,
    }
}

/// Codex's permissions, from the least it may do alone to the most.
pub const PERMISSIONS: [NamedPermission; 4] = [
    named("read-only", "Read only", "on-request", "read-only", "user"),
    NamedPermission {
        normal: true,
        ..named(
            "default",
            "Default",
            "on-request",
            "workspace-write",
            "user",
        )
    },
    named(
        "auto",
        "Auto",
        "on-request",
        "workspace-write",
        "auto_review",
    ),
    NamedPermission {
        never_asks: true,
        ..named(
            "full-access",
            "Full access",
            "never",
            "danger-full-access",
            "user",
        )
    },
];

/// Codex's modes: its collaboration modes, value and name.
const MODES: [(&str, &str); 2] = [("default", "Default"), ("plan", "Plan")];

/// The permission a catalogue value names.
pub fn permission(value: &str) -> Option<&'static NamedPermission> {
    PERMISSIONS.iter().find(|named| named.value == value)
}

/// The permission Codex's settings make, if any; a reviewer Codex has not
/// named is the person.
fn permission_of(approval: &str, sandbox: &str, reviewer: Option<&str>) -> Option<&'static str> {
    let reviewer = reviewer.unwrap_or("user");
    PERMISSIONS
        .iter()
        .find(|named| {
            named.approval_policy == approval
                && named.sandbox == sandbox
                && named.approvals_reviewer == reviewer
        })
        .map(|named| named.value)
}

/// What Codex offers on a host with no agent running, from its
/// `model/list` pages and its `skills/list` answer.
pub fn host_catalogue(
    models: &[codex_protocol::server::Model],
    skills: codex_protocol::server::SkillsListResponse,
) -> wire::Catalogue {
    wire::Catalogue {
        models: facts::offered_models(models),
        commands: facts::offered_skills(skills),
        permissions: offered_permissions(),
        modes: offered_modes(),
    }
}

fn offered_permissions() -> Vec<OfferedPermission> {
    PERMISSIONS
        .iter()
        .map(|named| OfferedPermission {
            value: named.value.to_owned(),
            display_name: named.display_name.to_owned(),
            normal: named.normal,
            never_asks: named.never_asks,
            settable: true,
            models: Vec::new(),
        })
        .collect()
}

fn offered_modes() -> Vec<OfferedMode> {
    MODES
        .iter()
        .map(|(value, display_name)| OfferedMode {
            value: (*value).to_owned(),
            display_name: (*display_name).to_owned(),
            normal: *value == "default",
            settable: true,
        })
        .collect()
}

/// Everything the Codex interpreter holds; its checkpoint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    shared: Shared<CodexAsk>,
    consumption: InjectConsumption,
    /// The spec's incarnation: a later one's first thread is a resume.
    incarnation: u32,
    /// The agent's name, which the thread carries.
    name: String,
    /// How many times the thread was named, for the requests' ids.
    names_set: u64,
    thread_id: Option<String>,
    version: Option<String>,
    model: Option<String>,
    launch_model: Option<String>,
    effort: Option<String>,
    approval: Option<String>,
    sandbox: Option<String>,
    /// Who answers approvals, as Codex names it.
    reviewer: Option<String>,
    /// The collaboration mode, as Codex names it.
    collaboration: Option<String>,
    /// What `model/list` and `skills/list` answered, whole: the catalogue.
    #[serde(with = "serde_pb::msgs")]
    models: Vec<OfferedModel>,
    #[serde(with = "serde_pb::msgs")]
    commands: Vec<OfferedCommand>,
    /// This server was asked what it offers.
    offers_asked: bool,
    /// How many times `skills/list` was asked, which names each ask.
    skills_asked: u32,
    /// Codex said its skills changed after the pending `skills/list` was
    /// sent, so its answer may be stale: ask again once it comes.
    skills_stale: bool,
    overrides: Overrides,
    /// The turn Codex reports running.
    active_turn: Option<String>,
    /// The running turn has reported its prompt: a later user message in
    /// it is a steer.
    turn_prompted: bool,
    /// An interrupt asked for before Codex named the turn.
    interrupt_pending: bool,
    /// amux interrupted the running turn: the asks Codex then resolves are
    /// withdrawn, not answered elsewhere.
    interrupted_here: bool,
    requests: BTreeMap<String, Request>,
    next_request: u64,
    next_boundary: u64,
    /// Agent messages accepted while no thread was running.
    #[serde(with = "serde_pb::msgs")]
    held: Vec<Envelope>,
    /// Agent messages injected mid-turn, waiting for the next turn (parked
    /// arm only).
    #[serde(with = "serde_pb::bytes_vec")]
    parked: Vec<Vec<u8>>,
    works: BTreeMap<String, WorkState>,
    /// When each amux send call started, so its completion keeps the time.
    sent: BTreeMap<String, i64>,
    streamed: BTreeMap<String, Streamed>,
    asks: BTreeMap<String, AskMeta>,
    context_tokens: Option<u64>,
    context_window: Option<u64>,
    #[serde(with = "serde_pb::opt_msg")]
    usage: Option<CodexUsage>,
    #[serde(with = "serde_pb::opt_msg")]
    servers: Option<ToolServerHealth>,
    #[serde(with = "serde_pb::opt_msg")]
    sign_in: Option<wire::SignIn>,
    /// Reviewer verdicts on work Codex has not reported yet.
    #[serde(with = "serde_pb::msg_map")]
    reviewed: BTreeMap<String, ToolDecision>,
    /// The whole-turn diff last emitted, by key.
    last_diff: Option<(String, String)>,
    /// The turn whose final error is already an item.
    final_error: Option<String>,
    /// Commands still running after the turn that started them ended.
    background: Option<BTreeSet<String>>,
    /// How many times Codex was asked for its background jobs, which
    /// names each ask.
    jobs_asked: u32,
    plan: Option<Vec<(String, i32)>>,
    /// The plan Codex proposed last.
    proposed: Option<Proposed>,
}

fn item_body(kind: codex_item::Kind) -> Vec<u8> {
    CodexItem { kind: Some(kind) }.encode_to_vec()
}

/// A request id as an ask key: the JSON the server sent.
fn ask_key(id: &codex_protocol::RequestId) -> String {
    serde_json::to_string(id).expect("a request id serializes")
}

impl State {
    /// The kind-neutral state: the agent process reads the pending
    /// agent-message set and quiescence from here.
    pub fn shared(&self) -> &Shared<CodexAsk> {
        &self.shared
    }

    /// The arm this state runs.
    pub fn consumption(&self) -> InjectConsumption {
        self.consumption
    }

    fn new(spec: &AgentSpec, producer_version: &str, consumption: InjectConsumption) -> Self {
        let created_mode = spec
            .config
            .as_ref()
            .and_then(|config| config.mode.clone())
            .filter(|mode| MODES.iter().any(|(value, _)| value == mode));
        Self {
            shared: Shared::new(spec, KIND, producer_version),
            consumption,
            incarnation: spec.incarnation,
            name: spec.name.clone(),
            names_set: 0,
            thread_id: None,
            version: None,
            model: None,
            launch_model: None,
            effort: None,
            approval: None,
            sandbox: None,
            reviewer: None,
            // The mode the agent was created in, which its first turn sets.
            collaboration: created_mode.clone(),
            models: Vec::new(),
            commands: Vec::new(),
            offers_asked: false,
            skills_asked: 0,
            skills_stale: false,
            overrides: Overrides {
                mode: created_mode,
                ..Overrides::default()
            },
            active_turn: None,
            turn_prompted: false,
            interrupt_pending: false,
            interrupted_here: false,
            requests: BTreeMap::new(),
            next_request: 0,
            next_boundary: 0,
            held: Vec::new(),
            parked: Vec::new(),
            works: BTreeMap::new(),
            sent: BTreeMap::new(),
            streamed: BTreeMap::new(),
            asks: BTreeMap::new(),
            context_tokens: None,
            context_window: None,
            usage: None,
            servers: None,
            sign_in: None,
            reviewed: BTreeMap::new(),
            final_error: None,
            last_diff: None,
            background: None,
            jobs_asked: 0,
            plan: None,
            proposed: None,
        }
    }

    fn body(&self) -> Vec<u8> {
        CodexSnapshot {
            asks: self.shared.asks().open_asks().to_vec(),
            context: Some(match self.context_tokens {
                None => unknown::context_meter(),
                Some(used_tokens) => ContextMeter {
                    known: true,
                    used_tokens,
                    window_tokens: self.context_window,
                    breakdown: Vec::new(),
                },
            }),
            model: self.model.clone(),
            // Codex's models are offered by their ids, so the one chosen
            // is the one whose value is the id.
            model_name: crate::shared::model_name(
                self.model.as_deref(),
                None,
                &self.models,
                tidy_model,
            ),
            permission: self
                .approval
                .as_deref()
                .zip(self.sandbox.as_deref())
                .and_then(|(approval, sandbox)| {
                    permission_of(approval, sandbox, self.reviewer.as_deref())
                })
                .map(str::to_owned),
            mode: self
                .collaboration
                .clone()
                .filter(|mode| MODES.iter().any(|(value, _)| value == mode)),
            approval_policy: self.approval.clone(),
            sandbox: self.sandbox.clone(),
            approvals_reviewer: self.reviewer.clone(),
            collaboration_mode: self.collaboration.clone(),
            active_turn: self.active_turn.clone(),
            servers: Some(
                self.servers
                    .clone()
                    .unwrap_or_else(unknown::tool_server_health),
            ),
            usage: Some(self.usage.clone().unwrap_or_else(unknown::codex_usage)),
            sign_in: Some(self.sign_in.clone().unwrap_or_else(unknown::sign_in)),
            background_jobs: Some(self.shared.jobs()),
            plan: Some(match &self.plan {
                None => unknown::task_list(),
                Some(steps) => TaskList {
                    known: true,
                    entries: steps
                        .iter()
                        .enumerate()
                        .map(|(index, (step, status))| TaskListEntry {
                            id: (index + 1).to_string(),
                            subject: step.clone(),
                            status: *status,
                            active_form: String::new(),
                        })
                        .collect(),
                },
            }),
            effort: self.effort.clone(),
            thread_id: self.thread_id.clone(),
        }
        .encode_to_vec()
    }

    // --- writing to Codex ------------------------------------------------

    fn request(&mut self, emit: &mut Emit, message: ClientRequest, request: Request) {
        emit.effect(Effect::ProviderWrite(self.request_bytes(message, request)));
    }

    fn request_bytes(&mut self, message: ClientRequest, request: Request) -> Vec<u8> {
        self.next_request += 1;
        let id = format!("amux-{}", self.next_request);
        self.request_as(id, message, request)
    }

    /// A request under an id of its own choosing, outside the numbered run
    /// that turns and their controls take.
    fn request_as(&mut self, id: String, message: ClientRequest, request: Request) -> Vec<u8> {
        self.requests.insert(id.clone(), request);
        codex_protocol::encode(&ClientMessage::request(RequestId::String(id), message))
    }

    /// Answers the server request an ask key names.
    fn respond(&self, emit: &mut Emit, key: &str, response: Result<ClientResponse, RpcError>) {
        let Ok(id) = serde_json::from_str::<RequestId>(key) else {
            return;
        };
        emit.effect(Effect::ProviderWrite(codex_protocol::encode(
            &ClientMessage::Response {
                id,
                response,
                extra: Default::default(),
            },
        )));
    }

    fn thread(&self) -> String {
        self.thread_id.clone().unwrap_or_default()
    }

    // --- items -----------------------------------------------------------

    fn emit_item(&mut self, emit: &mut Emit, draft: ItemDraft) {
        self.shared.item(emit, draft);
    }

    /// Extends an open item's text with an append; an item not open is
    /// sent whole.
    fn extend(&mut self, emit: &mut Emit, key: &str, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if self.shared.append(emit, key, delta) {
            return;
        }
        if let Some(open) = self.shared.open_item(key).cloned() {
            self.shared.item(
                emit,
                ItemDraft {
                    key: key.to_owned(),
                    text: open.text + delta,
                    attachments: open.attachments,
                    input_id: open.input_id,
                    body: open.body,
                    at_ms: Some(open.at_ms),
                    complete: false,
                },
            );
        }
    }

    fn emit_work(&mut self, emit: &mut Emit, key: &str) {
        let Some(state) = self.works.get(key).cloned() else {
            return;
        };
        let Some(work) = state.work else {
            return;
        };
        let complete = work_complete(&work);
        self.emit_item(
            emit,
            ItemDraft {
                key: key.to_owned(),
                text: state.text,
                body: item_body(codex_item::Kind::Work(work)),
                at_ms: Some(state.at_ms),
                complete,
                ..Default::default()
            },
        );
    }

    /// Writes the item of an ask that is the work, open or closed; asks that
    /// point at a unit of work have none.
    fn emit_ask(&mut self, emit: &mut Emit, ask: &CodexAsk, at_ms: i64, closed: Option<AskClosed>) {
        let Some(item) = work_ask(ask) else {
            return;
        };
        let item = match closed {
            Some(closed) => ask_item::close(item, closed),
            None => item,
        };
        self.emit_item(
            emit,
            ItemDraft {
                key: ask.item_key.clone(),
                body: item_body(codex_item::Kind::Ask(item)),
                at_ms: Some(at_ms),
                complete: true,
                ..Default::default()
            },
        );
    }

    /// Closes an ask the server resolved without saying how: the decision
    /// on its unit of work, or its own item, reads dismissed.
    fn dismiss(&mut self, emit: &mut Emit, ask: &CodexAsk, meta: Option<AskMeta>) {
        self.decide(
            emit,
            &ask.item_key,
            ToolDecision {
                outcome: DecisionOutcome::Unknown as i32,
                ..Default::default()
            },
        );
        if let Some(meta) = meta {
            self.emit_ask(emit, ask, meta.at_ms, Some(ask_item::dismissed()));
        }
    }

    /// Closes an ask another client answered. An approval's decision says
    /// so, and takes its outcome from how the work then goes; an ask that
    /// is the work reads dismissed, as Codex does not say what was answered.
    fn answered_elsewhere(&mut self, emit: &mut Emit, ask: &CodexAsk, meta: Option<AskMeta>) {
        self.decide(
            emit,
            &ask.item_key,
            ToolDecision {
                outcome: DecisionOutcome::Unknown as i32,
                elsewhere: true,
                ..Default::default()
            },
        );
        if let Some(meta) = meta {
            self.emit_ask(emit, ask, meta.at_ms, Some(ask_item::dismissed()));
        }
    }

    /// Puts a decision on the work item an ask pointed at.
    fn decide(&mut self, emit: &mut Emit, item_key: &str, decision: ToolDecision) {
        let Some(work) = self
            .works
            .get_mut(item_key)
            .and_then(|state| state.work.as_mut())
        else {
            return;
        };
        work.decision = Some(decision);
        self.emit_work(emit, item_key);
    }

    /// Cancels the running turn; one still starting is cancelled as soon
    /// as Codex names it.
    fn interrupt(&mut self, emit: &mut Emit) {
        if self.shared.is_busy() {
            self.interrupted_here = true;
        }
        match self.active_turn.clone() {
            Some(turn) => self.request(
                emit,
                ClientRequest::TurnInterrupt(TurnInterruptParams {
                    thread_id: self.thread(),
                    turn_id: turn,
                    extra: Default::default(),
                }),
                Request::Interrupt,
            ),
            None if self.shared.is_busy() => self.interrupt_pending = true,
            None => {}
        }
    }

    fn boundary(&mut self, emit: &mut Emit, kind: wire::BoundaryKind, exit: wire::IncarnationEnd) {
        self.next_boundary += 1;
        self.emit_item(
            emit,
            ItemDraft {
                key: format!("boundary:{}", self.next_boundary),
                body: item_body(codex_item::Kind::Boundary(wire::Boundary {
                    kind: kind as i32,
                    provider_session: self.thread_id.clone().unwrap_or_default(),
                    provider_version: self.version.clone().unwrap_or_default(),
                    cause: exit.cause,
                    cause_detail: exit.detail,
                    keymap: String::new(),
                })),
                complete: true,
                ..Default::default()
            },
        );
    }

    // --- inputs ----------------------------------------------------------

    fn input(&mut self, emit: &mut Emit, input: Input) {
        let id = input.input_id.clone();
        // Another kind's input, or an arm a newer client sends that this
        // version cannot read.
        let arm = match input.decode_body() {
            Some(KindInput::Codex(wire::CodexInput { of: Some(arm) })) => arm,
            _ => return self.shared.reject(emit, &id, Refusal::Unsupported),
        };
        match arm {
            codex_input::Of::Prompt(prompt) => {
                if let Some(key) = self.plan_ask() {
                    self.shared.close_ask(&key);
                    self.decide_plan(emit, wire::PlanVerdict::Dismissed, None);
                }
                if let Some(entry) = self.shared.admit_prompt(emit, &id, prompt, human()) {
                    self.submit(emit, entry, Vec::new());
                }
            }
            codex_input::Of::Withdraw(withdraw) => {
                self.shared.withdraw(emit, &id, &withdraw.queued_input_id)
            }
            codex_input::Of::SendNow(send) => {
                let turn = self.active_turn.clone();
                if let Some(entry) =
                    self.shared
                        .send_now(emit, &id, &send.queued_input_id, turn.is_some())
                    && let Some(turn) = turn
                {
                    self.steer(emit, &turn, entry);
                }
            }
            codex_input::Of::Interrupt(_) => {
                self.interrupt(emit);
                self.shared.accept(emit, &id, false);
            }
            codex_input::Of::Permission(set) => {
                let Some(named) = permission(&set.value) else {
                    return self.shared.reject(emit, &id, Refusal::Unsupported);
                };
                self.approval = Some(named.approval_policy.to_owned());
                self.sandbox = Some(named.sandbox.to_owned());
                self.reviewer = Some(named.approvals_reviewer.to_owned());
                self.overrides.permission = Some(set.value);
                self.shared.accept(emit, &id, false);
            }
            codex_input::Of::Mode(set) => {
                if !MODES.iter().any(|(value, _)| *value == set.value) {
                    return self.shared.reject(emit, &id, Refusal::Unsupported);
                }
                self.collaboration = Some(set.value.clone());
                self.overrides.mode = Some(set.value);
                self.left_plan(emit);
                self.shared.accept(emit, &id, false);
            }
            codex_input::Of::Model(model) => {
                let model = model.model.or_else(|| self.launch_model.clone());
                self.model = model.clone();
                self.overrides.model = model;
                self.shared.accept(emit, &id, false);
            }
            codex_input::Of::Effort(effort) => {
                self.effort = effort.effort.clone();
                self.overrides.effort = Some(effort.effort);
                self.shared.accept(emit, &id, false);
            }
            codex_input::Of::Answer(answer) => self.answer(emit, &id, answer),
        }
    }

    /// The daemon's word that the agent was renamed: the thread takes the
    /// name now, or when the handshake names the thread.
    fn rename(&mut self, emit: &mut Emit, name: String) {
        self.name = name;
        if self.thread_id.is_some() {
            self.name_thread(emit);
        }
    }

    /// Starts a turn for a prompt. Its item is written now, keyed by its
    /// input id; Codex's reflection of it is recognised and left out.
    fn submit(&mut self, emit: &mut Emit, entry: QueuedInput, mut consumes: Vec<Vec<u8>>) {
        let key = prompt_key(&entry.input_id);
        if entry.text.trim() == "/compact" {
            // Codex compacts on request rather than from a message; the
            // compaction runs as a turn of its own and reflects nothing.
            self.shared.reflect_prompt();
            self.request(
                emit,
                ClientRequest::ThreadCompactStart(ThreadIdParams {
                    thread_id: self.thread(),
                    extra: Default::default(),
                }),
                Request::Compact,
            );
        } else {
            consumes.append(&mut self.parked);
            let mut params = self.turn_params(&entry.text);
            params.client_user_message_id = Some(client_message_id(&entry.input_id));
            let request =
                self.request_bytes(ClientRequest::TurnStart(params), Request::Turn { consumes });
            emit.effect(if entry.attachments.is_empty() {
                Effect::ProviderWrite(request)
            } else {
                Effect::CodexTurnInput {
                    request,
                    attachments: entry.attachments.clone(),
                }
            });
        }
        self.emit_item(
            emit,
            ItemDraft {
                key,
                text: entry.text,
                attachments: entry.attachments,
                input_id: entry.input_id,
                body: item_body(codex_item::Kind::Prompt(wire::Prompt {})),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn turn_params(&mut self, text: &str) -> TurnStartParams {
        let overrides = std::mem::take(&mut self.overrides);
        let named = overrides.permission.as_deref().and_then(permission);
        // A collaboration mode carries the model and effort it runs with:
        // the ones in force, so changing the mode changes nothing else.
        let collaboration_mode = overrides.mode.map(|mode| CollaborationMode {
            mode: ModeKind::parse(&mode),
            settings: CollaborationSettings {
                model: overrides
                    .model
                    .clone()
                    .or_else(|| self.model.clone())
                    .unwrap_or_default(),
                reasoning_effort: self.effort.as_deref().map(ReasoningEffort::parse),
                developer_instructions: None,
                extra: Default::default(),
            },
            extra: Default::default(),
        });
        TurnStartParams {
            thread_id: self.thread(),
            input: if text.is_empty() {
                Vec::new()
            } else {
                vec![UserInput::text(text)]
            },
            model: overrides.model,
            effort: overrides
                .effort
                .map(|effort| effort.as_deref().map(ReasoningEffort::parse)),
            approval_policy: named
                .map(|named| AskForApproval::Named(ApprovalPolicy::parse(named.approval_policy))),
            approvals_reviewer: named
                .map(|named| ApprovalsReviewer::parse(named.approvals_reviewer)),
            sandbox_policy: named.map(|named| SandboxMode::parse(named.sandbox).policy()),
            collaboration_mode,
            ..Default::default()
        }
    }

    /// Delivers a queued prompt into the running turn. Its item waits for
    /// Codex's reflection; a refused steer returns it to the queue.
    fn steer(&mut self, emit: &mut Emit, turn: &str, entry: QueuedInput) {
        let params = TurnSteerParams {
            thread_id: self.thread(),
            expected_turn_id: turn.to_owned(),
            input: vec![UserInput::text(entry.text.clone())],
            client_user_message_id: Some(client_message_id(&entry.input_id)),
            ..Default::default()
        };
        let request = self.request_bytes(
            ClientRequest::TurnSteer(params),
            Request::Steer {
                input_id: entry.input_id,
            },
        );
        emit.effect(if entry.attachments.is_empty() {
            Effect::ProviderWrite(request)
        } else {
            Effect::CodexTurnInput {
                request,
                attachments: entry.attachments,
            }
        });
    }

    /// An agent message: its item now, since Codex reports nothing about an
    /// injected item, then the inject.
    fn agent_message(&mut self, emit: &mut Emit, envelope: Envelope) {
        let envelope = crate::shared::worded(envelope);
        let id = envelope.id.clone();
        self.shared.message_accepted(&id);
        self.emit_item(
            emit,
            ItemDraft {
                key: agent_message_key(&envelope.id),
                text: envelope.text.clone(),
                input_id: envelope.id.clone(),
                body: item_body(codex_item::Kind::AgentMessage(agent_message_body(
                    &envelope,
                ))),
                complete: true,
                ..Default::default()
            },
        );
        if !self.shared.is_started() {
            self.held.push(envelope);
            return self.shared.accept(emit, &id, true);
        }
        self.inject(emit, &envelope);
        self.shared.accept(emit, &id, false);
    }

    fn inject(&mut self, emit: &mut Emit, envelope: &Envelope) {
        let during_turn = self.shared.is_busy();
        self.inject_items(emit, envelope, during_turn);
        if !during_turn {
            // An idle thread records the item and waits; an empty turn
            // answers it under either arm.
            self.kick(emit, vec![envelope.id.clone()]);
        } else if self.consumption == InjectConsumption::ParkedUntilNextTurn {
            self.parked.push(envelope.id.clone());
        }
    }

    /// Records an agent message in the thread; whatever turn runs next
    /// sees it.
    fn inject_items(&mut self, emit: &mut Emit, envelope: &Envelope, during_turn: bool) {
        let message = InjectedMessage {
            role: "user".into(),
            content: vec![InjectedContent::InputText(TextContent {
                text: crate::shared::carried(envelope),
                extra: Default::default(),
            })],
            extra: Default::default(),
        };
        self.request(
            emit,
            ClientRequest::ThreadInjectItems(InjectItemsParams {
                thread_id: self.thread(),
                items: vec![InjectedItem::Message(message)],
                extra: Default::default(),
            }),
            Request::Inject {
                envelope_id: envelope.id.clone(),
                during_turn,
                turn_over: false,
            },
        );
    }

    /// Starts an empty turn; the messages it carries are consumed at its
    /// acknowledgement.
    fn kick(&mut self, emit: &mut Emit, consumes: Vec<Vec<u8>>) {
        self.shared.turn_started();
        let params = self.turn_params("");
        self.request(
            emit,
            ClientRequest::TurnStart(params),
            Request::Turn { consumes },
        );
    }

    /// Inject what was held until a thread was running. A prompt queued
    /// for the thread (a spawn's task) carries the held messages in its own
    /// turn, so the model reads its task with them rather than answering
    /// them first in an empty turn of their own.
    fn release_held(&mut self, emit: &mut Emit) {
        let held = std::mem::take(&mut self.held);
        if held.is_empty() {
            return;
        }
        // A compaction runs as a turn that carries no input.
        let compacts = self
            .shared
            .queue()
            .entries()
            .iter()
            .find(|entry| !entry.steer)
            .is_some_and(|entry| entry.text.trim() == "/compact");
        let Some(entry) = (!compacts).then(|| self.shared.next_queued()).flatten() else {
            for envelope in &held {
                self.inject(emit, envelope);
            }
            return;
        };
        for envelope in &held {
            self.inject_items(emit, envelope, false);
        }
        let consumes = held.into_iter().map(|envelope| envelope.id).collect();
        self.submit(emit, entry, consumes);
    }

    /// At turn end in the parked arm: parked messages need a turn. A queued
    /// prompt's turn carries them; else an empty one is started.
    fn turn_over_parked(&mut self, emit: &mut Emit) {
        if self.parked.is_empty() || !self.shared.queue().is_empty() {
            return;
        }
        let parked = std::mem::take(&mut self.parked);
        self.kick(emit, parked);
    }

    fn answer(&mut self, emit: &mut Emit, id: &[u8], answer: CodexAnswer) {
        let key = answer.ask_key;
        let Some(ask) = self.shared.asks().get(&key).cloned() else {
            return self.shared.reject(emit, id, Refusal::ClosedAsk);
        };
        let parsed = answer.of;
        if let Some(codex_ask::Body::Plan(_)) = ask.body {
            return match parsed {
                Some(codex_answer::Of::Plan(plan)) => self.answer_plan(emit, id, &key, plan),
                _ => self.shared.reject(emit, id, Refusal::Unsupported),
            };
        }
        if let Some(codex_answer::Of::Approve(decision)) = parsed {
            return self.approve(emit, id, &ask, decision);
        }
        if let (Some(codex_ask::Body::Question(asked)), Some(codex_answer::Of::Reply(reply))) =
            (&ask.body, &parsed)
        {
            return self.reply_instead(emit, id, &ask, asked, reply);
        }
        let Some((meta, parsed)) = self.asks.get(&key).cloned().zip(parsed) else {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        };
        let Some((response, closed)) = codex_answer_response(&ask, &meta, parsed) else {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        };
        self.shared.answer(emit, id, &key);
        self.asks.remove(&key);
        self.respond(emit, &meta.id, Ok(response));
        self.emit_ask(emit, &ask, meta.at_ms, Some(closed));
        self.shared.accept(emit, id, false);
    }

    /// An approval: only a decision the request offered is sent, as the
    /// response the request paired with it.
    fn approve(&mut self, emit: &mut Emit, id: &[u8], ask: &CodexAsk, decision: i32) {
        let key = ask.key.clone();
        let Some(meta) = self.asks.get(&key).cloned() else {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        };
        let Some(response) = ask
            .decisions
            .iter()
            .position(|offered| *offered == decision)
            .and_then(|at| meta.responses.get(at))
            .and_then(|response| serde_json::from_str::<ClientResponse>(response).ok())
        else {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        };
        self.shared.answer(emit, id, &key);
        self.asks.remove(&key);
        let decision = Decision::try_from(decision).unwrap_or_default();
        let granted = granted(decision, &response);
        self.respond(emit, &meta.id, Ok(response));
        let outcome = match decision {
            Decision::Approve
            | Decision::ApproveSession
            | Decision::ApproveSimilar
            | Decision::ApproveNetwork => DecisionOutcome::Allowed,
            Decision::Deny | Decision::Abort => DecisionOutcome::Denied,
            Decision::Unspecified => DecisionOutcome::Unknown,
        };
        self.decide(
            emit,
            &ask.item_key,
            ToolDecision {
                outcome: outcome as i32,
                note: String::new(),
                elsewhere: false,
                granted: granted.map(tool_decision::Granted::Codex),
            },
        );
        self.shared.accept(emit, id, false);
    }
}

impl State {
    /// The open plan ask's key, if one is open.
    fn plan_ask(&self) -> Option<String> {
        let key = plan_ask_key(&self.proposed.as_ref()?.key);
        self.shared.asks().get(&key).is_some().then_some(key)
    }

    /// Records how the last plan was decided and sends its item again.
    fn decide_plan(&mut self, emit: &mut Emit, verdict: wire::PlanVerdict, note: Option<String>) {
        let Some(proposed) = &mut self.proposed else {
            return;
        };
        proposed.verdict = verdict as i32;
        proposed.note = note;
        self.emit_plan(emit);
    }

    pub(super) fn emit_plan(&mut self, emit: &mut Emit) {
        let Some(proposed) = self.proposed.clone() else {
            return;
        };
        self.emit_item(
            emit,
            ItemDraft {
                key: proposed.key,
                text: proposed.text,
                body: item_body(codex_item::Kind::Plan(wire::Plan {
                    verdict: proposed.verdict,
                    note: proposed.note,
                    complete: proposed.complete,
                })),
                at_ms: Some(proposed.at_ms),
                complete: proposed.complete,
                ..Default::default()
            },
        );
    }

    /// A turn in plan mode ended with a plan: Codex asks nothing, so amux
    /// opens the decision itself.
    pub(super) fn open_plan_ask(&mut self, turn: &str) {
        let Some(proposed) = &self.proposed else {
            return;
        };
        if proposed.turn != turn
            || !proposed.complete
            || proposed.verdict != wire::PlanVerdict::Undecided as i32
            || self.collaboration.as_deref() != Some("plan")
        {
            return;
        }
        self.shared.open_ask(CodexAsk {
            key: plan_ask_key(&proposed.key),
            item_key: proposed.key.clone(),
            body: Some(codex_ask::Body::Plan(wire::PlanAsk {
                choices: vec![
                    wire::PlanChoice::Start as i32,
                    wire::PlanChoice::KeepPlanning as i32,
                ],
            })),
            decisions: Vec::new(),
        });
    }

    /// The agent left plan, whoever moved it: the waiting plan was
    /// approved.
    pub(super) fn left_plan(&mut self, emit: &mut Emit) {
        if self.collaboration.as_deref() == Some("plan") {
            return;
        }
        if let Some(key) = self.plan_ask() {
            self.shared.close_ask(&key);
            self.decide_plan(emit, wire::PlanVerdict::Approved, None);
        }
    }

    /// A turn started with the plan waiting, whoever started it: the mode
    /// it runs in says how the plan was decided.
    pub(super) fn plan_overtaken(&mut self, emit: &mut Emit) {
        if let Some(key) = self.plan_ask() {
            self.shared.close_ask(&key);
            let verdict = if self.collaboration.as_deref() == Some("plan") {
                wire::PlanVerdict::SentBack
            } else {
                wire::PlanVerdict::Approved
            };
            self.decide_plan(emit, verdict, None);
        }
    }

    /// The person replied in their own words instead of answering: the turn
    /// is interrupted, which withdraws the question, and the words go as
    /// the next prompt.
    fn reply_instead(
        &mut self,
        emit: &mut Emit,
        id: &[u8],
        ask: &CodexAsk,
        asked: &wire::QuestionAsk,
        reply: &wire::ReplyInstead,
    ) {
        let fits =
            reply.answers_so_far.is_empty() || reply.answers_so_far.len() == asked.questions.len();
        let Some(closed) =
            ask_item::replied(asked, reply).filter(|_| fits && !reply.text.trim().is_empty())
        else {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        };
        if self.shared.answer(emit, id, &ask.key).is_none() {
            return;
        }
        let meta = self.asks.remove(&ask.key);
        self.emit_ask(emit, ask, meta.map_or(0, |meta| meta.at_ms), Some(closed));
        self.interrupt(emit);
        let prompt = wire::PromptInput {
            text: reply.text.clone(),
            ..Default::default()
        };
        if let Some(entry) = self.shared.admit_prompt(emit, id, prompt, human()) {
            self.submit(emit, entry, Vec::new());
        }
    }

    /// Implement leaves plan for the default mode, keeping the permission,
    /// and starts a turn with Codex's own words; stay sends the note, if
    /// any, as the next prompt, still in plan mode.
    fn answer_plan(&mut self, emit: &mut Emit, id: &[u8], key: &str, plan: wire::PlanAnswer) {
        let (verdict, prompt) = match plan.choice() {
            wire::PlanChoice::Start => (wire::PlanVerdict::Approved, Some(IMPLEMENT.to_owned())),
            wire::PlanChoice::KeepPlanning => (
                wire::PlanVerdict::SentBack,
                plan.note.clone().filter(|note| !note.trim().is_empty()),
            ),
            _ => return self.shared.reject(emit, id, Refusal::Unsupported),
        };
        if self.shared.answer(emit, id, key).is_none() {
            return;
        }
        if verdict == wire::PlanVerdict::Approved {
            self.collaboration = Some("default".to_owned());
            self.overrides.mode = Some("default".to_owned());
        }
        let note = (verdict == wire::PlanVerdict::SentBack)
            .then_some(plan.note)
            .flatten();
        self.decide_plan(emit, verdict, note);
        let Some(text) = prompt else {
            return self.shared.accept(emit, id, false);
        };
        let prompt = wire::PromptInput {
            text,
            ..Default::default()
        };
        if let Some(entry) = self.shared.admit_prompt(emit, id, prompt, human()) {
            self.submit(emit, entry, Vec::new());
        }
    }
}

/// The text the model sees for an agent message.
/// The client message id a prompt or steer goes to Codex with: its input
/// id, which Codex echoes on the user message it reports, so the echo is
/// told from another client's prompt by id rather than by order.
fn client_message_id(input_id: &[u8]) -> String {
    serde_pb::to_hex(input_id)
}

fn prompt_key(input_id: &[u8]) -> String {
    format!("prompt:{}", serde_pb::to_hex(input_id))
}

fn work_complete(work: &Work) -> bool {
    !matches!(
        wire::ToolState::try_from(work.state).unwrap_or_default(),
        wire::ToolState::Pending | wire::ToolState::Running | wire::ToolState::Unspecified
    )
}

/// How Codex marks a note among a question's answers.
pub(crate) const USER_NOTE: &str = "user_note: ";

/// The response an answer sends, and how the ask's own item closes; None
/// when the answer does not fit the ask.
fn codex_answer_response(
    ask: &CodexAsk,
    meta: &AskMeta,
    answer: codex_answer::Of,
) -> Option<(ClientResponse, AskClosed)> {
    match (ask.body.as_ref()?, answer) {
        (codex_ask::Body::Question(asked), codex_answer::Of::Question(answer)) => {
            if answer.answers.len() != meta.questions.len() {
                return None;
            }
            let mut answers = std::collections::BTreeMap::new();
            for ((question, labels), response) in meta.questions.iter().zip(&answer.answers) {
                // A skipped question goes with no answers, and a note is
                // appended to its question's answers, as Codex's own form
                // sends them.
                let mut picked = Vec::new();
                for index in &response.selected {
                    picked.push(labels.get(*index as usize)?.clone());
                }
                picked.extend(response.other.clone());
                if let Some(note) = response
                    .note
                    .as_deref()
                    .filter(|note| !note.trim().is_empty())
                {
                    picked.push(format!("{USER_NOTE}{note}"));
                }
                answers.insert(
                    question.clone(),
                    Answer {
                        answers: picked,
                        extra: Default::default(),
                    },
                );
            }
            Some((
                ClientResponse::UserInput(UserInputResponse {
                    answers,
                    extra: Default::default(),
                }),
                ask_item::answered(asked, &answer)?,
            ))
        }
        (codex_ask::Body::McpForm(asked), codex_answer::Of::Form(form)) => {
            let action = form_action(form.action)?;
            let content = (action == ElicitationAction::Accept).then(|| {
                serde_json::from_slice(&form.content_json)
                    .unwrap_or_else(|_| Value::Object(Default::default()))
            });
            Some((
                elicitation_response(action, content, None),
                ask_item::form_sent(&form, &asked.schema_json),
            ))
        }
        (codex_ask::Body::McpLink(_), codex_answer::Of::Link(link)) => Some((
            elicitation_response(form_action(link.action)?, None, None),
            ask_item::link_answered(link.action),
        )),
        (codex_ask::Body::Access(asked), codex_answer::Of::Grant(grant)) => {
            let subset = |granted: &[String], asked: &[String]| {
                granted.iter().all(|path| asked.contains(path))
            };
            if !subset(&grant.read, &asked.read)
                || !subset(&grant.write, &asked.write)
                || (grant.network && !asked.network)
            {
                return None;
            }
            let files = (!grant.read.is_empty() || !grant.write.is_empty()).then(|| {
                Some(FileSystemPermissions {
                    read: Some(grant.read.clone()),
                    write: Some(grant.write.clone()),
                    extra: Default::default(),
                })
            });
            let network = grant.network.then(|| {
                Some(NetworkPermissions {
                    enabled: Some(true),
                    extra: Default::default(),
                })
            });
            let scope = if grant.for_session {
                GrantScope::Session
            } else {
                GrantScope::Turn
            };
            Some((
                ClientResponse::PermissionsApproval(PermissionsApprovalResponse {
                    permissions: PermissionProfile {
                        file_system: files,
                        network,
                        extra: Default::default(),
                    },
                    scope: Some(scope),
                    extra: Default::default(),
                }),
                ask_item::granted(&grant),
            ))
        }
        _ => None,
    }
}

/// The item an ask that is the work opens with; None for an approval,
/// whose decision lands on the unit of work it points at.
fn work_ask(ask: &CodexAsk) -> Option<AskItem> {
    let asked = match ask.body.as_ref()? {
        codex_ask::Body::Question(question) => wire::ask_item::Ask::Question(question.clone()),
        codex_ask::Body::McpForm(form) => wire::ask_item::Ask::Form(form.clone()),
        codex_ask::Body::McpLink(link) => wire::ask_item::Ask::Link(link.clone()),
        codex_ask::Body::Access(access) => wire::ask_item::Ask::Access(access.clone()),
        codex_ask::Body::Command(_)
        | codex_ask::Body::FileChange(_)
        | codex_ask::Body::McpTool(_)
        | codex_ask::Body::Plan(_) => return None,
    };
    Some(ask_item::opened(asked))
}

fn form_action(action: i32) -> Option<ElicitationAction> {
    Some(match FormAction::try_from(action).ok()? {
        FormAction::Accept => ElicitationAction::Accept,
        FormAction::Decline => ElicitationAction::Decline,
        FormAction::Cancel => ElicitationAction::Cancel,
        FormAction::Unspecified => return None,
    })
}

/// An answer to an MCP server's elicitation.
fn elicitation_response(
    action: ElicitationAction,
    content: Option<Value>,
    persist: Option<&str>,
) -> ClientResponse {
    ClientResponse::Elicitation(ElicitationResponse {
        action,
        content,
        meta: persist.map(|persist| codex_protocol::client::ElicitationResponseMeta {
            persist: Some(persist.to_owned()),
            extra: Default::default(),
        }),
        extra: Default::default(),
    })
}

impl Checkpoint for State {
    fn resume(mut self) -> (Self, Step) {
        let body = self.body();
        let step = self.shared.resume_step(body);
        (self, step)
    }
}

const KIND: wire::Kind = wire::Kind::Codex;

impl<A: Arm> Interpreter for CodexWith<A> {
    type State = State;
    const KIND: wire::Kind = KIND;

    fn initial(spec: &AgentSpec, producer_version: &str) -> (State, Step) {
        let mut state = State::new(spec, producer_version, A::CONSUMPTION);
        let step = state.shared.initial_step(Self::unknown_snapshot());
        (state, step)
    }

    fn reincarnate(mut state: State, spec: &AgentSpec, producer_version: &str) -> (State, Step) {
        state.shared.reincarnate(spec, producer_version);
        state.incarnation = spec.incarnation;
        state.name = spec.name.clone();
        state.resume()
    }

    fn step(state: &mut State, event: Event) -> Stepped {
        let mut emit = Emit::default();
        match event {
            Event::Tick { at_ms } => state.shared.tick(at_ms),
            Event::Fact(fact) => state.fact(&mut emit, fact),
            Event::Input(input) => state.input(&mut emit, input),
            Event::AgentMessage(envelope) => state.agent_message(&mut emit, envelope),
            Event::Rename { name } => state.rename(&mut emit, name),
            Event::ProviderExit { code } => state.exited(&mut emit, crate::provider_exit(code)),
            Event::Exiting(exiting) => state.exited(&mut emit, exiting),
            Event::DaemonLost => state.boundary(
                &mut emit,
                wire::BoundaryKind::DaemonLost,
                wire::IncarnationEnd::default(),
            ),
            Event::StopRequested(wire::StopMode::Abort) => state.interrupt(&mut emit),
            Event::StopRequested(_) => {}
            Event::Git(git) => state.shared.set_git(git),
        }
        if let Some(entry) = state.shared.next_queued() {
            state.submit(&mut emit, entry, Vec::new());
        }
        let body = state.body();
        state.shared.finish(emit, body)
    }

    fn redact(target: RedactTarget) -> RedactTarget {
        crate::redact::redact_kind::<wire::CodexItem, wire::CodexSnapshot, wire::CodexInput, State>(
            target,
        )
    }

    fn pending_messages(state: &Self::State) -> Vec<Vec<u8>> {
        state.shared.pending_messages().iter().cloned().collect()
    }

    fn unknown_snapshot() -> Vec<u8> {
        unknown::codex().encode_to_vec()
    }

    fn describe_item(body: &[u8]) -> ItemView {
        describe_item(body)
    }

    fn describe_snapshot(body: &[u8]) -> SnapshotView {
        describe_snapshot(body)
    }

    fn fixture_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input> {
        codex_input(input_id, input)
    }

    fn recording(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
        recording::read(format, bytes)
    }
}

fn describe_work(work: &Work) -> String {
    let mut text = match &work.of {
        Some(work::Of::Command(command)) => format!(
            "command {} cwd={}{}{}{}",
            clip(&command.command, 60),
            or_dash(&command.cwd),
            command
                .exit_code
                .map_or(String::new(), |code| format!(" exit={code}")),
            if command.background {
                " background"
            } else {
                ""
            },
            match command.output_dropped_bytes {
                0 => String::new(),
                dropped => format!(" dropped={dropped}"),
            }
        ),
        Some(work::Of::FileChange(change)) => format!(
            "file_change [{}]",
            change
                .changes
                .iter()
                .map(|file| format!(
                    "{} {}{}{} patch={}",
                    wire::FileChangeKind::try_from(file.kind)
                        .map_or("?", |kind| kind.as_str_name()),
                    file.path,
                    if file.move_to.is_empty() {
                        String::new()
                    } else {
                        format!(" -> {}", file.move_to)
                    },
                    file.line
                        .map(|line| format!(" line={line}"))
                        .unwrap_or_default(),
                    clip(&file.patch, 40)
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ),
        Some(work::Of::Mcp(call)) => format!(
            "mcp {}·{} args={} result={}{}",
            call.server,
            call.tool,
            clip(&String::from_utf8_lossy(&call.arguments_json), 40),
            clip(&String::from_utf8_lossy(&call.result_json), 40),
            if call.error.is_empty() {
                String::new()
            } else {
                format!(" error={}", Value::String(call.error.clone()))
            }
        ),
        Some(work::Of::WebSearch(search)) => {
            format!("web_search {}", Value::String(search.query.clone()))
        }
        Some(work::Of::Image(image)) => format!(
            "image {} {}",
            if image.generated {
                "generated"
            } else {
                "viewed"
            },
            image.path
        ),
        Some(work::Of::Collab(collab)) => format!(
            "collab {} threads=[{}] prompt={}",
            collab.tool,
            collab.thread_ids.join(","),
            clip(&collab.prompt, 40)
        ),
        None => "none".to_owned(),
    };
    text.push_str(&format!(
        " {}",
        wire::ToolState::try_from(work.state).map_or("?", |state| state.as_str_name())
    ));
    text.push_str(crate::claude_common::describe_class(work.class));
    if let Some(decision) = &work.decision {
        text.push_str(&format!(
            " decision={}{}{}",
            DecisionOutcome::try_from(decision.outcome)
                .map_or("?", |outcome| outcome.as_str_name()),
            crate::claude_common::describe_grant(decision.granted.as_ref()),
            if decision.elsewhere { " elsewhere" } else { "" }
        ));
    }
    if let Some(ended) = work.ended_at_ms {
        text.push_str(&format!(" ended={ended}"));
    }
    text
}

fn describe_item(body: &[u8]) -> ItemView {
    use codex_item::Kind;
    let item = CodexItem::decode(body).unwrap_or_default();
    let (arm, complete, text) = match item.kind {
        None => ("none", true, String::new()),
        Some(Kind::Prompt(_)) => ("prompt", true, String::new()),
        Some(Kind::Message(text)) => ("message", text.complete, String::new()),
        Some(Kind::Plan(plan)) => ("plan", plan.complete, crate::shared::describe_plan(&plan)),
        Some(Kind::WorkingNote(text)) => ("working_note", text.complete, String::new()),
        Some(Kind::Reasoning(reasoning)) => (
            "reasoning",
            reasoning.complete,
            format!(
                "summary=[{}]",
                reasoning
                    .summary
                    .iter()
                    .map(|part| clip(part, 60))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ),
        Some(Kind::Work(work)) => ("work", work_complete(&work), describe_work(&work)),
        Some(Kind::McpStartup(startup)) => (
            "mcp_startup",
            true,
            format!(
                "{} {} {}",
                startup.server,
                wire::ToolServerStatus::try_from(startup.status)
                    .map_or("?", |status| status.as_str_name()),
                Value::String(startup.error)
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
        Some(Kind::Steer(_)) => ("steer", true, String::new()),
        Some(Kind::Boundary(boundary)) => (
            "boundary",
            true,
            format!(
                "{} session={} version={}{}",
                wire::BoundaryKind::try_from(boundary.kind).map_or("?", |kind| kind.as_str_name()),
                or_dash(&boundary.provider_session),
                or_dash(&boundary.provider_version),
                crate::claude_common::describe_cause(&boundary)
            ),
        ),
        Some(Kind::AgentMessage(message)) => (
            "agent_message",
            true,
            crate::shared::describe_agent_message(&message),
        ),
        Some(Kind::Error(error)) => (
            "error",
            true,
            format!(
                "{} {} retry={} {}/{}",
                error.error_kind,
                clip(&error.message, 80),
                error.will_retry,
                error.attempt,
                error.max_attempts,
            ),
        ),
        Some(Kind::Reroute(switch)) => (
            "reroute",
            true,
            format!(
                "{} -> {} {}",
                switch.from,
                switch.to,
                Value::String(switch.reason)
            ),
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
        Some(Kind::TurnDiff(diff)) => (
            "turn_diff",
            true,
            format!("patch={}", clip(&diff.patch, 60)),
        ),
        Some(Kind::Ask(item)) => ("ask", true, ask_item::describe(&item)),
        Some(Kind::Verdict(verdict)) => (
            "verdict",
            true,
            format!(
                "{}:{} risk={} on={} {}",
                wire::ReviewStatus::try_from(verdict.status).map_or("?", |status| status
                    .as_str_name()
                    .trim_start_matches("REVIEW_STATUS_")),
                verdict.status_word,
                or_dash(&verdict.risk),
                or_dash(&verdict.item_key),
                clip(&verdict.rationale, 60)
            ),
        ),
    };
    ItemView {
        arm: arm.into(),
        complete,
        text,
    }
}

/// What an approval allows from now on, read from the answer sent: the
/// rule it carries, or the session-wide choice.
fn granted(decision: Decision, response: &ClientResponse) -> Option<CodexGrant> {
    let of = match response {
        ClientResponse::CommandApproval(answer) => match &answer.decision {
            CommandDecision::AcceptWithExecpolicyAmendment {
                accept_with_execpolicy_amendment: rule,
            } => Some(codex_grant::Of::CommandPrefix(wire::CommandPrefix {
                words: rule.execpolicy_amendment.clone(),
            })),
            CommandDecision::ApplyNetworkPolicyAmendment {
                apply_network_policy_amendment: choice,
            } if choice.network_policy_amendment.action == "allow" => {
                Some(codex_grant::Of::NetworkHosts(wire::NetworkHosts {
                    hosts: vec![choice.network_policy_amendment.host.clone()],
                }))
            }
            _ => None,
        },
        _ => None,
    };
    of.or_else(|| {
        (decision == Decision::ApproveSession).then_some(codex_grant::Of::Session(wire::Empty {}))
    })
    .map(|of| CodexGrant { of: Some(of) })
}

fn describe_ask(ask: &CodexAsk) -> String {
    let body = match &ask.body {
        Some(codex_ask::Body::Command(command)) => format!(
            "command {} reason={}{}{}",
            clip(&command.command, 50),
            Value::String(command.reason.clone()),
            if command.allow_prefix.is_empty() {
                String::new()
            } else {
                format!(" prefix={:?}", command.allow_prefix)
            },
            if command.network_hosts.is_empty() {
                String::new()
            } else {
                format!(" hosts={:?}", command.network_hosts)
            }
        ),
        Some(codex_ask::Body::FileChange(change)) => format!(
            "file_change files={} reason={}{}",
            change.changes.len(),
            Value::String(change.reason.clone()),
            if change.grant_root.is_empty() {
                String::new()
            } else {
                format!(" root={}", change.grant_root)
            }
        ),
        Some(codex_ask::Body::McpTool(tool)) => format!(
            "mcp_tool {}·{} args={}",
            tool.server,
            tool.tool,
            clip(&String::from_utf8_lossy(&tool.arguments_json), 40)
        ),
        Some(codex_ask::Body::McpForm(form)) => format!(
            "form {} {} schema={}",
            form.server,
            clip(&form.message, 50),
            clip(&String::from_utf8_lossy(&form.schema_json), 40)
        ),
        Some(codex_ask::Body::McpLink(link)) => {
            format!(
                "link {} {} {}",
                link.server,
                clip(&link.message, 50),
                link.url
            )
        }
        Some(codex_ask::Body::Access(access)) => format!(
            "access reason={} read={:?} write={:?} network={}{}",
            Value::String(access.reason.clone()),
            access.read,
            access.write,
            access.network,
            if access.network_hosts.is_empty() {
                String::new()
            } else {
                format!(" hosts={:?}", access.network_hosts)
            }
        ),
        Some(codex_ask::Body::Plan(plan)) => crate::shared::describe_plan_ask(plan),
        Some(codex_ask::Body::Question(question)) => format!(
            "question [{}]",
            question
                .questions
                .iter()
                .map(|question| format!(
                    "{}{}{}{} ({})",
                    Value::String(question.question.clone()),
                    if question.multi_select { " multi" } else { "" },
                    if question.allow_other { " other" } else { "" },
                    if question.secret { " secret" } else { "" },
                    question
                        .options
                        .iter()
                        .map(|option| format!(
                            "{}{}",
                            option.label,
                            if option.recommended { "*" } else { "" }
                        ))
                        .collect::<Vec<_>>()
                        .join("|")
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ),
        None => "none".into(),
    };
    format!(
        "{}→{} {} decisions=[{}]",
        ask.key,
        or_dash(&ask.item_key),
        body,
        ask.decisions
            .iter()
            .map(|decision| Decision::try_from(*decision)
                .map_or("?", |decision| decision.as_str_name())
                .trim_start_matches("DECISION_"))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Codex's usage as goldens print it: the overall state, each window by
/// its limit (its length when unrecognised), use and state, and credits.
fn describe_codex_usage(usage: &wire::CodexUsage) -> String {
    if usage.state() == wire::UsageState::Unknown && usage.windows.is_empty() {
        return "?".into();
    }
    let mut text = crate::shared::describe_usage_state(usage.state).to_owned();
    for window in &usage.windows {
        text.push_str(&crate::shared::describe_usage_window(
            window.limit(),
            None,
            || format!("{}m?", window.window_minutes),
            window.meter,
        ));
    }
    if let Some(credits) = &usage.credits {
        text.push_str(&format!(" credits={credits}"));
    }
    text
}

fn describe_snapshot(body: &[u8]) -> SnapshotView {
    let snapshot = CodexSnapshot::decode(body).unwrap_or_default();
    let context = snapshot.context.unwrap_or_default();
    let usage = snapshot.usage.unwrap_or_default();
    let servers = snapshot.servers.unwrap_or_default();
    let sign_in = snapshot.sign_in.unwrap_or_default();
    SnapshotView {
        asks: snapshot
            .asks
            .iter()
            .map(|ask| (ask.key.clone(), ask.item_key.clone()))
            .collect(),
        text: format!(
            "asks=[{}] thread={} turn={} model={} model_name={} effort={} permission={} mode={} approval={} sandbox={} reviewer={} collaboration={} context={} plan={} usage={} servers={} sign_in={} background={}",
            snapshot
                .asks
                .iter()
                .map(describe_ask)
                .collect::<Vec<_>>()
                .join("; "),
            snapshot.thread_id.as_deref().unwrap_or("?"),
            snapshot.active_turn.as_deref().unwrap_or("-"),
            snapshot.model.as_deref().unwrap_or("?"),
            crate::claude_common::describe_model_name(snapshot.model_name.as_deref()),
            snapshot.effort.as_deref().unwrap_or("?"),
            snapshot.permission.as_deref().unwrap_or("custom"),
            snapshot.mode.as_deref().unwrap_or("?"),
            snapshot.approval_policy.as_deref().unwrap_or("?"),
            snapshot.sandbox.as_deref().unwrap_or("?"),
            snapshot.approvals_reviewer.as_deref().unwrap_or("?"),
            snapshot.collaboration_mode.as_deref().unwrap_or("?"),
            if context.known {
                format!(
                    "{}/{}",
                    context.used_tokens,
                    context
                        .window_tokens
                        .map_or("?".to_owned(), |window| window.to_string())
                )
            } else {
                "?".into()
            },
            describe_tasks(&snapshot.plan.unwrap_or_default()),
            describe_codex_usage(&usage),
            match wire::HealthState::try_from(servers.state).unwrap_or_default() {
                wire::HealthState::Unknown => "?".to_owned(),
                state => format!(
                    "{}[{}]",
                    state.as_str_name(),
                    servers
                        .servers
                        .iter()
                        .map(|server| format!(
                            "{}:{}",
                            server.name,
                            wire::ToolServerStatus::try_from(server.status)
                                .map_or("?", |status| status.as_str_name())
                        ))
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            },
            match wire::SignInState::try_from(sign_in.state).unwrap_or_default() {
                wire::SignInState::Unknown => "?".to_owned(),
                state => format!(
                    "{} {}{}",
                    state.as_str_name(),
                    or_dash(&sign_in.account),
                    if sign_in.message.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", clip(&sign_in.message, 60))
                    }
                ),
            },
            crate::shared::describe_jobs(snapshot.background_jobs.as_ref())
        ),
    }
}

/// A Codex model id read as a name when Codex lists no name for it:
/// "gpt-5-codex" reads "GPT-5 Codex", "gpt-5.1-codex-max" reads
/// "GPT-5.1 Codex Max". An id of no recognisable shape stays as it is.
fn tidy_model(id: &str) -> String {
    let parts: Vec<&str> = id.split('-').filter(|part| !part.is_empty()).collect();
    let numeric = |part: &str| part.chars().all(|c| c.is_ascii_digit() || c == '.');
    match parts.as_slice() {
        ["gpt", version, rest @ ..] if numeric(version) => {
            crate::shared::tidy_model_parts(id, vec![format!("GPT-{version}")], rest)
        }
        _ => crate::shared::tidy_model_parts(id, Vec::new(), &parts),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn codex_ids_tidy_into_names() {
        assert_eq!(super::tidy_model("gpt-5-codex"), "GPT-5 Codex");
        assert_eq!(super::tidy_model("gpt-5.1-codex-max"), "GPT-5.1 Codex Max");
        assert_eq!(super::tidy_model("o3"), "o3");
    }
}
