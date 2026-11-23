//! The language-neutral bridge over the embedded runtime.
//!
//! One [`AmuxRuntime`] hosts the embedded installation, with one profile
//! per account. An [`AmuxProfile`] is the fleet and chats of one of its
//! profiles, opened for as long as it is on screen; each open chat is an
//! [`AmuxChat`]. View values cross as JSON whose Swift and Kotlin mirrors are
//! generated from the Rust definitions, so every client shares one definition
//! of every value.
//!
//! A profile's fleet and each open chat open with the whole of what they
//! show (`fleet_opening`, `opening`); after that a step hands over what moved
//! in each as a delta, only what moved. Rows are handed out by item key: a
//! chat's id sequence changes only at its two edges, newer keys above the
//! newest the host holds and older ones below its oldest, until a delta
//! reloads it; there is no slot identity to keep in step.
//!
//! Threads: the app owns the loop. The daemon cores step on whichever queue
//! calls [`AmuxRuntime::step`]; links, channels and gRPC run on one confined
//! Rust thread, which never calls into the app. Whenever something waits for
//! a step, the descriptor [`AmuxRuntime::wake_fd`] answers becomes readable,
//! and the app steps when it sees it. Reads return at once. An act that waits
//! on the agent returns a ticket at once, and its JSON result comes back from
//! a later step under that ticket, after the deltas of what it moved; a step
//! also carries the fleets' and chats' deltas, and the profile list when it
//! moved. The calls that block until the cores have done something (start,
//! stop, a profile or chat opening, a lifecycle input) step the cores on the
//! calling thread while they wait.
//!
//! Language bindings (`app-ffi` for C/Swift, `app-jni` for Kotlin/JNI) wrap
//! these handles and methods without duplicating runtime or JSON logic.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

pub use app_embedded::{EdgeOverrides, EmbeddedRuntime, ProfileId, StartConfig};
use app_embedded::{PairRequest, Phone, ProfileView, UsageEvent};
use app_runtime::values::{
    AgentAct, ChatAct, ChatDelta, Draft, FleetDelta, Found, MachineSettings, NewAgent,
    OnboardingAnswer, OnboardingState, PairingStatusView, PairingWindowView, RepositorySuggestion,
    ReviewChange, ReviewChoice, ReviewOutcome, SettingsChange, SignInAnswer, SignInOfferOutcome,
    SignInOfferView,
};
use app_runtime::{AppRuntime, Chat, ReviewSession, Wake};
pub use client::Surface;
use node::SourcePolicy;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;
use ui_state::{AgentKey, Key};
use ui_view::{Comparison, ReviewContextAsk};

/// The embedded installation and every profile it hosts.
pub struct AmuxRuntime {
    shared: Arc<Shared>,
    watch: Option<tokio::task::JoinHandle<()>>,
    tail: u32,
}

/// What the runtime, its profiles and its chats share.
struct Shared {
    phone: Phone,
    /// The profiles, managed the same way whether the installation is
    /// hosted here or is a daemon this app attached to.
    installation: client::Installation,
    outbox: Arc<Outbox>,
}

/// What a phone-only call answers on an app attached to a daemon.
const ATTACHED: &str = "an app attached to amux does not host its profiles; ask the daemon";

/// One profile's fleet and chats.
pub struct AmuxProfile {
    profile: ProfileId,
    /// The number deltas name this fleet by.
    fleet: u64,
    shared: Arc<Shared>,
    app: Arc<AppRuntime>,
    tail: u32,
    /// Hands the app the sign-in offers paired machines make this profile,
    /// while the profile is open.
    offers: tokio::task::JoinHandle<()>,
    /// Hands the app the profile's setting up, once it asked to follow it.
    onboarding: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Hands the app the pairing window's status while it holds one open.
    pairing: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for AmuxProfile {
    fn drop(&mut self) {
        self.offers.abort();
        for watch in [&self.onboarding, &self.pairing] {
            if let Some(watch) = watch.lock().unwrap_or_else(|p| p.into_inner()).take() {
                watch.abort();
            }
        }
    }
}

/// One open chat.
pub struct AmuxChat {
    chat: Arc<Chat>,
    fleet: u64,
    shared: Arc<Shared>,
}

/// What the app's life feeds the runtime.
#[derive(Debug, Deserialize)]
pub enum Lifecycle {
    /// The app came to the front: counted at most once an hour, and every
    /// profile suspended dials its hosts and the relay again.
    Foreground,
    /// The system woke the app in the background, for a push: every profile
    /// suspended dials its hosts and the relay again, until the next
    /// `Suspending`.
    Woken,
    /// The app is leaving the screen: the analytics waiting are sent.
    Background,
    /// The app is about to be suspended: every profile closes its links
    /// and dials nothing until the app runs again, the analytics waiting
    /// are sent, and every profile's core is checkpointed.
    ///
    /// The checkpoint makes the store whole on the drive should the system
    /// end the app; a relaunch starts from that store, not the checkpoint,
    /// whose links, calls, jobs and timers belong to the process that ended.
    Suspending,
    /// The phone's network changed: every profile's link socket is bound
    /// afresh and every path machine probes its paths again, which the
    /// link log's rebind line records.
    NetworkChanged,
}

impl Shared {
    fn embedded(&self) -> Option<&Arc<EmbeddedRuntime>> {
        self.phone.hosted()
    }

    fn handle(&self) -> &Handle {
        self.phone.handle()
    }

    /// Starts `act` on the transport and answers the ticket its result
    /// comes back under, from a later step.
    fn act<T, F>(&self, act: F) -> u64
    where
        T: Serialize,
        F: Future<Output = T> + Send + 'static,
    {
        self.outbox.act(self.handle(), act)
    }

    /// Starts an act that changes the profile list. The list as the act
    /// left it is posted before the act's answer, so the app holds it by
    /// the time it hears the answer.
    fn act_on_profiles<T, F>(&self, act: F) -> u64
    where
        T: Serialize + Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let installation = self.installation.clone();
        let outbox = self.outbox.clone();
        self.act(async move {
            let answer = act.await;
            outbox.post_profiles(&installation).await;
            answer
        })
    }
}

/// What the transport left for the app's next step: which views moved,
/// and the answers to acts it started. Nothing here calls the app; each
/// change rings the wake descriptor instead.
struct Outbox {
    tickets: AtomicU64,
    fleets: AtomicU64,
    waiting: Mutex<Waiting>,
    /// The fleets and chats open, by the numbers deltas name them by (a
    /// fleet as chat 0): nothing closed is handed over again, however many
    /// of its acts are still in flight.
    open: Mutex<HashMap<(u64, u64), View>>,
    ring: Box<dyn Fn() + Send + Sync>,
}

#[derive(Clone)]
enum View {
    Fleet(Weak<AppRuntime>),
    Chat(Weak<Chat>),
}

#[derive(Default)]
struct Waiting {
    profiles: Option<Vec<ProfileView>>,
    /// The fleets (chat 0) and chats that moved.
    moved: BTreeSet<(u64, u64)>,
    /// Each open profile's sign-in offers, by fleet, when they moved.
    offers: std::collections::BTreeMap<u64, Vec<SignInOfferView>>,
    /// Each followed profile's setting up, by fleet, when it moved.
    onboarding: std::collections::BTreeMap<u64, OnboardingState>,
    /// Each open pairing window's status, by fleet, when it moved.
    pairing: std::collections::BTreeMap<u64, PairingStatusView>,
    answers: Vec<Answer>,
}

/// What a step hands the app, as JSON.
#[derive(Default, Serialize)]
struct Stepped {
    /// Every profile, oldest first, when the list moved since the last
    /// step.
    profiles: Option<Vec<ProfileView>>,
    fleets: Vec<FleetMoved>,
    chats: Vec<ChatMoved>,
    /// The open profiles whose sign-in offers moved, each with every offer
    /// it holds now: an offer gone is missing from its list.
    sign_in_offers: Vec<SignInOffersMoved>,
    /// The followed profiles whose setting up moved, each as it is now.
    onboarding: Vec<OnboardingMoved>,
    /// The profiles whose pairing window's status moved, each as it is now.
    pairing: Vec<PairingMoved>,
    /// The acts that ended since the last step, after what they moved.
    answers: Vec<Answer>,
}

#[derive(Serialize)]
struct SignInOffersMoved {
    fleet: u64,
    offers: Vec<SignInOfferView>,
}

#[derive(Serialize)]
struct OnboardingMoved {
    fleet: u64,
    onboarding: OnboardingState,
}

#[derive(Serialize)]
struct PairingMoved {
    fleet: u64,
    status: PairingStatusView,
}

#[derive(Serialize)]
struct FleetMoved {
    fleet: u64,
    delta: FleetDelta,
}

#[derive(Serialize)]
struct ChatMoved {
    fleet: u64,
    delta: ChatDelta,
}

#[derive(Serialize)]
struct Answer {
    ticket: u64,
    /// The act's result, as JSON text.
    json: String,
}

impl Outbox {
    fn post(&self, post: impl FnOnce(&mut Waiting)) {
        post(&mut self.waiting.lock().unwrap_or_else(|p| p.into_inner()));
        (self.ring)();
    }

    /// Posts the profile list as it stands now; a list that cannot be read
    /// is left for the profile stream to post.
    async fn post_profiles(&self, installation: &client::Installation) {
        if let Ok(profiles) = installation.list_profiles().await {
            self.post_listed(&profiles);
        }
    }

