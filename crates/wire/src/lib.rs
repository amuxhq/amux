//! The committed protobuf messages of every amux protocol boundary: the session
//! records the journal frames and the store keeps, the agent process's spec
//! and control frames, and the client, pairing, profile and installation
//! services.

/// A sized client and server constructor for each service that carries
/// attachments: every message may be as large as the largest attachment.
/// The generic form builds its client on any service from a request to a
/// response, so a caller in the same process as its node calls the router
/// with no socket between them.
macro_rules! sized_service {
    ($client_fn:ident, $server_fn:ident, $client_mod:ident, $client:ident, $server_mod:ident, $server:ident, $service:ident) => {
        #[cfg(feature = "grpc")]
        pub fn $client_fn(
            channel: tonic::transport::Channel,
        ) -> $client_mod::$client<tonic::transport::Channel> {
            $client_mod::$client::new(channel)
                .max_decoding_message_size($crate::CHANNEL_MESSAGE_SIZE_LIMIT)
                .max_encoding_message_size($crate::CHANNEL_MESSAGE_SIZE_LIMIT)
        }

        sized_service!(@server $server_fn, $server_mod, $server, $service);
    };
    (generic $client_fn:ident, $server_fn:ident, $client_mod:ident, $client:ident, $server_mod:ident, $server:ident, $service:ident) => {
        #[cfg(feature = "grpc")]
        pub fn $client_fn<T>(inner: T) -> $client_mod::$client<T>
        where
            T: tonic::client::GrpcService<tonic::body::Body>,
            T::Error: Into<tonic::codegen::StdError>,
            T::ResponseBody: tonic::codegen::Body<Data = tonic::codegen::Bytes> + Send + 'static,
            <T::ResponseBody as tonic::codegen::Body>::Error:
                Into<tonic::codegen::StdError> + Send,
        {
            $client_mod::$client::new(inner)
                .max_decoding_message_size($crate::CHANNEL_MESSAGE_SIZE_LIMIT)
                .max_encoding_message_size($crate::CHANNEL_MESSAGE_SIZE_LIMIT)
        }

        sized_service!(@server $server_fn, $server_mod, $server, $service);
    };
    (@server $server_fn:ident, $server_mod:ident, $server:ident, $service:ident) => {
        #[cfg(feature = "grpc")]
        pub fn $server_fn<T>(service: T) -> $server_mod::$server<T>
        where
            T: $server_mod::$service,
        {
            $server_mod::$server::new(service)
                .max_decoding_message_size($crate::CHANNEL_MESSAGE_SIZE_LIMIT)
                .max_encoding_message_size($crate::CHANNEL_MESSAGE_SIZE_LIMIT)
        }
    };
}

pub mod amux {
    pub mod v1 {
        #![allow(dead_code, clippy::enum_variant_names, clippy::large_enum_variant)]
        include!("generated/amux.v1.rs");

        sized_service!(
            generic inventory_service_client,
            inventory_service_server,
            inventory_service_client,
            InventoryServiceClient,
            inventory_service_server,
            InventoryServiceServer,
            InventoryService
        );
        sized_service!(
            generic session_service_client,
            session_service_server,
            session_service_client,
            SessionServiceClient,
            session_service_server,
            SessionServiceServer,
            SessionService
        );
        sized_service!(
            generic draft_service_client,
            draft_service_server,
            draft_service_client,
            DraftServiceClient,
            draft_service_server,
            DraftServiceServer,
            DraftService
        );
        sized_service!(
            generic agent_service_client,
            agent_service_server,
            agent_service_client,
            AgentServiceClient,
            agent_service_server,
            AgentServiceServer,
            AgentService
        );
        sized_service!(
            generic host_service_client,
            host_service_server,
            host_service_client,
            HostServiceClient,
            host_service_server,
            HostServiceServer,
            HostService
        );
        sized_service!(
            generic review_service_client,
            review_service_server,
            review_service_client,
            ReviewServiceClient,
            review_service_server,
            ReviewServiceServer,
            ReviewService
        );
        sized_service!(
            generic peer_service_client,
            peer_service_server,
            peer_service_client,
            PeerServiceClient,
            peer_service_server,
            PeerServiceServer,
            PeerService
        );
    }

    /// What a client writes into a dump, read back only by the build that
    /// wrote it; outside the protocol's compatibility promises.
    pub mod dump {
        #![allow(dead_code, clippy::enum_variant_names, clippy::large_enum_variant)]
        include!("generated/amux.dump.rs");
    }

