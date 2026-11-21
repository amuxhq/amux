//! What a provider offers on a host with no agent running: the daemon runs
//! `amux catalogue <provider>` against the scripted fakes, keeps a copy per
//! provider, asks again when the provider's version changes, says on the
//! host's fleet entry whether each is signed in, answers a paired host's
//! clients through it, and hands Claude's to terminal Claude agents.

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

fn by_host(host: uuid::Uuid, provider: &str) -> tonic::Request<GetCatalogueRequest> {
    tonic::Request::new(GetCatalogueRequest {
        host_id: host.as_bytes().to_vec(),
        provider_id: provider.to_owned(),
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

async fn ask(api: &node::ClientApi, host: uuid::Uuid, provider: &str) -> Asked {
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

/// Claude's or Codex's entry on a host whose Claude runs headless, as
/// [`listed`] reads it.
fn on_host(provider: &str, asked: &Asked, signed_in: bool) -> ProviderOnHost {
    let (name, kind, hint, capabilities) = match provider {
        "claude" => (
            "Claude",
            wire::Kind::ClaudeSdk,
            "Run claude and sign in with /login.",
            wire::AgentCapabilities::claude_sdk(),
        ),
        _ => (
            "Codex",
            wire::Kind::Codex,
            "Run codex login.",
            wire::AgentCapabilities::codex(),
        ),
    };
    ProviderOnHost {
        provider_id: provider.to_owned(),
        catalogue: Some(asked.blob.hash.clone()),
        signed_in,
        descriptor: Some(wire::ProviderDescriptor {
            name: name.to_owned(),
            kind: kind as i32,
            sign_in_hint: hint.to_owned(),
            version: String::new(),
        }),
        capabilities: Some(capabilities),
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
        commands: vec![command("review")],
        ..Script::default()
    });
    let daemon = host.install.start("boot-1", host.launch()).await;
    let runtime = runtime(&daemon, &host.install);
    let api = node::ClientApi::new(&runtime, None);
    let here = runtime.host();
    assert_eq!(
        listed(runtime.host_entry().providers),
        [unasked("claude"), unasked("codex")],
        "named, and nothing asked yet"
    );

    let claude = ask(&api, here, "claude").await;
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

    let codex = ask(&api, here, "codex").await;
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
    assert!(
        codex.new_agent.is_some() && claude.new_agent.is_some(),
        "each provider says what a new agent would start with"
    );

    // A second ask answers from the copy: neither provider runs again.
    assert_eq!(ask(&api, here, "claude").await, claude);
    assert_eq!(ask(&api, here, "codex").await, codex);
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
        let unknown = api
            .get_catalogue(tonic::Request::new(GetCatalogueRequest {
                host_id: here.as_bytes().to_vec(),
                provider_id: provider.to_owned(),
            }))
            .await
            .unwrap_err();
        assert_eq!(unknown.code(), tonic::Code::InvalidArgument, "{unknown:?}");
    }
    // A released build's request names Claude by the enum it had at tag 2,
    // which this build reads as an unknown field: the request still decodes,
    // and the host says in words why it answers nothing.
    let mut released = GetCatalogueRequest {
        host_id: here.as_bytes().to_vec(),
        provider_id: String::new(),
    }
    .encode_to_vec();
    released.extend([0x10, 0x01]);
    let released = GetCatalogueRequest::decode(released.as_slice()).unwrap();
    let refused = api
        .get_catalogue(tonic::Request::new(released))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), tonic::Code::InvalidArgument);
    assert!(
        refused.message().contains("names no provider"),
        "{refused:?}"
    );

    // The copies outlive the daemon.
    drop((inventory, api, runtime));
    daemon.shutdown().await.unwrap();
    let daemon = host.install.start("boot-2", host.launch()).await;
    let runtime = support::runtime(&daemon, &host.install);
    assert_eq!(listed(runtime.host_entry().providers), expected);
    let api = node::ClientApi::new(&runtime, None);
    assert_eq!(ask(&api, here, "codex").await, codex);
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

    let claude = ask(&api, here, "claude").await;
    let codex = ask(&api, here, "codex").await;
    assert_eq!(
        listed(runtime.host_entry().providers),
        [
            on_host("claude", &claude, false),
            on_host("codex", &codex, false)
        ],
        "both read signed out"
    );
    assert!(!codex.models.is_empty(), "a signed-out Codex still lists");

    // Signing in shows at the next ask.
    host.play(&Script::default());
    let codex = ask(&api, here, "codex").await;
    assert_eq!(host.asked(CODEX_ASKED), 2);
    let entry = listed(runtime.host_entry().providers);
    assert!(entry.contains(&on_host("codex", &codex, true)), "{entry:?}");

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

    let first = ask(&api, here, "claude").await;
    assert_eq!(ask(&api, here, "claude").await, first);
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

    host.play(&Script {
        version: Some("2.1.301".into()),
        models: vec![model("opus"), model("fable")],
        ..Script::default()
    });
    let second = ask(&api, here, "claude").await;
    assert_eq!(host.asked(CLAUDE_ASKED), 2);
    assert_eq!(values(&second.models, |m| &m.value), ["opus", "fable"]);
    assert_ne!(second.blob.hash, first.blob.hash);
    assert!(listed(runtime.host_entry().providers).contains(&on_host("claude", &second, true)));

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
    assert!(wait_for_route(&laptop_edge, desk_runtime.host(), PATIENCE).await);

    let on_laptop = node::ClientApi::new(&laptop_runtime, None);
    let desk_host = desk_runtime.host();
    let codex = ask(&on_laptop, desk_host, "codex").await;
    assert_eq!(values(&codex.models, |m| &m.value), ["gpt-desk"]);
    let on_desk = node::ClientApi::new(&desk_runtime, None);
    assert_eq!(ask(&on_desk, desk_host, "codex").await, codex);
    assert_eq!(desk.asked(CODEX_ASKED), 1, "the desk asked once, for both");

    // The laptop's fleet list carries the desk's word for its providers.
    let expected = vec![unasked("claude"), on_host("codex", &codex, false)];
    until("the laptop to list the desk's providers", || {
        let listed = laptop_runtime
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
    let claude = ask(&api, runtime.host(), "claude").await;
    assert_eq!(values(&claude.models, |m| &m.value), ["opus"]);
}

/// A person's pick is what later agents start with, in every client's
/// catalogue and in the agent's own launch.
#[tokio::test(flavor = "multi_thread")]
async fn a_persons_pick_is_what_later_agents_start_with() {
    use wire::create_agent_request::Agent;
    use wire::{ClaudeCreateConfig, CreateAgentRequest};

    let host = Host::new(&Script::default());
    let daemon = host.install.start("boot-1", host.launch()).await;
    let runtime = runtime(&daemon, &host.install);
    let api = node::ClientApi::new(&runtime, None);
    let here = runtime.host();
    let cwd = host.install.path("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let create = |name: &str, model: Option<&str>, remember_model: bool| CreateAgentRequest {
        agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
        name: Some(name.into()),
        cwd: cwd.to_string_lossy().into_owned(),
        agent: Some(Agent::ClaudeSdk(ClaudeCreateConfig {
            model: model.map(str::to_owned),
            ..Default::default()
        })),
        remember_model,
        ..CreateAgentRequest::default()
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
    let before = ask(&api, here, "claude").await;

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
    let kept = ask(&api, here, "claude").await;
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
                CreateAgentRequest {
                    agent: Agent::defaults(wire::Kind::ClaudePty),
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
    daemon.shutdown().await.unwrap();
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
        .map(|p| p.provider_id.as_str())
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
        mine.capabilities,
        Some(wire::AgentCapabilities::acp(true, true, false)),
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
    row.lifecycle = wire::Lifecycle::Exited as i32;
    row.snapshot = Some(wire::Snapshot {
        kind: wire::Kind::Acp as i32,
        catalogue: Some(hash),
        at_ms: 10,
        body: wire::AcpSnapshot {
            agent: Some("mine".into()),
            sign_in: Some(wire::SignIn {
                state: wire::SignInState::SignedOut as i32,
                ..Default::default()
            }),
            ..Default::default()
        }
        .encode_to_vec(),
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
                agent: Some(wire::create_agent_request::Agent::Acp(
                    wire::AcpCreateConfig {
                        agent: "mine".into(),
                        ..Default::default()
                    },
                )),
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
            (row.phase == wire::Phase::Idle as i32)
                .then_some(())
                .ok_or(format!("phase {}", row.phase))
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
                agent: Some(wire::create_agent_request::Agent::Acp(
                    wire::AcpCreateConfig {
                        agent: "mine".into(),
                        ..Default::default()
                    },
                )),
                ..wire::CreateAgentRequest::default()
            },
            None,
        )
        .await
        .expect("the agent starts");
    let key = store::AgentKey::new(agent.host_id.clone(), agent.agent_id.clone());
    let row_holds = |what: &'static str,
                     holds: fn(&store::AgentRow, &wire::AcpSnapshot) -> bool| {
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
                    let snapshot = row
                        .snapshot
                        .as_ref()
                        .and_then(|snapshot| {
                            wire::AcpSnapshot::decode(snapshot.body.as_slice()).ok()
                        })
                        .unwrap_or_default();
                    holds(&row, &snapshot).then_some(()).ok_or(format!(
                        "phase {} lifecycle {} sign-in {:?}",
                        row.phase, row.lifecycle, snapshot.sign_in
                    ))
                }
            })
            .await
            .unwrap();
        }
    };
    let signed_out = |row: &store::AgentRow, snapshot: &wire::AcpSnapshot| {
        row.lifecycle == wire::Lifecycle::Live as i32
            && row.phase == wire::Phase::Idle as i32
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
    let answered = |row: &store::AgentRow, snapshot: &wire::AcpSnapshot| {
        row.lifecycle == wire::Lifecycle::Live as i32
            && row.phase == wire::Phase::Idle as i32
            && snapshot.sign_in.as_ref().map(wire::SignIn::state)
                == Some(wire::SignInState::SignedIn)
            && row
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.queue.is_empty())
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
    drop((inventory, runtime));
    daemon.shutdown().await.unwrap();
}
