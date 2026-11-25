//! An ACP agent in the shared views, from small authored states: what its
//! host and its snapshot say it can do decides what is offered (send-now,
//! resume, attachments), its provider is named as its host names it, and
//! its asks offer exactly what the agent offered, a reply instead of any.

use prost::Message;
use ui_state::{Composer, FleetMsg, FleetState, Msg, SessionState};
use ui_view::*;
use wire::{Kind, Phase, SessionEvent, session_event};

use crate::support::CAP;

fn event(of: session_event::Of) -> Msg {
    Msg::Event(SessionEvent { of: Some(of) })
}

fn row(lifecycle: wire::Lifecycle) -> wire::Agent {
    wire::Agent {
        agent_id: b"agent".to_vec(),
        host_id: b"host".to_vec(),
        kind: Kind::Acp as i32,
        provider: "opencode".into(),
        name: "worker".into(),
        lifecycle: lifecycle as i32,
        status: wire::AgentStatus::in_phase(Phase::Working),
        incarnation: 1,
        ..Default::default()
    }
}

/// The host's entry, offering OpenCode under its own name and words for
/// signing in.
fn host(can: wire::AgentCapabilities) -> wire::HostEntry {
    wire::HostEntry {
        providers: vec![wire::ProviderOnHost {
            provider: "opencode".into(),
            descriptor: Some(wire::ProviderDescriptor {
                name: "OpenCode".into(),
                kind: Kind::Acp as i32,
                sign_in_hint: "Run `opencode auth login` in the terminal".into(),
                version: "1.18.33".into(),
            }),
            kinds: vec![wire::KindOnHost {
                kind: Kind::Acp as i32,
                capabilities: Some(can),
            }],
            ..Default::default()
        }],
        ..wire::HostEntry::trusted_host(
            b"host",
            "desk",
            wire::trusted::Reach::Online(wire::Online::via(wire::RouteKind::Direct)),
        )
    }
}

fn session(
    lifecycle: wire::Lifecycle,
    can: wire::AgentCapabilities,
    body: wire::AcpSnapshot,
    queue: Vec<wire::QueuedInput>,
) -> SessionState {
    session_with(
        lifecycle,
        can,
        wire::Snapshot {
            body: body.encode_to_vec(),
            queue,
            ..Default::default()
        },
        None,
    )
}

/// A caught-up session whose snapshot is `envelope`, as an ACP agent with
/// `can` working, signed in as `sign_in` says.
fn session_with(
    lifecycle: wire::Lifecycle,
    can: wire::AgentCapabilities,
    envelope: wire::Snapshot,
    sign_in: Option<wire::SignIn>,
) -> SessionState {
    let mut state = SessionState::new(row(lifecycle), CAP);
    state.update(Msg::Host(host(can.clone())));
    state.update(event(session_event::Of::Snapshot(wire::Snapshot {
        kind: Kind::Acp as i32,
        status: wire::AgentStatus::in_phase(Phase::Working),
        capabilities: Some(Box::new(can)),
        sign_in,
        ..envelope
    })));
    state.update(event(session_event::Of::CaughtUp(wire::CaughtUp {
        revision: 0,
    })));
    state
}

fn queued(id: &[u8]) -> wire::QueuedInput {
    wire::QueuedInput {
        input_id: id.to_vec(),
        text: "and then this".into(),
        ..Default::default()
    }
}

/// A queued prompt is offered send-now only by an agent known to take one
/// into its running turn; every agent's can be withdrawn.
#[test]
fn send_now_is_offered_only_by_an_agent_that_steers() {
    for steers in [true, false] {
        let can = wire::AgentCapabilities::acp(true, true, steers);
        let state = session(
            wire::Lifecycle::Live,
            can,
            wire::AcpSnapshot::default(),
            vec![queued(b"q1")],
        );
        let rows = queue_rows(&state);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].can_withdraw);
        assert_eq!(rows[0].can_send_now, steers, "steers: {steers}");
    }
}

/// An exited agent whose program cannot reopen its conversation is
/// offered no resume; one that can is.
#[test]
fn an_exited_agent_is_resumed_only_where_its_program_reopens_conversations() {
    for (resumable, composer) in [(true, Composer::Resume), (false, Composer::Ended)] {
        let can = wire::AgentCapabilities::acp(resumable, true, false);
        let state = session(
            wire::Lifecycle::Exited,
            can,
            wire::AcpSnapshot::default(),
            Vec::new(),
        );
        assert_eq!(state.composer(), composer, "resumable: {resumable}");
        assert_eq!(ui_view::composer(&state, 0).mode, composer);
    }
}

