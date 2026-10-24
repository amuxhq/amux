//! A new agent's settings before it exists: its name, which agent, its
//! model, effort, permission and mode, where it works and on which machine.
//! What there is to pick comes from the chosen host's catalogue for the
//! chosen agent, asked of that host before any agent runs there. The
//! flyover that changes one of them, which a running chat also uses for its
//! model and effort. And the short form that starts an agent to be used in
//! its own terminal.
//!
//! A person sees two agents, Claude and Codex, everywhere. Where they chat
//! with it decides what runs behind it: in amux's chat, headless Claude or
//! Codex's app server; in the agents' own terminals, Claude in its terminal,
//! or Codex's app server with a terminal attached that runs its own
//! interface on the same thread.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ui_state::FleetState;
use ui_view::{Changeable, NewAgentChoices, NewAgentPick, Reach, SettingChange, SettingsView};
use wire::{
    ClaudeCreateConfig, CodexCreateConfig, CreateAgentRequest, Kind, PromptSubmission,
    create_agent_request,
};

use crate::editor::Editor;
use crate::text::{self, push};
use crate::theme::Theme;

/// Where a person chats with their agents: a preference for this
/// installation, asked once (in onboarding, when it exists) and changed in
/// settings. It decides what a new agent runs on and how starting it ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatIn {
    /// In amux's own chat.
    #[default]
    Amux,
    /// In each agent's own terminal, attached on start.
    Terminal,
}

/// The two agents a person chooses between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
}

const AGENTS: [Agent; 2] = [Agent::Claude, Agent::Codex];

impl Agent {
    fn name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
        }
    }

    /// What runs it where the person chats.
    fn kind(self, chat_in: ChatIn) -> Kind {
        match (self, chat_in) {
            (Agent::Claude, ChatIn::Amux) => Kind::ClaudeSdk,
            (Agent::Claude, ChatIn::Terminal) => Kind::ClaudePty,
            (Agent::Codex, _) => Kind::Codex,
        }
    }

    fn of(kind: Kind) -> Agent {
        match kind {
            Kind::Codex => Agent::Codex,
            _ => Agent::Claude,
        }
    }

    /// The provider a host's catalogue is asked for.
    pub fn provider(self) -> wire::Provider {
        match self {
            Agent::Claude => wire::Provider::Claude,
            Agent::Codex => wire::Provider::Codex,
        }
    }
}

/// A provider as a person names it.
pub fn provider_name(provider: wire::Provider) -> &'static str {
    match provider {
        wire::Provider::Claude => "Claude",
        wire::Provider::Codex => "Codex",
        wire::Provider::Unspecified => "the provider",
    }
}

/// Every provider a host can be asked about.
pub const PROVIDERS: [wire::Provider; 2] = [wire::Provider::Claude, wire::Provider::Codex];

/// Whether `provider` is signed in on `host`, once the host has said.
pub fn signed_in(fleet: &FleetState, host: &[u8], provider: wire::Provider) -> Option<bool> {
    fleet
        .host(host)?
        .providers
        .iter()
        .find(|offer| offer.provider == provider as i32)
        .map(|offer| offer.signed_in)
}

/// What a new agent starts with, per agent: model, effort and permission,
/// from the installation's settings (shipped with real values), so the new
/// agent always shows what it will run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentDefaults {
    pub model: String,
    pub effort: String,
    /// A permission by its catalogue value.
    pub permission: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Defaults {
    pub claude: AgentDefaults,
    pub codex: AgentDefaults,
}

impl Defaults {
    fn of(&self, agent: Agent) -> &AgentDefaults {
        match agent {
            Agent::Claude => &self.claude,
            Agent::Codex => &self.codex,
        }
    }
}

/// One of the settings, as the composer's edge shows it and a flyover
/// changes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Name,
    Kind,
    Model,
    Effort,
    Permission,
    Mode,
    Folder,
    Worktree,
    Host,
}

/// What a new agent will be.
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    /// None: named for the person automatically.
    pub name: Option<String>,
    pub agent: Agent,
    pub chat_in: ChatIn,
    /// None runs the provider's default.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The permission by its value.
    pub permission: Option<String>,
    /// The mode by its value; None starts in the agent's normal one.
    pub mode: Option<String>,
    /// What the chosen host offers for the chosen agent, once it has said.
    pub offered: Option<(Vec<u8>, wire::Catalogue)>,
    /// Where it works, as the request names it; empty until one is chosen
    /// on a host that has none yet.
    pub folder: String,
    pub host: Vec<u8>,
    /// This machine's host, from which every other is reached or not.
    pub local_host: Vec<u8>,
    /// Start it in a new worktree of the folder's repository.
    pub worktree: bool,
    /// What each agent starts with, for when the agent changes.
    pub defaults: Defaults,
}

impl Setup {
    /// From home: the defaults. No memory per folder: that would start a
    /// notion of projects nobody has designed.
    pub fn defaults(
        chat_in: ChatIn,
        working_dir: &str,
        local_host: &[u8],
        defaults: &Defaults,
    ) -> Setup {
        let mut setup = Setup {
            name: None,
            agent: Agent::Claude,
            chat_in,
            model: None,
            effort: None,
            permission: None,
            mode: None,
            offered: None,
            folder: working_dir.to_owned(),
            host: local_host.to_vec(),
            local_host: local_host.to_vec(),
            worktree: false,
            defaults: defaults.clone(),
        };
        setup.start_from_defaults();
        setup
    }

    /// The agent's model, effort and permission from the settings.
    fn start_from_defaults(&mut self) {
        let defaults = self.defaults.of(self.agent).clone();
        self.model = Some(defaults.model);
        self.effort = Some(defaults.effort);
        self.permission = Some(defaults.permission);
        self.mode = None;
    }

    /// A running agent's settings, to start a sibling where the person
    /// chats now: whatever the chat did not report comes from the settings.
    pub fn sibling(mut self, kind: Kind, chat_in: ChatIn) -> Setup {
        self.agent = Agent::of(kind);
        self.chat_in = chat_in;
        self.offered = None;
        let defaults = self.defaults.of(self.agent).clone();
        self.model.get_or_insert(defaults.model);
        self.effort.get_or_insert(defaults.effort);
        self.permission.get_or_insert(defaults.permission);
        self
    }

