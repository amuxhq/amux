//! Executable specifications for the Claude protocol surface.
//!
//! A specification is one async function that drives a [`SpecSession`] and states
//! what must hold. The same function runs in two modes: against a live Claude
//! Code process it records the conversation, and against that recording it
//! verifies the claim. Nothing is authored in between, which is what stops a
//! specification describing traffic that was never observed.
//!
//! The division of labour matters. A specification asserts only what survives
//! a re-capture — the acknowledged mode, the completed text, the result
//! subtype. Everything transcript-shaped (token counts, message ids,
//! durations) stays in the recording and is enforced instead by strict replay,
//! which requires every recorded frame to be consumed. Claims stay readable;
//! evidence stays exact.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures_core::Stream;
use replay_support::{SpecEntry, StrictReplay};
use semver::Version;
use tokio::io::{AsyncBufRead, AsyncWrite};

use crate::driver::sdk::init::ContextUsage;
use crate::driver::sdk::{
    CompactBoundaryMessage, ContentBlock, Error, InitializationResult, McpServerStatus, Message,
    MessageContent, PermissionDeniedMessage, PermissionMode, ProcessExit, QueryOptions,
    ResultMessage, SdkEvent, StreamDelta, StreamEvent, TaskNotificationMessage, TaskStartedMessage,
    Usage,
};

pub mod agents;
pub mod channels;
pub mod commands;
pub mod configured;
pub mod control;
pub mod history;
pub mod pty;
pub mod questions;
pub mod results;
pub mod session;
pub mod steering;
pub mod tools;
pub mod work;

/// Specifications reach for Haiku first: shorter turns make smaller, more
/// stable recordings, and nothing below needs a larger model to be true.
pub(crate) const HAIKU: &str = "claude-haiku-4-5-20251001";
pub const MINIMUM_SUPPORTED: &str = "2.1.247";

/// A 32×32 solid red PNG that image scenarios hand to the provider, and that
/// live capture writes into the working directory as `square.png`.
pub const SQUARE_PNG: &[u8] = include_bytes!("../../assets/square.png");
pub const ALLOWED_MODELS: &[&str] = &[HAIKU, "claude-sonnet-5"];

/// How long a draining session must stay silent before it counts as finished.
/// Replay reaches the end of its recording instead and never waits this out.
const QUIET: std::time::Duration = std::time::Duration::from_secs(3);

/// Assert a specification claim. The message states the claim in the words a
/// reader needs, because it is the whole of what a failure reports.
#[macro_export]
macro_rules! expect {
    ($condition:expr, $claim:expr) => {
        assert!($condition, "unmet claim: {}", $claim)
    };
    ($condition:expr, $claim:expr, $($argument:tt)+) => {
        assert!($condition, "unmet claim: {}", format!($claim, $($argument)+))
    };
}

/// How a session opens. Identical in both modes, so a recording cannot be
/// replayed under options that differ from the ones that produced it.
pub struct SessionSetup {
    pub prompt: String,
    pub options: QueryOptions,
    answer_permission: bool,
    questions: Option<QuestionReply>,
    plan_reviews: VecDeque<PlanReview>,
    hook_log: Option<Arc<std::sync::Mutex<Vec<String>>>>,
    elicitation_answer: Option<crate::driver::sdk::ElicitationResult>,
    dialog_result: Option<serde_json::Value>,
    defer_prompt: bool,
    /// The id the opening prompt carries on stdin, when a specification is
    /// about that id.
    prompt_uuid: Option<String>,
    /// A PNG attached to the opening prompt, ahead of its text.
    prompt_image: Option<&'static [u8]>,
    /// Variables added to a live session's inherited environment.
    extra_env: Vec<(&'static str, &'static str)>,
}

/// How the specification answers AskUserQuestion.
#[derive(Clone)]
pub(crate) enum QuestionReply {
    /// Answer every question with the text this returns for it: an option's
    /// label, several labels joined by ", ", or free text.
    Answer(Arc<dyn Fn(&serde_json::Value) -> String + Send + Sync>),
    /// Dismiss the form and stop the turn, as a person pressing Escape does.
    Dismiss(String),
}

#[derive(Clone)]
pub(crate) enum PlanReview {
    ApproveAuto,
    ApproveManual,
    RequestChanges(String),
}

