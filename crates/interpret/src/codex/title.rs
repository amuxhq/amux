//! A title for a conversation nobody named. Codex's app server titles
//! nothing itself: its own terminal asks a hidden thread for one, and amux
//! does the same, so an agent only ever opened on a phone has one too.

use codex_protocol::client::{
    ConfigReadParams, ThreadIdParams, ThreadStartParams, TurnStartParams,
};
use codex_protocol::items::{TextInput, ThreadItem, UserInput};
use codex_protocol::server::{ServerNotification, ThreadResponse};
use codex_protocol::thread::{ReasoningEffort, SandboxMode};
use codex_protocol::{ClientRequest, RpcError};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{Effect, Emit, Request, State, Titling};

/// The small model Codex's terminal titles with, where the account has it.
const TITLE_MODEL: &str = "gpt-5.6-luna";
const TITLE_MAX_CHARS: u32 = 36;
/// How much of the prompt the title is drawn from.
const PROMPT_MAX_CHARS: usize = 900;

/// Everything a title does not need. Without these the hidden thread would
/// have tools, and would start every MCP server the person configured.
const OFF: &[&str] = &[
    "features.apps",
    "features.code_mode",
    "features.code_mode_only",
    "features.context_management",
    "features.current_time_reminder",
    "features.deferred_executor",
    "features.enable_fanout",
    "features.goals",
    "features.hooks",
    "features.image_generation",
    "features.memories",
    "features.multi_agent",
    "features.multi_agent_v2",
    "features.plugins",
    "features.request_permissions_tool",
    "features.shell_snapshot",
    "features.shell_tool",
    "features.standalone_web_search",
    "features.token_budget",
    "features.tool_suggest",
    "features.unified_exec",
    "features.view_image",
    "cloud.skills.enabled",
    "skills.include_instructions",
    "tools.experimental_request_user_input.enabled",
    "tools.update_plan.enabled",
];

fn instructions() -> String {
    format!(
        "Generate a concise, single-line task title of at most {TITLE_MAX_CHARS} characters and \
         under five words where possible. Start with an imperative verb. Capitalize only the \
         first word unless the user's language, proper nouns, acronyms, or code terms require \
         otherwise. Preserve ticket references exactly. Write in the user's language. Do not use \
         quotes, markdown, or trailing punctuation. Do not answer the request."
    )
}

fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": { "type": "string", "minLength": 1, "maxLength": TITLE_MAX_CHARS },
        },
        "required": ["title"],
        "additionalProperties": false,
    })
}

/// What the hidden thread answers, by the schema it is given.
#[derive(Deserialize)]
struct TitleAnswer {
    title: String,
}

fn title_in(answer: &str) -> Option<String> {
    serde_json::from_str::<TitleAnswer>(answer)
        .ok()
        .map(|answer| answer.title)
}

/// The configuration a hidden thread runs under, given the server's own.
/// Each MCP server is switched off whole: an override with only `enabled`
/// is refused for a server given on the command line, as amux's is.
fn hidden_config(effective: &Value) -> Map<String, Value> {
    let mut config: Map<String, Value> = OFF
        .iter()
        .map(|key| ((*key).to_owned(), Value::Bool(false)))
        .collect();
    config.insert("web_search".to_owned(), "disabled".into());
    let servers = effective["config"]["mcp_servers"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, server)| {
            let mut server: Map<String, Value> = server
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            server.insert("enabled".to_owned(), Value::Bool(false));
            (name.clone(), Value::Object(server))
        })
        .collect();
    config.insert("mcp_servers".to_owned(), Value::Object(servers));
    config
}

impl State {
    /// A person's prompt reached the thread: asks for a title if one is
    /// still wanted and this process has not tried.
    pub(super) fn ask_title(&mut self, emit: &mut Emit, prompt: &str) {
        let prompt = prompt.trim();
        if self.titling != Titling::Unasked
            || !self.shared.wants_title()
            || prompt.is_empty()
            || prompt.starts_with('/')
        {
            return;
        }
        self.titling = Titling::Preparing {
            prompt: prompt.chars().take(PROMPT_MAX_CHARS).collect(),
        };
        self.request(
            emit,
            ClientRequest::ConfigRead(ConfigReadParams {
                include_layers: false,
                cwd: Some(self.cwd.clone()),
                extra: Default::default(),
            }),
            Request::TitleConfig,
        );
    }

