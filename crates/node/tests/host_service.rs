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
use wire::{Catalogue, GetCatalogueRequest, GetCatalogueResponse, ProviderOnHost, inventory_event};

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
            &wire::Caller::person("terminal").encode_to_vec(),
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

fn catalogue_of(provider: &str) -> GetCatalogueRequest {
    GetCatalogueRequest {
        provider: provider.to_owned(),
    }
}

async fn ask_with(request: Request<GetCatalogueRequest>, channel: &Channel) -> Asked {
    let provider = request.get_ref().provider.clone();
    let blob = wire::host_service_client(channel.clone())
        .get_catalogue(request)
        .await
        .unwrap_or_else(|error| panic!("provider {provider} on the host: {error:?}"))
        .into_inner();
    let catalogue = Catalogue::decode(blob.bytes.as_slice()).unwrap();
    Asked { blob, catalogue }
}

/// GetCatalogue for `provider`, asked of the host `channel` reaches.
async fn ask(channel: &Channel, provider: &str) -> Asked {
    ask_with(Request::new(catalogue_of(provider)), channel).await
}

/// Waits until the host's entry lists `provider` as `holds` wants, and
/// returns the hash it names.
async fn until_listed(
    runtime: &node::ProfileRuntime,
    provider: &str,
    holds: impl Fn(&ProviderOnHost) -> bool,
) -> Vec<u8> {
    until(&format!("the entry to list {provider} anew"), || {
        let entry = runtime.host_entry();
        let offered = entry.provider(provider).cloned();
        let holds = offered.as_ref().is_some_and(&holds);
        async move {
            match offered {
                Some(offered) if holds => offered.catalogue.ok_or("no copy".to_owned()),
                other => Err(format!("{other:?}")),
            }
        }
    })
    .await
    .unwrap()
}

/// GetCatalogue for `provider`, which must answer the copy `hash` names.
async fn ask_for(channel: &Channel, provider: &str, hash: &[u8]) -> Asked {
    let asked = ask(channel, provider).await;
    assert_eq!(asked.blob.hash, hash, "the copy the entry names");
    asked
}

fn values<T>(listed: &[T], value: impl Fn(&T) -> &str) -> Vec<&str> {
    listed.iter().map(value).collect()
}

/// Claude's or Codex's entry on a host whose Claude runs headless, as
/// [`listed`] reads it.
fn on_host(provider: &str, asked: &Asked, signed_in: bool) -> ProviderOnHost {
    let (name, kind, hint) = match provider {
        "claude" => (
            "Claude",
            wire::Kind::ClaudeSdk,
            "Run claude and sign in with /login.",
        ),
        _ => ("Codex", wire::Kind::Codex, "Run codex login."),
    };
    ProviderOnHost {
        provider: provider.to_owned(),
        catalogue: Some(asked.blob.hash.clone()),
        signed_in,
        descriptor: Some(wire::ProviderDescriptor {
            name: name.to_owned(),
            kind: kind as i32,
            sign_in_hint: hint.to_owned(),
            version: String::new(),
        }),
        kinds: node::own_kinds(provider)
            .iter()
            .map(|kind| wire::KindOnHost {
                kind: *kind as i32,
                capabilities: Some(match kind {
                    wire::Kind::ClaudePty => wire::AgentCapabilities::claude_pty(),
                    wire::Kind::ClaudeSdk => wire::AgentCapabilities::claude_sdk(),
                    _ => wire::AgentCapabilities::codex(),
                }),
            })
            .collect(),
    }
}

/// Claude's or Codex's entry before the host has asked it: named, but with
/// no catalogue and nothing said of its sign-in.
fn unasked(provider: &str) -> ProviderOnHost {
    let mut entry = on_host(
        provider,
        &Asked {
            blob: GetCatalogueResponse::default(),
            catalogue: Catalogue::default(),
        },
        false,
    );
    entry.catalogue = None;
    entry
}