/// What a prompt may carry follows what the agent takes: an image to an
/// agent that takes none is refused before it is stored or sent, as is a
/// file to one that takes no files; pasted words always go.
#[test]
fn attachments_follow_what_the_agent_takes() {
    let blob = |mime: &str| wire::BlobRef {
        name: "x".into(),
        mime: mime.into(),
        ..Default::default()
    };
    let image = wire::Attachment {
        of: Some(wire::attachment::Of::Image(blob("image/png"))),
        ..Default::default()
    };
    let file = wire::Attachment {
        of: Some(wire::attachment::Of::File(blob("text/plain"))),
        ..Default::default()
    };
    let text = wire::Attachment {
        of: Some(wire::attachment::Of::Text(wire::InlineText {
            name: "pasted".into(),
            text: "words".into(),
        })),
        ..Default::default()
    };
    let takes_none = wire::AgentCapabilities {
        files: false,
        ..wire::AgentCapabilities::acp(true, false, false)
    };
    let state = session(
        wire::Lifecycle::Live,
        takes_none,
        wire::AcpSnapshot::default(),
        Vec::new(),
    );
    assert_eq!(untaken_file(&state, "image/png"), Some(Untaken::Images));
    assert_eq!(untaken_file(&state, "text/plain"), Some(Untaken::Files));
    assert_eq!(
        untaken(&state, &[text.clone(), image.clone()]),
        Some(Untaken::Images)
    );
    assert_eq!(
        untaken(&state, std::slice::from_ref(&file)),
        Some(Untaken::Files)
    );
    assert_eq!(untaken(&state, std::slice::from_ref(&text)), None);
    let takes_all = session(
        wire::Lifecycle::Live,
        wire::AgentCapabilities::acp(true, true, false),
        wire::AcpSnapshot::default(),
        Vec::new(),
    );
    assert_eq!(untaken(&takes_all, &[image, file, text]), None);
}

/// The host names the agent's provider, says how to sign in to it and
/// whether it has a terminal; the fleet card says all three without a chat
/// open, and the sign-in notice in a chat says who and how.
#[test]
fn the_provider_is_named_and_signed_in_to_as_its_host_says() {
    let can = wire::AgentCapabilities::acp(true, true, false);
    let state = session_with(
        wire::Lifecycle::Live,
        can.clone(),
        wire::Snapshot::default(),
        Some(wire::SignIn {
            state: wire::SignInState::SignedOut as i32,
            ..Default::default()
        }),
    );
    let notice = sign_in(&state).expect("signed out");
    assert_eq!(notice.provider_name, "OpenCode");
    assert_eq!(notice.hint, "Run `opencode auth login` in the terminal");

    let mut fleet = FleetState::new();
    for of in [
        wire::inventory_event::Of::Host(host(can)),
        wire::inventory_event::Of::Agent(row(wire::Lifecycle::Live)),
        wire::inventory_event::Of::CaughtUp(wire::CaughtUp { revision: 0 }),
    ] {
        fleet.update(FleetMsg::Event(Box::new(wire::InventoryEvent {
            of: Some(of),
        })));
    }
    let card = fleet_card(&fleet, b"host", b"agent").expect("a card");
    assert_eq!(card.provider, "opencode");
    assert_eq!(card.provider_name, "OpenCode");
    assert!(!card.attachable);
    assert!(matches(&fleet, &row(wire::Lifecycle::Live), "opencode"));
}

fn permission_ask() -> wire::AcpSnapshot {
    let option = |id: &str, name: &str, kind: wire::AcpPermissionKind| wire::AcpPermissionOption {
        option_id: id.into(),
        name: name.into(),
        kind: kind as i32,
    };
    wire::AcpSnapshot {
        asks: vec![wire::AcpAsk {
            key: "\"7\"".into(),
            item_key: "tool:c1".into(),
            body: Some(wire::acp_ask::Body::Permission(wire::AcpPermission {
                title: "ls -la".into(),
                kind: wire::AcpToolKind::Execute as i32,
                options: vec![
                    option("once", "Allow", wire::AcpPermissionKind::AllowOnce),
                    option(
                        "always",
                        "Always allow",
                        wire::AcpPermissionKind::AllowAlways,
                    ),
                    option("no", "Reject", wire::AcpPermissionKind::RejectOnce),
                    option("never", "Never", wire::AcpPermissionKind::RejectAlways),
                ],
            })),
        }],
        ..Default::default()
    }
}

