//! The daemon's side of product analytics: which profiles the uploader
//! posts for, what a profile reports once a day, and how the daemon's own
//! values read as events. The events and the uploader are the
//! [`analytics`] crate; this module only translates.

use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::Duration;

use analytics::{Ask, CheckIn, Counts, Event, Kind, On, Route};

use crate::HostId;
use crate::daemon::Installation;
use crate::edge::{Observed, RelayCarrier};
use crate::runtime::ProfileRuntime;

/// What the daemon does with analytics.
#[derive(Clone)]
pub enum Telemetry {
    /// Nothing is recorded: development builds, tests and test networks,
    /// and an installation whose person turned it off.
    Off,
    /// Every profile records into the sink and nothing is sent: for tests.
    Record(Arc<dyn analytics::Sink>),
    /// Events are posted to the endpoint while the gate says yes.
    Upload {
        endpoint: analytics::Endpoint,
        channel: analytics::Channel,
        gate: analytics::Gate,
    },
}

/// A client that opens again within this long is the same session.
pub(crate) const CLIENT_OPENED_EVERY: Duration = Duration::from_secs(60 * 60);
/// A free tier's refusal repeats on every retry; one an hour says it.
pub(crate) const RELAY_REFUSED_EVERY: Duration = Duration::from_secs(60 * 60);
/// How long a profile waits between check-ins.
pub(crate) const CHECK_IN_EVERY_MS: i64 = 24 * 60 * 60 * 1000;
/// How long after a start the first check-in waits at the soonest, so a
/// daemon that only starts to stop again reports nothing.
pub(crate) const CHECK_IN_SETTLE_MS: i64 = 60 * 1000;
/// When the profile last checked in, as Unix milliseconds.
pub(crate) const CHECKED_IN: &str = "checked_in";
/// How long a shutdown waits for the last events to go.
pub(crate) const SHUTDOWN_FLUSH: Duration = Duration::from_secs(2);

/// The installation's profiles, as the uploader asks after them.
pub(crate) struct Profiles(pub(crate) Weak<Installation>);

impl Profiles {
    fn runtime(&self, host: HostId) -> Option<Arc<ProfileRuntime>> {
        let installation = self.0.upgrade()?;
        let hosted = installation.hosted.lock().unwrap();
        hosted
            .values()
            .find(|hosted| hosted.runtime.host() == host)
            .map(|hosted| hosted.runtime.clone())
    }
}

#[async_trait::async_trait]
impl analytics::Accounts for Profiles {
    fn service(&self, host: HostId) -> Option<Option<String>> {
        let runtime = self.runtime(host)?;
        Some(runtime.edge().and_then(|edge| edge.account_service()))
    }

    async fn bearer(&self, host: HostId) -> Option<String> {
        let edge = self.runtime(host)?.edge()?;
        edge.access_token().await.ok().map(|token| token.bearer)
    }
}

pub(crate) fn kind(kind: wire::Kind) -> Option<Kind> {
    match kind {
        wire::Kind::ClaudePty => Some(Kind::ClaudePty),
        wire::Kind::ClaudeSdk => Some(Kind::ClaudeSdk),
        wire::Kind::Codex => Some(Kind::Codex),
        wire::Kind::Unspecified => None,
    }
}

pub(crate) fn kind_named(name: &str) -> Option<Kind> {
    wire::kind_from_tag(name).and_then(kind)
}

/// What a person's input is, as analytics counts it: a prompt or an answer
/// to an ask. Keys, interrupts and settings are not counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputShape {
    Prompt(Kind),
    Answer(Kind, Ask),
}

