//! HostService: what a host offers before any agent exists, asked of the
//! host itself on its profile socket, or by a paired host's person over a
//! link. Each provider's catalogue comes from `amux catalogue <provider>`
//! run against the scripted fakes, kept as a copy per provider, asked again
//! when the provider's version changes or a signed-out one is due, and
//! named on the host's fleet entry; the repositories are the ones its
//! person chose, then where its agents ran.

use std::path::PathBuf;
use std::time::Duration;

use prost::Message as _;
use provider_fakes::script::{INPUT_LOG_ENV, OfferedModel, SCRIPT_ENV, Script};
use sha2::{Digest as _, Sha256};
use support::synthetic::*;
use support::*;
use tonic::Request;
use tonic::transport::Channel;
use wire::amux::staging::{
    self as staging, GetCatalogueRequest, GetCatalogueResponse, ListRepositoriesRequest,
    ListRepositoriesResponse,
};
use wire::{Catalogue, ProviderOnHost, inventory_event};

use crate::support;

fn model(value: &str) -> OfferedModel {
    OfferedModel {
        value: value.to_owned(),
        display_name: None,
        description: format!("{value}, scripted"),
        efforts: vec!["low".into(), "high".into()],
        default_effort: Some("low".into()),
        resolved_model: None,
    }
}

/// A host whose fakes play `script`, logging what they are sent.
struct Host {
    install: Install,
    script: PathBuf,
    log: PathBuf,
}

impl Host {
    fn new(script: &Script) -> Self {
        let install = Install::new();
        let host = Self {
            script: install.path("providers.json"),
            log: install.path("providers.log"),
            install,
        };
        host.play(script);
        host
    }

    fn play(&self, script: &Script) {
        std::fs::write(&self.script, serde_json::to_string(script).unwrap()).unwrap();
    }

    fn launch(&self) -> node::Launch {
        let mut launch = self.install.launch("agents", Vec::new());
        launch.provider_env.insert(
            SCRIPT_ENV.to_owned(),
            self.script.to_string_lossy().into_owned(),
        );
        launch.provider_env.insert(
            INPUT_LOG_ENV.to_owned(),
            self.log.to_string_lossy().into_owned(),
        );
        launch
    }

    /// How many times a provider was asked: lines the fakes were sent
    /// that hold `marker`.
    fn asked(&self, marker: &str) -> usize {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains(marker))
            .count()
    }
}

/// Claude's initialize carries this request id; Codex's lists start with
/// `model/list`.
const CLAUDE_ASKED: &str = "amux-catalogue";
const CODEX_ASKED: &str = "\"model/list\"";

/// A daemon on `install` that serves its profile's socket, as the
/// installed one does.
async fn serving(install: &Install, boot_id: &str, launch: node::Launch) -> node::Daemon {
    let options = node::StartOptions {
        front_door: Some(install.path("door.sock")),
        ..install.options(boot_id, launch)
    };
    node::start(options, None).await.expect("the daemon starts")
}

/// The profile's socket, where a person's clients call.
async fn profile_socket(runtime: &node::ProfileRuntime) -> Channel {
    local_channel(&runtime.dir().join(node::PROFILE_SOCKET)).await
}

/// A call as a paired host carries it for a person at it: on the link,
/// with the caller header that names them.
fn from_a_person<T>(body: T) -> Request<T> {
    let mut request = Request::new(body);
    request.metadata_mut().insert_bin(
        wire::CALLER_HEADER,
        tonic::metadata::BinaryMetadataValue::from_bytes(
            &staging::Caller::person("terminal").encode_to_vec(),
        ),
    );
    request
}

/// What GetCatalogue answered, decoded.
#[derive(Debug, PartialEq)]
struct Asked {
    blob: GetCatalogueResponse,
    catalogue: Catalogue,
}

impl std::ops::Deref for Asked {
    type Target = Catalogue;

    fn deref(&self) -> &Catalogue {
        &self.catalogue
    }
}

fn catalogue_of(provider: wire::Provider) -> GetCatalogueRequest {
    GetCatalogueRequest {
        provider: provider as i32,
    }
}

