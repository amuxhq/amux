//! The delta and ticket outbox drained on each `amux_runtime_step`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use app_embedded::ProfileView;
use app_runtime::values::{
    ChatDelta, FleetDelta, OnboardingState, PairingStatusView, SignInOfferView,
};
use app_runtime::{AppRuntime, Chat};
use serde::Serialize;
use tokio::runtime::Handle;

/// What the transport left for the app's next step: which views moved,
/// and the answers to acts it started. Nothing here calls the app; each
/// change rings the wake descriptor instead.
pub(crate) struct Outbox {
    pub(crate) tickets: AtomicU64,
    pub(crate) fleets: AtomicU64,
    pub(crate) waiting: Mutex<Waiting>,
    /// The fleets and chats open, by the numbers deltas name them by (a
    /// fleet as chat 0): nothing closed is handed over again, however many
    /// of its acts are still in flight.
    pub(crate) open: Mutex<HashMap<(u64, u64), View>>,
    pub(crate) ring: Box<dyn Fn() + Send + Sync>,
}

#[derive(Clone)]
pub(crate) enum View {
    Fleet(Weak<AppRuntime>),
    Chat(Weak<Chat>),
}

#[derive(Default)]
pub(crate) struct Waiting {
    profiles: Option<Vec<ProfileView>>,
    /// The fleets (chat 0) and chats that moved.
    moved: BTreeSet<(u64, u64)>,
    /// Each open profile's sign-in offers, by fleet, when they moved.
    offers: BTreeMap<u64, Vec<SignInOfferView>>,
    /// Each followed profile's setting up, by fleet, when it moved.
    onboarding: BTreeMap<u64, OnboardingState>,
    /// Each open pairing window's status, by fleet, when it moved.
    pairing: BTreeMap<u64, PairingStatusView>,
    pub(crate) answers: Vec<Answer>,
}

/// What a step hands the app, as JSON.
#[derive(Default, Serialize)]
pub(crate) struct Stepped {
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
    pub(crate) answers: Vec<Answer>,
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
pub(crate) struct Answer {
    pub(crate) ticket: u64,
    /// The act's result, as JSON text.
    pub(crate) json: String,
}

impl Outbox {
    fn post(&self, post: impl FnOnce(&mut Waiting)) {
        post(&mut self.waiting.lock().unwrap_or_else(|p| p.into_inner()));
        (self.ring)();
    }

    /// Posts the profile list as it stands now; a list that cannot be read
    /// is left for the profile stream to post.
    pub(crate) async fn post_profiles(&self, installation: &dyn client::Installation) {
        if let Ok(profiles) = installation.list_profiles().await {
            self.post_listed(&profiles);
        }
    }

    pub(crate) fn post_listed(&self, profiles: &[wire::Profile]) {
        let views = profiles.iter().map(ProfileView::from).collect();
        self.post(|waiting| waiting.profiles = Some(views));
    }

    /// What waits for the app, each view that moved as its delta. Called
    /// inside the runtime, since reading a row can start a fetch.
    pub(crate) fn take(&self) -> Stepped {
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
    pub(crate) fn post_offers(&self, fleet: u64, offers: Vec<SignInOfferView>) {
        self.post(|waiting| {
            waiting.offers.insert(fleet, offers);
        });
    }

    /// Hands the app a profile's setting up in the next step.
    pub(crate) fn post_onboarding(&self, fleet: u64, onboarding: OnboardingState) {
        self.post(|waiting| {
            waiting.onboarding.insert(fleet, onboarding);
        });
    }

    /// Hands the app a profile's pairing window status in the next step.
    pub(crate) fn post_pairing(&self, fleet: u64, status: PairingStatusView) {
        self.post(|waiting| {
            waiting.pairing.insert(fleet, status);
        });
    }

    /// Names a fleet (chat 0) or chat in the next step.
    pub(crate) fn redraw(&self, fleet: u64, chat: u64) {
        self.post(|waiting| {
            waiting.moved.insert((fleet, chat));
        });
    }

    pub(crate) fn opened(&self, fleet: u64, chat: u64, view: View) {
        self.open
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert((fleet, chat), view);
    }

    /// Closes a fleet with every chat of it, or one chat.
    pub(crate) fn close(&self, fleet: u64, chat: Option<u64>) {
        self.open
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(of, open), _| *of != fleet || chat.is_some_and(|chat| *open != chat));
    }

    pub(crate) fn act<T, F>(self: &Arc<Self>, handle: &Handle, act: F) -> u64
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
