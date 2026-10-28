//! The events a device may send, and the JSON each becomes.
//!
//! Every field is an enum, a number, a bool, a version, a host id or a
//! check id: there is no string a caller could fill with a name, a path or a
//! prompt, so the type is the allowlist of what can leave the machine.

use std::collections::BTreeMap;
use std::time::SystemTime;

use semver::Version;
use serde_json::{Map, Value, json};

use crate::HostId;

/// A closed set of short lowercase words, each sent as itself.
pub trait Token: Copy + Ord + 'static {
    fn as_str(self) -> &'static str;
}

macro_rules! tokens {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $token:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
        }

        impl Token for $name {
            fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $token),+
                }
            }
        }
    };
}

tokens! {
    /// Which interpreter an agent runs under.
    Kind {
        ClaudePty => "claude_pty",
        ClaudeSdk => "claude_sdk",
        Codex => "codex",
    }
}

tokens! {
    /// Which client opened.
    Client {
        Terminal => "terminal",
        Phone => "phone",
    }
}

tokens! {
    /// Who asked for an agent: a person, or another agent through its tools.
    By {
        Person => "person",
        Agent => "agent",
    }
}

tokens! {
    /// What kind of ask an answer closed.
    Ask {
        Question => "question",
        Permission => "permission",
        Plan => "plan",
        Form => "form",
        Link => "link",
        Grant => "grant",
    }
}

tokens! {
    /// Which side of a pairing this host was: the one showing the code, or
    /// the one entering it.
    Role {
        Offerer => "offerer",
        Joiner => "joiner",
    }
}

tokens! {
    /// How the pairing secret travelled.
    Method {
        Pin => "pin",
        Qr => "qr",
        Ssh => "ssh",
    }
}

tokens! {
    /// Why a pairing did not finish, as a code.
    PairingFailure {
        /// The PIN or QR secret did not match.
        WrongSecret => "wrong_secret",
        /// No pairing window was open, or it had closed.
        NoWindow => "no_window",
        /// The other machine could not be reached.
        Unreachable => "unreachable",
        TimedOut => "timed_out",
        /// The other side turned the pairing down or walked away.
        Abandoned => "abandoned",
        /// Pairing a host with itself.
        SelfPairing => "self_pairing",
        Other => "other",
    }
}

tokens! {
    /// Why binding a profile to an account failed, as a code.
    SignInFailure {
        /// The account service refused the login's token.
        Rejected => "rejected",
        /// The account service could not be reached.
        Unreachable => "unreachable",
        /// The account is already signed in on another profile here.
        AccountElsewhere => "account_elsewhere",
        /// The profile belongs to another account, or holds work the
        /// login did not ask to adopt.
        ProfileConflict => "profile_conflict",
        Other => "other",
    }
}

tokens! {
    /// How a paired host is reached.
    Route {
        Direct => "direct",
        Relay => "relay",
        Ssh => "ssh",
    }
}

tokens! {
    /// Which transport the relay link runs on.
    RelayCarrier {
        Quic => "quic",
        Tcp => "tcp",
    }
}

tokens! {
    /// What the account buys.
    Tier {
        Free => "free",
        Pro => "pro",
    }
}

tokens! {
    /// The phone's tab the paywall was opened from.
    PaywallFrom {
        Agents => "agents",
        Hosts => "hosts",
        You => "you",
    }
}

tokens! {
    /// A subscription's billing period.
    Interval {
        Monthly => "monthly",
        Yearly => "yearly",
    }
}

tokens! {
    /// Which part of amux a check that broke belongs to.
    CheckCore {
        Daemon => "daemon",
        Agent => "agent",
        Relay => "relay",
        Path => "path",
        /// A client's check of the chat transcript it holds.
        Transcript => "transcript",
    }
}

tokens! {
    /// How a check broke.
    CheckCause {
        /// One of the core's invariants failed at the end of a step.
        Invariant => "invariant",
        /// A step took longer than the core's budget.
        OverBudget => "over_budget",
    }
}

/// The id of one of amux's own checks, such as `daemon.generation_fell`:
/// lowercase letters, digits, `_` and `.`, at most 64 bytes, with a `.`
/// between the core and the check. The shape is all [`CheckId::new`]
/// accepts, so nothing but a code-chosen name fits.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CheckId(String);