impl InputShape {
    /// Read from a decoded copy of the body. An input whose body this
    /// daemon cannot decode, of a kind or arm it does not know, is not
    /// counted; it is still delivered, since the agent alone decides it.
    pub(crate) fn of(input: &wire::Input) -> Option<Self> {
        use wire::KindInput;
        Some(match input.decode_body()? {
            KindInput::ClaudePty(input) => {
                use wire::claude_pty_input::Of;
                match input.of? {
                    Of::Prompt(_) => Self::Prompt(Kind::ClaudePty),
                    Of::Answer(answer) => Self::Answer(Kind::ClaudePty, claude_ask(&answer)?),
                    _ => return None,
                }
            }
            KindInput::ClaudeSdk(input) => {
                use wire::claude_sdk_input::Of;
                match input.of? {
                    Of::Prompt(_) => Self::Prompt(Kind::ClaudeSdk),
                    Of::Answer(answer) => Self::Answer(Kind::ClaudeSdk, claude_ask(&answer)?),
                    _ => return None,
                }
            }
            KindInput::Codex(input) => {
                use wire::codex_input::Of;
                match input.of? {
                    Of::Prompt(_) => Self::Prompt(Kind::Codex),
                    Of::Answer(answer) => Self::Answer(Kind::Codex, codex_ask(&answer)?),
                    _ => return None,
                }
            }
        })
    }

    /// The event once the input was accepted.
    pub(crate) fn event(self, on: On, queued: bool) -> Event {
        match self {
            Self::Prompt(kind) => Event::PromptSent { kind, on, queued },
            Self::Answer(kind, ask) => Event::AskAnswered { kind, on, ask },
        }
    }
}

/// Whether a send was accepted, and queued behind a running turn.
pub(crate) fn accepted(response: &wire::SendInputResponse) -> Option<bool> {
    match response.of.as_ref()? {
        wire::send_input_response::Of::Accepted(accepted) => Some(accepted.queued),
        wire::send_input_response::Of::Rejected(_) => None,
    }
}

fn claude_ask(answer: &wire::ClaudeAnswer) -> Option<Ask> {
    use wire::claude_answer::Of;
    Some(match answer.of.as_ref()? {
        Of::Permission(_) => Ask::Permission,
        Of::Question(_) => Ask::Question,
        Of::Plan(_) => Ask::Plan,
        Of::Form(_) => Ask::Form,
        Of::Link(_) => Ask::Link,
        // A reply in place of answering closes the question it stood in for.
        Of::Reply(_) => Ask::Question,
    })
}

fn codex_ask(answer: &wire::CodexAnswer) -> Option<Ask> {
    use wire::codex_answer::Of;
    Some(match answer.of.as_ref()? {
        Of::Approve(_) => Ask::Permission,
        Of::Question(_) => Ask::Question,
        Of::Form(_) => Ask::Form,
        Of::Link(_) => Ask::Link,
        Of::Grant(_) => Ask::Grant,
        Of::Plan(_) => Ask::Plan,
        Of::Reply(_) => Ask::Question,
    })
}

/// When a profile last checked in, or None before its first.
pub(crate) fn last_check_in(profile_dir: &Path) -> Option<i64> {
    std::fs::read_to_string(profile_dir.join(CHECKED_IN))
        .ok()?
        .trim()
        .parse()
        .ok()
}

pub(crate) fn mark_checked_in(profile_dir: &Path, at_ms: i64) {
    if let Err(error) = std::fs::write(profile_dir.join(CHECKED_IN), at_ms.to_string()) {
        tracing::debug!(%error, "could not record the check-in");
    }
}

/// The next check-in: a day after the last, but never before the start
/// has settled.
pub(crate) fn next_check_in(last: Option<i64>, now_ms: i64) -> i64 {
    let settled = now_ms + CHECK_IN_SETTLE_MS;
    last.map_or(settled, |last| (last + CHECK_IN_EVERY_MS).max(settled))
}

