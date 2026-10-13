//! Around the composer: context use, a sign-in problem, the composer's mode
//! and tokens, the queued prompts and this client's unconfirmed inputs.

use schemars::JsonSchema;
use serde::Serialize;
use ui_state::{
    Activity, Composer, InputState, InputWhat, PhaseView, Refusal, SessionState, Waiting,
};
use wire::{Attachment, SignInState};

use crate::segments::{Segment, segments};

/// Context use from here on is near full, which a client may set apart.
pub const CONTEXT_NEAR_FULL_PERCENT: u64 = 80;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ContextView {
    pub used_tokens: u64,
    pub window_tokens: Option<u64>,
    pub percent: Option<u64>,
    /// At or past [`CONTEXT_NEAR_FULL_PERCENT`] of the window.
    pub near_full: bool,
}

/// How much of its context window the agent uses, once it has said.
pub fn context(state: &SessionState) -> Option<ContextView> {
    let agent = state.agent_state();
    agent.context.known.then(|| {
        let percent = agent
            .context
            .window_tokens
            .filter(|window| *window > 0)
            .map(|window| agent.context.used_tokens * 100 / window);
        ContextView {
            used_tokens: agent.context.used_tokens,
            window_tokens: agent.context.window_tokens,
            percent,
            near_full: percent.is_some_and(|percent| percent >= CONTEXT_NEAR_FULL_PERCENT),
        }
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SignInView {
    pub state: SignInState,
    pub account: String,
    pub message: String,
}

/// The agent's account, only when it needs signing in: it replaces the
/// composer with a foot card.
pub fn sign_in(state: &SessionState) -> Option<SignInView> {
    let agent = state.agent_state();
    match agent.sign_in.state() {
        SignInState::SignedOut | SignInState::Expired | SignInState::Failed => Some(SignInView {
            state: agent.sign_in.state(),
            account: agent.sign_in.account.clone(),
            message: agent.sign_in.message.clone(),
        }),
        SignInState::Unknown | SignInState::SignedIn => None,
    }
}

/// The composer as the chat draws it: Send, Resume for an exited agent,
/// or waiting while drafting continues; with the activity line inside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ComposerView {
    pub mode: Composer,
    pub activity: Option<Activity>,
    /// The agent's own view is on another conversation: prompts are
    /// refused until a switch back.
    pub switched_away: bool,
}

pub fn composer(state: &SessionState, now_ms: i64) -> ComposerView {
    ComposerView {
        mode: state.composer(),
        activity: crate::fleet::activity(state, now_ms),
        switched_away: switched_away(state),
    }
}

/// Why the chat is not current, for a client that words the composer
/// holding its sends.
pub fn waiting(state: &SessionState) -> Option<Waiting> {
    match state.composer() {
        Composer::Holds(why) => Some(why),
        Composer::Send | Composer::Resume => None,
    }
}

/// The draft as tokens: text runs and attachment chips at their places.
pub fn composer_tokens(draft: &str, attachments: &[Attachment]) -> Vec<Segment> {
    segments(draft, attachments)
}

/// A queued prompt under the composer.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct QueuedRow {
    pub input_id: Vec<u8>,
    pub text: Vec<Segment>,
    /// Who queued it: None for a person, the agent's name for an agent.
    pub from_agent: Option<String>,
    /// Reads "steered" until its reflection lands.
    pub steered: bool,
    pub can_withdraw: bool,
    /// Send now steers it into the running turn.
    pub can_send_now: bool,
}

pub fn queue_rows(state: &SessionState) -> Vec<QueuedRow> {
    let live = state.can_send();
    state
        .queue()
        .into_iter()
        .map(|row| {
            let from_agent = match row
                .entry
                .sender
                .as_ref()
                .and_then(|sender| sender.value.as_ref())
            {
                Some(wire::sender::Value::Agent(agent)) => Some(agent.name.clone()),
                _ => None,
            };
            QueuedRow {
                input_id: row.entry.input_id.clone(),
                text: segments(&row.entry.text, &row.entry.attachments),
                from_agent,
                steered: row.steered,
                can_withdraw: live && !row.steered,
                can_send_now: live && !row.steered,
            }
        })
        .collect()
}

/// Where a new prompt of this client's lands: at the feed's end, as it
/// will stand once the agent has it, while the agent is idle with nothing
/// queued; otherwise in the queue, behind what waits there.
pub fn sends_to_feed(state: &SessionState) -> bool {
    state.phase() == PhaseView::Idle && state.queue().is_empty()
}

/// Where a prompt on its way is drawn: where it was first drawn, which the
/// client remembers, so a prompt does not jump as the agent starts work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum Lands {
    Feed,
    Queue,
}

