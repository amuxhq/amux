//! Committed protobuf messages for every amux protocol boundary: the session
//! records the journal frames and the store keeps, the agent process's spec
//! and control frames, and the client, peer, pairing, profile and
//! installation services.

/// Protocol version for the native-stream link handshake. It moves only for
/// a break both ends of a link must take; see `proto/amux/v1/README.md` at the repository root.
pub const PROTOCOL_VERSION: u32 = 1;

pub mod amux {
    pub mod v1 {
        #![allow(dead_code, clippy::enum_variant_names, clippy::large_enum_variant)]
        include!("generated/amux.v1.rs");
    }

    /// What a client writes into a dump, read back only by the build that
    /// wrote it; outside the protocol's compatibility promises.
    pub mod dump {
        #![allow(dead_code, clippy::enum_variant_names, clippy::large_enum_variant)]
        include!("generated/amux.dump.rs");
    }
}

pub use amux::dump;

/// gRPC's richer error model, from the vendored googleapis protos.
pub mod google {
    pub mod rpc {
        #![allow(dead_code, clippy::enum_variant_names, clippy::doc_lazy_continuation)]
        include!("generated/google.rpc.rs");
    }
}

pub use google::rpc;

mod error;
pub use amux::v1::*;
pub use error::DOMAIN as ERROR_DOMAIN;
/// An error's code, gRPC's own.
pub use rpc::Code as ErrorCode;
/// amux's error, a `google.rpc.Status`: named apart so it never reads as
/// tonic's `Status`, the transport's carrier for it.
pub use rpc::Status as Error;

/// Bound for link-control messages and application-stream prefaces.
pub const MESSAGE_SIZE_LIMIT: usize = 16 * 1024 * 1024;
/// The largest attachment a person may send: 64 MiB of the file's own
/// bytes. Clients word it "64 MB".
pub const ATTACHMENT_SIZE_LIMIT: usize = 64 * 1024 * 1024;
/// RPC payloads: the largest attachment plus room for the protobuf framing
/// around it (a `PutBlobRequest`'s agent id, name and mime type), so a file
/// at the attachment limit still fits in one message.
pub const CHANNEL_MESSAGE_SIZE_LIMIT: usize = ATTACHMENT_SIZE_LIMIT + 1024 * 1024;

/// A sized client and server constructor for each service that carries
/// attachments: every message may be as large as the largest attachment.
macro_rules! sized_service {
    ($client_fn:ident, $server_fn:ident, $client_mod:ident, $client:ident, $server_mod:ident, $server:ident, $service:ident) => {
        #[cfg(feature = "grpc")]
        pub fn $client_fn(
            channel: tonic::transport::Channel,
        ) -> $client_mod::$client<tonic::transport::Channel> {
            $client_mod::$client::new(channel)
                .max_decoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
                .max_encoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
        }

        #[cfg(feature = "grpc")]
        pub fn $server_fn<T>(service: T) -> $server_mod::$server<T>
        where
            T: $server_mod::$service,
        {
            $server_mod::$server::new(service)
                .max_decoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
                .max_encoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
        }
    };
}

sized_service!(
    agent_service_client,
    agent_service_server,
    agent_service_client,
    AgentServiceClient,
    agent_service_server,
    AgentServiceServer,
    AgentService
);
sized_service!(
    session_service_client,
    session_service_server,
    session_service_client,
    SessionServiceClient,
    session_service_server,
    SessionServiceServer,
    SessionService
);
sized_service!(
    peer_agent_service_client,
    peer_agent_service_server,
    peer_agent_service_client,
    PeerAgentServiceClient,
    peer_agent_service_server,
    PeerAgentServiceServer,
    PeerAgentService
);
sized_service!(
    peer_session_service_client,
    peer_session_service_server,
    peer_session_service_client,
    PeerSessionServiceClient,
    peer_session_service_server,
    PeerSessionServiceServer,
    PeerSessionService
);