    fn post_listed(&self, profiles: &[wire::Profile]) {
        let views = profiles.iter().map(ProfileView::from).collect();
        self.post(|waiting| waiting.profiles = Some(views));
    }

    /// What waits for the app, each view that moved as its delta. Called
    /// inside the runtime, since reading a row can start a fetch.
    fn take(&self) -> Stepped {
        let waiting = std::mem::take(&mut *self.waiting.lock().unwrap_or_else(|p| p.into_inner()));
        let mut stepped = Stepped {
            profiles: waiting.profiles,
            answers: waiting.answers,
            sign_in_offers: waiting
                .offers
                .into_iter()
                .map(|(fleet, offers)| SignInOffersMoved { fleet, offers })
                .collect(),
            onboarding: waiting
                .onboarding
                .into_iter()
                .map(|(fleet, onboarding)| OnboardingMoved { fleet, onboarding })
                .collect(),
            pairing: waiting
                .pairing
                .into_iter()
                .map(|(fleet, status)| PairingMoved { fleet, status })
                .collect(),
            ..Stepped::default()
        };
        for (fleet, chat) in waiting.moved {
            let view = self
                .open
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&(fleet, chat))
                .cloned();
            match view {
                Some(View::Fleet(app)) => {
                    if let Some(delta) = app.upgrade().and_then(|app| app.fleet_delta()) {
                        stepped.fleets.push(FleetMoved { fleet, delta });
                    }
                }
                Some(View::Chat(open)) => {
                    if let Some(delta) = open.upgrade().and_then(|open| open.delta()) {
                        stepped.chats.push(ChatMoved { fleet, delta });
                    }
                }
                None => {}
            }
        }
        stepped
    }

    /// Hands the app a profile's sign-in offers in the next step.
    fn post_offers(&self, fleet: u64, offers: Vec<SignInOfferView>) {
        self.post(|waiting| {
            waiting.offers.insert(fleet, offers);
        });
    }

    /// Hands the app a profile's setting up in the next step.
    fn post_onboarding(&self, fleet: u64, onboarding: OnboardingState) {
        self.post(|waiting| {
            waiting.onboarding.insert(fleet, onboarding);
        });
    }

    /// Hands the app a profile's pairing window status in the next step.
    fn post_pairing(&self, fleet: u64, status: PairingStatusView) {
        self.post(|waiting| {
            waiting.pairing.insert(fleet, status);
        });
    }

    /// Names a fleet (chat 0) or chat in the next step.
    fn redraw(&self, fleet: u64, chat: u64) {
        self.post(|waiting| {
            waiting.moved.insert((fleet, chat));
        });
    }

    fn opened(&self, fleet: u64, chat: u64, view: View) {
        self.open
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert((fleet, chat), view);
    }

    /// Closes a fleet with every chat of it, or one chat.
    fn close(&self, fleet: u64, chat: Option<u64>) {
        self.open
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(of, open), _| *of != fleet || chat.is_some_and(|chat| *open != chat));
    }

    fn act<T, F>(self: &Arc<Self>, handle: &Handle, act: F) -> u64
    where
        T: Serialize,
        F: Future<Output = T> + Send + 'static,
    {
        let ticket = self.tickets.fetch_add(1, Ordering::SeqCst) + 1;
        let owed = Owed {
            outbox: self.clone(),
            ticket,
            answered: false,
        };
        handle.spawn(async move {
            let result = act.await;
            owed.answer(serde_json::to_string(&result).unwrap_or_else(|_| "null".to_owned()));
        });
        ticket
    }
}

/// An act's one answer, owed from the moment the act is started. A stop
/// drops the acts still running; the drop answers for them, so whoever
/// waits on the app's side hears that the runtime has stopped rather than
/// nothing, ever.
struct Owed {
    outbox: Arc<Outbox>,
    ticket: u64,
    answered: bool,
}

impl Owed {
    fn answer(mut self, json: String) {
        self.answered = true;
        let ticket = self.ticket;
        self.outbox
            .post(|waiting| waiting.answers.push(Answer { ticket, json }));
    }
}

impl Drop for Owed {
    fn drop(&mut self) {
        if !self.answered {
            let ticket = self.ticket;
            self.outbox.post(|waiting| {
                waiting.answers.push(Answer {
                    ticket,
                    json: STOPPED.to_owned(),
                })
            });
        }
    }
}

/// What an act dropped unanswered answers: the error the app's own
/// stopped-runtime path reports.
const STOPPED: &str = "{\"Err\":\"the runtime has stopped\"}";

#[derive(Serialize)]
enum Answered<T> {
    Ok(T),
    Err(String),
}

impl<T, E: std::fmt::Display> From<Result<T, E>> for Answered<T> {
    fn from(result: Result<T, E>) -> Self {
        match result {
            Ok(value) => Answered::Ok(value),
            Err(error) => Answered::Err(error.to_string()),
        }
    }
}

fn parse<T: DeserializeOwned>(value: Option<&str>) -> Option<T> {
    serde_json::from_str(value?).ok()
}

fn json<T: Serialize>(value: &T) -> Option<String> {
    serde_json::to_string(value).ok()
}

fn profile_id(profile: Option<&str>) -> Option<ProfileId> {
    profile?.parse().ok()
}

/// A profile's description as the app holds it, from the installation's.
fn view(profile: Result<wire::Profile, client::RpcError>) -> Answered<ProfileView> {
    Answered::from(profile.map(|profile| ProfileView::from(&profile)))
}

// --- startup and helpers ---------------------------------------------------

/// This build's version string, carrying `+debug-tools` when the driving
/// tools feature is enabled.
pub fn version() -> String {
    let mut version = node::version().to_owned();
    if cfg!(feature = "debug-tools") {
        version.push_str("+debug-tools");
    }
    version
}

/// Why `name` cannot name an agent, as a JSON `AgentNameProblem`, or JSON
/// `null` when it can.
pub fn agent_name_problem(name: &str) -> Option<String> {
    json(&ui_view::agent_name_problem(name))
}

/// A new agent's `SettingsView` from a host's `Catalogue` and the
/// `NewAgentChoices` made so far, both JSON; `None` when either does not
/// parse.
pub fn new_agent_settings(catalogue: Option<&str>, chosen: Option<&str>) -> Option<String> {
    let catalogue: ui_view::Catalogue = parse(catalogue)?;
    let chosen: ui_view::NewAgentChoices = parse(chosen)?;
    json(&ui_view::new_agent_settings(&catalogue, &chosen))
}

/// The `NewAgentChoices` after a `NewAgentPick`, with the rules a pick
/// carries; every argument JSON, `None` when one does not parse.
pub fn new_agent_pick(
    catalogue: Option<&str>,
    chosen: Option<&str>,
    pick: Option<&str>,
) -> Option<String> {
    let catalogue: ui_view::Catalogue = parse(catalogue)?;
    let chosen: ui_view::NewAgentChoices = parse(chosen)?;
    let pick: ui_view::NewAgentPick = parse(pick)?;
    json(&ui_view::new_agent_pick(&catalogue, &chosen, &pick))
}

/// The step a run of setting up shows after `after`, a `SetupStep` as JSON
/// or `None` from the start, in an `OnboardingState` as JSON.
pub fn onboarding_next(
    onboarding: Option<&str>,
    after: Option<&str>,
    finishing: bool,
) -> Option<String> {
    let onboarding: OnboardingState = parse(onboarding)?;
    let after: Option<ui_view::SetupStep> = parse(after);
    json(&ui_view::next_step(
        &wire::Onboarding::from(&onboarding),
        after,
        finishing,
    ))
}

/// The problems of a form's values, as the shared view checks them.
pub fn form_problems(fields: Option<&str>, values: Option<&str>) -> Option<String> {
    let fields: Vec<ui_view::FormField> = parse(fields)?;
    let values: Vec<ui_view::FormValue> = parse(values)?;
    json(&ui_view::form_problems(&fields, &values))
}

/// The agent's commands matching the word a draft ends in.
pub fn command_matches(commands: Option<&str>, draft: &str) -> Option<String> {
    let commands: Vec<ui_view::CommandView> = parse(commands)?;
    json(&ui_view::command_matches(&commands, draft))
}

/// Whether `pasted`, arriving in a draft in one paste, joins it as one text
/// attachment rather than as words.
pub fn pastes_as_attachment(pasted: &str) -> bool {
    ui_view::pastes_as_attachment(pasted)
}

/// The most an attachment may hold, in bytes.
pub fn attachment_limit() -> u64 {
    ui_view::ATTACHMENT_SIZE_LIMIT as u64
}

/// A fenced code block coloured by kind of token, as the shared view
/// colours it for every client: the code, the fence's info string (its
/// first word names the language) and whether this is the block still
/// being written, the last of a message still streaming whose closing
/// fence has not arrived. Answers a `HighlightedCode` as JSON, one line
/// for each of the code's lines.
///
/// A result that is not `settled` is drawn as it is and asked for again
/// soon (within a tenth of a second): a grammar is being built, or a
/// streaming block's completed lines are being passed over, in the
/// background. Callable from any thread; finished code is kept by content,
/// so asking again for the same block costs a lookup.
pub fn highlight_block(code: &str, info: &str, streaming: bool) -> Option<String> {
    json(&ui_code::highlight_block(
        ui_code::Highlighter::shared(),
        code,
        info,
        streaming,
    ))
}

