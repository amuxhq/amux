//! The app's side of onboarding and the settings screen: the
//! installation's calls (settings, onboarding, sign-in) and the profile's
//! pairing window and sign-in offers, run as tasks whose answers come back
//! as [`DeskEvent`]s. The screens decide; this only carries.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use client::Installation;
use futures_util::StreamExt as _;
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;
use ui_view::SetupStep;
use wire::answer_onboarding_request::Answer;

use super::{App, AppEvent, Tone, now_ms};
use crate::onboarding::{Onboarding, SetupEffect, WINDOW_SECONDS};
use crate::settings_screen::{SettingsEffect, SettingsScreen};

/// What the terminal client needs to run onboarding and settings: the
/// installation's front door and the profile it opened.
#[derive(Clone)]
pub struct DeskConfig {
    pub installation: Arc<dyn Installation>,
    pub profile: wire::Profile,
    /// What this machine is called in "Keep this Mac awake": "Mac" or
    /// "computer".
    pub machine: String,
    /// The account service a sign-in goes to; empty is amux.sh.
    pub cloud_url: String,
    /// Onboarding opens by itself while a step is needed: a bare `amux`,
    /// not a verb that opens the client on something else.
    pub opens: bool,
}

/// What came back from the installation or the profile's administration.
#[derive(Debug)]
pub enum DeskEvent {
    Steps(wire::Onboarding),
    Settings(wire::Settings),
    Profiles(Vec<wire::Profile>),
    Offers(Vec<wire::SignInOffer>),
    PairingOpened(Result<wire::PairingWindow, String>),
    Pairing(wire::PairingStatus),
    SignInCode {
        code: wire::SignInCode,
        opened: bool,
    },
    SignedIn,
    SignInFailed(String),
    OfferAnswered(Result<bool, String>),
    Suggested(Vec<wire::RepositoryEntry>),
    Confirmed(SetupStep),
    ConfirmFailed(String),
    /// A settings change was refused.
    Refused(String),
}

/// The onboarding and settings state the app keeps.
pub struct Desk {
    config: DeskConfig,
    steps: Option<wire::Onboarding>,
    settings: Option<wire::Settings>,
    offers: Vec<wire::SignInOffer>,
    /// Whether onboarding opening at start was decided.
    decided: bool,
    pub(super) onboarding: Option<Onboarding>,
    pub(super) page: Option<SettingsScreen>,
    sign_in: Option<AbortHandle>,
    pairing_watch: Option<AbortHandle>,
    /// Opening and closing the pairing window, one call after another.
    pairing_calls: InOrder,
}

type Call = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Runs calls one after another, each finished before the next starts, in
/// the order they were handed over. Opening the pairing window and closing
/// it go through one of these: as independent tasks a close made while the
/// open was still on its way could land first, leaving a window open that
/// nothing on screen shows or follows, and that refuses the next one until
/// it ends.
#[derive(Default)]
pub(super) struct InOrder {
    calls: Option<mpsc::UnboundedSender<Call>>,
}

impl InOrder {
    /// Queues `call` after every call handed over before it; the task that
    /// runs them starts with the first.
    pub(super) fn run(&mut self, call: impl Future<Output = ()> + Send + 'static) {
        let calls = self.calls.get_or_insert_with(|| {
            let (calls, mut queued) = mpsc::unbounded_channel::<Call>();
            tokio::spawn(async move {
                while let Some(call) = queued.recv().await {
                    call.await;
                }
            });
            calls
        });
        let _ = calls.send(Box::pin(call));
    }
}

impl Desk {
    pub fn new(config: DeskConfig) -> Desk {
        Desk {
            config,
            steps: None,
            settings: None,
            offers: Vec::new(),
            decided: false,
            onboarding: None,
            page: None,
            sign_in: None,
            pairing_watch: None,
            pairing_calls: InOrder::default(),
        }
    }

    pub(super) fn open(&self) -> bool {
        self.onboarding.is_some() || self.page.is_some()
    }
}