/// The client services' stubs on one profile or tools socket: what a
/// caller that speaks gRPC directly holds.
#[cfg(feature = "grpc")]
#[derive(Clone, Debug)]
pub struct ClientStubs {
    pub agents: agent_service_client::AgentServiceClient<tonic::transport::Channel>,
    pub sessions: session_service_client::SessionServiceClient<tonic::transport::Channel>,
}

#[cfg(feature = "grpc")]
pub fn client_stubs(channel: tonic::transport::Channel) -> ClientStubs {
    ClientStubs {
        agents: agent_service_client(channel.clone()),
        sessions: session_service_client(channel),
    }
}

/// A person's input as its kind's own message: what a client builds and
/// what the agent of that kind decodes from [`Input::body`].
#[derive(Clone, Debug, PartialEq)]
pub enum KindInput {
    ClaudePty(ClaudePtyInput),
    ClaudeSdk(ClaudeSdkInput),
    Codex(CodexInput),
}

impl KindInput {
    pub fn kind(&self) -> Kind {
        match self {
            Self::ClaudePty(_) => Kind::ClaudePty,
            Self::ClaudeSdk(_) => Kind::ClaudeSdk,
            Self::Codex(_) => Kind::Codex,
        }
    }

    fn encode(&self) -> Vec<u8> {
        use prost::Message as _;
        match self {
            Self::ClaudePty(input) => input.encode_to_vec(),
            Self::ClaudeSdk(input) => input.encode_to_vec(),
            Self::Codex(input) => input.encode_to_vec(),
        }
    }
}

impl From<ClaudePtyInput> for KindInput {
    fn from(input: ClaudePtyInput) -> Self {
        Self::ClaudePty(input)
    }
}

impl From<ClaudeSdkInput> for KindInput {
    fn from(input: ClaudeSdkInput) -> Self {
        Self::ClaudeSdk(input)
    }
}

impl From<CodexInput> for KindInput {
    fn from(input: CodexInput) -> Self {
        Self::Codex(input)
    }
}

impl Input {
    /// `input` encoded under its kind, with the client's input id.
    pub fn new(input_id: Vec<u8>, input: impl Into<KindInput>) -> Self {
        let input = input.into();
        Self {
            input_id,
            kind: input.kind() as i32,
            body: input.encode(),
        }
    }

    /// The body read as the kind it names. None for a kind this build does
    /// not know or a body that does not decode; an arm this build does not
    /// know decodes as a message with no arm set.
    pub fn decode_body(&self) -> Option<KindInput> {
        use prost::Message as _;
        let body = self.body.as_slice();
        match Kind::try_from(self.kind).ok()? {
            Kind::ClaudePty => ClaudePtyInput::decode(body).ok().map(KindInput::ClaudePty),
            Kind::ClaudeSdk => ClaudeSdkInput::decode(body).ok().map(KindInput::ClaudeSdk),
            Kind::Codex => CodexInput::decode(body).ok().map(KindInput::Codex),
            Kind::Unspecified => None,
        }
    }
}

impl create_agent_request::Agent {
    /// The kind with its default settings; None for no kind.
    pub fn defaults(kind: Kind) -> Option<Self> {
        match kind {
            Kind::ClaudePty => Some(Self::ClaudePty(ClaudeCreateConfig::default())),
            Kind::ClaudeSdk => Some(Self::ClaudeSdk(ClaudeCreateConfig::default())),
            Kind::Codex => Some(Self::Codex(CodexCreateConfig::default())),
            Kind::Unspecified => None,
        }
    }

    /// The interpreter this arm creates.
    pub fn kind(&self) -> Kind {
        match self {
            Self::ClaudePty(_) => Kind::ClaudePty,
            Self::ClaudeSdk(_) => Kind::ClaudeSdk,
            Self::Codex(_) => Kind::Codex,
        }
    }
}

impl Profile {
    /// The profile's standing with its cloud account.
    pub fn standing(&self) -> Option<&cloud_standing::Standing> {
        self.cloud.as_ref()?.standing.as_ref()
    }

