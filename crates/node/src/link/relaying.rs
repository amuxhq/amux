//! The driver of the forwarding core: the relay's and each daemon's.
//!
//! [`relay_core::Relay`] decides which pairs of links forward packets to
//! each other and what every link is told of its neighbors. Once the
//! profile's link socket is handed over, the core is stepped through the
//! [`Forwarder`], which carries the packets between the pairs it installs.
//! A relay shares one core across every account; a daemon has one of its
//! own, forwarding between its paired hosts.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Instant;

use core_ring::{DumpKind, DumpLimit};
use daemon_core::DumpReason;
use daemon_core::payload::ForwardRefusal;
use relay_core::{Input, Output, Relay};
use wire::pb;

use super::forwarder::{ForwardLinks, Forwarder, HoldLimits, LinkLimits};
use super::link_log::PathKind;
use super::socket::{ForwardSink, LinkSocket};
use crate::HostId;
use crate::routing::LinkOutputTx;
use crate::window::{self, Window};

/// Prefixes the reason the forwarding core asked for a dump.
pub const RELAY_CORE_DUMP: &str = "the relay core: ";

/// The forwarding core, and on a relay the forwarder its pairs drive.
pub(crate) struct Relaying {
    core: Arc<Mutex<Relay>>,
    /// Set on a relay, whose links carry forwarded packets and no streams.
    forwarding: OnceLock<Forwarding>,
    history: Mutex<History>,
    /// The clock the core is ticked by when nothing forwards here.
    epoch: Instant,
    /// Where a dump the core asks for is written; unset only in tests that
    /// never read one.
    reports: OnceLock<PathBuf>,
}

/// What a dump is made of: the core's recent inputs, and which dumps were
/// already reported this interval.
#[derive(Default)]
struct History {
    window: Window<Relay, Input>,
    dumps: Dumps,
}

/// The forwarder on a relay's link socket and the links it puts packets
/// on.
struct Forwarding {
    forwarder: Arc<Forwarder>,
    links: Arc<RelayLinks>,
}

/// The dumps the relay core asked for, reported once per kind in each
/// interval: on a busy relay one broken invariant or slow step comes again
/// with every stream.
#[derive(Debug, Default)]
struct Dumps(DumpLimit);

impl Dumps {
    /// The reason to report for `reason` asked for at `now_ms`, or `None`
    /// when it is held back.
    fn reason(&mut self, reason: &DumpReason, now_ms: u64) -> Option<String> {
        let kind = match reason {
            DumpReason::Violation(violation) => DumpKind::Violation(violation.invariant.into()),
            DumpReason::OverBudget { .. } => DumpKind::OverBudget,
        };
        let suppressed = self.0.admit(kind, now_ms)?;
        Some(core_ring::with_suppressed(
            format!("{RELAY_CORE_DUMP}{reason}"),
            suppressed,
        ))
    }
}

impl Relaying {
    pub(crate) fn new(own: HostId) -> Arc<Self> {
        Arc::new(Self {
            core: Arc::new(Mutex::new(Relay::new(daemon_core::HostId(
                own.into_bytes(),
            )))),
            forwarding: OnceLock::new(),
            history: Mutex::default(),
            epoch: Instant::now(),
            reports: OnceLock::new(),
        })
    }

    /// Writes the dumps the core asks for into `dir`: a relay's directory
    /// from its configuration, a daemon's its installation's reports.
    pub(crate) fn report_to(&self, dir: PathBuf) {
        let _ = self.reports.set(dir);
    }