impl SessionSetup {
    /// The common case: one text prompt against a named model.
    pub fn new(model: &str, prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            options: QueryOptions::new(model),
            answer_permission: false,
            questions: None,
            plan_reviews: VecDeque::new(),
            hook_log: None,
            elicitation_answer: None,
            dialog_result: None,
            defer_prompt: false,
            prompt_uuid: None,
            prompt_image: None,
            extra_env: Vec::new(),
        }
    }

    /// A session that stays open for more than one turn.
    ///
    /// A single text prompt closes Claude Code's stdin as soon as it has been
    /// answered, which is right for a one-shot question and fatal for a
    /// conversation: a second message would have nowhere to go. Opening with a
    /// stream instead leaves the input open until the session is closed.
    pub fn conversation(model: &str, first: impl Into<String>) -> Self {
        let mut setup = Self::new(model, first);
        setup.defer_prompt = true;
        setup
    }

    pub(crate) fn allow_permissions(&mut self) {
        self.answer_permission = true;
    }

    pub(crate) fn answer_question(&mut self, answer: impl Into<String>) {
        let answer = answer.into();
        self.answer_questions(move |_| answer.clone());
    }

    pub(crate) fn answer_questions(
        &mut self,
        answer: impl Fn(&serde_json::Value) -> String + Send + Sync + 'static,
    ) {
        self.questions = Some(QuestionReply::Answer(Arc::new(answer)));
    }

    pub(crate) fn dismiss_questions(&mut self, message: impl Into<String>) {
        self.questions = Some(QuestionReply::Dismiss(message.into()));
    }

    pub(crate) fn review_plans(&mut self, reviews: impl IntoIterator<Item = PlanReview>) {
        self.plan_reviews = reviews.into_iter().collect();
    }

    pub(crate) fn answer_hooks(&mut self, log: Option<Arc<std::sync::Mutex<Vec<String>>>>) {
        self.hook_log = Some(log.unwrap_or_default());
    }

    /// Accept a server's form with `content`; `Null` accepts a link form,
    /// which carries no content because the person went to the page.
    pub(crate) fn accept_elicitation(&mut self, content: serde_json::Value) {
        let content = (!content.is_null()).then_some(content);
        self.elicitation_answer = Some(crate::driver::sdk::ElicitationResult::Accept {
            content,
            extensions: Default::default(),
        });
    }

    /// Dismiss a server's form without answering it, as opposed to the
    /// default, which declines it.
    pub(crate) fn cancel_elicitation(&mut self) {
        self.elicitation_answer = Some(crate::driver::sdk::ElicitationResult::Cancel {
            extensions: Default::default(),
        });
    }

    pub(crate) fn complete_dialog(&mut self, result: serde_json::Value) {
        self.dialog_result = Some(result);
    }
}

/// One recorded session's identity and I/O, for replay to hand back.
pub struct RecordedTransport {
    pub session_id: String,
    pub reader: Box<dyn AsyncBufRead + Unpin + Send>,
    pub writer: Box<dyn AsyncWrite + Unpin + Send>,
}

enum Source {
    /// Open real Claude Code processes, applying this to every session's
    /// options so that capture's environment reaches all of them.
    Live(Box<dyn FnMut(&mut QueryOptions) + Send>),
    /// Hand back the recorded sessions in the order they were opened.
    Recorded(VecDeque<RecordedTransport>),
}

