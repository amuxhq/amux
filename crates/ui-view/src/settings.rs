//! The settings view: what the agent offers to change (model, effort,
//! permission, mode) with the current value of each marked, how each one
//! changes from a client, and the commands it offers. The same choices,
//! built from a host's catalogue, make a new agent's settings before it
//! exists, and a pick there is applied here so both clients keep the same
//! rules.
//!
//! Everything offered comes from a catalogue; a client never supplies a
//! list of its own, and every value here is a typed fact each client words.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui_state::SessionState;
use wire::Kind;

/// Claude commands that open an interactive screen of Claude's own
/// terminal, or change a setting this view offers directly: typed into a
/// headless agent they do nothing useful.
const TERMINAL_ONLY_COMMANDS: &[&str] = &[
    "agents",
    "auto-mode-setup",
    "color",
    "config",
    "doctor",
    "effort",
    "exit",
    "heapdump",
    "hooks",
    "ide",
    "keybindings",
    "login",
    "logout",
    "mcp",
    "memory",
    "model",
    "permissions",
    "plugin",
    "resume",
    "statusline",
    "terminal-setup",
    "theme",
    "vim",
];

#[derive(Clone, Debug, Default, PartialEq, Serialize, JsonSchema)]
pub struct SettingsView {
    /// The offered models, then a current model the catalogue does not
    /// list.
    pub models: Vec<ModelChoice>,
    /// The current model's efforts, then a current effort outside them.
    pub efforts: Vec<EffortChoice>,
    /// The offered permissions, then a current one the catalogue does not
    /// list.
    pub permissions: Vec<PermissionChoice>,
    /// The offered modes, then a current one the catalogue does not list;
    /// empty for an agent without modes (Claude).
    pub modes: Vec<ModeChoice>,
    /// How each setting changes from a client.
    pub changeable: Changeability,
    pub commands: Vec<CommandView>,
}

/// How each of an agent's settings changes from a client.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Changeability {
    pub model: Changeable,
    pub effort: Changeable,
    pub permission: Changeable,
    pub mode: Changeable,
}

/// How one setting changes from a client. `Pick`: a pick from its list
/// sets it. `Cycle`: only stepping to the agent's next one changes it, never
/// a pick (terminal Claude's permission, which its own cycle key steps).
/// `ByTyping`: only the agent's own command, named here ("/model"), typed in
/// the composer as any prompt (terminal Claude's model and effort).
/// `NotOffered`: nothing to pick from, as the agent offers none yet, or
/// none of this setting at all.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub enum Changeable {
    Pick,
    Cycle,
    ByTyping(String),
    #[default]
    NotOffered,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ModelChoice {
    /// What a model input names.
    pub value: String,
    /// The name a person reads: the catalogue's, else (for the running
    /// model) the interpreter's, else the value.
    pub display_name: String,
    pub description: String,
    /// The efforts it takes, in the provider's order.
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    pub current: bool,
    /// Current, but the catalogue does not list it: the agent reports it,
    /// or a new agent was given it elsewhere (the installation's settings,
    /// a chat, typed).
    pub unlisted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct EffortChoice {
    pub value: String,
    pub current: bool,
    /// What the model runs at when none is chosen.
    pub default: bool,
    pub unlisted: bool,
}

/// A permission: how much the agent may do without asking.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PermissionChoice {
    /// What a permission input names. Empty when `custom`.
    pub value: String,
    /// The name a person reads: the catalogue's, else the value; empty when
    /// `custom`.
    pub display_name: String,
    /// Codex settings that match no named permission.
    pub custom: bool,
    pub current: bool,
    pub unlisted: bool,
    /// The agent's ordinary one, which a client may leave unsaid.
    pub normal: bool,
    /// Under it the agent acts without asking first.
    pub never_asks: bool,
    /// A pick sets it now: the agent offers it to be set and the model
    /// takes it.
    pub settable: bool,
}

/// A mode: how the agent works (Codex's default or plan).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ModeChoice {
    /// What a mode input names.
    pub value: String,
    /// The name a person reads: the catalogue's, else the value.
    pub display_name: String,
    pub current: bool,
    pub unlisted: bool,
    pub normal: bool,
    pub settable: bool,
}

