//! First-run onboarding: the daemon's steps drawn full-screen before home,
//! one screen a step, each with "Later". Which step opens and which comes
//! next is the shared view's to say ([`ui_view::next_step`]); this module
//! words each step, takes its keys and says what the app should do about
//! them as [`SetupEffect`]s, and is told what came back.
//!
//! The pairing step holds one window that takes the PIN or the QR's secret
//! while it shows, renewed before it expires and closed when the step is
//! left. Pairing ends on the "Keep it connected" screen, which is the
//! reachability step. A step the daemon sees happen elsewhere (a phone
//! paired from another client, a sign-in from the phone) moves the screen
//! on by itself.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ui_state::{AgentKey, FleetState};
use ui_view::SetupStep;
use wire::answer_onboarding_request::Answer;

use crate::editor::Editor;
use crate::qr::Code;
use crate::text::{self, push};
use crate::theme::Theme;

const MARGIN: usize = 2;
/// The text column beside the QR code is never narrower than this.
const TEXT_LEAST: usize = 34;
/// Columns between the steps and the QR code beside them.
const GAP: usize = 4;
/// How long a pairing window is asked for. Long, so the PIN does not change
/// while someone types it; the screen renews it before it ends.
pub const WINDOW_SECONDS: u64 = 10 * 60;
/// How long before a window ends the screen renews it.
const RENEW_BEFORE_MS: i64 = 30_000;

/// The analytics notice, in the words every client shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notice {
    pub words: String,
    pub url: String,
}

/// What the app does for the onboarding screen.
#[derive(Clone, Debug, PartialEq)]
pub enum SetupEffect {
    /// Answer the daemon: a step shown, done or skipped, the notice shown.
    Answer(Answer),
    /// Change settings, then confirm the step when one is named; the
    /// screen moves on once both are taken.
    Confirm {
        settings: Box<wire::UpdateSettingsRequest>,
        step: Option<SetupStep>,
    },
    /// Rename this machine.
    Rename(String),
    /// Open the pairing window (a fresh one, closing any first) and watch it.
    OpenPairing,
    /// Close the pairing window and stop watching it.
    ClosePairing,
    /// Start the browser sign-in.
    SignIn,
    /// Abandon a browser sign-in in progress.
    CancelSignIn,
    /// Offer the paired host this account.
    OfferSignIn(Vec<u8>),
    /// Accept or dismiss a paired host's offer of its account.
    AnswerOffer { host_id: Vec<u8>, accept: bool },
    /// Open the account service's offer page, signed in.
    OfferPage,
    /// Open a link in the browser.
    OpenUrl(String),
    /// Ask for the folders worth offering as projects.
    Suggest,
    /// Onboarding is over: home, with this agent selected when one is named.
    Home(Option<AgentKey>),
}

/// The pairing window as this screen holds it.
#[derive(Debug, Default)]
struct Pairing {
    window: Option<wire::PairingWindow>,
    /// When the window was opened, in Unix milliseconds.
    opened_ms: i64,
    /// The codes the window's link makes: the whole link, and the page
    /// alone, which still sends a phone to the App Store when the whole
    /// code does not fit.
    codes: Option<(Code, Option<Code>)>,
    status: Option<wire::PairingStatus>,
    /// A fresh window was asked for: the old one closing is not an end.
    renewing: bool,
    /// A window was seen open since the step showed. The watch's first
    /// status can be the last window's outcome, a pairing made long ago,
    /// which is not this one.
    seen_open: bool,
    /// Why no window opened.
    failed: Option<String>,
}

#[derive(Debug, Default)]
struct SignIn {
    /// The phone just paired here, still signed out, to offer the account
    /// too; ticked until the person unticks it.
    also: Option<(Vec<u8>, String)>,
    also_ticked: bool,
    code: Option<wire::SignInCode>,
    /// The browser could not be opened: the link is shown instead.
    unopened: bool,
    /// The browser sign-in is running.
    waiting: bool,
    /// Offers paired hosts made this profile of their account.
    offers: Vec<wire::SignInOffer>,
    /// The offer a "shown" was reported for.
    offer_counted: Option<Vec<u8>>,
    /// An offer is being accepted.
    accepting: bool,
    /// An offer was accepted: the offer page opens before the screen moves
    /// on.
    accepted: bool,
}

#[derive(Debug, Default)]
struct Projects {
    /// Each folder: its path, as shown, and whether it is ticked.
    rows: Vec<(String, String, bool)>,
    adding: Option<Editor>,
    loaded: bool,
}

/// The onboarding screen.
#[derive(Debug)]
pub struct Onboarding {
    pub step: SetupStep,
    /// Opened from "Finish setting up": skipped steps are walked too.
    finishing: bool,
    steps: wire::Onboarding,
    settings: wire::Settings,
    notice: Notice,
    /// The step the analytics notice is drawn on: the first one shown,
    /// when no client had shown it yet.
    notice_on: Option<SetupStep>,
    /// What this machine is, in "Keep this Mac awake": "Mac" or "computer".
    machine: String,
    local_host: Vec<u8>,
    rename: Option<Editor>,
    pairing: Pairing,
    /// The phone this run paired, for the screens after.
    paired: Option<wire::Peer>,
    start_at_login: bool,
    keep_awake: bool,
    sign_in: SignIn,
    projects: Projects,
    telemetry: bool,
    auto_reports: bool,
    /// The agents on this machine when the last step opened: one that is
    /// not among them came from the phone.
    agents_before: HashSet<Vec<u8>>,
    /// The focused row on a screen of choices.
    focus: usize,
    /// Why the last confirmation did not go through.
    error: Option<String>,
    /// A confirmation is on its way.
    busy: bool,
    /// Click targets on the last frame: (row, columns, what it does).
    spots: Vec<(u16, (u16, u16), Hit)>,
}