/// Where a specification's sessions come from.
///
/// A specification that resumes or forks needs to open a second session, and
/// the two modes differ only here: capture spawns another process, replay hands
/// back the next transport from the recording. Everything else about a session
/// - what it is asked, what it claims - is the specification's alone.
#[derive(Clone)]
pub struct Sessions {
    source: Arc<tokio::sync::Mutex<Source>>,
    /// The identity of every session opened so far, in order. Capture writes
    /// these into the manifest so replay can reuse them.
    opened: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Sessions {
    /// Every session opened so far, in the order the specification opened them.
    pub fn opened(&self) -> Vec<String> {
        self.opened
            .lock()
            .expect("the session ledger is not poisoned")
            .clone()
    }

    /// Open real processes, patching each session's options with the capture
    /// environment: which binary to talk to, where to work, and the sandbox.
    pub fn live(environment: impl FnMut(&mut QueryOptions) + Send + 'static) -> Self {
        Self::from(Source::Live(Box::new(environment)))
    }

    /// Replay the recorded sessions, in the order the recording opened them.
    pub fn recorded(transports: Vec<RecordedTransport>) -> Self {
        Self::from(Source::Recorded(transports.into()))
    }

    fn from(source: Source) -> Self {
        Self {
            source: Arc::new(tokio::sync::Mutex::new(source)),
            opened: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    pub async fn open(&self, mut setup: SessionSetup) -> Result<SpecSession, Error> {
        let session = match &mut *self.source.lock().await {
            Source::Live(environment) => {
                environment(&mut setup.options);
                if !setup.extra_env.is_empty() {
                    let mut env = setup
                        .options
                        .env
                        .take()
                        .unwrap_or_else(|| std::env::vars().collect());
                    env.extend(
                        setup
                            .extra_env
                            .iter()
                            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
                    );
                    setup.options.env = Some(env);
                }
                crate::driver::sdk::spawn(setup.options).await?
            }
            Source::Recorded(transports) => {
                let transport = transports.pop_front().ok_or_else(|| {
                    Error::InvalidOptions(
                        "the specification opened more sessions than the recording holds".into(),
                    )
                })?;
                // Reuse the identity the recording was made under, unless the
                // specification chose one itself: an explicit id or a resumed
                // session is a claim, and silently overwriting it would hide a
                // specification that no longer opens what it recorded.
                if setup.options.session_id.is_none() && setup.options.resume.is_none() {
                    setup.options.session_id = Some(transport.session_id);
                }
                crate::driver::sdk::from_io(transport.reader, transport.writer, setup.options)
                    .await?
            }
        };
        let mut prompt = match setup.prompt_image {
            None => crate::driver::sdk::UserMessage::text(setup.prompt),
            Some(png) => crate::driver::sdk::UserMessage::new(
                crate::driver::sdk::MessageParam {
                    role: crate::driver::sdk::Role::User,
                    content: MessageContent::Blocks(vec![
                        ContentBlock::Image {
                            source: crate::driver::sdk::ImageSource {
                                r#type: crate::driver::sdk::ImageSourceType::Base64,
                                media_type: "image/png".to_owned(),
                                data: base64(png),
                                extensions: Default::default(),
                            },
                            extensions: Default::default(),
                        },
                        ContentBlock::Text {
                            text: setup.prompt,
                            extensions: Default::default(),
                        },
                    ]),
                    extensions: Default::default(),
                },
                None,
            ),
        };
        if let Some(uuid) = setup.prompt_uuid {
            prompt = prompt.with_uuid(uuid);
        }
        if setup.defer_prompt {
            let control = session.control.clone();
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                control.prompt(prompt).await
            });
        } else {
            session.control.prompt(prompt).await?;
        }
        self.opened
            .lock()
            .expect("the session ledger is not poisoned")
            .push(session.control.session_id().to_owned());
        Ok(SpecSession {
            events: session.events,
            control: Some(session.control),
            sessions: self.clone(),
            pending: Vec::new(),
            answer_permission: setup.answer_permission,
            questions: setup.questions,
            plan_reviews: setup.plan_reviews,
            permission_requests: Vec::new(),
            hook_log: setup.hook_log,
            elicitation_answer: setup.elicitation_answer,
            dialog_result: setup.dialog_result,
            dialog_requests: Vec::new(),
            elicitation_requests: Vec::new(),
            exit: None,
        })
    }
}

/// A driven Claude session. The transport behind it is the only thing that
/// differs between recording and replay.
pub struct SpecSession {
    events: crate::driver::sdk::EventStream,
    control: Option<crate::driver::sdk::Control>,
    /// So a specification can open a further session - to resume or fork the
    /// one it is already driving - without knowing which mode it is running in.
    sessions: Sessions,
    /// Messages read by [`SpecSession::advance_to`] that the turn they belong
    /// to has not been given yet.
    pending: Vec<Message>,
    answer_permission: bool,
    questions: Option<QuestionReply>,
    plan_reviews: VecDeque<PlanReview>,
    permission_requests: Vec<(String, serde_json::Value)>,
    hook_log: Option<Arc<std::sync::Mutex<Vec<String>>>>,
    elicitation_answer: Option<crate::driver::sdk::ElicitationResult>,
    dialog_result: Option<serde_json::Value>,
    dialog_requests: Vec<crate::driver::sdk::UserDialogRequest>,
    elicitation_requests: Vec<crate::driver::sdk::ElicitationRequest>,
    exit: Option<ProcessExit>,
}

impl SpecSession {
    /// Open a further session, from the same place this one came from.
    ///
    /// This is how a specification resumes or forks: the session it is already
    /// driving stays alive, and the new one is opened beside it.
    pub async fn open(&self, setup: SessionSetup) -> Result<SpecSession, Error> {
        self.sessions.open(setup).await
    }

    pub fn session_id(&self) -> &str {
        self.control().session_id()
    }

    /// What Claude Code answered the SDK's initialize control with: the model,
    /// the tools, the slash commands, the account. A session cannot open
    /// without it, so it is not optional here.
    pub fn initialization(&self) -> &InitializationResult {
        self.control()
            .initialization_result()
            .expect("an opened session has been initialized")
    }

    pub async fn applied_settings(
        &self,
    ) -> Result<claude_protocol::stream::init::AppliedSettings, Error> {
        self.control().applied_settings().await
    }

    pub async fn context_usage(&self) -> Result<ContextUsage, Error> {
        self.control().get_context_usage().await
    }

    pub async fn mcp_server_status(&self) -> Result<Vec<McpServerStatus>, Error> {
        self.control().mcp_server_status().await
    }

    pub async fn background_tasks(&self, tool_use_id: Option<&str>) -> Result<bool, Error> {
        self.control().background_tasks(tool_use_id).await
    }

    pub async fn stop_task(&self, task_id: &str) -> Result<(), Error> {
        self.control().stop_task(task_id).await
    }

    pub async fn apply_flag_settings(
        &self,
        settings: claude_protocol::stream::FlagSettings,
    ) -> Result<(), Error> {
        self.control().apply_flag_settings(settings).await
    }

    /// Send another user message into a session that is already open.
    pub async fn say(&self, text: &str) -> Result<(), Error> {
        self.control()
            .prompt(crate::driver::sdk::UserMessage::text(text))
            .await
    }

    /// Send a message built by the specification, when its id or queue
    /// priority is part of the claim.
    pub(crate) async fn send(&self, message: crate::driver::sdk::UserMessage) -> Result<(), Error> {
        self.control().prompt(message).await
    }