impl App {
    fn desk_send(&self) -> impl Fn(DeskEvent) + Send + Sync + Clone + 'static {
        let events = self.events.clone();
        move |event| {
            let _ = events.send(AppEvent::Desk(event));
        }
    }

    /// Starts reading the settings, the onboarding steps, the profile and
    /// the offers made to it: the settings first, so the first screen has
    /// them.
    pub(super) fn start_desk(&mut self) {
        let Some(desk) = &self.desk else {
            return;
        };
        let installation = desk.config.installation.clone();
        let profile_id = desk.config.profile.id.clone();
        let send = self.desk_send();
        let steps_send = send.clone();
        tokio::spawn(async move {
            if let Ok(settings) = installation.get_settings().await {
                steps_send(DeskEvent::Settings(settings));
            }
            let Ok(mut steps) = installation
                .subscribe_onboarding(wire::OnboardingRequest { profile_id })
                .await
            else {
                return;
            };
            while let Some(Ok(now)) = steps.next().await {
                steps_send(DeskEvent::Steps(now));
            }
        });
        let installation = desk.config.installation.clone();
        let profiles_send = send.clone();
        tokio::spawn(async move {
            let Ok(mut profiles) = installation.subscribe_profiles().await else {
                return;
            };
            while let Some(Ok(listed)) = profiles.next().await {
                profiles_send(DeskEvent::Profiles(listed.profiles));
            }
        });
        let client = self.client.clone();
        tokio::spawn(async move {
            let Ok(mut offers) = client.subscribe_sign_in_offers().await else {
                return;
            };
            while let Some(Ok(now)) = offers.next().await {
                send(DeskEvent::Offers(now.offers));
            }
        });
    }

    /// Onboarding or settings has the screen.
    pub(super) fn desk_open(&self) -> bool {
        self.desk.as_ref().is_some_and(Desk::open)
    }

    pub(super) fn desk_event(&mut self, event: DeskEvent) {
        let Some(desk) = &mut self.desk else {
            return;
        };
        let fleet = self.fleet.state();
        let mut effects = Vec::new();
        match event {
            DeskEvent::Steps(steps) => {
                desk.steps = Some(steps.clone());
                let view = ui_view::onboarding_view(&steps);
                if let Some(onboarding) = &mut desk.onboarding {
                    effects = onboarding.steps_changed(steps, &fleet);
                } else if !desk.decided {
                    desk.decided = true;
                    if let Some(at) = view.opens_at.filter(|_| desk.config.opens) {
                        drop(fleet);
                        self.open_onboarding(at, false);
                        return;
                    }
                }
                self.home.finish = if desk.onboarding.is_some() {
                    0
                } else {
                    view.to_finish
                };
            }
            DeskEvent::Settings(settings) => {
                self.config.chat_in = match settings.chat_in() {
                    wire::ChatIn::Native => settings::ChatInSetting::Native,
                    _ => settings::ChatInSetting::Amux,
                };
                self.home.place.chat_in = self.config.chat_in;
                if let Some(onboarding) = &mut desk.onboarding {
                    onboarding.settings_changed(settings.clone());
                }
                if let Some(page) = &mut desk.page {
                    page.settings_changed(settings.clone());
                }
                desk.settings = Some(settings);
            }
            DeskEvent::Profiles(profiles) => {
                if let Some(profile) = profiles
                    .into_iter()
                    .find(|profile| profile.id == desk.config.profile.id)
                {
                    if let Some(page) = &mut desk.page {
                        page.profile_changed(profile.clone());
                    }
                    desk.config.profile = profile;
                }
            }
            DeskEvent::Offers(offers) => {
                desk.offers = offers.clone();
                if let Some(onboarding) = &mut desk.onboarding {
                    effects = onboarding.offers_changed(offers);
                }
            }
            DeskEvent::PairingOpened(Ok(window)) => {
                if let Some(onboarding) = &mut desk.onboarding {
                    onboarding.pairing_opened(window, now_ms());
                }
            }
            DeskEvent::PairingOpened(Err(why)) => {
                if let Some(onboarding) = &mut desk.onboarding {
                    onboarding.pairing_failed(why);
                }
            }
            DeskEvent::Pairing(status) => {
                if let Some(onboarding) = &mut desk.onboarding {
                    effects = onboarding.pairing_status(status, &fleet);
                }
            }
            DeskEvent::SignInCode { code, opened } => {
                if let Some(onboarding) = &mut desk.onboarding {
                    onboarding.sign_in_code(code.clone(), opened);
                }
                if let Some(page) = &mut desk.page {
                    page.sign_in_code(code, opened);
                }
            }
            DeskEvent::SignedIn => {
                desk.sign_in = None;
                if let Some(onboarding) = &mut desk.onboarding {
                    effects = onboarding.signed_in(&fleet);
                }
                if let Some(page) = &mut desk.page {
                    page.signed_in();
                }
            }
            DeskEvent::SignInFailed(why) => {
                desk.sign_in = None;
                if let Some(onboarding) = &mut desk.onboarding {
                    onboarding.sign_in_failed(why.clone());
                }
                if let Some(page) = &mut desk.page {
                    page.failed(why);
                }
            }
            DeskEvent::OfferAnswered(answered) => {
                if let Some(onboarding) = &mut desk.onboarding {
                    effects = match answered {
                        Ok(accepted) => onboarding.offer_answered(accepted, &fleet),
                        Err(why) => {
                            onboarding.sign_in_failed(why);
                            Vec::new()
                        }
                    };
                }
            }
            DeskEvent::Suggested(suggested) => {
                if let Some(onboarding) = &mut desk.onboarding {
                    onboarding.suggestions(suggested);
                }
            }
            DeskEvent::Confirmed(step) => {
                if let Some(onboarding) = &mut desk.onboarding {
                    effects = onboarding.confirmed(step, &fleet);
                }
            }
            DeskEvent::ConfirmFailed(why) => {
                if let Some(onboarding) = &mut desk.onboarding {
                    onboarding.confirm_failed(why);
                }
            }
            DeskEvent::Refused(why) => match &mut desk.page {
                Some(page) => page.failed(why),
                None => {
                    drop(fleet);
                    self.notice(why, Tone::Warn);
                    return;
                }
            },
        }
        drop(fleet);
        self.setup_effects(effects);
    }

    /// Opens onboarding at `step`; `finishing` walks the skipped steps too.
    pub(super) fn open_onboarding(&mut self, step: SetupStep, finishing: bool) {
        if self.desk.as_ref().is_none_or(|desk| desk.steps.is_none()) {
            return;
        }
        self.close_chat_for_desk();
        let fleet = self.fleet.state();
        let Some(desk) = &mut self.desk else {
            return;
        };
        let Some(steps) = desk.steps.clone() else {
            return;
        };
        let settings = desk.settings.clone().unwrap_or_default();
        let (mut onboarding, effects) = Onboarding::open(
            step,
            finishing,
            steps,
            settings,
            &desk.config.machine,
            &self.config.local_host,
            &fleet,
        );
        let mut effects = effects;
        effects.extend(onboarding.offers_changed(desk.offers.clone()));
        desk.onboarding = Some(onboarding);
        desk.page = None;
        self.home.finish = 0;
        drop(fleet);
        self.setup_effects(effects);
    }

    fn close_chat_for_desk(&mut self) {
        // A chat cannot be open under a full screen of its own: closed, its
        // draft is kept as leaving it for home keeps it.
        if self.chat.is_some() {
            self.close_chat();
        }
    }

    /// "Finish setting up" on home: onboarding at the first step left.
    pub(super) fn finish_setup(&mut self) {
        let next = self
            .desk
            .as_ref()
            .and_then(|desk| desk.steps.as_ref())
            .and_then(|steps| ui_view::next_step(steps, None, true));
        if let Some(step) = next {
            self.open_onboarding(step, true);
        }
    }

    pub(super) fn dismiss_setup(&mut self) {
        self.home.finish = 0;
        self.setup_effects(vec![SetupEffect::Answer(Answer::Dismiss(wire::Empty {}))]);
    }

    /// The settings screen over home.
    pub(super) fn open_settings(&mut self) {
        let Some(desk) = &mut self.desk else {
            self.notice("settings are not served here", Tone::Info);
            return;
        };
        let Some(settings) = desk.settings.clone() else {
            self.notice("the settings have not arrived yet", Tone::Info);
            return;
        };
        desk.page = Some(SettingsScreen::new(
            settings,
            Some(desk.config.profile.clone()),
            &desk.config.machine,
        ));
        self.close_chat_for_desk();
    }

    /// A key while onboarding or settings has the screen.
    pub(super) fn desk_key(&mut self, key: crossterm::event::KeyEvent) {
        let fleet = self.fleet.state();
        let Some(desk) = &mut self.desk else {
            return;
        };
        if let Some(onboarding) = &mut desk.onboarding {
            let effects = onboarding.key(key, &fleet);
            drop(fleet);
            self.setup_effects(effects);
        } else if let Some(page) = &mut desk.page {
            let effects = page.key(key);
            drop(fleet);
            self.settings_effects(effects);
        }
    }

    pub(super) fn desk_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        let fleet = self.fleet.state();
        let Some(desk) = &mut self.desk else {
            return;
        };
        if let Some(onboarding) = &mut desk.onboarding {
            let effects = onboarding.mouse(mouse, &fleet);
            drop(fleet);
            self.setup_effects(effects);
        } else if let Some(page) = &mut desk.page {
            let effects = page.mouse(mouse);
            drop(fleet);
            self.settings_effects(effects);
        }
    }

    pub(super) fn desk_paste(&mut self, pasted: &str) {
        if let Some(desk) = &mut self.desk {
            if let Some(onboarding) = &mut desk.onboarding {
                onboarding.paste(pasted);
            } else if let Some(page) = &mut desk.page {
                page.paste(pasted);
            }
        }
    }

    pub(super) fn desk_field_text(&self) -> Option<bool> {
        let desk = self.desk.as_ref()?;
        if let Some(onboarding) = &desk.onboarding {
            return Some(onboarding.field_text());
        }
        desk.page.as_ref().map(SettingsScreen::field_text)
    }

    pub(super) fn desk_kill_field(&mut self) {
        if let Some(desk) = &mut self.desk {
            if let Some(onboarding) = &mut desk.onboarding {
                onboarding.kill_field();
            } else if let Some(page) = &mut desk.page {
                page.kill_field();
            }
        }
    }

    /// The fleet moved: the last step watches for the phone's agent.
    pub(super) fn desk_fleet_changed(&mut self) {
        let fleet = self.fleet.state();
        let effects = match self.desk.as_mut().and_then(|desk| desk.onboarding.as_mut()) {
            Some(onboarding) => onboarding.fleet_changed(&fleet),
            None => return,
        };
        drop(fleet);
        self.setup_effects(effects);
    }

    /// When the pairing window wants renewing.
    pub(super) fn desk_next_tick(&self) -> Option<std::time::Instant> {
        let at = self.desk.as_ref()?.onboarding.as_ref()?.renew_at()?;
        let wait = u64::try_from(at.saturating_sub(now_ms())).unwrap_or(0);
        Some(std::time::Instant::now() + std::time::Duration::from_millis(wait))
    }

    pub(super) fn desk_tick(&mut self) {
        let effects = match self.desk.as_mut().and_then(|desk| desk.onboarding.as_mut()) {
            Some(onboarding) => onboarding.tick(now_ms()),
            None => return,
        };
        self.setup_effects(effects);
    }

    /// Closes what onboarding holds open, as the client quits: the pairing
    /// window first of all, so no window outlives the screen that showed
    /// its code.
    pub(crate) fn desk_closing(
        &mut self,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        let close = self
            .desk
            .as_mut()
            .and_then(|desk| desk.onboarding.as_mut())
            .map(Onboarding::close)
            .unwrap_or_default()
            .contains(&SetupEffect::ClosePairing);
        let mut closed = None;
        if let Some(desk) = &mut self.desk {
            if let Some(watch) = desk.pairing_watch.take() {
                watch.abort();
            }
            if let Some(sign_in) = desk.sign_in.take() {
                sign_in.abort();
            }
            if close {
                // After any opening still on its way, so the window it
                // opens is the one closed.
                let (done, finished) = oneshot::channel();
                let client = self.client.clone();
                desk.pairing_calls.run(async move {
                    let _ = client.cancel_pairing().await;
                    let _ = done.send(());
                });
                closed = Some(finished);
            }
        }
        async move {
            if let Some(closed) = closed {
                let _ = closed.await;
            }
        }
    }

    pub(super) fn draw_desk(
        &mut self,
        paint: &mut ratatui::Frame<'_>,
        area: ratatui::layout::Rect,
        footer: Option<ratatui::text::Line<'static>>,
    ) -> bool {
        let theme = self.config.theme;
        let Some(desk) = &mut self.desk else {
            return false;
        };
        if let Some(onboarding) = &mut desk.onboarding {
            onboarding.draw(paint, area, footer, theme);
            return true;
        }
        if let Some(page) = &mut desk.page {
            page.draw(paint, area, footer, theme);
            return true;
        }
        false
    }

    fn answer(&self, answer: Answer) {
        let Some(desk) = &self.desk else {
            return;
        };
        let installation = desk.config.installation.clone();
        let request = wire::AnswerOnboardingRequest {
            profile_id: desk.config.profile.id.clone(),
            answer: Some(answer),
            surface: wire::Surface::Terminal as i32,
        };
        // The steps come back on their stream.
        tokio::spawn(async move {
            let _ = installation.answer_onboarding(request).await;
        });
    }

    /// Starts the browser sign-in: the code comes back to show, the
    /// browser opens at the approval page with it filled in, and the end
    /// comes back signed in or not.
    fn start_sign_in(&mut self) {
        let send = self.desk_send();
        let Some(desk) = &mut self.desk else {
            return;
        };
        if let Some(running) = desk.sign_in.take() {
            running.abort();
        }
        let installation = desk.config.installation.clone();
        let request = wire::SignInRequest {
            profile_id: Some(desk.config.profile.id.clone()),
            cloud_url: desk.config.cloud_url.clone(),
            // This profile is the one being set up: what it already holds
            // (the phone just paired) is the person's.
            adopt_non_pristine: true,
        };
        let task = tokio::spawn(async move {
            use wire::sign_in_event::Of;
            let mut events = match installation.sign_in(request).await {
                Ok(events) => events,
                Err(error) => return send(DeskEvent::SignInFailed(sign_in_words(&error))),
            };
            while let Some(event) = events.next().await {
                match event {
                    Ok(wire::SignInEvent {
                        of: Some(Of::Code(code)),
                    }) => {
                        let opened = super::open_url(&code.verification_uri_complete).is_ok();
                        send(DeskEvent::SignInCode { code, opened });
                    }
                    Ok(wire::SignInEvent {
                        of: Some(Of::Adopt(asked)),
                    }) => {
                        let _ = installation
                            .answer_sign_in(wire::AnswerSignInRequest {
                                sign_in_id: asked.sign_in_id,
                                adopt: true,
                            })
                            .await;
                    }
                    Ok(wire::SignInEvent {
                        of: Some(Of::SignedIn(_)),
                    }) => return send(DeskEvent::SignedIn),
                    Ok(_) => {}
                    Err(error) => return send(DeskEvent::SignInFailed(sign_in_words(&error))),
                }
            }
            send(DeskEvent::SignInFailed(
                "the sign-in ended before it finished".into(),
            ));
        });
        desk.sign_in = Some(task.abort_handle());
    }

    pub(super) fn setup_effects(&mut self, effects: Vec<SetupEffect>) {
        for effect in effects {
            self.setup_effect(effect);
        }
    }

    fn setup_effect(&mut self, effect: SetupEffect) {
        let send = self.desk_send();
        let client = self.client.clone();
        let events = self.events.clone();
        let Some(desk) = &mut self.desk else {
            return;
        };
        let installation = desk.config.installation.clone();
        let profile_id = desk.config.profile.id.clone();
        match effect {
            SetupEffect::Answer(answer) => self.answer(answer),
            SetupEffect::Confirm { settings, step } => {
                let confirm = step.filter(|step| {
                    desk.steps
                        .as_ref()
                        .is_some_and(|steps| ui_view::step_open(steps, *step))
                });
                tokio::spawn(async move {
                    match installation.update_settings(*settings).await {
                        Ok(settings) => send(DeskEvent::Settings(settings)),
                        Err(error) => return send(DeskEvent::ConfirmFailed(error.to_string())),
                    }
                    if let Some(step) = confirm {
                        let answer = wire::AnswerOnboardingRequest {
                            profile_id,
                            answer: Some(Answer::Done(step.id() as i32)),
                            surface: wire::Surface::Terminal as i32,
                        };
                        if let Err(error) = installation.answer_onboarding(answer).await {
                            return send(DeskEvent::ConfirmFailed(error.to_string()));
                        }
                    }
                    if let Some(step) = step {
                        send(DeskEvent::Confirmed(step));
                    }
                });
            }
            SetupEffect::Rename(name) => self.update_settings(wire::UpdateSettingsRequest {
                host_name: Some(name),
                ..Default::default()
            }),
            SetupEffect::OpenPairing => {
                if desk.pairing_watch.is_none() {
                    let watched = send.clone();
                    let client = client.clone();
                    let task = tokio::spawn(async move {
                        let Ok(mut statuses) = client.watch_pairing().await else {
                            return;
                        };
                        while let Some(Ok(status)) = statuses.next().await {
                            watched(DeskEvent::Pairing(status));
                        }
                    });
                    desk.pairing_watch = Some(task.abort_handle());
                }
                // A renewal closes the window it replaces first: one window
                // is open at a time.
                let renewing = desk
                    .onboarding
                    .as_ref()
                    .is_some_and(Onboarding::holds_window);
                desk.pairing_calls.run(async move {
                    if renewing {
                        let _ = client.cancel_pairing().await;
                    }
                    let opened = client
                        .start_pairing(wire::StartPairingRequest {
                            method: Some(wire::start_pairing_request::Method::PinOrQr(
                                wire::Empty {},
                            )),
                            ttl_seconds: Some(WINDOW_SECONDS),
                        })
                        .await
                        .map_err(|error| pairing_words(&error));
                    send(DeskEvent::PairingOpened(opened));
                });
            }
            SetupEffect::ClosePairing => {
                if let Some(watch) = desk.pairing_watch.take() {
                    watch.abort();
                }
                desk.pairing_calls.run(async move {
                    let _ = client.cancel_pairing().await;
                });
            }
            SetupEffect::SignIn => self.start_sign_in(),
            SetupEffect::CancelSignIn => {
                if let Some(running) = desk.sign_in.take() {
                    running.abort();
                }
            }
            SetupEffect::OfferSignIn(host_id) => {
                tokio::spawn(async move {
                    if let Err(error) = client.offer_sign_in(host_id).await {
                        let _ = events.send(AppEvent::Notice(
                            format!("could not offer the account: {error}"),
                            Tone::Warn,
                        ));
                    }
                });
            }
            SetupEffect::AnswerOffer { host_id, accept } => {
                tokio::spawn(async move {
                    let answered = client
                        .answer_sign_in_offer(wire::AnswerSignInOfferRequest { host_id, accept })
                        .await
                        .map(|_| accept)
                        .map_err(|error| error.to_string());
                    send(DeskEvent::OfferAnswered(answered));
                });
            }
            SetupEffect::OfferPage => {
                tokio::spawn(async move {
                    let link = installation
                        .web_sign_in_link(wire::WebSignInLinkRequest {
                            profile_id,
                            path: "/offer".to_owned(),
                        })
                        .await;
                    let notice = match link {
                        Ok(url) => match super::open_url(&url) {
                            Ok(()) => return,
                            Err(_) => format!("open {url} to see what Pro adds"),
                        },
                        Err(error) => format!("could not open the account's page: {error}"),
                    };
                    let _ = events.send(AppEvent::Notice(notice, Tone::Info));
                });
            }
            SetupEffect::OpenUrl(url) => {
                if let Err(error) = super::open_url(&url) {
                    self.notice(format!("could not open the link: {error}"), Tone::Warn);
                }
            }
            SetupEffect::Suggest => {
                tokio::spawn(async move {
                    let suggested = installation
                        .suggest_repositories()
                        .await
                        .unwrap_or_default();
                    send(DeskEvent::Suggested(suggested));
                });
            }
            SetupEffect::Home(agent) => {
                if let Some(watch) = desk.pairing_watch.take() {
                    watch.abort();
                }
                desk.onboarding = None;
                self.home.finish = desk
                    .steps
                    .as_ref()
                    .map_or(0, |steps| ui_view::onboarding_view(steps).to_finish);
                if let Some(agent) = agent {
                    let name = self
                        .fleet
                        .state()
                        .agent(&agent)
                        .map(|entry| entry.name.clone())
                        .unwrap_or_default();
                    self.home.select(agent);
                    self.notice(format!("There it is: {name}"), Tone::Info);
                }
            }
        }
    }

    fn update_settings(&self, request: wire::UpdateSettingsRequest) {
        let Some(desk) = &self.desk else {
            return;
        };
        let installation = desk.config.installation.clone();
        let send = self.desk_send();
        tokio::spawn(async move {
            match installation.update_settings(request).await {
                Ok(settings) => send(DeskEvent::Settings(settings)),
                Err(error) => send(DeskEvent::Refused(format!("not changed: {error}"))),
            }
        });
    }

    fn settings_effects(&mut self, effects: Vec<SettingsEffect>) {
        for effect in effects {
            match effect {
                SettingsEffect::Update(request) => self.update_settings(*request),
                SettingsEffect::SignIn => self.start_sign_in(),
                SettingsEffect::CancelSignIn => {
                    if let Some(running) = self.desk.as_mut().and_then(|desk| desk.sign_in.take()) {
                        running.abort();
                    }
                }
                SettingsEffect::SignOut => {
                    let client = self.client.clone();
                    let events = self.events.clone();
                    tokio::spawn(async move {
                        let said = match client.sign_out().await {
                            Ok(()) => AppEvent::Notice("signed out".into(), Tone::Info),
                            Err(error) => {
                                AppEvent::Notice(format!("could not sign out: {error}"), Tone::Warn)
                            }
                        };
                        let _ = events.send(said);
                    });
                }
                SettingsEffect::Close => {
                    if let Some(desk) = &mut self.desk {
                        desk.page = None;
                    }
                }
            }
        }
    }
}