    /// Takes what `host` offers for `provider`, when that is what this
    /// setup is for; the choices already made stay, offered or not.
    pub fn offer(
        &mut self,
        host: &[u8],
        provider: wire::Provider,
        hash: &[u8],
        catalogue: &wire::Catalogue,
    ) {
        if host == self.host && provider == self.agent.provider() {
            self.offered = Some((hash.to_vec(), catalogue.clone()));
        }
    }

    /// The hash of what this setup holds from its host, if anything.
    pub fn offered_hash(&self) -> Option<&[u8]> {
        self.offered.as_ref().map(|(hash, _)| hash.as_slice())
    }

    /// What this setup holds from its host, or nothing offered.
    fn offered(&self) -> wire::Catalogue {
        self.offered
            .as_ref()
            .map(|(_, catalogue)| catalogue.clone())
            .unwrap_or_default()
    }

    /// What is chosen so far, as the settings view takes it.
    fn chosen(&self) -> NewAgentChoices {
        NewAgentChoices {
            model: self.model.clone(),
            effort: self.effort.clone(),
            permission: self.permission.clone(),
            mode: self.mode.clone(),
        }
    }

    /// Its settings as the shared view builds them from what the host
    /// offers, before the host has said, from what is chosen alone.
    fn view(&self) -> SettingsView {
        ui_view::new_agent_settings(&self.offered(), &self.chosen())
    }

    /// A pick, with the rules the shared view gives it: another model
    /// starts at its default effort and drops a permission it does not
    /// take.
    fn apply(&mut self, pick: NewAgentPick) {
        let next = ui_view::new_agent_pick(&self.offered(), &self.chosen(), &pick);
        self.model = next.model;
        self.effort = next.effort;
        self.permission = next.permission;
        self.mode = next.mode;
    }

    /// What runs it, as the request names it.
    pub fn kind(&self) -> Kind {
        self.agent.kind(self.chat_in)
    }

    /// The composer's edge, in groups: its name, what runs it, where, and
    /// on which machine. Without a name the first item invites one; "new
    /// worktree" shows only when it is on.
    pub fn edge(&self, fleet: &FleetState) -> Vec<Vec<(Item, String)>> {
        // Always the values it will start with, the permission too: here it
        // is a setting to pick, so even the normal one is named. The mode,
        // which Shift+Tab steps, speaks only when it is not the normal one,
        // as in a chat.
        let view = self.view();
        let mut model = view
            .models
            .iter()
            .find(|model| model.current)
            .map(|model| model.display_name.clone())
            .unwrap_or_default();
        if let Some(effort) = &self.effort {
            model.push_str(&format!(" ({effort})"));
        }
        let permission = view
            .permissions
            .iter()
            .find(|permission| permission.current)
            .map(crate::words::permission_words)
            .unwrap_or_default();
        let mut agent = self.agent.name().to_owned();
        if signed_in(fleet, &self.host, self.agent.provider()) == Some(false) {
            agent.push_str(" (not signed in)");
        }
        let mut runs = vec![
            (Item::Kind, agent),
            (Item::Model, model),
            (Item::Permission, permission),
        ];
        if let Some(mode) = view.modes.iter().find(|mode| mode.current && !mode.normal) {
            runs.push((Item::Mode, crate::words::mode_words(mode)));
        }
        let folder = match self.folder.as_str() {
            "" => "+ Folder".to_owned(),
            folder => text::tilde(folder),
        };
        let mut place = vec![(Item::Folder, folder)];
        if self.worktree {
            place.push((Item::Worktree, "new worktree".to_owned()));
        }
        let name = self.name.clone().unwrap_or_else(|| "+ Name".to_owned());
        vec![
            vec![(Item::Name, name)],
            runs,
            place,
            vec![(Item::Host, host_name(fleet, &self.host))],
        ]
    }

    /// The choices for one setting, the current one marked.
    pub fn choices(&self, item: Item, fleet: &FleetState) -> Vec<Choice> {
        let choice = |label: String, value: String, current: bool| Choice {
            label,
            detail: String::new(),
            value,
            current,
            disabled: false,
            typed: false,
        };
        match item {
            Item::Kind => AGENTS
                .iter()
                .enumerate()
                .map(|(at, agent)| Choice {
                    detail: not_signed_in(fleet, &self.host, *agent),
                    ..choice(
                        agent.name().to_owned(),
                        at.to_string(),
                        *agent == self.agent,
                    )
                })
                .collect(),
            Item::Model => model_choices(&self.view()),
            Item::Effort => effort_choices(&self.view()),
            Item::Permission => permission_choices(&self.view()),
            Item::Mode => mode_choices(&self.view()),
            Item::Folder => recent_folders(fleet, &self.host, &self.folder)
                .into_iter()
                .map(|folder| {
                    let current = folder == self.folder;
                    choice(text::tilde(&folder), folder, current)
                })
                .collect(),
            Item::Host => hosts(fleet, self, true),
            Item::Name | Item::Worktree => Vec::new(),
        }
    }

    /// Whether the flyover for `item` takes a typed value as a choice of
    /// its own: a folder's path, or a model by name until the host has said
    /// which it offers.
    pub fn takes_typed(&self, item: Item) -> bool {
        match item {
            Item::Folder => true,
            Item::Model => self.view().changeable.model != Changeable::Pick,
            _ => false,
        }
    }