    /// The shapes of experiments, carried as bytes in the `experimental`
    /// maps of amux.v1 messages; outside the protocol's compatibility
    /// promises, so they change freely.
    pub mod experimental {
        #![allow(dead_code, clippy::enum_variant_names, clippy::large_enum_variant)]
        include!("generated/amux.experimental.rs");

        pub use crate::entry::{Entry, Map};

        /// The workspaces experiment's entry on an agent's row:
        /// `Agent.experimental["workspaces/1"]`.
        pub const WORKSPACE: Entry<AgentWorkspace> = Entry::new("workspaces/1");

        /// The suspension experiment's entry in an agent's spec:
        /// `AgentSpec.experimental["suspension/1"]`.
        pub const SUSPENSION: Entry<SuspensionSpec> = Entry::new("suspension/1");

        /// The reviews experiment's entry on an agent's row:
        /// `Agent.experimental["reviews/1"]`.
        pub const REVIEW_ROW: Entry<ReviewRowEntry> = Entry::new("reviews/1");

        /// The reviews experiment's entry on a sent review's attachment:
        /// `Attachment.experimental["reviews/1"]`. The same version keys a
        /// different message on each map.
        pub const SENT_REVIEW: Entry<SentReview> = Entry::new("reviews/1");

        sized_service!(
            generic reviews1_client,
            reviews1_server,
            reviews1_client,
            Reviews1Client,
            reviews1_server,
            Reviews1Server,
            Reviews1
        );
    }
}

pub use amux::{dump, experimental};

/// gRPC's richer error model, from the vendored googleapis protos.
pub mod google {
    pub mod rpc {
        #![allow(dead_code, clippy::enum_variant_names, clippy::doc_lazy_continuation)]
        include!("generated/google.rpc.rs");
    }
}

pub use google::rpc;

pub mod agents;
mod authority;
mod caller_header;
mod capabilities;
pub mod codex;
mod continued;
mod entry;
mod error;
mod host_name;
pub mod levels;
#[cfg(test)]
mod pinned;
mod said;
pub use amux::v1::*;
pub use authority::{authority, host_of_authority};
pub use caller_header::CallerHeaderError;
pub use continued::{continued_input_id, is_continued_input};
pub use host_name::{HostName, HostNameError, MAX_HOST_NAME_CHARS};
/// An error's code, gRPC's own.
pub use rpc::Code as ErrorCode;
/// amux's error, a `google.rpc.Status`: named apart so it never reads as
/// tonic's `Status`, the transport's carrier for it.
pub use rpc::Status as Error;
pub use said::{Said, Wants};

/// Bound for link-control messages.
pub const MESSAGE_SIZE_LIMIT: usize = 16 * 1024 * 1024;
/// Bound for a link's first message, its Hello or the answer to one, read
/// before anything has checked who sent it. Its fields are bounded but for
/// the neighbors, one host entry per link the sender holds: this fits well
/// over a hundred neighbors at the longest names.
pub const HELLO_SIZE_LIMIT: usize = 64 * 1024;
/// The largest attachment a person may send: 64 MiB of the file's own
/// bytes. Clients word it "64 MB".
pub const ATTACHMENT_SIZE_LIMIT: usize = 64 * 1024 * 1024;
/// RPC payloads: the largest attachment plus room for the protobuf framing
/// around it (a `PutBlobRequest`'s agent id, name and mime type), so a file
/// at the attachment limit still fits in one message.
pub const CHANNEL_MESSAGE_SIZE_LIMIT: usize = ATTACHMENT_SIZE_LIMIT + 1024 * 1024;

/// The request header a client of a profile socket names itself in, for
/// the usage counts only: `terminal`, `mac`, `phone` or `browser`. It is
/// optional: a client that sends none, or a word amux doesn't know, is
/// counted as `other`.
pub const SURFACE_HEADER: &str = "amux-surface";

/// The request header a node writes beside a call it carries to another
/// host, naming the person or agent the call is for: a `Caller`, encoded,
/// as gRPC writes a binary header. A host reads it only on a link, where
/// the host it speaks for is the one the link's handshake authenticated.
pub const CALLER_HEADER: &str = "amux-caller-bin";

/// The detail an incarnation's end gives when its provider died of a signal
/// the agent did not send: amux's own stops and kills end with causes of
/// their own, so this one came from outside, as when the machine shuts
/// down. Released builds write these words, so they are read as written.
pub const PROVIDER_SIGNALLED: &str = "killed by a signal";

/// How an incarnation's end words a provider's exit status: "exit code 1".
pub const PROVIDER_EXIT_CODE: &str = "exit code ";