/// What a running agent's controls come to, said beside its composer: the
/// model by the name a person reads, the effort in force, and the
/// permission and mode only while they are not the agent's normal ones.
/// The permission is not said until the agent's catalogue is held, which
/// says which one is normal; a reported mode the catalogue does not list is
/// said as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ControlsSummary {
    /// None until the agent says which model runs.
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission: Option<PermissionChoice>,
    pub mode: Option<ModeChoice>,
}

/// A person's pick on the settings view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SettingChange {
    Model(String),
    Effort(String),
    /// A permission by its value.
    Permission(String),
    /// A mode by its value.
    Mode(String),
    /// The next permission in the agent's own cycle.
    CyclePermission,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CommandView {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    /// A plugin's namespace or the provider's scope word; empty when the
    /// provider does not say.
    pub source: String,
}

/// What a new agent will start with, by catalogue values. None leaves the
/// model, effort and permission to the host's defaults, and starts in the
/// agent's normal mode.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewAgentChoices {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission: Option<String>,
    pub mode: Option<String>,
}

/// A person's pick for a new agent; None goes back to the host's default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NewAgentPick {
    Model(Option<String>),
    Effort(Option<String>),
    Permission(Option<String>),
    Mode(Option<String>),
}

/// The effort the agent runs at: the one it reports, else its current
/// model's default. Codex reports none until one is chosen, which means
/// the model's default, and its model list says what that is.
fn effort_in_force(agent: &ui_state::AgentState) -> Option<String> {
    if agent.effort.is_some() {
        return agent.effort.clone();
    }
    offered_model(&agent.models, agent.model.as_deref()?)
        .and_then(|model| model.default_effort.clone())
}

/// The offered model `model` names: by value, else the first alias that
/// resolves to it (several may; the one named exactly wins).
fn offered_model<'a>(
    offered: &'a [wire::OfferedModel],
    model: &str,
) -> Option<&'a wire::OfferedModel> {
    offered
        .iter()
        .find(|entry| entry.value == model)
        .or_else(|| offered.iter().find(|entry| entry.resolved_model == model))
}

/// The running model by the name a person reads: the interpreter's name
/// for it, else the catalogue's, else its id. None while it is unknown.
fn model_name(agent: &ui_state::AgentState) -> Option<String> {
    if let Some(name) = agent.model_name.clone().filter(|name| !name.is_empty()) {
        return Some(name);
    }
    let model = agent.model.as_deref().filter(|model| !model.is_empty())?;
    Some(
        offered_model(&agent.models, model)
            .map(|entry| entry.display_name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| model.to_owned()),
    )
}

pub fn settings(state: &SessionState) -> SettingsView {
    let kind = state.kind();
    let agent = state.agent_state();
    let models = model_choices(&agent.models, agent.model.as_deref(), || {
        model_name(agent).unwrap_or_default()
    });
    let current_model = models.iter().find(|model| model.current);
    let efforts = effort_choices(current_model, effort_in_force(agent).as_deref());
    let reported = match kind {
        // Codex settings that match no named permission are reported with
        // no value.
        Kind::Codex if agent.permission.is_none() && agent.approval_policy.is_some() => Some(""),
        _ => agent.permission.as_deref(),
    };
    let permissions = permission_choices(
        agent.permissions.iter(),
        reported,
        current_model.map(|model| model.value.as_str()),
    );
    let modes = mode_choices(agent.modes.iter(), agent.mode.as_deref());
    let terminal_only =
        |name: &str| kind == Kind::ClaudeSdk && TERMINAL_ONLY_COMMANDS.contains(&name);
    let commands = agent
        .commands
        .iter()
        .filter(|command| !terminal_only(&command.name))
        .map(|command| CommandView {
            name: command.name.clone(),
            description: command.description.clone(),
            argument_hint: command.argument_hint.clone(),
            source: command.source.clone(),
        })
        .collect();
    let changeable = match kind {
        // Terminal Claude lists no models or efforts and takes no model,
        // effort or permission input: it runs its own commands, typed as
        // any prompt, and steps its permission with its own cycle key.
        Kind::ClaudePty => Changeability {
            model: Changeable::ByTyping("/model".to_owned()),
            effort: Changeable::ByTyping("/effort".to_owned()),
            permission: Changeable::Cycle,
            mode: Changeable::NotOffered,
        },
        Kind::ClaudeSdk | Kind::Codex => {
            let mut changeable = picks(&models, &efforts, &permissions, &modes);
            if kind == Kind::ClaudeSdk {
                changeable.mode = Changeable::NotOffered;
            }
            changeable
        }
        Kind::Unspecified => Changeability::default(),
    };
    SettingsView {
        models,
        efforts,
        permissions,
        modes,
        changeable,
        commands,
    }
}