    /// What Shift+Tab steps, as its hint names it: the mode when the agent
    /// offers modes, else the permission.
    pub fn cycles(&self) -> &'static str {
        match crate::chat::next_control(&self.view()) {
            Some(SettingChange::Mode(_)) => "mode",
            _ => "permission",
        }
    }

    /// Shift+Tab, as in a chat: the next mode when the agent offers modes,
    /// otherwise the next permission that still asks before acting.
    pub fn next_control(&mut self) {
        match crate::chat::next_control(&self.view()) {
            Some(SettingChange::Mode(mode)) => self.apply(NewAgentPick::Mode(Some(mode))),
            Some(SettingChange::Permission(permission)) => {
                self.apply(NewAgentPick::Permission(Some(permission)))
            }
            _ => {}
        }
    }

    /// Takes a picked value for `item`. Another agent starts its model,
    /// effort and permission afresh: they do not carry across providers.
    pub fn pick(&mut self, item: Item, value: &str) {
        let some = |value: &str| (!value.is_empty()).then(|| value.to_owned());
        match item {
            Item::Name => self.name = some(value.trim()),
            Item::Kind => {
                if let Some(agent) = value.parse::<usize>().ok().and_then(|at| AGENTS.get(at))
                    && *agent != self.agent
                {
                    self.agent = *agent;
                    self.offered = None;
                    self.start_from_defaults();
                }
            }
            // An empty pick keeps the model and the effort.
            Item::Model => {
                if let Some(model) = some(value) {
                    self.apply(NewAgentPick::Model(Some(model)));
                }
            }
            Item::Effort => {
                if let Some(effort) = some(value) {
                    self.apply(NewAgentPick::Effort(Some(effort)));
                }
            }
            Item::Permission => self.apply(NewAgentPick::Permission(some(value))),
            Item::Mode => self.apply(NewAgentPick::Mode(some(value))),
            Item::Folder => {
                if !value.trim().is_empty() {
                    self.folder = expand_tilde(value.trim());
                }
            }
            // A folder names a place on one machine: on another host it
            // names nothing, or something else. The new host's last folder
            // comes from the fleet (`fill_folder`), else one must be chosen.
            Item::Host => {
                if let Some(host) = host_of_value(value)
                    && host != self.host
                {
                    self.host = host;
                    self.folder.clear();
                    self.offered = None;
                }
            }
            Item::Worktree => self.worktree = !self.worktree,
        }
    }

    /// With no folder on its host yet, the one the host's agents last
    /// worked in, when the fleet knows one.
    pub fn fill_folder(&mut self, fleet: &FleetState) {
        if self.folder.is_empty()
            && let Some(folder) = recent_folders(fleet, &self.host, "").into_iter().next()
        {
            self.folder = folder;
        }
    }

    /// The create request, with the first prompt when there is one.
    pub fn request(
        &self,
        agent_id: Vec<u8>,
        prompt: Option<PromptSubmission>,
    ) -> CreateAgentRequest {
        // Used in its own terminal, the agent's terminal sets these.
        let own = self.chat_in == ChatIn::Terminal;
        let permission = if own { None } else { self.permission.clone() };
        let model = if own { None } else { self.model.clone() };
        let effort = if own { None } else { self.effort.clone() };
        let mode = if own { None } else { self.mode.clone() };
        let claude = ClaudeCreateConfig {
            args: Vec::new(),
            model: model.clone(),
            permission: permission.clone(),
            effort: effort.clone(),
        };
        let config = match self.kind() {
            Kind::ClaudePty => create_agent_request::Agent::ClaudePty(claude),
            Kind::ClaudeSdk => create_agent_request::Agent::ClaudeSdk(claude),
            _ => create_agent_request::Agent::Codex(CodexCreateConfig {
                model,
                permission,
                mode,
                effort,
            }),
        };
        CreateAgentRequest {
            agent_id,
            host: Some(wire::HostRef::id(self.host.clone())),
            name: self.name.clone(),
            cwd: self.folder.clone(),
            initial_prompt: prompt,
            agent: Some(config),
            new_worktree: self.worktree,
            ..CreateAgentRequest::default()
        }
    }
}

/// "not signed in" when the host has said `agent` is signed out there.
fn not_signed_in(fleet: &FleetState, host: &[u8], agent: Agent) -> String {
    match signed_in(fleet, host, agent.provider()) {
        Some(false) => "not signed in".to_owned(),
        _ => String::new(),
    }
}

fn host_name(fleet: &FleetState, host: &[u8]) -> String {
    fleet
        .host(host)
        .map(|host| host.name().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "this machine".into())
}

/// The hosts an agent can start on, the chosen one first in a list (a
/// form's chips keep their places as the choice moves); one away cannot
/// take a new agent, so it is listed but not picked.
fn hosts(fleet: &FleetState, setup: &Setup, chosen_first: bool) -> Vec<Choice> {
    let chosen = &setup.host[..];
    let mut hosts: Vec<&wire::HostEntry> = fleet
        .hosts()
        .filter(|host| host.trusted().is_some() || host.host_id() == chosen)
        .collect();
    // By name, so the row holds still while the arrows move along it.
    hosts.sort_by(|a, b| {
        (a.name().to_lowercase(), &a.host_id()).cmp(&(b.name().to_lowercase(), &b.host_id()))
    });
    if chosen_first {
        hosts.sort_by_key(|host| host.host_id() != chosen);
    }
    hosts
        .into_iter()
        .map(|host| {
            let (detail, away) = match ui_view::reach(fleet, &setup.local_host, host.host_id()) {
                Reach::Online(_) => (String::new(), false),
                Reach::Away => ("away".to_owned(), true),
                Reach::Offline(_) => ("offline".to_owned(), true),
            };
            Choice {
                label: host_name(fleet, host.host_id()),
                detail,
                value: host_value(host.host_id()),
                current: host.host_id() == chosen,
                disabled: away && host.host_id() != chosen,
                typed: false,
            }
        })
        .collect()
}

/// A host's id as a choice's value: hex, since an id is bytes, not text.
fn host_value(host: &[u8]) -> String {
    host.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The host id a choice's value names.
fn host_of_value(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(value.get(at..at + 2)?, 16).ok())
        .collect()
}

/// The folders agents on `host` work in, the most recently busy first,
/// with `current` among them.
fn recent_folders(fleet: &FleetState, host: &[u8], current: &str) -> Vec<String> {
    let mut agents: Vec<&wire::Agent> = fleet
        .agents()
        .filter(|agent| agent.host_id == host && !agent.cwd.is_empty())
        .collect();
    agents.sort_by_key(|agent| std::cmp::Reverse(agent.phase_since_ms));
    let mut folders: Vec<String> = Some(current.to_owned())
        .filter(|current| !current.is_empty())
        .into_iter()
        .collect();
    for agent in agents {
        if !folders.contains(&agent.cwd) {
            folders.push(agent.cwd.clone());
        }
    }
    folders
}

fn expand_tilde(path: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", std::env::var("HOME").unwrap_or_default())
        }
        _ => path.to_owned(),
    }
}

/// One choice of a flyover.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub label: String,
    /// Faint after the label: "away", "default".
    pub detail: String,
    /// What picking it sets.
    pub value: String,
    pub current: bool,
    /// Shown but not picked: a host that is away.
    pub disabled: bool,
    /// What is typed in the field, offered as a choice of its own.
    pub typed: bool,
}