/// A file's change coloured as the file at `path` is, from its hunks as an
/// edit ask's card carries them (a `FilePatch`'s `[PatchHunk]` as JSON),
/// each hunk coloured as a fragment of its own. Answers a `HighlightedCode`
/// as JSON with one line for each of the hunks' lines, hunk after hunk;
/// `None` when the hunks do not parse. Settles as [`highlight_block`] does.
pub fn highlight_patch(path: &str, hunks: Option<&str>) -> Option<String> {
    let hunks: Vec<ui_view::PatchHunk> = parse(hunks)?;
    json(&ui_code::highlight_patch(
        ui_code::Highlighter::shared(),
        path,
        &hunks,
    ))
}

/// Starts the installation with its transport on a thread of its own and
/// its cores on the calling thread's queue; blocks, stepping them, until
/// every profile's store is open. The apps start from JSON
/// ([`start_from_json`]); this form lets a test choose the overrides, such
/// as serving links on loopback. `surface` is the app hosting it, which
/// every call it makes names to the installation.
#[doc(hidden)]
pub fn start(
    config: &StartConfig,
    overrides: EdgeOverrides,
    surface: Surface,
) -> Result<AmuxRuntime, String> {
    let phone = Phone::start(config, overrides, Arc::new(client::SystemClock), surface)
        .map_err(|error| error.to_string())?;
    let installation = phone
        .embedded()
        .installation()
        .map_err(|error| error.to_string())?;
    serve(phone, installation, config.tail)
}

/// Starts the installation from a `StartConfig` as JSON, hosted by the app
/// `surface` names.
pub fn start_from_json(config: Option<&str>, surface: Surface) -> Result<AmuxRuntime, String> {
    let config: StartConfig =
        parse(config).ok_or_else(|| "the start configuration is not a StartConfig".to_owned())?;
    start(&config, overrides(&config), surface)
}

/// Attaches to the daemon answering on `front_door` (`<root>/amux.sock`):
/// the same runtime as [`start`], with the profiles and their agents the
/// daemon's, reached over its sockets, and nothing hosted here.
///
/// Only the Mac app attaches, so the runtime names itself the Mac app to
/// the daemon: on every profile socket it opens, which the daemon counts
/// as the Mac app opening, and in what it answers of setting up.
pub fn attach(front_door: &std::path::Path, tail: u32) -> Result<AmuxRuntime, String> {
    let phone = Phone::attached().map_err(|error| error.to_string())?;
    let door = front_door.to_owned();
    let installation = phone
        .wait(async move { client::Installation::connect(&door, Surface::Mac).await })
        .ok_or("the runtime has stopped")?
        .map_err(|error| format!("amux is not answering on {}: {error}", front_door.display()))?;
    serve(phone, installation, tail)
}

/// Attaches to amux on this machine from an `AttachConfig` as JSON.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub fn attach_from_json(config: Option<&str>) -> Result<AmuxRuntime, String> {
    let config: app_runtime::values::AttachConfig = parse(config)
        .ok_or_else(|| "the attach configuration is not an AttachConfig".to_owned())?;
    let door =
        installer::front_door(config.config.as_deref()).map_err(|error| error.to_string())?;
    attach(&door, config.tail)
}

/// Ensures amux is installed and answering, from `InstallOptions` as JSON,
/// returning an `Installed` serialized as JSON.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub fn install_from_json(options: Option<&str>) -> Result<String, String> {
    let options: app_runtime::values::InstallOptions =
        parse(options).ok_or_else(|| "the options are not InstallOptions".to_owned())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let options = installer::Options {
        config: options.config,
    };
    let outcome = runtime
        .block_on(installer::ensure(&options, |_| {}))
        .map_err(|error| error.to_string())?;
    let installed = match outcome {
        installer::Outcome::Running => app_runtime::values::Installed::Running,
        installer::Outcome::Started => app_runtime::values::Installed::Started,
        installer::Outcome::Installed { version } => {
            app_runtime::values::Installed::Installed { version }
        }
    };
    serde_json::to_string(&installed).map_err(|error| error.to_string())
}

/// Checks for a newer version of the Mac app, from an `UpdateCheck` as JSON,
/// returning an `Option<AppUpdate>` serialized as JSON.
#[cfg(target_os = "macos")]
pub fn check_for_update_from_json(check: Option<&str>) -> Result<String, String> {
    let check: app_runtime::values::UpdateCheck =
        parse(check).ok_or_else(|| "the check is not an UpdateCheck".to_owned())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let options = installer::update::Options {
        config: check.config,
        current: check.current,
        dir: check.dir,
    };
    let staged = runtime
        .block_on(installer::update::check(&options))
        .map_err(|error| error.to_string())?;
    let update = staged.map(|staged| app_runtime::values::AppUpdate {
        version: staged.version,
        app: staged.app,
    });
    serde_json::to_string(&update).map_err(|error| error.to_string())
}

/// How soon a stream from the daemon is opened again after it ends,
/// doubling up to the most while the daemon does not answer.
const RESUBSCRIBE_FIRST: std::time::Duration = std::time::Duration::from_millis(250);
const RESUBSCRIBE_MOST: std::time::Duration = std::time::Duration::from_secs(5);

/// Hands `each` every message of the stream `open` opens, for as long as
/// the task runs. An attached daemon restarts under the app (an update, a
/// stop and start), which ends the stream; it is opened again once the
/// daemon answers, and every stream followed this way starts with the state
/// now, so nothing that moved meanwhile is missed.
async fn follow<T, Opened>(
    mut open: impl FnMut() -> Opened,
    mut each: impl FnMut(T),
) -> std::convert::Infallible
where
    Opened: Future<Output = Result<client::EventStream<T>, client::RpcError>>,
{
    use futures_util::StreamExt as _;
    let mut pause = RESUBSCRIBE_FIRST;
    loop {
        if let Ok(mut stream) = open().await {
            while let Some(Ok(now)) = stream.next().await {
                each(now);
                pause = RESUBSCRIBE_FIRST;
            }
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(RESUBSCRIBE_MOST);
    }
}

/// Serves the profile list to the app: posted now, for the first step, and
/// again whenever the installation's profile stream says it moved.
fn serve(
    phone: Phone,
    installation: client::Installation,
    tail: u32,
) -> Result<AmuxRuntime, String> {
    let outbox = Arc::new(Outbox {
        tickets: AtomicU64::new(0),
        fleets: AtomicU64::new(0),
        waiting: Mutex::new(Waiting::default()),
        open: Mutex::new(HashMap::new()),
        ring: Box::new(phone.bell()),
    });
    // The stream begins before the list is first read, so no move falls
    // between them; its first message is the list as it stands.
    let watching = installation.clone();
    let mut moves = phone
        .wait(async move { watching.subscribe_profiles().await })
        .ok_or("the runtime has stopped")?
        .map_err(|error| error.to_string())?;
    // The list as the start left it waits for the first step, so the app
    // has it before anything else the runtime hands over.
    let listing = installation.clone();
    let listed = phone
        .wait(async move { listing.list_profiles().await })
        .ok_or("the runtime has stopped")?
        .map_err(|error| error.to_string())?;
    outbox.post_listed(&listed);
    let posting = outbox.clone();
    let resubscribing = installation.clone();
    let watch = phone.handle().spawn(async move {
        use futures_util::StreamExt as _;
        // An attached daemon restarts under the app (an update, a stop and
        // start), which ends the stream; it is opened again once the front
        // door answers, and its first message is the whole list, so nothing
        // that moved meanwhile is missed.
        let mut pause = RESUBSCRIBE_FIRST;
        loop {
            while let Some(Ok(moved)) = moves.next().await {
                posting.post_listed(&moved.profiles);
                pause = RESUBSCRIBE_FIRST;
            }
            loop {
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(RESUBSCRIBE_MOST);
                if let Ok(again) = resubscribing.subscribe_profiles().await {
                    moves = again;
                    break;
                }
            }
        }
    });
    Ok(AmuxRuntime {
        shared: Arc::new(Shared {
            phone,
            installation,
            outbox,
        }),
        watch: Some(watch),
        tail,
    })
}

/// Opens a profile's fleet; blocks, stepping the cores, until it has
/// caught up with the profile's store.
fn open_profile(runtime: &AmuxRuntime, profile: ProfileId) -> Result<AmuxProfile, String> {
    let shared = runtime.shared.clone();
    let installation = shared.installation.clone();
    let clock: Arc<dyn client::Clock> = match shared.embedded() {
        Some(embedded) => embedded.clock(),
        None => Arc::new(client::SystemClock),
    };
    let outbox = shared.outbox.clone();
    let fleet = outbox.fleets.fetch_add(1, Ordering::SeqCst) + 1;
    let app = shared
        .phone
        .wait(async move {
            let host_wake = Arc::new(move |moved: Wake| {
                let chat = match moved {
                    Wake::Fleet => 0,
                    Wake::Chat(id) => id,
                };
                outbox.redraw(fleet, chat)
            });
            let listed = installation
                .list_profiles()
                .await
                .map_err(|error| error.to_string())?;
            let described = listed
                .iter()
                .find(|listed| listed.id == profile.as_bytes())
                .ok_or_else(|| format!("the installation hosts no profile {profile}"))?;
            // The phone's installation opens its hosted node in memory; the
            // Mac's dials the profile's socket, as the Mac app.
            let client = installation
                .open_profile(described)
                .await
                .map_err(|error| error.to_string())?;
            let host = client
                .admin()
                .get_device_identity()
                .await
                .map_err(|error| error.to_string())?
                .host_id;
            AppRuntime::open(client, clock, host, host_wake)
                .await
                .map_err(|error| error.to_string())
        })
        .ok_or("the runtime has stopped")??;
    let app = Arc::new(app);
    shared
        .outbox
        .opened(fleet, 0, View::Fleet(Arc::downgrade(&app)));
    let (watching, posting) = (app.client().admin(), shared.outbox.clone());
    let offers = shared.phone.handle().spawn(async move {
        follow(
            || watching.subscribe_sign_in_offers(),
            |now: wire::SubscribeSignInOffersResponse| {
                posting.post_offers(
                    fleet,
                    now.offers.iter().map(SignInOfferView::from).collect(),
                );
            },
        )
        .await;
    });
    Ok(AmuxProfile {
        profile,
        fleet,
        shared,
        app,
        tail: runtime.tail,
        offers,
        onboarding: Mutex::new(None),
        pairing: Mutex::new(None),
    })
}

fn overrides(config: &StartConfig) -> EdgeOverrides {
    let mut overrides = EdgeOverrides {
        // Only the app as shipped sends analytics: the bridge built
        // optimised and without the driving tools, which development,
        // simulators and test runs always carry.
        published: !cfg!(debug_assertions) && !cfg!(feature = "debug-tools"),
        ..EdgeOverrides::default()
    };
    // A driving build may keep direct links on loopback, so a simulator
    // run never listens on the machine's network.
    if cfg!(feature = "debug-tools")
        && let Some(bind) = config.lan_bind
    {
        overrides.lan_bind = bind;
    }
    // And may dial a served test relay's carriers: TCP in plaintext, QUIC
    // trusting the relay's own certificate.
    if cfg!(feature = "debug-tools") {
        overrides.cloud.relay_tcp = config.relay_tcp;
        if let (Some(addr), Some(root)) = (config.relay_quic, &config.relay_root)
            && let Some(der) = from_hex(root)
        {
            overrides.cloud.relay_quic = node::RelayQuic::trusting(addr, der).ok();
        }
    }
    overrides
}

fn from_hex(hex: &str) -> Option<Vec<u8>> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect()
}

