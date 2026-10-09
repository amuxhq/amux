//! Whole-frame goldens reached through served hosts: a testnet desk and
//! laptop running real daemons and agent processes on the fake providers,
//! observed from the laptop's client service the way a terminal there
//! subscribes, and drawn by home and the chat into the test backend. Home
//! at two widths and with every standing, the hosts overlay, a chat, an
//! ask the chat cannot answer, a call running, and a replica chat across
//! its origin's rewind: rows kept on screen until the Reset's CaughtUp,
//! then the rebuilt transcript.
//!
//! Rewrite with `UPDATE_GOLDENS=1 just test-tui`; CI refuses to rewrite.
//!
//! Unix only: every served host here runs terminal Claude agents, which
//! Windows does not host (see docs/ARCHITECTURE.md, "Windows, as a stated
//! cost"); the fleet and chat views these frames draw are the same code on
//! every platform and keep their component goldens there.
#![cfg(unix)]
#![cfg(feature = "fixtures")]

mod common;

use std::collections::HashMap;
use std::time::Duration;

use common::{assert_golden, capture};
use provider_fakes::script::{Ask, Outcome, Step, Tool, ToolClass};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use testnet::observe::{caught_up, inventory_caught_up};
use testnet::{AgentDecl, FakeKind, JournalCut, Net, PATIENCE, Topology};
use tui::chat::ChatView;
use tui::fleet::FleetView;
use tui::{ColorMode, Theme};
use ui_state::{
    AgentKey, Attention, Connection, FleetMsg, FleetState, ItemClass, Msg, SessionState,
};
use ui_view::SessionLine;
use wire::{InventoryEvent, SessionEvent, session_event};

fn theme() -> Theme {
    Theme::dark(ColorMode::TrueColor)
}

fn text(chunk: &str) -> Step {
    Step::Text {
        chunks: vec![chunk.to_owned()],
    }
}

fn topology() -> Topology {
    Topology::new()
        .host("desk")
        .host("laptop")
        .link("desk", "laptop")
}

/// The served agents, in the order home lists them: each starts once the
/// one before it has settled, since started together their first turns
/// race and so would the rows. Each works in `/`, the same on every
/// machine, so the header's folder never moves.
fn agents() -> [AgentDecl; 3] {
    [
        AgentDecl::new("coder", "desk")
            .cwd("/")
            .kind(FakeKind::Codex)
            .steps(vec![text("Hello from Codex."), Step::TurnEnd])
            .prompt("Say hello."),
        AgentDecl::new("worker", "desk")
            .cwd("/")
            .kind(FakeKind::ClaudeSdk)
            .steps(vec![
                text("Ready when you are."),
                Step::TurnEnd,
                text("Working on the relay tests."),
                Step::WaitFor {
                    path: "hold".into(),
                },
                Step::TurnEnd,
            ])
            .prompt("Get ready."),
        AgentDecl::new("scout", "desk")
            .cwd("/")
            .kind(FakeKind::ClaudePty)
            .steps(vec![
                text("pty.sock is named in crates/agent-dir/src/lib.rs."),
                Step::TurnEnd,
            ])
            .prompt("Find where the attach socket is named."),
    ]
}

/// Spawns `decl` and returns once it has settled after its first turn.
/// Policy time moves a second first: the net's clock stands still
/// otherwise, and home orders agents by when each was made.
async fn spawn_settled(net: &mut Net, decl: AgentDecl) {
    net.advance(Duration::from_secs(1)).unwrap();
    let name = decl.name.clone();
    net.spawn(decl).await.unwrap();
    net.settle(&name).await.unwrap();
}