/// A running or new agent's models as a flyover lists them, each with the
/// provider's own line on it, which also says what an alias such as
/// Default stands for.
pub(crate) fn model_choices(view: &SettingsView) -> Vec<Choice> {
    view.models
        .iter()
        .map(|model| Choice {
            label: model.display_name.clone(),
            detail: model.description.clone(),
            value: model.value.clone(),
            current: model.current,
            disabled: false,
            typed: false,
        })
        .collect()
}

/// The current model's efforts, its default marked.
pub(crate) fn effort_choices(view: &SettingsView) -> Vec<Choice> {
    view.efforts
        .iter()
        .map(|effort| Choice {
            label: effort.value.clone(),
            detail: if effort.default {
                "default".into()
            } else {
                String::new()
            },
            value: effort.value.clone(),
            current: effort.current,
            disabled: false,
            typed: false,
        })
        .collect()
}

/// The permissions, one that acts without asking and one the model does
/// not take said so; only a settable one is picked.
pub(crate) fn permission_choices(view: &SettingsView) -> Vec<Choice> {
    view.permissions
        .iter()
        .map(|permission| Choice {
            label: crate::words::permission_words(permission),
            detail: if permission.never_asks {
                "acts without asking".into()
            } else if !permission.settable && !permission.unlisted {
                "not with this model".into()
            } else {
                String::new()
            },
            value: permission.value.clone(),
            current: permission.current,
            disabled: !permission.settable,
            typed: false,
        })
        .collect()
}

/// The modes, the one in force marked.
pub(crate) fn mode_choices(view: &SettingsView) -> Vec<Choice> {
    view.modes
        .iter()
        .map(|mode| Choice {
            label: crate::words::mode_words(mode),
            detail: String::new(),
            value: mode.value.clone(),
            current: mode.current,
            disabled: !mode.settable,
            typed: false,
        })
        .collect()
}

/// What a key on a flyover did.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    None,
    Close,
    /// The value picked, or typed text (a name, a folder's path).
    Value(String),
}

/// What a flyover holds: a list to pick from, which long lists filter as
/// you type; a list that also takes what is typed as a choice (a folder's
/// path, a model's name); or a single text field, for a name.
#[derive(Debug, PartialEq, Eq)]
enum Shape {
    List,
    Typed,
    Text,
}

/// The panel that changes one setting: it opens upward from the setting's
/// place on the composer's edge, anchored over it, with the chat still
/// visible around it.
#[derive(Debug)]
pub struct Picker {
    pub item: Item,
    title: String,
    choices: Vec<Choice>,
    selected: usize,
    field: Editor,
    shape: Shape,
    /// Why the choice last reached for cannot be picked, said under the
    /// list until the next key.
    refused: Option<String>,
}

/// Lists this long filter as you type.
const FILTERS_FROM: usize = 6;
/// Choices shown at once.
const SHOWN: usize = 8;
/// The narrowest a flyover is drawn, inside its border.
const FLYOVER_MIN: usize = 24;

impl Picker {
    /// A list of `choices`; `typed` also takes what is typed as a choice.
    pub fn new(item: Item, title: &str, choices: Vec<Choice>, typed: bool) -> Picker {
        let selected = choices.iter().position(|c| c.current).unwrap_or(0);
        Picker {
            item,
            title: title.to_owned(),
            choices,
            selected,
            field: Editor::default(),
            shape: if typed { Shape::Typed } else { Shape::List },
            refused: None,
        }
    }

    /// A single text field, holding `text` to begin with.
    pub fn text(item: Item, title: &str, text: &str) -> Picker {
        let mut field = Editor::default();
        field.set(text, Vec::new());
        Picker {
            item,
            title: title.to_owned(),
            choices: Vec::new(),
            selected: 0,
            field,
            shape: Shape::Text,
            refused: None,
        }
    }

    fn filtering(&self) -> bool {
        self.shape != Shape::List || self.choices.len() >= FILTERS_FROM
    }

    /// Why the host would refuse the name typed in a name field; an empty
    /// field names the agent automatically.
    fn name_problem(&self) -> Option<wire::AgentNameProblem> {
        let typed = self.field.text().trim();
        if self.shape != Shape::Text || typed.is_empty() {
            return None;
        }
        wire::agent_name_problem(typed)
    }

    /// The choices the filter keeps, and what is typed as the last.
    fn shown(&self) -> Vec<Choice> {
        if self.shape == Shape::Text {
            return Vec::new();
        }
        let needle = self.field.text().trim().to_lowercase();
        let mut shown: Vec<Choice> = self
            .choices
            .iter()
            .filter(|c| needle.is_empty() || c.label.to_lowercase().contains(&needle))
            .cloned()
            .collect();
        if self.shape == Shape::Typed
            && !needle.is_empty()
            && !shown.iter().any(|c| c.label.to_lowercase() == needle)
        {
            shown.push(Choice {
                label: format!("Use {}", self.field.text().trim()),
                detail: String::new(),
                value: self.field.text().trim().to_owned(),
                current: false,
                disabled: false,
                typed: true,
            });
        }
        shown
    }

    /// The next choice from `from` in `step`'s direction that can be
    /// picked, or `from` when there is none.
    fn step(shown: &[Choice], from: usize, step: isize) -> usize {
        let mut at = from as isize;
        loop {
            at += step;
            if at < 0 || at as usize >= shown.len() {
                return from;
            }
            if !shown[at as usize].disabled {
                return at as usize;
            }
        }
    }

