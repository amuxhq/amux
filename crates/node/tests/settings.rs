//! The everyday settings the front door serves: read from the config file,
//! changed in it, and in force at once, on this host and as its paired
//! hosts see it.

use support::synthetic::*;
use support::*;
use tonic::Request;
use wire::UpdateSettingsRequest;
use wire::agent_service_server::AgentService as _;
use wire::installation_service_server::InstallationService as _;

use crate::support;

/// A desk whose settings live in a config file beside its data.
async fn desk_with_file(install: &Install, text: &str) -> (node::Daemon, std::path::PathBuf) {
    let file = install.path("config.toml");
    std::fs::write(&file, text).unwrap();
    let mut options = install.options("boot-1", quiet_launch());
    let initial = settings::InstallationConfig::from_file(&file).unwrap();
    // Running the experiments the file switches on, as `amux daemon` does.
    options.launch.experiments = initial.experiments.enabled();
    options.settings = node::SettingsOptions {
        file: Some(file.clone()),
        initial,
        login: None,
        home: options.settings.home.take(),
    };
    options.edge.host_name = "desk".to_owned();
    (node::start(options, None).await.unwrap(), file)
}

/// A change through the front door is written to the file, keeping the rest
/// of it, and is in force at once: the host's own entry says its new name
/// and Claude kind, and its listing offers its new repositories.
#[tokio::test(flavor = "multi_thread")]
async fn a_settings_change_is_written_and_in_force_at_once() {
    let desk = Install::new();
    let (daemon, file) = desk_with_file(&desk, "# the desk\nkeep_awake = true\n").await;
    let door = daemon.front_door();
    let runtime = runtime(&daemon, &desk);

    let settings = door
        .get_settings(Request::new(wire::GetSettingsRequest {}))
        .await
        .unwrap()
        .into_inner()
        .settings
        .unwrap();
    assert_eq!(settings.host_name, "desk");
    assert_eq!(
        settings.start_at_login, None,
        "no supervisor, no login item"
    );
    assert!(settings.keep_awake && settings.repositories.is_empty());
    assert_eq!(settings.claude_kind, wire::Kind::Unspecified as i32);
    assert_eq!(
        runtime
            .host_entry()
            .provider("claude")
            .map_or(wire::Kind::Unspecified, wire::ProviderOnHost::kind),
        wire::Kind::ClaudeSdk,
        "unset follows amux's chat"
    );

    let repository = desk.work.join("app");
    std::fs::create_dir_all(&repository).unwrap();
    let changed = door
        .update_settings(Request::new(UpdateSettingsRequest {
            host_name: Some("study".into()),
            claude_kind: Some(wire::Kind::ClaudePty as i32),
            repositories: Some(wire::RepositoryList {
                paths: vec![repository.to_str().unwrap().to_owned()],
            }),
            ..UpdateSettingsRequest::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .settings
        .unwrap();
    assert_eq!(changed.host_name, "study");
    assert_eq!(changed.claude_kind, wire::Kind::ClaudePty as i32);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("# the desk\n"), "{text}");
    assert!(text.contains("host_name = \"study\""), "{text}");

    let entry = runtime.host_entry();
    assert_eq!(
        entry
            .provider("claude")
            .map_or(wire::Kind::Unspecified, wire::ProviderOnHost::kind),
        wire::Kind::ClaudePty
    );
    assert_eq!(entry.host.unwrap().name, "study");
    until(
        "the desk's entry to offer its repository",
        pinned_app(&runtime),
    )
    .await
    .unwrap();

    // A profile opened after the change starts with it too.
    let created = door
        .create_profile(Request::new(wire::CreateProfileRequest {
            label: Some("spare".to_owned()),
        }))
        .await
        .unwrap()
        .into_inner()
        .profile
        .unwrap();
    let spare = daemon
        .profile(uuid::Uuid::from_slice(&created.id).unwrap())
        .expect("the new profile runs");
    assert_eq!(
        spare
            .host_entry()
            .provider("claude")
            .map_or(wire::Kind::Unspecified, wire::ProviderOnHost::kind),
        wire::Kind::ClaudePty
    );
    until(
        "the new profile's entry to offer it too",
        pinned_app(&spare),
    )
    .await
    .unwrap();
    drop(spare);

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

/// The experiment tables in the profile's store, by name.
fn experiment_tables(install: &Install) -> Vec<String> {
    let store = install.store();
    let mut read = store
        .connection()
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'exp%' ORDER BY name")
        .unwrap();
    read.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// A daemon runs the experiments switched on when it started: their store
/// tables are made as it opens a profile, and a switch while it runs is
/// written and served but changes nothing until the next start, which
/// drops the tables of an experiment switched off.
#[tokio::test(flavor = "multi_thread")]
async fn an_experiment_switched_takes_effect_at_the_next_start() {
    let desk = Install::new();
    let file = desk.path("config.toml");
    std::fs::write(&file, "[experiments]\nenabled = [\"suspension\"]\n").unwrap();
    let start = |boot: &'static str| {
        let config = settings::InstallationConfig::from_file(&file).unwrap();
        // As the amux binary starts its daemon: the switch, read once.
        let launch = node::Launch {
            experiments: config.experiments.enabled(),
            ..quiet_launch()
        };
        let mut options = desk.options(boot, launch);
        options.settings = node::SettingsOptions {
            file: Some(file.clone()),
            initial: config,
            login: None,
            home: options.settings.home.take(),
        };
        async move { node::start(options, None).await.unwrap() }
    };

    let daemon = start("boot-1").await;
    assert_eq!(experiment_tables(&desk), ["exp_suspension_1_runs"]);
    let door = daemon.front_door();
    let changed = door
        .update_settings(Request::new(UpdateSettingsRequest {
            experiments: Some(wire::ExperimentList {
                names: vec!["workspaces".into()],
            }),
            ..UpdateSettingsRequest::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .settings
        .unwrap();
    let on: Vec<_> = changed
        .experiments
        .iter()
        .filter(|experiment| experiment.enabled)
        .map(|experiment| experiment.version.as_str())
        .collect();
    assert_eq!(on, ["workspaces/1"], "the settings answer the switch");
    assert_eq!(
        runtime(&daemon, &desk).launch().experiments.versions(),
        ["suspension/1"],
        "what runs waits for the next start"
    );
    assert_eq!(experiment_tables(&desk), ["exp_suspension_1_runs"]);
    daemon.shutdown().await.unwrap();

    let daemon = start("boot-1").await;
    // Workspaces keeps nothing in tables of its own: its entries ride on
    // the rows.
    assert!(experiment_tables(&desk).is_empty());
    assert_eq!(
        runtime(&daemon, &desk).launch().experiments.versions(),
        ["workspaces/1"]
    );
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
        let kind = runtime
            .host_entry()
            .provider("claude")
            .map_or(wire::Kind::Unspecified, wire::ProviderOnHost::kind);
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
            let kind = laptop_runtime.host_entry_of(desk_host).map(|entry| {
                entry
                    .provider("claude")
                    .map_or(wire::Kind::Unspecified, wire::ProviderOnHost::kind)
            });
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

/// A paired laptop pins a folder on the desk: the desk keeps it among its
/// repositories, in its config file, and its entry in the laptop's inventory
/// lists it as pinned, with its working tree; unpinned, it goes.
#[tokio::test(flavor = "multi_thread")]
async fn a_paired_host_pins_a_folder_and_sees_the_desks_entry_say_it() {
    let desk = Install::new();
    let (desk_daemon, file) = desk_with_file(&desk, "").await;
    let desk_runtime = runtime(&desk_daemon, &desk);
    let app = desk.work.join("app");
    std::fs::create_dir_all(app.join(".git")).unwrap();
    let app = app.canonicalize().unwrap();
    let laptop = Install::new();
    let laptop_daemon = laptop.start("boot-1", quiet_launch()).await;
    let laptop_runtime = runtime(&laptop_daemon, &laptop);
    let (desk_edge, laptop_edge) = (desk_runtime.edge().unwrap(), laptop_runtime.edge().unwrap());
    let desk_host = desk_runtime.host();
    desk_edge.trust(&laptop_edge).await.unwrap();
    laptop_edge.trust(&desk_edge).await.unwrap();
    let _link = laptop_edge.link_in_process(&desk_edge).unwrap();
    assert!(wait_for_route(&laptop_edge, desk_host, PATIENCE).await);

    let from_laptop = node::ClientApi::new(&laptop_runtime, None);
    let pin = |pinned: bool| wire::PinFolderRequest {
        host_id: Some(desk_host.as_bytes().to_vec()),
        path: app.to_str().unwrap().to_owned(),
        pinned,
    };
    from_laptop
        .pin_folder(Request::new(pin(true)))
        .await
        .unwrap();
    let pinned = |want: bool| {
        let laptop_runtime = laptop_runtime.clone();
        let app = app.clone();
        move || {
            let folder = laptop_runtime.host_entry_of(desk_host).and_then(|entry| {
                entry
                    .folders
                    .into_iter()
                    .find(|folder| folder.path == app.to_str().unwrap())
            });
            async move {
                match folder {
                    Some(folder) if want && folder.kind() == wire::FolderKind::Pinned => Ok(folder),
                    None if !want => Ok(wire::Folder::default()),
                    other => Err(format!("{other:?}")),
                }
            }
        }
    };
    let folder = until("the laptop to see the desk's pin", pinned(true))
        .await
        .unwrap();
    assert_eq!(folder.name, "app");
    assert_eq!(
        folder.checkout.map(|checkout| checkout.root),
        Some(app.to_str().unwrap().to_owned()),
        "a folder in Git says its working tree"
    );
    assert_eq!(
        settings::InstallationConfig::from_file(&file)
            .unwrap()
            .repositories,
        std::slice::from_ref(&app),
        "the desk keeps the pin in its config file"
    );

    from_laptop
        .pin_folder(Request::new(pin(false)))
        .await
        .unwrap();
    until("the laptop to see the desk's unpin", pinned(false))
        .await
        .unwrap();

    drop((desk_edge, laptop_edge, desk_runtime, laptop_runtime));
    desk_daemon.shutdown().await.unwrap();
    laptop_daemon.shutdown().await.unwrap();
}

/// Under `keep_awake` the daemon keeps the machine awake while one of its
/// agents works and lets it sleep once none does; turning the setting off
/// lets it sleep at once.
#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_keeps_the_machine_awake_while_an_agent_works() {
    let install = Install::new();
    let gate = install.path("gate");
    let file = install.path("config.toml");
    std::fs::write(&file, "keep_awake = true\n").unwrap();
    let mut options = install.options("boot-1", install.launch("held", vec![wait_for(&gate)]));
    options.settings = node::SettingsOptions {
        file: Some(file.clone()),
        initial: settings::InstallationConfig::from_file(&file).unwrap(),
        login: None,
        home: options.settings.home.take(),
    };
    let daemon = node::start(options, None).await.unwrap();
    let runtime = runtime(&daemon, &install);
    assert!(!daemon.keeps_awake(), "no agent works yet");

    let agent = runtime
        .spawn(create(&install.work, "busy", Some("hold on")), None)
        .await
        .unwrap();
    let awake = |want: bool| {
        let daemon = &daemon;
        move || {
            let now = daemon.keeps_awake();
            async move {
                (now == want)
                    .then_some(())
                    .ok_or_else(|| format!("keeps awake: {now}"))
            }
        }
    };
    until("the working agent to keep the machine awake", awake(true))
        .await
        .unwrap();
    daemon
        .front_door()
        .update_settings(Request::new(UpdateSettingsRequest {
            keep_awake: Some(false),
            ..UpdateSettingsRequest::default()
        }))
        .await
        .unwrap();
    assert!(!daemon.keeps_awake(), "off lets it sleep at once");
    daemon
        .front_door()
        .update_settings(Request::new(UpdateSettingsRequest {
            keep_awake: Some(true),
            ..UpdateSettingsRequest::default()
        }))
        .await
        .unwrap();
    assert!(daemon.keeps_awake());

    std::fs::write(&gate, "").unwrap();
    until("the finished turn to let the machine sleep", awake(false))
        .await
        .unwrap();

    let id = uuid::Uuid::from_slice(&agent.agent_id).unwrap();
    let _ = runtime.kill_agent(id).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// Waits on the runtime's own entry to offer one pinned folder, "app".
fn pinned_app(
    runtime: &std::sync::Arc<node::ProfileRuntime>,
) -> impl Fn() -> std::future::Ready<Result<(), String>> + '_ {
    move || {
        let pinned: Vec<String> = runtime
            .host_entry()
            .folders
            .into_iter()
            .filter(|folder| folder.kind() == wire::FolderKind::Pinned)
            .map(|folder| folder.name)
            .collect();
        std::future::ready(if pinned == ["app"] {
            Ok(())
        } else {
            Err(format!("{pinned:?}"))
        })
    }
}

/// Switches the experiments to `names` through the front door, answering
/// the settings the daemon now serves.
async fn switch(daemon: &node::Daemon, names: &[&str]) -> wire::Settings {
    daemon
        .front_door()
        .update_settings(Request::new(UpdateSettingsRequest {
            experiments: Some(wire::ExperimentList {
                names: names.iter().map(|name| (*name).to_owned()).collect(),
            }),
            ..UpdateSettingsRequest::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .settings
        .unwrap()
}

/// Each experiment's name, whether the settings switch it on and whether
/// it is being applied.
fn experiments(settings: &wire::Settings) -> Vec<(&str, bool, bool)> {
    settings
        .experiments
        .iter()
        .map(|experiment| {
            (
                experiment.name.as_str(),
                experiment.enabled,
                experiment.applying,
            )
        })
        .collect()
}

/// Several switches in a row are one restart: the daemon wants it only once
/// the switch has stayed put for the debounce, for the set switched on last,
/// and says meanwhile that each switch that moved is being applied.
#[tokio::test(flavor = "multi_thread")]
async fn several_switches_in_a_row_want_one_restart_once_they_stay_put() {
    let desk = Install::new();
    let (daemon, _file) = desk_with_file(&desk, "").await;
    let mut restart = Box::pin(daemon.restart_wanted(|| true));

    switch(&daemon, &["suspension"]).await;
    switch(&daemon, &[]).await;
    switch(&daemon, &["workspaces"]).await;
    let last = std::time::Instant::now();
    let settings = switch(&daemon, &["workspaces", "suspension"]).await;
    assert_eq!(
        experiments(&settings),
        [("workspaces", true, true), ("suspension", true, true)]
    );

    let wanted = tokio::time::timeout(PATIENCE, &mut restart)
        .await
        .expect("a restart is wanted");
    assert!(
        last.elapsed() >= node::RESTART_DEBOUNCE,
        "wanted {:?} after the last switch",
        last.elapsed()
    );
    assert_eq!(
        wanted,
        node::RestartWanted {
            running: vec![],
            wanted: vec!["workspaces/1", "suspension/1"],
            settings: vec!["suspension.after = 30m".to_owned()],
            settled: true,
        }
    );
    drop(restart);
    daemon.shutdown().await.unwrap();
}

/// A switch flipped back before the debounce is up, and a hand edit that
/// only names an experiment this build does not have, change nothing the
/// daemon runs: no restart is wanted, and nothing reads as being applied.
#[tokio::test(flavor = "multi_thread")]
async fn a_switch_flipped_back_or_an_unknown_name_wants_no_restart() {
    let desk = Install::new();
    let (daemon, file) = desk_with_file(&desk, "").await;
    let mut restart = Box::pin(daemon.restart_wanted(|| true));

    switch(&daemon, &["suspension"]).await;
    let settings = switch(&daemon, &[]).await;
    assert_eq!(
        experiments(&settings),
        [("workspaces", false, false), ("suspension", false, false)]
    );
    // A name another build has, as `amux config set` or an editor writes it;
    // the daemon reads the file again within its poll.
    std::fs::write(&file, "[experiments]\nenabled = [\"teleport\"]\n").unwrap();
    let held = tokio::time::timeout(
        node::RESTART_DEBOUNCE + std::time::Duration::from_secs(4),
        &mut restart,
    )
    .await;
    assert!(held.is_err(), "no restart is wanted: {held:?}");
    let settings = daemon
        .front_door()
        .get_settings(Request::new(wire::GetSettingsRequest {}))
        .await
        .unwrap()
        .into_inner()
        .settings
        .unwrap();
    assert_eq!(
        experiments(&settings),
        [("workspaces", false, false), ("suspension", false, false)]
    );

    // A hand edit that does switch one on is noticed like a client's switch.
    std::fs::write(
        &file,
        "[experiments]\nenabled = [\"teleport\", \"suspension\"]\n",
    )
    .unwrap();
    let wanted = tokio::time::timeout(PATIENCE, &mut restart)
        .await
        .expect("a restart is wanted");
    assert_eq!(wanted.wanted, ["suspension/1"]);
    drop(restart);
    daemon.shutdown().await.unwrap();
}

/// An experiment's own settings restart the daemon as its switch does, as
/// its module reads them: a new delay for suspension while it runs wants
/// one restart, with the new delay, and says suspension is being applied;
/// a value the module cannot use, a key it does not read, and the delay of
/// an experiment that is off change nothing it runs and restart nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_running_experiments_own_settings_want_a_restart_as_its_module_reads_them() {
    let on = "[experiments]\nenabled = [\"suspension\"]\n";
    let desk = Install::new();
    let (daemon, file) = desk_with_file(&desk, on).await;
    let mut restart = Box::pin(daemon.restart_wanted(|| true));
    let quiet = node::RESTART_DEBOUNCE + std::time::Duration::from_secs(4);

    // As written by hand; the daemon reads the file again within its poll.
    std::fs::write(
        &file,
        format!("{on}[experiments.suspension]\nafter = \"soon\"\nbefore = \"1h\"\n"),
    )
    .unwrap();
    let held = tokio::time::timeout(quiet, &mut restart).await;
    assert!(held.is_err(), "nothing it runs changed: {held:?}");

    std::fs::write(
        &file,
        format!("{on}[experiments.suspension]\nafter = \"45m\"\n"),
    )
    .unwrap();
    until("suspension is being applied", || async {
        let settings = daemon
            .front_door()
            .get_settings(Request::new(wire::GetSettingsRequest {}))
            .await
            .unwrap()
            .into_inner()
            .settings
            .unwrap();
        let listed = experiments(&settings);
        (listed == [("workspaces", false, false), ("suspension", true, true)])
            .then_some(())
            .ok_or(format!("{listed:?}"))
    })
    .await
    .unwrap();
    let wanted = tokio::time::timeout(PATIENCE, &mut restart)
        .await
        .expect("a restart is wanted");
    assert_eq!(
        wanted,
        node::RestartWanted {
            running: vec!["suspension/1"],
            wanted: vec!["suspension/1"],
            settings: vec!["suspension.after = 45m".to_owned()],
            settled: true,
        }
    );
    drop(restart);
    daemon.shutdown().await.unwrap();

    // Off, its delay changes nothing the daemon runs.
    let desk = Install::new();
    let (daemon, file) = desk_with_file(&desk, "").await;
    let mut restart = Box::pin(daemon.restart_wanted(|| true));
    std::fs::write(&file, "[experiments.suspension]\nafter = \"45m\"\n").unwrap();
    let held = tokio::time::timeout(quiet, &mut restart).await;
    assert!(held.is_err(), "suspension is off: {held:?}");
    drop(restart);
    daemon.shutdown().await.unwrap();
}