    /// The profile's relay link, while it is signed in and connected.
    pub fn relay_connected(&self) -> Option<&RelayConnected> {
        match self.standing()? {
            cloud_standing::Standing::SignedIn(RelayLink {
                state: Some(relay_link::State::Connected(connected)),
                ..
            }) => Some(connected),
            _ => None,
        }
    }
}

impl HostRef {
    /// A host by its id.
    pub fn id(host_id: impl Into<Vec<u8>>) -> Self {
        Self {
            of: Some(host_ref::Of::HostId(host_id.into())),
        }
    }

    /// A host as a person names it.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            of: Some(host_ref::Of::Name(name.into())),
        }
    }
}

impl HostEntry {
    /// A trusted host as `reach` says it reads.
    pub fn trusted_host(
        host_id: impl Into<Vec<u8>>,
        name: impl Into<String>,
        reach: trusted::Reach,
    ) -> Self {
        Self {
            host: Some(Host {
                host_id: host_id.into(),
                name: name.into(),
                ..Host::default()
            }),
            standing: Some(host_entry::Standing::Trusted(Trusted {
                reach: Some(reach),
                current: false,
            })),
            ..Self::default()
        }
    }

    /// A host discovery found at `addrs`, never paired.
    pub fn candidate_host(
        host_id: impl Into<Vec<u8>>,
        name: impl Into<String>,
        addrs: Vec<String>,
    ) -> Self {
        Self {
            host: Some(Host {
                host_id: host_id.into(),
                name: name.into(),
                ..Host::default()
            }),
            standing: Some(host_entry::Standing::Candidate(Candidate {
                addrs,
                announced_by: Vec::new(),
            })),
            ..Self::default()
        }
    }

    /// The host's id, as its handshake gave it.
    pub fn host_id(&self) -> &[u8] {
        self.host.as_ref().map_or(&[], |host| &host.host_id)
    }

    pub fn name(&self) -> &str {
        self.host.as_ref().map_or("", |host| &host.name)
    }

    /// Paired with this machine, or this machine itself.
    pub fn trusted(&self) -> Option<&Trusted> {
        match &self.standing {
            Some(host_entry::Standing::Trusted(trusted)) => Some(trusted),
            _ => None,
        }
    }

    /// Never paired: found by discovery, or announced by a neighbor.
    pub fn candidate(&self) -> Option<&Candidate> {
        match &self.standing {
            Some(host_entry::Standing::Candidate(candidate)) => Some(candidate),
            _ => None,
        }
    }

    /// How a trusted host reads; none for a candidate.
    pub fn reach(&self) -> Option<&trusted::Reach> {
        self.trusted()?.reach.as_ref()
    }

    /// The kind of route a trusted host is online over.
    pub fn online(&self) -> Option<RouteKind> {
        match self.reach()? {
            trusted::Reach::Online(kind) => Some(RouteKind::try_from(*kind).unwrap_or_default()),
            _ => None,
        }
    }

    pub fn away(&self) -> bool {
        matches!(self.reach(), Some(trusted::Reach::Away(_)))
    }

    /// Why a trusted host reads offline.
    pub fn offline(&self) -> Option<&Offline> {
        match self.reach()? {
            trusted::Reach::Offline(offline) => Some(offline),
            _ => None,
        }
    }

    /// Whether a trusted host said it no longer trusts this machine.
    pub fn revoked(&self) -> bool {
        self.offline().is_some_and(|offline| offline.revoked)
    }

    /// Whether this machine's copy of a trusted host's agents has caught up.
    pub fn current(&self) -> bool {
        self.trusted().is_some_and(|trusted| trusted.current)
    }

    /// The build the host runs, once it has said.
    pub fn version(&self) -> Option<&str> {
        Some(self.host.as_ref()?.version.as_str()).filter(|version| !version.is_empty())
    }

    pub fn platform(&self) -> Platform {
        self.host
            .as_ref()
            .map_or(Platform::Unspecified, Host::platform)
    }