/// What a running agent's controls come to, beside its composer.
pub fn controls(state: &SessionState) -> ControlsSummary {
    let agent = state.agent_state();
    let view = settings(state);
    ControlsSummary {
        model: model_name(agent),
        effort: effort_in_force(agent),
        permission: view
            .permissions
            .into_iter()
            .find(|permission| permission.current && !permission.normal)
            .filter(|_| !agent.permissions.is_empty()),
        mode: view
            .modes
            .into_iter()
            .find(|mode| mode.current && !mode.normal),
    }
}

/// A new agent's settings before it exists, from what its host's catalogue
/// offers and what is chosen so far: only what can be set when it starts
/// (a permission the chosen model does not take is not listed), with each
/// chosen value marked, and one the catalogue does not list kept as a
/// choice. The mode in force is marked when none is chosen: the normal one.
pub fn new_agent_settings(catalogue: &wire::Catalogue, chosen: &NewAgentChoices) -> SettingsView {
    let models = model_choices(&catalogue.models, chosen.model.as_deref(), String::new);
    let model = models.iter().find(|model| model.current);
    let efforts = effort_choices(model, chosen.effort.as_deref());
    let model_value = model.map(|model| model.value.as_str());
    let mut permissions = permission_choices(
        catalogue
            .permissions
            .iter()
            .filter(|offered| offered.settable && takes(&offered.models, model_value)),
        chosen.permission.as_deref(),
        model_value,
    );
    // One the catalogue has but this model does not take keeps its name.
    if let Some(unlisted) = permissions.last_mut().filter(|last| last.unlisted)
        && let Some(offered) = catalogue
            .permissions
            .iter()
            .find(|offered| offered.value == unlisted.value)
    {
        unlisted.display_name = name_or_value(&offered.display_name, &offered.value);
    }
    let settable_modes = || catalogue.modes.iter().filter(|mode| mode.settable);
    let mode = chosen.mode.as_deref().or_else(|| {
        settable_modes()
            .find(|mode| mode.normal)
            .map(|mode| mode.value.as_str())
    });
    let modes = mode_choices(settable_modes(), mode);
    let changeable = picks(&models, &efforts, &permissions, &modes);
    SettingsView {
        models,
        efforts,
        permissions,
        modes,
        changeable,
        commands: Vec::new(),
    }
}

/// `chosen` with `pick` applied. Another model starts at its own default
/// effort, and a permission it does not take gives way to the normal one
/// (else the host's default). A model the catalogue does not list says
/// nothing of its efforts, so the effort stays. Picking the normal mode
/// leaves the mode unchosen.
pub fn new_agent_pick(
    catalogue: &wire::Catalogue,
    chosen: &NewAgentChoices,
    pick: &NewAgentPick,
) -> NewAgentChoices {
    let mut next = chosen.clone();
    match pick {
        NewAgentPick::Model(model) => {
            if *model == chosen.model {
                return next;
            }
            next.model = model.clone();
            let entry = model
                .as_deref()
                .and_then(|model| offered_model(&catalogue.models, model));
            match (model, entry) {
                (_, Some(entry)) => next.effort = entry.default_effort.clone(),
                (None, None) => next.effort = None,
                (Some(_), None) => {}
            }
            let model_value = entry.map(|entry| entry.value.as_str()).or(model.as_deref());
            let offered = |permission: &wire::OfferedPermission| {
                permission.settable && takes(&permission.models, model_value)
            };
            let dropped = chosen.permission.as_deref().is_some_and(|value| {
                catalogue
                    .permissions
                    .iter()
                    .any(|permission| permission.value == value && !offered(permission))
            });
            if dropped {
                next.permission = catalogue
                    .permissions
                    .iter()
                    .find(|permission| permission.normal && offered(permission))
                    .map(|permission| permission.value.clone());
            }
        }
        NewAgentPick::Effort(effort) => next.effort = effort.clone(),
        NewAgentPick::Permission(permission) => next.permission = permission.clone(),
        NewAgentPick::Mode(mode) => {
            let normal = mode.as_deref().is_some_and(|mode| {
                catalogue
                    .modes
                    .iter()
                    .any(|offered| offered.value == mode && offered.normal)
            });
            next.mode = if normal { None } else { mode.clone() };
        }
    }
    next
}