// --- AmuxRuntime -----------------------------------------------------------

impl AmuxRuntime {
    /// The installation hosted in process; none when attached. For tests
    /// that reach past the bridge into the installation.
    #[doc(hidden)]
    pub fn embedded(&self) -> Option<&Arc<EmbeddedRuntime>> {
        self.shared.embedded()
    }

    /// The transport runtime handle. For tests that reach past the bridge
    /// into the installation.
    #[doc(hidden)]
    pub fn handle(&self) -> &Handle {
        self.shared.handle()
    }

    fn act<T, F>(&self, act: F) -> u64
    where
        T: Serialize,
        F: Future<Output = T> + Send + 'static,
    {
        self.shared.act(act)
    }

    /// Runs an act on the hosted installation that names a profile, answering a
    /// profile id that does not parse without running it.
    fn profile_act<T, F, Fut>(&self, profile: Option<&str>, act: F) -> u64
    where
        T: Serialize + Send + 'static,
        F: FnOnce(Arc<EmbeddedRuntime>, ProfileId) -> Fut,
        Fut: Future<Output = Answered<T>> + Send + 'static,
    {
        match (profile_id(profile), self.embedded()) {
            (Some(profile), Some(embedded)) => self.act(act(embedded.clone(), profile)),
            (None, _) => {
                self.act(async { Answered::<T>::Err("the profile is not a profile id".into()) })
            }
            (_, None) => self.act(async { Answered::<T>::Err(ATTACHED.into()) }),
        }
    }

    /// As [`Self::profile_act`], for an act on the installation's profiles,
    /// through whichever installation the app has: the list it left is posted
    /// before its answer.
    fn profile_act_on_profiles<T, F, Fut>(&self, profile: Option<&str>, act: F) -> u64
    where
        T: Serialize + Send + 'static,
        F: FnOnce(client::Installation, ProfileId) -> Fut,
        Fut: Future<Output = Answered<T>> + Send + 'static,
    {
        match profile_id(profile) {
            Some(profile) => self
                .shared
                .act_on_profiles(act(self.shared.installation.clone(), profile)),
            None => {
                self.act(async { Answered::<T>::Err("the profile is not a profile id".into()) })
            }
        }
    }

    /// A descriptor that becomes readable whenever something waits for
    /// [`Self::step`].
    pub fn wake_fd(&self) -> i32 {
        self.shared.phone.wake_fd()
    }

    /// Makes [`Self::wake_fd`] readable from any thread, so an app thread
    /// waiting on it returns, to step or to see that the runtime stopped.
    pub fn ring(&self) {
        self.shared.phone.ring();
    }

    /// Steps every input waiting for the cores on the calling queue, then
    /// answers what the app must do as JSON.
    pub fn step(&self) -> Option<String> {
        self.shared.phone.step();
        let _entered = self.handle().enter();
        json(&self.shared.outbox.take())
    }