    /// `choice` picked, or when it cannot be (a host that is away), why.
    fn reach(&mut self, choice: Option<&Choice>) -> Pick {
        match choice {
            Some(choice) if choice.disabled => {
                self.refused = Some(format!(
                    "{} is {}, so it cannot start an agent now",
                    choice.label, choice.detail
                ));
                Pick::None
            }
            Some(choice) => Pick::Value(choice.value.clone()),
            None => Pick::None,
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Pick {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        self.refused = None;
        if self.shape == Shape::Text {
            return match key.code {
                KeyCode::Esc => Pick::Close,
                // A name the host would refuse is not taken: the field
                // stays open beside its reason.
                KeyCode::Enter if self.name_problem().is_some() => Pick::None,
                KeyCode::Enter => Pick::Value(self.field.text().trim().to_owned()),
                _ => {
                    if !ctrl || matches!(key.code, KeyCode::Char('a' | 'e' | 'k' | 'u' | 'w')) {
                        self.field.key(key);
                    }
                    Pick::None
                }
            };
        }
        let shown = self.shown();
        let selected = self.selected.min(shown.len().saturating_sub(1));
        match key.code {
            KeyCode::Esc => return Pick::Close,
            KeyCode::Up => self.selected = Self::step(&shown, selected, -1),
            KeyCode::Down => self.selected = Self::step(&shown, selected, 1),
            KeyCode::Char('k') if !self.filtering() => {
                self.selected = Self::step(&shown, selected, -1)
            }
            KeyCode::Char('j') if !self.filtering() => {
                self.selected = Self::step(&shown, selected, 1)
            }
            KeyCode::Char(c @ '1'..='9') if !self.filtering() => {
                return self.reach(shown.get(c as usize - '1' as usize));
            }
            // The best match fills the field, to go on typing.
            KeyCode::Tab if self.shape == Shape::Typed => {
                if let Some(choice) = shown.get(selected).filter(|c| !c.typed) {
                    let label = choice.label.clone();
                    self.field.set(&label, Vec::new());
                    self.selected = 0;
                }
            }
            KeyCode::Enter => return self.reach(shown.get(selected)),
            _ if self.filtering() && !ctrl => {
                self.field.key(key);
                self.selected = 0;
            }
            _ => {}
        }
        Pick::None
    }

    /// The flyover's lines inside its border, which choice each line shows,
    /// and the cursor's (column, line) when a field has the keys.
    #[allow(clippy::type_complexity)]
    fn body(
        &self,
        theme: Theme,
    ) -> (
        Vec<Line<'static>>,
        Vec<Option<usize>>,
        Option<(usize, usize)>,
    ) {
        let wide = usize::MAX / 4;
        let mut lines = Vec::new();
        let mut rows = Vec::new();
        let mut cursor = None;
        if self.filtering() {
            // A name's field is the prompt; a filter sits above choices whose
            // own pointer is the one that moves.
            let mark = if self.shape == Shape::Text {
                "› "
            } else {
                "  "
            };
            let mut field = Line::from(Span::styled(mark, theme.muted()));
            let at = text::line_width(&field);
            if self.field.is_empty() {
                let hint = match self.shape {
                    Shape::Text => "named automatically",
                    Shape::Typed if self.item == Item::Folder => "type a path or filter",
                    Shape::Typed => "type a name or filter",
                    Shape::List => "type to filter",
                };
                cursor = Some((at, 0));
                push(&mut field, hint, theme.faint(), wide);
            } else {
                cursor = Some((at + self.field.cursor_chars(), 0));
                push(&mut field, self.field.text(), theme.text(), wide);
            }
            lines.push(field);
            rows.push(None);
            // The reason on a line of its own, so a narrow flyover does
            // not cut it.
            if let Some(problem) = self.name_problem() {
                let mut why = Line::from(Span::raw("  "));
                push(
                    &mut why,
                    crate::words::name_problem(problem),
                    theme.warning(),
                    wide,
                );
                lines.push(why);
                rows.push(None);
            }
        }
        if self.shape == Shape::Text {
            return (lines, rows, cursor);
        }
        let shown = self.shown();
        let selected = self.selected.min(shown.len().saturating_sub(1));
        let from = selected.saturating_sub(SHOWN - 1);
        for (at, choice) in shown.iter().enumerate().skip(from).take(SHOWN) {
            let lit = at == selected && !choice.disabled;
            let mut row = Line::from(Span::styled(
                if lit { "› " } else { "  " },
                theme.attention(),
            ));
            if !self.filtering() {
                push(&mut row, format!("{}. ", at + 1), theme.muted(), wide);
            }
            let ink = if choice.disabled {
                theme.faint()
            } else if lit {
                theme.bright()
            } else {
                theme.text()
            };
            push(&mut row, choice.label.clone(), ink, wide);
            // The mark before the detail, so a long detail cut at the
            // edge never takes it.
            if choice.current {
                push(&mut row, " ✓", theme.faint(), wide);
            }
            if !choice.detail.is_empty() {
                push(
                    &mut row,
                    format!(" · {}", choice.detail),
                    theme.faint(),
                    wide,
                );
            }
            lines.push(row);
            rows.push(Some(at));
        }
        if shown.is_empty() {
            let mut none = Line::from(Span::raw("  "));
            push(&mut none, "nothing matches", theme.faint(), wide);
            lines.push(none);
            rows.push(None);
        }
        if let Some(refused) = &self.refused {
            let mut why = Line::from(Span::raw("  "));
            push(&mut why, refused, theme.warning(), wide);
            lines.push(why);
            rows.push(None);
        }
        (lines, rows, cursor)
    }

    /// A click on the `at`th choice shown.
    pub fn click(&mut self, at: usize) -> Pick {
        self.refused = None;
        let shown = self.shown();
        self.reach(shown.get(at))
    }

    /// The flyover in its hairline border, its title on the top edge, at
    /// most `room` columns wide. Returns its lines, which choice each line
    /// shows, and the cursor's (column, line) within them.
    #[allow(clippy::type_complexity)]
    fn panel(
        &self,
        room: usize,
        theme: Theme,
    ) -> (
        Vec<Line<'static>>,
        Vec<Option<usize>>,
        Option<(usize, usize)>,
    ) {
        let (body, rows, cursor) = self.body(theme);
        let inner = crate::panel::content_width(&body)
            .max(text::str_width(&self.title) + 3)
            .max(FLYOVER_MIN)
            .min(room.saturating_sub(4).max(8));
        let lines = crate::panel::bordered(&self.title, body, inner, &[], None, theme);
        let rows = std::iter::once(None).chain(rows).chain([None]).collect();
        (
            lines,
            rows,
            cursor.map(|(col, line)| ((col + 2).min(inner + 1), line + 1)),
        )
    }

    /// The keys while it is open, as (key, action) pairs.
    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match self.shape {
            Shape::Text if self.name_problem().is_some() => vec![("esc", "back")],
            Shape::Text => vec![("enter", "save"), ("esc", "back")],
            Shape::Typed => vec![
                ("enter", "pick"),
                ("tab", "fill"),
                ("↑↓", "move"),
                ("esc", "back"),
            ],
            Shape::List => vec![("enter", "pick"), ("↑↓", "move"), ("esc", "back")],
        }
    }

    pub fn hint(&self) -> String {
        self.hints()
            .iter()
            .map(|(key, action)| format!("{key} {action}"))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// A flyover as drawn: where it is, where its choices are, and where the
/// cursor goes when a field has the keys.
#[derive(Clone, Debug, Default)]
pub struct Flyover {
    pub rect: Rect,
    /// Each choice's screen row and the index a click picks.
    pub choices: Vec<(u16, usize)>,
    pub cursor: Option<Position>,
}

impl Flyover {
    /// The choice at a screen position, when the flyover covers it.
    pub fn choice_at(&self, x: u16, y: u16) -> Option<usize> {
        let inside = (self.rect.x..self.rect.x + self.rect.width).contains(&x);
        self.choices
            .iter()
            .find(|(row, _)| inside && *row == y)
            .map(|(_, at)| *at)
    }

    pub fn covers(&self, x: u16, y: u16) -> bool {
        (self.rect.x..self.rect.x + self.rect.width).contains(&x)
            && (self.rect.y..self.rect.y + self.rect.height).contains(&y)
    }
}

/// Draws `picker` as a flyover whose foot rests on the row above `above`,
/// its left edge under `anchor` (the setting's column) where it fits, inside
/// `area`.
pub fn draw_flyover(
    paint: &mut Paint<'_>,
    picker: &Picker,
    anchor: u16,
    above: u16,
    area: Rect,
    theme: Theme,
) -> Flyover {
    // Two columns of margin either side, like everything else.
    let room = usize::from(area.width).saturating_sub(4);
    let (lines, rows, cursor) = picker.panel(room, theme);
    let Some((rect, skip)) = crate::panel::rise(paint, lines, anchor, above, area) else {
        return Flyover::default();
    };
    Flyover {
        rect,
        choices: rows
            .into_iter()
            .enumerate()
            .skip(skip)
            .filter_map(|(line, at)| at.map(|at| (rect.y + (line - skip) as u16, at)))
            .collect(),
        cursor: cursor
            .filter(|(_, line)| *line >= skip)
            .map(|(col, line)| Position::new(rect.x + col as u16, rect.y + (line - skip) as u16)),
    }
}

/// A row of the form for an agent used in its own terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Name,
    Kind,
    Folder,
    Host,
    Worktree,
}

/// The form's rows, in order.
const FIELDS: [Field; 5] = [
    Field::Name,
    Field::Kind,
    Field::Folder,
    Field::Host,
    Field::Worktree,
];

/// What a click on the form lands on.
#[derive(Clone, Debug, PartialEq)]
pub enum FormHit {
    Field(Field),
    Agent(usize),
    Host(String),
    Folder(String),
    Worktree,
    Start,
    Cancel,
}

/// What a key or click on the form did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormOutcome {
    None,
    Start,
    Close,
}

/// The form that starts an agent to be used in its own terminal, drawn as
/// a modal over home: no prompt and no model, effort or mode, which that
/// terminal sets itself. Every choice sits in the form, changed in place.
/// Esc backs out one level: out of typing, then out of the form.
#[derive(Debug)]
pub struct Form {
    pub field: Field,
    /// A text row (name, folder) has the keys for typing.
    pub typing: bool,
    name: Editor,
    folder: Editor,
}

/// The form's label column.
const LABEL: usize = 10;
/// The widest the modal is drawn, inside its border.
const MODAL_MOST: usize = 68;

impl Form {
    /// It opens typing into the name.
    pub fn new(setup: &Setup) -> Form {
        let mut name = Editor::default();
        name.set(setup.name.as_deref().unwrap_or(""), Vec::new());
        let mut folder = Editor::default();
        folder.set(&text::tilde(&setup.folder), Vec::new());
        Form {
            field: Field::Name,
            typing: true,
            name,
            folder,
        }
    }

