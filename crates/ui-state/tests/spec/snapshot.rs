//! The thin per-kind snapshot decode and who the header believes.

use prost::Message;
use ui_state::{PhaseView, SessionState, decode_snapshot};
use wire::{Kind, Phase};

use crate::spec::harness::*;

fn strip_facts() -> (
    wire::ClaudeUsage,
    wire::CodexUsage,
    wire::ToolServerHealth,
    wire::SignIn,
    wire::BackgroundJobs,
) {
    (
        wire::ClaudeUsage {
            state: wire::UsageState::NearLimit as i32,
            windows: vec![wire::ClaudeUsageWindow {
                limit: wire::Limit::Weekly as i32,
                model: Some("Fable".into()),
                provider_name: "seven_day_overage_included".into(),
                meter: Some(wire::UsageMeter {
                    used_percent: 91.0,
                    resets_at_ms: Some(9),
                    state: wire::UsageState::NearLimit as i32,
                    used_unknown: false,
                }),
            }],
        },
        wire::CodexUsage {
            state: wire::UsageState::Blocked as i32,
            windows: vec![wire::CodexUsageWindow {
                limit: wire::Limit::FiveHour as i32,
                window_minutes: 300,
                meter: Some(wire::UsageMeter {
                    used_percent: 100.0,
                    resets_at_ms: Some(9),
                    state: wire::UsageState::Blocked as i32,
                    used_unknown: false,
                }),
            }],
            credits: Some("12".into()),
        },
        wire::ToolServerHealth {
            state: wire::HealthState::Degraded as i32,
            servers: vec![wire::ToolServer {
                name: "github".into(),
                status: wire::ToolServerStatus::Failed as i32,
                error: "401".into(),
            }],
        },
        wire::SignIn {
            state: wire::SignInState::Expired as i32,
            account: "me".into(),
            message: "log in".into(),
        },
        wire::BackgroundJobs {
            known: true,
            jobs: vec![
                wire::BackgroundJob {
                    item_key: "t1".into(),
                    command: "npm run dev".into(),
                    started_at_ms: 1_000,
                    ..Default::default()
                },
                wire::BackgroundJob {
                    item_key: "t2".into(),
                    command: "cargo watch".into(),
                    started_at_ms: 2_000,
                    ..Default::default()
                },
            ],
        },
    )
}

#[test]
fn every_kind_decodes_its_snapshot_including_the_four_strip_facts() {
    let (claude_usage, codex_usage, servers, sign_in, background) = strip_facts();
    let tasks = wire::TaskList {
        known: true,
        entries: vec![wire::TaskListEntry {
            id: "1".into(),
            subject: "Update copy".into(),
            status: 2,
            active_form: "Updating".into(),
        }],
    };
    let context = wire::ContextMeter {
        known: true,
        used_tokens: 71_000,
        window_tokens: Some(100_000),
        breakdown: vec![],
    };
    let bodies = [
        (
            Kind::ClaudePty,
            wire::ClaudePtySnapshot {
                tasks: Some(tasks.clone()),
                context: Some(context.clone()),
                model: Some("opus".into()),
                permission: Some("plan".into()),
                usage: Some(claude_usage.clone()),
                servers: Some(servers.clone()),
                sign_in: Some(sign_in.clone()),
                background_jobs: Some(background.clone()),
                ..Default::default()
            }
            .encode_to_vec(),
        ),
        (
            Kind::ClaudeSdk,
            wire::ClaudeSdkSnapshot {
                tasks: Some(tasks.clone()),
                context: Some(context.clone()),
                model: Some("opus".into()),
                effort: Some("high".into()),
                permission: Some("plan".into()),
                usage: Some(claude_usage.clone()),
                servers: Some(servers.clone()),
                sign_in: Some(sign_in.clone()),
                background_jobs: Some(background.clone()),
                ..Default::default()
            }
            .encode_to_vec(),
        ),
        (
            Kind::Codex,
            wire::CodexSnapshot {
                plan: Some(tasks.clone()),
                context: Some(context.clone()),
                model: Some("opus".into()),
                effort: Some("high".into()),
                permission: Some("plan".into()),
                approval_policy: Some("on-request".into()),
                sandbox: Some("workspace-write".into()),
                usage: Some(codex_usage.clone()),
                servers: Some(servers.clone()),
                sign_in: Some(sign_in.clone()),
                background_jobs: Some(background.clone()),
                ..Default::default()
            }
            .encode_to_vec(),
        ),
    ];
    for (kind, body) in bodies {
        let state = decode_snapshot(kind, &body);
        assert_eq!(state.tasks, tasks);
        assert_eq!(state.context, context);
        assert_eq!(state.model.as_deref(), Some("opus"));
        assert_eq!(state.permission.as_deref(), Some("plan"));
        let usage = match kind {
            Kind::Codex => ui_state::Usage::Codex(codex_usage.clone()),
            _ => ui_state::Usage::Claude(claude_usage.clone()),
        };
        assert_eq!(state.usage, usage);
        assert_eq!(state.servers, servers);
        assert_eq!(state.sign_in, sign_in);
        assert_eq!(state.background, background);
        if kind != Kind::ClaudePty {
            assert_eq!(state.effort.as_deref(), Some("high"));
        }
    }
}