    /// Forwards the packets hosts send `socket` to forward, between the
    /// pairs the core installs. From then on the core is stepped through
    /// the forwarder, and no stream is spliced.
    pub(crate) fn forward_on(self: &Arc<Self>, socket: Arc<LinkSocket>) {
        let links = Arc::new(RelayLinks {
            socket: Arc::downgrade(&socket),
            relaying: Arc::downgrade(self),
            admitted: Mutex::default(),
        });
        let forwarder = Arc::new(Forwarder::new(
            self.core.clone(),
            links.clone(),
            HoldLimits::DEFAULT,
            LinkLimits::DEFAULT,
        ));
        socket.forward_to(Arc::new(Arrivals {
            forwarder: forwarder.clone(),
            links: links.clone(),
        }));
        let _ = self.forwarding.set(Forwarding { forwarder, links });
    }

    /// The core this relays with, for a test that must make it ask for a
    /// dump.
    #[cfg(test)]
    fn with_relay(own: HostId, relay: Relay) -> Arc<Self> {
        let relaying = Self::new(own);
        *lock(&relaying.core) = relay;
        relaying
    }

    /// Steps each input in turn, through the forwarder where there is one,
    /// and reports the dumps the core asks for. Every other output is
    /// returned for the caller to act on.
    pub(crate) fn step(&self, inputs: impl IntoIterator<Item = Input>) -> Vec<Output> {
        let mut out = Vec::new();
        for input in inputs {
            {
                let core = lock(&self.core);
                lock(&self.history).window.record(&core, &input);
            }
            let gone = match input {
                Input::LinkDown { link } => Some(link),
                _ => None,
            };
            match self.forwarding.get() {
                // The forwarder has reported the dumps among these.
                Some(forwarding) => out.extend(
                    forwarding
                        .forwarder
                        .step(input)
                        .into_iter()
                        .filter(|output| !matches!(output, Output::Dump { .. })),
                ),
                None => {
                    let mut stepped = Vec::new();
                    lock(&self.core).step(input, &mut stepped);
                    for output in stepped {
                        match output {
                            Output::Dump { reason } => self.report(&reason),
                            output => out.push(output),
                        }
                    }
                }
            }
            if let Some(forwarding) = self.forwarding.get() {
                let displaced = out.iter().filter_map(|output| match output {
                    Output::Displace { link } => Some(*link),
                    _ => None,
                });
                for link in gone.into_iter().chain(displaced) {
                    forwarding.links.forget(link);
                }
            }
        }
        out
    }

    /// A link the core holds as up came from `stand_in` on the relay's
    /// socket: its packets are forwarded from now on, those held for it
    /// first, and refusals are written to `tx`.
    ///
    /// `through` says the link reaches a forwarder this host sends through:
    /// the relay, or a paired daemon. Its forwarded packets are this host's
    /// own from then on, carried on a path of that kind.
    pub(crate) fn admitted(
        &self,
        link: daemon_core::LinkId,
        tx: LinkOutputTx,
        stand_in: Option<SocketAddr>,
        through: Option<PathKind>,
    ) {
        let (Some(forwarding), Some(stand_in)) = (self.forwarding.get(), stand_in) else {
            return;
        };
        if let Some(kind) = through
            && let Some(socket) = forwarding.links.socket.upgrade()
        {
            socket.add_forwarding(link, stand_in, kind);
        }
        forwarding.links.admit(link, tx, stand_in);
        forwarding.forwarder.on_admitted(link, stand_in);
    }

    /// The connection from `stand_in` ended without a link coming up on
    /// it, or after its link went: what was held for it is dropped.
    pub(crate) fn not_admitted(&self, stand_in: SocketAddr) {
        if let Some(forwarding) = self.forwarding.get()
            && !forwarding.links.is_sender(stand_in)
        {
            forwarding.forwarder.on_admission_failed(stand_in);
        }
    }

