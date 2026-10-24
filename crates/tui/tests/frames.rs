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
    // Home orders rows by when each agent entered its phase. The net's
    // daemons run on its driven clock but the agent processes on the wall
    // clock, and an agent stamps a phase with the later of its own time and
    // its creation time on its daemon. Moved well ahead of the wall clock
    // here, every agent started below is stamped with its creation time, in
    // the order it was made, after the agents above (stamped on the wall
    // clock) however long the run takes; still under a minute, so every
    // age reads "now".
    net.advance(Duration::from_secs(20)).unwrap();
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
                .map(|agent| (agent.lifecycle(), agent.exit_cause))
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
                .any(|host| host.name() == "studio" && host.offline().is_some())
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

// --- a draft across leaving the chat -----------------------------------------

/// The client's next event from a task it spawned (a chat opening), applied.
async fn next_app_event(app: &mut tui::App) {
    let event = tokio::time::timeout(PATIENCE, app.receiver.recv())
        .await
        .expect("the client's task answers")
        .expect("the client's event channel is open");
    app.event(event);
}

/// Opens `id`'s chat the way Enter on its home row does, and waits for it.
async fn open_chat(app: &mut tui::App, id: &[u8]) {
    app.config.initial_chat = Some(id.to_vec());
    app.fleet_changed();
    next_app_event(app).await;
    assert!(
        app.chat
            .as_ref()
            .is_some_and(|chat| chat.view.agent_id == id),
        "the chat opened"
    );
}

