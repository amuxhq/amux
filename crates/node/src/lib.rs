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
mod audit;
mod auth;
mod blobs;
mod connection;
mod daemon;
pub mod discovery;
mod dispatcher;
mod drafts;
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
mod profiles;
mod relay;
pub mod release;
mod replicas;
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
pub use daemon::{Daemon, StartError, StartOptions, start};
pub use daemon_core::names::word_pairs;
pub use driver::{
    AGENT_DUMP, AGENT_VIOLATION_DUMP, CORE_DUMP, CORE_VIOLATION_DUMP, Cores, InventorySubscription,
    Subscription,
};
pub use dump::{
    CLIENT_CHECKPOINT, CLIENT_EVENTS, CLIENT_SESSIONS, DAEMON_LOG, DUMP_LOG_BYTES, DUMP_ROWS,
    DUMP_SEGMENTS, DumpError, MANIFEST, pack,
};
pub use edge::{
    ACCOUNT_FILE, CloudLinkServer, CloudOptions, DiscoveryFactory, Edge, EdgeError, EdgeOptions,
    FREE_TIER_REFRESH_INTERVAL, JwtCloudLinkAuthenticator, LAN_PORT_FILE, LINK_SOCKET, LanOptions,
    LoopbackLink, Observed, RelayCarrier, RelayIdentity, RelayQuic, UDP_BLOCKED_MEMORY,
    mdns_discovery,
};
pub use forward::{ForwardError, HostNameError, Owner};
pub use front_door::FrontDoor;
pub use generation::{Generation, boot_id};
pub use grpc::{ClientApi, status};
pub use identity::{quic_endpoint, quic_endpoint_config};
pub use install::{
    AGENTS, CORE_CHECKPOINT, CORE_RING, GENERATION, HOST_ID, INSTALLATION_LOCK, InstallationLock,
    LockError, PROFILES, REGISTRY, REPLICAS, REPORTS, STORE, write_durably,
};
pub use link::{PeerChannel, PeerClient, TRUST_REVOKED, socket};
pub use offers::{CatalogueError, HostCatalogue, PROVIDERS, Provider};
pub use outbox::{DrainReport, HttpSender, NoopSender, Push, PushError, PushFuture, PushSender};
pub use pairing::PairingAdmin;
pub use pairing::qr::{
    PAIR_LINK, QrPairingError, QrPairingPayload, encode_qr_pairing_invitation, pair_link,
    parse_pair_link, parse_qr_pairing_payload,
};
pub use pairing::ssh::{
    SshPairingError, SshPairingPeer, SshPairingProfile, SshTarget, pair_via_ssh_target,
};
#[cfg(unix)]
pub use pairing::ssh::{pair_via_ssh_responder_stdio, relay_stdio_to_unix_socket};
pub use profiles::{
    PROFILE_SOCKET, ProfileEntry, ProfileId, Registry, create_profile, host_id, profile_dir,
};
pub use relay::RelayError;
pub use replicas::{InventoryHook, SourceHook, SourcePolicy, SourceVerdict};
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
    pub use crate::identity::{device_key_path, host_id_path};
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
/// This binary's version, as stamped: the one it writes into every spec.
pub use release::version;
pub use transport::{TransportError, create_tls_acceptor, relay_quic_server_config};
pub use wire::PROTOCOL_VERSION;