/// Whether a provider's own end, by its detail, was a signal: killed by one
/// (see [`PROVIDER_SIGNALLED`]), or exiting with the status a shell gives for
/// one, 128 and its number, as a provider that catches the signal does
/// (headless Claude exits 143 on SIGTERM). Only the signals a machine
/// shutting down or a closed terminal sends count: SIGHUP, SIGINT, SIGKILL
/// and SIGTERM.
pub fn provider_signalled(detail: Option<&str>) -> bool {
    const SIGNALS: [i32; 4] = [1, 2, 9, 15];
    let Some(detail) = detail else {
        return false;
    };
    detail == PROVIDER_SIGNALLED
        || detail
            .strip_prefix(PROVIDER_EXIT_CODE)
            .and_then(|code| code.parse::<i32>().ok())
            .is_some_and(|code| SIGNALS.iter().any(|signal| code == 128 + signal))
}

/// What a caller says of a method its host does not have. gRPC's own answer
/// to an unknown method has no words, and a caller's "could not start the
/// agent: " would end at the colon.
pub const HOST_NEEDS_UPDATE: &str = "that device needs updating to do this";

/// A person's input as its kind's own message: what a client builds and
/// what the agent of that kind decodes from [`Input::body`].
#[derive(Clone, Debug, PartialEq)]
pub enum KindInput {
    ClaudePty(ClaudePtyInput),
    ClaudeSdk(ClaudeSdkInput),
    Codex(CodexInput),
    Acp(AcpInput),
}

impl KindInput {
    pub fn kind(&self) -> Kind {
        match self {
            Self::ClaudePty(_) => Kind::ClaudePty,
            Self::ClaudeSdk(_) => Kind::ClaudeSdk,
            Self::Codex(_) => Kind::Codex,
            Self::Acp(_) => Kind::Acp,
        }
    }