    /// Writes one dump the core asked for, unless one like it was written
    /// this interval: the reason, the core's state when the window began,
    /// every input stepped since and the state now. The relay core sees
    /// ids, prefaces and tiers, never a token (tokens are checked before a
    /// link is admitted), so the file holds no secret.
    fn report(&self, reason: &DumpReason) {
        let now_ms = self.now_ms();
        let (reason, window) = {
            let mut history = lock(&self.history);
            let Some(reason) = history.dumps.reason(reason, now_ms) else {
                return;
            };
            (reason, history.window.clone())
        };
        let Some(dir) = self.reports.get() else {
            tracing::error!(%reason, "the relay core asked for a dump, with nowhere to write it");
            return;
        };
        let relay = lock(&self.core).clone();
        let text = window.render("the relay core", &reason, &relay);
        match window::write(dir, "relay", &text) {
            Ok(file) => tracing::error!(
                %reason,
                file = %file.display(),
                "the relay core asked for a dump"
            ),
            Err(error) => tracing::error!(
                %reason,
                %error,
                "the relay core asked for a dump, which could not be written"
            ),
        }
    }

    /// The hosts on `user`'s account holding a link here.
    pub(crate) fn neighbors(&self, user: relay_core::UserId) -> Vec<daemon_core::HostId> {
        lock(&self.core).neighbors(user)
    }

    fn now_ms(&self) -> u64 {
        match self.forwarding.get() {
            Some(forwarding) => forwarding.forwarder.now_ms(),
            None => u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The relay's links as the forwarder sends on them: each admitted link's
/// stand-in on the socket and its control writer.
struct RelayLinks {
    /// Weak: the socket holds these links as its forward sink, and a
    /// profile that stops must let its socket go.
    socket: Weak<LinkSocket>,
    relaying: Weak<Relaying>,
    admitted: Mutex<Admitted>,
}

#[derive(Default)]
struct Admitted {
    links: HashMap<daemon_core::LinkId, (LinkOutputTx, SocketAddr)>,
    senders: HashMap<SocketAddr, daemon_core::LinkId>,
}

impl RelayLinks {
    fn admit(&self, link: daemon_core::LinkId, tx: LinkOutputTx, stand_in: SocketAddr) {
        let mut admitted = lock(&self.admitted);
        admitted.links.insert(link, (tx, stand_in));
        admitted.senders.insert(stand_in, link);
    }

    fn forget(&self, link: daemon_core::LinkId) {
        if let Some(socket) = self.socket.upgrade() {
            socket.remove_forwarding(link);
        }
        let mut admitted = lock(&self.admitted);
        if let Some((_, stand_in)) = admitted.links.remove(&link)
            && admitted.senders.get(&stand_in) == Some(&link)
        {
            admitted.senders.remove(&stand_in);
        }
    }

    fn is_sender(&self, stand_in: SocketAddr) -> bool {
        lock(&self.admitted).senders.contains_key(&stand_in)
    }

    fn sender(&self, stand_in: SocketAddr) -> Option<daemon_core::LinkId> {
        lock(&self.admitted).senders.get(&stand_in).copied()
    }
}

impl ForwardLinks for RelayLinks {
    fn send(&self, link: daemon_core::LinkId, datagram: &[u8]) -> std::io::Result<()> {
        let stand_in = lock(&self.admitted)
            .links
            .get(&link)
            .map(|(_, stand_in)| *stand_in)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotConnected, "the link is gone")
            })?;
        self.socket
            .upgrade()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotConnected, "the socket is gone")
            })?
            .send_raw(stand_in, datagram)
    }

    fn refuse(&self, link: daemon_core::LinkId, dst: HostId, reason: ForwardRefusal) {
        let Some(tx) = lock(&self.admitted)
            .links
            .get(&link)
            .map(|(tx, _)| tx.clone())
        else {
            return;
        };
        // A link too far behind to take its refusal hears it again when it
        // next sends there.
        let _ = tx.try_send(pb::Message {
            body: Some(pb::message::Body::ForwardRefused(pb::ForwardRefused {
                dst: dst.as_bytes().to_vec(),
                reason: reason as i32,
            })),
        });
    }

    fn dump(&self, reason: DumpReason) {
        if let Some(relaying) = self.relaying.upgrade() {
            relaying.report(&reason);
        }
    }
}

