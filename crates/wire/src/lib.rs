//! Committed protobuf messages for every amux protocol boundary: the session
//! records the journal frames and the store keeps, the agent process's spec
//! and control frames, and the client, peer, pairing, profile and
//! installation services.

/// Protocol version for the native-stream link handshake. It stays 1 until
/// the first release; see the header of `service.proto`.
pub const PROTOCOL_VERSION: u32 = 1;

pub mod amux {
    pub mod v1 {
        #![allow(dead_code, clippy::enum_variant_names, clippy::large_enum_variant)]
        include!("generated/amux.v1.rs");
    }
}

pub use amux::v1::*;

pub mod pb {
    pub use super::amux::v1::*;
}

/// Bound for link-control messages and application-stream prefaces.
pub const MESSAGE_SIZE_LIMIT: usize = 16 * 1024 * 1024;
/// RPC payloads include blobs plus protobuf framing overhead.
pub const CHANNEL_MESSAGE_SIZE_LIMIT: usize = 64 * 1024 * 1024;

pub fn client_service_client(
    channel: tonic::transport::Channel,
) -> client_service_client::ClientServiceClient<tonic::transport::Channel> {
    client_service_client::ClientServiceClient::new(channel)
        .max_decoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
}

pub fn client_service_server<T>(service: T) -> client_service_server::ClientServiceServer<T>
where
    T: client_service_server::ClientService,
{
    client_service_server::ClientServiceServer::new(service)
        .max_decoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
}

pub fn peer_service_client(
    channel: tonic::transport::Channel,
) -> peer_service_client::PeerServiceClient<tonic::transport::Channel> {
    peer_service_client::PeerServiceClient::new(channel)
        .max_decoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
}

pub fn peer_service_server<T>(service: T) -> peer_service_server::PeerServiceServer<T>
where
    T: peer_service_server::PeerService,
{
    peer_service_server::PeerServiceServer::new(service)
        .max_decoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
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

impl ExitCause {
    /// How a person reads the cause, after the word "exited", so none
    /// repeats it. A cause this build does not know reads as nothing: an
    /// unknown cause is not a failure.
    pub fn words(self) -> &'static str {
        match self {
            ExitCause::Unspecified => "",
            ExitCause::Finished => "finished",
            ExitCause::Stopped => "stopped",
            ExitCause::Aborted => "aborted",
            ExitCause::Killed => "killed",
            ExitCause::ProviderExited => "the provider exited",
            // Both read as the person sees them: the agent ended while no
            // daemon was there, whether it drained or was found dead.
            ExitCause::DaemonLost | ExitCause::EndedWhileAway => "while the daemon was away",
            ExitCause::Orphaned => "orphaned while waiting for you",
            ExitCause::CouldNotStart => "could not start",
            ExitCause::DirectoryFailed => "its directory failed",
            ExitCause::EndedUnexpectedly => "ended unexpectedly",
        }
    }
}

/// A cause as one line: its words, then its own words when it has any
/// ("the provider exited: exit code 1"). A value this build does not know
/// shows only its own words.
pub fn exit_words(cause: i32, detail: Option<&str>) -> String {
    let words = ExitCause::try_from(cause).map_or("", ExitCause::words);
    match detail.filter(|detail| !detail.is_empty()) {
        Some(detail) if words.is_empty() => detail.to_owned(),
        Some(detail) => format!("{words}: {detail}"),
        None => words.to_owned(),
    }
}