    pub async fn set_permission_mode(
        &self,
        mode: PermissionMode,
    ) -> Result<Option<PermissionMode>, Error> {
        self.control().set_permission_mode(mode).await
    }

    pub async fn set_model(&self, model: &str) -> Result<(), Error> {
        self.control().set_model(Some(model)).await
    }

    pub async fn interrupt(&self) -> Result<(), Error> {
        self.control().interrupt().await.map(|_| ())
    }

    pub(crate) fn permission_request_count(&self, tool_name: &str) -> usize {
        self.permission_requests
            .iter()
            .filter(|(seen, _)| seen == tool_name)
            .count()
    }

    pub(crate) fn dialog_requests(&self) -> &[crate::driver::sdk::UserDialogRequest] {
        &self.dialog_requests
    }

    /// Read forward until the session has produced a message of this shape,
    /// keeping what was read for whoever takes it.
    ///
    /// This is how a specification says "once the turn is under way" without
    /// saying "after three seconds". A duration would be a guess when
    /// recording and pure waste when replaying, where the answer is already
    /// known.
    pub async fn advance_to(&mut self, kind: &str) {
        let wanted = kind.to_owned();
        self.advance_until(move |message| message.kind() == wanted)
            .await;
    }

    pub(crate) async fn advance_to_permission_request(&mut self, tool_name: &str, count: usize) {
        while self.permission_request_count(tool_name) < count {
            let Some(message) = self.next_message().await else {
                panic!(
                    "the session ended before reaching permission request {count} for {tool_name}"
                );
            };
            self.pending.push(message);
        }
    }

    /// Read the stream to the end of the turn in flight.
    pub async fn turn(&mut self) -> Turn {
        self.advance_until(|message| matches!(message, Message::Result(_)))
            .await;
        self.take()
    }

    /// Read until the session goes quiet, then hand over everything read.
    ///
    /// Work the session delegated can outlive the turn that asked for it, so a
    /// specification about delegation has to be able to say "and then whatever
    /// else this session had to say", rather than stopping at the result and
    /// leaving the rest of the conversation unexamined.
    pub async fn drain(&mut self) -> Turn {
        while let Ok(Some(message)) = tokio::time::timeout(QUIET, self.next_message()).await {
            self.pending.push(message);
        }
        self.take()
    }

    /// Everything read since the last turn was taken.
    pub fn take(&mut self) -> Turn {
        Turn {
            messages: std::mem::take(&mut self.pending),
        }
    }

    async fn advance_until(&mut self, mut reached: impl FnMut(&Message) -> bool) {
        if self.pending.iter().any(&mut reached) {
            return;
        }
        loop {
            let Some(message) = self.next_message().await else {
                panic!("the session ended before reaching what was waited for");
            };
            let done = reached(&message);
            self.pending.push(message);
            if done {
                return;
            }
        }
    }

    async fn next_message(&mut self) -> Option<Message> {
        loop {
            let item = std::future::poll_fn(|cx| Pin::new(&mut self.events).poll_next(cx)).await?;
            match item.expect("the session stream failed mid-turn") {
                SdkEvent::Message(message) => return Some(message),
                SdkEvent::PermissionRequest {
                    id,
                    input,
                    suggestions: _,
                    blocked_path: _,
                    tool_name,
                } => {
                    self.permission_requests
                        .push((tool_name.clone(), input.clone()));
                    let questions = (tool_name == "AskUserQuestion")
                        .then(|| self.questions.clone())
                        .flatten();
                    let result = if let Some(reply) = questions {
                        match reply {
                            QuestionReply::Answer(answer) => {
                                crate::driver::sdk::PermissionResult::Allow {
                                    updated_input: Some(question_input_with_answer(
                                        input, &*answer,
                                    )),
                                    updated_permissions: None,
                                    tool_use_id: None,
                                }
                            }
                            QuestionReply::Dismiss(message) => {
                                crate::driver::sdk::PermissionResult::Deny {
                                    message,
                                    interrupt: Some(true),
                                    tool_use_id: None,
                                }
                            }
                        }
                    } else if tool_name == "ExitPlanMode" && !self.plan_reviews.is_empty() {
                        match self.plan_reviews.pop_front().expect("plan review checked") {
                            PlanReview::ApproveAuto => crate::driver::sdk::PermissionResult::Allow {
                                updated_input: Some(input),
                                updated_permissions: Some(vec![
                                    crate::driver::sdk::PermissionUpdate::SetMode {
                                        mode: PermissionMode::AcceptEdits,
                                        destination:
                                            crate::driver::sdk::PermissionUpdateDestination::Session,
                                    },
                                ]),
                                tool_use_id: None,
                            },
                            // A bare allow leaves plan mode into accept-edits
                            // on the CLI's own initiative, so approving
                            // manually has to say which mode it wants.
                            PlanReview::ApproveManual => crate::driver::sdk::PermissionResult::Allow {
                                updated_input: Some(input),
                                updated_permissions: Some(vec![
                                    crate::driver::sdk::PermissionUpdate::SetMode {
                                        mode: PermissionMode::Default,
                                        destination:
                                            crate::driver::sdk::PermissionUpdateDestination::Session,
                                    },
                                ]),
                                tool_use_id: None,
                            },
                            PlanReview::RequestChanges(message) => {
                                crate::driver::sdk::PermissionResult::Deny {
                                    message,
                                    interrupt: Some(false),
                                    tool_use_id: None,
                                }
                            }
                        }
                    } else if self.answer_permission {
                        crate::driver::sdk::PermissionResult::Allow {
                            updated_input: Some(input),
                            updated_permissions: None,
                            tool_use_id: None,
                        }
                    } else {
                        crate::driver::sdk::PermissionResult::Deny {
                            message: "the specification did not authorize this request".into(),
                            interrupt: Some(false),
                            tool_use_id: None,
                        }
                    };
                    self.control()
                        .answer_permission(id, result)
                        .await
                        .expect("the permission request is answered through Control");
                }
                SdkEvent::HookCallback { id, input, .. } => {
                    if let Some(log) = &self.hook_log {
                        log.lock()
                            .expect("the hook log is not poisoned")
                            .push(hook_event_name(&input));
                    }
                    self.control()
                        .answer_hook(id, allow_hook())
                        .await
                        .expect("the hook request is answered through Control");
                }
                SdkEvent::Elicitation { id, request } => {
                    self.elicitation_requests.push(request);
                    let result = self.elicitation_answer.clone().unwrap_or(
                        crate::driver::sdk::ElicitationResult::Decline {
                            extensions: Default::default(),
                        },
                    );
                    self.control()
                        .answer_elicitation(id, result)
                        .await
                        .expect("the elicitation is answered through Control");
                }
                SdkEvent::UserDialog { id, request } => {
                    self.dialog_requests.push(request);
                    let result = match self.dialog_result.clone() {
                        Some(result) => crate::driver::sdk::UserDialogResult::Completed {
                            result,
                            extensions: Default::default(),
                        },
                        None => crate::driver::sdk::UserDialogResult::Cancelled {
                            extensions: Default::default(),
                        },
                    };
                    self.control()
                        .answer_user_dialog(id, result)
                        .await
                        .expect("the dialog request is answered through Control");
                }
                SdkEvent::Exited(exit) => {
                    self.exit = Some(exit);
                    return None;
                }
            }
        }
    }