    /// Whether the host is signed in to its account, as it said.
    pub fn account_signed_in(&self) -> Option<bool> {
        self.host.as_ref()?.account_signed_in
    }
}

impl Candidate {
    /// Discovery found it on this network, where pairing dials it on any
    /// plan.
    pub fn nearby(&self) -> bool {
        !self.addrs.is_empty()
    }

    /// The paired hosts announcing it, any of which carries a pairing on any
    /// plan.
    pub fn through_paired(&self) -> impl Iterator<Item = &Host> {
        self.announced_by
            .iter()
            .filter(|host| !host.is_cloud_relay())
    }

    /// Only the cloud relay reaches it, which carries a pairing only for a
    /// paid account.
    pub fn relay_only(&self) -> bool {
        !self.nearby()
            && !self.announced_by.is_empty()
            && self.announced_by.iter().all(Host::is_cloud_relay)
    }
}

impl Host {
    /// Whether the host serves as a cloud relay.
    pub fn is_cloud_relay(&self) -> bool {
        self.capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.cloud_relay)
    }
}

/// The longest name an agent can have.
pub const AGENT_NAME_MOST: usize = 64;

/// The tool server amux's own tools are registered under, with every
/// provider; the interpreters and the clients' views both recognise its
/// calls by it.
pub const AMUX_TOOL_SERVER: &str = "amux";
/// amux's tool that sets the caller's working_on.
pub const STATUS_TOOL: &str = "status";
/// amux's tool that messages another agent.
pub const SEND_TOOL: &str = "send";
/// amux's tool that attaches a file to the caller's reply.
pub const ATTACH_TOOL: &str = "attach";
/// amux's tool that starts a child agent.
pub const SPAWN_TOOL: &str = "spawn";
/// amux's tool that stops one of the caller's children.
pub const STOP_TOOL: &str = "stop";
/// amux's tools that list the fleet and the trusted hosts.
pub const AGENTS_TOOL: &str = "agents";
pub const HOSTS_TOOL: &str = "hosts";

/// Why a name cannot name an agent. Each client words it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub enum AgentNameProblem {
    Empty,
    /// Longer than [`AGENT_NAME_MOST`].
    TooLong,
    /// Not lowercase letters, digits and hyphens starting with a letter or
    /// digit.
    Characters,
}

/// Why `name` cannot name an agent, or None when it can. A name is
/// lowercase letters, digits and hyphens, starting with a letter or digit,
/// so the same word serves as the agent's branch, its worktree's folder and
/// its handle on a command line, with nothing to translate.
pub fn agent_name_problem(name: &str) -> Option<AgentNameProblem> {
    if name.is_empty() {
        return Some(AgentNameProblem::Empty);
    }
    if name.len() > AGENT_NAME_MOST {
        return Some(AgentNameProblem::TooLong);
    }
    let fits = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    if !name.starts_with(fits) || !name.chars().all(|c| fits(c) || c == '-') {
        return Some(AgentNameProblem::Characters);
    }
    None
}