/// A host's providers as it listed them, each asked one's version checked
/// as given and then left out, since the fakes' versions are their own.
fn listed(providers: Vec<ProviderOnHost>) -> Vec<ProviderOnHost> {
    providers
        .into_iter()
        .map(|mut provider| {
            let asked = provider.catalogue.is_some();
            let descriptor = provider.descriptor.as_mut().expect("a descriptor");
            assert_eq!(!descriptor.version.is_empty(), asked, "{descriptor:?}");
            descriptor.version.clear();
            provider
        })
        .collect()
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
    assert_eq!(
        listed(runtime.host_entry().providers),
        [unasked("claude"), unasked("codex")],
        "named, and nothing asked yet"
    );

    let claude = ask(&socket, "claude").await;
    assert_eq!(values(&claude.models, |m| &m.value), ["opus", "haiku"]);
    assert_eq!(claude.models[0].efforts, ["low", "high"]);
    assert_eq!(
        claude.blob.hash,
        Sha256::digest(&claude.blob.bytes).to_vec(),
        "the hash names exactly the bytes served"
    );
    let codex = ask(&socket, "codex").await;
    assert_eq!(values(&codex.models, |m| &m.value), ["opus", "haiku"]);
    assert_eq!(values(&codex.modes, |m| &m.value), ["default", "plan"]);

    // A second ask answers from the copy: neither provider runs again.
    assert_eq!(ask(&socket, "claude").await, claude);
    assert_eq!(ask(&socket, "codex").await, codex);
    assert_eq!(host.asked(CLAUDE_ASKED), 1);
    assert_eq!(host.asked(CODEX_ASKED), 1);

    // The host's entry in the fleet list names each copy, signed in.
    let expected = vec![
        on_host("claude", &claude, true),
        on_host("codex", &codex, true),
    ];
    let mut inventory = runtime.subscribe_inventory().await.unwrap();
    let mut seen = Vec::new();
    read_inventory_until(&mut inventory, &mut seen, "this host's entry", |seen| {
        !seen.is_empty()
    })
    .await;
    match &seen[0].of {
        Some(inventory_event::Of::Host(entry)) => {
            assert_eq!(listed(entry.providers.clone()), expected)
        }
        other => panic!("the inventory opens with this host, not {other:?}"),
    }

    // A provider this host does not know, and none at all.
    for provider in ["nobody", ""] {
        let unknown = wire::host_service_client(socket.clone())
            .get_catalogue(catalogue_of(provider))
            .await
            .unwrap_err();
        assert_eq!(unknown.code(), tonic::Code::InvalidArgument, "{unknown:?}");
    }

    // The copies outlive the daemon.
    drop((inventory, socket, runtime));
    daemon.shutdown().await.unwrap();
    let daemon = serving(&host.install, "boot-2", host.launch()).await;
    let runtime = support::runtime(&daemon, &host.install);
    assert_eq!(listed(runtime.host_entry().providers), expected);
    let socket = profile_socket(&runtime).await;
    assert_eq!(ask(&socket, "codex").await, codex);
    assert_eq!(host.asked(CODEX_ASKED), 1, "the kept copy answers");
    drop((socket, runtime));
    daemon.shutdown().await.unwrap();
}