/// The longest name an agent can have.
pub const AGENT_NAME_MOST: usize = 64;

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

    fn messages(set: &FileDescriptorSet) -> BTreeMap<String, DescriptorProto> {
        set.file
            .iter()
            .filter(|file| file.package.as_deref() == Some("amux.v1"))
            .flat_map(|file| file.message_type.iter())
            .map(|message| (message.name().to_owned(), message.clone()))
            .collect()
    }

    fn fields(message: &DescriptorProto) -> BTreeMap<&str, i32> {
        message
            .field
            .iter()
            .map(|field| (field.name(), field.number()))
            .collect()
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

    #[test]
    fn services_are_the_client_peer_pairing_profile_and_installation_surfaces() {
        let set = descriptor();
        let services = set
            .file
            .iter()
            .flat_map(|file| file.service.iter())
            .map(|service| service.name().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            services,
            BTreeSet::from(
                [
                    "ClientService",
                    "InstallationService",
                    "PairingService",
                    "PeerService",
                    "ProfileService"
                ]
                .map(str::to_owned)
            )
        );

        let client = methods(&set, "ClientService");
        assert_eq!(
            client,
            BTreeSet::from(
                [
                    "SubscribeInventory",
                    "ResolveAgent",
                    "Subscribe",
                    "Fetch",
                    "Get",
                    "SendInput",
                    "CreateAgent",
                    "RenameAgent",
                    "StopAgent",
                    "ResumeAgent",
                    "DeleteAgent",
                    "SendMessage",
                    "PutBlob",
                    "GetBlob",
                    "Diff",
                    "ListRepositories",
                    "Dump",
                    "GetCatalogue",
                ]
                .map(str::to_owned)
            )
        );

        // The peer surface is the client surface minus name resolution, over
        // the same request and response messages. A peer's Dump answers
        // for the agents the called host runs, so a dump taken elsewhere
        // holds their host-side parts.
        let mut peer_expected = client.clone();
        peer_expected.remove("ResolveAgent");
        assert_eq!(methods(&set, "PeerService"), peer_expected);

        let installation = methods(&set, "InstallationService");
        assert_eq!(
            installation,
            BTreeSet::from(["GetInfo", "Shutdown"].map(str::to_owned))
        );
    }

    #[test]
    fn a_spec_names_its_parent_by_host_and_id() {
        let set = descriptor();
        let messages = messages(&set);
        let parent = messages["AgentSpec"]
            .field
            .iter()
            .find(|field| field.name() == "parent")
            .expect("AgentSpec.parent");
        assert_eq!(parent.number(), 6);
        assert_eq!(parent.type_name(), ".amux.v1.AgentRef");
        assert_eq!(
            fields(&messages["AgentRef"]),
            BTreeMap::from([("host_id", 1), ("agent_id", 2)])
        );
    }

    #[test]
    fn every_snapshot_body_carries_usage_servers_sign_in_and_background_jobs() {
        let set = descriptor();
        let messages = messages(&set);
        for (snapshot, usage) in [
            ("ClaudePtySnapshot", ".amux.v1.ClaudeUsage"),
            ("ClaudeSdkSnapshot", ".amux.v1.ClaudeUsage"),
            ("CodexSnapshot", ".amux.v1.CodexUsage"),
        ] {
            let types = messages[snapshot]
                .field
                .iter()
                .map(|field| field.type_name())
                .collect::<BTreeSet<_>>();
            for fact in [
                usage,
                ".amux.v1.ToolServerHealth",
                ".amux.v1.SignIn",
                ".amux.v1.BackgroundJobs",
            ] {
                assert!(types.contains(fact), "{snapshot} lacks {fact}");
            }
        }
    }

    #[test]
    fn a_step_round_trips_with_an_opaque_body() {
        let body = ClaudeSdkItem {
            kind: Some(claude_sdk_item::Kind::Message(Text { complete: true })),
        }
        .encode_to_vec();
        let step = Step {
            items: vec![Item {
                key: "msg:1".into(),
                text: "hello".into(),
                kind: Kind::ClaudeSdk as i32,
                body: body.clone(),
                at_ms: 7,
                ..Default::default()
            }],
            turn_end: Some(TurnEnd {
                turn_id: 1,
                last_message_key: "msg:1".into(),
            }),
            ..Default::default()
        };
        let decoded = Step::decode(step.encode_to_vec().as_slice()).expect("decode");
        assert_eq!(decoded, step);
        assert_eq!(decoded.items[0].kind(), Kind::ClaudeSdk);
        assert_eq!(
            ClaudeSdkItem::decode(decoded.items[0].body.as_slice()).expect("body"),
            ClaudeSdkItem::decode(body.as_slice()).expect("body")
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

    /// The enums whose zero is a meaning, not "unknown": an absent field
    /// reads as that meaning, so the zero must stay where it is documented.
    #[test]
    fn zero_defaults_are_the_documented_ones() {
        let enums = enums(&descriptor());
        for (enumeration, zero) in [
            ("Phase", "PHASE_STARTING"),
            ("StopMode", "STOP_MODE_GRACEFUL"),
            ("PtyMode", "PTY_MODE_FILES"),
            ("UsageState", "USAGE_STATE_UNKNOWN"),
            ("HealthState", "HEALTH_STATE_UNKNOWN"),
            ("SignInState", "SIGN_IN_STATE_UNKNOWN"),
            ("DecisionOutcome", "DECISION_OUTCOME_NONE"),
            ("PlanVerdict", "PLAN_VERDICT_UNDECIDED"),
            ("AskOutcome", "ASK_OUTCOME_UNSPECIFIED"),
        ] {
            let values = enums
                .get(enumeration)
                .unwrap_or_else(|| panic!("no enum {enumeration}"));
            let found = values.iter().find(|(_, number)| *number == 0);
            assert_eq!(
                found.map(|(name, _)| name.as_str()),
                Some(zero),
                "{enumeration}'s zero"
            );
        }
        assert_eq!(Phase::default(), Phase::Starting);
        assert_eq!(StopMode::default(), StopMode::Graceful);
        assert_eq!(PtyMode::default(), PtyMode::Files);
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