/// The fleet as `host`'s inventory says it once every agent is idle.
async fn fleet_at(net: &Net, host: &str) -> FleetState {
    let mut inventory = net.observe_inventory(host).await.unwrap();
    let events: Vec<InventoryEvent> = inventory
        .observe_until(
            |events| {
                // Every agent through its first turn, so no row is caught
                // mid-start.
                let agents = testnet::observe::inventory_agents(events);
                inventory_caught_up(events)
                    && agents.len() == 3
                    && agents.iter().all(|agent| {
                        agent.lifecycle() == wire::Lifecycle::Live
                            && agent.phase() == wire::Phase::Idle
                    })
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .to_vec();
    let mut fleet = FleetState::new();
    fleet.update(FleetMsg::Connection(Connection::Live));
    for event in events {
        fleet.update(FleetMsg::Event(Box::new(event)));
    }
    fleet
}

/// The fleet as `host`'s inventory says it once `agent` is listed.
async fn fleet_listing(net: &Net, host: &str, agent: &[u8]) -> FleetState {
    let mut inventory = net.observe_inventory(host).await.unwrap();
    let events: Vec<InventoryEvent> = inventory
        .observe_until(
            |events| {
                inventory_caught_up(events)
                    && testnet::observe::inventory_agents(events)
                        .iter()
                        .any(|listed| listed.agent_id == agent)
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .to_vec();
    let mut fleet = FleetState::new();
    fleet.update(FleetMsg::Connection(Connection::Live));
    for event in events {
        fleet.update(FleetMsg::Event(Box::new(event)));
    }
    fleet
}

/// The fleet as `host`'s inventory says it once `ready` holds.
async fn fleet_when(net: &Net, host: &str, ready: impl Fn(&FleetState) -> bool) -> FleetState {
    let replay = |events: &[InventoryEvent]| {
        let mut fleet = FleetState::new();
        fleet.update(FleetMsg::Connection(Connection::Live));
        for event in events {
            fleet.update(FleetMsg::Event(Box::new(event.clone())));
        }
        fleet
    };
    let mut inventory = net.observe_inventory(host).await.unwrap();
    let events = inventory
        .observe_until(
            |events| inventory_caught_up(events) && ready(&replay(events)),
            PATIENCE,
        )
        .await
        .unwrap()
        .to_vec();
    replay(&events)
}

/// A chat's state after `events`, with its fleet entry and host.
fn chat_state(fleet: &FleetState, agent: &[u8], events: &[SessionEvent]) -> SessionState {
    let entry = fleet.find(agent).expect("the agent is listed").clone();
    let host = fleet.host(&entry.host_id).cloned();
    let mut state = SessionState::new(entry, tui::chat::layout::CAP as usize);
    state.update(Msg::Connection(Connection::Live));
    if let Some(host) = host {
        state.update(Msg::Host(host));
    }
    for event in events {
        state.update(Msg::Event(event.clone()));
    }
    state
}

/// What each named agent's session on `host` says for its home row, read
/// once the session holds what the agent's state in `fleet` calls for: its
/// ask, its step, or what it last said.
async fn session_lines(
    net: &Net,
    fleet: &FleetState,
    host: &str,
    names: &[&str],
) -> HashMap<AgentKey, SessionLine> {
    let mut lines = HashMap::new();
    for name in names {
        let id = net.agent(name).unwrap().id;
        let entry = fleet.find(id.as_bytes()).expect("the agent is listed");
        let attention = ui_state::attention(entry);
        let line_of = |events: &[SessionEvent]| {
            ui_view::session_line(&chat_state(fleet, id.as_bytes(), events), 0)
        };
        let mut session = net.observe(host, name, 20).await.unwrap();
        let events = session
            .observe_until(
                |events| {
                    let line = line_of(events);
                    caught_up(events)
                        && match attention {
                            Attention::NeedsYou => line.ask.is_some(),
                            Attention::Working => line.step.is_some(),
                            Attention::Idle => line.last_said.is_some(),
                            Attention::Starting | Attention::Exited => true,
                        }
                },
                PATIENCE,
            )
            .await
            .unwrap()
            .to_vec();
        lines.insert(ui_state::agent_key(entry), line_of(&events));
    }
    lines
}

/// The newest item's time: "now" for ages, so none of them moves.
fn now_of(events: &[SessionEvent]) -> i64 {
    events
        .iter()
        .filter_map(|event| match &event.of {
            Some(session_event::Of::Item(item)) => Some(item.at_ms),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

fn draw_chat(state: &SessionState, width: u16, height: u16) -> Buffer {
    let mut view = ChatView::new(state.agent().agent_id.clone(), 0, false);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            view.draw(frame, area, state, None, None, 0, theme());
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn draw_fleet(
    fleet: &FleetState,
    lines: &HashMap<AgentKey, SessionLine>,
    now_ms: i64,
    width: u16,
    height: u16,
) -> Buffer {
    draw_fleet_view(
        &mut FleetView::default(),
        fleet,
        lines,
        now_ms,
        width,
        height,
    )
}

fn draw_fleet_view(
    view: &mut FleetView,
    fleet: &FleetState,
    lines: &HashMap<AgentKey, SessionLine>,
    now_ms: i64,
    width: u16,
    height: u16,
) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            view.draw(frame, area, fleet, lines, None, now_ms, theme());
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn frame(name: &str, buffer: &Buffer) {
    assert!(
        tui::vocabulary::FRAMES
            .iter()
            .any(|(known, _)| *known == name),
        "{name} is not described in tui::vocabulary::FRAMES"
    );
    assert_golden(
        &format!("frame_{name}"),
        &capture(&without_clock(buffer), theme()),
    );
}

/// The frame with every time of day ("14:59") read as "hh:mm" and every
/// measured duration ("Worked 1s", "Working · 0ms") as "<t>", cell for
/// cell, styles kept: these follow the wall clock and the machine's pace.
fn without_clock(buffer: &Buffer) -> Buffer {
    let mut out = buffer.clone();
    let area = buffer.area;
    for y in 0..area.height {
        let row: String = (0..area.width)
            .map(|x| buffer[(x, y)].symbol().to_owned())
            .collect();
        for lead in ["Worked ", "Working · "] {
            let Some(at) = row.find(lead) else {
                continue;
            };
            // Cells, not bytes: everything before it on these rows is one
            // cell a character.
            let start = row[..at].chars().count() + lead.chars().count();
            let mut x = start as u16;
            let mut masked = "<t>".chars();
            while x < area.width && !buffer[(x, y)].symbol().trim().is_empty() {
                out[(x, y)].set_char(masked.next().unwrap_or(' '));
                x += 1;
            }
            for mask in masked {
                if x >= area.width {
                    break;
                }
                out[(x, y)].set_char(mask);
                x += 1;
            }
        }
    }
    let digit = |x: u16, y: u16| {
        buffer[(x, y)].symbol().chars().all(|c| c.is_ascii_digit())
            && !buffer[(x, y)].symbol().is_empty()
    };
    for y in 0..area.height {
        for x in 0..area.width.saturating_sub(4) {
            let bounded = (x == 0 || !digit(x - 1, y)) && (x + 5 >= area.width || !digit(x + 5, y));
            if bounded
                && digit(x, y)
                && digit(x + 1, y)
                && buffer[(x + 2, y)].symbol() == ":"
                && digit(x + 3, y)
                && digit(x + 4, y)
            {
                for (i, mask) in "hh:mm".chars().enumerate() {
                    out[(x + i as u16, y)].set_char(mask);
                }
            }
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn served_frames_match_their_goldens() {
    let mut net = Net::start(topology()).await.unwrap();
    for decl in agents() {
        spawn_settled(&mut net, decl).await;
    }
    let worker = net.agent("worker").unwrap().id;
    let fleet = fleet_at(&net, "laptop").await;
    let mut chat = net.observe("laptop", "worker", 20).await.unwrap();
    let events = chat
        .observe_until(
            |events| {
                caught_up(events)
                    && events.iter().any(|event| {
                        matches!(&event.of, Some(session_event::Of::Item(item))
                            if item.text.contains("Ready when you are"))
                    })
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .to_vec();
    let now = now_of(&events) + 1_000;
    let lines = session_lines(&net, &fleet, "laptop", &["coder", "worker", "scout"]).await;
    frame("home", &draw_fleet(&fleet, &lines, now, 110, 16));
    frame("home_60col", &draw_fleet(&fleet, &lines, now, 60, 16));
    let mut state = chat_state(&fleet, worker.as_bytes(), &events);
    // What the agent offers, fetched as a chat on screen fetches it.
    let (hash, catalogue) = net.catalogue("laptop", "worker").await.unwrap();
    state.update(Msg::Catalogue { hash, catalogue });
    frame("chat", &draw_chat(&state, 110, 24));

    // A terminal Claude on the desk shows a tool server's dialog in its own
    // terminal: the laptop's chat docks the escape where the composer was,
    // with the conversation still above it.
    let gatekeeper = net
        .spawn(
            AgentDecl::new("gatekeeper", "desk")
                .cwd("/")
                .kind(FakeKind::ClaudePty)
                .steps(vec![
                    text("Signing in to the tracker first."),
                    Step::Ask(Ask::ToolServerDialog {
                        server: "tracker".to_owned(),
                        tool: "sign_in".to_owned(),
                        link: false,
                        wait_for: None,
                        output: String::new(),
                    }),
                    Step::TurnEnd,
                ])
                .prompt("File the flaky relay test."),
        )
        .await
        .unwrap();
    let listed = fleet_listing(&net, "laptop", &gatekeeper.agent_id).await;
    let mut asking = net.observe("laptop", "gatekeeper", 20).await.unwrap();
    let events = asking
        .observe_until(
            // Terminal Claude's rows come from its transcript in the order
            // Claude wrote them, so the text that introduced the call sits
            // above it. The dialog is announced by a hook: its card can open
            // before the call's row lands, and its own row waits for that
            // row, so the frame waits for the card to point at its row.
            |events| {
                caught_up(events)
                    && ui_view::ask_card(&chat_state(&listed, &gatekeeper.agent_id, events))
                        .is_some_and(|card| !card.item_key.is_empty())
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .to_vec();
    let state = chat_state(&listed, &gatekeeper.agent_id, &events);
    assert!(
        matches!(
            ui_view::ask_card(&state).map(|card| card.body),
            Some(ui_view::AskBody::Unanswerable { .. })
        ),
        "the dialog is an ask the chat cannot answer"
    );
    frame("ask_escape", &draw_chat(&state, 110, 24));

    // A terminal Claude's call opens on its transcript row, as it starts,
    // so the chat draws it running, below the text that introduced it,
    // while it runs.
    let runner = net
        .spawn(
            AgentDecl::new("runner", "desk")
                .cwd("/")
                .kind(FakeKind::ClaudePty)
                .steps(vec![
                    text("Reading the relay logs first."),
                    Step::Tool(Tool {
                        name: Some("Read".to_owned()),
                        class: ToolClass::Exploration,
                        input: Some(serde_json::json!({ "file_path": "logs/relay.log" })),
                        outcome: Outcome::default(),
                        wait_for: Some("never".into()),
                    }),
                    Step::TurnEnd,
                ])
                .prompt("Why did the relay drop?"),
        )
        .await
        .unwrap();
    let listed = fleet_listing(&net, "laptop", &runner.agent_id).await;
    let mut running = net.observe("laptop", "runner", 20).await.unwrap();
    let events = running
        .observe_until(
            |events| {
                caught_up(events)
                    && chat_state(&listed, &runner.agent_id, events)
                        .transcript()
                        .iter()
                        .any(|held| matches!(&held.class, ItemClass::Tool(tool) if tool.in_flight))
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .to_vec();
    let state = chat_state(&listed, &runner.agent_id, &events);
    frame("running_call", &draw_chat(&state, 110, 24));

    // The origin rewinds to a checkpoint taken before the second turn: the
    // laptop's replica is Reset while its rows stay on screen, and swaps to
    // the rebuilt transcript at CaughtUp.
    let cut = net.journal_end("worker").unwrap();
    net.checkpoint_host("desk").await.unwrap();
    net.send("worker", "Run the relay tests.").await.unwrap();
    let before = chat
        .observe_until(
            |events| {
                events.iter().any(|event| {
                    matches!(&event.of, Some(session_event::Of::Item(item))
                        if item.text.contains("Working on the relay tests"))
                })
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .len();
    net.rewind_host(
        "desk",
        &[JournalCut {
            agent: "worker".to_owned(),
            byte: cut,
        }],
    )
    .await
    .unwrap();
    // The laptop's source ends with the desk's daemon (Detached) before
    // the next one opens on the rebuilt journal (Reset, then CaughtUp), so
    // the frame before the swap shows the desk away. Waiting for the
    // Detached pins that order: the frame is composed from events, and a
    // Reset read before the detach was noticed would draw "catching up".
    let events = chat
        .observe_until(
            |events| {
                let after = &events[before..];
                let Some(detached) = after
                    .iter()
                    .position(|event| matches!(event.of, Some(session_event::Of::Detached(_))))
                else {
                    return false;
                };
                after[detached..]
                    .iter()
                    .position(|event| matches!(event.of, Some(session_event::Of::Reset(_))))
                    .is_some_and(|reset| caught_up(&after[detached + reset..]))
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .to_vec();
    let detached = before
        + events[before..]
            .iter()
            .position(|event| matches!(event.of, Some(session_event::Of::Detached(_))))
            .unwrap();
    let reset = detached
        + events[detached..]
            .iter()
            .position(|event| matches!(event.of, Some(session_event::Of::Reset(_))))
            .unwrap();
    let swap = reset
        + events[reset..]
            .iter()
            .position(|event| matches!(event.of, Some(session_event::Of::CaughtUp(_))))
            .unwrap();
    let (hash, catalogue) = net.catalogue("laptop", "worker").await.unwrap();
    let offered = Msg::Catalogue { hash, catalogue };
    let mut pending = chat_state(&fleet, worker.as_bytes(), &events[..swap]);
    pending.update(offered.clone());
    assert!(pending.reset_pending(), "the Reset waits for its CaughtUp");
    frame("rewind_before_swap", &draw_chat(&pending, 110, 24));
    let mut swapped = chat_state(&fleet, worker.as_bytes(), &events[..=swap]);
    swapped.update(offered);
    assert!(!swapped.reset_pending());
    frame("rewind_after_swap", &draw_chat(&swapped, 110, 24));

    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// Every standing an agent can have on the fleet at once, as the laptop
/// sees it: one asking for permission, one working, a family whose
/// one-shot child finished, one whose provider exited, one idle, and one on
/// a host that has gone offline.
#[tokio::test(flavor = "multi_thread")]
async fn a_fleet_of_every_standing_matches_its_golden() {
    let topology = Topology::new()
        .host("desk")
        .host("laptop")
        .host("studio")
        .link("desk", "laptop")
        .link("laptop", "studio")
        .agent(
            AgentDecl::new("fixer", "desk")
                .cwd("/")
                .kind(FakeKind::ClaudeSdk)
                .steps(vec![
                    text("Running the auth tests first."),
                    Step::Ask(Ask::Permission(Tool {
                        name: Some("Bash".to_owned()),
                        class: ToolClass::Consequential,
                        input: Some(serde_json::json!({ "command": "cargo test -p auth" })),
                        outcome: Outcome::default(),
                        wait_for: None,
                    })),
                    Step::TurnEnd,
                ])
                .prompt("Fix the auth bug."),
        )
        .agent(
            AgentDecl::new("runner", "desk")
                .cwd("/")
                .kind(FakeKind::ClaudePty)
                .steps(vec![
                    text("Running the relay tests."),
                    Step::WaitFor {
                        path: "never".into(),
                    },
                    Step::TurnEnd,
                ])
                .prompt("Run the relay tests."),
        )
        .agent(
            AgentDecl::new("crasher", "desk")
                .cwd("/")
                .kind(FakeKind::Codex)
                .steps(vec![text("Starting."), Step::Exit { code: 1 }])
                .prompt("Migrate the store."),
        );
    let mut net = Net::start(topology).await.unwrap();
    // Each starts once the one before it has settled: started together,
    // their first turns race and so would the rows.
    let idle = [
        AgentDecl::new("scout", "desk")
            .cwd("/")
            .kind(FakeKind::ClaudePty)
            .steps(vec![text("Found it."), Step::TurnEnd])
            .prompt("Find the socket name."),
        AgentDecl::new("archivist", "studio")
            .cwd("/")
            .kind(FakeKind::ClaudePty)
            .steps(vec![text("Archived."), Step::TurnEnd])
            .prompt("Archive the old logs."),
        AgentDecl::new("planner", "desk")
            .cwd("/")
            .kind(FakeKind::ClaudeSdk)
            .steps(vec![
                text("Handing the specs to a helper."),
                Step::TurnEnd,
                text("The specs are updated."),
                Step::TurnEnd,
            ])
            .prompt("Update the specs."),
    ];
    for decl in idle {
        spawn_settled(&mut net, decl).await;
    }
    // Every studio agent listed on the laptop before the studio goes.
    let archivist = net.agent("archivist").unwrap().id;
    fleet_when(&net, "laptop", |fleet| {
        fleet
            .find(archivist.as_bytes())
            .is_some_and(|agent| agent.phase() == wire::Phase::Idle)
    })
    .await;
    let planner = net.agent("planner").unwrap().id;
    net.advance(Duration::from_secs(1)).unwrap();
    let specs = net
        .spawn_child(
            "planner",
            AgentDecl::new("specs", "desk")
                .cwd("/")
                .kind(FakeKind::ClaudeSdk)
                .steps(vec![text("3 specs updated."), Step::TurnEnd])
                .prompt("Update the three specs."),
        )
        .await
        .unwrap();
    net.stop_daemon("studio").await.unwrap();
    let ids = |name: &str| net.agent(name).unwrap().id.as_bytes().to_vec();
    let (fixer, runner, crasher, scout) =
        (ids("fixer"), ids("runner"), ids("crasher"), ids("scout"));
    let fleet = fleet_when(&net, "laptop", |fleet| {
        let phase = |id: &[u8]| fleet.find(id).map(|agent| agent.phase());
        let exited = |id: &[u8]| {
            fleet
                .find(id)
                .map(|agent| (agent.lifecycle(), agent.exit_cause.clone()))
        };
        phase(&fixer) == Some(wire::Phase::NeedsYou)
            && phase(&runner) == Some(wire::Phase::Working)
            && phase(&scout) == Some(wire::Phase::Idle)
            && phase(planner.as_bytes()) == Some(wire::Phase::Idle)
            && exited(&specs.agent_id)
                .is_some_and(|(lifecycle, _)| lifecycle == wire::Lifecycle::Exited)
            && exited(&crasher).is_some_and(|(lifecycle, _)| lifecycle == wire::Lifecycle::Exited)
            && fleet
                .hosts()
                .any(|host| host.name == "studio" && host.presence() == wire::Presence::Offline)
    })
    .await;
    let lines = session_lines(
        &net,
        &fleet,
        "laptop",
        &["fixer", "runner", "scout", "planner"],
    )
    .await;
    // Ages are drawn against the newest activity, so every one reads alike.
    let now = fleet
        .agents()
        .map(|agent| agent.phase_since_ms)
        .max()
        .unwrap()
        + 1_000;
    let mut view = FleetView {
        local_host: net.host("laptop").unwrap().host_id.as_bytes().to_vec(),
        ..FleetView::default()
    };
    // The planner's family unfolded, as Right does on its row.
    let family = fleet
        .agents()
        .find(|agent| agent.agent_id == planner.as_bytes())
        .map(ui_state::agent_key)
        .unwrap();
    view.select(family);
    view.key(
        &fleet,
        crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Right),
    );
    frame(
        "home_standings",
        &draw_fleet_view(&mut view, &fleet, &lines, now, 110, 34),
    );
    frame(
        "home_standings_80col",
        &draw_fleet_view(&mut view, &fleet, &lines, now, 80, 34),
    );
    view.key(
        &fleet,
        crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('h')),
    );
    frame(
        "hosts",
        &draw_fleet_view(&mut view, &fleet, &lines, now, 110, 34),
    );

    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}