impl CheckId {
    pub fn new(id: &str) -> Option<Self> {
        let shaped = !id.is_empty()
            && id.len() <= 64
            && id.contains('.')
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.');
        shaped.then(|| Self(id.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where an act happened: on this host, or on a paired host, which is then
/// named so a person's hosts can be joined without an account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum On {
    ThisHost,
    PairedHost(HostId),
}

/// Counts by a token, sent as `{token: count}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counts<T: Token>(BTreeMap<T, u32>);

impl<T: Token> Default for Counts<T> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}

impl<T: Token> Counts<T> {
    pub fn add(&mut self, key: T, n: u32) {
        if n > 0 {
            *self.0.entry(key).or_default() += n;
        }
    }

    pub fn get(&self, key: T) -> u32 {
        self.0.get(&key).copied().unwrap_or(0)
    }

    fn to_json(&self) -> Value {
        Value::Object(
            self.0
                .iter()
                .map(|(key, n)| (key.as_str().to_owned(), json!(n)))
                .collect(),
        )
    }
}

impl<T: Token> FromIterator<T> for Counts<T> {
    fn from_iter<I: IntoIterator<Item = T>>(keys: I) -> Self {
        let mut counts = Self::default();
        for key in keys {
            counts.add(key, 1);
        }
        counts
    }
}

/// What a profile reports once a day while it runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckIn {
    /// This host's own agents running now.
    pub agents_running: Counts<Kind>,
    /// This host's own agents created in the last 24 hours.
    pub agents_created_24h: Counts<Kind>,
    /// Turns this host's agents ended since the previous check-in or, the
    /// first time after a start, since the start.
    pub turns_24h: Counts<Kind>,
    pub paired_hosts: u32,
    /// Paired hosts by the route that reaches each now; offline ones are
    /// left out.
    pub hosts_by_route: Counts<Route>,
    /// The relay link's carrier, while it is up.
    pub relay_carrier: Option<RelayCarrier>,
    pub signed_in: bool,
    /// What the account buys, as the relay link last said.
    pub tier: Option<Tier>,
}

/// One thing that happened on a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The first start of an installation.
    Installed,
    CheckedIn(CheckIn),
    /// A start found a newer build than the previous run.
    Updated {
        from: Version,
        to: Version,
    },
    /// A start found an older build than the previous run: the newer one
    /// was put back.
    UpdateRolledBack {
        from: Version,
        to: Version,
    },
    /// A start found that the previous run, on this same boot of the
    /// machine, never shut down cleanly.
    DaemonCrashed {
        version: Option<Version>,
    },
    ClientOpened {
        client: Client,
    },
    AgentCreated {
        kind: Kind,
        on: On,
        by: By,
    },
    PromptSent {
        kind: Kind,
        on: On,
        queued: bool,
    },
    AskAnswered {
        kind: Kind,
        on: On,
        ask: Ask,
    },
    PairingStarted {
        role: Role,
        method: Method,
    },
    PairingSucceeded {
        role: Role,
        method: Method,
        remote_host: HostId,
    },
    PairingFailed {
        role: Role,
        method: Method,
        reason: PairingFailure,
    },
    SignedIn,
    SignInFailed {
        reason: SignInFailure,
    },
    SignedOut,
    /// The relay refused to carry traffic for the account's tier.
    RelayRefused,
    /// The phone's subscription page opened.
    PaywallViewed {
        from: PaywallFrom,
    },
    /// The phone started a purchase with the App Store.
    PurchaseStarted {
        interval: Interval,
    },
    /// One of amux's checks broke: a core's invariant failed or a step ran
    /// over its budget, or a client's transcript failed its check. Sent at
    /// most once per check every ten minutes; `held_back` counts the rest.
    CheckFailed {
        core: CheckCore,
        check: CheckId,
        cause: CheckCause,
        held_back: u32,
    },
}

impl Event {
    /// How many variants there are; [`Event::ordinal`] numbers them.
    pub const COUNT: usize = 19;