/// How a prompt of this client's is on its way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum Underway {
    /// Sent; `waiting` while the link to the agent's host is down, which
    /// is what the prompt waits for.
    Sending { waiting: bool },
    /// The connection dropped before a reply, and catching up found it
    /// neither queued nor in the transcript: only the person can say
    /// whether to send it again or discard it.
    MayNotHaveArrived,
    /// It never left: the agent's host cannot be reached. It goes once the
    /// host is back; Cancel takes it back into the draft.
    Held,
}

/// A prompt of this client's on its way to the agent.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct SentPrompt {
    pub input_id: Vec<u8>,
    pub text: Vec<Segment>,
    pub lands: Lands,
    pub underway: Underway,
}

/// A prompt the agent refused. Its words go back to the composer, with
/// why, and it is forgotten.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RefusedPrompt {
    pub input_id: Vec<u8>,
    pub reason: Refusal,
}

/// This client's prompts on their way, in the order they will land: those
/// sending first, then those that may not have arrived, which wait last in
/// the queue for the person. `in_feed` says whether a prompt was first
/// drawn at the feed's end; a client asks [`sends_to_feed`] when it first
/// sees one. A prompt the agent's queue lists is its queue row's to draw.
pub fn prompts_underway(state: &SessionState, in_feed: impl Fn(&[u8]) -> bool) -> Vec<SentPrompt> {
    let listed: Vec<&[u8]> = state
        .queue()
        .iter()
        .map(|row| row.entry.input_id.as_slice())
        .collect();
    let caught_up = state.caught_up();
    let mut sending = Vec::new();
    let mut unconfirmed = Vec::new();
    for sent in state.inputs().iter() {
        let InputWhat::Prompt { text, attachments } = &sent.what else {
            continue;
        };
        if listed.contains(&sent.id.as_slice()) {
            continue;
        }
        let prompt = |lands, underway| SentPrompt {
            input_id: sent.id.clone(),
            text: segments(text, attachments),
            lands,
            underway,
        };
        let waiting = Underway::Sending {
            waiting: !caught_up,
        };
        let place = if in_feed(&sent.id) {
            Lands::Feed
        } else {
            Lands::Queue
        };
        match &sent.state {
            // Accepted into the queue before a snapshot lists it.
            InputState::Queued => sending.push(prompt(Lands::Queue, waiting)),
            InputState::Sent => sending.push(prompt(place, waiting)),
            InputState::Uncertain if !caught_up => sending.push(prompt(place, waiting)),
            InputState::Uncertain => {
                unconfirmed.push(prompt(Lands::Queue, Underway::MayNotHaveArrived))
            }
            InputState::Held => sending.push(prompt(Lands::Queue, Underway::Held)),
            InputState::Rejected(_) | InputState::Settled => {}
        }
    }
    sending.extend(unconfirmed);
    sending
}

/// This client's prompts the agent refused, with why.
pub fn refused_prompts(state: &SessionState) -> Vec<RefusedPrompt> {
    state
        .inputs()
        .iter()
        .filter_map(|sent| match (&sent.what, &sent.state) {
            (InputWhat::Prompt { .. }, InputState::Rejected(reason)) => Some(RefusedPrompt {
                input_id: sent.id.clone(),
                reason: reason.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Whether the agent's own view is on another conversation than the
/// agent's (terminal Claude resumed another in its own screen): the agent
/// takes no prompt until it is back, and a switch back takes it back.
pub fn switched_away(state: &SessionState) -> bool {
    !matches!(state.composer(), Composer::Resume) && state.agent_state().switched_away_to.is_some()
}