/// A permission offers exactly the agent's options, in its order, each
/// sending its own option id; the one-time refusal takes the person's
/// words, which then go instead of answering, as does a reply on its own.
/// Nothing on an ACP card falls through to another kind's answer.
#[test]
fn a_permission_offers_exactly_the_agents_options_and_a_reply_instead() {
    let state = session(
        wire::Lifecycle::Live,
        wire::AgentCapabilities::acp(true, true, false),
        permission_ask(),
        Vec::new(),
    );
    let card = ask_card(&state).expect("a card");
    assert!(card.reply_instead);
    assert_eq!(
        card.choices
            .iter()
            .map(|choice| choice.outcome.clone())
            .collect::<Vec<_>>(),
        [
            ChoiceOutcome::AllowOnce,
            ChoiceOutcome::Offered {
                name: "Always allow".into(),
                refuses: false
            },
            ChoiceOutcome::Deny,
            ChoiceOutcome::Offered {
                name: "Never".into(),
                refuses: true
            },
        ]
    );
    assert!(card.choices[0].primary);
    assert!(card.choices[2].takes_note);
    let sent = |answer: &Answer, note: &str| {
        let input = answer_input(&card, answer, note).expect("an input");
        match input.decode_body() {
            Some(wire::KindInput::Acp(wire::AcpInput {
                of: Some(wire::acp_input::Of::Answer(answer)),
            })) => answer,
            other => panic!("not an ACP answer: {other:?}"),
        }
    };
    for (choice, id) in card.choices.iter().zip(["once", "always", "no", "never"]) {
        let answer = sent(&choice.answer, "");
        assert_eq!(answer.ask_key, "\"7\"");
        assert_eq!(answer.of, Some(wire::acp_answer::Of::Option(id.into())));
    }
    let instead = |of: Option<wire::acp_answer::Of>| match of {
        Some(wire::acp_answer::Of::Reply(reply)) => reply.text,
        other => panic!("not a reply: {other:?}"),
    };
    assert_eq!(
        instead(sent(&card.choices[2].answer, "use the fixture").of),
        "use the fixture"
    );
    let reply = reply_answer(&card, "talk first", &[]);
    assert_eq!(instead(sent(&reply, "").of), "talk first");
}

/// Model, thought level, permission and mode are picked from what the
/// agent offers and sent in its own arm; nothing steps its permission.
#[test]
fn an_acp_agents_settings_go_in_its_own_arm() {
    for (change, expect) in [
        (
            SettingChange::Model("m".into()),
            wire::acp_input::Of::Model(wire::SetModel {
                model: Some("m".into()),
            }),
        ),
        (
            SettingChange::Effort("high".into()),
            wire::acp_input::Of::Effort(wire::SetEffort {
                effort: Some("high".into()),
            }),
        ),
        (
            SettingChange::Permission("x".into()),
            wire::acp_input::Of::Permission(wire::SetPermission { value: "x".into() }),
        ),
        (
            SettingChange::Mode("plan".into()),
            wire::acp_input::Of::Mode(wire::SetMode {
                value: "plan".into(),
            }),
        ),
    ] {
        let input = setting_input(Kind::Acp, &change).expect("an input");
        assert_eq!(
            input.decode_body(),
            Some(wire::KindInput::Acp(wire::AcpInput { of: Some(expect) }))
        );
    }
    assert_eq!(
        setting_input(Kind::Acp, &SettingChange::CyclePermission),
        None
    );
}

/// A new ACP agent starts as the provider its host offers, with what was
/// picked; a mode is sent only when one was.
#[test]
fn a_new_acp_agent_is_created_as_that_agent() {
    let chosen = NewAgentChoices {
        model: Some("m".into()),
        effort: Some("high".into()),
        permission: Some("plan".into()),
        mode: None,
    };
    let request = create_request("opencode", Kind::Acp, &chosen);
    assert_eq!(request.provider, "opencode");
    assert_eq!(
        request.settings,
        Some(wire::AgentSettings {
            model: Some("m".into()),
            effort: Some("high".into()),
            permission: Some("plan".into()),
            ..wire::AgentSettings::default()
        })
    );
}