    /// Feeds a [`Lifecycle`] input; blocks, stepping the cores, until carried
    /// out.
    pub fn feed(&self, input: Lifecycle) -> Result<(), String> {
        // A daemon keeps its own links and analytics; an attached app's life
        // is nothing to it.
        let Some(embedded) = self.embedded().cloned() else {
            return Ok(());
        };
        match input {
            Lifecycle::Foreground => {
                {
                    let _entered = self.handle().enter();
                    embedded.client_opened();
                }
                self.resume(embedded)?;
            }
            Lifecycle::Woken => self.resume(embedded)?,
            Lifecycle::Background => {
                self.shared
                    .phone
                    .wait(async move { embedded.flush_analytics().await })
                    .ok_or("the runtime has stopped")?;
            }
            Lifecycle::Suspending => {
                self.shared
                    .phone
                    .wait(async move {
                        embedded.suspend().await;
                        embedded.flush_analytics().await;
                        embedded.checkpoint().await
                    })
                    .ok_or("the runtime has stopped")?
                    .map_err(|error| error.to_string())?;
            }
            Lifecycle::NetworkChanged => {
                self.shared
                    .phone
                    .wait(async move { embedded.network_changed().await })
                    .ok_or("the runtime has stopped")?
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    /// Feeds a [`Lifecycle`] input encoded as JSON.
    pub fn feed_json(&self, input: Option<&str>) -> Result<(), String> {
        let input: Lifecycle = parse(input).ok_or("the input is not a Lifecycle")?;
        self.feed(input)
    }

    fn resume(&self, embedded: Arc<EmbeddedRuntime>) -> Result<(), String> {
        self.shared
            .phone
            .wait(async move { embedded.resume().await })
            .ok_or("the runtime has stopped")?;
        Ok(())
    }

    /// Stops the installation: flushes every profile's store and ends its
    /// transport. Close every chat and profile first.
    pub fn stop(mut self) {
        // The watch holds the installation until its task is dropped on the
        // transport, and the phone shuts down only an installation nothing
        // else holds: wait for the abort to land.
        if let Some(watch) = self.watch.take() {
            watch.abort();
            let _ = self.shared.phone.wait(async move {
                let _ = watch.await;
            });
        }
        // A profile or chat left open holds the installation; it stops
        // once the last of them closes.
        if let Some(shared) = Arc::into_inner(self.shared) {
            shared.phone.stop();
        }
    }

    // --- the profile registry ----------------------------------------------

    /// Creates a profile nobody has signed in on; the ticket answers
    /// `{"Ok": ProfileView}` or `{"Err": ..}`.
    pub fn create_profile(&self) -> u64 {
        let installation = self.shared.installation.clone();
        self.shared.act_on_profiles(async move {
            view(
                installation
                    .create_profile(wire::CreateProfileRequest::default())
                    .await,
            )
        })
    }

    /// Deletes a profile by its id; the ticket answers `{"Ok": null}` or
    /// `{"Err": ..}`.
    pub fn delete_profile(&self, profile: Option<&str>) -> u64 {
        self.profile_act_on_profiles(profile, |installation, profile| async move {
            Answered::from(
                installation
                    .delete_profile(wire::DeleteProfileRequest {
                        profile_id: profile.as_bytes().to_vec(),
                    })
                    .await,
            )
        })
    }

    /// Binds a profile to an account with the refresh token obtained for
    /// `client_id`.
    pub fn bind(
        &self,
        profile: Option<&str>,
        cloud_url: &str,
        client_id: &str,
        refresh_token: &str,
    ) -> u64 {
        let named = profile.map(str::to_owned);
        let url = cloud_url.to_owned();
        let client = client_id.to_owned();
        let token = refresh_token.to_owned();
        let installation = self.shared.installation.clone();
        self.shared.act_on_profiles(async move {
            let profile = match named.map(|named| named.parse::<ProfileId>()) {
                None => None,
                Some(Ok(profile)) => Some(profile),
                Some(Err(_)) => return Answered::Err("the profile is not a profile id".into()),
            };
            // A profile the person named is theirs to sign in, whatever it
            // already holds.
            view(
                installation
                    .bind_profile(wire::BindProfileRequest {
                        profile_id: profile.map(|profile| profile.as_bytes().to_vec()),
                        cloud_url: url,
                        staged_refresh_token: token,
                        adopt_non_pristine: profile.is_some(),
                        client_id: client,
                    })
                    .await,
            )
        })
    }

    /// Signs a profile out of its account; it stays tied to the account.
    pub fn sign_out(&self, profile: Option<&str>) -> u64 {
        self.profile_act_on_profiles(profile, |installation, profile| async move {
            let signed_out = async {
                let listed = installation.list_profiles().await?;
                let described = listed
                    .iter()
                    .find(|listed| listed.id == profile.as_bytes())
                    .ok_or_else(|| {
                        client::RpcError::Refused(wire::Error::new(
                            wire::ErrorCode::NotFound,
                            format!("no profile {profile}"),
                        ))
                    })?;
                installation
                    .open_profile(described)
                    .await?
                    .admin()
                    .sign_out()
                    .await?;
                let listed = installation.list_profiles().await?;
                listed
                    .into_iter()
                    .find(|listed| listed.id == profile.as_bytes())
                    .ok_or_else(|| {
                        client::RpcError::Refused(wire::Error::new(
                            wire::ErrorCode::NotFound,
                            format!("no profile {profile}"),
                        ))
                    })
            };
            view(signed_out.await)
        })
    }

    /// Every profile whose trust store holds a host, by its id as a JSON byte
    /// array, oldest first.
    pub fn trusting_profiles(&self, host_id: Option<&str>) -> u64 {
        let host_id: Option<Vec<u8>> = parse(host_id);
        let Some(embedded) = self.embedded().cloned() else {
            return self.act(async { Answered::<()>::Err(ATTACHED.into()) });
        };
        self.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(embedded.trusting(&host_id).map(|profiles| {
                profiles
                    .iter()
                    .map(ProfileId::to_string)
                    .collect::<Vec<_>>()
            }))
        })
    }

    /// Opens pairing mode on a profile and answers the link its QR code would
    /// carry; only a driving build offers one.
    pub fn offer_pairing(&self, profile: Option<&str>) -> u64 {
        self.profile_act(profile, |embedded, profile| async move {
            if !cfg!(feature = "debug-tools") {
                return Answered::Err("only a driving build offers pairing".into());
            }
            Answered::from(embedded.offer_pairing(profile).await)
        })
    }

    /// Keeps a source open for every listed agent of `profile`, and only for
    /// open chats on every other profile (or none when `profile` is `None`).
    pub fn list_sources(&self, profile: Option<&str>) {
        let shown = profile_id(profile);
        // Switching opens and closes sources, which run on the pool.
        let _entered = self.handle().enter();
        if let Some(embedded) = self.embedded() {
            embedded.list_sources(shown);
        }
    }

    /// Whether every listed agent of a profile keeps a source open.
    pub fn source_policy_listed(&self, profile: Option<&str>) -> bool {
        profile_id(profile).is_some_and(|profile| {
            self.embedded().is_some_and(|embedded| {
                embedded.source_policy(profile).ok() == Some(SourcePolicy::Listed)
            })
        })
    }

    /// Hands over the whole set the phone's browser found, as `[Found]`, to
    /// every profile.
    pub fn discovered(&self, found: Option<&str>) {
        if let Some(found) = parse::<Vec<Found>>(found) {
            let _entered = self.handle().enter();
            if let Some(embedded) = self.embedded() {
                embedded.discovered(found);
            }
        }
    }

    /// A bearer for a profile's account, which the profile refreshes.
    pub fn access_token(&self, profile: Option<&str>) -> u64 {
        self.profile_act(profile, |embedded, profile| async move {
            Answered::from(embedded.access_token(profile).await)
        })
    }

    /// A profile's host id, which notifications for it are addressed to.
    pub fn host_id(&self, profile: Option<&str>) -> u64 {
        self.profile_act(profile, |embedded, profile| async move {
            Answered::from(embedded.host_id(profile))
        })
    }

    /// Says whether a profile wants notifications: `key` is a JSON byte array
    /// or `"null"` to take the wish back.
    pub fn want_notifications(&self, profile: Option<&str>, key: Option<&str>) -> u64 {
        let key: Option<Option<Vec<u8>>> = parse(key);
        self.profile_act(profile, |embedded, profile| async move {
            let Some(key) = key else {
                return Answered::Err("the key is not a byte array".into());
            };
            Answered::from(embedded.want_notifications(profile, key))
        })
    }

    /// Asks the account service what a profile's account buys now, over its
    /// relay link.
    pub fn refresh_entitlement(&self, profile: Option<&str>) -> u64 {
        self.profile_act(profile, |embedded, profile| async move {
            Answered::from(embedded.refresh_entitlement(profile).await)
        })
    }

    // --- analytics ---------------------------------------------------------

    /// The app's telemetry setting changed.
    pub fn set_telemetry(&self, on: bool) {
        if let Some(embedded) = self.embedded() {
            embedded.set_telemetry(on);
        }
    }

    /// The app's automatic-reports setting changed.
    pub fn set_auto_reports(&self, on: bool) {
        if let Some(embedded) = self.embedded() {
            embedded.set_auto_reports(on);
        }
    }

    /// Records what only the app sees, a `UsageEvent` as JSON, on `profile`,
    /// or on every profile when `profile` is `None` or not hosted.
    pub fn record(&self, profile: Option<&str>, event: Option<&str>) {
        let (profile, event) = (profile_id(profile), parse::<UsageEvent>(event));
        if let (Some(event), Some(embedded)) = (event, self.embedded()) {
            embedded.record(profile, event);
        }
    }

    // --- settings ----------------------------------------------------------

    /// The installation's everyday settings.
    pub fn settings(&self) -> u64 {
        let installation = self.shared.installation.clone();
        self.act(async move {
            Answered::from(
                installation
                    .get_settings()
                    .await
                    .map(|settings| MachineSettings::from(&settings)),
            )
        })
    }

    /// Changes the installation's settings by a `SettingsChange` as JSON.
    pub fn update_settings(&self, change: Option<&str>) -> u64 {
        let change: Option<SettingsChange> = parse(change);
        let installation = self.shared.installation.clone();
        self.act(async move {
            let Some(change) = change else {
                return Answered::Err("the change is not a SettingsChange".into());
            };
            Answered::from(
                installation
                    .update_settings(change.into())
                    .await
                    .map(|settings| MachineSettings::from(&settings)),
            )
        })
    }

    /// The folders worth offering as repositories.
    pub fn suggest_repositories(&self) -> u64 {
        let installation = self.shared.installation.clone();
        self.act(async move {
            Answered::from(installation.suggest_repositories().await.map(|suggested| {
                suggested
                    .into_iter()
                    .map(|entry| RepositorySuggestion {
                        repository: ui_view::Repository::of_checkout(
                            entry.checkout.as_ref(),
                            &entry.path,
                        ),
                        path: entry.path,
                        name: entry.name,
                    })
                    .collect::<Vec<_>>()
            }))
        })
    }

    /// A one-time link that signs the browser in to a profile's account and
    /// opens `path` on its account service.
    pub fn web_sign_in_link(&self, profile: Option<&str>, path: Option<&str>) -> u64 {
        let (profile, path) = (profile_id(profile), path.map(str::to_owned));
        let installation = self.shared.installation.clone();
        self.act(async move {
            let Some(profile) = profile else {
                return Answered::Err("the profile is not a profile id".into());
            };
            Answered::from(
                installation
                    .web_sign_in_link(wire::WebSignInLinkRequest {
                        profile_id: profile.as_bytes().to_vec(),
                        path: path.unwrap_or_default(),
                    })
                    .await,
            )
        })
    }

    /// Opens a profile's fleet by its id string.
    pub fn open_profile(&self, profile: Option<&str>) -> Result<AmuxProfile, String> {
        let profile = profile_id(profile).ok_or("the profile is not a profile id")?;
        open_profile(self, profile)
    }
}

// --- AmuxProfile -----------------------------------------------------------

impl AmuxProfile {
    /// Starts an act on the profile; what it moved in the fleet is handed
    /// over before its answer.
    fn act<T, F>(&self, act: F) -> u64
    where
        T: Serialize + Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let outbox = self.shared.outbox.clone();
        let fleet = self.fleet;
        self.shared.act(async move {
            let answer = act.await;
            outbox.redraw(fleet, 0);
            answer
        })
    }

    /// Closes the profile's fleet. Close its chats first.
    pub fn close(self) {
        self.shared.outbox.close(self.fleet, None);
        let shared = self.shared.clone();
        let _entered = shared.handle().enter();
        drop(self);
    }

    /// The number deltas name this fleet by.
    pub fn fleet_id(&self) -> u64 {
        self.fleet
    }

    /// Writes a dump of the profile with the fleet's part and the record of
    /// every chat on screen.
    pub fn dump(&self, reason: &str) -> u64 {
        let reason = reason.to_owned();
        let app = self.app.clone();
        self.act(async move { Answered::from(app.dump(&reason).await) })
    }

    /// Writes a person's report around the dump at `dump_path` and sends it
    /// to the account the profile is signed in to.
    pub fn submit_report(&self, dump_path: &str, header: Vec<u8>, frame: Vec<u8>) -> u64 {
        let dump_path = dump_path.to_owned();
        let app = self.app.clone();
        self.act(async move { Answered::from(app.submit_report(dump_path, header, frame).await) })
    }

    /// Reaches and authenticates a machine by a `PairRequest` as JSON.
    pub fn begin_pair(&self, request: Option<&str>) -> u64 {
        let request: Option<PairRequest> = parse(request);
        let Some(embedded) = self.shared.embedded().cloned() else {
            return self.act(async { Answered::<()>::Err(ATTACHED.into()) });
        };
        let id = self.profile;
        self.act(async move {
            let Some(request) = request else {
                return Answered::Err("the request is not a PairRequest".into());
            };
            Answered::from(embedded.begin_pair(id, &request).await)
        })
    }