#[test]
fn unknown_is_explicit_and_never_depends_on_which_fact_came_first() {
    for kind in KINDS {
        let state = decode_snapshot(kind, &snapshot_body(kind, &[]));
        assert_eq!(
            state,
            decode_snapshot(kind, &[]),
            "all unknown is the empty body"
        );
        assert_eq!(state.model, None);
        assert_eq!(state.usage.state(), wire::UsageState::Unknown);
        assert!(!state.background.known);
        assert!(!state.context.known);
        let garbage = decode_snapshot(kind, &[0xff, 0xff, 0xff]);
        assert_eq!(
            garbage,
            decode_snapshot(kind, &[]),
            "an unreadable body is nothing known"
        );
    }
}

#[test]
fn the_envelope_carries_phase_and_queue() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), CAP);
        let snap = snapshot(kind, 4, Phase::Working, &[], &[(b"q1", false)]);
        apply_checked(&mut state, ev_snapshot(snap));
        let decoded = state.agent_state();
        assert_eq!(decoded.phase, Phase::Working);
        assert_eq!(decoded.queue.len(), 1);
        assert_eq!(decoded.revision, 4);
    }
}

#[test]
fn the_chat_header_reads_the_snapshot_while_the_row_says_otherwise() {
    for kind in KINDS {
        // The inventory row lags one round trip behind the stream.
        let mut state = SessionState::new(with_phase(agent(kind), Phase::Idle), CAP);
        assert_eq!(
            state.phase(),
            PhaseView::Idle,
            "before any snapshot the row speaks"
        );
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 3, Phase::Working, &[], &[])),
        );
        assert_eq!(
            state.phase(),
            PhaseView::Working,
            "the snapshot wins on what the agent is doing"
        );
        apply_checked(
            &mut state,
            ui_state::Msg::Entry(with_phase(agent(kind), Phase::Idle)),
        );
        assert_eq!(state.phase(), PhaseView::Working);
        apply_checked(
            &mut state,
            ui_state::Msg::Entry(exited(agent(kind), wire::ExitCause::Stopped)),
        );
        assert_eq!(
            state.phase(),
            PhaseView::Exited {
                cause: ui_state::ExitCause::Ended
            },
            "the entry wins on whether the process exists"
        );
    }
}