async fn ask_with(request: Request<GetCatalogueRequest>, channel: &Channel) -> Asked {
    let provider = request.get_ref().provider;
    let blob = staging::host_service_client(channel.clone())
        .get_catalogue(request)
        .await
        .unwrap_or_else(|error| panic!("provider {provider} on the host: {error:?}"))
        .into_inner();
    let catalogue = Catalogue::decode(blob.bytes.as_slice()).unwrap();
    Asked { blob, catalogue }
}

/// GetCatalogue for `provider`, asked of the host `channel` reaches.
async fn ask(channel: &Channel, provider: wire::Provider) -> Asked {
    ask_with(Request::new(catalogue_of(provider)), channel).await
}

fn values<T>(listed: &[T], value: impl Fn(&T) -> &str) -> Vec<&str> {
    listed.iter().map(value).collect()
}

fn on_host(provider: wire::Provider, asked: &Asked, signed_in: bool) -> ProviderOnHost {
    ProviderOnHost {
        provider: provider as i32,
        catalogue: Some(asked.blob.hash.clone()),
        signed_in,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_says_what_its_providers_offer_with_no_agent_running() {
    let host = Host::new(&Script {
        models: vec![model("opus"), model("haiku")],
        ..Script::default()
    });
    let daemon = serving(&host.install, "boot-1", host.launch()).await;
    let runtime = runtime(&daemon, &host.install);
    let socket = profile_socket(&runtime).await;
    assert!(
        runtime.host_entry().providers.is_empty(),
        "nothing asked yet"
    );

    let claude = ask(&socket, wire::Provider::Claude).await;
    assert_eq!(values(&claude.models, |m| &m.value), ["opus", "haiku"]);
    assert_eq!(claude.models[0].efforts, ["low", "high"]);
    assert_eq!(
        claude.blob.hash,
        Sha256::digest(&claude.blob.bytes).to_vec(),
        "the hash names exactly the bytes served"
    );
    let codex = ask(&socket, wire::Provider::Codex).await;
    assert_eq!(values(&codex.models, |m| &m.value), ["opus", "haiku"]);
    assert_eq!(values(&codex.modes, |m| &m.value), ["default", "plan"]);

    // A second ask answers from the copy: neither provider runs again.
    assert_eq!(ask(&socket, wire::Provider::Claude).await, claude);
    assert_eq!(ask(&socket, wire::Provider::Codex).await, codex);
    assert_eq!(host.asked(CLAUDE_ASKED), 1);
    assert_eq!(host.asked(CODEX_ASKED), 1);

    // The host's entry in the fleet list names each copy, signed in.
    let expected = vec![
        on_host(wire::Provider::Claude, &claude, true),
        on_host(wire::Provider::Codex, &codex, true),
    ];
    let mut inventory = runtime.subscribe_inventory().await.unwrap();
    let mut seen = Vec::new();
    read_inventory_until(&mut inventory, &mut seen, "this host's entry", |seen| {
        !seen.is_empty()
    })
    .await;
    match &seen[0].of {
        Some(inventory_event::Of::Host(entry)) => assert_eq!(entry.providers, expected),
        other => panic!("the inventory opens with this host, not {other:?}"),
    }

    // A provider this build does not know, and none at all.
    for provider in [99, wire::Provider::Unspecified as i32] {
        let unknown = staging::host_service_client(socket.clone())
            .get_catalogue(GetCatalogueRequest { provider })
            .await
            .unwrap_err();
        assert_eq!(unknown.code(), tonic::Code::InvalidArgument, "{unknown:?}");
    }

    // The copies outlive the daemon.
    drop((inventory, socket, runtime));
    daemon.shutdown().await.unwrap();
    let daemon = serving(&host.install, "boot-2", host.launch()).await;
    let runtime = support::runtime(&daemon, &host.install);
    assert_eq!(runtime.host_entry().providers, expected);
    let socket = profile_socket(&runtime).await;
    assert_eq!(ask(&socket, wire::Provider::Codex).await, codex);
    assert_eq!(host.asked(CODEX_ASKED), 1, "the kept copy answers");
    drop((socket, runtime));
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signed_out_provider_says_so_and_is_asked_again_once_due() {
    let host = Host::new(&Script {
        signed_out: true,
        ..Script::default()
    });
    let mut launch = host.launch();
    launch.catalogue_recheck_ms = 0;
    let daemon = serving(&host.install, "boot-1", launch).await;
    let runtime = runtime(&daemon, &host.install);
    let socket = profile_socket(&runtime).await;

    let claude = ask(&socket, wire::Provider::Claude).await;
    let codex = ask(&socket, wire::Provider::Codex).await;
    assert_eq!(
        runtime.host_entry().providers,
        [
            on_host(wire::Provider::Claude, &claude, false),
            on_host(wire::Provider::Codex, &codex, false)
        ],
        "both read signed out"
    );

    // Signing in shows at the next ask.
    host.play(&Script::default());
    let codex = ask(&socket, wire::Provider::Codex).await;
    assert_eq!(host.asked(CODEX_ASKED), 2);
    assert!(
        runtime
            .host_entry()
            .providers
            .contains(&on_host(wire::Provider::Codex, &codex, true))
    );

    drop((socket, runtime));
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_provider_version_is_asked_again() {
    let host = Host::new(&Script {
        version: Some("2.1.300".into()),
        models: vec![model("opus")],
        ..Script::default()
    });
    let mut launch = host.launch();
    // Every ask reads the version.
    launch.catalogue_recheck_ms = 0;
    let daemon = serving(&host.install, "boot-1", launch).await;
    let runtime = runtime(&daemon, &host.install);
    let socket = profile_socket(&runtime).await;

    let first = ask(&socket, wire::Provider::Claude).await;
    assert_eq!(ask(&socket, wire::Provider::Claude).await, first);
    assert_eq!(
        host.asked(CLAUDE_ASKED),
        1,
        "the same version keeps the copy"
    );

    host.play(&Script {
        version: Some("2.1.301".into()),
        models: vec![model("opus"), model("fable")],
        ..Script::default()
    });
    let second = ask(&socket, wire::Provider::Claude).await;
    assert_eq!(host.asked(CLAUDE_ASKED), 2);
    assert_eq!(values(&second.models, |m| &m.value), ["opus", "fable"]);
    assert_ne!(second.blob.hash, first.blob.hash);

    drop((socket, runtime));
    daemon.shutdown().await.unwrap();
}

/// Under an organization's managed MCP config Claude refuses
/// `--strict-mcp-config` and will not start: the host still asks it what
/// it offers.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_under_a_managed_mcp_config_still_asks_claude() {
    let host = Host::new(&Script {
        models: vec![model("opus")],
        ..Script::default()
    });
    let managed = host.install.path("managed");
    std::fs::create_dir_all(&managed).unwrap();
    std::fs::write(managed.join("managed-mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
    let mut launch = host.launch();
    launch.provider_env.insert(
        "CLAUDE_CODE_MANAGED_SETTINGS_PATH".to_owned(),
        managed.to_string_lossy().into_owned(),
    );
    let daemon = serving(&host.install, "boot-1", launch).await;
    let runtime = runtime(&daemon, &host.install);
    let socket = profile_socket(&runtime).await;
    let claude = ask(&socket, wire::Provider::Claude).await;
    assert_eq!(values(&claude.models, |m| &m.value), ["opus"]);
    drop((socket, runtime));
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_that_cannot_ask_its_provider_says_so() {
    let install = Install::new();
    let daemon = serving(&install, "boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);
    let socket = profile_socket(&runtime).await;
    let refused = staging::host_service_client(socket.clone())
        .get_catalogue(catalogue_of(wire::Provider::Codex))
        .await
        .unwrap_err();
    // This host cannot start the helper that would ask its Codex.
    assert_eq!(refused.code(), tonic::Code::Unavailable, "{refused:?}");
    drop((socket, runtime));
    daemon.shutdown().await.unwrap();
}

/// A person's pick is what the host offers a new agent from then on.
#[tokio::test(flavor = "multi_thread")]
async fn a_persons_pick_is_what_the_host_offers_next() {
    use wire::create_agent_request::Agent;
    use wire::{ClaudeCreateConfig, CreateAgentRequest};

    let host = Host::new(&Script::default());
    let daemon = serving(&host.install, "boot-1", host.launch()).await;
    let runtime = runtime(&daemon, &host.install);
    let socket = profile_socket(&runtime).await;
    let cwd = host.install.path("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let before = ask(&socket, wire::Provider::Claude).await;

    runtime
        .spawn(
            CreateAgentRequest {
                agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                name: Some("picked".into()),
                cwd: cwd.to_string_lossy().into_owned(),
                agent: Some(Agent::ClaudeSdk(ClaudeCreateConfig {
                    model: Some("haiku".into()),
                    ..Default::default()
                })),
                remember_model: true,
                ..CreateAgentRequest::default()
            },
            None,
        )
        .await
        .unwrap();
    let kept = ask(&socket, wire::Provider::Claude).await;
    assert_eq!(
        kept.new_agent.as_ref().and_then(|new| new.model.as_deref()),
        Some("haiku")
    );
    assert_ne!(kept, before);

    kill_all(&runtime).await;
    drop((socket, runtime));
    daemon.shutdown().await.unwrap();
}

/// A desk with a scripted Codex, and a laptop paired with it and linked.
struct Pair {
    desk: Host,
    desk_daemon: node::Daemon,
    desk_runtime: std::sync::Arc<node::ProfileRuntime>,
    laptop: Install,
    laptop_daemon: node::Daemon,
    laptop_runtime: std::sync::Arc<node::ProfileRuntime>,
    link: node::LoopbackLink,
    /// The laptop's channel to the desk over the link.
    to_desk: Channel,
}

impl Pair {
    async fn start(desk: Host, launch: node::Launch) -> Self {
        let desk_daemon = serving(&desk.install, "boot-1", launch).await;
        let desk_runtime = runtime(&desk_daemon, &desk.install);
        let laptop = Install::new();
        let laptop_daemon = laptop.start("boot-1", quiet_launch()).await;
        let laptop_runtime = runtime(&laptop_daemon, &laptop);
        let (desk_edge, laptop_edge) =
            (desk_runtime.edge().unwrap(), laptop_runtime.edge().unwrap());
        desk_edge.trust(&laptop_edge).await.unwrap();
        laptop_edge.trust(&desk_edge).await.unwrap();
        let link = laptop_edge.link_in_process(&desk_edge).unwrap();
        assert!(wait_for_route(&laptop_edge, desk_runtime.host(), PATIENCE).await);
        let to_desk = laptop_edge.channel(desk_runtime.host()).await.unwrap();
        Self {
            desk,
            desk_daemon,
            desk_runtime,
            laptop,
            laptop_daemon,
            laptop_runtime,
            link,
            to_desk,
        }
    }

    async fn stop(self) {
        let Self {
            desk,
            desk_daemon,
            desk_runtime,
            laptop,
            laptop_daemon,
            laptop_runtime,
            link,
            to_desk,
        } = self;
        drop((to_desk, link, desk_runtime, laptop_runtime));
        desk_daemon.shutdown().await.unwrap();
        laptop_daemon.shutdown().await.unwrap();
        drop((desk, laptop));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paired_hosts_person_is_answered_for_this_hosts_providers() {
    let desk = Host::new(&Script {
        // A model scripted with no efforts at all.
        models: vec![OfferedModel {
            efforts: Vec::new(),
            default_effort: None,
            ..model("gpt-desk")
        }],
        signed_out: true,
        ..Script::default()
    });
    let launch = desk.launch();
    let pair = Pair::start(desk, launch).await;

    let codex = ask_with(
        from_a_person(catalogue_of(wire::Provider::Codex)),
        &pair.to_desk,
    )
    .await;
    assert_eq!(values(&codex.models, |m| &m.value), ["gpt-desk"]);
    let on_desk = profile_socket(&pair.desk_runtime).await;
    assert_eq!(ask(&on_desk, wire::Provider::Codex).await, codex);
    assert_eq!(
        pair.desk.asked(CODEX_ASKED),
        1,
        "the desk asked once, for both"
    );

    // The laptop's fleet list carries the desk's word for its providers.
    let desk_host = pair.desk_runtime.host();
    let expected = vec![on_host(wire::Provider::Codex, &codex, false)];
    until("the laptop to list the desk's providers", || {
        let listed = pair
            .laptop_runtime
            .host_entry_of(desk_host)
            .map(|entry| entry.providers);
        let expected = expected.clone();
        async move {
            if listed == Some(expected) {
                Ok(())
            } else {
                Err(format!("{listed:?}"))
            }
        }
    })
    .await
    .unwrap();

    drop(on_desk);
    pair.stop().await;
}

fn names(entries: &[wire::RepositoryEntry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.name.as_str()).collect()
}

fn listing(query: Option<&str>, limit: u32) -> ListRepositoriesRequest {
    ListRepositoriesRequest {
        query: query.map(str::to_owned),
        limit,
    }
}

async fn list(
    channel: &Channel,
    request: Request<ListRepositoriesRequest>,
) -> ListRepositoriesResponse {
    staging::host_service_client(channel.clone())
        .list_repositories(request)
        .await
        .unwrap()
        .into_inner()
}

/// Where a host offers to start an agent: its repositories, in the order
/// its person chose them, then the folders its agents ran in, newest first
/// and still after the agents are deleted. A paired host's person gets the
/// same answer.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_lists_its_repositories_and_where_its_agents_ran() {
    let desk = Host::new(&Script::default());
    let code = desk.install.path("code");
    for repository in ["site", "amux", "amux-review"] {
        std::fs::create_dir_all(code.join(repository)).unwrap();
    }
    let work = desk.install.work.clone();
    let (older, newer) = (work.join("older"), work.join("newer"));
    std::fs::create_dir_all(&older).unwrap();
    std::fs::create_dir_all(&newer).unwrap();
    let mut launch = desk.launch();
    launch.repositories = vec![
        code.join("site"),
        code.join("amux"),
        desk.install.path("missing"),
        code.join("amux-review"),
        // A repository an agent also ran in is listed once, among the
        // chosen ones.
        newer.clone(),
    ];
    let pair = Pair::start(desk, launch).await;
    let runtime = &pair.desk_runtime;
    let first = id_of(
        &runtime
            .spawn(create(&older, "first", None), None)
            .await
            .unwrap(),
    );
    // The listing orders recent folders by the agents' creation times,
    // which the clock stamps in milliseconds.
    tokio::time::sleep(Duration::from_millis(5)).await;
    runtime
        .spawn(create(&newer, "second", None), None)
        .await
        .unwrap();
    runtime.delete(first).await.unwrap();

    let socket = profile_socket(runtime).await;
    let local = list(&socket, Request::new(listing(None, 50))).await;
    assert_eq!(
        names(&local.repositories),
        ["site", "amux", "amux-review", "newer"],
        "as chosen, gone ones left out"
    );
    assert_eq!(
        names(&local.recent),
        ["older"],
        "deleted agents' folders too, repositories once"
    );
    assert_eq!(
        local.recent[0].path,
        older.canonicalize().unwrap().to_str().unwrap()
    );
    let found = list(&socket, Request::new(listing(Some("REVIEW"), 50))).await;
    assert!(found.recent.is_empty());
    assert_eq!(names(&found.repositories), ["amux-review"]);
    let capped = list(&socket, Request::new(listing(None, 3))).await;
    assert_eq!(names(&capped.repositories), ["site", "amux", "amux-review"]);
    assert!(capped.recent.is_empty(), "the limit counts both lists");

    assert_eq!(
        list(&pair.to_desk, from_a_person(listing(None, 50))).await,
        local,
        "the desk answers the laptop's person as it answers its own clients"
    );

    kill_all(runtime).await;
    drop(socket);
    pair.stop().await;
}