    fn control(&self) -> &crate::driver::sdk::Control {
        self.control.as_ref().expect("the session is still open")
    }

    pub async fn close(mut self) -> ProcessExit {
        match self.control.take() {
            Some(control) => control.close().await,
            None => self.exit.unwrap_or(ProcessExit {
                success: true,
                code: Some(0),
                stderr: String::new(),
                termination: crate::driver::sdk::Termination::Exited,
            }),
        }
    }
}

fn question_input_with_answer(
    mut input: serde_json::Value,
    answer: &(dyn Fn(&serde_json::Value) -> String + Send + Sync),
) -> serde_json::Value {
    let answers = input
        .get("questions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|question| {
            let text = question
                .get("question")
                .and_then(serde_json::Value::as_str)?;
            Some((text.to_owned(), serde_json::Value::String(answer(question))))
        })
        .collect::<serde_json::Map<_, _>>();
    input["answers"] = serde_json::Value::Object(answers);
    input
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = chunk.iter().enumerate().fold(0u32, |word, (index, byte)| {
            word | u32::from(*byte) << (16 - 8 * index)
        });
        for index in 0..4 {
            if index <= chunk.len() {
                encoded.push(ALPHABET[(word >> (18 - 6 * index) & 63) as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

fn allow_hook() -> crate::driver::sdk::HookOutput {
    crate::driver::sdk::HookOutput::Sync(crate::driver::sdk::SyncHookOutput {
        r#continue: Some(true),
        suppress_output: None,
        stop_reason: None,
        decision: None,
        system_message: None,
        reason: None,
        hook_specific_output: None,
    })
}

fn hook_event_name(input: &crate::driver::sdk::HookInput) -> String {
    match &input.event {
        crate::driver::sdk::HookEventData::UserPromptSubmit { .. } => "UserPromptSubmit",
        crate::driver::sdk::HookEventData::PreToolUse { .. } => "PreToolUse",
        crate::driver::sdk::HookEventData::PostToolUse { .. } => "PostToolUse",
        crate::driver::sdk::HookEventData::Stop { .. } => "Stop",
        other => return format!("{other:?}"),
    }
    .to_string()
}

/// Everything one turn produced, with accessors for the parts a claim is
/// usually about.
pub struct Turn {
    messages: Vec<Message>,
}

impl Turn {
    /// The completed assistant text, in order, concatenated.
    pub fn text(&self) -> String {
        self.blocks()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Every permission mode the session stated during this turn, in
    /// order: the `system.status` rows that follow a mode change.
    pub fn permission_modes(&self) -> Vec<PermissionMode> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::Status(status) => status.permission_mode.clone(),
                _ => None,
            })
            .collect()
    }

    /// The completed assistant thinking, in order, concatenated.
    pub fn thinking(&self) -> String {
        self.blocks()
            .filter_map(|block| match block {
                ContentBlock::Thinking { thinking, .. } => Some(thinking.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The names of the tools this turn asked to use, in order.
    pub fn tools_used(&self) -> Vec<&str> {
        self.blocks()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The tool calls this turn made, as `(id, name)`. The id is what
    /// everything else about a tool call correlates against.
    pub fn tool_uses(&self) -> Vec<(&str, &str)> {
        self.blocks()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, .. } => Some((id.as_str(), name.as_str())),
                _ => None,
            })
            .collect()
    }

    /// The completion news for delegated tasks, which can arrive after the
    /// turn that spawned them has already ended.
    pub fn task_notifications(&self) -> Vec<&TaskNotificationMessage> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::TaskNotification(task) => Some(task),
                _ => None,
            })
            .collect()
    }

    /// The compaction boundaries this turn crossed, with what was reclaimed.
    pub fn compactions(&self) -> Vec<&CompactBoundaryMessage> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::CompactBoundary(boundary) => Some(boundary),
                _ => None,
            })
            .collect()
    }

    /// Tool uses this turn refused.
    pub fn permission_denials(&self) -> Vec<&PermissionDeniedMessage> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::PermissionDenied(denial) => Some(denial),
                _ => None,
            })
            .collect()
    }

    /// History the session re-stated rather than took anew.
    pub fn replayed(&self) -> usize {
        self.messages
            .iter()
            .filter(|message| matches!(message, Message::UserReplay(_)))
            .count()
    }

    /// The delegated tasks this turn announced.
    pub fn tasks_started(&self) -> Vec<&TaskStartedMessage> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::TaskStarted(task) => Some(task),
                _ => None,
            })
            .collect()
    }

    /// The tool results that came back, as the text they carried. These arrive
    /// as user messages, because a tool result is input to the next assistant
    /// turn however it was produced.
    pub fn tool_results(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::User(user) => Some(&user.message.content),
                _ => None,
            })
            .flat_map(|content| match content {
                MessageContent::Text(text) => vec![text.clone()],
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolResult { content, .. } => Some(match content {
                            Some(crate::driver::sdk::ToolResultBody::Blocks(parts)) => parts
                                .iter()
                                .map(|part| match part {
                                    crate::driver::sdk::ToolResultContent::Text {
                                        text, ..
                                    } => text.clone(),
                                    other => serde_json::to_string(other).unwrap_or_default(),
                                })
                                .collect::<String>(),
                            Some(body) => body.text(),
                            None => String::new(),
                        }),
                        _ => None,
                    })
                    .collect(),
            })
            .collect()
    }

    /// The incremental events this turn streamed, in the order they arrived.
    /// Empty unless the session opted into partial messages.
    pub fn stream_events(&self) -> Vec<&StreamEvent> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::StreamEvent(event) => Some(&event.event),
                _ => None,
            })
            .collect()
    }

    /// The text assembled from the deltas, which must agree with the completed
    /// text: streaming is a view of the same answer, not a second one.
    pub fn streamed_text(&self) -> String {
        self.deltas()
            .filter_map(|delta| match delta {
                StreamDelta::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    pub fn streamed_thinking(&self) -> String {
        self.deltas()
            .filter_map(|delta| match delta {
                StreamDelta::Thinking { thinking, .. } => Some(thinking.as_str()),
                _ => None,
            })
            .collect()
    }

    /// What Claude Code reported when a turn ended badly.
    pub fn errors(&self) -> Vec<&str> {
        match self.result() {
            Some(
                ResultMessage::ErrorDuringExecution(error)
                | ResultMessage::ErrorMaxTurns(error)
                | ResultMessage::ErrorMaxBudgetUsd(error)
                | ResultMessage::ErrorMaxStructuredOutputRetries(error),
            ) => error.errors.iter().map(String::as_str).collect(),
            _ => Vec::new(),
        }
    }

    pub fn usage(&self) -> Option<&Usage> {
        match self.result()? {
            ResultMessage::Success(success) => Some(&success.common.usage),
            ResultMessage::ErrorDuringExecution(error)
            | ResultMessage::ErrorMaxTurns(error)
            | ResultMessage::ErrorMaxBudgetUsd(error)
            | ResultMessage::ErrorMaxStructuredOutputRetries(error) => Some(&error.common.usage),
            ResultMessage::Unknown(_) => None,
        }
    }

    pub fn result(&self) -> Option<&ResultMessage> {
        self.messages.iter().find_map(|message| match message {
            Message::Result(result) => Some(result),
            _ => None,
        })
    }

    pub fn succeeded(&self) -> bool {
        matches!(self.result(), Some(ResultMessage::Success(_)))
    }

    /// True when the turn carried a message of this shape, named by the same
    /// discriminant string the protocol manifest uses.
    pub fn saw(&self, kind: &str) -> bool {
        self.messages.iter().any(|message| message.kind() == kind)
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    fn deltas(&self) -> impl Iterator<Item = &StreamDelta> {
        self.stream_events()
            .into_iter()
            .filter_map(|event| match event {
                StreamEvent::ContentBlockDelta { delta, .. } => Some(delta),
                _ => None,
            })
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn blocks(&self) -> impl Iterator<Item = &ContentBlock> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                Message::Assistant(assistant) => Some(&assistant.message.content),
                _ => None,
            })
            .flatten()
    }
}

/// A specification, and the recording that is its evidence.
pub struct SpecDef {
    /// Capability path, e.g. `control/permission_mode`.
    pub name: &'static str,
    /// The fixture directory holding this specification's recording. One
    /// recording per specification: a specification with none cannot run, and
    /// a recording no specification claims is an orphan.
    pub fixture: &'static str,
    /// Opens the session. Called identically when recording and replaying.
    pub setup: fn() -> SessionSetup,
    /// Drives the session and states the claims.
    pub run: for<'a> fn(&'a mut SpecSession) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
}