    /// Whether the account can use the small model.
    fn title_model(&self) -> Option<String> {
        if self.models.iter().any(|model| model.value == TITLE_MODEL) {
            Some(TITLE_MODEL.to_owned())
        } else {
            self.model.clone()
        }
    }

    pub(super) fn title_config_read(&mut self, emit: &mut Emit, result: Result<Value, RpcError>) {
        let Ok(effective) = result else {
            self.titling = Titling::Over;
            return;
        };
        self.request(
            emit,
            ClientRequest::ThreadStart(ThreadStartParams {
                cwd: Some(self.cwd.clone()),
                model: self.title_model(),
                sandbox: Some(SandboxMode::ReadOnly),
                ephemeral: Some(true),
                config: Some(hidden_config(&effective)),
                ..Default::default()
            }),
            Request::TitleThread,
        );
    }

    pub(super) fn title_thread_started(
        &mut self,
        emit: &mut Emit,
        result: Result<Value, RpcError>,
    ) {
        let thread = result
            .ok()
            .and_then(|result| codex_protocol::result::<ThreadResponse>(&result).ok())
            .map(|response| response.thread.id);
        let (Some(thread), Titling::Preparing { prompt }) = (thread, self.titling.clone()) else {
            self.titling = Titling::Over;
            return;
        };
        let model = self.title_model();
        let small = model.as_deref() == Some(TITLE_MODEL);
        self.titling = Titling::Running {
            thread: thread.clone(),
            answer: None,
        };
        self.request(
            emit,
            ClientRequest::TurnStart(TurnStartParams {
                thread_id: thread,
                input: vec![UserInput::Text(TextInput {
                    text: format!("{}\n\nUser prompt:\n{prompt}", instructions()),
                    text_elements: Some(Vec::new()),
                    extra: Default::default(),
                })],
                model,
                effort: small.then_some(Some(ReasoningEffort::Low)),
                output_schema: Some(Box::new(output_schema())),
                ..Default::default()
            }),
            Request::TitleTurn,
        );
    }

    pub(super) fn title_turn_refused(&mut self, emit: &mut Emit) {
        self.end_title(emit, None);
    }

    /// A notification about the hidden thread, which is nothing to anything
    /// else; true when it was one.
    pub(super) fn title_notified(
        &mut self,
        emit: &mut Emit,
        notification: &ServerNotification,
    ) -> bool {
        let Titling::Running { thread, answer } = &mut self.titling else {
            return false;
        };
        if notification.thread_id() != Some(thread.as_str()) {
            return false;
        }
        match notification {
            ServerNotification::ItemCompleted(completed) => {
                if let ThreadItem::AgentMessage(message) = &completed.item {
                    *answer = Some(message.text.clone());
                }
            }
            ServerNotification::TurnCompleted(_) => {
                let title = answer.as_deref().and_then(title_in);
                self.end_title(emit, title);
            }
            _ => {}
        }
        true
    }

    /// Lets the hidden thread go, and takes its title unless the agent has
    /// come by a name or a title in the meantime.
    fn end_title(&mut self, emit: &mut Emit, title: Option<String>) {
        if let Titling::Running { thread, .. } = std::mem::replace(&mut self.titling, Titling::Over)
        {
            let bytes = self.request_as(
                format!("amux-unsubscribe-{thread}"),
                ClientRequest::ThreadUnsubscribe(ThreadIdParams {
                    thread_id: thread,
                    extra: Default::default(),
                }),
                Request::Unsubscribe,
            );
            emit.effect(Effect::ProviderWrite(bytes));
        }
        if let Some(title) = title.filter(|_| self.shared.wants_title()) {
            self.shared.provider_titled(&title);
            self.name_thread(emit);
        }
    }
}