#[derive(Clone, Debug, PartialEq)]
enum Hit {
    Key(KeyEvent),
    Row(usize),
}

fn plain(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// What a step is called on its screen.
pub fn title(step: SetupStep) -> &'static str {
    match step {
        SetupStep::PairPhone => "Connect your iPhone",
        SetupStep::KeepReachable => "Keep it connected",
        SetupStep::SignIn => "Reach it from anywhere",
        SetupStep::Projects => "Projects",
        SetupStep::Improve => "Help improve amux",
        SetupStep::FirstAgent => "Try starting an agent from your phone",
    }
}

/// A PIN as people read it aloud: two groups of three.
fn spaced(pin: &str) -> String {
    if pin.len() == 6 && pin.is_ascii() {
        format!("{} {}", &pin[..3], &pin[3..])
    } else {
        pin.to_owned()
    }
}

/// The page a pairing link opens, without the invitation after `#`.
fn page_of(link: &str) -> Option<&str> {
    link.split_once('#').map(|(page, _)| page)
}

impl Onboarding {
    /// Onboarding opened at `step`, with what to do as it shows.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        step: SetupStep,
        finishing: bool,
        steps: wire::Onboarding,
        settings: wire::Settings,
        notice: Notice,
        machine: &str,
        local_host: &[u8],
        fleet: &FleetState,
    ) -> (Onboarding, Vec<SetupEffect>) {
        let mut onboarding = Onboarding {
            step,
            finishing,
            notice_on: (!steps.notice_shown).then_some(step),
            steps,
            telemetry: settings.telemetry,
            auto_reports: settings.auto_reports,
            settings,
            notice,
            machine: machine.to_owned(),
            local_host: local_host.to_vec(),
            rename: None,
            pairing: Pairing::default(),
            paired: None,
            start_at_login: true,
            keep_awake: true,
            sign_in: SignIn::default(),
            projects: Projects::default(),
            agents_before: HashSet::new(),
            focus: 0,
            error: None,
            busy: false,
            spots: Vec::new(),
        };
        let mut effects = Vec::new();
        if onboarding.notice_on.is_some() {
            effects.push(SetupEffect::Answer(Answer::NoticeShown(wire::Empty {})));
        }
        effects.extend(onboarding.enter(step, fleet));
        (onboarding, effects)
    }

    /// What this run shows: the step on screen, then what follows.
    fn enter(&mut self, step: SetupStep, fleet: &FleetState) -> Vec<SetupEffect> {
        self.step = step;
        self.focus = 0;
        self.error = None;
        self.busy = false;
        self.rename = None;
        let mut effects = vec![SetupEffect::Answer(Answer::Shown(step.id() as i32))];
        match step {
            SetupStep::PairPhone => effects.push(SetupEffect::OpenPairing),
            SetupStep::KeepReachable => {
                // Both on until the person has answered; after that, what
                // they chose.
                let confirmed = !ui_view::step_open(&self.steps, step);
                self.start_at_login = !confirmed || self.settings.start_at_login == Some(true);
                self.keep_awake = !confirmed || self.settings.keep_awake;
            }
            SetupStep::SignIn => {
                self.sign_in.also = self.paired.as_ref().and_then(|peer| {
                    let signed_in = fleet
                        .host(&peer.host_id)
                        .and_then(wire::HostEntry::account_signed_in);
                    (signed_in != Some(true)).then(|| (peer.host_id.clone(), peer.name.clone()))
                });
                self.sign_in.also_ticked = self.sign_in.also.is_some();
                effects.extend(self.count_offer());
            }
            SetupStep::Projects => {
                if !self.projects.loaded {
                    effects.push(SetupEffect::Suggest);
                }
            }
            SetupStep::Improve => {
                self.telemetry = self.settings.telemetry;
                self.auto_reports = self.settings.auto_reports;
            }
            SetupStep::FirstAgent => {
                self.agents_before = fleet
                    .agents()
                    .filter(|agent| agent.host_id == self.local_host)
                    .map(|agent| agent.agent_id.clone())
                    .collect();
            }
        }
        effects
    }

    /// What leaving the step on screen undoes.
    fn leave(&mut self) -> Vec<SetupEffect> {
        let mut effects = Vec::new();
        match self.step {
            // Closed even when the window already ended with a pairing:
            // the watch on it stops too.
            SetupStep::PairPhone => {
                self.pairing = Pairing::default();
                effects.push(SetupEffect::ClosePairing);
            }
            SetupStep::SignIn if self.sign_in.waiting => {
                self.sign_in.waiting = false;
                self.sign_in.code = None;
                effects.push(SetupEffect::CancelSignIn);
            }
            _ => {}
        }
        effects
    }

    /// On to the step after the one on screen, or home after the last.
    fn advance(&mut self, fleet: &FleetState) -> Vec<SetupEffect> {
        let mut effects = self.leave();
        // Pairing ends on the reachability questions, whatever the daemon
        // says of them: "Paired ✓" is where they make sense.
        let next = if self.step == SetupStep::PairPhone && self.paired.is_some() {
            Some(SetupStep::KeepReachable)
        } else {
            ui_view::next_step(&self.steps, Some(self.step), self.finishing)
        };
        match next {
            Some(step) => effects.extend(self.enter(step, fleet)),
            None => effects.push(SetupEffect::Home(None)),
        }
        effects
    }

    /// "Later": the step is skipped and the next one shows.
    fn later(&mut self, fleet: &FleetState) -> Vec<SetupEffect> {
        let mut effects = Vec::new();
        if ui_view::step_open(&self.steps, self.step) {
            effects.push(SetupEffect::Answer(Answer::Skip(self.step.id() as i32)));
        }
        effects.extend(self.advance(fleet));
        effects
    }

    /// Whether a pairing window is open for this screen, which opening
    /// another replaces.
    pub fn holds_window(&self) -> bool {
        self.pairing.window.is_some()
    }

    pub fn paste(&mut self, pasted: &str) {
        if let Some(editor) = self.rename.as_mut().or(self.projects.adding.as_mut()) {
            editor.insert_str(pasted.trim());
        }
    }

    /// Closes what the screen holds open, as the client quits.
    pub fn close(&mut self) -> Vec<SetupEffect> {
        self.leave()
    }

    /// Whether a field with text has the keys, for Ctrl+C to clear.
    pub fn field_text(&self) -> bool {
        self.rename
            .as_ref()
            .or(self.projects.adding.as_ref())
            .is_some_and(|editor| !editor.is_empty())
    }

    pub fn kill_field(&mut self) {
        if let Some(editor) = self.rename.as_mut().or(self.projects.adding.as_mut()) {
            editor.kill_all();
        }
    }

    // --- what comes back -------------------------------------------------

    /// The daemon's steps changed. A step it saw happen elsewhere moves
    /// the screen on.
    pub fn steps_changed(
        &mut self,
        steps: wire::Onboarding,
        fleet: &FleetState,
    ) -> Vec<SetupEffect> {
        self.steps = steps;
        let done = !ui_view::step_open(&self.steps, self.step);
        match self.step {
            SetupStep::SignIn if done && !self.sign_in.waiting && !self.sign_in.accepting => {
                self.advance(fleet)
            }
            SetupStep::FirstAgent if done => self.fleet_changed(fleet),
            _ => Vec::new(),
        }
    }

    pub fn settings_changed(&mut self, settings: wire::Settings) {
        if self.rename.is_none() {
            self.settings.host_name = settings.host_name.clone();
        }
        self.settings = settings;
    }

    /// An agent new on this machine while the last step shows is the one
    /// the phone started: home, with it selected.
    pub fn fleet_changed(&mut self, fleet: &FleetState) -> Vec<SetupEffect> {
        if self.step != SetupStep::FirstAgent {
            return Vec::new();
        }
        let newest = fleet
            .agents()
            .filter(|agent| {
                agent.host_id == self.local_host && !self.agents_before.contains(&agent.agent_id)
            })
            .max_by_key(|agent| agent.created_at_ms);
        match newest {
            Some(agent) => vec![SetupEffect::Home(Some(ui_state::agent_key(agent)))],
            None => Vec::new(),
        }
    }

    /// A window opened for the pairing screen; one that answers after the
    /// screen was left is closed by the close queued after it, and is not
    /// this screen's to show or renew.
    pub fn pairing_opened(&mut self, window: wire::PairingWindow, now_ms: i64) {
        if self.step != SetupStep::PairPhone {
            return;
        }
        let codes = Code::encode(&window.qr_link).map(|whole| {
            let page = page_of(&window.qr_link).and_then(Code::encode);
            (whole, page)
        });
        self.pairing = Pairing {
            window: Some(window),
            opened_ms: now_ms,
            codes,
            status: None,
            renewing: false,
            failed: None,
            seen_open: self.pairing.seen_open,
        };
    }

    pub fn pairing_failed(&mut self, why: String) {
        self.pairing.failed = Some(why);
        self.pairing.renewing = false;
    }

    /// The pairing window's status moved: a phone proving the code, then
    /// paired, which ends the step on "Paired ✓".
    pub fn pairing_status(
        &mut self,
        status: wire::PairingStatus,
        fleet: &FleetState,
    ) -> Vec<SetupEffect> {
        if self.step != SetupStep::PairPhone {
            return Vec::new();
        }
        if status.active {
            self.pairing.seen_open = true;
        } else if !self.pairing.seen_open {
            return Vec::new();
        }
        if !status.active
            && let Some(peer) = status.paired.clone()
        {
            self.paired = Some(peer);
            // The window closed with the pairing.
            self.pairing = Pairing::default();
            return self.advance(fleet);
        }
        if self.pairing.renewing && !status.active {
            return Vec::new();
        }
        self.pairing.status = Some(status);
        Vec::new()
    }

    /// When the window wants renewing, in Unix milliseconds.
    pub fn renew_at(&self) -> Option<i64> {
        let window = self.pairing.window.as_ref()?;
        if self.step != SetupStep::PairPhone || self.joining() || self.pairing.renewing {
            return None;
        }
        let ttl = i64::try_from(window.ttl_seconds.saturating_mul(1000)).unwrap_or(i64::MAX);
        Some(self.pairing.opened_ms + (ttl - RENEW_BEFORE_MS).max(ttl / 2))
    }

    /// Renews the window when it is due.
    pub fn tick(&mut self, now_ms: i64) -> Vec<SetupEffect> {
        match self.renew_at() {
            Some(at) if now_ms >= at => {
                self.pairing.renewing = true;
                vec![SetupEffect::OpenPairing]
            }
            _ => Vec::new(),
        }
    }

    fn joining(&self) -> bool {
        self.pairing
            .status
            .as_ref()
            .is_some_and(|status| status.joining)
    }

    fn locked_out(&self) -> bool {
        self.pairing
            .status
            .as_ref()
            .is_some_and(|status| status.locked_out && !status.active)
    }

    pub fn sign_in_code(&mut self, code: wire::SignInCode, opened: bool) {
        self.sign_in.code = Some(code);
        self.sign_in.unopened = !opened;
    }

    /// Signed in: the phone ticked is offered the account, and the screen
    /// moves on. The browser sign-in lands on the offer page by itself.
    pub fn signed_in(&mut self, fleet: &FleetState) -> Vec<SetupEffect> {
        self.sign_in.waiting = false;
        self.sign_in.code = None;
        let mut effects = Vec::new();
        if self.sign_in.also_ticked
            && let Some((host, _)) = &self.sign_in.also
        {
            effects.push(SetupEffect::OfferSignIn(host.clone()));
        }
        if self.step == SetupStep::SignIn {
            effects.extend(self.advance(fleet));
        }
        effects
    }

    pub fn sign_in_failed(&mut self, why: String) {
        self.sign_in.waiting = false;
        self.sign_in.accepting = false;
        self.sign_in.code = None;
        self.error = Some(why);
    }

    /// The offers paired hosts made this profile changed.
    pub fn offers_changed(&mut self, offers: Vec<wire::SignInOffer>) -> Vec<SetupEffect> {
        self.sign_in.offers = offers;
        if self.step == SetupStep::SignIn {
            self.count_offer()
        } else {
            Vec::new()
        }
    }

    /// The offer on screen, counted once as shown; or, with none, the
    /// choice to offer the paired phone, counted the same way.
    fn count_offer(&mut self) -> Vec<SetupEffect> {
        let (key, role) = match (self.sign_in.offers.first(), &self.sign_in.also) {
            (Some(offer), _) => (offer.host_id.clone(), wire::OfferRole::Receiver),
            (None, Some((host, _))) => (host.clone(), wire::OfferRole::Offerer),
            (None, None) => return Vec::new(),
        };
        if self.sign_in.offer_counted.as_ref() == Some(&key) {
            return Vec::new();
        }
        self.sign_in.offer_counted = Some(key);
        vec![SetupEffect::Answer(Answer::SignInOfferShown(role as i32))]
    }

    /// An offer was answered: accepted, this profile is signed in and the
    /// offer page opens; dismissed, the browser sign-in is offered.
    pub fn offer_answered(&mut self, accepted: bool, fleet: &FleetState) -> Vec<SetupEffect> {
        self.sign_in.accepting = false;
        if !accepted {
            return Vec::new();
        }
        self.sign_in.accepted = true;
        let mut effects = vec![SetupEffect::OfferPage];
        if self.step == SetupStep::SignIn {
            effects.extend(self.advance(fleet));
        }
        effects
    }

    pub fn suggestions(&mut self, suggested: Vec<wire::ProjectEntry>) {
        self.projects.loaded = true;
        let mut rows: Vec<(String, String, bool)> = suggested
            .into_iter()
            .map(|entry| {
                let shown = text::tilde(&entry.path);
                (entry.path, shown, true)
            })
            .collect();
        // Projects already chosen stay; they are not asked about again.
        rows.retain(|(path, ..)| !self.settings.projects.contains(path));
        self.projects.rows = rows;
    }

    /// The step's settings were taken and it is confirmed.
    pub fn confirmed(&mut self, step: SetupStep, fleet: &FleetState) -> Vec<SetupEffect> {
        self.busy = false;
        if step != self.step {
            return Vec::new();
        }
        self.advance(fleet)
    }

    pub fn confirm_failed(&mut self, why: String) {
        self.busy = false;
        self.error = Some(why);
    }

    // --- keys ------------------------------------------------------------

    /// The choices on the screen, for the focus to move over.
    fn rows(&self) -> usize {
        match self.step {
            SetupStep::KeepReachable => 1 + usize::from(self.settings.start_at_login.is_some()),
            SetupStep::SignIn if self.sign_in.offers.is_empty() => {
                usize::from(self.sign_in.also.is_some())
            }
            SetupStep::Projects => self.projects.rows.len() + 1,
            SetupStep::Improve => 2,
            _ => 0,
        }
    }

    pub fn key(&mut self, key: KeyEvent, fleet: &FleetState) -> Vec<SetupEffect> {
        if let Some(editor) = &mut self.rename {
            match key.code {
                KeyCode::Esc => self.rename = None,
                KeyCode::Enter => {
                    let name = editor.text().trim().to_owned();
                    self.rename = None;
                    self.settings.host_name = name.clone();
                    return vec![SetupEffect::Rename(name)];
                }
                _ => {
                    editor.key(key);
                }
            }
            return Vec::new();
        }
        if let Some(editor) = &mut self.projects.adding {
            match key.code {
                KeyCode::Esc => self.projects.adding = None,
                KeyCode::Enter => {
                    let path = editor.text().trim().to_owned();
                    self.projects.adding = None;
                    if !path.is_empty() && !self.projects.rows.iter().any(|(at, ..)| *at == path) {
                        let shown = text::tilde(&path);
                        self.projects.rows.push((path, shown, true));
                        self.focus = self.projects.rows.len() - 1;
                    }
                }
                _ => {
                    editor.key(key);
                }
            }
            return Vec::new();
        }
        if self.busy {
            return Vec::new();
        }
        let rows = self.rows();
        match key.code {
            KeyCode::Esc => return self.later(fleet),
            KeyCode::Up | KeyCode::Char('k') if rows > 0 => {
                self.focus = self.focus.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if rows > 0 => {
                self.focus = (self.focus + 1).min(rows - 1);
            }
            KeyCode::Char(' ') => self.toggle(self.focus),
            KeyCode::Enter => return self.primary(fleet),
            KeyCode::Char('r') if self.step == SetupStep::PairPhone => {
                let mut editor = Editor::default();
                editor.set(&self.settings.host_name, Vec::new());
                self.rename = Some(editor);
            }
            KeyCode::Char('n') if self.step == SetupStep::PairPhone && self.locked_out() => {
                self.pairing.renewing = true;
                return vec![SetupEffect::OpenPairing];
            }
            KeyCode::Char('n') if self.step == SetupStep::SignIn => {
                if let Some(offer) = self.sign_in.offers.first() {
                    let host_id = offer.host_id.clone();
                    self.sign_in.offers.remove(0);
                    return vec![SetupEffect::AnswerOffer {
                        host_id,
                        accept: false,
                    }];
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn toggle(&mut self, row: usize) {
        match self.step {
            SetupStep::KeepReachable => {
                if self.settings.start_at_login.is_some() && row == 0 {
                    self.start_at_login = !self.start_at_login;
                } else {
                    self.keep_awake = !self.keep_awake;
                }
            }
            SetupStep::SignIn => self.sign_in.also_ticked = !self.sign_in.also_ticked,
            SetupStep::Projects => match self.projects.rows.get_mut(row) {
                Some((.., ticked)) => *ticked = !*ticked,
                None => self.projects.adding = Some(Editor::default()),
            },
            SetupStep::Improve => match row {
                0 => self.telemetry = !self.telemetry,
                _ => self.auto_reports = !self.auto_reports,
            },
            _ => {}
        }
    }

    /// Enter: the step's own action.
    fn primary(&mut self, fleet: &FleetState) -> Vec<SetupEffect> {
        match self.step {
            SetupStep::PairPhone => Vec::new(),
            SetupStep::KeepReachable => {
                let settings = wire::UpdateSettingsRequest {
                    start_at_login: self
                        .settings
                        .start_at_login
                        .is_some()
                        .then_some(self.start_at_login),
                    keep_awake: Some(self.keep_awake),
                    ..Default::default()
                };
                self.confirm(settings)
            }
            SetupStep::SignIn => {
                if self.sign_in.waiting || self.sign_in.accepting {
                    return Vec::new();
                }
                self.error = None;
                if let Some(offer) = self.sign_in.offers.first() {
                    self.sign_in.accepting = true;
                    return vec![SetupEffect::AnswerOffer {
                        host_id: offer.host_id.clone(),
                        accept: true,
                    }];
                }
                self.sign_in.waiting = true;
                vec![SetupEffect::SignIn]
            }
            SetupStep::Projects => {
                if self.focus == self.projects.rows.len() {
                    self.projects.adding = Some(Editor::default());
                    return Vec::new();
                }
                let mut paths = self.settings.projects.clone();
                for (path, _, ticked) in &self.projects.rows {
                    if *ticked && !paths.contains(path) {
                        paths.push(path.clone());
                    }
                }
                self.confirm(wire::UpdateSettingsRequest {
                    projects: Some(wire::ProjectList { paths }),
                    ..Default::default()
                })
            }
            SetupStep::Improve => self.confirm(wire::UpdateSettingsRequest {
                telemetry: Some(self.telemetry),
                auto_reports: Some(self.auto_reports),
                ..Default::default()
            }),
            SetupStep::FirstAgent => self.advance(fleet),
        }
    }

    /// Applies the screen's choices; the step is confirmed with them while
    /// it is still to do.
    fn confirm(&mut self, settings: wire::UpdateSettingsRequest) -> Vec<SetupEffect> {
        self.busy = true;
        self.error = None;
        vec![SetupEffect::Confirm {
            settings: Box::new(settings),
            step: Some(self.step),
        }]
    }

    pub fn mouse(&mut self, mouse: MouseEvent, fleet: &FleetState) -> Vec<SetupEffect> {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return Vec::new();
        }
        let hit = self
            .spots
            .iter()
            .find(|(row, (from, to), _)| *row == mouse.row && (*from..*to).contains(&mouse.column))
            .map(|(.., hit)| hit.clone());
        match hit {
            Some(Hit::Key(key)) => self.key(key, fleet),
            Some(Hit::Row(row)) => {
                self.focus = row;
                self.toggle(row);
                Vec::new()
            }
            None => Vec::new(),
        }
    }

    // --- drawing ---------------------------------------------------------

    /// The keys the screen's bottom line offers, most needed first.
    fn hints(&self) -> Vec<(&'static str, &'static str, KeyEvent)> {
        let key = plain;
        if self.rename.is_some() || self.projects.adding.is_some() {
            let what = if self.rename.is_some() { "save" } else { "add" };
            return vec![
                ("enter", what, key(KeyCode::Enter)),
                ("esc", "cancel", key(KeyCode::Esc)),
            ];
        }
        let later = ("esc", "later", key(KeyCode::Esc));
        let change = ("space", "change", key(KeyCode::Char(' ')));
        let next = ("enter", "continue", key(KeyCode::Enter));
        match self.step {
            SetupStep::PairPhone if self.locked_out() => vec![
                ("n", "new code", key(KeyCode::Char('n'))),
                ("r", "rename", key(KeyCode::Char('r'))),
                later,
            ],
            SetupStep::PairPhone => vec![("r", "rename", key(KeyCode::Char('r'))), later],
            SetupStep::KeepReachable | SetupStep::Improve => vec![next, change, later],
            SetupStep::SignIn if !self.sign_in.offers.is_empty() => vec![
                ("enter", "sign in", key(KeyCode::Enter)),
                ("n", "not now", key(KeyCode::Char('n'))),
                later,
            ],
            SetupStep::SignIn if self.sign_in.waiting || self.sign_in.accepting => vec![later],
            SetupStep::SignIn if self.sign_in.also.is_some() => {
                vec![("enter", "sign in", key(KeyCode::Enter)), change, later]
            }
            SetupStep::SignIn => vec![("enter", "sign in", key(KeyCode::Enter)), later],
            SetupStep::Projects if self.focus == self.projects.rows.len() => vec![
                ("enter", "add a folder", key(KeyCode::Enter)),
                ("↑↓", "move", key(KeyCode::Up)),
                later,
            ],
            SetupStep::Projects => vec![next, ("space", "tick", key(KeyCode::Char(' '))), later],
            SetupStep::FirstAgent => vec![later],
        }
    }

    pub fn draw(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        footer: Option<Line<'static>>,
        theme: Theme,
    ) {
        let width = usize::from(area.width);
        let height = usize::from(area.height);
        self.spots.clear();
        if height < 4 || width < 2 * MARGIN + 8 {
            return;
        }
        let room = width - 2 * MARGIN;
        let mut screen = Screen::new(width, theme);
        // A blank, the title, a blank.
        screen.blank();
        let mut top = Line::from(Span::raw(" ".repeat(MARGIN)));
        push(&mut top, title(self.step), theme.emphasis(), width);
        let right = "setting up amux";
        if text::line_width(&top) + 4 + right.len() + MARGIN <= width {
            text::push_right(&mut top, right, theme.faint(), width - MARGIN);
        }
        screen.line(top);
        screen.blank();

        // The notice and the keys below the body: a blank between.
        let mut tail: Vec<Line<'static>> = Vec::new();
        if self.notice_on == Some(self.step) {
            let words = format!("{} {}", self.notice.words, self.notice.url);
            for part in text::wrap(&words, room) {
                let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
                push(&mut line, part, theme.faint(), width);
                tail.push(line);
            }
        }
        // The body keeps a blank above the notice, and one above the keys.
        let below = tail.len() + 2 + usize::from(!tail.is_empty());
        let body_rows = height.saturating_sub(screen.lines.len() + below);
        let cursor = match self.step {
            SetupStep::PairPhone => self.pair_body(&mut screen, body_rows),
            SetupStep::KeepReachable => {
                self.keep_body(&mut screen);
                None
            }
            SetupStep::SignIn => {
                self.sign_in_body(&mut screen);
                None
            }
            SetupStep::Projects => self.projects_body(&mut screen),
            SetupStep::Improve => {
                self.improve_body(&mut screen);
                None
            }
            SetupStep::FirstAgent => {
                screen.words("On your phone, tap + and pick a project.", theme.text());
                screen.blank();
                screen.words("Waiting for an agent from your phone…", theme.faint());
                None
            }
        };
        if let Some(error) = &self.error {
            screen.blank();
            screen.words(error, theme.warning());
        }
        let keep = height.saturating_sub(below - usize::from(!tail.is_empty()));
        screen.lines.truncate(keep);
        screen.spots.retain(|(row, ..)| usize::from(*row) < keep);
        screen.lines.resize(keep, Line::default());
        screen.lines.extend(tail);
        screen.blank();
        // The key line, or the app's notice in its place.
        let mut keys = Line::from(Span::raw(" ".repeat(MARGIN)));
        match footer {
            Some(footer) => {
                let spans = footer
                    .spans
                    .into_iter()
                    .skip_while(|span| span.content.trim().is_empty());
                keys.spans.extend(spans);
            }
            None => {
                let hints = self.hints();
                let pairs: Vec<(&str, &str)> =
                    hints.iter().map(|(key, word, _)| (*key, *word)).collect();
                let row = screen.lines.len() as u16;
                for (at, (from, to)) in text::push_hints(&mut keys, &pairs, room, theme) {
                    screen
                        .spots
                        .push((row, (from as u16, to as u16), Hit::Key(hints[at].2)));
                }
            }
        }
        screen.line(keys);
        screen.lines.truncate(height);
        self.spots = screen
            .spots
            .into_iter()
            .map(|(row, (from, to), hit)| (area.y + row, (area.x + from, area.x + to), hit))
            .collect();
        paint.render_widget(Paragraph::new(screen.lines), area);
        if let Some((x, y)) = cursor {
            paint.set_cursor_position(Position::new(
                area.x + x.min(width.saturating_sub(1)) as u16,
                area.y + y as u16,
            ));
        }
    }

    /// A field with its text and cursor: (line, cursor column).
    fn field(editor: &Editor, label: &str, width: usize, theme: Theme) -> (Line<'static>, usize) {
        let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
        push(&mut line, label, theme.muted(), width);
        push(&mut line, "▎ ", theme.attention(), width);
        let at = text::line_width(&line);
        push(&mut line, editor.text(), theme.text(), width);
        let before: String = editor.text().chars().take(editor.cursor_chars()).collect();
        (line, at + text::str_width(&before))
    }

    fn pair_body(&self, screen: &mut Screen, rows: usize) -> Option<(usize, usize)> {
        let theme = screen.theme;
        let width = screen.width;
        let mut cursor = None;
        match &self.rename {
            Some(editor) => {
                let (line, at) = Self::field(editor, "This machine is called ", width, theme);
                cursor = Some((at, screen.lines.len()));
                screen.line(line);
            }
            None => {
                let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
                push(&mut line, "This machine is called ", theme.text(), width);
                push(&mut line, &self.settings.host_name, theme.bright(), width);
                push(&mut line, ".", theme.text(), width);
                screen.line(line);
            }
        }
        screen.blank();
        let rows = rows.saturating_sub(2);

        // The two steps, then what the window is doing.
        let mut steps: Vec<Line<'static>> = Vec::new();
        let words = |text: &str, style| Line::from(Span::styled(text.to_owned(), style));
        steps.push(words("1. Scan to get amux on your iPhone", theme.text()));
        steps.push(Line::default());
        let pin = self
            .pairing
            .window
            .as_ref()
            .and_then(|window| match &window.secret {
                Some(wire::pairing_window::Secret::PinAndQr(both)) => Some(both.pin.clone()),
                Some(wire::pairing_window::Secret::Pin(pin)) => Some(pin.clone()),
                _ => None,
            });
        match &pin {
            Some(pin) => steps.push(Line::from(vec![
                Span::styled("2. Enter ", theme.text()),
                Span::styled(spaced(pin), theme.emphasis()),
                Span::styled(" in the app", theme.text()),
            ])),
            None => steps.push(words("2. Enter the code in the app", theme.text())),
        }
        steps.push(Line::default());
        let (status, style) = if let Some(why) = &self.pairing.failed {
            (format!("Pairing could not open: {why}"), theme.warning())
        } else if self.locked_out() {
            (
                "Closed after too many wrong codes.".to_owned(),
                theme.warning(),
            )
        } else if self.joining() {
            ("Your iPhone is pairing…".to_owned(), theme.attention())
        } else if self.pairing.window.is_some() {
            ("Waiting for your iPhone…".to_owned(), theme.faint())
        } else {
            ("Opening pairing…".to_owned(), theme.faint())
        };
        steps.push(words(&status, style));
        steps.push(Line::default());
        steps.push(words(
            "Whoever enters this code can run agents on this machine as you.",
            theme.faint(),
        ));

        let dark_ground = crate::theme::luminance(theme.tokens.background.rgb) < 0.18;
        let ink = theme.text();
        let room = width - 2 * MARGIN;
        let codes = self.pairing.codes.as_ref();
        let candidates = codes
            .into_iter()
            .flat_map(|(whole, page)| std::iter::once(whole).chain(page.as_ref()));
        for code in candidates {
            let (columns, height) = code.cells();
            // Beside the steps, the text wrapped to what is left.
            if room >= TEXT_LEAST + GAP + columns && rows >= height {
                let text_room = room - GAP - columns;
                let left = rewrap(&steps, text_room);
                // The code sits by the steps it belongs to, not across the
                // screen from them.
                let widest = left.iter().map(text::line_width).max().unwrap_or(0);
                let at_column = MARGIN + widest.min(text_room) + GAP;
                let qr = code.lines(dark_ground, ink);
                for at in 0..height.max(left.len()) {
                    let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
                    if let Some(words) = left.get(at) {
                        line.spans.extend(words.spans.iter().cloned());
                    }
                    text::pad_to(&mut line, at_column);
                    if let Some(code) = qr.get(at) {
                        line.spans.extend(code.spans.iter().cloned());
                    }
                    screen.line(line);
                }
                return cursor;
            }
            // Below them, when the window is tall rather than wide.
            let left = rewrap(&steps, room);
            if room >= columns && rows > height + left.len() {
                let qr = code.lines(dark_ground, ink);
                screen.indented(left);
                screen.blank();
                screen.indented(qr);
                return cursor;
            }
        }
        // Too small for any code: the steps, and how to make room.
        screen.indented(rewrap(&steps, room));
        if let Some((_, Some(page))) = codes {
            let (columns, height) = page.cells();
            screen.blank();
            screen.words(
                &format!(
                    "Make this window at least {} columns wide and {} rows tall to show the code.",
                    columns + TEXT_LEAST + GAP + 2 * MARGIN,
                    height + 9
                ),
                theme.muted(),
            );
        }
        cursor
    }

    fn check_row(&self, screen: &mut Screen, row: usize, on: bool, words: &str) {
        let theme = screen.theme;
        let focused = self.focus == row;
        let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
        let lead = if focused { "› " } else { "  " };
        push(&mut line, lead, theme.attention(), screen.width);
        push(
            &mut line,
            if on { "[x] " } else { "[ ] " },
            if focused {
                theme.bright()
            } else {
                theme.muted()
            },
            screen.width,
        );
        push(
            &mut line,
            words,
            if focused {
                theme.bright()
            } else {
                theme.text()
            },
            screen.width,
        );
        screen.row(line, row);
    }

    fn keep_body(&self, screen: &mut Screen) {
        let theme = screen.theme;
        if let Some(peer) = &self.paired {
            let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
            push(
                &mut line,
                format!("Paired with {} ", peer.name),
                theme.text(),
                screen.width,
            );
            push(&mut line, "✓", theme.ok(), screen.width);
            screen.line(line);
            screen.blank();
        }
        screen.words(
            &format!(
                "So your phone reaches {} whenever you need it:",
                self.settings.host_name
            ),
            theme.text(),
        );
        screen.blank();
        let mut row = 0;
        if self.settings.start_at_login.is_some() {
            self.check_row(
                screen,
                row,
                self.start_at_login,
                "Start amux when you log in",
            );
            row += 1;
        }
        self.check_row(
            screen,
            row,
            self.keep_awake,
            &format!("Keep this {} awake while it's plugged in", self.machine),
        );
        screen.blank();
        screen.words(
            "On battery it stays awake only while an agent works.",
            theme.faint(),
        );
    }

    fn sign_in_body(&self, screen: &mut Screen) {
        let theme = screen.theme;
        if let Some(offer) = self.sign_in.offers.first() {
            let who = if offer.email.is_empty() {
                &offer.account_name
            } else {
                &offer.email
            };
            screen.words(
                &format!("Sign in as {who} (from {})?", offer.host_name),
                theme.bright(),
            );
            screen.blank();
            screen.words(
                &format!(
                    "{} is signed in to this account. Signing in here too lets your phone reach {} from anywhere.",
                    offer.host_name, self.settings.host_name
                ),
                theme.muted(),
            );
            if self.sign_in.accepting {
                screen.blank();
                screen.words("Signing in…", theme.faint());
            }
            return;
        }
        screen.words(
            &format!(
                "Sign in to reach {} from your phone when you are away from this network.",
                self.settings.host_name
            ),
            theme.text(),
        );
        screen.blank();
        if let Some((_, name)) = &self.sign_in.also {
            self.check_row(
                screen,
                0,
                self.sign_in.also_ticked,
                &format!("Also sign in {name}"),
            );
            screen.blank();
        }
        if self.sign_in.waiting {
            match &self.sign_in.code {
                Some(code) => {
                    let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
                    push(
                        &mut line,
                        "Approve amux in your browser. It should show ",
                        theme.text(),
                        screen.width,
                    );
                    push(&mut line, &code.user_code, theme.emphasis(), screen.width);
                    push(&mut line, ".", theme.text(), screen.width);
                    screen.line(line);
                    if self.sign_in.unopened {
                        screen.blank();
                        screen.words("The browser did not open; open this link:", theme.muted());
                        screen.words(&code.verification_uri_complete, theme.code());
                    }
                    screen.blank();
                    screen.words("Waiting for you to sign in…", theme.faint());
                }
                None => screen.words("Starting sign-in…", theme.faint()),
            }
        }
    }

    fn projects_body(&self, screen: &mut Screen) -> Option<(usize, usize)> {
        let theme = screen.theme;
        let mut cursor = None;
        screen.words(
            "New agents start in a project. These are where Claude Code and Codex ran lately:",
            theme.text(),
        );
        screen.blank();
        if !self.projects.loaded {
            screen.words("Looking for recent folders…", theme.faint());
            return None;
        }
        if self.projects.rows.is_empty() {
            screen.words("No recent folders found.", theme.faint());
            screen.blank();
        }
        for (row, (_, shown, ticked)) in self.projects.rows.iter().enumerate() {
            self.check_row(screen, row, *ticked, shown);
        }
        let add = self.projects.rows.len();
        match &self.projects.adding {
            Some(editor) => {
                let (mut line, at) = Self::field(editor, "    Folder ", screen.width, theme);
                if editor.is_empty() {
                    push(
                        &mut line,
                        "a path, such as ~/src/app",
                        theme.faint(),
                        screen.width,
                    );
                }
                cursor = Some((at, screen.lines.len()));
                screen.line(line);
            }
            None => {
                let focused = self.focus == add;
                let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
                push(
                    &mut line,
                    if focused { "› " } else { "  " },
                    theme.attention(),
                    screen.width,
                );
                push(
                    &mut line,
                    "+ Add a folder…",
                    if focused {
                        theme.bright()
                    } else {
                        theme.muted()
                    },
                    screen.width,
                );
                screen.row(line, add);
            }
        }
        cursor
    }

    fn improve_body(&self, screen: &mut Screen) {
        let theme = screen.theme;
        screen.words(
            "Tell us how amux is used and when it goes wrong, so we can fix what matters.",
            theme.text(),
        );
        screen.blank();
        self.check_row(screen, 0, self.telemetry, "Send usage counts");
        self.check_row(
            screen,
            1,
            self.auto_reports,
            "Send a report automatically when something goes wrong",
        );
        screen.blank();
        screen.words("Never your prompts, code, or file names.", theme.muted());
    }
}

/// Lines `rewrap`ped to `room`: each line's words wrapped in its first
/// span's style.
fn rewrap(lines: &[Line<'static>], room: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for line in lines {
        if text::line_width(line) <= room || line.spans.is_empty() {
            out.push(line.clone());
            continue;
        }
        let words: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        let style = line.spans[0].style;
        out.extend(
            text::wrap(&words, room)
                .into_iter()
                .map(|part| Line::from(Span::styled(part, style))),
        );
    }
    out
}

/// A screen's lines as they are laid, with what each row does when
/// clicked.
struct Screen {
    lines: Vec<Line<'static>>,
    spots: Vec<(u16, (u16, u16), Hit)>,
    width: usize,
    theme: Theme,
}

impl Screen {
    fn new(width: usize, theme: Theme) -> Screen {
        Screen {
            lines: Vec::new(),
            spots: Vec::new(),
            width,
            theme,
        }
    }

    fn blank(&mut self) {
        self.lines.push(Line::default());
    }

    fn line(&mut self, line: Line<'static>) {
        self.lines.push(line);
    }

    /// A line that focuses and changes row `row` when clicked.
    fn row(&mut self, line: Line<'static>, row: usize) {
        let to = text::line_width(&line) as u16;
        self.spots
            .push((self.lines.len() as u16, (MARGIN as u16, to), Hit::Row(row)));
        self.lines.push(line);
    }

    /// Words wrapped to the margins, in one style.
    fn words(&mut self, words: &str, style: ratatui::style::Style) {
        for part in text::wrap(words, self.width - 2 * MARGIN) {
            let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
            push(&mut line, part, style, self.width);
            self.lines.push(line);
        }
    }

    fn indented(&mut self, lines: Vec<Line<'static>>) {
        for line in lines {
            let mut indented = Line::from(Span::raw(" ".repeat(MARGIN)));
            indented.spans.extend(line.spans);
            self.lines.push(indented);
        }
    }
}