/// The daemon asks Claude and Codex what they offer as it starts, in the
/// background, so the host's entry names both copies before any client
/// asks, and a client's first ask is answered from the copy.
#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_asks_its_providers_as_it_starts() {
    let host = Host::new(&Script {
        models: vec![model("opus")],
        ..Script::default()
    });
    let mut launch = host.launch();
    launch.ask_providers_at_start = true;
    let daemon = serving(&host.install, "boot-1", launch).await;
    let runtime = runtime(&daemon, &host.install);
    let claude = until_listed(&runtime, "claude", |_| true).await;
    let codex = until_listed(&runtime, "codex", |_| true).await;
    let socket = profile_socket(&runtime).await;
    assert_eq!(
        values(&ask_for(&socket, "claude", &claude).await.models, |m| &m
            .value),
        ["opus"]
    );
    ask_for(&socket, "codex", &codex).await;
    assert_eq!(
        host.asked(CLAUDE_ASKED),
        1,
        "asked once, as the daemon started"
    );
    assert_eq!(host.asked(CODEX_ASKED), 1);
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

    let claude = ask(&socket, "claude").await;
    let codex = ask(&socket, "codex").await;
    assert_eq!(
        listed(runtime.host_entry().providers),
        [
            on_host("claude", &claude, false),
            on_host("codex", &codex, false)
        ],
        "both read signed out"
    );

    // Signing in shows once the copy is asked again, which the next ask
    // starts in the background: it is answered with the copy it has.
    host.play(&Script::default());
    assert_eq!(ask(&socket, "codex").await, codex, "the copy, at once");
    let codex = until_listed(&runtime, "codex", |offered| offered.signed_in).await;
    assert_eq!(host.asked(CODEX_ASKED), 2);
    let codex = ask_for(&socket, "codex", &codex).await;
    let entry = listed(runtime.host_entry().providers);
    assert!(entry.contains(&on_host("codex", &codex, true)), "{entry:?}");

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

    let first = ask(&socket, "claude").await;
    assert_eq!(ask(&socket, "claude").await, first);
    assert_eq!(
        host.asked(CLAUDE_ASKED),
        1,
        "the same version keeps the copy"
    );
    assert_eq!(
        runtime
            .host_catalogue("claude")
            .await
            .unwrap()
            .provider_version,
        "2.1.300 (Claude Code)"
    );

    // A new version is found by the ask that reads it, in the background:
    // that ask is answered with the copy at once, and the host's entry
    // names the new copy once the helper has answered.
    host.play(&Script {
        version: Some("2.1.301".into()),
        models: vec![model("opus"), model("fable")],
        ..Script::default()
    });
    assert_eq!(ask(&socket, "claude").await, first, "the copy, at once");
    let hash = until_listed(&runtime, "claude", |offered| {
        offered.catalogue.as_ref() != Some(&first.blob.hash)
    })
    .await;
    assert_eq!(host.asked(CLAUDE_ASKED), 2);
    let second = ask_for(&socket, "claude", &hash).await;
    assert_eq!(values(&second.models, |m| &m.value), ["opus", "fable"]);
    assert!(listed(runtime.host_entry().providers).contains(&on_host("claude", &second, true)));

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
    let claude = ask(&socket, "claude").await;
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
    let refused = wire::host_service_client(socket.clone())
        .get_catalogue(catalogue_of("codex"))
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
    let host = Host::new(&Script::default());
    let daemon = serving(&host.install, "boot-1", host.launch()).await;
    let runtime = runtime(&daemon, &host.install);
    let socket = profile_socket(&runtime).await;
    let cwd = host.install.path("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let create = |name: &str, model: Option<&str>, remember_model: bool| wire::CreateAgentRequest {
        agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
        name: Some(name.into()),
        cwd: cwd.to_string_lossy().into_owned(),
        provider: "claude".into(),
        kind: wire::Kind::ClaudeSdk as i32,
        settings: Some(wire::AgentSettings {
            model: model.map(str::to_owned),
            ..Default::default()
        }),
        remember_model,
        ..wire::CreateAgentRequest::default()
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
    let before = ask(&socket, "claude").await;

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
    let kept = ask(&socket, "claude").await;
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
                wire::CreateAgentRequest {
                    provider: "claude".into(),
                    kind: wire::Kind::ClaudePty as i32,
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
    use wire::session_event;

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
            wire::CreateAgentRequest {
                agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                name: Some("terminal".into()),
                cwd: host.install.work.to_string_lossy().into_owned(),
                provider: "claude".into(),
                kind: wire::Kind::ClaudePty as i32,
                ..wire::CreateAgentRequest::default()
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
    let blob = wire::session_service_client(socket.clone())
        .get_blob(Request::new(wire::GetBlobRequest {
            agent_id: agent.agent_id.clone(),
            hash: named(&seen).unwrap(),
        }))
        .await
        .unwrap()
        .into_inner();
    let offered = Catalogue::decode(blob.bytes.as_slice()).unwrap();
    // A folder with no project of its own offers what the host's Claude
    // offers anywhere; the agent's was asked in its folder.
    let hosts = runtime.host_catalogue("claude").await.unwrap();
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

    use wire::session_event;

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
            wire::CreateAgentRequest {
                agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                name: Some("terminal".into()),
                cwd: project.to_string_lossy().into_owned(),
                provider: "claude".into(),
                kind: wire::Kind::ClaudePty as i32,
                ..wire::CreateAgentRequest::default()
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
    let blob = wire::session_service_client(socket.clone())
        .get_blob(Request::new(wire::GetBlobRequest {
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

    let codex = ask_with(from_a_person(catalogue_of("codex")), &pair.to_desk).await;
    assert_eq!(values(&codex.models, |m| &m.value), ["gpt-desk"]);
    let on_desk = profile_socket(&pair.desk_runtime).await;
    assert_eq!(ask(&on_desk, "codex").await, codex);
    assert_eq!(
        pair.desk.asked(CODEX_ASKED),
        1,
        "the desk asked once, for both"
    );

    // The laptop's fleet list carries the desk's word for its providers.
    let desk_host = pair.desk_runtime.host();
    let expected = vec![unasked("claude"), on_host("codex", &codex, false)];
    until("the laptop to list the desk's providers", || {
        let listed = pair
            .laptop_runtime
            .host_entry_of(desk_host)
            .map(|entry| listed(entry.providers));
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

/// A host that knows one ACP agent, `mine`, run as fake-acp with the
/// arguments the built-ins use, its sessions kept in `sessions`.
fn with_acp_agent(host: &Host, sessions: &std::path::Path) -> node::Launch {
    let mut launch = host.launch();
    launch.agents = vec![node::agents::AgentProgram {
        id: "mine".into(),
        name: "Mine".into(),
        command: provider_fakes::binaries::path("fake-acp")
            .to_string_lossy()
            .into_owned(),
        args: vec!["agent".into(), "stdio".into()],
        env: [(
            provider_fakes::acp::SESSIONS_ENV.to_owned(),
            sessions.to_string_lossy().into_owned(),
        )]
        .into(),
        built_in: false,
    }];
    launch
}

/// An ACP agent's copy on the host comes from its `initialize` answer
/// alone, so asking leaves no conversation in the agent's history: its
/// version, how it reopens a conversation and no models before any
/// conversation. The copy is kept, so a second ask runs nothing. What the
/// host's own newest conversation with the agent published, its models and
/// whether it was refused as signed out, fills in what `initialize` does
/// not tell.
#[tokio::test(flavor = "multi_thread")]
async fn an_acp_agents_copy_is_its_initialize_answer_and_what_its_conversations_showed() {
    use prost::Message as _;
    let host = Host::new(&Script {
        version: Some("3.1.4".into()),
        acp: provider_fakes::script::AcpAgent {
            name: "Mine".into(),
            load: true,
            resume: false,
            ..Default::default()
        },
        ..Script::default()
    });
    let sessions = host.install.path("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let daemon = host
        .install
        .start("boot-1", with_acp_agent(&host, &sessions))
        .await;
    let runtime = runtime(&daemon, &host.install);
    assert_eq!(
        runtime
            .offered_agents()
            .iter()
            .map(|offered| offered.program.id.as_str())
            .collect::<Vec<_>>(),
        ["mine"]
    );

    let copy = runtime.agent_catalogue("mine").await.unwrap();
    assert_eq!(copy.provider_version, "3.1.4");
    let acp = copy.acp.clone().expect("what initialize said");
    assert_eq!(acp.reopen(), wire::AcpReopen::Load);
    assert!(copy.catalogue.models.is_empty(), "no session, no models");
    assert!(copy.signed_in, "never signed out on a guess");
    let sent = std::fs::read_to_string(&host.log).unwrap_or_default();
    assert!(sent.contains("\"initialize\""), "{sent}");
    assert!(!sent.contains("session/"), "no session was opened: {sent}");
    assert!(
        std::fs::read_dir(&sessions).unwrap().next().is_none(),
        "no conversation left in the agent's history"
    );
    assert_eq!(runtime.agent_catalogue("mine").await.unwrap(), copy);
    assert_eq!(
        host.asked("\"initialize\""),
        1,
        "the copy answers the second ask"
    );

    // The host's entry lists it after Claude and Codex, by the name its
    // config gives, with what an agent of it can do.
    let entry = runtime.host_entry();
    let ids: Vec<&str> = entry
        .providers
        .iter()
        .map(|p| p.provider.as_str())
        .collect();
    assert_eq!(ids, ["claude", "codex", "mine"]);
    let mine = entry.provider("mine").unwrap();
    assert_eq!(mine.catalogue.as_ref(), Some(&copy.blob.hash));
    assert!(mine.signed_in);
    let descriptor = mine.descriptor.clone().unwrap();
    assert_eq!(descriptor.name, "Mine");
    assert_eq!(descriptor.kind(), wire::Kind::Acp);
    assert_eq!(descriptor.version, "3.1.4");
    assert_eq!(
        mine.capabilities(),
        Some(&wire::AgentCapabilities::acp(true, true, false)),
        "it loads a conversation, takes images and is not known to steer"
    );

    // A conversation with it on this host published its catalogue and was
    // refused as signed out.
    let catalogue = Catalogue {
        models: vec![wire::OfferedModel {
            value: "mine-fast".into(),
            ..Default::default()
        }],
        ..Catalogue::default()
    };
    let bytes = catalogue.encode_to_vec();
    let hash = Sha256::digest(&bytes).to_vec();
    let agent = SyntheticAgent::new(&host.install, "chatter", 1 << 20);
    let blobs = agent.dir.join("blobs");
    std::fs::create_dir_all(&blobs).unwrap();
    let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    std::fs::write(blobs.join(hex), &bytes).unwrap();
    let mut row = agent.row(&host.install);
    row.kind = wire::Kind::Acp as i32;
    row.provider = "mine".into();
    row.lifecycle = wire::Lifecycle::Exited as i32;
    row.snapshot = Some(wire::Snapshot {
        kind: wire::Kind::Acp as i32,
        catalogue: Some(hash),
        at_ms: 10,
        body: wire::AcpSnapshot::default().encode_to_vec(),
        sign_in: Some(wire::SignIn {
            state: wire::SignInState::SignedOut as i32,
            ..Default::default()
        }),
        ..Default::default()
    });
    host.install.store().put_agent(&row).unwrap();

    let learned = runtime.agent_catalogue("mine").await.unwrap();
    assert_eq!(
        values(&learned.catalogue.models, |m| &m.value),
        ["mine-fast"]
    );
    assert!(!learned.signed_in);
    assert_eq!(learned.acp.unwrap().sign_in(), wire::SignInState::SignedOut);
    assert_eq!(
        host.asked("\"initialize\""),
        1,
        "learned without asking the agent"
    );

    // An agent the host does not know, or cannot find, is refused.
    assert!(matches!(
        runtime.agent_catalogue("theirs").await,
        Err(node::CatalogueError::NoAgent(_))
    ));
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// Antigravity lists its models with `agy models`, so the host's copy has
/// them before any conversation with it, and asking opens no session: the
/// helper runs the agent's program with `models` after `initialize` and
/// reads a line per model and effort as the models with their efforts.
#[tokio::test(flavor = "multi_thread")]
async fn antigravitys_copy_lists_its_models_before_any_conversation() {
    let host = Host::new(&Script {
        models: vec![model("gemini-fast"), model("gemini-deep")],
        ..Script::default()
    });
    let sessions = host.install.path("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let mut launch = with_acp_agent(&host, &sessions);
    launch.agents[0].id = "antigravity".into();
    launch.agents[0].name = "Antigravity".into();
    let daemon = host.install.start("boot-1", launch).await;
    let runtime = support::runtime(&daemon, &host.install);
    let copy = runtime.agent_catalogue("antigravity").await.unwrap();
    assert_eq!(
        values(&copy.catalogue.models, |m| &m.value),
        ["gemini-fast", "gemini-deep"]
    );
    assert_eq!(copy.catalogue.models[0].efforts, ["low", "high"]);
    let sent = std::fs::read_to_string(&host.log).unwrap_or_default();
    assert!(
        !sent.contains("session/"),
        "asking opens no session: {sent}"
    );
    assert_eq!(
        runtime
            .host_entry()
            .provider("antigravity")
            .unwrap()
            .catalogue,
        Some(copy.blob.hash.clone())
    );

    // A model named as Antigravity's own CLI spells it is the model and
    // effort the session takes; one it does not offer is refused with the
    // models it does, before any agent starts, never dropped for the
    // agent's default.
    let cwd = host.install.path("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let start = |model: &str| wire::CreateAgentRequest {
        agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
        cwd: cwd.to_string_lossy().into_owned(),
        provider: "antigravity".into(),
        kind: wire::Kind::Acp as i32,
        settings: Some(wire::AgentSettings {
            model: Some(model.into()),
            ..Default::default()
        }),
        ..wire::CreateAgentRequest::default()
    };
    let refused = runtime
        .spawn(start("gemini-9-ultra"), None)
        .await
        .unwrap_err();
    assert_eq!(refused.code(), tonic::Code::InvalidArgument, "{refused:?}");
    assert!(
        refused.message().contains("gemini-fast or gemini-deep"),
        "{refused:?}"
    );
    runtime
        .spawn(start("gemini-deep-high"), None)
        .await
        .expect("the agent starts");
    until("the session to take the model and effort", || {
        let sent = std::fs::read_to_string(&host.log).unwrap_or_default();
        let set = |value: &str| {
            sent.lines()
                .any(|line| line.contains("session/set_config_option") && line.contains(value))
        };
        let taken = set("\"gemini-deep\"") && set("\"high\"");
        async move { taken.then_some(()).ok_or("not yet set") }
    })
    .await
    .unwrap();
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// A conversation that publishes a catalogue the host's copy was not made
/// from has the copy made again at once, without asking the agent, so the
/// host's entry names the new copy and every client fetches it by the hash
/// it follows.
#[tokio::test(flavor = "multi_thread")]
async fn a_conversations_new_catalogue_republishes_the_hosts_copy() {
    use prost::Message as _;
    let host = Host::new(&Script::default());
    let sessions = host.install.path("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let mut agent = SyntheticAgent::new(&host.install, "chatter", 1 << 20);
    let mut row = agent.row(&host.install);
    row.kind = wire::Kind::Acp as i32;
    row.provider = "mine".into();
    host.install.store().put_agent(&row).unwrap();
    agent.go_live();
    let daemon = host
        .install
        .start("boot-1", with_acp_agent(&host, &sessions))
        .await;
    let runtime = support::runtime(&daemon, &host.install);
    let before = runtime.agent_catalogue("mine").await.unwrap();
    assert!(before.catalogue.models.is_empty(), "no conversation yet");
    let asked = host.asked("\"initialize\"");

    let catalogue = Catalogue {
        models: vec![wire::OfferedModel {
            value: "mine-fast".into(),
            ..Default::default()
        }],
        ..Catalogue::default()
    };
    let bytes = catalogue.encode_to_vec();
    let hash = Sha256::digest(&bytes).to_vec();
    let blobs = agent.dir.join("blobs");
    std::fs::create_dir_all(&blobs).unwrap();
    let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    std::fs::write(blobs.join(hex), &bytes).unwrap();
    agent.append(&wire::Step {
        snapshot: Some(wire::Snapshot {
            kind: wire::Kind::Acp as i32,
            status: wire::AgentStatus::in_phase(wire::Phase::Idle),
            catalogue: Some(hash),
            at_ms: 10,
            body: wire::AcpSnapshot::default().encode_to_vec(),
            ..Default::default()
        }),
        ..Default::default()
    });
    agent.nudge().await;

    let copy = until_listed(&runtime, "mine", |offered| {
        offered.catalogue.as_ref() != Some(&before.blob.hash)
    })
    .await;
    let after = runtime.agent_catalogue("mine").await.unwrap();
    assert_eq!(after.blob.hash, copy);
    assert_eq!(values(&after.catalogue.models, |m| &m.value), ["mine-fast"]);
    assert_eq!(
        host.asked("\"initialize\""),
        asked,
        "made again without asking the agent"
    );
    agent.die().await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// Whether `seen` holds an entry for `host` that lists `provider` with a
/// copy.
fn lists(seen: &[wire::InventoryEvent], host: uuid::Uuid, provider: &str) -> bool {
    seen.iter().any(|event| match &event.of {
        Some(inventory_event::Of::Host(entry)) => {
            entry.host_id() == host.as_bytes()
                && entry
                    .provider(provider)
                    .is_some_and(|offered| offered.catalogue.is_some())
        }
        _ => false,
    })
}

/// A copy of an ACP agent's offer the profile kept is read again when the
/// host next starts and is asked for, without asking the agent; it is new
/// to the host's entry, so the entry is published with it, as it is for a
/// fresh ask.
#[tokio::test(flavor = "multi_thread")]
async fn an_acp_agents_kept_copy_read_at_a_restart_is_published() {
    let host = Host::new(&Script {
        version: Some("2.0.0".into()),
        ..Script::default()
    });
    let sessions = host.install.path("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let with_mine = with_acp_agent(&host, &sessions);
    let daemon = host.install.start("boot-1", with_mine.clone()).await;
    let runtime = support::runtime(&daemon, &host.install);
    let copy = runtime.agent_catalogue("mine").await.unwrap();
    assert_eq!(host.asked("\"initialize\""), 1);
    drop(runtime);
    daemon.shutdown().await.unwrap();

    // Started knowing no agent, so nothing reads the copy before the
    // inventory is open; then the config names it again.
    let daemon = host.install.start("boot-2", host.launch()).await;
    let runtime = support::runtime(&daemon, &host.install);
    let here = runtime.host();
    let mut inventory = runtime.subscribe_inventory().await.unwrap();
    let mut seen = Vec::new();
    read_inventory_until(&mut inventory, &mut seen, "the opening", |seen| {
        seen.iter()
            .any(|event| matches!(event.of, Some(inventory_event::Of::CaughtUp(_))))
    })
    .await;
    assert!(!lists(&seen, here, "mine"), "not offered yet");
    runtime.set_launch(with_mine);
    assert_eq!(runtime.agent_catalogue("mine").await.unwrap(), copy);
    assert_eq!(host.asked("\"initialize\""), 1, "the kept copy answers");
    read_inventory_until(
        &mut inventory,
        &mut seen,
        "the entry listing the kept copy",
        |seen| lists(seen, here, "mine"),
    )
    .await;
    drop((inventory, runtime));
    daemon.shutdown().await.unwrap();
}

/// A kept copy belongs to the program the config named when it was asked:
/// once the agent's command, arguments or environment change, the agent is
/// asked again, and a copy asked of the same program still answers.
#[tokio::test(flavor = "multi_thread")]
async fn an_acp_agents_copy_is_asked_again_when_its_launch_changes() {
    let host = Host::new(&Script::default());
    let sessions = host.install.path("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let launch = with_acp_agent(&host, &sessions);
    let daemon = host.install.start("boot-1", launch.clone()).await;
    let runtime = support::runtime(&daemon, &host.install);
    runtime.agent_catalogue("mine").await.unwrap();
    let asked = host.asked("\"initialize\"");
    runtime.agent_catalogue("mine").await.unwrap();
    assert_eq!(
        host.asked("\"initialize\""),
        asked,
        "the same program: the copy"
    );

    let mut changed = launch.clone();
    changed.agents[0]
        .env
        .insert("MINE_PROFILE".into(), "work".into());
    runtime.set_launch(changed.clone());
    runtime.agent_catalogue("mine").await.unwrap();
    assert_eq!(host.asked("\"initialize\""), asked + 1, "a new environment");

    let mut changed = changed;
    changed.agents[0].args.push("--verbose".into());
    runtime.set_launch(changed.clone());
    runtime.agent_catalogue("mine").await.unwrap();
    assert_eq!(host.asked("\"initialize\""), asked + 2, "new arguments");
    runtime.agent_catalogue("mine").await.unwrap();
    assert_eq!(
        host.asked("\"initialize\""),
        asked + 2,
        "then the copy again"
    );
    drop(runtime);
    daemon.shutdown().await.unwrap();

    // The copy on disk says what it was asked with too.
    let daemon = host.install.start("boot-2", launch).await;
    let runtime = support::runtime(&daemon, &host.install);
    runtime.agent_catalogue("mine").await.unwrap();
    assert_eq!(
        host.asked("\"initialize\""),
        asked + 3,
        "the kept copy was asked with other arguments"
    );
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// A model or effort a person picks in an ACP agent's chat is what that
/// agent's later agents start with once the agent took it, and only then:
/// a pick it is refused changes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn an_acp_agents_pick_is_kept_once_the_agent_took_it() {
    let host = Host::new(&Script {
        models: vec![model("fast"), model("slow")],
        ..Script::default()
    });
    let sessions = host.install.path("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let daemon = host
        .install
        .start("boot-1", with_acp_agent(&host, &sessions))
        .await;
    let runtime = support::runtime(&daemon, &host.install);
    let cwd = host.install.path("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let agent = runtime
        .spawn(
            wire::CreateAgentRequest {
                agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                name: Some("chat".into()),
                cwd: cwd.to_string_lossy().into_owned(),
                provider: "mine".into(),
                kind: wire::Kind::Acp as i32,
                ..wire::CreateAgentRequest::default()
            },
            None,
        )
        .await
        .expect("the agent starts");
    let key = store::AgentKey::new(agent.host_id.clone(), agent.agent_id.clone());
    until("the session to open", || {
        let key = key.clone();
        let runtime = runtime.clone();
        async move {
            let row = runtime
                .read_store(move |store| store.agent(&key).unwrap())
                .await
                .unwrap()
                .ok_or("no row")?;
            (row.phase() == wire::Phase::Idle)
                .then_some(())
                .ok_or(format!("phase {:?}", row.phase()))
        }
    })
    .await
    .unwrap();
    let send = |of: wire::acp_input::Of, accepted: bool| {
        let runtime = runtime.clone();
        let agent_id = agent.agent_id.clone();
        async move {
            let verdict = runtime
                .send_input(&wire::SendInputRequest {
                    agent_id,
                    input: Some(wire::Input::new(
                        uuid::Uuid::new_v4().as_bytes().to_vec(),
                        wire::AcpInput { of: Some(of) },
                    )),
                })
                .await
                .unwrap();
            assert_eq!(
                matches!(verdict.of, Some(wire::send_input_response::Of::Accepted(_))),
                accepted,
                "{verdict:?}"
            );
        }
    };
    let kept = || runtime.launch().new_agent.of("mine");
    let until_kept = |what: &'static str, holds: fn(&settings::NewAgentDefaults) -> bool| {
        let kept = &kept;
        async move {
            until(what, || async move {
                let now = kept();
                holds(&now).then_some(()).ok_or(format!("{now:?}"))
            })
            .await
            .unwrap();
        }
    };

    send(
        wire::acp_input::Of::Model(wire::SetModel {
            model: Some("slow".into()),
        }),
        true,
    )
    .await;
    until_kept("the model the agent took", |kept| {
        kept.model.as_deref() == Some("slow")
    })
    .await;

    // A model the session does not offer is refused, so nothing moves; an
    // effort it offers is taken and kept after it. (A pick the agent itself
    // refuses is never said taken: the interpreter's own tests show it.)
    send(
        wire::acp_input::Of::Model(wire::SetModel {
            model: Some("nope".into()),
        }),
        false,
    )
    .await;
    send(
        wire::acp_input::Of::Effort(wire::SetEffort {
            effort: Some("high".into()),
        }),
        true,
    )
    .await;
    until_kept("the effort the agent took", |kept| {
        kept.effort.as_deref() == Some("high")
    })
    .await;
    assert_eq!(
        kept().model.as_deref(),
        Some("slow"),
        "the refused pick is not kept"
    );
    // An agent outlives its daemon by design; one left running would
    // outlive the test too.
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// Whether the newest entry in `seen` for `host` says `provider` is signed
/// in.
fn says_signed_in(seen: &[wire::InventoryEvent], host: uuid::Uuid, provider: &str) -> Option<bool> {
    seen.iter().rev().find_map(|event| match &event.of {
        Some(inventory_event::Of::Host(entry)) if entry.host_id() == host.as_bytes() => entry
            .provider(provider)
            .and_then(wire::ProviderOnHost::signed_in),
        _ => None,
    })
}

/// An ACP agent that will not open its session until the person signs in
/// stays, signed out, holding what is sent; the host's entry says the agent
/// is signed out as soon as it is refused. Once the person has signed in, a
/// prompt opens the session, what waited runs in order, and the entry says
/// it is signed in again.
#[tokio::test(flavor = "multi_thread")]
async fn a_signed_out_acp_agent_waits_for_a_sign_in_and_the_entry_follows() {
    use provider_fakes::script::Step;
    let signed_in = tempfile::tempdir().unwrap();
    let signed_in = signed_in.path().join("signed-in");
    let host = Host::new(&Script {
        steps: vec![
            text("First answer."),
            Step::TurnEnd,
            text("Second answer."),
            Step::TurnEnd,
        ],
        acp: provider_fakes::script::AcpAgent {
            signed_in_at: Some(signed_in.clone()),
            ..Default::default()
        },
        ..Script::default()
    });
    let sessions = host.install.path("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let daemon = host
        .install
        .start("boot-1", with_acp_agent(&host, &sessions))
        .await;
    let runtime = support::runtime(&daemon, &host.install);
    let here = runtime.host();
    assert!(
        runtime.agent_catalogue("mine").await.unwrap().signed_in,
        "initialize cannot tell, and nothing is called signed out on a guess"
    );
    let mut inventory = runtime.subscribe_inventory().await.unwrap();
    let mut seen = Vec::new();
    read_inventory_until(&mut inventory, &mut seen, "the opening", |seen| {
        seen.iter()
            .any(|event| matches!(event.of, Some(inventory_event::Of::CaughtUp(_))))
    })
    .await;

    let cwd = host.install.path("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let agent = runtime
        .spawn(
            wire::CreateAgentRequest {
                agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                name: Some("chat".into()),
                cwd: cwd.to_string_lossy().into_owned(),
                provider: "mine".into(),
                kind: wire::Kind::Acp as i32,
                ..wire::CreateAgentRequest::default()
            },
            None,
        )
        .await
        .expect("the agent starts");
    let key = store::AgentKey::new(agent.host_id.clone(), agent.agent_id.clone());
    let row_holds = |what: &'static str, holds: fn(&store::AgentRow, &wire::Snapshot) -> bool| {
        let key = key.clone();
        let runtime = runtime.clone();
        async move {
            until(what, || {
                let key = key.clone();
                let runtime = runtime.clone();
                async move {
                    let row = runtime
                        .read_store(move |store| store.agent(&key).unwrap())
                        .await
                        .unwrap()
                        .ok_or("no row")?;
                    let snapshot = row.snapshot.clone().unwrap_or_default();
                    holds(&row, &snapshot).then_some(()).ok_or(format!(
                        "phase {:?} lifecycle {} sign-in {:?}",
                        row.phase(),
                        row.lifecycle,
                        snapshot.sign_in
                    ))
                }
            })
            .await
            .unwrap();
        }
    };
    let signed_out = |row: &store::AgentRow, snapshot: &wire::Snapshot| {
        row.lifecycle == wire::Lifecycle::Live as i32
            && row.phase() == wire::Phase::Idle
            && snapshot.sign_in.as_ref().map(wire::SignIn::state)
                == Some(wire::SignInState::SignedOut)
    };
    row_holds("the agent waiting, signed out", signed_out).await;
    read_inventory_until(
        &mut inventory,
        &mut seen,
        "the entry saying signed out",
        |seen| says_signed_in(seen, here, "mine") == Some(false),
    )
    .await;

    let prompt = |text: &str| {
        let runtime = runtime.clone();
        let agent_id = agent.agent_id.clone();
        let text = text.to_owned();
        async move {
            let verdict = runtime
                .send_input(&wire::SendInputRequest {
                    agent_id,
                    input: Some(wire::Input::new(
                        uuid::Uuid::new_v4().as_bytes().to_vec(),
                        wire::AcpInput {
                            of: Some(wire::acp_input::Of::Prompt(wire::PromptInput {
                                text,
                                attachments: Vec::new(),
                            })),
                        },
                    )),
                })
                .await
                .unwrap();
            assert!(
                matches!(verdict.of, Some(wire::send_input_response::Of::Accepted(_))),
                "{verdict:?}"
            );
        }
    };
    // Still signed out: the prompt asks again, is refused, and waits.
    prompt("First question.").await;
    until("the agent asked again", || async {
        let asked = host.asked("\"session/new\"");
        (asked >= 2).then_some(()).ok_or(format!("{asked} asks"))
    })
    .await
    .unwrap();
    row_holds("the agent still waiting, signed out", signed_out).await;

    // Signed in now: the next prompt opens the session, and both run.
    std::fs::write(&signed_in, "").unwrap();
    prompt("Second question.").await;
    let answered = |row: &store::AgentRow, snapshot: &wire::Snapshot| {
        row.lifecycle == wire::Lifecycle::Live as i32
            && row.phase() == wire::Phase::Idle
            && snapshot.sign_in.as_ref().map(wire::SignIn::state)
                == Some(wire::SignInState::SignedIn)
            && snapshot.queue.is_empty()
    };
    row_holds("the session open and the queue drained", answered).await;
    let said = {
        let key = key.clone();
        runtime
            .read_store(move |store| {
                store
                    .last_n(&key, 50)
                    .unwrap()
                    .into_iter()
                    .map(|item| item.text)
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap()
    };
    let at = |text: &str| said.iter().position(|said| said == text);
    assert!(
        at("First answer.") < at("Second answer.") && at("Second answer.").is_some(),
        "{said:?}"
    );
    read_inventory_until(
        &mut inventory,
        &mut seen,
        "the entry saying signed in",
        |seen| says_signed_in(seen, here, "mine") == Some(true),
    )
    .await;
    // An agent outlives its daemon by design; one left running would
    // outlive the test too.
    kill_all(&runtime).await;
    drop((inventory, runtime));
    daemon.shutdown().await.unwrap();
}
