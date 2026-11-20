//! The host's copy of what an ACP agent offers, read from its `initialize`
//! answer alone: asking for more would mean opening a session, which leaves
//! a conversation in the agent's history (Antigravity and OpenCode offer no
//! way to delete one).

use acp_protocol::agent::InitializeResponse;
use acp_protocol::update::AvailableCommand;
use serde::Deserialize;
use wire::{AcpOffer, AcpReopen, AcpSignInMethod, Catalogue, NewAgentDefaults, OfferedModel};

use super::{Reopens, quirks};

/// What the host keeps of an agent that answered `initialize`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostOffer {
    /// What a new agent can pick from before any session: empty but for
    /// what an agent lists without one (Grok's models and commands).
    pub catalogue: Catalogue,
    pub acp: AcpOffer,
    /// The version the agent names, empty where it names none.
    pub version: String,
}

impl HostOffer {
    /// Whether the agent may be signed in: false only where `initialize`
    /// shows it signed out, so an agent whose state only a session would
    /// tell is never called signed out on a guess.
    pub fn signed_in(&self) -> bool {
        self.acp.sign_in != wire::SignInState::SignedOut as i32
    }
}

/// Grok's `initialize` `_meta`: its version and, session-less, its models
/// and commands (`grok-catalogue`).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Meta {
    #[serde(default)]
    agent_version: Option<String>,
    #[serde(default)]
    model_state: Option<ModelState>,
    #[serde(default)]
    available_commands: Option<Vec<AvailableCommand>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelState {
    #[serde(default)]
    current_model_id: Option<String>,
    #[serde(default)]
    available_models: Vec<MetaModel>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetaModel {
    model_id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "_meta", default)]
    meta: Option<MetaModelMeta>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetaModelMeta {
    /// The effort the model runs at when none is chosen.
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    reasoning_efforts: Vec<MetaEffort>,
}

#[derive(Deserialize)]
struct MetaEffort {
    value: String,
}

/// What agent `agent` offers by its `initialize` answer.
pub fn host_offer(agent: &str, init: &InitializeResponse) -> HostOffer {
    let quirks = quirks(agent);
    let caps = init.agent_capabilities.clone().unwrap_or_default();
    let prompt = caps.prompt_capabilities.clone().unwrap_or_default();
    let reopens = Reopens::of(init);
    let reopen = if reopens.resume {
        AcpReopen::Resume
    } else if reopens.load {
        AcpReopen::Load
    } else {
        AcpReopen::None
    };
    let methods: Vec<_> = init.auth_methods.iter().flatten().collect();
    let sign_in = match quirks.signed_in_method {
        Some(method) if methods.iter().any(|offered| offered.id() == Some(method)) => {
            wire::SignInState::SignedIn
        }
        Some(_) => wire::SignInState::SignedOut,
        None => wire::SignInState::Unspecified,
    };
    let meta: Meta = init
        .meta
        .clone()
        .and_then(|meta| serde_json::from_value(meta).ok())
        .unwrap_or_default();
    let agent_info = init.agent_info.clone().flatten();
    let version = agent_info
        .as_ref()
        .map(|info| info.version.clone())
        .or(meta.agent_version.clone())
        .unwrap_or_default();
    let catalogue = if quirks.offers_in_initialize {
        session_less(meta)
    } else {
        Catalogue::default()
    };
    HostOffer {
        catalogue,
        version,
        acp: AcpOffer {
            name: agent_info.map(|info| info.name).unwrap_or_default(),
            reopen: reopen as i32,
            images: prompt.image == Some(true),
            embedded_context: prompt.embedded_context == Some(true),
            sign_in_methods: methods
                .iter()
                .filter_map(|method| {
                    Some(AcpSignInMethod {
                        id: method.id()?.to_owned(),
                        name: method_name(method),
                        description: method.description().unwrap_or_default().to_owned(),
                    })
                })
                .collect(),
            sign_in: sign_in as i32,
        },
    }
}

fn method_name(method: &acp_protocol::session::AuthMethod) -> String {
    use acp_protocol::session::AuthMethod;
    match method {
        AuthMethod::Terminal(method) => method.name.clone(),
        AuthMethod::Agent(method) => method.name.clone(),
        AuthMethod::Unknown(_) => String::new(),
    }
}

/// The models, their efforts and the commands an agent lists in its
/// `initialize` `_meta`, as Grok does (`grok-catalogue`). The efforts are
/// the values its sessions' thought-level option takes (`grok-config`).
fn session_less(meta: Meta) -> Catalogue {
    let state = meta.model_state;
    let current = state
        .as_ref()
        .and_then(|state| state.current_model_id.clone());
    let models: Vec<OfferedModel> = state
        .map(|state| state.available_models)
        .unwrap_or_default()
        .into_iter()
        .map(|model| {
            let meta = model.meta;
            OfferedModel {
                value: model.model_id,
                display_name: model.name,
                description: model.description.unwrap_or_default(),
                efforts: meta
                    .as_ref()
                    .map(|meta| {
                        meta.reasoning_efforts
                            .iter()
                            .map(|effort| effort.value.clone())
                            .collect()
                    })
                    .unwrap_or_default(),
                default_effort: meta.and_then(|meta| meta.reasoning_effort),
                ..Default::default()
            }
        })
        .collect();
    let effort = current.as_ref().and_then(|current| {
        models
            .iter()
            .find(|model| &model.value == current)
            .and_then(|model| model.default_effort.clone())
    });
    Catalogue {
        new_agent: Some(NewAgentDefaults {
            model: current.filter(|current| models.iter().any(|model| &model.value == current)),
            effort,
            permission: None,
        }),
        models,
        commands: meta
            .available_commands
            .unwrap_or_default()
            .into_iter()
            .map(|command| wire::OfferedCommand {
                name: command.name,
                description: command.description,
                argument_hint: command
                    .input
                    .flatten()
                    .map(|input| input.hint)
                    .unwrap_or_default(),
                source: String::new(),
            })
            .collect(),
        ..Default::default()
    }
}