/// Which settings a pick changes, from the lists: a model whenever the
/// catalogue offers any, an effort when the current model takes some, a
/// permission when one is settable, and a mode when there are two to
/// choose between.
fn picks(
    models: &[ModelChoice],
    efforts: &[EffortChoice],
    permissions: &[PermissionChoice],
    modes: &[ModeChoice],
) -> Changeability {
    let pick = |offered: bool| {
        if offered {
            Changeable::Pick
        } else {
            Changeable::NotOffered
        }
    };
    Changeability {
        model: pick(models.iter().any(|model| !model.unlisted)),
        effort: pick(efforts.iter().any(|effort| !effort.unlisted)),
        permission: pick(permissions.iter().any(|permission| permission.settable)),
        mode: pick(modes.iter().filter(|mode| mode.settable).count() >= 2),
    }
}

/// The input a pick sends, in the kind's arm; None where the view offers
/// no such pick. The id is the sender's to fill.
pub fn setting_input(kind: Kind, change: &SettingChange) -> Option<wire::Input> {
    use wire::{
        ClaudePtyInput, ClaudeSdkInput, CodexInput, CyclePermissionMode, KindInput, SetEffort,
        SetMode, SetModel, SetPermission, claude_pty_input, claude_sdk_input, codex_input,
    };
    let model = |model: &String| SetModel {
        model: Some(model.clone()),
    };
    let of = match (kind, change) {
        (Kind::ClaudePty, SettingChange::CyclePermission) => KindInput::ClaudePty(ClaudePtyInput {
            of: Some(claude_pty_input::Of::CyclePermissionMode(
                CyclePermissionMode {},
            )),
        }),
        (Kind::ClaudeSdk, SettingChange::Model(name)) => KindInput::ClaudeSdk(ClaudeSdkInput {
            of: Some(claude_sdk_input::Of::Model(model(name))),
        }),
        (Kind::ClaudeSdk, SettingChange::Effort(effort)) => KindInput::ClaudeSdk(ClaudeSdkInput {
            of: Some(claude_sdk_input::Of::Effort(SetEffort {
                effort: Some(effort.clone()),
            })),
        }),
        (Kind::ClaudeSdk, SettingChange::Permission(value)) if !value.is_empty() => {
            KindInput::ClaudeSdk(ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Permission(SetPermission {
                    value: value.clone(),
                })),
            })
        }
        (Kind::Codex, SettingChange::Model(name)) => KindInput::Codex(CodexInput {
            of: Some(codex_input::Of::Model(model(name))),
        }),
        (Kind::Codex, SettingChange::Effort(effort)) => KindInput::Codex(CodexInput {
            of: Some(codex_input::Of::Effort(SetEffort {
                effort: Some(effort.clone()),
            })),
        }),
        // Settings that match no named permission have no value to send.
        (Kind::Codex, SettingChange::Permission(value)) if !value.is_empty() => {
            KindInput::Codex(CodexInput {
                of: Some(codex_input::Of::Permission(SetPermission {
                    value: value.clone(),
                })),
            })
        }
        (Kind::Codex, SettingChange::Mode(value)) if !value.is_empty() => {
            KindInput::Codex(CodexInput {
                of: Some(codex_input::Of::Mode(SetMode {
                    value: value.clone(),
                })),
            })
        }
        _ => return None,
    };
    Some(wire::Input::new(Vec::new(), of))
}

/// Whether a permission that names `models` (none: every model) is taken
/// while `model` runs.
fn takes(models: &[String], model: Option<&str>) -> bool {
    models.is_empty() || model.is_some_and(|model| models.iter().any(|named| named == model))
}