    /// Offers the paired machine whose host id `host_id` names, as a JSON byte
    /// array, this profile's account.
    pub fn offer_sign_in(&self, host_id: Option<&str>) -> u64 {
        let host_id: Option<Vec<u8>> = parse(host_id);
        let admin = self.app.client().admin();
        self.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(admin.offer_sign_in(host_id).await)
        })
    }

    /// Answers a paired machine's offer of its account, a `SignInAnswer` as
    /// JSON.
    pub fn answer_sign_in_offer(&self, answer: Option<&str>) -> u64 {
        let answer: Option<SignInAnswer> = parse(answer);
        let admin = self.app.client().admin();
        self.act(async move {
            let Some(answer) = answer else {
                return Answered::Err("the answer is not a SignInAnswer".into());
            };
            let answered = admin
                .answer_sign_in_offer(wire::AnswerSignInOfferRequest {
                    host_id: answer.host_id,
                    accept: answer.accept,
                })
                .await;
            match answered {
                Ok(profile) if answer.accept => {
                    Answered::Ok(SignInOfferOutcome::SignedIn(ProfileView::from(&profile)))
                }
                Ok(_) => Answered::Ok(SignInOfferOutcome::Declined),
                // Said apart from every other refusal: the person's next
                // step is on the other machine, not on this one.
                Err(error) if error.code() == Some(wire::ErrorCode::Unavailable) => {
                    Answered::Ok(SignInOfferOutcome::Unreachable)
                }
                Err(error) => Answered::Err(error.to_string()),
            }
        })
    }

    /// Follows the profile's setting up until the profile closes.
    pub fn watch_onboarding(&self) {
        let mut watch = self.onboarding.lock().unwrap_or_else(|p| p.into_inner());
        if watch.is_some() {
            return;
        }
        let installation = self.shared.installation.clone();
        let outbox = self.shared.outbox.clone();
        let (fleet, id) = (self.fleet, self.profile);
        *watch = Some(self.shared.handle().spawn(async move {
            follow(
                || {
                    installation.subscribe_onboarding(wire::SubscribeOnboardingRequest {
                        profile_id: id.as_bytes().to_vec(),
                    })
                },
                |now: wire::Onboarding| {
                    outbox.post_onboarding(fleet, OnboardingState::from(&now));
                },
            )
            .await;
        }));
    }

    /// Tells the daemon what the person did with setting up this profile, an
    /// `OnboardingAnswer` as JSON.
    pub fn answer_onboarding(&self, answer: Option<&str>) -> u64 {
        let answer: Option<OnboardingAnswer> = parse(answer);
        let installation = self.shared.installation.clone();
        let id = self.profile;
        self.act(async move {
            let Some(answer) = answer else {
                return Answered::Err("the answer is not an OnboardingAnswer".into());
            };
            Answered::from(
                installation
                    .answer_onboarding(wire::AnswerOnboardingRequest {
                        profile_id: id.as_bytes().to_vec(),
                        answer: Some(answer.into()),
                    })
                    .await
                    .map(|now| OnboardingState::from(&now)),
            )
        })
    }

    /// Opens a pairing window on the profile and follows its status until
    /// [`Self::close_pairing`].
    pub fn open_pairing(&self, ttl_seconds: u64) -> u64 {
        let admin = self.app.client().admin();
        {
            let mut watch = self.pairing.lock().unwrap_or_else(|p| p.into_inner());
            if watch.is_none() {
                let (watching, outbox, fleet) =
                    (admin.clone(), self.shared.outbox.clone(), self.fleet);
                *watch = Some(self.shared.handle().spawn(async move {
                    follow(
                        || watching.subscribe_pairing(),
                        |status: wire::PairingStatus| {
                            outbox.post_pairing(fleet, PairingStatusView::from(&status));
                        },
                    )
                    .await;
                }));
            }
        }
        self.act(async move {
            // One window is open at a time: a renewal replaces the one
            // before, which may already have ended.
            let _ = admin.cancel_pairing().await;
            Answered::from(
                admin
                    .start_pairing(wire::StartPairingRequest {
                        method: Some(wire::start_pairing_request::Method::PinOrQr(wire::Empty {})),
                        ttl_seconds: (ttl_seconds > 0).then_some(ttl_seconds),
                    })
                    .await
                    .map(|window| PairingWindowView::from(&window)),
            )
        })
    }

    /// Closes the profile's pairing window and stops following it.
    pub fn close_pairing(&self) -> u64 {
        if let Some(watch) = self
            .pairing
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            watch.abort();
        }
        let admin = self.app.client().admin();
        self.act(async move { Answered::from(admin.cancel_pairing().await) })
    }

    /// Trusts the machine a pending pairing reached, named by its token as a
    /// JSON byte array.
    pub fn confirm_pair(&self, token: Option<&str>) -> u64 {
        let token: Option<Vec<u8>> = parse(token);
        let Some(embedded) = self.shared.embedded().cloned() else {
            return self.act(async { Answered::<()>::Err(ATTACHED.into()) });
        };
        let id = self.profile;
        self.act(async move {
            let Some(token) = token else {
                return Answered::Err("the token is not a byte array".into());
            };
            Answered::from(embedded.confirm_pair(id, &token).await.map(drop))
        })
    }

    /// Turns away the machine a pending pairing reached.
    pub fn abandon_pair(&self, token: Option<&str>) -> u64 {
        let token: Option<Vec<u8>> = parse(token);
        let Some(embedded) = self.shared.embedded().cloned() else {
            return self.act(async { Answered::<()>::Err(ATTACHED.into()) });
        };
        let id = self.profile;
        self.act(async move {
            let Some(token) = token else {
                return Answered::Err("the token is not a byte array".into());
            };
            Answered::from(embedded.abandon_pair(id, &token).await)
        })
    }

    /// Stops trusting a paired machine, by its host id as a JSON byte array.
    pub fn unpair(&self, host_id: Option<&str>) -> u64 {
        let host_id: Option<Vec<u8>> = parse(host_id);
        let Some(embedded) = self.shared.embedded().cloned() else {
            return self.act(async { Answered::<()>::Err(ATTACHED.into()) });
        };
        let id = self.profile;
        self.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(embedded.unpair(id, &host_id).await)
        })
    }

    /// The profile's identity and the machines it trusts.
    pub fn roster(&self) -> u64 {
        let Some(embedded) = self.shared.embedded().cloned() else {
            return self.act(async { Answered::<()>::Err(ATTACHED.into()) });
        };
        let id = self.profile;
        self.act(async move { Answered::from(embedded.roster(id)) })
    }

    /// Starts an agent from a `NewAgent` as JSON.
    pub fn create_agent(&self, agent: Option<&str>) -> u64 {
        let agent: Option<NewAgent> = parse(agent);
        let app = self.app.clone();
        self.act(async move {
            let Some(agent) = agent else {
                return Answered::Err("the agent is not a NewAgent".into());
            };
            Answered::from(app.create_agent(&agent).await)
        })
    }

    /// Where a host offers to start an agent, matching `query`, at most
    /// `limit` of each.
    pub fn directories(&self, host_id: Option<&str>, query: &str, limit: u32) -> u64 {
        let host_id: Option<Vec<u8>> = parse(host_id);
        let query = query.to_owned();
        let app = self.app.clone();
        self.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(app.directories(&host_id, &query, limit).await)
        })
    }

    /// Pins or unpins `path` among a host's repositories.
    pub fn pin_folder(&self, host_id: Option<&str>, path: &str, pinned: bool) -> u64 {
        let host_id: Option<Vec<u8>> = parse(host_id);
        let path = path.to_owned();
        let app = self.app.clone();
        self.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(app.pin_folder(&host_id, &path, pinned).await)
        })
    }

    /// Renames, stops or deletes an agent, named by its `AgentKey`, with an
    /// `AgentAct`, both as JSON.
    pub fn agent_act(&self, agent: Option<&str>, act: Option<&str>) -> u64 {
        let agent: Option<AgentKey> = parse(agent);
        let act: Option<AgentAct> = parse(act);
        let app = self.app.clone();
        self.act(async move {
            let (Some(agent), Some(act)) = (agent, act) else {
                return Answered::Err("the agent or the act is not readable".into());
            };
            Answered::from(app.agent_act(&agent, &act).await)
        })
    }

    /// What deleting an agent does to its workspace, as the agent's host
    /// previews it: `Unknown` when the host could not be asked, `null` when
    /// the workspace stays.
    pub fn delete_warning(&self, agent: Option<&str>) -> u64 {
        let agent: Option<AgentKey> = parse(agent);
        let app = self.app.clone();
        self.act(async move {
            let Some(agent) = agent else {
                return Answered::Err("the agent is not readable".into());
            };
            Answered::Ok(app.delete_warning(&agent).await)
        })
    }

    /// What a provider, by its id as a JSON string ("claude", "codex", an
    /// ACP agent's), offers on a host with no agent running there.
    pub fn host_catalogue(&self, host_id: Option<&str>, provider: Option<&str>) -> u64 {
        let host_id: Option<Vec<u8>> = parse(host_id);
        let provider: Option<String> = parse(provider);
        let app = self.app.clone();
        self.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            let Some(provider) = provider else {
                return Answered::Err("the provider is not a provider id".into());
            };
            Answered::from(app.host_catalogue(&host_id, &provider).await)
        })
    }

    /// The app leaving (`false`) or returning to (`true`) the foreground.
    pub fn set_foreground(&self, foreground: bool) {
        let _entered = self.shared.handle().enter();
        self.app.set_foreground(foreground);
    }

    // --- the fleet ---------------------------------------------------------

    /// The fleet whole as a JSON `FleetDelta`.
    pub fn fleet_opening(&self) -> Option<String> {
        json(&self.app.fleet_opening())
    }

    /// Lists a family's members under its root, or folds them away again.
    pub fn fleet_toggle_family(&self, root: Option<&str>) {
        let Some(root) = parse::<Vec<u8>>(root) else {
            return;
        };
        self.app.toggle_family(&root);
        self.shared.outbox.redraw(self.fleet, 0);
    }

    /// Narrows home's list to what a `FleetFilter`, as JSON, keeps.
    pub fn fleet_filter(&self, filter: Option<&str>) {
        let filter: ui_view::FleetFilter = parse(filter).unwrap_or_default();
        self.app.set_filter(filter);
        self.shared.outbox.redraw(self.fleet, 0);
    }

    /// One agent's `FleetCard` as JSON, or `"null"` when not listed; `None`
    /// when `agent` does not parse.
    pub fn fleet_card(&self, agent: Option<&str>) -> Option<String> {
        let agent: AgentKey = parse(agent)?;
        json(&self.app.fleet_card(&agent))
    }

    /// The `FamilyHeader` over an agent's chat as JSON, or `"null"`; `None`
    /// when `agent` does not parse.
    pub fn fleet_family(&self, agent: Option<&str>) -> Option<String> {
        let agent: AgentKey = parse(agent)?;
        json(&self.app.family_header(&agent))
    }

    /// What settling an agent costs, as a JSON `SettleWarning`, or `"null"`
    /// for an agent the fleet does not hold; `None` when `agent` does not
    /// parse. Ask before settling when it cuts work short or ends a
    /// conversation nothing resumes.
    pub fn fleet_settle_warning(&self, agent: Option<&str>) -> Option<String> {
        let agent: AgentKey = parse(agent)?;
        json(&self.app.settle_warning(&agent))
    }

    /// Opens a chat on a fleet agent, named by its `AgentKey` as JSON, with
    /// `tail` rows (the configured tail when zero).
    pub fn open_chat(&self, agent: Option<&str>, tail: u32) -> Result<AmuxChat, String> {
        let agent: AgentKey = parse(agent).ok_or("the agent is not an AgentKey")?;
        let tail = if tail == 0 { self.tail } else { tail };
        let app = self.app.clone();
        let chat = self
            .shared
            .phone
            .wait(async move { app.open_chat(&agent, tail).await })
            .ok_or("the runtime has stopped")?
            .map_err(|error| error.to_string())?;
        self.shared
            .outbox
            .opened(self.fleet, chat.id(), View::Chat(Arc::downgrade(&chat)));
        Ok(AmuxChat {
            chat,
            fleet: self.fleet,
            shared: self.shared.clone(),
        })
    }
}