pub const DESCRIPTOR_SET: &[u8] = include_bytes!("generated/amux.v1.bin");

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use prost::Message as _;
    use prost_types::{DescriptorProto, FileDescriptorSet};

    use super::*;

    fn descriptor() -> FileDescriptorSet {
        FileDescriptorSet::decode(DESCRIPTOR_SET).expect("descriptor set should decode")
    }

    fn methods(set: &FileDescriptorSet, service: &str) -> BTreeSet<String> {
        set.file
            .iter()
            .flat_map(|file| file.service.iter())
            .filter(|candidate| candidate.name() == service)
            .flat_map(|candidate| candidate.method.iter())
            .map(|method| method.name().to_owned())
            .collect()
    }

    fn set_of(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn services_are_the_agent_session_peer_pairing_profile_admin_and_installation_surfaces() {
        let set = descriptor();
        let services = set
            .file
            .iter()
            .flat_map(|file| file.service.iter())
            .map(|service| service.name().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            services,
            set_of(&[
                "AgentService",
                "InstallationService",
                "PairingService",
                "PeerAgentService",
                "PeerSessionService",
                "ProfileAdminService",
                "SessionService",
            ])
        );

        let agents = methods(&set, "AgentService");
        assert_eq!(
            agents,
            set_of(&[
                "SubscribeInventory",
                "ResolveAgent",
                "CreateAgent",
                "RenameAgent",
                "StopAgent",
                "ResumeAgent",
                "DeleteAgent",
                "SendMessage",
                "PutBlob",
                "GetBlob",
                "GetDiff",
                "GetCatalogue",
                "ListRepositories",
            ])
        );
        let sessions = methods(&set, "SessionService");
        assert_eq!(
            sessions,
            set_of(&[
                "SubscribeSession",
                "ListItems",
                "GetItem",
                "SendInput",
                "UpdateDraft",
                "GetDraft",
            ])
        );

        // The peer surface is the client surface minus name resolution and
        // drafts (a draft stays on the machine it was typed on), over the
        // same request and response messages. No host asks another for its
        // side of a dump.
        let mut peer_agents = agents.clone();
        peer_agents.remove("ResolveAgent");
        assert_eq!(methods(&set, "PeerAgentService"), peer_agents);
        let mut peer_sessions = sessions.clone();
        peer_sessions.remove("UpdateDraft");
        peer_sessions.remove("GetDraft");
        assert_eq!(methods(&set, "PeerSessionService"), peer_sessions);

        let admin = methods(&set, "ProfileAdminService");
        assert!(admin.contains("Dump") && admin.contains("SubmitReport"));
        assert_eq!(
            methods(&set, "InstallationService"),
            set_of(&[
                "GetInstallation",
                "Shutdown",
                "ListProfiles",
                "SubscribeProfiles",
                "CreateProfile",
                "BindProfile",
                "RenameProfile",
                "DeleteProfile",
            ])
        );
    }

    /// Every top-level enum, with its values named under it.
    fn enums(set: &FileDescriptorSet) -> BTreeMap<String, Vec<(String, i32)>> {
        set.file
            .iter()
            .filter(|file| file.package.as_deref() == Some("amux.v1"))
            .flat_map(|file| file.enum_type.iter())
            .map(|enumeration| {
                let values = enumeration
                    .value
                    .iter()
                    .map(|value| (value.name().to_owned(), value.number()))
                    .collect();
                (enumeration.name().to_owned(), values)
            })
            .collect()
    }

    /// Every enum's zero says nothing is known, so an absent field, and a
    /// value from a newer build, read as that rather than as a meaning.
    #[test]
    fn every_enum_zero_is_unspecified_or_unknown() {
        for (enumeration, values) in enums(&descriptor()) {
            let zero = values
                .iter()
                .find(|(_, number)| *number == 0)
                .map(|(name, _)| name.as_str());
            assert!(
                zero.is_some_and(
                    |name| name.ends_with("_UNSPECIFIED") || name.ends_with("_UNKNOWN")
                ),
                "{enumeration}'s zero is {zero:?}"
            );
        }
    }

    /// The types code encodes on its own rather than inside another
    /// message or a call: frames on an agent's sockets, the journal's
    /// steps, the spec an agent starts from, the link's framing, error
    /// details, and each provider's item, input
    /// and snapshot bodies, which travel as bytes.
    const ENCODED_ON_THEIR_OWN: &[&str] = &[
        "AgentSpec",
        "CtlFrame",
        "PtyFrame",
        "ProviderOffer",
        "Step",
        "Message",
        "ProtocolVersionMismatch",
        // Named, not encoded: an ErrorInfo's reason is a value's name.
        "ErrorReason",
        "AmbiguousAgentName",
        "AmbiguousHostName",
        "ClaudePtyItem",
        "ClaudePtyInput",
        "ClaudePtySnapshot",
        "ClaudeSdkItem",
        "ClaudeSdkInput",
        "ClaudeSdkSnapshot",
        "CodexItem",
        "CodexInput",
        "CodexSnapshot",
    ];

    /// Every message, nested ones included, and every enum, by full name.
    fn types(set: &FileDescriptorSet) -> (BTreeMap<String, DescriptorProto>, BTreeSet<String>) {
        fn walk(
            prefix: &str,
            message: &DescriptorProto,
            messages: &mut BTreeMap<String, DescriptorProto>,
            enums: &mut BTreeSet<String>,
        ) {
            let name = format!("{prefix}.{}", message.name());
            for enumeration in &message.enum_type {
                enums.insert(format!("{name}.{}", enumeration.name()));
            }
            for nested in &message.nested_type {
                walk(&name, nested, messages, enums);
            }
            messages.insert(name, message.clone());
        }
        let (mut messages, mut enums) = (BTreeMap::new(), BTreeSet::new());
        // The vendored googleapis files are whole, used or not.
        for file in set
            .file
            .iter()
            .filter(|file| file.package().starts_with("amux."))
        {
            let package = format!(".{}", file.package());
            for message in &file.message_type {
                walk(&package, message, &mut messages, &mut enums);
            }
            for enumeration in &file.enum_type {
                enums.insert(format!("{package}.{}", enumeration.name()));
            }
        }
        (messages, enums)
    }

    /// A type stays only while something carries it. The compiler cannot
    /// tell: a generated type is public, so deleting the last field or call
    /// that used a message would otherwise leave it behind unnoticed.
    #[test]
    fn every_type_is_reachable_from_a_call_or_a_type_encoded_on_its_own() {
        let set = descriptor();
        let (messages, enums) = types(&set);
        for name in ENCODED_ON_THEIR_OWN {
            assert!(
                messages.contains_key(&format!(".amux.v1.{name}"))
                    || enums.contains(&format!(".amux.v1.{name}")),
                "{name} is listed as encoded on its own but not declared"
            );
        }
        let mut pending = set
            .file
            .iter()
            .flat_map(|file| file.service.iter())
            .flat_map(|service| service.method.iter())
            .flat_map(|method| {
                [
                    method.input_type().to_owned(),
                    method.output_type().to_owned(),
                ]
            })
            .chain(
                ENCODED_ON_THEIR_OWN
                    .iter()
                    .map(|name| format!(".amux.v1.{name}")),
            )
            // A client encodes its session history into DumpRequest.sessions.
            .chain([".amux.dump.ClientSessionHistory".to_owned()])
            .collect::<Vec<_>>();
        let mut reached = BTreeSet::new();
        while let Some(name) = pending.pop() {
            if let Some(message) = messages.get(&name)
                && !reached.contains(&name)
            {
                pending.extend(
                    message
                        .field
                        .iter()
                        .map(|field| field.type_name().to_owned())
                        .filter(|name| !name.is_empty()),
                );
            }
            reached.insert(name);
        }
        let unreached = messages
            .keys()
            .chain(&enums)
            .filter(|name| !reached.contains(*name))
            .collect::<Vec<_>>();
        assert!(
            unreached.is_empty(),
            "nothing carries these; delete them, or list a type code encodes on its own: {unreached:?}"
        );
    }

    /// Top-level enum values share one namespace in the package, so each
    /// carries its enum's name as a prefix; prost strips it in Rust.
    #[test]
    fn every_top_level_enum_value_carries_its_enum_prefix() {
        for (enumeration, values) in enums(&descriptor()) {
            let mut prefix = String::new();
            for (at, letter) in enumeration.chars().enumerate() {
                if letter.is_ascii_uppercase() && at > 0 {
                    prefix.push('_');
                }
                prefix.push(letter.to_ascii_uppercase());
            }
            prefix.push('_');
            for (name, _) in values {
                assert!(
                    name.starts_with(&prefix),
                    "{enumeration}.{name} lacks the {prefix} prefix"
                );
            }
        }
    }
}