/// The catalogue's models with `current` marked by value, else by the id
/// an alias resolves to; a current model the catalogue lacks is added after
/// them, named by `unlisted_name` when it has a name, else by its value.
fn model_choices(
    offered: &[wire::OfferedModel],
    current: Option<&str>,
    unlisted_name: impl FnOnce() -> String,
) -> Vec<ModelChoice> {
    let at = current.and_then(|current| {
        offered
            .iter()
            .position(|model| model.value == current)
            .or_else(|| {
                offered
                    .iter()
                    .position(|model| model.resolved_model == current)
            })
    });
    let mut models: Vec<ModelChoice> = offered
        .iter()
        .enumerate()
        .map(|(index, model)| ModelChoice {
            value: model.value.clone(),
            display_name: name_or_value(&model.display_name, &model.value),
            description: model.description.clone(),
            efforts: model.efforts.clone(),
            default_effort: model.default_effort.clone(),
            current: at == Some(index),
            unlisted: false,
        })
        .collect();
    if let Some(value) = current
        && at.is_none()
    {
        models.push(ModelChoice {
            value: value.to_owned(),
            display_name: name_or_value(&unlisted_name(), value),
            description: String::new(),
            efforts: Vec::new(),
            default_effort: None,
            current: true,
            unlisted: true,
        });
    }
    models
}

/// `model`'s efforts with `current` marked, one it does not take added
/// after them.
fn effort_choices(model: Option<&ModelChoice>, current: Option<&str>) -> Vec<EffortChoice> {
    let (offered, default) = model
        .map(|model| (model.efforts.as_slice(), model.default_effort.as_deref()))
        .unwrap_or_default();
    let mut efforts: Vec<EffortChoice> = offered
        .iter()
        .map(|effort| EffortChoice {
            value: effort.clone(),
            current: current == Some(effort.as_str()),
            default: default == Some(effort.as_str()),
            unlisted: false,
        })
        .collect();
    if let Some(value) = current
        && !efforts.iter().any(|effort| effort.current)
    {
        efforts.push(EffortChoice {
            value: value.to_owned(),
            current: true,
            default: false,
            unlisted: true,
        });
    }
    efforts
}

/// The `offered` permissions with `current` marked, one they lack added
/// after them; an empty current value is Codex's custom settings. A
/// permission that names its models is settable only while `model` is one
/// of them.
fn permission_choices<'a>(
    offered: impl Iterator<Item = &'a wire::OfferedPermission>,
    current: Option<&str>,
    model: Option<&str>,
) -> Vec<PermissionChoice> {
    let mut permissions: Vec<PermissionChoice> = offered
        .map(|offered| PermissionChoice {
            value: offered.value.clone(),
            display_name: name_or_value(&offered.display_name, &offered.value),
            custom: false,
            current: current == Some(offered.value.as_str()),
            unlisted: false,
            normal: offered.normal,
            never_asks: offered.never_asks,
            settable: offered.settable && takes(&offered.models, model),
        })
        .collect();
    if let Some(value) = current
        && !permissions.iter().any(|permission| permission.current)
    {
        permissions.push(PermissionChoice {
            value: value.to_owned(),
            display_name: value.to_owned(),
            custom: value.is_empty(),
            current: true,
            unlisted: true,
            normal: false,
            never_asks: false,
            settable: false,
        });
    }
    permissions
}

/// The `offered` modes with `current` marked, one they lack added after
/// them.
fn mode_choices<'a>(
    offered: impl Iterator<Item = &'a wire::OfferedMode>,
    current: Option<&str>,
) -> Vec<ModeChoice> {
    let mut modes: Vec<ModeChoice> = offered
        .map(|offered| ModeChoice {
            value: offered.value.clone(),
            display_name: name_or_value(&offered.display_name, &offered.value),
            current: current == Some(offered.value.as_str()),
            unlisted: false,
            normal: offered.normal,
            settable: offered.settable,
        })
        .collect();
    if let Some(value) = current
        && !modes.iter().any(|mode| mode.current)
    {
        modes.push(ModeChoice {
            value: value.to_owned(),
            display_name: value.to_owned(),
            current: true,
            unlisted: true,
            normal: false,
            settable: false,
        });
    }
    modes
}

fn name_or_value(name: &str, value: &str) -> String {
    if name.is_empty() { value } else { name }.to_owned()
}
