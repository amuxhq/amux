//! Product analytics: what a device tells amux.sh about how amux is used.
//!
//! [`Event`] is every event a device may send. Its fields are enums,
//! numbers, bools, versions and host ids only, so nothing a person typed,
//! named or ran can be put in one. Each profile holds an [`Analytics`]
//! handle carrying its host id; recording is a non-blocking push that drops
//! when the queue is full, and a handle that is off does nothing, so call
//! sites never ask whether telemetry is on. One [`Uploader`] per
//! installation batches what the handles record and posts it, per host, to
//! `POST <base>/api/events`.
//!
//! This is separate from the daemon's local audit log on purpose: that log
//! names hosts and carries reasons as text, which must never leave the
//! machine.

mod context;
mod event;
mod upload;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

pub use context::{
    Channel, Context, DO_NOT_TRACK_ENV, Endpoint, Platform, URL_ENV, do_not_track, events_url,
};
pub use event::{
    Ask, By, CheckIn, Client, Counts, Event, Interval, Kind, Method, On, PairingFailure,
    PaywallFrom, Recorded, RelayCarrier, Role, Route, SignInFailure, Tier, Token,
};
pub use upload::{
    Accounts, Flusher, Gate, Http, MAX_EVENTS_PER_REQUEST, Params, Posted, Transport, Upload,
    Uploader,
};

/// A host's id: one per profile, the `distinct_id` of its events.
pub type HostId = uuid::Uuid;

/// Where recorded events go.
pub trait Sink: Send + Sync + 'static {
    /// Takes the event without waiting; may drop it.
    fn record(&self, host: HostId, recorded: Recorded);
}

/// One profile's way to record events. Cheap to clone; off does nothing.
#[derive(Clone, Default)]
pub struct Analytics(Option<Arc<Shared>>);

struct Shared {
    host: HostId,
    sink: Arc<dyn Sink>,
    /// When each throttled event was last recorded.
    last: Mutex<HashMap<&'static str, Instant>>,
}

impl Analytics {
    /// A handle that records nothing.
    pub fn off() -> Self {
        Self(None)
    }

    pub fn new(host: HostId, sink: Arc<dyn Sink>) -> Self {
        Self(Some(Arc::new(Shared {
            host,
            sink,
            last: Mutex::new(HashMap::new()),
        })))
    }

    pub fn is_on(&self) -> bool {
        self.0.is_some()
    }

    pub fn record(&self, event: Event) {
        if let Some(shared) = &self.0 {
            shared.sink.record(
                shared.host,
                Recorded {
                    event,
                    at: SystemTime::now(),
                },
            );
        }
    }

    /// Records the event unless one of the same name was recorded within
    /// `period`: for what can repeat faster than it means anything new.
    pub fn record_at_most_every(&self, period: Duration, event: Event) {
        let Some(shared) = &self.0 else {
            return;
        };
        let now = Instant::now();
        {
            let mut last = shared.last.lock().unwrap_or_else(|p| p.into_inner());
            if last
                .get(event.name())
                .is_some_and(|at| now.duration_since(*at) < period)
            {
                return;
            }
            last.insert(event.name(), now);
        }
        self.record(event);
    }
}

/// A sink that keeps what it is given, for tests.
#[derive(Default)]
pub struct Recording(Mutex<Vec<(HostId, Event)>>);

impl Recording {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Every event so far, in order, with the host that recorded it.
    pub fn events(&self) -> Vec<(HostId, Event)> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The events so far with this name.
    pub fn named(&self, name: &str) -> Vec<Event> {
        self.events()
            .into_iter()
            .filter(|(_, event)| event.name() == name)
            .map(|(_, event)| event)
            .collect()
    }
}

impl Sink for Recording {
    fn record(&self, host: HostId, recorded: Recorded) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((host, recorded.event));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;

    #[test]
    fn throttled_events_are_kept_at_most_once_per_period() {
        let recording = Recording::new();
        let analytics = Analytics::new(HostId::from_u128(1), recording.clone());
        let opened = Event::ClientOpened {
            client: Client::Phone,
        };
        analytics.record_at_most_every(Duration::from_secs(3600), opened.clone());
        analytics.record_at_most_every(Duration::from_secs(3600), opened.clone());
        analytics.record(Event::Installed);
        analytics.record_at_most_every(Duration::ZERO, Event::RelayRefused);
        analytics.record_at_most_every(Duration::ZERO, Event::RelayRefused);
        assert_eq!(recording.named("client_opened").len(), 1);
        assert_eq!(recording.named("installed").len(), 1);
        assert_eq!(recording.named("relay_refused").len(), 2);
    }

    #[test]
    fn the_examples_cover_every_variant() {
        let ordinals: BTreeSet<usize> = Event::examples().iter().map(Event::ordinal).collect();
        assert_eq!(ordinals, (0..Event::COUNT).collect());
    }