// --- AmuxChat --------------------------------------------------------------

impl AmuxChat {
    /// Runs a read on the chat inside its runtime, so a read that starts a
    /// fetch can spawn it.
    fn read<T>(&self, read: impl FnOnce(&Chat) -> T) -> T {
        let _entered = self.shared.handle().enter();
        read(&self.chat)
    }

    /// Starts an act on the chat and answers its ticket, after the step that
    /// hands over what it moved.
    fn start_act<T, F, Fut>(&self, act: F) -> u64
    where
        T: Serialize + Send + 'static,
        F: FnOnce(Arc<Chat>) -> Fut,
        Fut: Future<Output = T> + Send + 'static,
    {
        let acting = act(self.chat.clone());
        let outbox = self.shared.outbox.clone();
        let (fleet, id) = (self.fleet, self.chat.id());
        self.shared.act(async move {
            let answer = acting.await;
            outbox.redraw(fleet, id);
            answer
        })
    }

    /// The number deltas name this chat by, within its fleet.
    pub fn id(&self) -> u64 {
        self.chat.id()
    }

    /// Closes the chat.
    pub fn close(self) {
        self.shared.outbox.close(self.fleet, Some(self.chat.id()));
        let shared = self.shared.clone();
        let _entered = shared.handle().enter();
        drop(self);
    }

    /// Why an attachment of `size` bytes would be refused before anything
    /// of it is read or stored, as a JSON string naming the file; `"null"`
    /// when it may go.
    pub fn attachment_refusal(&self, name: &str, mime: &str, size: u64) -> Option<String> {
        self.read(|chat| json(&chat.attachment_refusal(name, mime, size)))
    }

    /// The chat whole as a JSON `ChatDelta`.
    pub fn opening(&self) -> Option<String> {
        self.read(|chat| json(&chat.opening()))
    }

    /// Tells the chat whether the reader is at the newest row.
    pub fn follow(&self, following: bool) {
        self.read(|chat| chat.follow(following));
    }

    /// The `Draft` this device kept for the agent when the chat opened, as
    /// JSON (`"null"` when none).
    pub fn kept_draft(&self) -> Option<String> {
        self.read(|chat| json(&chat.kept_draft()))
    }

    /// Keeps a `Draft`, as JSON, with this device's daemon.
    pub fn keep_draft(&self, draft: Option<&str>) {
        let Some(draft) = parse::<Draft>(draft) else {
            return;
        };
        self.read(|chat| chat.keep_draft(&draft));
    }

    /// `[Row]` for these keys (a JSON array of strings), in order.
    pub fn rows_for(&self, keys: Option<&str>) -> Option<String> {
        let keys: Vec<Key> = parse(keys).unwrap_or_default();
        self.read(|chat| json(&chat.rows_for(&keys)))
    }

    /// Opens or closes the run the row `member` belongs to.
    pub fn toggle_run(&self, member: &str) {
        self.read(|chat| chat.toggle_run(member));
    }

    /// Opens or closes the group of calls that opened agents the row `member`
    /// belongs to.
    pub fn toggle_group(&self, member: &str) {
        self.read(|chat| chat.toggle_group(member));
    }

    /// Folds each finished turn to its prompt, the line over its work and its
    /// answer (`true`), or only its runs (`false`).
    pub fn fold_turns(&self, on: bool) {
        self.read(|chat| chat.fold_turns(on));
    }

    /// Opens or closes the finished turn whose end is the row `end`.
    pub fn toggle_turn(&self, end: &str) {
        self.read(|chat| chat.toggle_turn(end));
    }

    /// Starts a `ChatAct`, as JSON, on the chat.
    pub fn act(&self, act: Option<&str>) -> u64 {
        let chat_act: Option<ChatAct> = parse(act);
        self.start_act(|chat| async move {
            match chat_act {
                Some(chat_act) => acted(&chat, chat_act).await,
                None => answer(Answered::<()>::Err("the act is not a ChatAct".into())),
            }
        })
    }

    /// A queued or sent prompt as the `Draft` it came from, as JSON.
    pub fn draft_of(&self, input_id: Option<&str>) -> Option<String> {
        let id: Vec<u8> = parse(input_id).unwrap_or_default();
        self.read(|chat| json(&chat.draft_of(&id)))
    }

    /// Forgets a not-confirmed input.
    pub fn discard(&self, input_id: Option<&str>) {
        let id: Vec<u8> = parse(input_id).unwrap_or_default();
        self.read(|chat| chat.discard(&id));
    }

    /// Asks the agent's host which files changed for `comparison` (a JSON
    /// `Comparison`), as the overview opens.
    pub fn open_overview(&self, comparison: Option<&str>) -> u64 {
        let comparison: Comparison = parse(comparison).unwrap_or_default();
        self.start_act(
            move |chat| async move { Answered::from(chat.open_overview(comparison).await) },
        )
    }

    /// Stores bytes to attach to a prompt.
    pub fn put_blob(&self, bytes: Vec<u8>, name: &str, mime: &str) -> u64 {
        let name = name.to_owned();
        let mime = mime.to_owned();
        self.start_act(move |chat| async move {
            Answered::from(chat.put_blob(&name, &mime, bytes).await)
        })
    }

    /// An attachment's bytes, by its hash as a JSON byte array, once fetched.
    pub fn blob(&self, hash: Option<&str>) -> Option<Arc<[u8]>> {
        let hash: Vec<u8> = parse(hash).unwrap_or_default();
        self.read(|chat| chat.blob(&hash))
    }

    /// Whether fetching an attachment's bytes failed, by its hash as a JSON
    /// byte array.
    pub fn blob_failed(&self, hash: Option<&str>) -> bool {
        let hash: Vec<u8> = parse(hash).unwrap_or_default();
        self.chat.blob_failed(&hash)
    }

    /// Fetches an attachment's bytes again, after a failure.
    pub fn fetch_blob(&self, hash: Option<&str>) {
        let hash: Vec<u8> = parse(hash).unwrap_or_default();
        self.read(|chat| chat.fetch_blob(&hash));
    }

    // --- the review page -----------------------------------------------------
    //
    // The agent's review page open on the chat: from `review_open` the
    // chat's deltas carry the page whole whenever it moved (`review`), until
    // `review_close`. The app draws the page as handed: it asks for the
    // bodies the page lists in `to_fetch`, for more context from a gap's
    // asks, and turns a selection of rows into an anchor here. Acts answer
    // by ticket with a `ReviewOutcome`, after the step that hands over what
    // they moved. The chip comes with the chat's frame (`review_chip`), and
    // a send with the review is the chat's `SendWithReview` act. A review
    // sent into the chat opens its round by `review_round`, apart from the
    // page.

    /// Starts an act on the chat's open review page.
    fn page_act<F, Fut>(&self, act: F) -> u64
    where
        F: FnOnce(Arc<ReviewSession>) -> Fut + Send + 'static,
        Fut: Future<Output = ReviewOutcome> + Send + 'static,
    {
        self.start_act(move |chat| async move {
            match chat.review_page() {
                Some(page) => act(page).await,
                None => ReviewOutcome::Failed("no review page is open on the chat".into()),
            }
        })
    }