static DEFINITIONS: &[&SpecDef] = &[
    &session::TEXT_TURN,
    &session::STREAMED_TURN,
    &session::MULTI_TURN,
    &session::SIGN_IN_PROBLEM,
    &commands::COMPACTED,
    &commands::CLEARED,
    &control::PERMISSION_MODE_AND_MODEL,
    &control::SESSION_INTROSPECTION,
    &control::EFFORT,
    &tools::PERMISSION_CALLBACK,
    &tools::QUESTION_ASKED,
    &tools::PLAN_REVIEWED,
    &questions::EVERY_SHAPE,
    &questions::DISMISSED,
    &tools::ELICITATION_ACCEPTED,
    &tools::ELICITATION_DECLINED,
    &tools::ELICITATION_CANCELLED,
    &tools::ELICITATION_LINK,
    &tools::DIALOG_REQUESTED,
    &tools::HOOK_LIFECYCLE,
    &configured::CONFIGURED_TURN,
    &configured::EFFORTFUL_TURN,
    &agents::SUBAGENT_TASK,
    &history::RESUMED,
    &results::INTERRUPTED,
    &work::IMAGE,
    &work::FAILING_COMMAND,
    &work::TASK_LIST,
    &work::BACKGROUND_SHELL,
    &work::FAILED_TOOL_SERVER,
    &channels::CLIENT_ID,
    &channels::SIDE_CHANNEL,
    &steering::FOLDED,
    &steering::PREEMPTED,
];