/// docs/TERMINAL.md: the client owns the drafts, and "the composer takes a
/// draft at any time". A half-written prompt is the person's work: going
/// home with the leader then `s` and coming back to the same chat must find
/// it in the composer, as the phone keeps it (docs/IOS.md: leaving the page
/// loses none of the draft).
#[tokio::test(flavor = "multi_thread")]
async fn a_draft_left_in_a_chat_is_in_its_composer_when_the_chat_opens_again() {
    let net = Net::start(
        Topology::new().host("desk").agent(
            AgentDecl::new("worker", "desk")
                .kind(FakeKind::ClaudeSdk)
                .steps(vec![text("Ready."), Step::TurnEnd])
                .prompt("go"),
        ),
    )
    .await
    .unwrap();
    net.settle("worker").await.unwrap();
    let worker = net.agent("worker").unwrap().id;
    let client: std::sync::Arc<dyn client::Client> =
        std::sync::Arc::new(client::InProcess::new(net.client("desk").unwrap()));
    let fleet = ui_runtime::Fleet::connect(client.clone(), client::SystemClock)
        .await
        .unwrap();
    let config = tui::TuiConfig {
        working_dir: "/".into(),
        leader: 'a',
        theme: theme(),
        initial_chat: None,
        attach: false,
        version: String::new(),
        local_host: net.host("desk").unwrap().host_id.as_bytes().to_vec(),
        layout: None,
        reports: None,
        chat_in: tui::setup::ChatIn::Amux,
        defaults: tui::setup::Defaults::default(),
    };
    let mut app = tui::App::new(client, fleet, config);
    let mut changed = app.fleet.changed();
    while app.fleet.state().find(worker.as_bytes()).is_none() {
        tokio::time::timeout(PATIENCE, changed.changed())
            .await
            .expect("the fleet lists the worker")
            .unwrap();
    }

    open_chat(&mut app, worker.as_bytes()).await;
    for c in "important unsent draft".chars() {
        app.key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char(c),
        ));
    }
    assert_eq!(
        app.chat.as_ref().unwrap().view.editor.text(),
        "important unsent draft"
    );
    // The leader, then `s`: home.
    app.key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('a'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    app.key(crossterm::event::KeyEvent::from(
        crossterm::event::KeyCode::Char('s'),
    ));
    assert!(app.chat.is_none(), "home");

    open_chat(&mut app, worker.as_bytes()).await;
    assert_eq!(
        app.chat.as_ref().unwrap().view.editor.text(),
        "important unsent draft",
        "the draft waits in the chat's composer"
    );

    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

// --- the client closing, and a chat closing under the person ----------------

/// A served net of idle headless Claude agents on desk, and a client of desk.
async fn desk_of(names: &[&str]) -> (Net, std::sync::Arc<dyn client::Client>, tui::TuiConfig) {
    let mut topology = Topology::new().host("desk");
    for name in names {
        topology = topology.agent(
            AgentDecl::new(name, "desk")
                .kind(FakeKind::ClaudeSdk)
                .steps(vec![text("Ready."), Step::TurnEnd])
                .prompt("go"),
        );
    }
    let net = Net::start(topology).await.unwrap();
    for name in names {
        net.settle(name).await.unwrap();
    }
    let client: std::sync::Arc<dyn client::Client> =
        std::sync::Arc::new(client::InProcess::new(net.client("desk").unwrap()));
    let config = tui::TuiConfig {
        working_dir: "/".into(),
        leader: 'a',
        theme: theme(),
        initial_chat: None,
        attach: false,
        version: String::new(),
        local_host: net.host("desk").unwrap().host_id.as_bytes().to_vec(),
        layout: None,
        reports: None,
        chat_in: tui::setup::ChatIn::Amux,
        defaults: tui::setup::Defaults::default(),
    };
    (net, client, config)
}

/// A client of desk whose fleet lists every one of `ids`.
async fn app_listing(
    client: std::sync::Arc<dyn client::Client>,
    config: tui::TuiConfig,
    ids: &[&[u8]],
) -> tui::App {
    let fleet = ui_runtime::Fleet::connect(client.clone(), client::SystemClock)
        .await
        .unwrap();
    let app = tui::App::new(client, fleet, config);
    let mut changed = app.fleet.changed();
    while ids.iter().any(|id| app.fleet.state().find(id).is_none()) {
        tokio::time::timeout(PATIENCE, changed.changed())
            .await
            .expect("the fleet lists the agents")
            .unwrap();
    }
    app
}

fn press(app: &mut tui::App, code: crossterm::event::KeyCode) {
    app.key(crossterm::event::KeyEvent::from(code));
}

/// A draft is the person's work and outlives the client: closing the
/// terminal client (or detaching) and opening it again finds the unsent
/// words in the agent's composer. Kept on this device, never synced. They
/// were held only in the client's memory and lost.
#[tokio::test(flavor = "multi_thread")]
async fn a_draft_outlives_the_terminal_client_closing() {
    let (net, client, config) = desk_of(&["worker"]).await;
    let worker = net.agent("worker").unwrap().id;
    let mut app = app_listing(client.clone(), config.clone(), &[worker.as_bytes()]).await;
    open_chat(&mut app, worker.as_bytes()).await;
    for c in "important unsent draft".chars() {
        press(&mut app, crossterm::event::KeyCode::Char(c));
    }
    drop(app);

    let mut app = app_listing(client, config, &[worker.as_bytes()]).await;
    open_chat(&mut app, worker.as_bytes()).await;
    assert_eq!(
        app.chat.as_ref().unwrap().view.editor.text(),
        "important unsent draft",
        "the draft did not outlive the client"
    );
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// A client of the daemon whose saves of an emptied draft never land while
/// `hold` is set: the clearing a person did is still on its way. It notes
/// the agent of every draft saved, and refuses the first save of a draft
/// reading `refuse_once`, as a daemon briefly away would.
struct SlowClearing {
    inner: std::sync::Arc<dyn client::Client>,
    hold: std::sync::atomic::AtomicBool,
    saved: std::sync::Mutex<Vec<Vec<u8>>>,
    refuse_once: std::sync::Mutex<Option<String>>,
}

#[async_trait::async_trait]
impl client::Client for SlowClearing {
    async fn save_draft(&self, request: wire::SaveDraftRequest) -> Result<(), client::RpcError> {
        self.saved.lock().unwrap().push(request.agent_id.clone());
        let text = request.draft.as_ref().map(|draft| draft.text.clone());
        {
            let mut refuse = self.refuse_once.lock().unwrap();
            if refuse.is_some() && *refuse == text {
                *refuse = None;
                return Err(client::RpcError::Transport(
                    "the daemon is restarting".into(),
                ));
            }
        }
        let empty = request
            .draft
            .as_ref()
            .is_none_or(|draft| draft.text.is_empty() && draft.attachments.is_empty());
        if empty && self.hold.load(std::sync::atomic::Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        self.inner.save_draft(request).await
    }
    async fn get_draft(
        &self,
        request: wire::GetDraftRequest,
    ) -> Result<wire::ComposerDraft, client::RpcError> {
        self.inner.get_draft(request).await
    }
    async fn subscribe_inventory(
        &self,
    ) -> Result<client::EventStream<wire::InventoryEvent>, client::RpcError> {
        self.inner.subscribe_inventory().await
    }
    async fn resolve_agent(
        &self,
        request: wire::ResolveAgentRequest,
    ) -> Result<wire::Agent, client::RpcError> {
        self.inner.resolve_agent(request).await
    }
    async fn subscribe(
        &self,
        request: wire::SubscribeRequest,
    ) -> Result<client::EventStream<wire::SessionEvent>, client::RpcError> {
        self.inner.subscribe(request).await
    }
    async fn fetch(
        &self,
        request: wire::FetchRequest,
    ) -> Result<wire::FetchResponse, client::RpcError> {
        self.inner.fetch(request).await
    }
    async fn get(&self, request: wire::GetRequest) -> Result<wire::Item, client::RpcError> {
        self.inner.get(request).await
    }
    async fn send_input(
        &self,
        request: wire::SendInputRequest,
    ) -> Result<wire::SendInputResponse, client::RpcError> {
        self.inner.send_input(request).await
    }
    async fn create_agent(
        &self,
        request: wire::CreateAgentRequest,
    ) -> Result<wire::Agent, client::RpcError> {
        self.inner.create_agent(request).await
    }
    async fn rename_agent(
        &self,
        request: wire::RenameAgentRequest,
    ) -> Result<wire::Agent, client::RpcError> {
        self.inner.rename_agent(request).await
    }
    async fn stop_agent(&self, request: wire::StopAgentRequest) -> Result<(), client::RpcError> {
        self.inner.stop_agent(request).await
    }
    async fn resume_agent(
        &self,
        request: wire::ResumeAgentRequest,
    ) -> Result<wire::Agent, client::RpcError> {
        self.inner.resume_agent(request).await
    }
    async fn delete_agent(
        &self,
        request: wire::DeleteAgentRequest,
    ) -> Result<(), client::RpcError> {
        self.inner.delete_agent(request).await
    }
    async fn send_message(
        &self,
        request: wire::SendMessageRequest,
    ) -> Result<wire::SendMessageResponse, client::RpcError> {
        self.inner.send_message(request).await
    }
    async fn put_blob(
        &self,
        request: wire::PutBlobRequest,
    ) -> Result<wire::BlobRef, client::RpcError> {
        self.inner.put_blob(request).await
    }
    async fn get_blob(
        &self,
        request: wire::GetBlobRequest,
    ) -> Result<wire::GetBlobResponse, client::RpcError> {
        self.inner.get_blob(request).await
    }
    async fn diff(&self, request: wire::DiffRequest) -> Result<wire::Diff, client::RpcError> {
        self.inner.diff(request).await
    }
    async fn get_catalogue(
        &self,
        request: wire::GetCatalogueRequest,
    ) -> Result<wire::CatalogueBlob, client::RpcError> {
        self.inner.get_catalogue(request).await
    }
    async fn list_repositories(
        &self,
        request: wire::ListRepositoriesRequest,
    ) -> Result<wire::ListRepositoriesResponse, client::RpcError> {
        self.inner.list_repositories(request).await
    }
    async fn dump(&self, request: wire::DumpRequest) -> Result<wire::DumpReport, client::RpcError> {
        self.inner.dump(request).await
    }
}

/// A draft the person cleared stays cleared while the daemon's copy is
/// still being emptied: reopening the chat at once finds the composer
/// empty, not the words this machine kept before, and quitting from home
/// waits for the emptying. The empty save went from a task nobody waited
/// on, and nothing here remembered the clearing.
#[tokio::test(flavor = "multi_thread")]
async fn a_cleared_draft_stays_cleared_before_the_daemon_hears_of_it() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (net, daemon, config) = desk_of(&["worker"]).await;
    let worker = net.agent("worker").unwrap().id;
    let stale = || wire::GetDraftRequest {
        agent_id: worker.as_bytes().to_vec(),
    };
    daemon
        .save_draft(wire::SaveDraftRequest {
            agent_id: worker.as_bytes().to_vec(),
            draft: Some(wire::ComposerDraft {
                text: "kept from before".into(),
                attachments: Vec::new(),
            }),
        })
        .await
        .unwrap();
    let slow = std::sync::Arc::new(SlowClearing {
        inner: daemon.clone(),
        hold: std::sync::atomic::AtomicBool::new(true),
        saved: Default::default(),
        refuse_once: Default::default(),
    });
    let client: std::sync::Arc<dyn client::Client> = slow.clone();
    let mut app = app_listing(client, config, &[worker.as_bytes()]).await;
    let home = |app: &mut tui::App| {
        app.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        press(app, KeyCode::Char('s'));
        assert!(app.chat.is_none(), "home");
    };
    open_chat(&mut app, worker.as_bytes()).await;
    assert_eq!(
        app.chat.as_ref().unwrap().view.editor.text(),
        "kept from before"
    );
    app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.chat.as_ref().unwrap().view.editor.is_empty(), "cleared");
    home(&mut app);

    open_chat(&mut app, worker.as_bytes()).await;
    assert_eq!(
        app.chat.as_ref().unwrap().view.editor.text(),
        "",
        "the cleared draft came back from the daemon's old copy"
    );
    home(&mut app);

    // Quitting from home waits for the emptying, whatever became of the
    // saves sent as the chat closed.
    slow.hold.store(false, std::sync::atomic::Ordering::SeqCst);
    tokio::time::timeout(PATIENCE, app.keep_drafts())
        .await
        .expect("the quit's save lands");
    let left = daemon.get_draft(stale()).await.unwrap();
    assert!(
        left.text.is_empty(),
        "the daemon still keeps {:?} after the client quit",
        left.text
    );
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// A chat that closes because its agent was deleted keeps no draft: the
/// words in its composer went back to this machine's daemon as the chat
/// closed, and again as the client quit, for an agent that no longer
/// exists.
#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_agents_chat_keeps_no_draft() {
    use crossterm::event::KeyCode;
    let (net, daemon, config) = desk_of(&["doomed"]).await;
    let doomed = net.agent("doomed").unwrap().id;
    let recording = std::sync::Arc::new(SlowClearing {
        inner: daemon.clone(),
        hold: std::sync::atomic::AtomicBool::new(false),
        saved: Default::default(),
        refuse_once: Default::default(),
    });
    let client: std::sync::Arc<dyn client::Client> = recording.clone();
    let mut app = app_listing(client, config, &[doomed.as_bytes()]).await;
    open_chat(&mut app, doomed.as_bytes()).await;
    for c in "half a thought".chars() {
        press(&mut app, KeyCode::Char(c));
    }

    daemon
        .delete_agent(wire::DeleteAgentRequest {
            agent_id: doomed.as_bytes().to_vec(),
        })
        .await
        .unwrap();
    tokio::time::timeout(PATIENCE, async {
        while app.chat.is_some() {
            app.housekeep();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the deleted agent's chat closes");
    tokio::time::timeout(PATIENCE, app.keep_drafts())
        .await
        .expect("the quit's save lands");

    assert!(
        !recording
            .saved
            .lock()
            .unwrap()
            .contains(&doomed.as_bytes().to_vec()),
        "the deleted agent's draft was saved"
    );
    let left = daemon
        .get_draft(wire::GetDraftRequest {
            agent_id: doomed.as_bytes().to_vec(),
        })
        .await
        .unwrap();
    assert!(left.text.is_empty(), "the daemon keeps {:?}", left.text);
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// When a chat closes under the person (its agent deleted elsewhere), the
/// rest of what they are typing never acts on home: "fix x" then Enter
/// opened the window to end the focused agent and confirmed it. Home holds
/// keys for the same half second an ask does; after that its keys work.
#[tokio::test(flavor = "multi_thread")]
async fn words_typed_into_a_chat_that_closes_under_the_person_end_no_other_agent() {
    use crossterm::event::KeyCode;
    let (net, client, config) = desk_of(&["doomed", "healthy"]).await;
    let doomed = net.agent("doomed").unwrap().id;
    let healthy = net.agent("healthy").unwrap().id;
    let mut app = app_listing(
        client.clone(),
        config,
        &[doomed.as_bytes(), healthy.as_bytes()],
    )
    .await;
    // Home's focus on healthy, as the person left it.
    press(&mut app, KeyCode::Char('/'));
    for c in "healthy".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down);
    open_chat(&mut app, doomed.as_bytes()).await;

    client
        .delete_agent(wire::DeleteAgentRequest {
            agent_id: doomed.as_bytes().to_vec(),
        })
        .await
        .unwrap();
    tokio::time::timeout(PATIENCE, async {
        while app.chat.is_some() {
            app.housekeep();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the deleted agent's chat closes");
    for c in "fix x".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    let ended = |app: &tui::App| {
        app.fleet
            .state()
            .find(healthy.as_bytes())
            .is_none_or(|agent| agent.lifecycle() != wire::Lifecycle::Live)
    };
    // A window: a key that home swallows leaves no mark to wait on.
    testnet::holds_for(
        "the words typed as the chat closed to leave healthy running",
        Duration::from_millis(300),
        || std::future::ready(!ended(&app)),
    )
    .await
    .expect("the words typed as the chat closed ended healthy");
    let mut changed = app.fleet.changed();
    // After half a second with no key, home's keys act again.
    tokio::time::sleep(Duration::from_millis(600)).await;
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Enter);
    tokio::time::timeout(PATIENCE, async {
        while !ended(&app) {
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("x then Enter on home ends the focused agent");
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// The whole screen as text, one row a line.
fn screen_text(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Raw attach asked for from inside a chat on another host's agent says
/// why in words that fit the chat: the notice was home's ("enter opens
/// its chat"), and in the chat Enter sends the draft.
#[tokio::test(flavor = "multi_thread")]
async fn raw_attach_from_a_far_agents_chat_does_not_say_enter_opens_its_chat() {
    let net = Net::start(
        topology().agent(
            AgentDecl::new("far", "desk")
                .kind(FakeKind::ClaudePty)
                .steps(vec![text("Ready."), Step::TurnEnd])
                .prompt("go"),
        ),
    )
    .await
    .unwrap();
    net.settle("far").await.unwrap();
    let far = net.agent("far").unwrap().id;
    let client: std::sync::Arc<dyn client::Client> =
        std::sync::Arc::new(client::InProcess::new(net.client("laptop").unwrap()));
    let config = tui::TuiConfig {
        working_dir: "/".into(),
        leader: 'a',
        theme: theme(),
        initial_chat: None,
        attach: true,
        version: String::new(),
        local_host: net.host("laptop").unwrap().host_id.as_bytes().to_vec(),
        layout: None,
        reports: None,
        chat_in: tui::setup::ChatIn::Amux,
        defaults: tui::setup::Defaults::default(),
    };
    let mut app = app_listing(client, config, &[far.as_bytes()]).await;
    open_chat(&mut app, far.as_bytes()).await;
    app.key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('a'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    press(&mut app, crossterm::event::KeyCode::Char('a'));
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let screen = screen_text(terminal.backend().buffer());
    assert!(
        screen.contains("another machine"),
        "the chat says the terminal is elsewhere:\n{screen}"
    );
    assert!(
        !screen.contains("enter opens its chat"),
        "inside far's chat the notice says Enter opens the chat:\n{screen}"
    );
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// desk's client with `name`'s chat open, `name` a terminal-less agent on
/// laptop, the two linked.
async fn far_chat(name: &str) -> (Net, tui::App) {
    far_chat_via(name, |client| client).await
}

/// [`far_chat`], its client reaching the daemon through `via`.
async fn far_chat_via(
    name: &str,
    via: impl FnOnce(std::sync::Arc<dyn client::Client>) -> std::sync::Arc<dyn client::Client>,
) -> (Net, tui::App) {
    let net = Net::start(
        topology().agent(
            AgentDecl::new(name, "laptop")
                .kind(FakeKind::ClaudeSdk)
                .steps(vec![text("Ready."), Step::TurnEnd])
                .prompt("go"),
        ),
    )
    .await
    .unwrap();
    net.settle(name).await.unwrap();
    let id = net.agent(name).unwrap().id;
    let client: std::sync::Arc<dyn client::Client> =
        std::sync::Arc::new(client::InProcess::new(net.client("desk").unwrap()));
    let client = via(client);
    let config = tui::TuiConfig {
        working_dir: "/".into(),
        leader: 'a',
        theme: theme(),
        initial_chat: None,
        attach: false,
        version: String::new(),
        local_host: net.host("desk").unwrap().host_id.as_bytes().to_vec(),
        layout: None,
        reports: None,
        chat_in: tui::setup::ChatIn::Amux,
        defaults: tui::setup::Defaults::default(),
    };
    let mut app = app_listing(client, config, &[id.as_bytes()]).await;
    open_chat(&mut app, id.as_bytes()).await;
    (net, app)
}

/// Every screen `app` draws for `patience`, housekeeping between.
async fn screens_for(app: &mut tui::App, patience: Duration) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    let mut screens = Vec::new();
    let until = tokio::time::Instant::now() + patience;
    while tokio::time::Instant::now() < until {
        app.housekeep();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        screens.push(screen_text(terminal.backend().buffer()));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    screens
}

/// Unlinking the agent's host is not the agent ceasing to exist: a chat
/// open on laptop's agent when desk and laptop were unlinked closed with
/// "the chat closed: no agent with that id", while laptop still ran it
/// with an approval open.
#[tokio::test(flavor = "multi_thread")]
async fn unlinking_the_agents_host_never_says_there_is_no_such_agent() {
    let (mut net, mut app) = far_chat("cx3").await;
    net.remove_link("desk", "laptop").await.unwrap();
    let screens = screens_for(&mut app, Duration::from_secs(5)).await;
    if let Some(screen) = screens
        .iter()
        .find(|screen| screen.contains("no agent with that id"))
    {
        panic!("unlinking laptop said cx3 does not exist:\n{screen}");
    }
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// A prompt held while its host can't be reached goes back to the draft if
/// the client closes first, even when it closes abruptly (its pane killed,
/// no quit to save from): the draft this machine keeps takes the held
/// prompt as soon as it is held. Only a clean quit returned it before, and
/// an older stored draft came back in its place.
#[tokio::test(flavor = "multi_thread")]
async fn a_held_prompt_is_in_the_kept_draft_before_the_client_quits() {
    use crossterm::event::KeyCode;
    let (mut net, mut app) = far_chat("far").await;
    let id = net.agent("far").unwrap().id;
    let daemon: std::sync::Arc<dyn client::Client> =
        std::sync::Arc::new(client::InProcess::new(net.client("desk").unwrap()));
    daemon
        .save_draft(wire::SaveDraftRequest {
            agent_id: id.as_bytes().to_vec(),
            draft: Some(wire::ComposerDraft {
                text: "an older draft".into(),
                attachments: Vec::new(),
            }),
        })
        .await
        .unwrap();
    net.refuse_calls("desk", "laptop", true).await.unwrap();
    for c in "race A".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    let kept = tokio::time::timeout(PATIENCE, async {
        loop {
            app.housekeep();
            let held = app.chat.as_ref().unwrap().session.state().held().count();
            let draft = daemon
                .get_draft(wire::GetDraftRequest {
                    agent_id: id.as_bytes().to_vec(),
                })
                .await
                .unwrap();
            if held == 1 && draft.text == "race A" {
                return draft;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the held prompt is kept as the draft while the client runs");
    assert_eq!(kept.text, "race A");
    // Closed abruptly: no quit, no drop.
    std::mem::forget(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// A send that no channel to its host could carry is held, and the host
/// reads offline (the chat words it "away"): the call closed the link it
/// could not open a stream on. When a link comes up again the host reads
/// online, and that alone releases the held prompt, which goes by itself.
/// Before, the link stayed listed, the host never stopped reading online,
/// and the prompt waited for good.
#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_held_when_no_call_reaches_its_host_goes_once_one_does() {
    use crossterm::event::KeyCode;
    let (mut net, mut app) = far_chat("far").await;
    net.refuse_calls("desk", "laptop", true).await.unwrap();
    for c in "race A".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    let away = tokio::time::timeout(PATIENCE, async {
        loop {
            app.housekeep();
            while let Ok(event) = app.receiver.try_recv() {
                app.event(event);
            }
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let screen = screen_text(terminal.backend().buffer());
            let held = app.chat.as_ref().unwrap().session.state().held().count();
            if held == 1 && screen.contains("laptop away") {
                return screen;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the prompt is held and the laptop reads away");
    println!("{away}");

    net.refuse_calls("desk", "laptop", false).await.unwrap();
    tokio::time::timeout(PATIENCE, async {
        loop {
            app.housekeep();
            while let Ok(event) = app.receiver.try_recv() {
                app.event(event);
            }
            if app.chat.as_ref().unwrap().session.state().held().count() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the held prompt goes once calls reach the laptop");
    let screens = screens_for(&mut app, Duration::from_secs(2)).await;
    let last = screens.last().cloned().unwrap_or_default();
    assert!(
        last.contains("race A") && !last.contains("sends when"),
        "the prompt shows as sent:\n{last}"
    );
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// A held prompt whose save this machine's daemon refused (it was briefly
/// away) is saved again: it counted as kept once the save was sent, so a
/// client killed after that lost the prompt.
#[tokio::test(flavor = "multi_thread")]
async fn a_held_prompt_whose_save_was_refused_is_saved_again() {
    use crossterm::event::KeyCode;
    let (mut net, mut app) = far_chat_via("far", |inner| {
        std::sync::Arc::new(SlowClearing {
            inner,
            hold: false.into(),
            saved: Default::default(),
            refuse_once: Some("race A".to_owned()).into(),
        })
    })
    .await;
    let id = net.agent("far").unwrap().id;
    let daemon: std::sync::Arc<dyn client::Client> =
        std::sync::Arc::new(client::InProcess::new(net.client("desk").unwrap()));
    net.refuse_calls("desk", "laptop", true).await.unwrap();
    for c in "race A".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    tokio::time::timeout(PATIENCE, async {
        loop {
            app.housekeep();
            while let Ok(event) = app.receiver.try_recv() {
                app.event(event);
            }
            let draft = daemon
                .get_draft(wire::GetDraftRequest {
                    agent_id: id.as_bytes().to_vec(),
                })
                .await
                .unwrap();
            if draft.text == "race A" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the refused save of the held prompt is made again");
    std::mem::forget(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// A prompt held for a chat that is closed goes back to the draft when its
/// agent exits before the host takes it: the session holding it is dropped
/// with the exited agent, or its resend is refused as exited. Either way the
/// draft this machine keeps was written again without it, and the prompt
/// was lost.
#[tokio::test(flavor = "multi_thread")]
async fn a_held_prompt_whose_agent_exits_while_its_chat_is_closed_stays_in_the_draft() {
    use crossterm::event::KeyCode;
    let (mut net, mut app) = far_chat("far").await;
    let id = net.agent("far").unwrap().id;
    let key = app.chat.as_ref().unwrap().agent.clone();
    let daemon: std::sync::Arc<dyn client::Client> =
        std::sync::Arc::new(client::InProcess::new(net.client("desk").unwrap()));
    net.refuse_calls("desk", "laptop", true).await.unwrap();
    for c in "race A".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    tokio::time::timeout(PATIENCE, async {
        loop {
            app.housekeep();
            while let Ok(event) = app.receiver.try_recv() {
                app.event(event);
            }
            if app.chat.as_ref().unwrap().session.state().held().count() == 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the prompt is held");
    app.close_chat();

    net.stop("far").await.unwrap();
    net.refuse_calls("desk", "laptop", false).await.unwrap();
    tokio::time::timeout(PATIENCE, async {
        loop {
            app.housekeep();
            while let Ok(event) = app.receiver.try_recv() {
                app.event(event);
            }
            // Not just no longer held: a resent prompt is in flight until
            // its refusal comes back, and only then is it on its way to the
            // draft.
            let ended = app.fleet.session(&key).is_none_or(|session| {
                session.state().inputs().iter().all(|sent| {
                    !matches!(
                        sent.state,
                        ui_state::InputState::Held | ui_state::InputState::Sent
                    )
                })
            });
            let exited = app
                .fleet
                .state()
                .agent(&key)
                .is_some_and(|entry| entry.lifecycle() == wire::Lifecycle::Exited);
            if exited && ended {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the agent exits and nothing is held or in flight any more");
    for _ in 0..10 {
        app.housekeep();
        while let Ok(event) = app.receiver.try_recv() {
            app.event(event);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    app.keep_drafts().await;
    let kept = daemon
        .get_draft(wire::GetDraftRequest {
            agent_id: id.as_bytes().to_vec(),
        })
        .await
        .unwrap();
    assert_eq!(
        kept.text, "race A",
        "the held prompt went back to the draft"
    );
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// A chat open on an agent deleted from its own host says it was deleted
/// (or closes): it said "exited · aborted" and "Enter resumes", and Enter
/// answered "could not resume: no agent <uuid>".
#[tokio::test(flavor = "multi_thread")]
async fn a_chat_on_an_agent_deleted_on_its_far_host_never_offers_to_resume_it() {
    let (mut net, mut app) = far_chat("i1").await;
    net.delete("i1").await.unwrap();
    let screens = screens_for(&mut app, Duration::from_secs(5)).await;
    let last = screens.last().cloned().unwrap_or_default();
    assert!(
        app.chat.is_none() || last.contains("deleted"),
        "five seconds after i1 was deleted its chat neither closed nor said so:\n{last}"
    );
    if let Some(screen) = screens
        .iter()
        .find(|screen| screen.contains("Enter resumes"))
    {
        panic!("the deleted i1's chat offered to resume it:\n{screen}");
    }
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}