impl ProfileRuntime {
    /// What this profile reports once a day.
    pub(crate) async fn check_in(&self) -> CheckIn {
        let now = self.clock().now_ms();
        let live = self.live();
        let mut check_in = CheckIn {
            turns_24h: self.take_turns(),
            ..CheckIn::default()
        };
        {
            let store = self.store().await;
            let rows = store::Store::agents(&*store).unwrap_or_default();
            let own = self.host().as_bytes().to_vec();
            for row in rows.iter().filter(|row| row.agent.host == own) {
                let Some(kind) = kind_named(&row.kind) else {
                    continue;
                };
                let running =
                    HostId::from_slice(&row.agent.agent).is_ok_and(|agent| live.contains(&agent));
                if running {
                    check_in.agents_running.add(kind, 1);
                }
                if row.created_at >= now - CHECK_IN_EVERY_MS {
                    check_in.agents_created_24h.add(kind, 1);
                }
            }
        }
        if let Some(edge) = self.edge() {
            check_in.paired_hosts = edge.list_peers().map_or(0, |peers| peers.len() as u32);
            check_in.hosts_by_route = edge.routes().await;
            check_in.signed_in = edge.account_signed_in() == Some(true);
            if let Observed::Connected { tier, carrier } = edge.observed() {
                check_in.tier = Some(match tier {
                    crate::Tier::Free => analytics::Tier::Free,
                    crate::Tier::Pro => analytics::Tier::Pro,
                });
                check_in.relay_carrier = Some(match carrier {
                    RelayCarrier::Quic => analytics::RelayCarrier::Quic,
                    RelayCarrier::Tcp => analytics::RelayCarrier::Tcp,
                });
            }
        }
        check_in
    }
}

pub(crate) fn route(via: crate::routing::HostVia) -> Option<Route> {
    match via {
        crate::routing::HostVia::Direct => Some(Route::Direct),
        crate::routing::HostVia::Relay => Some(Route::Relay),
        crate::routing::HostVia::Ssh => Some(Route::Ssh),
        crate::routing::HostVia::Offline => None,
    }
}

/// Counts turns as ingest commits them, by kind, until the next check-in
/// takes them.
#[derive(Default)]
pub(crate) struct Turns(std::sync::Mutex<Counts<Kind>>);

impl Turns {
    pub(crate) fn add(&self, kind: Kind, n: u32) {
        self.0.lock().unwrap().add(kind, n);
    }

    pub(crate) fn take(&self) -> Counts<Kind> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_ins_wait_a_day_and_never_come_straight_after_a_start() {
        assert_eq!(next_check_in(None, 1_000), 1_000 + CHECK_IN_SETTLE_MS);
        assert_eq!(next_check_in(Some(0), 10), CHECK_IN_EVERY_MS);
        assert_eq!(
            next_check_in(Some(0), CHECK_IN_EVERY_MS * 3),
            CHECK_IN_EVERY_MS * 3 + CHECK_IN_SETTLE_MS
        );
    }

    fn input(of: impl Into<wire::KindInput>) -> wire::Input {
        wire::Input::new(Vec::new(), of)
    }

    #[test]
    fn prompts_and_answers_are_counted_and_keys_are_not() {
        let prompt = input(wire::CodexInput {
            of: Some(wire::codex_input::Of::Prompt(wire::PromptInput::default())),
        });
        assert_eq!(
            InputShape::of(&prompt).map(|shape| shape.event(On::ThisHost, true)),
            Some(Event::PromptSent {
                kind: Kind::Codex,
                on: On::ThisHost,
                queued: true
            })
        );
        let answer = input(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Answer(wire::ClaudeAnswer {
                ask_key: "k".into(),
                of: Some(wire::claude_answer::Of::Plan(wire::PlanAnswer::default())),
            })),
        });
        let peer = HostId::from_u128(3);
        assert_eq!(
            InputShape::of(&answer).map(|shape| shape.event(On::PairedHost(peer), false)),
            Some(Event::AskAnswered {
                kind: Kind::ClaudeSdk,
                on: On::PairedHost(peer),
                ask: Ask::Plan
            })
        );
        let approve = input(wire::CodexInput {
            of: Some(wire::codex_input::Of::Answer(wire::CodexAnswer {
                ask_key: "k".into(),
                of: Some(wire::codex_answer::Of::Approve(
                    wire::Decision::Approve as i32,
                )),
            })),
        });
        assert_eq!(
            InputShape::of(&approve),
            Some(InputShape::Answer(Kind::Codex, Ask::Permission))
        );
        let key = input(wire::ClaudePtyInput {
            of: Some(wire::claude_pty_input::Of::Interrupt(wire::Interrupt {})),
        });
        assert_eq!(InputShape::of(&key), None);
    }
}