const fn entry(name: &'static str, recording: &'static str) -> SpecEntry {
    SpecEntry {
        name,
        recording,
        allowed_models: ALLOWED_MODELS,
    }
}

static SDK_REGISTRY: &[SpecEntry] = &[
    entry("session/text_turn", "text_turn"),
    entry("session/streamed_turn", "streamed_turn"),
    entry("session/multi_turn", "multi_turn"),
    entry("session/sign_in_problem", "sign_in_problem"),
    entry("commands/compacted", "compacted"),
    entry("commands/cleared", "cleared"),
    entry("control/permission_mode_and_model", "controls"),
    entry("control/session_introspection", "introspection"),
    entry("control/effort", "effort"),
    entry("tools/permission_callback", "permission_callback"),
    entry("tools/question_asked", "question_asked"),
    entry("tools/plan_reviewed", "plan_reviewed"),
    entry("tools/question_every_shape", "question_every_shape"),
    entry("tools/question_dismissed", "question_dismissed"),
    entry("tools/elicitation_accepted", "elicitation_accepted"),
    entry("tools/elicitation_declined", "elicitation_declined"),
    entry("tools/elicitation_cancelled", "elicitation_cancelled"),
    entry(
        "tools/elicitation_link_accepted",
        "elicitation_link_accepted",
    ),
    entry("tools/hook_lifecycle", "hook_lifecycle"),
    entry("options/configured_turn", "configured_turn"),
    entry("configured/effortful_turn", "effortful_turn"),
    entry("agents/subagent_task", "subagent_task"),
    entry("history/resumed", "resumed"),
    entry("results/interrupted", "interrupted"),
    entry("work/image", "image"),
    entry("work/failing_command", "failing_command"),
    entry("work/task_list", "task_list"),
    entry("work/background_shell", "background_shell"),
    entry("control/failed_tool_server", "failed_tool_server"),
    entry("probes/client_id", "client_id"),
    entry("probes/side_channel", "side_channel"),
    entry("probes/steer_folded", "steer_folded"),
    entry("probes/steer_preempted", "steer_preempted"),
];

/// The donor's executable SDK specifications in stable reading order.
pub fn sdk_registry() -> &'static [SpecEntry] {
    SDK_REGISTRY
}

pub fn pty_registry() -> &'static [SpecEntry] {
    pty::registry()
}

pub fn fixtures_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sdk")
}

/// Live captures amux's own interpreter drove, which no specification
/// replays: `sdk/` and `pty/` beside the registered corpora.
pub fn live_fixtures_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/live")
}

pub enum SpecSource {
    Live {
        binary: std::path::PathBuf,
        cwd: std::path::PathBuf,
        environment: Option<std::collections::HashMap<String, String>>,
    },
    Recorded {
        replay: StrictReplay,
        transport_order: Vec<String>,
        session_ids: Vec<String>,
    },
}