#[test]
fn an_older_catalogue_answering_late_never_replaces_the_one_the_snapshot_names() {
    let offering = |hash: &[u8], name: &str| ui_state::Msg::Catalogue {
        hash: hash.to_vec(),
        catalogue: wire::Catalogue {
            models: vec![wire::OfferedModel {
                value: name.into(),
                ..Default::default()
            }],
            permissions: vec![wire::OfferedPermission {
                value: name.into(),
                settable: true,
                ..Default::default()
            }],
            modes: vec![wire::OfferedMode {
                value: name.into(),
                settable: true,
                ..Default::default()
            }],
            ..Default::default()
        },
    };
    let offered = |state: &SessionState| {
        let agent = state.agent_state();
        (
            agent
                .models
                .iter()
                .map(|m| m.value.clone())
                .collect::<Vec<_>>(),
            agent
                .permissions
                .iter()
                .map(|p| p.value.clone())
                .collect::<Vec<_>>(),
            agent
                .modes
                .iter()
                .map(|m| m.value.clone())
                .collect::<Vec<_>>(),
        )
    };
    let b = || {
        (
            vec!["b".to_owned()],
            vec!["b".to_owned()],
            vec!["b".to_owned()],
        )
    };
    for kind in [Kind::ClaudeSdk, Kind::Codex] {
        let naming = |revision, hash: &[u8]| {
            let mut snap = snapshot(kind, revision, Phase::Idle, &[], &[]);
            snap.catalogue = Some(hash.to_vec());
            ev_snapshot(snap)
        };
        let mut state = SessionState::new(agent(kind), CAP);
        apply_checked(&mut state, naming(1, b"a"));
        apply_checked(&mut state, naming(2, b"b"));
        state.update(offering(b"b", "b"));
        assert_eq!(offered(&state), b());
        let late = state.update(offering(b"a", "a"));
        assert_eq!(offered(&state), b(), "the late older catalogue is dropped");
        assert!(!late.session, "and changes nothing a reader shows");
        assert_eq!(state.held_catalogue(), Some(&b"b"[..]));

        // Held but no longer named, a catalogue gives way to any other.
        apply_checked(&mut state, naming(3, b"a"));
        state.update(offering(b"a", "a"));
        assert_eq!(offered(&state).0, ["a"]);
    }
}

#[test]
fn what_the_fetched_catalogue_offers_shows_while_the_snapshot_names_it() {
    let models = vec![wire::OfferedModel {
        value: "sonnet".into(),
        display_name: "Sonnet".into(),
        description: "Efficient".into(),
        efforts: vec!["low".into(), "high".into()],
        default_effort: None,
        resolved_model: "claude-sonnet-5".into(),
    }];
    let commands = vec![wire::OfferedCommand {
        name: "stripe:test-cards".into(),
        description: "Test cards".into(),
        argument_hint: String::new(),
        source: "stripe".into(),
    }];
    let catalogue = wire::Catalogue {
        models: models.clone(),
        commands: commands.clone(),
        ..Default::default()
    };
    for kind in [Kind::ClaudeSdk, Kind::Codex] {
        let naming = |revision, hash: &[u8]| {
            let mut snap = snapshot(kind, revision, Phase::Idle, &[], &[]);
            snap.catalogue = Some(hash.to_vec());
            ev_snapshot(snap)
        };
        let mut state = SessionState::new(agent(kind), CAP);
        apply_checked(&mut state, naming(1, b"first"));
        assert_eq!(
            state.agent_state().catalogue.as_deref(),
            Some(&b"first"[..])
        );
        assert!(state.agent_state().models.is_empty(), "not fetched yet");
        state.set_catalogue(b"first".to_vec(), catalogue.clone());
        let offered = state.agent_state();
        assert_eq!((&offered.models, &offered.commands), (&models, &commands));
        apply_checked(&mut state, naming(2, b"first"));
        assert_eq!(
            state.agent_state().models,
            models,
            "the same catalogue stays"
        );
        apply_checked(&mut state, naming(3, b"second"));
        let changed = state.agent_state();
        assert!(
            changed.models.is_empty() && changed.commands.is_empty(),
            "a catalogue the snapshot no longer names offers nothing"
        );
    }
}
