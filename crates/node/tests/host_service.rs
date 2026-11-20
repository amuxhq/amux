//! HostService: what a host offers before any agent exists, asked of the
//! host itself on its profile socket, or by a paired host's person over a
//! link. Each provider's catalogue comes from `amux catalogue <provider>`
//! run against the scripted fakes, kept as a copy per provider, asked again
//! when the provider's version changes or a signed-out one is due, and
//! named on the host's fleet entry. The host's folders are not asked: they
//! ride on its entry (the registry tests), and pinning one is in the
//! settings tests.

use std::path::PathBuf;

use prost::Message as _;
use provider_fakes::script::{INPUT_LOG_ENV, OfferedCommand, OfferedModel, SCRIPT_ENV, Script};
use sha2::{Digest as _, Sha256};
use support::synthetic::*;
use support::*;
use tonic::Request;
use tonic::transport::Channel;
use wire::amux::staging::{self as staging, GetCatalogueRequest, GetCatalogueResponse};
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

fn command(name: &str) -> OfferedCommand {
    OfferedCommand {
        name: name.to_owned(),
        description: format!("{name}, scripted"),
        argument_hint: String::new(),
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

/// A person's pick is what later agents start with, in every client's
/// catalogue and in the agent's own launch.
#[tokio::test(flavor = "multi_thread")]
async fn a_persons_pick_is_what_later_agents_start_with() {
    use staging::create_agent_request::Agent;
    use wire::ClaudeCreateConfig;

    let host = Host::new(&Script::default());
    let daemon = serving(&host.install, "boot-1", host.launch()).await;
    let runtime = runtime(&daemon, &host.install);
    let socket = profile_socket(&runtime).await;
    let cwd = host.install.path("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let create =
        |name: &str, model: Option<&str>, remember_model: bool| staging::CreateAgentRequest {
            agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
            name: Some(name.into()),
            cwd: cwd.to_string_lossy().into_owned(),
            agent: Some(Agent::ClaudeSdk(ClaudeCreateConfig {
                model: model.map(str::to_owned),
                ..Default::default()
            })),
            remember_model,
            ..staging::CreateAgentRequest::default()
        };
    let args_of = |agent: &wire::Agent| {
        let bytes = std::fs::read(
            host.install
                .agent_dir(uuid::Uuid::from_slice(&agent.agent_id).unwrap())
                .join("spec.1"),
        )
        .unwrap();
        wire::AgentSpec::decode(bytes.as_slice())
            .unwrap()
            .provider_args
    };
    let before = ask(&socket, wire::Provider::Claude).await;

    let unpicked = runtime
        .spawn(create("first", None, false), None)
        .await
        .unwrap();
    assert!(
        !args_of(&unpicked).contains(&"--model".to_owned()),
        "with nothing kept, Claude decides"
    );

    runtime
        .spawn(create("second", Some("haiku"), true), None)
        .await
        .unwrap();
    let kept = ask(&socket, wire::Provider::Claude).await;
    assert_eq!(
        kept.new_agent.as_ref().and_then(|new| new.model.as_deref()),
        Some("haiku")
    );
    assert_ne!(kept, before);

    let later = runtime
        .spawn(create("third", None, false), None)
        .await
        .unwrap();
    let args = args_of(&later);
    assert!(
        args.windows(2).any(|pair| pair == ["--model", "haiku"]),
        "{args:?}"
    );

    // Claude in its own terminal keeps to Claude's settings. Windows does not
    // host terminal Claude (see docs/ARCHITECTURE.md, "Windows, as a stated
    // cost").
    #[cfg(unix)]
    {
        let terminal = runtime
            .spawn(
                staging::CreateAgentRequest {
                    agent: defaults(wire::Kind::ClaudePty),
                    ..create("fourth", None, false)
                },
                None,
            )
            .await
            .unwrap();
        assert!(!args_of(&terminal).contains(&"--model".to_owned()));
    }

    // A daemon that shuts down leaves its agents running.
    kill_all(&runtime).await;
    drop((socket, runtime));
    daemon.shutdown().await.unwrap();
}

// Unix only: Windows does not host terminal Claude (see docs/ARCHITECTURE.md,
// "Windows, as a stated cost").
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn terminal_claude_offers_what_its_hosts_claude_offers() {
    use wire::{Kind, session_event};

    let host = Host::new(&Script {
        models: vec![model("opus"), model("haiku")],
        commands: vec![command("review")],
        ..Script::default()
    });
    let daemon = serving(&host.install, "boot-1", host.launch()).await;
    let runtime = runtime(&daemon, &host.install);
    let socket = profile_socket(&runtime).await;

    let agent = runtime
        .spawn(
            staging::CreateAgentRequest {
                agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                name: Some("terminal".into()),
                cwd: host.install.work.to_string_lossy().into_owned(),
                agent: defaults(Kind::ClaudePty),
                ..staging::CreateAgentRequest::default()
            },
            None,
        )
        .await
        .unwrap();
    let mut subscription = runtime
        .subscribe_session(&agent.agent_id, 10)
        .await
        .unwrap();
    let mut seen = Vec::new();
    let named = |seen: &[wire::SessionEvent]| {
        seen.iter().find_map(|event| match &event.of {
            Some(session_event::Of::Snapshot(snapshot)) => snapshot.catalogue.clone(),
            _ => None,
        })
    };
    read_until(&mut subscription, &mut seen, "a catalogue", |seen| {
        named(seen).is_some()
    })
    .await;
    let blob = staging::session_service_client(socket.clone())
        .get_blob(Request::new(staging::GetBlobRequest {
            agent_id: agent.agent_id.clone(),
            hash: named(&seen).unwrap(),
        }))
        .await
        .unwrap()
        .into_inner();
    let offered = Catalogue::decode(blob.bytes.as_slice()).unwrap();
    // A folder with no project of its own offers what the host's Claude
    // offers anywhere; the agent's was asked in its folder.
    let hosts = runtime
        .host_catalogue(node::Provider::Claude)
        .await
        .unwrap();
    assert_eq!(offered.models, hosts.catalogue.models);
    assert_eq!(offered.commands, hosts.catalogue.commands);
    assert_eq!(
        values(&offered.permissions, |p| &p.value),
        ["default", "acceptEdits", "plan"],
        "auto as the host's Claude offers it, never-ask only when launched allowing it"
    );
    assert!(
        offered.permissions.iter().all(|p| !p.settable),
        "only the terminal's own key reaches them"
    );

    drop(subscription);
    kill_all(&runtime).await;
    drop((socket, runtime));
    daemon.shutdown().await.unwrap();
}

/// Terminal Claude offers the commands of the project it runs in: Claude
/// lists a project's own commands only when asked from that project's
/// folder, so what the agent offers is gathered there, not in the daemon's
/// own directory. The fake stands in for that by playing the project's
/// script when asked from inside the project.
/// QA: a project's own slash commands never appeared in terminal Claude's
/// command list.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn terminal_claude_offers_the_commands_of_the_project_it_runs_in() {
    use std::os::unix::fs::PermissionsExt as _;

    use wire::{Kind, session_event};

    let host = Host::new(&Script {
        commands: vec![command("review")],
        ..Script::default()
    });
    let project = host.install.path("project");
    std::fs::create_dir_all(project.join(".claude/commands")).unwrap();
    std::fs::write(
        project.join(".claude/commands/deploy.md"),
        "Deploy this project.\n",
    )
    .unwrap();
    std::fs::write(
        project.join(".claude/fake-script.json"),
        serde_json::to_string(&Script {
            commands: vec![command("review"), command("deploy")],
            ..Script::default()
        })
        .unwrap(),
    )
    .unwrap();
    // Claude as the host runs it: inside a project, the project's script.
    let claude = host.install.path("claude-in-folder");
    let fake = std::path::Path::new(&host.launch().claude_command).to_owned();
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nif [ -f .claude/fake-script.json ]; then\n  {SCRIPT_ENV}=\"$(pwd)/.claude/fake-script.json\"\n  export {SCRIPT_ENV}\nfi\nexec '{}' \"$@\"\n",
            fake.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut launch = host.launch();
    launch.claude_command = claude.to_string_lossy().into_owned();
    let daemon = serving(&host.install, "boot-1", launch).await;
    let runtime = runtime(&daemon, &host.install);
    let socket = profile_socket(&runtime).await;

    let agent = runtime
        .spawn(
            staging::CreateAgentRequest {
                agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                name: Some("terminal".into()),
                cwd: project.to_string_lossy().into_owned(),
                agent: defaults(Kind::ClaudePty),
                ..staging::CreateAgentRequest::default()
            },
            None,
        )
        .await
        .unwrap();
    let mut subscription = runtime
        .subscribe_session(&agent.agent_id, 10)
        .await
        .unwrap();
    let mut seen = Vec::new();
    let named = |seen: &[wire::SessionEvent]| {
        seen.iter().find_map(|event| match &event.of {
            Some(session_event::Of::Snapshot(snapshot)) => snapshot.catalogue.clone(),
            _ => None,
        })
    };
    read_until(&mut subscription, &mut seen, "a catalogue", |seen| {
        named(seen).is_some()
    })
    .await;
    let blob = staging::session_service_client(socket.clone())
        .get_blob(Request::new(staging::GetBlobRequest {
            agent_id: agent.agent_id.clone(),
            hash: named(&seen).unwrap(),
        }))
        .await
        .unwrap()
        .into_inner();
    let offered = Catalogue::decode(blob.bytes.as_slice()).unwrap();

    drop(subscription);
    kill_all(&runtime).await;
    drop((socket, runtime));
    daemon.shutdown().await.unwrap();
    let names = values(&offered.commands, |command| &command.name);
    assert!(
        names.contains(&"deploy"),
        "the project's own command is not offered; the catalogue was gathered outside the project: {names:?}"
    );
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