#[derive(Clone, Debug)]
pub struct RunReport {
    pub provider_version: Option<Version>,
    pub model: String,
    pub session_ids: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("specification {spec} failed: {claim}")]
pub struct SpecFailure {
    pub spec: String,
    pub claim: String,
}

pub async fn run(spec: &SpecEntry, source: SpecSource) -> Result<(), SpecFailure> {
    execute(spec, source).await.map(|_| ())
}

/// Runs one SDK specification and returns capture metadata for `claude-probe`.
#[doc(hidden)]
pub async fn execute(spec: &SpecEntry, source: SpecSource) -> Result<RunReport, SpecFailure> {
    let definition = DEFINITIONS
        .iter()
        .copied()
        .find(|definition| definition.name == spec.name && definition.fixture == spec.recording)
        .ok_or_else(|| failure(spec, "specification is not in the Claude SDK registry"))?;
    let setup = (definition.setup)();
    let model = setup
        .options
        .model
        .clone()
        .ok_or_else(|| failure(spec, "specification does not name a model"))?;
    if !spec.allowed_models.contains(&model.as_str()) {
        return Err(failure(spec, format!("model {model} is not allowed")));
    }

    let (sessions, replay_driver) = match source {
        SpecSource::Live {
            binary,
            cwd,
            environment,
        } => (
            Sessions::live(move |options| {
                options.cli_path = Some(binary.clone());
                options.cwd = Some(cwd.clone());
                options.env = environment.clone();
                if let Some(Some(socket)) = options.extra_args.get("messaging-socket-path") {
                    private_socket_directory(std::path::Path::new(socket));
                }
                let sandbox = serde_json::json!({"enabled": false});
                if let Some(crate::driver::sdk::SettingsConfig::Inline(
                    serde_json::Value::Object(settings),
                )) = &mut options.settings
                {
                    settings.insert("sandbox".to_owned(), sandbox);
                } else {
                    options.settings = Some(crate::driver::sdk::SettingsConfig::Inline(
                        serde_json::json!({"sandbox": sandbox}),
                    ));
                }
                // A capture records Claude, not the machine it ran on: the
                // owner's user settings (plugins, skills, agents, permission
                // rules, default mode) and the tool servers their account
                // connects stay out, unless a specification names a source.
                if options.setting_sources.is_empty() {
                    options.setting_sources = vec![
                        crate::driver::sdk::SettingSource::Project,
                        crate::driver::sdk::SettingSource::Local,
                    ];
                }
                options.strict_mcp_config = true;
            }),
            None,
        ),
        SpecSource::Recorded {
            mut replay,
            transport_order,
            session_ids,
        } => {
            if transport_order.len() != session_ids.len() {
                return Err(failure(
                    spec,
                    format!(
                        "recording has {} transports but {} session ids",
                        transport_order.len(),
                        session_ids.len()
                    ),
                ));
            }
            let mut transports = Vec::with_capacity(transport_order.len());
            for (transport_id, session_id) in transport_order.into_iter().zip(session_ids) {
                let transport = replay.transports.remove(&transport_id).ok_or_else(|| {
                    failure(spec, format!("recording omits transport {transport_id}"))
                })?;
                transports.push(RecordedTransport {
                    session_id,
                    reader: transport.reader,
                    writer: transport.writer,
                });
            }
            if !replay.transports.is_empty() {
                return Err(failure(spec, "recording has undeclared extra transports"));
            }
            let controller = replay.controller;
            let driver = tokio::spawn(async move {
                controller.drive().await;
                // The recording has no more output. Deliver EOF so a drain
                // observes the simulated exit without waiting for live quiet.
                controller
                    .close_reads()
                    .await
                    .expect("close recorded output");
            });
            (Sessions::recorded(transports), Some(driver))
        }
    };

    let ledger = sessions.clone();
    let driven = tokio::spawn(async move {
        let mut session = sessions
            .open(setup)
            .await
            .map_err(|error| error.to_string())?;
        (definition.run)(&mut session).await;
        session.close().await;
        Ok::<(), String>(())
    });
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(300), driven)
        .await
        .map_err(|_| failure(spec, "specification did not finish within 300 seconds"))?
        .map_err(|error| failure(spec, format!("claim task failed: {error}")))?
        .map_err(|claim| failure(spec, claim));
    if let Some(mut driver) = replay_driver
        && tokio::time::timeout(std::time::Duration::from_secs(5), &mut driver)
            .await
            .is_err()
    {
        driver.abort();
    }
    outcome?;

    Ok(RunReport {
        provider_version: None,
        model,
        session_ids: ledger.opened(),
    })
}

/// Claude refuses a messaging socket whose directory another user could
/// reach, so a live session gets a fresh directory only its owner can open.
fn private_socket_directory(socket: &std::path::Path) {
    let Some(directory) = socket.parent() else {
        return;
    };
    std::fs::create_dir_all(directory).expect("create the messaging socket directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .expect("make the messaging socket directory private");
    }
}

fn failure(spec: &SpecEntry, claim: impl ToString) -> SpecFailure {
    SpecFailure {
        spec: spec.name.to_string(),
        claim: claim.to_string(),
    }
}
