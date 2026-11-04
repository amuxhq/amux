//! The everyday settings the front door serves: read from the config file,
//! changed in it, and in force at once, on this host and as its paired
//! hosts see it.

use support::synthetic::*;
use support::*;
use tonic::Request;
use wire::agent_service_server::AgentService as _;
use wire::installation_service_server::InstallationService as _;
use wire::{Empty, UpdateSettingsRequest};

use crate::support;

/// A desk whose settings live in a config file beside its data.
async fn desk_with_file(install: &Install, text: &str) -> (node::Daemon, std::path::PathBuf) {
    let file = install.path("config.toml");
    std::fs::write(&file, text).unwrap();
    let mut options = install.options("boot-1", quiet_launch());
    options.settings = node::SettingsOptions {
        file: Some(file.clone()),
        initial: settings::InstallationConfig::from_file(&file).unwrap(),
        login: None,
    };
    options.edge.host_name = "desk".to_owned();
    (node::start(options, None).await.unwrap(), file)
}

/// A change through the front door is written to the file, keeping the rest
/// of it, and is in force at once: the host's own entry says its new name
/// and Claude kind, and its listing offers its new projects.
#[tokio::test(flavor = "multi_thread")]
async fn a_settings_change_is_written_and_in_force_at_once() {
    let desk = Install::new();
    let (daemon, file) = desk_with_file(&desk, "# the desk\nkeep_awake = true\n").await;
    let door = daemon.front_door();
    let runtime = runtime(&daemon, &desk);

    let settings = door
        .get_settings(Request::new(Empty {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(settings.host_name, "desk");
    assert_eq!(
        settings.start_at_login, None,
        "no supervisor, no login item"
    );
    assert!(settings.keep_awake && settings.projects.is_empty());
    assert_eq!(settings.claude_kind, wire::Kind::Unspecified as i32);
    assert_eq!(
        runtime.host_entry().claude_kind(),
        wire::Kind::ClaudeSdk,
        "unset follows amux's chat"
    );

    let project = desk.work.join("app");
    std::fs::create_dir_all(&project).unwrap();
    let changed = door
        .update_settings(Request::new(UpdateSettingsRequest {
            host_name: Some("study".into()),
            claude_kind: Some(wire::Kind::ClaudePty as i32),
            projects: Some(wire::ProjectList {
                paths: vec![project.to_str().unwrap().to_owned()],
            }),
            ..UpdateSettingsRequest::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(changed.host_name, "study");
    assert_eq!(changed.claude_kind, wire::Kind::ClaudePty as i32);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("# the desk\n"), "{text}");
    assert!(text.contains("host_name = \"study\""), "{text}");

    let entry = runtime.host_entry();
    assert_eq!(entry.claude_kind(), wire::Kind::ClaudePty);
    assert_eq!(entry.host.unwrap().name, "study");
    let listed = node::ClientApi::new(&runtime, None)
        .list_repositories(Request::new(wire::ListRepositoriesRequest {
            query: None,
            limit: 10,
            host_id: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.projects.len(), 1);
    assert_eq!(listed.projects[0].name, "app");

    let refused = door
        .update_settings(Request::new(UpdateSettingsRequest {
            claude_kind: Some(wire::Kind::Codex as i32),
            ..UpdateSettingsRequest::default()
        }))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), tonic::Code::InvalidArgument, "{refused:?}");

    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// A change made to the file beside the daemon, as `amux config` makes one,
/// is in force within moments: setting the interface moves the Claude kind
/// that follows it.
#[tokio::test(flavor = "multi_thread")]
async fn a_change_to_the_file_beside_the_daemon_is_applied() {
    let desk = Install::new();
    let (daemon, file) = desk_with_file(&desk, "").await;
    let runtime = runtime(&daemon, &desk);
    std::fs::write(&file, "[ui]\nchat_in = \"native\"\n").unwrap();
    until("the native interface's Claude to be published", || {
        let kind = runtime.host_entry().claude_kind();
        async move {
            match kind {
                wire::Kind::ClaudePty => Ok(()),
                other => Err(format!("{other:?}")),
            }
        }
    })
    .await
    .unwrap();
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// A paired laptop lists the desk with the Claude kind the desk publishes,
/// and follows it when the desk's person changes it.
#[tokio::test(flavor = "multi_thread")]
async fn a_paired_host_learns_the_claude_kind_a_host_publishes() {
    let desk = Install::new();
    let (desk_daemon, _file) = desk_with_file(&desk, "").await;
    let desk_runtime = runtime(&desk_daemon, &desk);
    let laptop = Install::new();
    let laptop_daemon = laptop.start("boot-1", quiet_launch()).await;
    let laptop_runtime = runtime(&laptop_daemon, &laptop);
    let (desk_edge, laptop_edge) = (desk_runtime.edge().unwrap(), laptop_runtime.edge().unwrap());
    let desk_host = desk_runtime.host();
    desk_edge.trust(&laptop_edge).await.unwrap();
    laptop_edge.trust(&desk_edge).await.unwrap();
    let _link = laptop_edge.link_in_process(&desk_edge).unwrap();
    assert!(wait_for_route(&laptop_edge, desk_host, PATIENCE).await);

    let listed = |want: wire::Kind| {
        let laptop_runtime = laptop_runtime.clone();
        move || {
            let kind = laptop_runtime
                .host_entry_of(desk_host)
                .map(|entry| entry.claude_kind());
            async move {
                match kind {
                    Some(kind) if kind == want => Ok(()),
                    other => Err(format!("{other:?}")),
                }
            }
        }
    };
    until(
        "the laptop to list the desk's Claude kind",
        listed(wire::Kind::ClaudeSdk),
    )
    .await
    .unwrap();
    desk_daemon
        .front_door()
        .update_settings(Request::new(UpdateSettingsRequest {
            claude_kind: Some(wire::Kind::ClaudePty as i32),
            ..UpdateSettingsRequest::default()
        }))
        .await
        .unwrap();
    until(
        "the laptop to follow the desk's change",
        listed(wire::Kind::ClaudePty),
    )
    .await
    .unwrap();

    drop((desk_edge, laptop_edge, desk_runtime, laptop_runtime));
    desk_daemon.shutdown().await.unwrap();
    laptop_daemon.shutdown().await.unwrap();
}