    /// The typed name and folder into `setup`, before it starts.
    pub fn apply(&self, setup: &mut Setup) {
        setup.pick(Item::Name, self.name.text());
        setup.pick(Item::Folder, self.folder.text());
    }

    /// Starts the agent, or with no folder yet, has the person type one.
    fn start(&mut self, setup: &mut Setup) -> FormOutcome {
        self.apply(setup);
        // A name the host would refuse is fixed here, not after the prompt.
        if setup
            .name
            .as_deref()
            .is_some_and(|name| wire::agent_name_problem(name).is_some())
        {
            self.field = Field::Name;
            self.typing = true;
            return FormOutcome::None;
        }
        if setup.folder.is_empty() {
            self.field = Field::Folder;
            self.typing = true;
            return FormOutcome::None;
        }
        FormOutcome::Start
    }

    /// Another host: the folder field follows the folder chosen there.
    fn host_changed(&mut self, setup: &mut Setup, fleet: &FleetState) {
        setup.fill_folder(fleet);
        self.folder.set(&text::tilde(&setup.folder), Vec::new());
    }

    fn text_row(field: Field) -> bool {
        matches!(field, Field::Name | Field::Folder)
    }

    /// The folders matching what is typed.
    fn folders(&self, setup: &Setup, fleet: &FleetState) -> Vec<String> {
        let typed = self.folder.text().trim().to_lowercase();
        recent_folders(fleet, &setup.host, &setup.folder)
            .into_iter()
            .map(|folder| text::tilde(&folder))
            .filter(|folder| typed.is_empty() || folder.to_lowercase().contains(&typed))
            .collect()
    }

    fn move_field(&mut self, step: isize) {
        let at = FIELDS.iter().position(|f| *f == self.field).unwrap_or(0) as isize;
        let next = (at + step).clamp(0, FIELDS.len() as isize - 1) as usize;
        self.field = FIELDS[next];
    }

    /// The next choice on a choice row, or the worktree flipped.
    fn change(&mut self, setup: &mut Setup, fleet: &FleetState, step: isize) {
        match self.field {
            Field::Kind => {
                let at = AGENTS.iter().position(|a| *a == setup.agent).unwrap_or(0) as isize;
                let next = (at + step).rem_euclid(AGENTS.len() as isize);
                setup.pick(Item::Kind, &next.to_string());
            }
            Field::Host => {
                let choices = hosts(fleet, setup, false);
                let at = choices.iter().position(|c| c.current).unwrap_or(0);
                let next = Picker::step(&choices, at, step);
                if let Some(choice) = choices.get(next) {
                    setup.pick(Item::Host, &choice.value);
                    self.host_changed(setup, fleet);
                }
            }
            Field::Worktree => setup.pick(Item::Worktree, ""),
            Field::Name | Field::Folder => {}
        }
    }