/// Why a sign-in did not finish, as a person reads it.
fn sign_in_words(error: &client::RpcError) -> String {
    match error.code() {
        Some(wire::ErrorCode::Cancelled) => "not signed in".to_owned(),
        _ => format!("could not sign in: {error}"),
    }
}

/// Why no pairing window opened.
fn pairing_words(error: &client::RpcError) -> String {
    if error.to_string().contains("PAIR_MODE_ALREADY_ACTIVE") {
        "pairing is open in another terminal (amux pair); close it there".to_owned()
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A close handed over while an open is still waiting on the daemon
    /// runs after the open, not before it.
    #[tokio::test]
    async fn a_close_waits_for_the_open_before_it() {
        let done = Arc::new(Mutex::new(Vec::new()));
        let (answer, answered) = oneshot::channel::<()>();
        let mut calls = InOrder::default();
        let opened = done.clone();
        calls.run(async move {
            let _ = answered.await;
            opened.lock().unwrap().push("open");
        });
        let (closing, closed) = oneshot::channel();
        let closer = done.clone();
        calls.run(async move {
            closer.lock().unwrap().push("close");
            let _ = closing.send(());
        });
        tokio::task::yield_now().await;
        assert!(done.lock().unwrap().is_empty(), "the close waits");
        answer.send(()).unwrap();
        closed.await.unwrap();
        assert_eq!(*done.lock().unwrap(), ["open", "close"]);
    }
}
