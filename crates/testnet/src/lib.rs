//! The many-daemons harness.
//!
//! A [`Topology`] declares hosts, the links between them and the agents
//! each runs; [`Net`] starts it as production daemons inside this process,
//! each on its own temporary installation with its own identity, trust
//! store, profile store and front door, running real `amux agent`
//! processes on the scripted fake providers. Policy timers on every daemon
//! run on one [`DrivenClock`] the specification advances; transports stay
//! on real time.
//!
//! The harness owns resources, verbs and observations, never scenarios:
//!
//! - **Verbs** change the world and return an [`Ack`] once the change is
//!   installed: sever and restore a link, kill, stop and restart a daemon,
//!   checkpoint a host and rewind it to that checkpoint under a new boot
//!   id, spawn and resume agents, advance policy time.
//! - **Observations** wait for consequences and return what they saw:
//!   [`Observer`] and [`InventoryObserver`] record Subscribe and
//!   SubscribeInventory streams exactly as a client receives them, and
//!   [`ObserverOf::observe_until`] fails on a stuck predicate, at its
//!   deadline or as soon as the stream ends, never passing by timing out.
//!   [`Net::assert_block_invariant`] checks a replica's rows against the
//!   origin's.
//! - **The door** ([`door::serve`]) serves a topology to a real client in
//!   another process with the same verbs, and publishes readiness only once
//!   the net is ready.
//!
//! Scenarios live in the tests that use these pieces.
//!
//! <!-- door-capabilities:start -->
//! | Door verb (`door::Control`) | Harness capability |
//! | --- | --- |
//! | `Sever` | `Net::sever_link` |
//! | `Restore` | `Net::restore_link` |
//! | `Link` | `Net::link_up` |
//! | `Trust` | `Net::trust` |
//! | `Untrust` | `Net::untrust` |
//! | `RefuseCalls` | `Net::refuse_calls` |
//! | `SetTier` | `Net::set_tier` |
//! | `SignIn` | `Net::sign_in` |
//! | `KillDaemon` | `Net::kill_daemon` |
//! | `StopDaemon` | `Net::stop_daemon` |
//! | `RestartDaemon` | `Net::kill_daemon when running + Net::restart_daemon` |
//! | `Checkpoint` | `Net::checkpoint_host` |
//! | `Rewind` | `Net::rewind_host` |
//! | `Advance` | `Net::advance` |
//! | `Spawn` | `Net::spawn` |
//! | `Resume` | `Net::resume` |
//! | `Send` | `Net::send` |
//! | `Stop` | `Net::stop` |
//! | `Freeze` | `Net::freeze` |
//! | `Thaw` | `Net::thaw` |
//! | `OpenGate` | `Net::open_gate` |
//! | `LanGate` | `Net::lan_gate` |
//! | `LanFaults` | `Net::set_lan_faults` |
//! | `RelayGate` | `Net::relay_gate` |
//! | `RelayFaults` | `Net::set_relay_faults` |
//! | `NetFaults` | `Net::set_net_faults` |
//! | `BlockUdp` | `Net::block_udp` |
//! | `Flap` | `Net::sever_link + Net::restore_link on a period` |
//! | `FlapStop` | `Net::restore_link for each flapping link` |
//! | `PairGate` | `Net::pair_gate` |
//! | `AddHost` | `Net::add_host` |
//! | `RemoveHost` | `Net::remove_host` |
//! | `AddLink` | `Net::add_link` |
//! | `RemoveLink` | `Net::remove_link` |
//! | `AddAccount` | `Net::add_account` |
//! | `Settle` | `Net::input_settled when an input is named + Net::settle_within` |
//! | `Release` | `Net::publish_release` |
//! | `Status` | `Net::topology + Net::host` |
//! | `Inventory` | `Net::observe_inventory + observe_until(CaughtUp)` |
//! | `Block` | `Net::assert_block_invariant` |
//! | `Chat` | `Net::observe or Net::observe_id + observe_until(CaughtUp)` |
//! | `ProviderInput` | `Net::provider_input` |
//! | `Shutdown` | `Net::shutdown + closes the control socket` |
//! <!-- door-capabilities:end -->

mod binaries;
mod bonjour;
mod clock;
pub mod ctl;
pub mod door;
mod feed;
mod gate;
mod invariant;
mod lan;
mod net;
pub mod observe;
mod process;
mod providers;
mod relay;
mod topology;

pub use binaries::{Binaries, PREBUILT_ENV};
pub use clock::DrivenClock;
pub use gate::{Faults, UdpGate};
pub use invariant::BlockViolation;
pub use net::{
    Ack, ClockMode, EdgeHook, HostInfo, JournalCut, LaunchHook, Net, NetAgent, NetError,
    NetOptions, REAL_PATIENCE, SUPERVISED_VERSION, local_channel, prompt, withdraw,
};
pub use observe::{
    InventoryObserver, Observer, ObserverOf, PATIENCE, Stuck, holds_for, until, until_within,
};
pub use relay::{CREDENTIAL_TTL, RELAY_HOST, Relay};
pub use topology::{
    AccountDecl, AgentDecl, FakeKind, HostDecl, LinkDecl, Provider, RelayDecl, TierDecl, Topology,
    TopologyError,
};