    pub fn key(&mut self, setup: &mut Setup, fleet: &FleetState, key: KeyEvent) -> FormOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.typing {
            match key.code {
                KeyCode::Esc => self.typing = false,
                KeyCode::Enter => {
                    self.typing = false;
                    self.move_field(1);
                }
                KeyCode::Up | KeyCode::BackTab => {
                    self.typing = false;
                    self.move_field(-1);
                }
                KeyCode::Down => {
                    self.typing = false;
                    self.move_field(1);
                }
                // The folder's best match fills it; again, it moves on.
                KeyCode::Tab => {
                    let top = (self.field == Field::Folder)
                        .then(|| self.folders(setup, fleet).into_iter().next())
                        .flatten()
                        .filter(|top| top != self.folder.text().trim());
                    match top {
                        Some(top) => self.folder.set(&top, Vec::new()),
                        None => {
                            self.typing = false;
                            self.move_field(1);
                        }
                    }
                }
                _ if self.field == Field::Name => {
                    self.name.key(key);
                }
                _ => {
                    self.folder.key(key);
                }
            }
            return FormOutcome::None;
        }
        match key.code {
            KeyCode::Esc => return FormOutcome::Close,
            KeyCode::Up | KeyCode::BackTab => self.move_field(-1),
            KeyCode::Down | KeyCode::Tab => self.move_field(1),
            KeyCode::Char('k') if !ctrl => self.move_field(-1),
            KeyCode::Char('j') if !ctrl => self.move_field(1),
            KeyCode::Left => self.change(setup, fleet, -1),
            KeyCode::Right => self.change(setup, fleet, 1),
            KeyCode::Char('h') if !ctrl => self.change(setup, fleet, -1),
            KeyCode::Char('l') if !ctrl => self.change(setup, fleet, 1),
            KeyCode::Char(' ') if self.field == Field::Worktree => setup.pick(Item::Worktree, ""),
            KeyCode::Enter if Self::text_row(self.field) => self.typing = true,
            KeyCode::Enter => return self.start(setup),
            _ => {}
        }
        FormOutcome::None
    }

    /// A click on the form.
    pub fn click(&mut self, setup: &mut Setup, fleet: &FleetState, hit: FormHit) -> FormOutcome {
        match hit {
            FormHit::Field(field) => {
                self.field = field;
                self.typing = Self::text_row(field);
            }
            FormHit::Agent(at) => {
                self.field = Field::Kind;
                self.typing = false;
                setup.pick(Item::Kind, &at.to_string());
            }
            FormHit::Host(value) => {
                self.field = Field::Host;
                self.typing = false;
                setup.pick(Item::Host, &value);
                self.host_changed(setup, fleet);
            }
            FormHit::Folder(folder) => {
                self.field = Field::Folder;
                self.folder.set(&folder, Vec::new());
            }
            FormHit::Worktree => {
                self.field = Field::Worktree;
                self.typing = false;
                setup.pick(Item::Worktree, "");
            }
            FormHit::Start => return self.start(setup),
            FormHit::Cancel => return FormOutcome::Close,
        }
        FormOutcome::None
    }

    /// The keys for where the form is, as (key, action) pairs.
    fn legend(&self) -> Vec<(&'static str, &'static str)> {
        if self.typing {
            let mut keys = vec![("enter", "next")];
            if self.field == Field::Folder {
                keys.push(("tab", "fill"));
            }
            keys.push(("esc", "done"));
            return keys;
        }
        let mut keys = match self.field {
            Field::Name | Field::Folder => vec![("enter", "edit")],
            Field::Kind | Field::Host => vec![("enter", "start"), ("←→", "choose")],
            Field::Worktree => vec![("enter", "start"), ("space", "toggle")],
        };
        keys.push(("↑↓", "move"));
        keys.push(("esc", "cancel"));
        keys
    }