/// Without a mode the agent marks as its normal one, a new agent's mode is
/// left unpicked: it starts on the agent's own default.
#[test]
fn a_new_acp_agent_starts_on_its_own_mode_unless_one_is_picked() {
    let catalogue = wire::Catalogue {
        modes: vec![
            wire::OfferedMode {
                value: "build".into(),
                settable: true,
                ..Default::default()
            },
            wire::OfferedMode {
                value: "plan".into(),
                settable: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let view = new_agent_settings(&catalogue, &NewAgentChoices::default());
    assert!(view.modes.iter().all(|mode| !mode.current));
    assert_eq!(view.changeable.mode, Changeable::Pick);
    let picked = new_agent_pick(
        &catalogue,
        &NewAgentChoices::default(),
        &NewAgentPick::Mode(Some("plan".into())),
    );
    assert_eq!(picked.mode.as_deref(), Some("plan"));
}

/// Before an agent's first conversation on a host, its host knows no models
/// for it (most ACP agents list them only in a session): a new agent then
/// offers no model or effort to pick, rather than an empty list, and starts
/// on the agent's own.
#[test]
fn a_new_acp_agent_with_no_catalogue_yet_offers_no_model_or_effort() {
    let view = new_agent_settings(&wire::Catalogue::default(), &NewAgentChoices::default());
    assert!(view.models.is_empty() && view.efforts.is_empty());
    assert_eq!(view.changeable.model, Changeable::NotOffered);
    assert_eq!(view.changeable.effort, Changeable::NotOffered);
    assert_eq!(view.changeable.permission, Changeable::NotOffered);
    assert_eq!(view.changeable.mode, Changeable::NotOffered);
}

/// An ACP item, numbered `order` in the session, keyed `key`.
fn acp_item(order: u64, key: &str, kind: wire::acp_item::Kind) -> wire::Item {
    wire::Item {
        key: key.into(),
        order,
        revision: order,
        kind: Kind::Acp as i32,
        body: wire::AcpItem { kind: Some(kind) }.encode_to_vec(),
        at_ms: order as i64 * 1_000,
        ..Default::default()
    }
}

fn acp_tool(title: &str, kind: wire::AcpToolKind, asks: &str) -> wire::acp_item::Kind {
    wire::acp_item::Kind::Tool(wire::AcpTool {
        title: title.into(),
        kind: kind as i32,
        state: wire::ToolState::Succeeded as i32,
        class: wire::ToolClass::Read as i32,
        ask_item_key: asks.into(),
        ..Default::default()
    })
}

/// A question tool's call names the ask that stands for it, and draws no
/// row and no step of its own: the question's row is its only trace, as
/// Claude's question is. A call naming none is a step as ever.
#[test]
fn a_question_tools_call_is_drawn_by_its_ask_alone() {
    use wire::acp_item::Kind as Acp;
    let mut state = SessionState::new(row(wire::Lifecycle::Live), CAP);
    state.update(Msg::Connection(ui_state::Connection::Live));
    state.update(event(
        session_event::Of::Snapshot(wire::Snapshot::default()),
    ));
    let question = wire::AskItem {
        ask: Some(wire::ask_item::Ask::Question(wire::QuestionAsk {
            questions: vec![wire::Question {
                question: "Tea or coffee?".into(),
                options: vec![wire::QuestionOption {
                    label: "Tea".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            provider_dialog: false,
        })),
        closed: None,
    };
    for item in [
        acp_item(1, "prompt:1", Acp::Prompt(wire::Prompt::default())),
        acp_item(
            2,
            "tool:read",
            acp_tool("Read notes.txt", wire::AcpToolKind::Read, ""),
        ),
        acp_item(
            3,
            "tool:ask",
            acp_tool("ask_user_question", wire::AcpToolKind::Other, "ask:ask-1"),
        ),
        acp_item(4, "ask:ask-1", Acp::Ask(question)),
    ] {
        state.update(event(session_event::Of::Item(item)));
    }
    state.update(event(session_event::Of::CaughtUp(wire::CaughtUp {
        revision: 0,
    })));
    let rows = crate::support::all_rows(&state, ToolRows::ShowAll);
    let at = |order: u64| rows.iter().find(|row| row.order == order).unwrap();
    assert!(matches!(at(3).kind, RowKind::Hidden), "{:?}", at(3).kind);
    assert!(at(3).collapsed);
    assert!(matches!(at(4).kind, RowKind::Ask(_)), "{:?}", at(4).kind);
    let read = at(2).run.clone().expect("the read is a step");
    assert_eq!(read.steps, 1);
    assert!(at(3).run.is_none());
}

/// Settling asks first when it cuts work short, or when nothing can resume
/// the agent after: what the host says its program can do decides that.
/// An idle agent a message resumes settles at once.
#[test]
fn settling_asks_when_it_cuts_work_or_ends_the_conversation_for_good() {
    let warning = |resumable: bool, phase: Phase| {
        let mut fleet = FleetState::new();
        let mut agent = row(wire::Lifecycle::Live);
        agent.status_mut().phase = phase as i32;
        for of in [
            wire::inventory_event::Of::Host(host(wire::AgentCapabilities::acp(
                resumable, false, false,
            ))),
            wire::inventory_event::Of::Agent(agent),
            wire::inventory_event::Of::CaughtUp(wire::CaughtUp { revision: 0 }),
        ] {
            fleet.update(FleetMsg::Event(Box::new(wire::InventoryEvent {
                of: Some(of),
            })));
        }
        let key = ui_state::AgentKey {
            host: b"host".to_vec(),
            agent: b"agent".to_vec(),
        };
        settle_warning(&fleet, &key).expect("a warning")
    };
    let idle = warning(true, Phase::Idle);
    assert!(!idle.cuts() && !idle.asks() && !idle.ends_for_good());
    let working = warning(true, Phase::Working);
    assert_eq!(working.turn, Some(SettleTurn::Working));
    assert!(working.asks() && !working.ends_for_good());
    let unresumable = warning(false, Phase::Idle);
    assert!(!unresumable.cuts() && unresumable.asks() && unresumable.ends_for_good());
    // A provider subagent goes with its parent and takes no messages: it
    // loses nothing of its own.
    let subagent = SettleWarning {
        provider_subagent: true,
        ..unresumable
    };
    assert!(!subagent.asks());
}

/// How a card may be answered is what its ask says, read where the ask is
/// decoded: an ACP question takes a note, a skip and words instead unless
/// it says it is the agent's own dialog; any ACP ask takes words instead,
/// stops the turn, and approves by no default.
#[test]
fn a_cards_rules_come_from_what_its_ask_says() {
    let question = |provider_dialog: bool| wire::AcpSnapshot {
        asks: vec![wire::AcpAsk {
            key: "ask-1".into(),
            item_key: "ask:ask-1".into(),
            body: Some(wire::acp_ask::Body::Question(wire::QuestionAsk {
                questions: vec![wire::Question {
                    question: "Tea or coffee?".into(),
                    ..Default::default()
                }],
                provider_dialog,
            })),
        }],
        ..Default::default()
    };
    let card = |body| {
        let state = session(
            wire::Lifecycle::Live,
            wire::AgentCapabilities::acp(true, true, false),
            body,
            Vec::new(),
        );
        ask_card(&state).expect("a card")
    };
    let asked = card(question(false));
    assert!(asked.question_note && asked.question_skip && asked.question_reply);
    assert!(asked.reply_instead && asked.stops_turn && !asked.default_to_no);
    let dialog = card(question(true));
    assert!(!dialog.question_note && !dialog.question_skip && !dialog.question_reply);
    let permission = card(permission_ask());
    assert!(permission.reply_instead && permission.stops_turn && !permission.default_to_no);
}

/// The tool servers an ACP agent reports (Grok's) are in the overview as
/// Claude's and Codex's are: the ones that failed, and those that want a
/// sign-in, said apart; a ready one is not listed.
#[test]
fn an_acp_agents_failed_servers_are_in_the_overview() {
    let server = |name: &str, status: wire::ToolServerStatus, error: &str| wire::ToolServer {
        name: name.into(),
        status: status as i32,
        error: error.into(),
    };
    let state = session(
        wire::Lifecycle::Live,
        wire::AgentCapabilities::default(),
        wire::AcpSnapshot {
            servers: vec![
                server("spec", wire::ToolServerStatus::Ready, ""),
                server(
                    "posthog",
                    wire::ToolServerStatus::NeedsAuth,
                    "Auth required",
                ),
                server("broken", wire::ToolServerStatus::Failed, "exited with 1"),
            ],
            ..Default::default()
        },
        Vec::new(),
    );
    let failed: Vec<(String, bool)> = overview(&state, None)
        .failed_servers
        .into_iter()
        .map(|server| (server.name, server.needs_auth))
        .collect();
    assert_eq!(
        failed,
        [("posthog".to_owned(), true), ("broken".to_owned(), false)]
    );
}
