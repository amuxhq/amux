//! The terminal client's model: the fleet driver, at most one open chat,
//! the views' state, and the keys that move between them. Effects run as
//! tasks against the drivers and report back as [`AppEvent`]s, so the
//! loop never waits on the network with a key in hand.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use client::{Client, SystemClock};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use prost::Message as _;
use ratatui::Frame as Paint;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;
use ui_runtime::{Fleet, InputError, Session, Window, inputs};
use ui_state::{AgentKey, Composer, InputWhat, PhaseView, SessionState};
use ui_view::family_header;
use wire::{Agent, Attachment, DeleteAgentRequest, Diff, RenameAgentRequest};

use crate::chat::layout::{CAP, PAGE};
use crate::chat::{ChatEffect, ChatView};
use crate::clipboard::read_clipboard;
use crate::fleet::{FleetEffect, FleetView};
use crate::text::{self, push};
use crate::theme::Theme;

/// How long a second Ctrl+C has to arrive to quit.
const QUIT_WINDOW: Duration = Duration::from_secs(3);
/// How long a notice stays in the footer.
const NOTICE_FOR: Duration = Duration::from_secs(5);
/// How long the leader waits before its panel shows: typed quickly, a
/// chord never draws it.
const PANEL_AFTER: Duration = Duration::from_millis(400);

/// What the terminal client is configured with at the CLI edge; the TUI
/// itself reads no environment.
#[derive(Clone, Debug)]
pub struct TuiConfig {
    /// Where agents created from the fleet work.
    pub working_dir: std::path::PathBuf,
    /// The leader's letter: `a` makes Ctrl+A the leader.
    pub leader: char,
    pub theme: Theme,
    /// A chat to open once its inventory row arrives.
    pub initial_chat: Option<Vec<u8>>,
    /// Whether the embedding CLI can hand the terminal to an agent's own
    /// interface.
    pub attach: bool,
    /// This build's version, compared with the daemon's.
    pub version: String,
    /// The profile's host on this machine: only its agents' terminals are
    /// here to attach to.
    pub local_host: Vec<u8>,
    /// Where this client keeps its layout between runs; None keeps it for
    /// this run only.
    pub layout: Option<std::path::PathBuf>,
    /// Where the person chats with their agents: in amux, or in each
    /// agent's own terminal, attached on start.
    pub chat_in: settings::ChatInSetting,
    /// What each agent starts with: its model, effort and mode.
    pub defaults: settings::NewAgentSettings,
}

/// How this client lays its screens out, kept between runs: a person's
/// preference, shared by every terminal on the installation and by no other
/// kind of client.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Layout {
    /// The chat's overview pane stays open across chats until closed.
    #[serde(default)]
    pub overview: bool,
    /// What a chat's changes are counted against.
    #[serde(default)]
    pub comparison: crate::chat::Comparison,
    /// Each chat that differs from the defaults, by agent id in hex. Only
    /// the [`CHATS_KEPT`] most recently changed are kept: an agent deleted
    /// elsewhere, or on a host this client cannot see now, is never named
    /// to it, so the file is bounded by age instead.
    #[serde(default)]
    pub chats: std::collections::BTreeMap<String, ChatLayout>,
}

/// How many chats' layouts the file keeps.
pub const CHATS_KEPT: usize = 200;

/// One chat's layout.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChatLayout {
    /// Its folded overview sections.
    #[serde(default)]
    pub folds: Vec<String>,
    /// How it draws its tool steps.
    #[serde(default)]
    pub tools: crate::chat::ToolSteps,
    /// When it last changed, in Unix milliseconds.
    pub changed_ms: i64,
}

impl Layout {
    fn chat(&self, agent_id: &[u8]) -> Option<&ChatLayout> {
        self.chats.get(&hex(agent_id))
    }

    fn folds_of(&self, agent_id: &[u8]) -> HashSet<crate::chat::pane::Section> {
        self.chat(agent_id)
            .into_iter()
            .flat_map(|chat| &chat.folds)
            .filter_map(|key| crate::chat::pane::Section::from_key(key))
            .collect()
    }

    pub(crate) fn tools_of(&self, agent_id: &[u8]) -> crate::chat::ToolSteps {
        self.chat(agent_id)
            .map(|chat| chat.tools)
            .unwrap_or_default()
    }

    /// Changes one chat's layout; returns whether it changed. A chat back
    /// at the defaults is no longer named.
    fn change(&mut self, agent_id: &[u8], change: impl FnOnce(&mut ChatLayout)) -> bool {
        let id = hex(agent_id);
        let before = self.chats.get(&id).cloned().unwrap_or_default();
        let mut after = before.clone();
        change(&mut after);
        if after.folds == before.folds && after.tools == before.tools {
            return false;
        }
        if after.folds.is_empty() && after.tools == crate::chat::ToolSteps::default() {
            self.chats.remove(&id);
        } else {
            after.changed_ms = now_ms();
            self.chats.insert(id, after);
        }
        true
    }

    /// Records a chat's folds; returns whether they changed.
    fn set_folds(&mut self, agent_id: &[u8], folds: &HashSet<crate::chat::pane::Section>) -> bool {
        let mut keys: Vec<String> = folds.iter().map(|s| s.key().to_owned()).collect();
        keys.sort();
        self.change(agent_id, |chat| chat.folds = keys)
    }

    /// Records how a chat draws its tool steps; returns whether that
    /// changed.
    pub(crate) fn set_tools(&mut self, agent_id: &[u8], tools: crate::chat::ToolSteps) -> bool {
        self.change(agent_id, |chat| chat.tools = tools)
    }