    pub fn name(&self) -> &'static str {
        match self {
            Event::Installed => "installed",
            Event::CheckedIn(_) => "checked_in",
            Event::Updated { .. } => "updated",
            Event::UpdateRolledBack { .. } => "update_rolled_back",
            Event::DaemonCrashed { .. } => "daemon_crashed",
            Event::ClientOpened { .. } => "client_opened",
            Event::AgentCreated { .. } => "agent_created",
            Event::PromptSent { .. } => "prompt_sent",
            Event::AskAnswered { .. } => "ask_answered",
            Event::PairingStarted { .. } => "pairing_started",
            Event::PairingSucceeded { .. } => "pairing_succeeded",
            Event::PairingFailed { .. } => "pairing_failed",
            Event::SignedIn => "signed_in",
            Event::SignInFailed { .. } => "sign_in_failed",
            Event::SignedOut => "signed_out",
            Event::RelayRefused => "relay_refused",
            Event::PaywallViewed { .. } => "paywall_viewed",
            Event::PurchaseStarted { .. } => "purchase_started",
            Event::CheckFailed { .. } => "check_failed",
        }
    }

    /// A number per variant, so a test can tell that a list of events
    /// covers every one: adding a variant fails to compile here until it is
    /// numbered.
    pub fn ordinal(&self) -> usize {
        match self {
            Event::Installed => 0,
            Event::CheckedIn(_) => 1,
            Event::Updated { .. } => 2,
            Event::UpdateRolledBack { .. } => 3,
            Event::DaemonCrashed { .. } => 4,
            Event::ClientOpened { .. } => 5,
            Event::AgentCreated { .. } => 6,
            Event::PromptSent { .. } => 7,
            Event::AskAnswered { .. } => 8,
            Event::PairingStarted { .. } => 9,
            Event::PairingSucceeded { .. } => 10,
            Event::PairingFailed { .. } => 11,
            Event::SignedIn => 12,
            Event::SignInFailed { .. } => 13,
            Event::SignedOut => 14,
            Event::RelayRefused => 15,
            Event::PaywallViewed { .. } => 16,
            Event::PurchaseStarted { .. } => 17,
            Event::CheckFailed { .. } => 18,
        }
    }

    /// The event's properties, as the contract spells them.
    pub fn properties(&self) -> Map<String, Value> {
        let mut out = Map::new();
        let mut put = |key: &str, value: Value| {
            out.insert(key.to_owned(), value);
        };
        match self {
            Event::Installed | Event::SignedIn | Event::SignedOut | Event::RelayRefused => {}
            Event::CheckedIn(check_in) => {
                put("agents_running", check_in.agents_running.to_json());
                put("agents_created_24h", check_in.agents_created_24h.to_json());
                put("turns_24h", check_in.turns_24h.to_json());
                put("paired_hosts", json!(check_in.paired_hosts));
                put("hosts_by_route", check_in.hosts_by_route.to_json());
                if let Some(carrier) = check_in.relay_carrier {
                    put("relay_carrier", token(carrier));
                }
                put("signed_in", json!(check_in.signed_in));
                if let Some(tier) = check_in.tier {
                    put("tier", token(tier));
                }
            }
            Event::Updated { from, to } | Event::UpdateRolledBack { from, to } => {
                put("from_version", version(from));
                put("to_version", version(to));
            }
            Event::DaemonCrashed { version: crashed } => {
                if let Some(crashed) = crashed {
                    put("crashed_version", version(crashed));
                }
            }
            Event::ClientOpened { client } => put("surface", token(*client)),
            Event::AgentCreated { kind, on, by } => {
                put("kind", token(*kind));
                where_(&mut put, *on);
                put("by", token(*by));
            }
            Event::PromptSent { kind, on, queued } => {
                put("kind", token(*kind));
                where_(&mut put, *on);
                put("queued", json!(queued));
            }
            Event::AskAnswered { kind, on, ask } => {
                put("kind", token(*kind));
                where_(&mut put, *on);
                put("ask", token(*ask));
            }
            Event::PairingStarted { role, method } => {
                put("role", token(*role));
                put("method", token(*method));
            }
            Event::PairingSucceeded {
                role,
                method,
                remote_host,
            } => {
                put("role", token(*role));
                put("method", token(*method));
                put("remote_host", json!(remote_host.to_string()));
            }
            Event::PairingFailed {
                role,
                method,
                reason,
            } => {
                put("role", token(*role));
                put("method", token(*method));
                put("reason", token(*reason));
            }
            Event::SignInFailed { reason } => put("reason", token(*reason)),
            Event::PaywallViewed { from } => put("from", token(*from)),
            Event::PurchaseStarted { interval } => put("interval", token(*interval)),
            Event::CheckFailed {
                core,
                check,
                cause,
                held_back,
            } => {
                put("core", token(*core));
                put("check", json!(check.as_str()));
                put("cause", token(*cause));
                put("held_back", json!(held_back));
            }
        }
        out
    }

    /// One event of every variant, for the tests that hold the documentation
    /// and the wire shape to the type.
    pub fn examples() -> Vec<Event> {
        let host = HostId::from_u128(0x5a5a);
        let v = |text: &str| Version::parse(text).expect("an example version");
        let mut check_in = CheckIn {
            paired_hosts: 2,
            relay_carrier: Some(RelayCarrier::Quic),
            signed_in: true,
            tier: Some(Tier::Free),
            ..CheckIn::default()
        };
        check_in.agents_running.add(Kind::Codex, 1);
        check_in.agents_created_24h.add(Kind::ClaudePty, 2);
        check_in.turns_24h.add(Kind::ClaudePty, 7);
        check_in.hosts_by_route.add(Route::Relay, 1);
        check_in.hosts_by_route.add(Route::Direct, 1);
        vec![
            Event::Installed,
            Event::CheckedIn(check_in),
            Event::Updated {
                from: v("0.7.0"),
                to: v("0.8.0"),
            },
            Event::UpdateRolledBack {
                from: v("0.8.0"),
                to: v("0.7.0"),
            },
            Event::DaemonCrashed {
                version: Some(v("0.8.0")),
            },
            Event::ClientOpened {
                client: Client::Terminal,
            },
            Event::AgentCreated {
                kind: Kind::ClaudeSdk,
                on: On::ThisHost,
                by: By::Person,
            },
            Event::PromptSent {
                kind: Kind::Codex,
                on: On::PairedHost(host),
                queued: false,
            },
            Event::AskAnswered {
                kind: Kind::ClaudePty,
                on: On::PairedHost(host),
                ask: Ask::Permission,
            },
            Event::PairingStarted {
                role: Role::Joiner,
                method: Method::Qr,
            },
            Event::PairingSucceeded {
                role: Role::Joiner,
                method: Method::Qr,
                remote_host: host,
            },
            Event::PairingFailed {
                role: Role::Offerer,
                method: Method::Pin,
                reason: PairingFailure::WrongSecret,
            },
            Event::SignedIn,
            Event::SignInFailed {
                reason: SignInFailure::Rejected,
            },
            Event::SignedOut,
            Event::RelayRefused,
            Event::PaywallViewed {
                from: PaywallFrom::Hosts,
            },
            Event::PurchaseStarted {
                interval: Interval::Yearly,
            },
            Event::CheckFailed {
                core: CheckCore::Daemon,
                check: CheckId::new("daemon.generation_fell").expect("a check id"),
                cause: CheckCause::Invariant,
                held_back: 3,
            },
        ]
    }
}

fn token<T: Token>(value: T) -> Value {
    json!(value.as_str())
}

fn where_(put: &mut impl FnMut(&str, Value), on: On) {
    match on {
        On::ThisHost => put("on", json!("this_host")),
        On::PairedHost(host) => {
            put("on", json!("paired_host"));
            put("remote_host", json!(host.to_string()));
        }
    }
}

/// A version as a token: lowercase, with no build metadata, which the
/// contract's token alphabet has no `+` for.
fn version(version: &Version) -> Value {
    let mut bare = version.clone();
    bare.build = semver::BuildMetadata::EMPTY;
    json!(bare.to_string().to_lowercase())
}

/// An event with the moment it happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    pub event: Event,
    pub at: SystemTime,
}

impl Recorded {
    pub fn to_json(&self) -> Value {
        let at: chrono::DateTime<chrono::Utc> = self.at.into();
        json!({
            "event": self.event.name(),
            "timestamp": at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "properties": Value::Object(self.event.properties()),
        })
    }
}