    /// The JSON every example becomes, as the wire contract spells it.
    #[test]
    fn events_serialise_as_the_contract_spells_them() {
        let host = HostId::from_u128(0x5a5a).to_string();
        let examples: Vec<_> = Event::examples()
            .iter()
            .map(|event| (event.name(), json!(event.properties())))
            .collect();
        let expected = vec![
            ("installed", json!({})),
            (
                "checked_in",
                json!({
                    "agents_running": {"codex": 1},
                    "agents_created_24h": {"claude_pty": 2},
                    "turns_24h": {"claude_pty": 7},
                    "paired_hosts": 2,
                    "hosts_by_route": {"direct": 1, "relay": 1},
                    "relay_carrier": "quic",
                    "signed_in": true,
                    "tier": "free",
                }),
            ),
            (
                "updated",
                json!({"from_version": "0.7.0", "to_version": "0.8.0"}),
            ),
            (
                "update_rolled_back",
                json!({"from_version": "0.8.0", "to_version": "0.7.0"}),
            ),
            ("daemon_crashed", json!({"crashed_version": "0.8.0"})),
            ("client_opened", json!({"surface": "terminal"})),
            (
                "agent_created",
                json!({"kind": "claude_sdk", "on": "this_host", "by": "person"}),
            ),
            (
                "prompt_sent",
                json!({"kind": "codex", "on": "paired_host", "remote_host": host, "queued": false}),
            ),
            (
                "ask_answered",
                json!({"kind": "claude_pty", "on": "paired_host", "remote_host": host, "ask": "permission"}),
            ),
            ("pairing_started", json!({"role": "joiner", "method": "qr"})),
            (
                "pairing_succeeded",
                json!({"role": "joiner", "method": "qr", "remote_host": host}),
            ),
            (
                "pairing_failed",
                json!({"role": "offerer", "method": "pin", "reason": "wrong_secret"}),
            ),
            ("signed_in", json!({})),
            ("sign_in_failed", json!({"reason": "rejected"})),
            ("signed_out", json!({})),
            ("relay_refused", json!({})),
            ("paywall_viewed", json!({"from": "hosts"})),
            ("purchase_started", json!({"interval": "yearly"})),
        ];
        assert_eq!(examples, expected);
    }

    /// Every value a property can take is a token the server accepts:
    /// `[a-z0-9_.:-]{1,64}`, a UUID, a number, a bool or a map of counts.
    #[test]
    fn every_value_is_a_token_a_number_or_a_uuid() {
        fn check(key: &str, value: &serde_json::Value) {
            match value {
                serde_json::Value::String(text) => {
                    let token = !text.is_empty()
                        && text.len() <= 64
                        && text.chars().all(|c| {
                            c.is_ascii_lowercase()
                                || c.is_ascii_digit()
                                || matches!(c, '_' | '.' | ':' | '-')
                        });
                    assert!(token, "{key}: {text:?} is not a token");
                }
                serde_json::Value::Number(_) | serde_json::Value::Bool(_) => {}
                serde_json::Value::Object(counts) => {
                    for (inner, count) in counts {
                        check(inner, &json!(inner));
                        assert!(count.is_u64(), "{key}.{inner} is not a count");
                    }
                }
                other => panic!("{key}: {other} is not a value the server takes"),
            }
        }
        for event in Event::examples() {
            for (key, value) in event.properties() {
                check(&key, &value);
            }
        }
        for kind in Kind::ALL {
            check("kind", &json!(kind.as_str()));
        }
        for reason in PairingFailure::ALL {
            check("reason", &json!(reason.as_str()));
        }
        for reason in SignInFailure::ALL {
            check("reason", &json!(reason.as_str()));
        }
        for ask in Ask::ALL {
            check("ask", &json!(ask.as_str()));
        }
    }

    /// The public page lists every event and every property a device sends.
    #[test]
    fn the_public_page_names_every_event_and_property() {
        let page = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/ANALYTICS.md"),
        )
        .expect("docs/ANALYTICS.md exists");
        let mut missing = Vec::new();
        for event in Event::examples() {
            let name = format!("`{}`", event.name());
            if !page.contains(&name) {
                missing.push(name);
            }
            for key in event.properties().keys() {
                let key = format!("`{key}`");
                if !page.contains(&key) {
                    missing.push(format!("{key} (of {})", event.name()));
                }
            }
        }
        for key in [
            "installation_id",
            "host_id",
            "platform",
            "os_version",
            "arch",
            "version",
            "channel",
        ] {
            let key = format!("`{key}`");
            if !page.contains(&key) {
                missing.push(key);
            }
        }
        assert!(
            missing.is_empty(),
            "docs/ANALYTICS.md does not mention: {}",
            missing.join(", ")
        );
    }
}