    /// The modal, at most `room` columns wide: its lines, each line's click
    /// targets as (line, from, to, hit) in the modal's own columns, and the
    /// cursor's (column, line) while a text row is typed into. `error` is
    /// why the last start failed, said under the rows.
    #[allow(clippy::type_complexity)]
    pub fn modal(
        &self,
        setup: &Setup,
        fleet: &FleetState,
        error: Option<&str>,
        room: usize,
        theme: Theme,
    ) -> (
        Vec<Line<'static>>,
        Vec<(usize, usize, usize, FormHit)>,
        Option<(usize, usize)>,
    ) {
        let inner = room.saturating_sub(4).min(MODAL_MOST);
        let wide = inner;
        let edge = theme.hairline();
        let mut rows: Vec<(Line<'static>, Vec<(usize, usize, FormHit)>, bool)> = Vec::new();
        let mut cursor = None;
        rows.push((Line::default(), Vec::new(), false));
        for field in FIELDS {
            let current = self.field == field;
            let mut line = Line::from(Span::raw(" "));
            let label = match field {
                Field::Name => "Name",
                Field::Kind => "Agent",
                Field::Folder => "Folder",
                Field::Host => "Host",
                Field::Worktree => "Worktree",
            };
            let label_ink = if current {
                theme.bright()
            } else {
                theme.muted()
            };
            push(&mut line, format!("{label:<LABEL$}"), label_ink, wide);
            let mut spots = vec![(0, text::line_width(&line), FormHit::Field(field))];
            // A choice as a chip: the chosen one on the highlight surface (a
            // step further on the current row), the rest plain.
            let mut chip =
                |line: &mut Line<'static>, words: String, on: bool, off: bool, hit: FormHit| {
                    let from = text::line_width(line);
                    let style = if off {
                        theme.faint()
                    } else if on && current {
                        theme.chip_raised()
                    } else if on {
                        theme.chip()
                    } else {
                        theme.text()
                    };
                    push(line, format!(" {words} "), style, wide);
                    if !off {
                        spots.push((from, text::line_width(line), hit));
                    }
                    push(line, " ", theme.text(), wide);
                };
            match field {
                Field::Name | Field::Folder => {
                    let editor = if field == Field::Name {
                        &self.name
                    } else {
                        &self.folder
                    };
                    let at = text::line_width(&line);
                    if editor.is_empty() {
                        let hint = if field == Field::Name {
                            "named automatically"
                        } else {
                            "a folder on that host"
                        };
                        push(&mut line, hint, theme.faint(), wide);
                    } else {
                        push(&mut line, editor.text(), theme.text(), wide);
                        if field == Field::Name {
                            name_problem(&mut line, editor.text(), theme);
                        }
                    }
                    if current && self.typing {
                        cursor = Some((at + editor.cursor_chars(), rows.len()));
                    }
                }
                Field::Kind => {
                    for (at, agent) in AGENTS.iter().enumerate() {
                        let mut words = agent.name().to_owned();
                        let why = not_signed_in(fleet, &setup.host, *agent);
                        if !why.is_empty() {
                            words.push_str(&format!(" · {why}"));
                        }
                        chip(
                            &mut line,
                            words,
                            *agent == setup.agent,
                            false,
                            FormHit::Agent(at),
                        );
                    }
                }
                Field::Host => {
                    for host in hosts(fleet, setup, false) {
                        let words = if host.detail.is_empty() {
                            host.label.clone()
                        } else {
                            format!("{} · {}", host.label, host.detail)
                        };
                        chip(
                            &mut line,
                            words,
                            host.current,
                            host.disabled,
                            FormHit::Host(host.value.clone()),
                        );
                    }
                }
                Field::Worktree => {
                    let from = text::line_width(&line);
                    let mark = if setup.worktree { "[✓] " } else { "[ ] " };
                    push(&mut line, mark, theme.text(), wide);
                    push(&mut line, "new worktree", theme.text(), wide);
                    spots.push((from, text::line_width(&line), FormHit::Worktree));
                }
            }
            rows.push((line, spots, current));
            // Typing a folder: the folders it could be, under it.
            if field == Field::Folder && current && self.typing {
                let folders = self.folders(setup, fleet);
                let mut more = Line::from(Span::raw(" ".repeat(1 + LABEL)));
                let mut spots = Vec::new();
                for (i, folder) in folders.iter().take(4).enumerate() {
                    if i > 0 {
                        push(&mut more, " · ", theme.faint(), wide);
                    }
                    let from = text::line_width(&more);
                    let ink = if i == 0 { theme.muted() } else { theme.faint() };
                    push(&mut more, folder.clone(), ink, wide);
                    spots.push((
                        from,
                        text::line_width(&more),
                        FormHit::Folder(folder.clone()),
                    ));
                }
                if !folders.is_empty() {
                    rows.push((more, spots, false));
                }
            }
        }
        if let Some(error) = error {
            rows.push((Line::default(), Vec::new(), false));
            let words = format!("Could not start the agent: {error}");
            for words in text::wrap(&words, wide.saturating_sub(2)) {
                let mut line = Line::from(Span::raw(" "));
                push(&mut line, words, theme.warning(), wide);
                rows.push((line, Vec::new(), false));
            }
        }
        rows.push((Line::default(), Vec::new(), false));
        let mut buttons = Line::from(Span::raw(" "));
        let mut spots = Vec::new();
        for (words, hit) in [("[Start]", FormHit::Start), ("[Cancel]", FormHit::Cancel)] {
            let from = text::line_width(&buttons);
            push(&mut buttons, words, theme.text(), wide);
            spots.push((from, text::line_width(&buttons), hit));
            push(&mut buttons, "  ", theme.text(), wide);
        }
        rows.push((buttons, spots, false));
        let mut legend = Line::from(Span::raw(" "));
        for (i, (keys, action)) in self.legend().into_iter().enumerate() {
            if i > 0 {
                push(&mut legend, "   ", theme.faint(), wide);
            }
            push(&mut legend, keys, theme.muted(), wide);
            push(&mut legend, format!(" {action}"), theme.faint(), wide);
        }
        rows.push((legend, Vec::new(), false));

        // In its hairline border, the title on the top edge; the current row
        // on the flat highlight, edge to edge inside the border.
        let mut lines = Vec::new();
        let mut hits = Vec::new();
        let mut top = Line::from(Span::styled("╭─ ", edge));
        push(&mut top, "New Agent", theme.muted(), inner + 4);
        push(&mut top, " ", edge, inner + 4);
        let used = text::line_width(&top);
        push(
            &mut top,
            "─".repeat((inner + 3).saturating_sub(used)),
            edge,
            inner + 4,
        );
        push(&mut top, "╮", edge, inner + 4);
        lines.push(top);
        let highlight = theme.row_surface();
        for (at, (line, spots, current)) in rows.into_iter().enumerate() {
            let mut body = Line::default();
            for span in line.spans {
                push(&mut body, &span.content, span.style, inner);
            }
            text::pad_to(&mut body, inner);
            if current && let Some(surface) = highlight {
                for span in &mut body.spans {
                    if span.style.bg.is_none() {
                        span.style = span.style.patch(surface);
                    }
                }
            }
            let mut row = Line::from(Span::styled("│ ", edge));
            row.spans.extend(body.spans);
            push(&mut row, " │", edge, inner + 4);
            lines.push(row);
            for (from, to, hit) in spots {
                hits.push((1 + at, 2 + from, 2 + to.min(inner), hit));
            }
        }
        let mut bottom = Line::from(Span::styled("╰", edge));
        push(&mut bottom, "─".repeat(inner + 2), edge, inner + 4);
        push(&mut bottom, "╯", edge, inner + 4);
        lines.push(bottom);
        (
            lines,
            hits,
            cursor.map(|(col, line)| ((2 + col).min(inner + 1), 1 + line)),
        )
    }
}

/// After a typed agent name, why the host will refuse it, if it will.
fn name_problem(line: &mut Line<'static>, typed: &str, theme: Theme) {
    if let Some(problem) = wire::agent_name_problem(typed.trim()) {
        line.spans.push(Span::styled(" · ", theme.muted()));
        line.spans.push(Span::styled(
            crate::words::name_problem(problem),
            theme.warning(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{host_of_value, host_value};

    /// A host id survives a choice's value whatever its bytes: ids are
    /// rarely valid text.
    #[test]
    fn a_host_id_round_trips_through_a_choice() {
        let id = [
            0x9f, 0x61, 0xc7, 0x4c, 0x13, 0xf4, 0x46, 0xbb, 0x97, 0x2d, 0x66, 0xb0, 0x8c, 0x84,
            0x4f, 0x44,
        ];
        assert_eq!(host_of_value(&host_value(&id)), Some(id.to_vec()));
        assert_eq!(host_of_value("zz"), None);
        assert_eq!(host_of_value("abc"), None);
    }
}