/// The packets hosts send the relay's socket to forward: from an admitted
/// link's stand-in through the table, from anyone else into the hold.
struct Arrivals {
    forwarder: Arc<Forwarder>,
    links: Arc<RelayLinks>,
}

impl ForwardSink for Arrivals {
    fn arrived(&self, sender: SocketAddr, dst: HostId, payload: &[u8]) {
        match self.links.sender(sender) {
            Some(link) => self.forwarder.on_packet(link, dst, payload),
            None => self.forwarder.hold(sender, dst, payload),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dump the relay core asks for leaves a file in the reports
    /// directory: the reason, the core's state when the window began,
    /// every input stepped since and the state now. A relay whose budget is
    /// zero asks at its first step measured over it.
    #[test]
    fn a_dump_the_relay_core_asks_for_is_a_file_of_its_state_and_inputs() {
        let reports = tempfile::tempdir().unwrap();
        let own = HostId::new_v4();
        let relaying = Relaying::with_relay(
            own,
            Relay::with_budget(
                daemon_core::HostId(own.into_bytes()),
                daemon_core::StepBudget { micros: 0 },
            ),
        );
        relaying.report_to(reports.path().to_owned());
        let written = || {
            std::fs::read_dir(reports.path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>()
        };
        let mut now_ms = 0;
        // Any step takes longer than nothing, given a clock fine enough.
        while written().is_empty() {
            now_ms += 1;
            relaying.step([Input::Tick { now_ms }]);
        }
        let [file] = &written()[..] else {
            panic!("one file: {:?}", written());
        };
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("relay-") && name.ends_with(".txt"),
            "{name}"
        );
        let text = std::fs::read_to_string(file).unwrap();
        println!("{}\n{text}", file.display());
        let header = format!("the relay core, amux {}\n", crate::version());
        assert!(text.starts_with(&header), "{text}");
        assert!(
            text.contains(&format!("reason: {RELAY_CORE_DUMP}"))
                && text.contains("over its budget of 0 µs"),
            "{text}"
        );
        assert!(
            text.contains("state at the window's start:\nRelay {"),
            "{text}"
        );
        // Steps quicker than the clock's microsecond pass the budget, so
        // the window may have moved on from the first tick: it holds an
        // unbroken run of ticks ending with the one that asked.
        let held: usize = text
            .split_once("inputs since (")
            .and_then(|(_, rest)| rest.split_once("):"))
            .and_then(|(count, _)| count.parse().ok())
            .expect("the dump counts its inputs");
        let first = now_ms - held as u64 + 1;
        assert!(
            text.contains(&format!("\n1 Tick {{ now_ms: {first} }}\n")),
            "{text}"
        );
        assert!(
            text.contains(&format!("\n{held} Tick {{ now_ms: {now_ms} }}\n")),
            "{text}"
        );
        assert!(text.contains("state now:\nRelay {"), "{text}");
        assert!(text.contains(&format!("now_ms: {now_ms},")), "{text}");
    }

    #[test]
    fn a_violation_the_relay_core_repeats_is_reported_once_per_interval() {
        let broke = DumpReason::Violation(daemon_core::Violation {
            invariant: "a splice only joins two live links",
            detail: "link 9 is down".into(),
        });
        let mut dumps = Dumps::default();
        let first = dumps.reason(&broke, 1_000).unwrap();
        assert!(first.starts_with(RELAY_CORE_DUMP), "{first}");
        for at in [1_001, 2_000, 1_000 + DumpLimit::INTERVAL_MS - 1] {
            assert_eq!(dumps.reason(&broke, at), None);
        }
        let second = dumps
            .reason(&broke, 1_000 + DumpLimit::INTERVAL_MS)
            .unwrap();
        assert!(
            second.ends_with("(3 more like it were not dumped since the last)"),
            "{second}"
        );
    }
}