    /// Opens the agent's review page on the chat, or opens the open page's
    /// review again. `base_branch` (`None` to keep the review's) picks its
    /// base anew; `refresh` takes a snapshot even with drafts, moving them
    /// onto it. The ticket answers a `ReviewOutcome`; the page itself comes
    /// with the chat's deltas, and says too what the open learned.
    pub fn review_open(&self, base_branch: Option<&str>, refresh: bool) -> u64 {
        let base_branch = base_branch.map(str::to_owned);
        self.start_act(move |chat| async move { chat.open_review(base_branch, refresh).await })
    }

    /// Opens the round a review sent into the chat closed: `sent` is the
    /// `SentReviewRef` its attachment draws with, as JSON. The ticket
    /// answers the round's `ReviewPage`, its whole diff and the comments it
    /// sent, read from the agent's host (or with the host out of reach, from
    /// what was loaded, saying so: with none of it loaded, a page with no
    /// header and only that banner), or JSON null when the message closed no
    /// round or the host no longer holds it, and the chip shows its text
    /// instead. It opens no page on the chat and moves none open there.
    pub fn review_round(&self, sent: Option<&str>) -> u64 {
        let sent: Option<SentRound> = parse(sent);
        self.start_act(move |chat| async move {
            let sent = sent?;
            chat.review_round(&sent.review_id, &sent.input_id).await
        })
    }

    /// Fetches file bodies for the view the page shows: `paths` as a JSON
    /// array of strings, those on screen first, or `None` for every file the
    /// page lists in `to_fetch`. The page moves as each batch lands; the
    /// ticket answers a `ReviewOutcome` once all have.
    pub fn review_files(&self, paths: Option<&str>) -> u64 {
        let paths: Option<Vec<String>> = match paths {
            None => Some(Vec::new()),
            paths => parse(paths),
        };
        self.page_act(move |page| async move {
            match paths {
                Some(paths) => page.files(paths).await,
                None => ReviewOutcome::Failed("the paths are not a JSON array of strings".into()),
            }
        })
    }

    /// Loads more context: a `ReviewContextAsk` as JSON, one of a gap's
    /// asks. The gap draws as loading until the lines arrive; the ticket
    /// answers a `ReviewOutcome`.
    pub fn review_context(&self, ask: Option<&str>) -> u64 {
        let ask: Option<ReviewContextAsk> = parse(ask);
        self.page_act(move |page| async move {
            match ask {
                Some(ask) => page.context(ask).await,
                None => ReviewOutcome::Failed("the ask is not a ReviewContextAsk".into()),
            }
        })
    }

    /// Changes the review at the agent's host: a JSON array of
    /// `ReviewChange`, made all or none. Works with no page open too, as the
    /// chip's discard does. The ticket answers a `ReviewOutcome`.
    pub fn review_edit(&self, changes: Option<&str>) -> u64 {
        let changes: Option<Vec<ReviewChange>> = parse(changes);
        self.start_act(move |chat| async move {
            match changes {
                Some(changes) => chat.edit_review(changes).await,
                None => ReviewOutcome::Failed("the changes are not ReviewChanges".into()),
            }
        })
    }

    /// Takes a `ReviewChoice`, as JSON: a view switched to, a file folded or
    /// opened, an outdated comment opened or folded. The next step hands
    /// over the page drawn so. Nothing happens with no page open or a
    /// choice that does not parse.
    pub fn review_choose(&self, choice: Option<&str>) {
        let Some(choice) = parse::<ReviewChoice>(choice) else {
            return;
        };
        self.read(|chat| {
            if let Some(page) = chat.review_page() {
                page.choose(choice);
            }
        });
    }

    /// The open review page as a `ReviewPage`, or JSON null with none open:
    /// for a model opening on a page already open. It is the page handed
    /// over from then, which anchors count rows of.
    pub fn review_page(&self) -> Option<String> {
        self.read(|chat| json(&chat.review_page().map(|page| page.read())))
    }

    /// Where a comment on a selection of the page's rows goes: the rows from
    /// index `from` to `to`, in either order, of the page handed over last,
    /// as a `ReviewAnchor` for a `Comment` change; JSON null for a selection
    /// with no line, across files, or with no page open.
    pub fn review_anchor(&self, from: u32, to: u32) -> Option<String> {
        self.read(|chat| {
            json(
                &chat
                    .review_page()
                    .and_then(|page| page.anchor(from as usize, to as usize)),
            )
        })
    }

    /// Closes the review page open on the chat: it stops following the
    /// agent's row, and no delta carries it again. Its acts in flight are
    /// answered all the same.
    pub fn review_close(&self) {
        self.read(|chat| chat.close_review());
    }
}

/// What a sent review's attachment names of its round, as the chat's rows
/// carry it (`SentReviewRef`).
#[derive(Deserialize)]
struct SentRound {
    review_id: Vec<u8>,
    input_id: Vec<u8>,
}

/// Runs an act on the chat and answers it as JSON.
async fn acted(chat: &Chat, act: ChatAct) -> serde_json::Value {
    match act {
        ChatAct::Send(draft) => answer(Answered::Ok(chat.send(&draft).await)),
        ChatAct::SendWithReview(draft) => {
            answer(Answered::from(chat.send_with_review(&draft).await))
        }
        ChatAct::Command(draft) => answer(chat.command(&draft).await),
        ChatAct::Answer { ask, choice, note } => {
            answer(chat.answer_choice(&ask, choice as usize, &note).await)
        }
        ChatAct::AnswerForm { ask, values } => answer(chat.answer_form(&ask, &values).await),
        ChatAct::AnswerQuestions { ask, responses } => {
            answer(chat.answer_questions(&ask, &responses).await)
        }
        ChatAct::ReplyInstead { ask, reply, so_far } => {
            answer(chat.reply_instead(&ask, &reply, &so_far).await)
        }
        ChatAct::ChangeSetting(change) => answer(chat.change_setting(&change).await),
        ChatAct::Withdraw(id) => answer(chat.withdraw(&id).await),
        ChatAct::SendNow(id) => answer(chat.send_now(&id).await),
        ChatAct::Resend(id) => answer(chat.resend(&id).await),
        ChatAct::Interrupt => answer(chat.interrupt().await),
        ChatAct::StopJob(key) => answer(chat.stop_job(&key).await),
        ChatAct::SwitchBack => answer(chat.switch_back().await),
        ChatAct::Resume(draft) => answer(chat.resume_with(&draft).await),
        ChatAct::PageOlder(n) => answer(chat.page_older(n).await),
    }
}

fn answer(value: impl Serialize) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// An outbox on its own, counting its rings.
    fn outbox() -> (Arc<Outbox>, Arc<AtomicU64>) {
        let rang = Arc::new(AtomicU64::new(0));
        let ringing = rang.clone();
        let outbox = Arc::new(Outbox {
            tickets: AtomicU64::new(0),
            fleets: AtomicU64::new(0),
            waiting: Mutex::new(Waiting::default()),
            open: Mutex::new(HashMap::new()),
            ring: Box::new(move || {
                ringing.fetch_add(1, Ordering::SeqCst);
            }),
        });
        (outbox, rang)
    }

    #[test]
    fn an_act_dropped_by_the_transports_shutdown_answers_that_the_runtime_stopped() {
        let transport = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (outbox, rang) = outbox();
        let ticket = outbox.act(transport.handle(), std::future::pending::<()>());
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            outbox.take().answers.is_empty(),
            "nothing answers while it runs"
        );

        transport.shutdown_timeout(Duration::from_secs(5));

        let answers = outbox.take().answers;
        assert_eq!(answers.len(), 1, "answered once");
        assert_eq!(answers[0].ticket, ticket);
        assert_eq!(answers[0].json, r#"{"Err":"the runtime has stopped"}"#);
        assert_eq!(rang.load(Ordering::SeqCst), 1, "and rang for its step");
    }

    #[test]
    fn an_act_that_finishes_answers_once_with_its_result() {
        let transport = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (outbox, rang) = outbox();
        let ticket = outbox.act(transport.handle(), async { 7u8 });
        let deadline = Instant::now() + Duration::from_secs(5);
        while rang.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "the act never answered");
            std::thread::sleep(Duration::from_millis(10));
        }
        transport.shutdown_timeout(Duration::from_secs(5));
        let answers = outbox.take().answers;
        assert_eq!(answers.len(), 1, "the drop does not answer again");
        assert_eq!((answers[0].ticket, answers[0].json.as_str()), (ticket, "7"));
    }

    #[test]
    fn version_and_stateless_helpers_match_shared_rules() {
        let v = version();
        assert!(v.starts_with(node::version()));
        assert_eq!(v.ends_with("+debug-tools"), cfg!(feature = "debug-tools"));
        assert_eq!(agent_name_problem("fix-login").as_deref(), Some("null"));
        assert_eq!(agent_name_problem("").as_deref(), Some("\"Empty\""));
        assert_eq!(attachment_limit(), wire::ATTACHMENT_SIZE_LIMIT as u64);
        assert!(pastes_as_attachment(&"line\n".repeat(8)));
        assert!(!pastes_as_attachment("two\nlines"));
        assert_eq!(from_hex("0a1bff"), Some(vec![0x0a, 0x1b, 0xff]));
        assert_eq!(from_hex("zz"), None);
    }
}
