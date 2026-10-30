//! The amux daemon.
//!
//! One daemon process is an installation: it holds the installation lock,
//! the profile registry and the generation file, and hosts one
//! [`ProfileRuntime`] per profile. A profile runtime owns its store and its
//! agents: every agent runs as its own process (`amux agent <dir>`) in a
//! directory under the profile, writes what happened to its journal there,
//! and the runtime ingests that journal into the store.
//!
//! [`start`] is the one startup path; [`Daemon::shutdown`] the clean
//! shutdown that marks the installation clean.

mod activation;
mod auth;
mod blobs;
mod connection;
mod daemon;
pub mod discovery;
mod dispatcher;
mod driver;
mod dump;
mod edge;
mod forward;
mod front_door;
mod generation;
mod grpc;
mod hosts;
mod identity;
mod install;
mod link;
mod names;
mod net_error;
mod offers;
mod outbox;
mod pairing;
mod profile_admin;
mod profiles;
mod registry;
mod relay;
pub mod release;
mod replicas;
mod reports;
mod repositories;
mod resource_limits;
mod retention;
mod routing;
mod runtime;
mod serve;
mod services;
mod spec;
#[cfg(not(target_os = "ios"))]
pub mod supervisor;
mod telemetry;
mod transport;
mod trust;
mod window;
mod worktree;

pub use activation::{ActivationError, ActivationPipe, GO, InheritedPipe, PIPE_ENV, PREPARED};
pub use blobs::{BlobError, PATCH_MIME};
pub use daemon::{Daemon, STOP_TARGET, StartError, StartOptions, start};
pub use daemon_core::names::word_pairs;
pub use driver::{
    AGENT_DUMP, AGENT_VIOLATION_DUMP, CORE_DUMP, CORE_VIOLATION_DUMP, Cores, InventorySubscription,
    Subscription,
};
pub use dump::{
    CLIENT_CHECKPOINT, CLIENT_EVENTS, CLIENT_SESSIONS, DAEMON_LOG, DUMP_LOG_BYTES, DUMP_ROWS,
    DUMP_SEGMENTS, DumpError, MANIFEST, SUPERVISOR_LOG, pack,
};
pub use edge::{
    ACCOUNT_FILE, Cloud, CloudLinkServer, CloudOptions, DUAL_STACK_ANY, DiscoveryFactory, Edge,
    EdgeError, EdgeOptions, FREE_TIER_REFRESH_INTERVAL, JwtCloudLinkAuthenticator, LAN_PORT_FILE,
    LINK_SOCKET, LanOptions, LoopbackLink, NetworkChange, RELAY_TOKEN_FILE, Relay, RelayCarrier,
    RelayIdentity, RelayQuic, RelayServing, UDP_BLOCKED_MEMORY, interface_addrs, mdns_discovery,
    test_interfaces,
};
pub use forward::{ForwardError, HostNameError, Owner};
pub use front_door::FrontDoor;
pub use generation::{Generation, boot_id};
pub use grpc::{ClientApi, status};
pub use identity::{LinkLiveness, quic_endpoint, quic_endpoint_config};
pub use install::{
    AGENTS, CORE_RING, GENERATION, INSTALLATION_LOCK, InstallationLock, LockError, PROFILES,
    REPLICAS, REPORTS, STORE, write_durably,
};
pub use link::{
    Cursor, EarlyAnswer, EarlyCall, EarlyReply, PeerChannel, PeerClient, ResumedLink,
    TRUST_REVOKED, forwarder, framed, link_log, paths, socket,
};
pub use offers::{CatalogueError, HostCatalogue, PROVIDERS, Provider};
pub use outbox::{DrainReport, HttpSender, NoopSender, Push, PushError, PushFuture, PushSender};
pub use pairing::qr::{
    PAIR_LINK, QrPairingError, QrPairingPayload, encode_qr_pairing_invitation, pair_link,
    parse_pair_link, parse_qr_pairing_payload,
};
#[cfg(unix)]
pub use pairing::ssh::relay_stdio_to_unix_socket;
pub use pairing::ssh::{
    SshPairingError, exchange_over_ssh, read_ssh_identity, ssh_stdio, write_ssh_identity,
};
pub use profile_admin::ProfileAdmin;
pub use profiles::{
    PROFILE_SOCKET, ProfileEntry, ProfileId, create_profile, create_profile_as, profile_dir,
};
pub use registry::{
    AccountState, FellBehind, ProfileEvent, ProfileRefusal, ProfileState, ProfileSubscription,
    Registry, SignIn,
};
pub use relay::RelayError;
pub use replicas::{InventoryHook, SourceHook, SourcePolicy, SourceVerdict};
pub use reports::{
    AutoReports, CLIENT_FILES, DUMP_ARCHIVE, NotSent, REPORT_DUMP, REPORT_JSON, REPORT_SCHEMA,
    ReportError, archive,
};
pub use retention::Retention;
pub use routing::local_kinds;
pub use runtime::{
    AGENT_LOG, AgentId, DUMPS_FINISH_WITHIN, FANOUT_CAPACITY, INGEST_BATCH, JoinHook,
    KEPT_SEGMENTS, Launch, Profile, ProfileRuntime, RegistryError, Stopped, SweepReport, TAIL_ROWS,
    locked, to_wire,
};
pub use serve::{MAX_PAGE, ServeError};
pub use telemetry::Telemetry;
pub use worktree::{GitWorktrees, MakeWorktree, WORKTREES};

/// Internals the testnet harness assembles topologies from. Hidden and
/// unstable: reachable only for test infrastructure, never for products.
#[doc(hidden)]
pub mod harness {
    pub use agent_dir::SystemClock;

    pub use crate::auth::claims::Tier;
    pub use crate::discovery::{Advertisement, Discovery, DiscoveryEvent, ScriptedDiscovery};
    pub use crate::identity::device_key_path;
    pub use crate::link::ByteStream;
    pub use crate::link::relay_host::RelayHost;
    pub use crate::routing::{AuthenticatedLinkUser, LinkTokenAuthenticator, LiveRoute};
    pub use crate::transport::{
        relay_quic_client_config_with_roots, relay_quic_server_config_from_der,
    };

    /// A discovery factory whose instances share one scripted bus.
    pub fn scripted_discovery(bus: &ScriptedDiscovery) -> crate::edge::DiscoveryFactory {
        let bus = bus.clone();
        std::sync::Arc::new(move || {
            Ok(std::sync::Arc::new(bus.clone()) as std::sync::Arc<dyn Discovery>)
        })
    }
}

/// A host's id: one per profile, the identity its peers pin.
pub type HostId = uuid::Uuid;

pub use agent_dir::Clock;
pub use auth::claims::Tier;
pub use auth::oauth::{OAuthError, run_device_flow};
pub use auth::{AccessToken, AuthError};
pub use link::client_tickets::CLIENT_TICKETS_DIR;
pub use link::resumption::{RELAY_TICKETS, SessionTickets, TICKETS_DIR};
pub use link::validation::CLIENT_TOKENS_DIR;
/// This binary's version, as stamped: the one it writes into every spec.
pub use release::version;
pub use transport::{TransportError, create_tls_acceptor, relay_quic_server_config};
pub use wire::PROTOCOL_VERSION;
