//! What a provider offers on a host with no agent running: the daemon runs
//! `amux catalogue <provider>` against the scripted fakes, keeps a copy per
//! provider, asks again when the provider's version changes, says on the
//! host's fleet entry whether each is signed in, answers a paired host's
//! clients through it, and hands Claude's to terminal Claude agents.

mod support;

use std::path::PathBuf;

use prost::Message as _;
use provider_fakes::script::{INPUT_LOG_ENV, OfferedCommand, OfferedModel, SCRIPT_ENV, Script};
use sha2::{Digest as _, Sha256};
use support::synthetic::*;
use support::*;
use wire::agent_service_server::AgentService as _;
use wire::{
    Catalogue, GetBlobRequest, GetCatalogueRequest, GetCatalogueResponse, ProviderOnHost,
    inventory_event,
};

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
        let script_path = install.path("providers.json");
        let log = install.path("providers.log");
        let host = Self {
            install,
            script: script_path,
            log,
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

fn by_host(host: uuid::Uuid, provider: wire::Provider) -> tonic::Request<GetCatalogueRequest> {
    tonic::Request::new(GetCatalogueRequest {
        host_id: host.as_bytes().to_vec(),
        provider: provider as i32,
    })
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

async fn ask(api: &node::ClientApi, host: uuid::Uuid, provider: wire::Provider) -> Asked {
    let blob = api
        .get_catalogue(by_host(host, provider))
        .await
        .unwrap_or_else(|error| panic!("{provider:?} on the host: {error:?}"))
        .into_inner();
    let catalogue = Catalogue::decode(blob.bytes.as_slice()).unwrap();
    Asked { blob, catalogue }
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
        commands: vec![command("review")],
        ..Script::default()
    });
    let daemon = host.install.start("boot-1", host.launch()).await;
    let runtime = runtime(&daemon, &host.install);
    let api = node::ClientApi::new(&runtime, None);
    let here = runtime.host();
    assert!(
        runtime.host_entry().providers.is_empty(),
        "nothing asked yet"
    );

    let claude = ask(&api, here, wire::Provider::Claude).await;
    assert_eq!(values(&claude.models, |m| &m.value), ["opus", "haiku"]);
    assert_eq!(claude.models[0].efforts, ["low", "high"]);
    assert_eq!(values(&claude.commands, |c| &c.name), ["review"]);
    assert_eq!(
        values(&claude.permissions, |p| &p.value),
        ["default", "acceptEdits", "plan", "bypassPermissions"],
        "no scripted model takes auto, and a new agent can be created allowing never-ask"
    );
    assert!(claude.permissions.iter().all(|p| p.settable));
    assert!(claude.modes.is_empty());
    assert_eq!(
        claude.blob.hash,
        Sha256::digest(&claude.blob.bytes).to_vec(),
        "the hash names exactly the bytes served"
    );

    let codex = ask(&api, here, wire::Provider::Codex).await;
    assert_eq!(values(&codex.models, |m| &m.value), ["opus", "haiku"]);
    // The commands amux runs for Codex, then its skills; the scripted
    // `review` skill is named like the command, which `/review` runs.
    assert_eq!(
        values(&codex.commands, |c| &c.name),
        ["compact", "clear", "review", "init", "plan"]
    );
    assert_eq!(
        values(&codex.permissions, |p| &p.value),
        ["read-only", "default", "auto", "full-access"]
    );
    assert_eq!(values(&codex.modes, |m| &m.value), ["default", "plan"]);

    // A second ask answers from the copy: neither provider runs again.
    assert_eq!(ask(&api, here, wire::Provider::Claude).await, claude);
    assert_eq!(ask(&api, here, wire::Provider::Codex).await, codex);
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
        let unknown = api
            .get_catalogue(tonic::Request::new(GetCatalogueRequest {
                host_id: here.as_bytes().to_vec(),
                provider,
            }))
            .await
            .unwrap_err();
        assert_eq!(unknown.code(), tonic::Code::InvalidArgument, "{unknown:?}");
    }

    // The copies outlive the daemon.
    drop((inventory, api, runtime));
    daemon.shutdown().await.unwrap();
    let daemon = host.install.start("boot-2", host.launch()).await;
    let runtime = support::runtime(&daemon, &host.install);
    assert_eq!(runtime.host_entry().providers, expected);
    let api = node::ClientApi::new(&runtime, None);
    assert_eq!(ask(&api, here, wire::Provider::Codex).await, codex);
    assert_eq!(host.asked(CODEX_ASKED), 1, "the kept copy answers");
    drop((api, runtime));
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
    let daemon = host.install.start("boot-1", launch).await;
    let runtime = runtime(&daemon, &host.install);
    let api = node::ClientApi::new(&runtime, None);
    let here = runtime.host();

    let claude = ask(&api, here, wire::Provider::Claude).await;
    let codex = ask(&api, here, wire::Provider::Codex).await;
    assert_eq!(
        runtime.host_entry().providers,
        [
            on_host(wire::Provider::Claude, &claude, false),
            on_host(wire::Provider::Codex, &codex, false)
        ],
        "both read signed out"
    );
    assert!(!codex.models.is_empty(), "a signed-out Codex still lists");

    // Signing in shows at the next ask.
    host.play(&Script::default());
    let codex = ask(&api, here, wire::Provider::Codex).await;
    assert_eq!(host.asked(CODEX_ASKED), 2);
    let entry = runtime.host_entry();
    assert!(
        entry
            .providers
            .contains(&on_host(wire::Provider::Codex, &codex, true)),
        "{:?}",
        entry.providers
    );

    drop((api, runtime));
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
    let daemon = host.install.start("boot-1", launch).await;
    let runtime = runtime(&daemon, &host.install);
    let api = node::ClientApi::new(&runtime, None);
    let here = runtime.host();

    let first = ask(&api, here, wire::Provider::Claude).await;
    assert_eq!(ask(&api, here, wire::Provider::Claude).await, first);
    assert_eq!(
        host.asked(CLAUDE_ASKED),
        1,
        "the same version keeps the copy"
    );
    assert_eq!(
        runtime
            .held_host_catalogue(node::Provider::Claude)
            .unwrap()
            .provider_version,
        "2.1.300 (Claude Code)"
    );

    host.play(&Script {
        version: Some("2.1.301".into()),
        models: vec![model("opus"), model("fable")],
        ..Script::default()
    });
    let second = ask(&api, here, wire::Provider::Claude).await;
    assert_eq!(host.asked(CLAUDE_ASKED), 2);
    assert_eq!(values(&second.models, |m| &m.value), ["opus", "fable"]);
    assert_ne!(second.blob.hash, first.blob.hash);
    assert!(runtime.host_entry().providers.contains(&on_host(
        wire::Provider::Claude,
        &second,
        true
    )));

    drop((api, runtime));
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paired_host_answers_for_its_peers_providers() {
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
    let desk_daemon = desk.install.start("boot-1", desk.launch()).await;
    let desk_runtime = runtime(&desk_daemon, &desk.install);
    let laptop = Install::new();
    let laptop_daemon = laptop.start("boot-1", quiet_launch()).await;
    let laptop_runtime = runtime(&laptop_daemon, &laptop);
    let (desk_edge, laptop_edge) = (desk_runtime.edge().unwrap(), laptop_runtime.edge().unwrap());
    desk_edge.trust(&laptop_edge).await.unwrap();
    laptop_edge.trust(&desk_edge).await.unwrap();
    let _link = laptop_edge.link_in_process(&desk_edge).unwrap();
    assert!(
        laptop_edge
            .wait_for_route(desk_runtime.host(), PATIENCE)
            .await
    );

    let on_laptop = node::ClientApi::new(&laptop_runtime, None);
    let desk_host = desk_runtime.host();
    let codex = ask(&on_laptop, desk_host, wire::Provider::Codex).await;
    assert_eq!(values(&codex.models, |m| &m.value), ["gpt-desk"]);
    let on_desk = node::ClientApi::new(&desk_runtime, None);
    assert_eq!(ask(&on_desk, desk_host, wire::Provider::Codex).await, codex);
    assert_eq!(desk.asked(CODEX_ASKED), 1, "the desk asked once, for both");

    // The laptop's fleet list carries the desk's word for its providers.
    let expected = vec![on_host(wire::Provider::Codex, &codex, false)];
    until("the laptop to list the desk's providers", || {
        let listed = laptop_runtime
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

    drop((on_laptop, on_desk, desk_edge, laptop_edge));
    drop((desk_runtime, laptop_runtime));
    desk_daemon.shutdown().await.unwrap();
    laptop_daemon.shutdown().await.unwrap();
}

// Unix only: Windows does not host terminal Claude (see docs/ARCHITECTURE.md,
// "Windows, as a stated cost").
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn terminal_claude_offers_what_its_hosts_claude_offers() {
    use wire::{CreateAgentRequest, Kind, session_event};

    let host = Host::new(&Script {
        models: vec![model("opus"), model("haiku")],
        commands: vec![command("review")],
        ..Script::default()
    });
    let daemon = host.install.start("boot-1", host.launch()).await;
    let runtime = runtime(&daemon, &host.install);
    let api = node::ClientApi::new(&runtime, None);

    let agent = runtime
        .spawn(
            CreateAgentRequest {
                agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                name: Some("terminal".into()),
                cwd: host.install.work.to_string_lossy().into_owned(),
                agent: wire::create_agent_request::Agent::defaults(Kind::ClaudePty),
                ..CreateAgentRequest::default()
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
    let blob = api
        .get_blob(tonic::Request::new(GetBlobRequest {
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
    drop((api, runtime));
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

    use wire::{CreateAgentRequest, Kind, session_event};

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
    let daemon = host.install.start("boot-1", launch).await;
    let runtime = runtime(&daemon, &host.install);
    let api = node::ClientApi::new(&runtime, None);

    let agent = runtime
        .spawn(
            CreateAgentRequest {
                agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                name: Some("terminal".into()),
                cwd: project.to_string_lossy().into_owned(),
                agent: wire::create_agent_request::Agent::defaults(Kind::ClaudePty),
                ..CreateAgentRequest::default()
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
    let blob = api
        .get_blob(tonic::Request::new(GetBlobRequest {
            agent_id: agent.agent_id.clone(),
            hash: named(&seen).unwrap(),
        }))
        .await
        .unwrap()
        .into_inner();
    let offered = Catalogue::decode(blob.bytes.as_slice()).unwrap();

    drop(subscription);
    kill_all(&runtime).await;
    drop((api, runtime));
    daemon.shutdown().await.unwrap();
    let names = values(&offered.commands, |command| &command.name);
    assert!(
        names.contains(&"deploy"),
        "the project's own command is not offered; the catalogue was gathered outside the project: {names:?}"
    );
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
    let daemon = host.install.start("boot-1", launch).await;
    let runtime = runtime(&daemon, &host.install);
    let api = node::ClientApi::new(&runtime, None);
    let claude = ask(&api, runtime.host(), wire::Provider::Claude).await;
    assert_eq!(values(&claude.models, |m| &m.value), ["opus"]);
}