    pub(crate) fn load(path: Option<&std::path::Path>) -> Layout {
        path.and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Best effort: a layout that cannot be written is only forgotten.
    /// Written whole through a file of this process's own, so two terminals
    /// saving at once leave one of their layouts, never a mix.
    pub(crate) fn save(&mut self, path: Option<&std::path::Path>) {
        let Some(path) = path else {
            return;
        };
        if self.chats.len() > CHATS_KEPT {
            let mut ages: Vec<i64> = self.chats.values().map(|chat| chat.changed_ms).collect();
            ages.sort_unstable_by(|a, b| b.cmp(a));
            let oldest_kept = ages[CHATS_KEPT - 1];
            self.chats.retain(|_, chat| chat.changed_ms >= oldest_kept);
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let Ok(bytes) = serde_json::to_vec_pretty(self) else {
            return;
        };
        let mut temp = path.as_os_str().to_owned();
        temp.push(format!(".{}.tmp", std::process::id()));
        if std::fs::write(&temp, bytes).is_err() || std::fs::rename(&temp, path).is_err() {
            let _ = std::fs::remove_file(&temp);
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Info,
    Warn,
}

/// What a finished task reports.
pub enum AppEvent {
    Opened {
        agent: AgentKey,
        result: Result<Arc<Session>, String>,
        /// The draft this machine kept for the agent, from an earlier
        /// client.
        kept: Option<wire::Draft>,
    },
    Notice(String, Tone),
    /// Put words back in the composer of this agent's chat, saying why
    /// when there is something to say.
    Restore {
        agent_id: Vec<u8>,
        text: String,
        attachments: Vec<Attachment>,
        notice: Option<String>,
    },
    Attached {
        agent_id: Vec<u8>,
        blob: wire::BlobRef,
    },
    /// A file could not be stored for the draft.
    AttachFailed {
        agent_id: Vec<u8>,
        name: String,
        why: String,
    },
    /// An agent created with its first prompt: its chat opens, or home
    /// stays and says it started.
    Started {
        agent: Agent,
        open: bool,
        /// Used in its own terminal: attach to it rather than open its chat.
        attach: bool,
    },
    /// Creating an agent from home's draft failed; the draft is kept.
    StartFailed(String),
    /// A working-tree diff frozen for this agent's review page.
    Review {
        agent_id: Vec<u8>,
        diff: Diff,
        patch: String,
        /// The file to open the page at.
        at: Option<String>,
    },
    /// The changed files of one comparison, for the chat's overview.
    Changes {
        agent_id: Vec<u8>,
        key: crate::chat::FetchKey,
        diff: Diff,
    },
    /// A save of an agent's held prompts to the draft this machine keeps
    /// ended: kept when it landed, to be made again when it did not.
    HeldSaved {
        agent: AgentKey,
        ids: Vec<ui_state::InputId>,
        saved: bool,
    },
    /// What a host offers for a provider, for a new agent there.
    HostOffer {
        host: Vec<u8>,
        provider: wire::Provider,
        hash: Vec<u8>,
        catalogue: wire::Catalogue,
    },
}

/// A host, a provider, and the catalogue hash the host named when asked.
type OfferAsked = (Vec<u8>, wire::Provider, Option<Vec<u8>>);

/// What the loop does after a key.
#[derive(Debug, PartialEq)]
pub enum Flow {
    Continue,
    Quit,
    /// Hand the terminal to this agent's own interface.
    Attach(Box<Agent>),
}

pub struct OpenChat {
    pub session: Arc<Session>,
    pub view: ChatView,
    pub agent: AgentKey,
}

impl OpenChat {
    /// Tells the session when the reader left or returned to the newest row.
    fn tell_following(&mut self) {
        if let Some(following) = self.view.following_moved() {
            self.session.follow(following);
        }
    }
}

pub struct App {
    client: Arc<dyn Client>,
    pub fleet: Arc<Fleet>,
    pub fleet_view: FleetView,
    pub chat: Option<OpenChat>,
    pub config: TuiConfig,
    leader_pending: bool,
    /// When the leader was pressed, for its panel's pause.
    leader_since: Option<Instant>,
    quit_armed: Option<Instant>,
    help: bool,
    /// The keys reference's first line on screen.
    help_scroll: usize,
    notice: Option<(String, Tone, Instant)>,
    opening: Option<AgentKey>,
    layout: Layout,
    /// A report asked for: the next frame drawn is the one it freezes.
    report_asked: bool,
    report: Option<crate::report::Report>,
    /// What hosts offer per provider, by hash, as asked for new agents.
    host_offers: HashMap<Vec<u8>, wire::Catalogue>,
    /// Each host's provider asked once per hash the host named.
    offers_asked: HashSet<OfferAsked>,
    /// Drafts left in chats that are not open, by agent: a half-written
    /// prompt is the person's work, and it waits in the composer for the
    /// chat's return, as on the phone. An emptied draft stays as an empty
    /// one: it outranks the daemon's copy, which the save may not have
    /// emptied yet, and the save as the client quits sends it again.
    drafts: HashMap<AgentKey, crate::editor::Editor>,
    /// By agent, the prompts held for an unreachable host that the draft
    /// this machine keeps already carries, by input id: a held prompt goes
    /// back to the draft if the client closes first, and a client killed
    /// without quitting has no chance to put it there then.
    held_kept: HashMap<AgentKey, Vec<ui_state::InputId>>,
    /// Saves of held prompts on their way to this machine's daemon, by
    /// agent; one that fails leaves here unkept and is made again.
    held_saving: HashMap<AgentKey, Vec<ui_state::InputId>>,
    /// By agent, the prompts last seen on their way back to its draft:
    /// held for an unreachable host, resent from held and not yet answered,
    /// or refused while its chat was closed.
    /// When the fleet drops the session holding them (its agent exited or
    /// left), their words go into the agent's draft here.
    returning: HashMap<AgentKey, Vec<(ui_state::InputId, String, Vec<Attachment>)>>,
    /// A chat closed under the person: home takes no key until
    /// [`crate::chat::ASK_ARMS_MS`] pass with none, as an ask arms, so the
    /// rest of what they were typing never acts on another agent. The last
    /// key's time, or the close's.
    home_held: Option<i64>,
    events: mpsc::UnboundedSender<AppEvent>,
    pub receiver: mpsc::UnboundedReceiver<AppEvent>,
}

fn now_ms() -> i64 {
    use client::Clock as _;
    SystemClock.now_ms()
}

/// Why an agent's own terminal cannot be attached from here: raw attach
/// reads the agent's directory on this machine, headless Claude has no
/// terminal at all, and a stopped agent's terminal is gone until it is
/// resumed. Its chat is the way in either way.
pub(crate) fn terminal_refusal(agent: &Agent, config: &TuiConfig) -> Option<&'static str> {
    terminal_elsewhere(agent, config)
        .or_else(|| (agent.lifecycle() == wire::Lifecycle::Exited).then_some("it is stopped"))
}

/// Why `agent` has no terminal on this machine at all, running or not.
fn terminal_elsewhere(agent: &Agent, config: &TuiConfig) -> Option<&'static str> {
    if agent.host_id != config.local_host {
        Some("its terminal is on another machine")
    } else if agent.kind() == wire::Kind::ClaudeSdk {
        Some("headless Claude has no terminal")
    } else {
        None
    }
}

/// A composer's words and attachments, as the daemon keeps them.
fn composer_draft(editor: &crate::editor::Editor) -> wire::Draft {
    wire::Draft {
        text: editor.text().to_owned(),
        attachments: editor.attachments().to_vec(),
    }
}

/// The prompts `state` holds for an unreachable host: id, words and
/// attachments.
fn held_prompts(state: &SessionState) -> Vec<(ui_state::InputId, String, Vec<Attachment>)> {
    returning_prompts(state, false)
}

/// The prompts of `state` whose words go back to the agent's draft: those
/// held for an unreachable host, and, for a chat that is not open, those
/// the agent refused, which come back into the composer as it opens.
fn returning_prompts(
    state: &SessionState,
    refused: bool,
) -> Vec<(ui_state::InputId, String, Vec<Attachment>)> {
    state
        .inputs()
        .iter()
        .filter(|sent| match sent.state {
            ui_state::InputState::Held => true,
            ui_state::InputState::Rejected(_) => refused,
            _ => false,
        })
        .filter_map(|sent| match &sent.what {
            InputWhat::Prompt { text, attachments } => {
                Some((sent.id.clone(), text.clone(), attachments.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Saves drafts with this machine's daemon; a failure loses only what a
/// client kept in memory anyway.
async fn save_drafts(client: Arc<dyn Client>, drafts: Vec<(Vec<u8>, wire::Draft)>) {
    for (agent_id, draft) in drafts {
        let request = wire::UpdateDraftRequest {
            agent_id,
            draft: Some(draft),
        };
        // Nowhere to say it as the client closes; the draft was only ever
        // in this client's memory before.
        let _ = client.update_draft(request).await;
    }
}

/// Why the open chat can no longer be served.
struct ChatGone {
    /// In the person's terms.
    why: String,
    /// Its agent was deleted, rather than its host unlinked or its session
    /// ended.
    deleted: bool,
}

impl Drop for App {
    /// A client dropped without quitting (a test, an embedding) still
    /// hands its drafts on, where a runtime is there to send them.
    fn drop(&mut self) {
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(self.keep_drafts());
        }
    }
}

fn plain(error: InputError) -> String {
    match error {
        InputError::Rejected(reason) => {
            format!("not sent: {}", crate::chat::refusal_words(&reason))
        }
        InputError::Uncertain | InputError::Held => error.to_string(),
    }
}

impl App {
    pub fn new(client: Arc<dyn Client>, fleet: Fleet, config: TuiConfig) -> App {
        let (events, receiver) = mpsc::unbounded_channel();
        let fleet_view = FleetView {
            attach: config.attach,
            version: config.version.clone(),
            local_host: config.local_host.clone(),
            working_dir: config.working_dir.to_string_lossy().into_owned(),
            chat_in: config.chat_in,
            defaults: config.defaults.clone(),
            ..FleetView::default()
        };
        let layout = Layout::load(config.layout.as_deref());
        App {
            layout,
            client,
            fleet: Arc::new(fleet),
            fleet_view,
            chat: None,
            config,
            leader_pending: false,
            leader_since: None,
            quit_armed: None,
            help: false,
            help_scroll: 0,
            notice: None,
            opening: None,
            report_asked: false,
            report: None,
            host_offers: HashMap::new(),
            drafts: HashMap::new(),
            held_kept: HashMap::new(),
            held_saving: HashMap::new(),
            returning: HashMap::new(),
            home_held: None,
            offers_asked: HashSet::new(),
            events,
            receiver,
        }
    }

    fn theme(&self) -> Theme {
        self.config.theme
    }

    pub fn notice(&mut self, words: impl Into<String>, tone: Tone) {
        self.notice = Some((words.into(), tone, Instant::now()));
    }

    fn spawn<F>(&self, task: F)
    where
        F: Future<Output = Option<AppEvent>> + Send + 'static,
    {
        let events = self.events.clone();
        tokio::spawn(async move {
            if let Some(event) = task.await {
                let _ = events.send(event);
            }
        });
    }

    /// When the loop should wake with nothing else happening: the activity
    /// line's second, the quit guard's end, a notice's end.
    pub fn next_tick(&self) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        let mut at = |instant: Instant| {
            next = Some(next.map_or(instant, |n| n.min(instant)));
        };
        if let Some(armed) = self.quit_armed {
            at(armed + QUIT_WINDOW);
        }
        if self.leader_pending
            && let Some(since) = self.leader_since
            && since.elapsed() < PANEL_AFTER
        {
            at(since + PANEL_AFTER);
        }
        if let Some((_, _, since)) = &self.notice {
            at(*since + NOTICE_FOR);
        }
        if let Some(chat) = &self.chat {
            let state = chat.session.state();
            if state.phase() == PhaseView::Working || state.transcript().is_empty() {
                at(Instant::now() + Duration::from_millis(250));
            }
        }
        // Code drawn plain while the grammars load is drawn again once
        // they arrive.
        if crate::highlight::loading() {
            at(Instant::now() + Duration::from_millis(50));
        }
        next
    }

    /// Drops what has expired; true when that changed the screen.
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        if self
            .quit_armed
            .is_some_and(|armed| armed.elapsed() >= QUIT_WINDOW)
        {
            self.quit_armed = None;
            changed = true;
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, _, since)| since.elapsed() >= NOTICE_FOR)
        {
            self.notice = None;
            changed = true;
        }
        changed || self.chat.is_some()
    }

    /// Opens the configured first chat once its row is in the fleet. The
    /// fleet keeps the open chat's entry and host current itself.
    pub fn fleet_changed(&mut self) {
        if let Some(id) = self.config.initial_chat.clone() {
            let agent = self.fleet.state().find(&id).map(ui_state::agent_key);
            if let Some(agent) = agent {
                self.config.initial_chat = None;
                self.open(agent);
            }
        }
    }

    pub fn event(&mut self, event: AppEvent) -> Flow {
        match event {
            AppEvent::HeldSaved { agent, ids, saved } => {
                if self.held_saving.get(&agent) == Some(&ids) {
                    self.held_saving.remove(&agent);
                }
                if saved && ids.is_empty() {
                    self.held_kept.remove(&agent);
                } else if saved {
                    self.held_kept.insert(agent, ids);
                }
            }
            AppEvent::Opened {
                agent,
                result,
                kept,
            } => {
                if self.opening.as_ref() != Some(&agent) {
                    if result.is_ok() {
                        self.fleet.close(&agent);
                    }
                    return Flow::Continue;
                }
                self.opening = None;
                match result {
                    Ok(session) => {
                        if let Some(open) = self.chat.take() {
                            self.fleet.close(&open.agent);
                            self.keep_draft(open);
                        }
                        let terminal = self.fleet.state().agent(&agent).is_some_and(|entry| {
                            self.config.attach && terminal_elsewhere(entry, &self.config).is_none()
                        });
                        let mut view = ChatView::new(agent.agent.clone(), now_ms(), terminal);
                        match (self.drafts.remove(&agent), kept) {
                            (Some(draft), _) => view.editor = draft,
                            // A draft this machine kept from an earlier
                            // client comes back from its daemon.
                            (None, Some(kept)) => view.editor.set(&kept.text, kept.attachments),
                            (None, None) => {}
                        }
                        view.leader = self.config.leader;
                        view.local = agent.host == self.config.local_host;
                        // The overview opens where it was left; the
                        // composer has the keys either way.
                        view.pane_open = self.layout.overview;
                        view.pane_folds = self.layout.folds_of(&agent.agent);
                        view.comparison = self.layout.comparison;
                        view.tools = self.layout.tools_of(&agent.agent);
                        self.fleet_view.select(agent.clone());
                        self.chat = Some(OpenChat {
                            session,
                            view,
                            agent,
                        });
                    }
                    Err(error) => {
                        self.notice(format!("could not open the chat: {error}"), Tone::Warn)
                    }
                }
            }
            AppEvent::Notice(words, tone) => self.notice(words, tone),
            AppEvent::Restore {
                agent_id,
                text,
                attachments,
                notice,
            } => {
                if let Some(notice) = notice {
                    self.notice(notice, Tone::Warn);
                }
                if let Some(chat) = self
                    .chat
                    .as_mut()
                    .filter(|chat| chat.view.agent_id == agent_id)
                {
                    chat.view.editor.restore(&text, attachments);
                }
            }
            AppEvent::Attached { agent_id, blob } => {
                let effects = match self
                    .chat
                    .as_mut()
                    .filter(|chat| chat.view.agent_id == agent_id)
                {
                    Some(chat) => {
                        let state = chat.session.state();
                        chat.view.attach_blob(&state, blob)
                    }
                    None => Vec::new(),
                };
                // A send that waited for the file goes now.
                for effect in effects {
                    self.chat_effect(effect);
                }
            }
            AppEvent::AttachFailed {
                agent_id,
                name,
                why,
            } => {
                if let Some(chat) = self
                    .chat
                    .as_mut()
                    .filter(|chat| chat.view.agent_id == agent_id)
                {
                    chat.view.attach_failed(&name, &why);
                }
                self.notice(format!("could not attach {name}: {why}"), Tone::Warn);
            }
            AppEvent::Review {
                agent_id,
                diff,
                patch,
                at,
            } => {
                if let Some(chat) = self
                    .chat
                    .as_mut()
                    .filter(|chat| chat.view.agent_id == agent_id)
                {
                    chat.view.open_review_at(diff, patch, at.as_deref());
                    // The page answers "reading the changes…".
                    self.notice = None;
                }
            }
            AppEvent::Changes {
                agent_id,
                key,
                diff,
            } => {
                if let Some(chat) = self
                    .chat
                    .as_mut()
                    .filter(|chat| chat.view.agent_id == agent_id)
                {
                    chat.view.changes_fetched(key, diff);
                }
            }
            AppEvent::HostOffer {
                host,
                provider,
                hash,
                catalogue,
            } => {
                if let Some(setup) = self.fleet_view.open_setup() {
                    setup.offer(&host, provider, &hash, &catalogue);
                }
                self.host_offers.insert(hash, catalogue);
            }
            AppEvent::Started {
                agent,
                open,
                attach,
            } => {
                self.fleet_view.home.started();
                let key = ui_state::agent_key(&agent);
                self.fleet_view.select(key.clone());
                if attach {
                    // Straight into its own terminal; where it has none
                    // here, its chat opens and says why.
                    let refusal = terminal_refusal(&agent, &self.config);
                    if refusal.is_none() && self.config.attach {
                        return Flow::Attach(Box::new(agent));
                    }
                    let why = match refusal {
                        Some(why) => format!("{why}; its chat opens instead"),
                        None => "its own terminal would open here, but this amux cannot hand the terminal over; its chat opens instead".to_owned(),
                    };
                    self.notice(why, Tone::Warn);
                    self.config.initial_chat = Some(agent.agent_id.clone());
                    self.fleet_changed();
                } else if open {
                    // The inventory may not list it yet; the chat opens
                    // once it does.
                    self.config.initial_chat = Some(agent.agent_id.clone());
                    self.fleet_changed();
                } else {
                    self.notice(format!("started {}", agent.name), Tone::Info);
                }
            }
            AppEvent::StartFailed(error) => {
                if !self.fleet_view.home.start_failed(error.clone()) {
                    self.notice(format!("could not start the agent: {error}"), Tone::Warn);
                }
            }
        }
        Flow::Continue
    }

    fn open(&mut self, agent: AgentKey) {
        if self.fleet.state().agent(&agent).is_none() {
            return;
        }
        self.opening = Some(agent.clone());
        let fleet = self.fleet.clone();
        let client = self.client.clone();
        self.spawn(async move {
            let window = Window {
                tail: PAGE,
                cap: CAP,
            };
            let result = fleet
                .open(&agent, window)
                .await
                .map_err(|error| error.to_string());
            let request = wire::GetDraftRequest {
                agent_id: agent.agent.clone(),
            };
            let kept = client
                .get_draft(request)
                .await
                .ok()
                .filter(|draft| !draft.text.is_empty() || !draft.attachments.is_empty());
            Some(AppEvent::Opened {
                agent,
                result,
                kept,
            })
        });
    }

    /// Back to the fleet: the chat's session closes; the fleet is home.
    pub fn close_chat(&mut self) {
        if let Some(chat) = self.chat.take() {
            self.fleet.close(&chat.agent);
            self.fleet_view.select(chat.agent.clone());
            self.keep_draft(chat);
        }
    }

    /// Back to the fleet from a chat whose agent was deleted: its draft
    /// goes with it, kept neither here nor with the daemon.
    fn close_deleted_chat(&mut self) {
        if let Some(chat) = self.chat.take() {
            self.fleet.close(&chat.agent);
            self.drafts.remove(&chat.agent);
        }
    }

    /// Keeps a closing chat's draft for when it opens again, here and with
    /// this machine's daemon, so it outlives this client. An empty one is
    /// kept too, so the chat reopens empty and the quit's save, which is
    /// waited for, empties the daemon's copy even if this save has not.
    fn keep_draft(&mut self, chat: OpenChat) {
        // Refused prompts its composer took back are in the editor now:
        // forgotten, so they neither come back again nor count as words
        // still on their way back to the draft.
        let taken: Vec<ui_state::InputId> = ui_view::refused_prompts(&chat.session.state())
            .into_iter()
            .map(|refused| refused.input_id)
            .filter(|id| chat.view.took_back(id))
            .collect();
        for id in taken {
            chat.session.discard(&id);
        }
        // Its held prompts go in the daemon's copy too, as they did while
        // the chat was open, but not in the composer it reopens with.
        let mut kept = chat.view.editor.clone();
        for (_, text, attachments) in held_prompts(&chat.session.state()) {
            kept.restore(&text, attachments);
        }
        // They are on their way back to the draft from now: the fleet may
        // drop the session (its agent exiting) before the next look at it.
        let returning = returning_prompts(&chat.session.state(), true);
        if !returning.is_empty() {
            self.returning.insert(chat.agent.clone(), returning);
        }
        let draft = composer_draft(&kept);
        let client = self.client.clone();
        let agent_id = chat.agent.agent.clone();
        self.spawn(async move {
            save_drafts(client, vec![(agent_id, draft)]).await;
            None
        });
        self.drafts.insert(chat.agent, chat.view.editor);
    }

    /// Every draft this client holds, for its daemon to keep as the client
    /// closes: the open chat's, those of chats left, and, in each agent's,
    /// the words of prompts held for an unreachable host, which go back to
    /// the draft rather than being lost with the client.
    fn drafts_to_keep(&mut self) -> Vec<(Vec<u8>, wire::Draft)> {
        let mut editors: HashMap<AgentKey, crate::editor::Editor> =
            std::mem::take(&mut self.drafts);
        if let Some(chat) = &self.chat {
            editors.insert(chat.agent.clone(), chat.view.editor.clone());
        }
        // A chat left before its agent was deleted keeps no draft: a fleet
        // that is current and no longer lists the agent says it is gone.
        {
            let fleet = self.fleet.state();
            if fleet.caught_up() && fleet.connection() == ui_state::Connection::Live {
                editors.retain(|agent, _| fleet.agent(agent).is_some());
            }
        }
        let open = self.chat.as_ref().map(|chat| chat.agent.clone());
        for session in self.fleet.sessions() {
            let state = session.state();
            let key = ui_state::agent_key(state.agent());
            let held = returning_prompts(&state, open.as_ref() != Some(&key));
            if held.is_empty() {
                continue;
            }
            let editor = editors.entry(key).or_default();
            for (_, text, attachments) in held {
                editor.restore(&text, attachments);
            }
        }
        editors
            .into_iter()
            .map(|(agent, editor)| (agent.agent, composer_draft(&editor)))
            .collect()
    }

    /// Keeps each agent's held prompts in the draft this machine's daemon
    /// keeps, from the moment they are held until they leave, so a client
    /// that dies without quitting still hands them back to the draft. Saved
    /// only when what is held changes, and counted kept only once the daemon
    /// took it; the quit's save covers the typing.
    fn keep_held_drafts(&mut self) {
        let mut seen = HashSet::new();
        let open = self.chat.as_ref().map(|chat| chat.agent.clone());
        for session in self.fleet.sessions() {
            let state = session.state();
            let key = ui_state::agent_key(state.agent());
            // A refused prompt comes back into an open chat's composer at
            // once; a closed chat's waits for it to open, in the draft.
            let held = returning_prompts(&state, open.as_ref() != Some(&key));
            // A held prompt resent once its host is back is still on its
            // way to the draft until the host answers: the fleet drops the
            // session of an agent that exited meanwhile before the refusal
            // arrives, and the prompt would be lost with it. One the host
            // took just before its agent exited comes back too, which is
            // the lesser harm.
            let resent: Vec<_> = self
                .returning
                .get(&key)
                .into_iter()
                .flatten()
                .filter(|(id, ..)| state.input_state(id) == Some(ui_state::InputState::Sent))
                .cloned()
                .collect();
            drop(state);
            seen.insert(key.clone());
            let returning: Vec<_> = held.iter().cloned().chain(resent).collect();
            if returning.is_empty() {
                self.returning.remove(&key);
            } else {
                self.returning.insert(key.clone(), returning);
            }
            let ids: Vec<ui_state::InputId> = held.iter().map(|(id, ..)| id.clone()).collect();
            if self
                .held_kept
                .get(&key)
                .map_or(ids.is_empty(), |kept| *kept == ids)
                || self.held_saving.get(&key) == Some(&ids)
            {
                continue;
            }
            let mut editor = match &self.chat {
                Some(chat) if chat.agent == key => chat.view.editor.clone(),
                _ => self.drafts.get(&key).cloned().unwrap_or_default(),
            };
            for (_, text, attachments) in held {
                editor.restore(&text, attachments);
            }
            let request = wire::UpdateDraftRequest {
                agent_id: key.agent.clone(),
                draft: Some(composer_draft(&editor)),
            };
            self.held_saving.insert(key.clone(), ids.clone());
            let client = self.client.clone();
            self.spawn(async move {
                let saved = client.update_draft(request).await.is_ok();
                Some(AppEvent::HeldSaved {
                    agent: key,
                    ids,
                    saved,
                })
            });
        }
        // A session gone (its agent exited or deleted) holds nothing more:
        // what it held or had refused goes back to the agent's draft, and
        // the draft is kept with it.
        let dropped: Vec<AgentKey> = self
            .returning
            .keys()
            .filter(|key| !seen.contains(*key))
            .cloned()
            .collect();
        for key in dropped {
            let Some(words) = self.returning.remove(&key) else {
                continue;
            };
            // A deleted agent keeps no draft.
            if self.fleet.state().agent(&key).is_none() {
                continue;
            }
            let editor = self.drafts.entry(key.clone()).or_default();
            for (_, text, attachments) in words {
                editor.restore(&text, attachments);
            }
            let draft = composer_draft(editor);
            let client = self.client.clone();
            self.spawn(async move {
                save_drafts(client, vec![(key.agent, draft)]).await;
                None
            });
        }
        self.held_kept.retain(|key, _| seen.contains(key));
        self.held_saving.retain(|key, _| seen.contains(key));
    }

    /// Hands every draft to this machine's daemon; awaited as the client
    /// quits.
    pub fn keep_drafts(&mut self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let drafts = self.drafts_to_keep();
        save_drafts(self.client.clone(), drafts)
    }

    fn field_text(&self) -> bool {
        match &self.chat {
            Some(chat) => {
                let state = chat.session.state();
                chat.view.field_text(&state)
            }
            None => self.fleet_view.field_text(),
        }
    }

    pub fn input(&mut self, event: Event) -> Flow {
        // A report has every key, click and paste until it is saved or
        // left: the frame under it is a picture of a moment that is over.
        if let Some(report) = &mut self.report {
            match event {
                Event::Key(key) if key.kind != KeyEventKind::Release => match report.key(key) {
                    crate::report::ReportAction::None => {}
                    crate::report::ReportAction::Cancel => self.report = None,
                    crate::report::ReportAction::Save => self.save_report(),
                },
                Event::Mouse(mouse) => report.mouse(mouse),
                Event::Paste(text) => report.paste(&text),
                _ => {}
            }
            return Flow::Continue;
        }
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key),
            Event::Paste(text) => {
                if self.help {
                    return Flow::Continue;
                }
                match &mut self.chat {
                    Some(chat) => {
                        let state = chat.session.state();
                        chat.view.paste_text(&state, &text);
                    }
                    None => self.fleet_view.paste(&text),
                }
                Flow::Continue
            }
            Event::Mouse(mouse) => {
                // The keys reference scrolls with the wheel when it is
                // taller than the screen.
                if self.help {
                    match mouse.kind {
                        crossterm::event::MouseEventKind::ScrollUp => {
                            self.help_scroll = self.help_scroll.saturating_sub(3)
                        }
                        crossterm::event::MouseEventKind::ScrollDown => self.help_scroll += 3,
                        _ => {}
                    }
                    return Flow::Continue;
                }
                let theme = self.theme();
                if let Some(chat) = &mut self.chat {
                    let effects = {
                        let state = chat.session.state();
                        chat.view.mouse(&state, mouse, theme)
                    };
                    chat.tell_following();
                    for effect in effects {
                        if let Some(flow) = self.chat_effect(effect) {
                            return flow;
                        }
                    }
                    return Flow::Continue;
                }
                let effects = {
                    let fleet = self.fleet.state();
                    self.fleet_view.mouse(&fleet, mouse)
                };
                for effect in effects {
                    if let Some(flow) = self.fleet_effect(effect) {
                        return flow;
                    }
                }
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Flow {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.help {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => self.help_scroll += 1,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                KeyCode::PageDown | KeyCode::Char(' ') => self.help_scroll += 10,
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
                _ => {
                    self.help = false;
                    self.help_scroll = 0;
                }
            }
            return Flow::Continue;
        }
        if key.code == KeyCode::Char('c') && ctrl {
            if self.field_text() {
                self.quit_armed = None;
                match &mut self.chat {
                    Some(chat) => {
                        let state = chat.session.state();
                        chat.view.kill_field(&state)
                    }
                    None => self.fleet_view.kill_field(),
                };
                return Flow::Continue;
            }
            if self.quit_armed.take().is_some() {
                return Flow::Quit;
            }
            self.quit_armed = Some(Instant::now());
            return Flow::Continue;
        }
        self.quit_armed = None;
        if key.code == KeyCode::Char(self.config.leader) && ctrl {
            self.leader_pending = true;
            self.leader_since = Some(Instant::now());
            return Flow::Continue;
        }
        if std::mem::take(&mut self.leader_pending) {
            self.leader_since = None;
            return self.chord(key);
        }
        match self.chat.is_some() {
            true => self.chat_key(key),
            false => self.fleet_key(key),
        }
    }

    fn chord(&mut self, key: KeyEvent) -> Flow {
        if key.code == KeyCode::Char(REPORT_KEY) {
            self.report_asked = true;
            return Flow::Continue;
        }
        let theme = self.theme();
        let Some(chat) = &mut self.chat else {
            return Flow::Continue;
        };
        let state = chat.session.state();
        match key.code {
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('s') => {
                drop(state);
                self.close_chat();
            }
            KeyCode::Char('h') => {
                drop(state);
                self.close_chat();
                self.fleet_view.home.open_hosts();
            }
            KeyCode::Char('d') => return Flow::Quit,
            KeyCode::Char('k') => chat.view.move_focus(&state, true, theme),
            KeyCode::Char('j') => chat.view.move_focus(&state, false, theme),
            KeyCode::Char('o') if ctrl_held(&key) => chat.view.pane_toggle_key(),
            KeyCode::Char('o') => chat.view.toggle_expanded(&state),
            KeyCode::Char(PANE_CHORD) => chat.view.pane_toggle_key(),
            KeyCode::Char('y') => {
                if let Some(text) = chat.view.copy_text() {
                    drop(state);
                    match crate::terminal::write_osc52(&mut std::io::stdout(), &text) {
                        Ok(Some(notice)) => self.notice(notice, Tone::Info),
                        Ok(None) => self.notice("copied", Tone::Info),
                        Err(error) => self.notice(format!("could not copy: {error}"), Tone::Warn),
                    }
                }
            }
            KeyCode::Char('n') => {
                let setup = sibling_setup(
                    &state,
                    self.fleet.state().agent(&chat.agent),
                    &self.config.local_host,
                    &self.config.defaults,
                )
                .sibling(state.kind(), self.config.chat_in);
                drop(state);
                self.close_chat();
                self.fleet_view.home.new_agent(setup);
            }
            KeyCode::Char('r') => {
                drop(state);
                self.review(None);
            }
            KeyCode::Char('c') => {
                if !chat.view.switch_comparison(&state) {
                    drop(state);
                    self.notice("no base branch to compare with", Tone::Info);
                }
            }
            KeyCode::Char('t') => {
                let tools = chat.view.switch_tools();
                drop(state);
                self.notice(format!("tool steps: {}", tools.words()), Tone::Info);
            }
            KeyCode::Char('a') if self.config.attach => {
                let agent = chat.agent.clone();
                drop(state);
                return self.raw_attach(&agent).unwrap_or(Flow::Continue);
            }
            _ => {}
        }
        Flow::Continue
    }

    /// `<leader> r`, or a `[diff]` clicked: the review page over the
    /// agent's working tree, or back to the one already in the draft.
    /// The review page, at the file `at` when one is named.
    fn review(&mut self, at: Option<String>) {
        let Some(chat) = &mut self.chat else {
            return;
        };
        if chat.view.resume_review() {
            if let Some(path) = &at {
                chat.view.review_file(path);
            }
            return;
        }
        let state = chat.session.state();
        let writable = matches!(state.composer(), Composer::Send | Composer::Resume);
        let agent_id = chat.view.agent_id.clone();
        // The page reads the changes the header counts.
        let base = ui_view::diff_base(&state, chat.view.comparison);
        drop(state);
        if !writable {
            self.notice("review waits until the chat is current", Tone::Warn);
            return;
        }
        let Some(base) = base else {
            self.notice("no base branch to compare with", Tone::Info);
            return;
        };
        {
            let client = self.client.clone();
            self.notice("reading the changes…", Tone::Info);
            self.spawn(async move {
                Some(
                    match ui_runtime::review::review(client.as_ref(), &agent_id, base).await {
                        Ok((_, patch)) if patch.trim().is_empty() => {
                            AppEvent::Notice("no changes to review".into(), Tone::Info)
                        }
                        Ok((diff, patch)) => AppEvent::Review {
                            agent_id,
                            diff,
                            patch,
                            at,
                        },
                        Err(error) => AppEvent::Notice(
                            format!("could not read the changes: {error}"),
                            Tone::Warn,
                        ),
                    },
                )
            });
        }
    }

    fn fleet_key(&mut self, key: KeyEvent) -> Flow {
        if let Some(since) = self.home_held {
            let now = now_ms();
            if now.saturating_sub(since) < crate::chat::ASK_ARMS_MS {
                self.home_held = Some(now);
                return Flow::Continue;
            }
            self.home_held = None;
        }
        let effects = {
            let fleet = self.fleet.state();
            self.fleet_view.key(&fleet, key)
        };
        for effect in effects {
            if let Some(flow) = self.fleet_effect(effect) {
                return flow;
            }
        }
        Flow::Continue
    }

    fn fleet_effect(&mut self, effect: FleetEffect) -> Option<Flow> {
        let client = self.client.clone();
        match effect {
            FleetEffect::Quit => return Some(Flow::Quit),
            FleetEffect::Help => self.help = true,
            FleetEffect::Report => self.report_asked = true,
            FleetEffect::Open(agent) => self.open(agent),
            FleetEffect::Attach(agent) => return self.raw_attach(&agent),
            FleetEffect::Start {
                setup,
                text,
                attachments,
                open,
            } => {
                let attach = setup.chat_in == settings::ChatInSetting::Native;
                self.spawn(async move {
                    // An agent used in its own terminal gets its first
                    // prompt there.
                    let prompt = if text.trim().is_empty() && attachments.is_empty() {
                        None
                    } else {
                        Some(inputs::submission(&text, attachments))
                    };
                    let request = setup.request(inputs::input_id(), prompt);
                    Some(match client.create_agent(request).await {
                        Ok(agent) => AppEvent::Started {
                            agent,
                            open,
                            attach,
                        },
                        Err(error) => AppEvent::StartFailed(error.to_string()),
                    })
                });
            }
            FleetEffect::Rename { agent, name } => {
                self.spawn(async move {
                    let request = RenameAgentRequest {
                        agent_id: agent.agent,
                        name,
                    };
                    client.rename_agent(request).await.err().map(|error| {
                        AppEvent::Notice(format!("could not rename: {error}"), Tone::Warn)
                    })
                })
            }
            FleetEffect::CancelStop(agent) => self.fleet.cancel_stop(&agent),
            FleetEffect::Stop(agent) => {
                let fleet = self.fleet.clone();
                self.spawn(async move {
                    // A stop whose host cannot be reached is held by the
                    // fleet and shown on the agent's row until it goes.
                    match fleet.stop(&agent).await {
                        Ok(_) => None,
                        Err(error) => Some(AppEvent::Notice(
                            format!("could not stop: {error}"),
                            Tone::Warn,
                        )),
                    }
                })
            }
            FleetEffect::Delete(agent) => self.spawn(async move {
                let request = DeleteAgentRequest {
                    agent_id: agent.agent,
                };
                Some(match client.delete_agent(request).await {
                    Ok(_) => AppEvent::Notice("deleted".into(), Tone::Info),
                    Err(error) => {
                        AppEvent::Notice(format!("could not delete: {error}"), Tone::Warn)
                    }
                })
            }),
        }
        None
    }

    fn chat_key(&mut self, key: KeyEvent) -> Flow {
        let theme = self.theme();
        let Some(chat) = &mut self.chat else {
            return Flow::Continue;
        };
        let effects = {
            let state = chat.session.state();
            if chat.view.opens_help(&state, key) {
                self.help = true;
                return Flow::Continue;
            }
            chat.view.key_at(&state, key, theme, now_ms())
        };
        // Before any send the key made: sending a prompt is a return to
        // the newest row.
        chat.tell_following();
        for effect in effects {
            if let Some(flow) = self.chat_effect(effect) {
                return flow;
            }
        }
        Flow::Continue
    }

    fn chat_effect(&mut self, effect: ChatEffect) -> Option<Flow> {
        let chat = self.chat.as_mut()?;
        let session = chat.session.clone();
        let agent_id = chat.view.agent_id.clone();
        match effect {
            // A refusal, a send that raced the exit among them, comes back
            // through the chat's own state: its words return to the
            // composer with why it was not sent.
            ChatEffect::Prompt { text, attachments } => self.spawn(async move {
                session.send_prompt(&text, attachments).await;
                None
            }),
            ChatEffect::Resume { text, attachments }
                if text.trim().is_empty() && attachments.is_empty() =>
            {
                self.spawn(async move {
                    session.resume().await.err().map(|error| {
                        AppEvent::Notice(format!("could not resume: {error}"), Tone::Warn)
                    })
                })
            }
            ChatEffect::Resume { text, attachments } => self.spawn(async move {
                let prompt = inputs::submission(&text, attachments.clone());
                match session.resume_with(prompt).await {
                    Ok(_) => None,
                    Err(error) => Some(AppEvent::Restore {
                        agent_id,
                        text,
                        attachments,
                        notice: Some(format!("could not resume: {error}")),
                    }),
                }
            }),
            ChatEffect::Answer(input) => self.spawn(async move {
                session
                    .answer(input)
                    .await
                    .err()
                    .and_then(|error| match error {
                        // The card itself says it was rejected, not
                        // confirmed or held.
                        InputError::Rejected(_) | InputError::Uncertain | InputError::Held => None,
                    })
            }),
            ChatEffect::SwitchBack => self.spawn(async move {
                session.switch_back().await.err().map(|error| {
                    AppEvent::Notice(
                        format!("could not switch back: {}", plain(error)),
                        Tone::Warn,
                    )
                })
            }),
            ChatEffect::StopJob { key } => {
                self.spawn(async move {
                    session.stop_job(&key).await.err().map(|error| {
                        AppEvent::Notice(format!("stop job {}", plain(error)), Tone::Warn)
                    })
                })
            }
            ChatEffect::Interrupt => self.spawn(async move {
                session
                    .interrupt()
                    .await
                    .err()
                    .map(|error| AppEvent::Notice(format!("stop {}", plain(error)), Tone::Warn))
            }),
            ChatEffect::Withdraw {
                id,
                text,
                attachments,
            } => self.spawn(async move {
                match session.withdraw(&id).await {
                    Ok(()) => Some(AppEvent::Restore {
                        agent_id,
                        text,
                        attachments,
                        notice: None,
                    }),
                    Err(error) => Some(AppEvent::Notice(
                        format!("withdraw {}", plain(error)),
                        Tone::Warn,
                    )),
                }
            }),
            ChatEffect::SendNow { id } => {
                self.spawn(async move {
                    session.send_now(&id).await.err().map(|error| {
                        AppEvent::Notice(format!("send now {}", plain(error)), Tone::Warn)
                    })
                })
            }
            ChatEffect::Resend { id } => {
                let input = session
                    .state()
                    .inputs()
                    .get(&id)
                    .map(|sent| sent.input.clone());
                if let Some(mut input) = input {
                    session.discard(&id);
                    input.input_id.clear();
                    self.spawn(async move {
                        session.send(input).await;
                        None
                    });
                }
            }
            ChatEffect::Discard { id } => session.discard(&id),
            ChatEffect::Cancel { id } => {
                let words = match session.state().inputs().get(&id).map(|sent| &sent.what) {
                    Some(InputWhat::Prompt { text, attachments }) => {
                        Some((text.clone(), attachments.clone()))
                    }
                    _ => None,
                };
                session.discard(&id);
                if let Some((text, attachments)) = words {
                    chat.view.editor.restore(&text, attachments);
                }
            }
            // Refused by the host (the name taken, the host away), the
            // command goes back to the composer to fix, with why.
            ChatEffect::Rename(name) => {
                let client = self.client.clone();
                self.spawn(async move {
                    let request = RenameAgentRequest {
                        agent_id: agent_id.clone(),
                        name: name.clone(),
                    };
                    client
                        .rename_agent(request)
                        .await
                        .err()
                        .map(|error| AppEvent::Restore {
                            agent_id,
                            text: format!("/rename {name}"),
                            attachments: Vec::new(),
                            notice: Some(format!("not renamed: {error}")),
                        })
                })
            }
            ChatEffect::Page(n) => {
                {
                    let state = session.state();
                    chat.view.page_sent(&state);
                }
                self.spawn(async move {
                    session
                        .page_older(n)
                        .await
                        .err()
                        .map(|error| AppEvent::Notice(error.to_string(), Tone::Warn))
                });
            }
            ChatEffect::Paste => match chat.view.paste(read_clipboard()) {
                Ok(Some(effect)) => return self.chat_effect(effect),
                Ok(None) => {}
                Err(error) => self.notice(error, Tone::Warn),
            },
            ChatEffect::Attach { name, mime, bytes } => {
                self.notice(format!("attaching {name}…"), Tone::Info);
                self.spawn(async move {
                    Some(match session.put_blob(&name, &mime, bytes).await {
                        Ok(blob) => AppEvent::Attached { agent_id, blob },
                        Err(error) => AppEvent::AttachFailed {
                            agent_id,
                            name,
                            why: error.to_string(),
                        },
                    })
                });
            }
            ChatEffect::Review => self.review(None),
            ChatEffect::ReviewAt(path) => self.review(Some(path)),
            ChatEffect::OpenUrl(url) => {
                if let Err(error) = open_url(&url) {
                    self.notice(format!("could not open {url}: {error}"), Tone::Warn);
                }
            }
            ChatEffect::Home => self.close_chat(),
            ChatEffect::Press(key) => {
                // The leader clicked shows its panel at once: the click is
                // the pause.
                if key.code == KeyCode::Char(self.config.leader)
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.leader_pending = true;
                    self.leader_since = Instant::now().checked_sub(PANEL_AFTER);
                    return None;
                }
                return Some(self.key(key));
            }
            ChatEffect::Chord(letter) => {
                self.leader_pending = false;
                self.leader_since = None;
                return Some(self.chord(KeyEvent::new(KeyCode::Char(letter), KeyModifiers::NONE)));
            }
            ChatEffect::RawAttach => {
                let agent = chat.agent.clone();
                return self.raw_attach(&agent);
            }
            ChatEffect::Notice(words) => self.notice(words, Tone::Info),
        }
        None
    }

    /// Hands the terminal to the agent's own interface when it is on this
    /// machine and has one; says why not otherwise, and from home, that
    /// Enter opens its chat (in the chat, Enter sends the draft).
    fn raw_attach(&mut self, agent: &AgentKey) -> Option<Flow> {
        let entry = self.fleet.state().agent(agent).cloned()?;
        match terminal_refusal(&entry, &self.config) {
            None => Some(Flow::Attach(Box::new(entry))),
            Some(why) if self.chat.is_some() => {
                self.notice(why, Tone::Warn);
                None
            }
            Some(why) => {
                self.notice(format!("{why}; enter opens its chat"), Tone::Warn);
                None
            }
        }
    }

    /// Why the open chat can no longer be served, in the person's terms:
    /// its agent deleted, or its host no longer linked to this machine (the
    /// runtime says only that it knows no such agent). None while it can.
    fn chat_gone(&self) -> Option<ChatGone> {
        let chat = self.chat.as_ref()?;
        let fleet = self.fleet.state();
        let ended = chat.session.ended();
        let listed = fleet.agent(&chat.agent).is_some();
        // An agent the fleet stopped listing, once the fleet is current.
        let dropped =
            !listed && fleet.caught_up() && fleet.connection() == ui_state::Connection::Live;
        if ended.is_none() && !dropped {
            return None;
        }
        let state = chat.session.state();
        let linked = fleet
            .host(&chat.agent.host)
            .is_some_and(|host| host.trusted().is_some());
        let host = state
            .host()
            .map(|host| host.name().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "its host".to_owned());
        let not_found = ended
            .as_ref()
            .is_some_and(|error| error.code() == Some(wire::ErrorCode::NotFound));
        let (why, deleted) = match ended {
            _ if !linked && (dropped || not_found) => {
                (format!("{host} is no longer linked to this machine"), false)
            }
            _ if dropped || not_found => (format!("{} was deleted", state.agent().name), true),
            Some(error) => (error.to_string(), false),
            None => return None,
        };
        Some(ChatGone { why, deleted })
    }

    /// What the loop does before each frame: a deleted agent's open chat
    /// closes to the fleet, and the changed files its overview lists are
    /// fetched when it asks.
    pub fn housekeep(&mut self) {
        self.keep_held_drafts();
        if let Some(gone) = self.chat_gone() {
            if gone.deleted {
                self.close_deleted_chat();
            } else {
                self.close_chat();
            }
            self.home_held = Some(now_ms());
            self.notice(format!("the chat closed: {}", gone.why), Tone::Warn);
            return;
        }
        self.ask_host_offers();
        let Some(chat) = &mut self.chat else {
            return;
        };
        let wanted = {
            let state = chat.session.state();
            chat.view.wants_changes(&state)
        };
        if let Some((key, base)) = wanted {
            let client = self.client.clone();
            let agent_id = chat.view.agent_id.clone();
            self.spawn(async move {
                let diff = ui_runtime::review::changed_files(client.as_ref(), &agent_id, base)
                    .await
                    .ok()?;
                Some(AppEvent::Changes {
                    agent_id,
                    key,
                    diff,
                })
            });
        }
    }

    /// While a new agent is being set up: what its host offers for each
    /// provider, asked once per version the host names, which also tells
    /// whether each is signed in there; the chosen provider's goes to the
    /// setup.
    fn ask_host_offers(&mut self) {
        let fleet = self.fleet.state();
        let Some(setup) = self.fleet_view.open_setup() else {
            return;
        };
        let host = setup.host.clone();
        let chosen = setup.agent.provider();
        let mut asks = Vec::new();
        for provider in crate::setup::PROVIDERS {
            let named = fleet
                .host(&host)
                .and_then(|entry| {
                    entry
                        .providers
                        .iter()
                        .find(|offer| offer.provider == provider as i32)
                })
                .and_then(|offer| offer.catalogue.clone());
            if let Some((hash, held)) = named
                .as_ref()
                .and_then(|hash| Some((hash, self.host_offers.get(hash)?)))
            {
                if setup.offered_hash() != Some(hash.as_slice()) {
                    setup.offer(&host, provider, hash, held);
                }
                continue;
            }
            if self
                .offers_asked
                .insert((host.clone(), provider, named.clone()))
            {
                asks.push((provider, provider == chosen));
            }
        }
        let host_name = fleet
            .host(&host)
            .map(|entry| entry.name().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "this machine".to_owned());
        drop(fleet);
        for (provider, chosen) in asks {
            let client = self.client.clone();
            let host = host.clone();
            let host_name = host_name.clone();
            self.spawn(async move {
                let request = wire::GetCatalogueRequest {
                    host_id: host.clone(),
                    provider: provider as i32,
                };
                let answer = client
                    .get_catalogue(request)
                    .await
                    .map_err(|error| error.to_string());
                let decoded = answer.and_then(|blob| {
                    wire::Catalogue::decode(blob.bytes.as_slice())
                        .map(|catalogue| (blob.hash, catalogue))
                        .map_err(|error| format!("the catalogue does not decode: {error}"))
                });
                match decoded {
                    Ok((hash, catalogue)) => Some(AppEvent::HostOffer {
                        host,
                        provider,
                        hash,
                        catalogue,
                    }),
                    // Only the agent being set up is worth a word.
                    Err(error) if chosen => Some(AppEvent::Notice(
                        format!(
                            "could not ask {host_name} what {} offers: {error}",
                            crate::setup::provider_name(provider)
                        ),
                        Tone::Warn,
                    )),
                    Err(_) => None,
                }
            });
        }
    }

    fn footer(&self, width: usize) -> Option<Line<'static>> {
        let theme = self.theme();
        if self.quit_armed.is_some() {
            let mut line = Line::from(Span::raw("  "));
            push(
                &mut line,
                "press ctrl+c again to quit",
                theme.warning(),
                width,
            );
            return Some(line);
        }
        if self.leader_pending && self.chat.is_none() {
            let mut line = Line::from(Span::raw("  "));
            push(&mut line, "leader: nothing here", theme.muted(), width);
            return Some(line);
        }
        let (words, tone, _) = self.notice.as_ref()?;
        let mut line = Line::from(Span::raw("  "));
        let style = match tone {
            Tone::Info => theme.muted(),
            Tone::Warn => theme.warning(),
        };
        push(&mut line, words.clone(), style, width);
        Some(line)
    }

    /// Paints the screen and asks for an older page when the reader is
    /// within one of the oldest held row.
    pub fn draw(&mut self, paint: &mut Paint<'_>) {
        self.keep_layout();
        let area = paint.area();
        let theme = self.theme();
        let width = usize::from(area.width);
        if self.help {
            let lines = help_lines(self.config.leader, width, theme);
            self.help_scroll = self
                .help_scroll
                .min(lines.len().saturating_sub(usize::from(area.height)));
            paint.render_widget(
                Paragraph::new(lines.into_iter().skip(self.help_scroll).collect::<Vec<_>>()),
                area,
            );
            return;
        }
        let footer = self.footer(width);
        let now = now_ms();
        let page = match &mut self.chat {
            Some(chat) => {
                let (family, reach) = {
                    let fleet = self.fleet.state();
                    let local = &self.config.local_host;
                    (
                        family_header(&fleet, local, &chat.agent.agent),
                        ui_view::reach(&fleet, local, &chat.agent.host),
                    )
                };
                chat.view.reach = reach;
                let panel_due = self.leader_pending
                    && self
                        .leader_since
                        .is_none_or(|since| since.elapsed() >= PANEL_AFTER);
                chat.view.panel = panel_due.then(|| panel_entries(self.config.attach));
                let page = {
                    let state = chat.session.state();
                    chat.view
                        .draw(paint, area, &state, family.as_ref(), footer, now, theme)
                };
                // A reload's swap takes the reader to the newest row.
                chat.tell_following();
                page
            }
            None => {
                // Each session is read on its own, before the fleet is.
                let lines = self
                    .fleet
                    .sessions()
                    .into_iter()
                    .map(|session| {
                        let state = session.state();
                        (
                            ui_state::agent_key(state.agent()),
                            ui_view::session_line(&state, now),
                        )
                    })
                    .collect();
                let fleet = self.fleet.state();
                self.fleet_view
                    .draw(paint, area, &fleet, &lines, footer, now, theme);
                None
            }
        };
        if std::mem::take(&mut self.report_asked) {
            let frozen = paint.buffer_mut().clone();
            self.report = Some(crate::report::Report::new(frozen, theme, self.start_dump()));
        }
        if let Some(report) = &self.report {
            let area = paint.area();
            report.draw(paint, area, theme);
        }
        if let Some(n) = page {
            self.chat_effect(ChatEffect::Page(n));
        }
    }

    /// The profile's dump with this client's parts, started as the screen
    /// freezes so it is ready by the time the note is.
    fn start_dump(&self) -> crate::report::Dump {
        let client = self.client.clone();
        let parts = [self.fleet.dump_part()];
        let sessions = self
            .chat
            .iter()
            .map(|chat| chat.session.history().encode_to_vec())
            .collect();
        tokio::spawn(async move {
            let response = client
                .dump(wire::DumpRequest {
                    agent_ids: Vec::new(),
                    reason: "report a problem".to_owned(),
                    automatic: false,
                    sessions,
                    scope: wire::DumpScope::Report.into(),
                    check: String::new(),
                })
                .await
                .map_err(|error| format!("the daemon could not dump: {error}"))?;
            if response.report_path.is_empty() {
                return Err("the daemon wrote its dump on another machine".to_owned());
            }
            let bundle = std::path::PathBuf::from(response.report_path);
            for part in &parts {
                ui_runtime::write_part(&bundle, part)
                    .map_err(|error| format!("writing this client's part of the dump: {error}"))?;
            }
            Ok(bundle)
        })
    }

    /// Writes the report and says where.
    fn save_report(&mut self) {
        let Some(report) = self.report.take() else {
            return;
        };
        let client = self.client.clone();
        let build = self.config.version.clone();
        self.notice("sending the report…", Tone::Info);
        self.spawn(async move {
            Some(match report.submit(client, build).await {
                Ok(done) if done.not_sent.is_empty() => AppEvent::Notice(
                    format!("report sent; a copy is in {}", done.report_path),
                    Tone::Info,
                ),
                Ok(done) => AppEvent::Notice(
                    format!(
                        "report saved in {}, not sent: {}",
                        done.report_path, done.not_sent
                    ),
                    Tone::Warn,
                ),
                Err(error) => {
                    AppEvent::Notice(format!("could not save the report: {error}"), Tone::Warn)
                }
            })
        });
    }
}

/// The key that reports a problem: bare on home, after the leader in a
/// chat. "b" for bug: "report" and "problem" hold letters that are taken.
const REPORT_KEY: char = 'b';

/// A new agent's settings copied from a chat: its model, effort,
/// permission, mode, folder and host ([`crate::setup::Setup::sibling`] then sets the agent and
/// where the person chats). Never a name, and never a new worktree: a chat
/// already working in a worktree's folder starts its sibling there.
fn sibling_setup(
    state: &ui_state::SessionState,
    agent: Option<&wire::Agent>,
    local_host: &[u8],
    defaults: &settings::NewAgentSettings,
) -> crate::setup::Setup {
    let view = ui_view::settings(state);
    crate::setup::Setup {
        name: None,
        agent: crate::setup::Agent::Claude,
        chat_in: settings::ChatInSetting::Amux,
        model: view
            .models
            .iter()
            .find(|model| model.current)
            .map(|model| model.value.clone()),
        effort: view
            .efforts
            .iter()
            .find(|effort| effort.current)
            .map(|effort| effort.value.clone()),
        permission: view
            .permissions
            .iter()
            .find(|permission| permission.current && !permission.custom)
            .map(|permission| permission.value.clone()),
        mode: view
            .modes
            .iter()
            .find(|mode| mode.current && !mode.unlisted)
            .map(|mode| mode.value.clone()),
        offered: None,
        folder: agent.map(|agent| agent.cwd.clone()).unwrap_or_default(),
        host: agent.map(|agent| agent.host_id.clone()).unwrap_or_default(),
        local_host: local_host.to_vec(),
        worktree: false,
        defaults: defaults.clone(),
    }
}

/// The overview's chord after the leader: Ctrl+O, which also works on its
/// own; the leader's flyover leaves it out.
const PANE_CHORD: char = '\u{f}';

impl App {
    /// Takes in a layout change the open chat made, and keeps it.
    fn keep_layout(&mut self) {
        let Some(chat) = &self.chat else {
            return;
        };
        let mut changed = chat.view.pane_open != self.layout.overview
            || chat.view.comparison != self.layout.comparison;
        self.layout.overview = chat.view.pane_open;
        self.layout.comparison = chat.view.comparison;
        changed |= self
            .layout
            .set_folds(&chat.view.agent_id, &chat.view.pane_folds);
        changed |= self.layout.set_tools(&chat.view.agent_id, chat.view.tools);
        if changed {
            self.layout.save(self.config.layout.as_deref());
        }
    }
}

/// The leader's panel in a chat: every chord and what it does.
fn panel_entries(attach: bool) -> Vec<crate::chat::PanelEntry> {
    let entry =
        |group: &'static str, key: &str, label: &str, chord: char| crate::chat::PanelEntry {
            group,
            key: key.to_owned(),
            label: label.to_owned(),
            chord,
        };
    // What is about amux first, then what is about this chat.
    let mut entries = vec![
        entry("amux", "s", "home", 's'),
        entry("amux", "n", "new agent", 'n'),
        entry("amux", "h", "hosts", 'h'),
        entry("amux", "d", "detach", 'd'),
        entry("amux", "b", "bug report", 'b'),
        entry("amux", "?", "all keys", '?'),
        entry("this chat", "r", "review changes", 'r'),
        entry(
            "this chat",
            "c",
            "count changes: uncommitted or on this branch",
            'c',
        ),
        entry(
            "this chat",
            "t",
            "tool steps: collapse, show all or hide",
            't',
        ),
    ];
    if attach {
        entries.push(entry("this chat", "a", "attach to its own terminal", 'a'));
    }
    entries.extend([
        entry("this chat", "k", "focus an older row", 'k'),
        entry("this chat", "j", "focus a newer row", 'j'),
        entry("this chat", "o", "open the focused row", 'o'),
        entry("this chat", "y", "copy the focused row", 'y'),
    ]);
    entries
}

pub(crate) fn help_lines(leader: char, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let leader = format!("ctrl+{leader}");
    let mut rows: Vec<(&str, String)> = crate::home::help_rows();
    rows.extend([
        ("", String::new()),
        ("Chat", String::new()),
        (
            "enter",
            "send; queue while it works; resume an exited agent".into(),
        ),
        ("shift+enter / ctrl+j", "new line".into()),
        (
            "ctrl+v",
            "attach an image or file from the clipboard".into(),
        ),
        (
            "↑ in an empty composer",
            "the queued messages: enter sends one now, backspace withdraws it; with none queued, the last prompt again".into(),
        ),
        ("ctrl+x", "interrupt: end the turn; the agent stays".into()),
        ("ctrl+c", "clear the draft; twice on nothing quits".into()),
        ("ctrl+o", "the overview of what is in flight".into()),
        (
            "shift+tab",
            "the agent's next mode, or its next permission".into(),
        ),
        (
            "ctrl+s then m / e / p",
            "the agent's model, effort or permission, where it can change from here".into(),
        ),
        (
            "pgup / pgdn, wheel",
            "scroll; ctrl+end (or the ↓ button) jumps to the newest".into(),
        ),
        (
            "click",
            "hints, [Diff], [Home], the permission, a pinned message, a folded line".into(),
        ),
        (
            "shift+drag",
            "select text to copy (option+drag in iTerm2 and Ghostty)".into(),
        ),
        (
            "esc",
            "back out one level: a flyover, a field, the pane; never stops the agent".into(),
        ),
        ("", String::new()),
        ("Asks", String::new()),
        ("↑ / ↓, digits", "move, or pick at once".into()),
        (
            "enter",
            "choose; on a text field, answer and move on".into(),
        ),
        ("space", "tick a box where several can be picked".into()),
        (
            "tab / shift+tab, ← / →",
            "the next or previous question".into(),
        ),
        ("tab on No", "add a note to the refusal".into()),
        ("esc", "to the way out (No, Decline)".into()),
        ("f", "the whole diff or command, inside the box".into()),
        ("", String::new()),
        ("Leader", String::new()),
    ]);
    // The leader's chords, by their whole keys.
    let chords: [(&str, &str); 13] = [
        ("s", "home"),
        ("n", "new agent, starting from this chat's settings"),
        ("h", "hosts"),
        ("d", "detach: leave to the shell; agents keep running"),
        ("b", "report a problem: mark the screen, write a note"),
        ("r", "review changes; comments go in the draft"),
        ("c", "count changes: uncommitted or on this branch"),
        ("t", "tool steps: collapse, show all or hide"),
        ("a", "attach to the agent's own terminal (this machine)"),
        ("k / j", "focus an older or newer row"),
        ("o", "open the focused row or run"),
        ("y", "copy the focused row"),
        ("?", "these keys"),
    ];
    let mut rows: Vec<(String, String)> = rows
        .into_iter()
        .map(|(key, words)| (key.to_owned(), words))
        .collect();
    rows.extend(
        chords
            .iter()
            .map(|(key, words)| (format!("{leader} {key}"), (*words).to_owned())),
    );
    let mut lines = vec![
        Line::from(Span::styled("  Keys", theme.emphasis())),
        Line::default(),
    ];
    // Headings like home's (bold, muted, a hairline to the edge); keys read
    // first as on the hint line, their words faint.
    for (key, words) in rows {
        let mut line = Line::from(Span::raw("  "));
        if words.is_empty() && !key.is_empty() {
            push(
                &mut line,
                &key,
                theme.muted().add_modifier(ratatui::style::Modifier::BOLD),
                width,
            );
            push(&mut line, " ", theme.muted(), width);
            let rule = width.saturating_sub(text::line_width(&line) + 2);
            push(&mut line, "─".repeat(rule), theme.hairline(), width);
        } else if !words.is_empty() {
            push(&mut line, format!("{key:<24}"), theme.emphasis(), width);
            // The words wrap under their own column: cut at the edge, a
            // narrow terminal lost the end of a rule ("never stops the
            // agent").
            let room = width.saturating_sub(2 * 2 + 24).max(12);
            let mut parts = text::wrap(&words, room).into_iter();
            if let Some(first) = parts.next() {
                push(&mut line, first, theme.faint(), width);
            }
            lines.push(line);
            for part in parts {
                let mut more = Line::from(Span::raw(" ".repeat(2 + 24)));
                push(&mut more, part, theme.faint(), width);
                lines.push(more);
            }
            continue;
        }
        lines.push(line);
    }
    lines.push(Line::default());
    let mut closes = Line::from(Span::raw("  "));
    push(&mut closes, "j/k", theme.emphasis(), width);
    push(&mut closes, " scroll   ", theme.faint(), width);
    push(&mut closes, "any other key", theme.emphasis(), width);
    push(&mut closes, " closes", theme.faint(), width);
    lines.push(closes);
    lines
}

/// Opens `url` with the system's handler, detached: the browser for a web
/// link. Only web and mail links open; anything else in agent text is not
/// run.
fn open_url(url: &str) -> std::io::Result<()> {
    if !["https://", "http://", "mailto:"]
        .iter()
        .any(|scheme| url.starts_with(scheme))
    {
        return Err(std::io::Error::other("not a web link"));
    }
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    } else {
        std::process::Command::new("xdg-open")
    };
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

fn ctrl_held(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}
