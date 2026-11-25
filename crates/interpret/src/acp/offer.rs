//! The host's copy of what an ACP agent offers, read from its `initialize`
//! answer and, for the models an agent tells only a session, from its own
//! listing command ([`super::ModelListing`]): `agy models` for Antigravity,
//! `opencode models` for OpenCode. A session is never opened just to ask:
//! each would leave something behind in the agent's own folders.

use acp_protocol::agent::InitializeResponse;
use acp_protocol::update::AvailableCommand;
use serde::Deserialize;
use wire::{AcpOffer, AcpReopen, AcpSignInMethod, AgentSettings, Catalogue, OfferedModel};

use super::{ModelListing, Printed, Reopens, quirks};

/// What the host keeps of an agent that answered `initialize`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostOffer {
    /// What a new agent can pick from before any session, by `initialize`
    /// alone: empty but for what an agent lists there (Grok's models and
    /// commands). [`listed_catalogue`] reads what an agent's listing command
    /// prints.
    pub catalogue: Catalogue,
    pub acp: AcpOffer,
    /// The version the agent names, empty where it names none.
    pub version: String,
    /// What an agent started now can do, by this answer.
    pub capabilities: wire::AgentCapabilities,
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
    let catalogue = if quirks.lists_models == ModelListing::Initialize {
        session_less(meta)
    } else {
        Catalogue::default()
    };
    HostOffer {
        catalogue,
        version,
        // What the program does with a conversation it has, which is what
        // an agent of it settled or exited has to lose.
        capabilities: super::capabilities(agent, true, Some(reopens), Some(super::Takes::of(init))),
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
        new_agent: Some(AgentSettings {
            model: current.filter(|current| models.iter().any(|model| &model.value == current)),
            effort,
            ..AgentSettings::default()
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

/// What agent `agent` offers by what its listing command printed
/// ([`ModelListing::Command`]): its models, read as the agent prints them
/// ([`Printed`]), and the modes amux knows the agent has, which are its
/// permissions, at their levels, with none called normal, since which one a
/// session starts in is the person's own setting. A line that is not a
/// model (a warning the program printed) is passed over.
pub fn listed_catalogue(agent: &str, printed: &str) -> Catalogue {
    let models = match quirks(agent).lists_models {
        ModelListing::Command {
            printed: Printed::IdsByEffort,
            ..
        } => by_effort(printed),
        _ => printed
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.contains(char::is_whitespace))
            .map(|id| OfferedModel {
                value: id.to_owned(),
                ..Default::default()
            })
            .collect(),
    };
    let permissions = wire::levels::acp_modes(agent)
        .iter()
        .map(|(value, _)| super::offered_permission(agent, value, value, false))
        .collect();
    Catalogue {
        models,
        permissions,
        ..Default::default()
    }
}

/// The models of a listing printed one line per model and effort
/// ([`Printed::IdsByEffort`]), in the order each model is first printed,
/// each with its efforts from least to most.
fn by_effort(printed: &str) -> Vec<OfferedModel> {
    /// One printed line: the id and name with the effort taken off, and
    /// the effort; or, for a line whose name carries no effort, the whole
    /// id and name and no effort.
    struct Line<'a> {
        model: &'a str,
        name: &'a str,
        effort: Option<String>,
        id: &'a str,
        full_name: &'a str,
    }
    let lines: Vec<Line> = printed
        .lines()
        .filter_map(|line| {
            let (id, name) = line.trim().split_once('\t')?;
            let (id, name) = (id.trim(), name.trim());
            if id.is_empty() || id.contains(char::is_whitespace) {
                return None;
            }
            let effort = name
                .strip_suffix(')')
                .and_then(|name| name.rsplit_once(" ("))
                .and_then(|(base, effort)| {
                    let effort = effort.to_ascii_lowercase();
                    // Brackets name a variant too ("Orchestrator (Neon)"),
                    // which is a model of its own.
                    if !EFFORTS.contains(&effort.as_str()) {
                        return None;
                    }
                    let model = id.strip_suffix(&format!("-{effort}"))?;
                    Some((model, base, effort))
                });
            Some(match effort {
                Some((model, base, effort)) => Line {
                    model,
                    name: base,
                    effort: Some(effort),
                    id,
                    full_name: name,
                },
                None => Line {
                    model: id,
                    name,
                    effort: None,
                    id,
                    full_name: name,
                },
            })
        })
        .collect();
    let mut models: Vec<OfferedModel> = Vec::new();
    for line in &lines {
        if models
            .iter()
            .any(|model| model.value == line.model || model.value == line.id)
        {
            continue;
        }
        let efforts: Vec<String> = lines
            .iter()
            .filter(|other| other.effort.is_some() && other.model == line.model)
            .filter_map(|other| other.effort.clone())
            .collect();
        models.push(if efforts.len() > 1 {
            let mut efforts = efforts;
            efforts.sort_by_key(|effort| effort_rank(effort));
            efforts.dedup();
            OfferedModel {
                value: line.model.to_owned(),
                display_name: line.name.to_owned(),
                efforts,
                ..Default::default()
            }
        } else {
            OfferedModel {
                value: line.id.to_owned(),
                display_name: line.full_name.to_owned(),
                ..Default::default()
            }
        });
    }
    models
}

/// The efforts a listing names in brackets, from least to most.
const EFFORTS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];

/// Where an effort sorts, from least to most.
fn effort_rank(effort: &str) -> usize {
    EFFORTS
        .iter()
        .position(|known| *known == effort)
        .unwrap_or(EFFORTS.len())
}