    fn encode(&self) -> Vec<u8> {
        use prost::Message as _;
        match self {
            Self::ClaudePty(input) => input.encode_to_vec(),
            Self::ClaudeSdk(input) => input.encode_to_vec(),
            Self::Codex(input) => input.encode_to_vec(),
            Self::Acp(input) => input.encode_to_vec(),
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

impl From<AcpInput> for KindInput {
    fn from(input: AcpInput) -> Self {
        Self::Acp(input)
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
            Kind::Acp => AcpInput::decode(body).ok().map(KindInput::Acp),
            Kind::Unspecified => None,
        }
    }
}

impl create_agent_request::Agent {
    /// The kind with its default settings; None for no kind, and for an
    /// ACP agent, which has no default: which agent it is must be named.
    pub fn defaults(kind: Kind) -> Option<Self> {
        match kind {
            Kind::ClaudePty => Some(Self::ClaudePty(ClaudeCreateConfig::default())),
            Kind::ClaudeSdk => Some(Self::ClaudeSdk(ClaudeCreateConfig::default())),
            Kind::Codex => Some(Self::Codex(CodexCreateConfig::default())),
            Kind::Acp | Kind::Unspecified => None,
        }
    }

    /// The interpreter this arm creates.
    pub fn kind(&self) -> Kind {
        match self {
            Self::ClaudePty(_) => Kind::ClaudePty,
            Self::ClaudeSdk(_) => Kind::ClaudeSdk,
            Self::Codex(_) => Kind::Codex,
            Self::Acp(_) => Kind::Acp,
        }
    }

    /// The provider id of the program this arm starts: Claude's or Codex's,
    /// or the ACP agent's it names.
    pub fn provider_id(&self) -> &str {
        match self {
            Self::Acp(acp) => &acp.agent,
            other => agents::implied_by(other.kind()),
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
                same_network: false,
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
                ..Candidate::default()
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

    /// A trusted host online over a direct path on a network both machines
    /// are on, which a client calls Local.
    pub fn same_network(&self) -> bool {
        self.online() == Some(RouteKind::Direct)
            && self.trusted().is_some_and(|trusted| trusted.same_network)
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

impl HostEntry {
    /// What the host says of the provider with this id, when it runs it.
    pub fn provider(&self, id: &str) -> Option<&ProviderOnHost> {
        self.providers
            .iter()
            .find(|provider| provider.provider_id == id)
    }
}

impl ProviderOnHost {
    /// The kind a new agent of this provider starts as on its host.
    pub fn kind(&self) -> Kind {
        self.descriptor
            .as_ref()
            .map_or(Kind::Unspecified, ProviderDescriptor::kind)
    }

    /// What people call it, else its id.
    pub fn name(&self) -> &str {
        self.descriptor
            .as_ref()
            .map(|descriptor| descriptor.name.as_str())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| agents::name_or_id(&self.provider_id))
    }

    /// Whether it is signed in, once the host has asked it.
    pub fn signed_in(&self) -> Option<bool> {
        self.catalogue.is_some().then_some(self.signed_in)
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

impl Platform {
    /// The operating system this build was compiled for, and nothing more,
    /// as a value: what an agent and its host say of themselves. A client
    /// describing another host's agent uses that host's platform instead.
    pub fn here() -> Self {
        match std::env::consts::OS {
            "macos" => Platform::Macos,
            "linux" => Platform::Linux,
            "windows" => Platform::Windows,
            "ios" => Platform::Ios,
            "android" => Platform::Android,
            _ => Platform::Unspecified,
        }
    }

    /// A phone: iOS or Android.
    pub fn is_phone(self) -> bool {
        matches!(self, Platform::Ios | Platform::Android)
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
pub const WORKING_ON_TOOL: &str = "set_working_on";
/// amux's tool that messages another agent.
pub const MESSAGE_TOOL: &str = "message_agent";
/// amux's tool that attaches a file to the caller's reply.
pub const ATTACH_TOOL: &str = "attach_file";
/// amux's tool that starts a child agent.
pub const START_TOOL: &str = "start_subagent";
/// amux's tool that stops one of the caller's children.
pub const STOP_TOOL: &str = "stop_subagent";
/// amux's tools that list the fleet and the trusted hosts.
pub const AGENTS_TOOL: &str = "list_agents";
pub const HOSTS_TOOL: &str = "list_hosts";

/// What releases before 0.11 named the same tools. An agent resumed from
/// then still has calls by these names in its provider's history, so the
/// interpreters and views read both.
pub mod former_tools {
    pub const STATUS_TOOL: &str = "status";
    pub const SEND_TOOL: &str = "send";
    pub const ATTACH_TOOL: &str = "attach";
    pub const SPAWN_TOOL: &str = "spawn";
    pub const STOP_TOOL: &str = "stop";
    pub const AGENTS_TOOL: &str = "agents";
    pub const HOSTS_TOOL: &str = "hosts";
}

/// Whether `tool` is amux's tool named `now`, by its name now or by the
/// name a release before 0.11 gave it, `before`.
pub fn is_tool(tool: &str, now: &str, before: &str) -> bool {
    tool == now || tool == before
}

/// Why a name cannot name an agent. Each client words it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

    #[test]
    fn a_provider_ending_by_a_shutdowns_signal_reads_signalled() {
        let signalled = [
            PROVIDER_SIGNALLED,
            "exit code 129",
            "exit code 130",
            "exit code 137",
            "exit code 143",
        ];
        for detail in signalled {
            assert!(provider_signalled(Some(detail)), "{detail}");
        }
        let not = [
            None,
            Some("exit code 1"),
            Some("exit code 134"),
            Some("exit code 15"),
            Some("exit code x"),
        ];
        for detail in not {
            assert!(!provider_signalled(detail), "{detail:?}");
        }
    }

    fn descriptor() -> FileDescriptorSet {
        FileDescriptorSet::decode(DESCRIPTOR_SET).expect("descriptor set should decode")
    }

    /// Every top-level enum, with its values named under it.
    fn enums(set: &FileDescriptorSet) -> BTreeMap<String, Vec<(String, i32)>> {
        set.file
            .iter()
            .filter(|file| file.package() == "amux.v1")
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
    /// steps, the spec an agent starts from, the link's framing, the error
    /// reason, and each provider's item, input and snapshot bodies, which
    /// travel as bytes.
    const ENCODED_ON_THEIR_OWN: &[&str] = &[
        "AgentSpec",
        "CtlFrame",
        "PtyFrame",
        "AgentAsk",
        "AgentAnswer",
        "ProviderOffer",
        "Step",
        "Message",
        // Named, not encoded: an ErrorInfo's reason is a value's name.
        "ErrorReason",
        "ClaudeItem",
        "ClaudeSnapshot",
        "ClaudePtyInput",
        "ClaudeSdkInput",
        "CodexItem",
        "CodexInput",
        "CodexSnapshot",
        "AcpItem",
        "AcpInput",
        "AcpSnapshot",
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
        // The vendored googleapis files are google's, outside amux's package.
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
            // Each experiment's values are encoded into an `experimental`
            // map entry, and a carried call's caller into its header.
            .chain(
                [
                    ".amux.experimental.AgentWorkspace",
                    ".amux.experimental.SuspensionSpec",
                    ".amux.experimental.ReviewRowEntry",
                    ".amux.experimental.SentReview",
                    ".amux.v1.Caller",
                ]
                .map(str::to_owned),
            )
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

    /// What a build before the `experimental` maps decodes an `Agent` row
    /// and an `AgentSpec` as: their first fields and nothing at 1000.
    #[derive(Clone, PartialEq, prost::Message)]
    struct Before {
        #[prost(bytes = "vec", tag = "1")]
        agent_id: Vec<u8>,
        #[prost(string, tag = "4")]
        name: String,
    }

    /// An `experimental` entry is field 1000 to a build that predates the
    /// map, so 0.9 hosts and agent processes read a row or a spec carrying
    /// one as they read it without, and an entry under a version a build
    /// does not run is only bytes under a key it never asks for.
    #[test]
    fn a_build_without_the_experimental_maps_reads_past_them() {
        let workspace = experimental::AgentWorkspace {
            kind: experimental::WorkspaceKind::Wt as i32,
            id: "/src/app".into(),
            made_by_amux: true,
            ..Default::default()
        };
        let mut agent = Agent {
            agent_id: b"agent".to_vec(),
            name: "coder".into(),
            ..Default::default()
        };
        experimental::WORKSPACE.put(&mut agent.experimental, &workspace);
        agent
            .experimental
            .insert("workspaces/2".into(), b"another shape".to_vec());
        let mut spec = AgentSpec {
            agent_id: b"agent".to_vec(),
            name: "coder".into(),
            ..Default::default()
        };
        experimental::SUSPENSION.put(
            &mut spec.experimental,
            &experimental::SuspensionSpec { after_ms: 5 },
        );
        for bytes in [agent.encode_to_vec(), spec.encode_to_vec()] {
            assert_eq!(
                Before::decode(bytes.as_slice()).unwrap(),
                Before {
                    agent_id: b"agent".to_vec(),
                    name: "coder".into(),
                }
            );
        }
        let read = Agent::decode(agent.encode_to_vec().as_slice()).unwrap();
        assert_eq!(
            experimental::WORKSPACE.get(&read.experimental),
            Some(workspace)
        );
        // The map encodes in key order, so a row encodes the same each time.
        assert_eq!(read.encode_to_vec(), agent.encode_to_vec());
    }

    /// The reviews experiment keeps one entry on an agent's row and one on
    /// a sent review's attachment, under the same version, and each comes
    /// back whole through its message's encoding.
    #[test]
    fn the_review_entries_round_trip_through_a_row_and_an_attachment() {
        assert_eq!(experimental::REVIEW_ROW.version(), "reviews/1");
        assert_eq!(experimental::SENT_REVIEW.version(), "reviews/1");
        let entry = experimental::ReviewRowEntry {
            drafts: 3,
            counter: 41,
            moved: true,
            review_id: b"review".to_vec(),
        };
        let mut agent = Agent {
            agent_id: b"agent".to_vec(),
            ..Default::default()
        };
        experimental::REVIEW_ROW.put(&mut agent.experimental, &entry);
        let read = Agent::decode(agent.encode_to_vec().as_slice()).unwrap();
        assert_eq!(
            experimental::REVIEW_ROW.get(&read.experimental),
            Some(entry)
        );

        let sent = experimental::SentReview {
            review_id: b"review".to_vec(),
            round: 2,
            snapshot: "4f2a".into(),
            base_commit: "9c1e".into(),
            comments: vec![
                experimental::ReviewSentComment {
                    id: b"first".to_vec(),
                    revision: 1,
                    anchor: Some(experimental::ReviewAnchor {
                        commit: "4f2a".into(),
                        path: "src/store.rs".into(),
                        lines: Some(experimental::ReviewLineRange { start: 3, end: 4 }),
                    }),
                },
                experimental::ReviewSentComment {
                    id: b"second".to_vec(),
                    revision: 3,
                    anchor: None,
                },
            ],
        };
        let mut attachment = Attachment {
            of: Some(attachment::Of::Text(InlineText {
                name: "review".into(),
                text: "Two comments.".into(),
            })),
            ..Default::default()
        };
        experimental::SENT_REVIEW.put(&mut attachment.experimental, &sent);
        let read = Attachment::decode(attachment.encode_to_vec().as_slice()).unwrap();
        assert_eq!(read, attachment);
        assert_eq!(
            experimental::SENT_REVIEW.get(&read.experimental),
            Some(sent)
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
