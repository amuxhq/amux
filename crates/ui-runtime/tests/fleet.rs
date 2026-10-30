//! The fleet driver against an in-memory runtime.

use std::collections::HashMap;
use std::sync::Arc;

use client::ManualClock;
use prost::Message as _;
use support::*;
use ui_runtime::{DriverEvent, Fleet, HELD_ROWS, TraceEvent, Window};
use ui_state::{AgentKey, Connection};
use wire::{Kind, ListItemsResponse, SessionEvent};

use crate::support;

fn named(id: &[u8]) -> wire::Agent {
    wire::Agent {
        agent_id: id.to_vec(),
        ..agent(Kind::Codex)
    }
}

#[tokio::test]
async fn the_fleet_opens_at_caught_up_and_relists_after_a_reconnect() {
    let clock = ManualClock::new(0);
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Fleet::connect(client, clock.clone()));
    let feed = calls.inventory().await;
    feed.send(inventory_agent(named(b"a")));
    feed.send(inventory_agent(named(b"b")));
    feed.send(inventory_caught_up());
    let fleet = open.await.unwrap().expect("the fleet opens");
    // Each live agent's session subscribes; the runtime has not answered
    // yet, which holds nothing up.
    let mut streams = Streams::default();
    streams.answer(&mut calls).await;
    streams.answer(&mut calls).await;
    assert!(fleet.state().caught_up());
    assert_eq!(fleet.state().agents().count(), 2);
    let key = |id: &[u8]| AgentKey {
        host: b"host-a".to_vec(),
        agent: id.to_vec(),
    };
    assert_eq!(fleet.take_changed(), [key(b"a"), key(b"b")]);

    // The daemon restarts: the fleet waits its backoff, re-lists, and drops
    // the agent the new stream no longer names.
    drop(feed);
    clock.armed(250).await;
    assert_eq!(fleet.state().connection(), Connection::Reconnecting);
    assert!(!fleet.state().caught_up());
    calls.none().await;
    clock.advance(250);
    let feed = calls.inventory().await;
    feed.send(inventory_agent(named(b"a")));
    feed.send(inventory_caught_up());
    until(fleet.changed(), || fleet.state().caught_up().then_some(())).await;
    let ids: Vec<Vec<u8>> = fleet
        .state()
        .agents()
        .map(|agent| agent.agent_id.clone())
        .collect();
    assert_eq!(ids, [b"a".to_vec()]);
    assert!(fleet.take_changed().contains(&key(b"b")));
    assert!(fleet.session(&key(b"b")).is_none());
    streams.until_closed(b"b").await;
    let state = fleet.state();
    assert_eq!(state.trace().replay(), *state);
    drop(state);
    let names: Vec<String> = fleet
        .dump_part()
        .files
        .into_iter()
        .map(|file| file.name)
        .collect();
    assert_eq!(names, ["client/fleet/state.txt", "client/fleet/trace.txt"]);
}

#[tokio::test]
async fn inventory_lag_resubscribes_at_once() {
    let clock = ManualClock::new(0);
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Fleet::connect(client, clock.clone()));
    let feed = calls.inventory().await;
    feed.send(inventory_agent(named(b"a")));
    feed.send(inventory_agent(named(b"b")));
    feed.send(inventory_caught_up());
    let fleet = open.await.unwrap().expect("the fleet opens");
    let mut streams = Streams::default();
    streams.answer(&mut calls).await;
    streams.answer(&mut calls).await;
    fleet.take_changed();

    // The runtime fell behind: it says so and closes. The fleet subscribes
    // again with no backoff and never reads as reconnecting; it is not
    // caught up until the fresh listing is.
    feed.send(inventory_lagged());
    drop(feed);
    let feed = calls.inventory().await;
    assert!(clock.sleeping().is_empty(), "no backoff after Lagged");
    assert!(!fleet.state().caught_up());
    assert_eq!(fleet.state().connection(), Connection::Live);
    feed.send(inventory_agent(named(b"a")));
    feed.send(inventory_caught_up());
    until(fleet.changed(), || fleet.state().caught_up().then_some(())).await;
    let ids: Vec<Vec<u8>> = fleet
        .state()
        .agents()
        .map(|agent| agent.agent_id.clone())
        .collect();
    assert_eq!(
        ids,
        [b"a".to_vec()],
        "the fresh listing reconciles the rows"
    );
    assert!(fleet.session(&key(b"b")).is_none());
    streams.until_closed(b"b").await;
    let state = fleet.state();
    let trace = state.trace();
    let driver: Vec<&DriverEvent> = trace
        .events
        .iter()
        .filter_map(|traced| match &traced.event {
            TraceEvent::Driver(event) => Some(event),
            TraceEvent::Msg(_) => None,
        })
        .collect();
    assert!(driver.contains(&&DriverEvent::Retail));
    assert!(
        !driver.iter().any(|event| matches!(
            event,
            DriverEvent::StreamEnded { .. } | DriverEvent::Backoff { .. }
        )),
        "a lag is not an outage: {driver:?}"
    );
    assert!(
        !trace
            .msgs()
            .any(|msg| *msg == ui_state::FleetMsg::Connection(Connection::Reconnecting)),
        "the fleet never read as reconnecting"
    );
    assert_eq!(trace.replay(), *state);
}

#[tokio::test]
async fn the_fleet_dump_part_writes_no_names_paths_or_status_text() {
    const NAME: &str = "PLANTEDname0001";
    const CWD: &str = "PLANTEDcwd0002";
    const WORKING_ON: &str = "PLANTEDworkingon0003";
    const EXIT: &str = "PLANTEDexit0004";
    const HOST: &str = "PLANTEDhost0005";
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Fleet::connect(client, ManualClock::new(0)));
    let feed = calls.inventory().await;
    feed.send(wire::InventoryEvent {
        of: Some(wire::inventory_event::Of::Host(wire::HostEntry {
            host: Some(wire::Host {
                host_id: b"host-a".to_vec(),
                name: HOST.into(),
                ..Default::default()
            }),
            ..Default::default()
        })),
    });
    feed.send(inventory_agent(wire::Agent {
        name: NAME.into(),
        cwd: CWD.into(),
        working_on: Some(WORKING_ON.into()),
        exit_cause: wire::ExitCause::ProviderExited as i32,
        exit_detail: Some(EXIT.into()),
        ..named(b"a")
    }));
    feed.send(inventory_caught_up());
    let fleet = open.await.unwrap().expect("the fleet opens");
    let part = fleet.dump_part();
    for secret in [NAME, CWD, WORKING_ON, EXIT, HOST] {
        assert_eq!(part_holds(&part, secret), None, "{secret} is in the dump");
    }
    let trace = String::from_utf8(part.files[1].contents.clone()).unwrap();
    assert!(trace.contains("Agent 686f73742d61/61 Codex"), "{trace}");
    assert!(trace.contains("CaughtUp rev=0"), "{trace}");
}

fn key(id: &[u8]) -> AgentKey {
    AgentKey {
        host: b"host-a".to_vec(),
        agent: id.to_vec(),
    }
}

/// Every session stream the driver opened, by agent, so a test can count
/// the ones still open.
#[derive(Default)]
struct Streams {
    feeds: HashMap<Vec<u8>, Vec<Feed<SessionEvent>>>,
}

impl Streams {
    /// Answers the next call, a Subscribe, and checks the agent had no
    /// other stream open. Returns the agent and the tail it asked for.
    async fn answer(&mut self, calls: &mut Calls) -> (Vec<u8>, u32) {
        let (request, feed) = calls.subscribe_session().await;
        let tail = request.tail;
        let agent = request.agent_id;
        assert_eq!(
            self.open(&agent),
            0,
            "{agent:?} was subscribed to twice at once"
        );
        self.feeds.entry(agent.clone()).or_default().push(feed);
        (agent, tail)
    }

    fn open(&self, agent: &[u8]) -> usize {
        self.feeds.get(agent).map_or(0, |feeds| {
            feeds.iter().filter(|feed| !feed.is_closed()).count()
        })
    }

    /// The agent's one open stream.
    fn feed(&self, agent: &[u8]) -> &Feed<SessionEvent> {
        assert_eq!(self.open(agent), 1, "{agent:?} has one stream open");
        self.feeds[agent]
            .iter()
            .find(|feed| !feed.is_closed())
            .unwrap()
    }

    async fn until_closed(&self, agent: &[u8]) {
        tokio::time::timeout(PATIENCE, async {
            while self.open(agent) > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{agent:?}'s stream stayed open"));
    }
}

/// Serves an opening: a snapshot and the newest `tail` of `rows` rows.
fn serve(feed: &Feed<SessionEvent>, rows: u64, tail: u32) {
    feed.send(snapshot(Kind::Codex, 1, &[]));
    let first = rows.saturating_sub(u64::from(tail)) + 1;
    for order in first..=rows {
        feed.send(ev(text_item(Kind::Codex, order, order, "row")));
    }
    feed.send(caught_up(1));
}

async fn fleet_of(agents: Vec<wire::Agent>) -> (Fleet, Calls, Feed<wire::InventoryEvent>) {
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Fleet::connect(client, ManualClock::new(0)));
    let feed = calls.inventory().await;
    for agent in agents {
        feed.send(inventory_agent(agent));
    }
    feed.send(inventory_caught_up());
    let fleet = open.await.unwrap().expect("the fleet opens");
    (fleet, calls, feed)
}

async fn session_of(fleet: &Fleet, agent: &AgentKey) -> Arc<ui_runtime::Session> {
    tokio::time::timeout(PATIENCE, async {
        loop {
            if let Some(session) = fleet.session(agent)
                && session.state().caught_up()
            {
                return session;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the agent's session opened")
}

const CHAT: Window = Window { tail: 40, cap: 200 };

#[tokio::test]
async fn home_and_the_chat_share_one_session_per_live_agent_and_never_subscribe_twice() {
    let (fleet, mut calls, _inventory) = fleet_of(vec![named(b"a"), exited(named(b"b"))]).await;
    let mut streams = Streams::default();

    // Only the live agent is subscribed to, with the fleet's few rows.
    let (agent, tail) = streams.answer(&mut calls).await;
    assert_eq!((agent.as_slice(), tail), (&b"a"[..], HELD_ROWS));
    serve(streams.feed(b"a"), 30, tail);
    let held = session_of(&fleet, &key(b"a")).await;
    assert_eq!(held.state().transcript().len(), HELD_ROWS as usize);
    assert!(
        fleet.session(&key(b"b")).is_none(),
        "an exited agent has none"
    );
    calls.none().await;

    // A chat on it widens the same session: older rows page in from the
    // local runtime, and nothing subscribes again.
    let opening = tokio::spawn({
        let fleet = Arc::new(fleet);
        let opened = fleet.clone();
        async move { (opened.open(&key(b"a"), CHAT).await, fleet) }
    });
    let (request, reply) = calls.list_items().await;
    assert_eq!(request.before_order, Some(15));
    assert_eq!(request.page_size, CHAT.tail - HELD_ROWS);
    let items = (1..=14)
        .rev()
        .map(|order| text_item(Kind::Codex, order, order, "older"))
        .collect();
    reply
        .send(Ok(ListItemsResponse {
            items,
            exhausted: true,
        }))
        .ok();
    let (opened, fleet) = opening.await.unwrap();
    let chat = opened.expect("the chat opens");
    assert!(Arc::ptr_eq(&chat, &held), "the chat reads home's session");
    assert_eq!(chat.state().transcript().len(), 30);
    assert_eq!(chat.state().oldest_order(), Some(1));
    calls.none().await;

    // Closing the chat narrows the session back to the fleet's rows.
    fleet.close(&key(b"a"));
    assert_eq!(held.state().transcript().len(), HELD_ROWS as usize);
    assert_eq!(held.state().oldest_order(), Some(15));
    assert_eq!(streams.open(b"a"), 1);

    // The exited agent's session exists only while a chat shows it.
    let opening = tokio::spawn({
        let fleet = fleet.clone();
        async move { fleet.open(&key(b"b"), CHAT).await.map(drop) }
    });
    let (agent, tail) = streams.answer(&mut calls).await;
    assert_eq!((agent.as_slice(), tail), (&b"b"[..], CHAT.tail));
    serve(streams.feed(b"b"), 3, tail);
    opening
        .await
        .unwrap()
        .expect("the exited agent's chat opens");
    assert!(fleet.session(&key(b"b")).is_some());
    fleet.close(&key(b"b"));
    assert!(fleet.session(&key(b"b")).is_none());
    streams.until_closed(b"b").await;

    // Leaving the foreground closes every stream; returning reopens each
    // once, with a tail.
    fleet.set_foreground(false);
    streams.until_closed(b"a").await;
    assert!(held.suspended());
    calls.none().await;

    // Out of the foreground a chat that opens (a notification bringing it
    // current) streams until it closes, and only it.
    let opening = tokio::spawn({
        let fleet = fleet.clone();
        let window = Window {
            tail: HELD_ROWS,
            cap: HELD_ROWS,
        };
        async move { fleet.open(&key(b"a"), window).await.map(drop) }
    });
    let (agent, tail) = streams.answer(&mut calls).await;
    assert_eq!((agent.as_slice(), tail), (&b"a"[..], HELD_ROWS));
    serve(streams.feed(b"a"), 31, tail);
    opening.await.unwrap().expect("the chat opens");
    assert!(!held.suspended());
    calls.none().await;
    fleet.close(&key(b"a"));
    streams.until_closed(b"a").await;
    assert!(held.suspended());

    fleet.set_foreground(true);
    let (agent, tail) = streams.answer(&mut calls).await;
    assert_eq!((agent.as_slice(), tail), (&b"a"[..], HELD_ROWS));
    serve(streams.feed(b"a"), 32, tail);
    until(held.changed(), || held.state().caught_up().then_some(())).await;
    assert!(!held.suspended());
    assert_eq!(held.state().transcript().head(), Some(32));
    assert!(Arc::ptr_eq(&fleet.session(&key(b"a")).unwrap(), &held));
    calls.none().await;
}

#[tokio::test]
async fn home_wakes_for_what_is_outside_the_rows_and_not_for_rows_that_leave_its_line() {
    let (fleet, mut calls, _inventory) = fleet_of(vec![named(b"a")]).await;
    let mut streams = Streams::default();
    let (_, tail) = streams.answer(&mut calls).await;
    serve(streams.feed(b"a"), 3, tail);
    let held = session_of(&fleet, &key(b"a")).await;
    fleet.take_changed();

    // A streamed row that leaves home's line as it was moves the session
    // and leaves home asleep.
    streams
        .feed(b"a")
        .send(ev(text_item(Kind::Codex, 4, 4, "streaming")));
    until(held.changed(), || {
        (held.state().transcript().head() == Some(4)).then_some(())
    })
    .await;
    assert!(fleet.take_changed().is_empty(), "a row does not wake home");

    // The turn ending arrives as a snapshot, which does.
    let mut idle = snapshot(Kind::Codex, 2, &[]);
    if let Some(wire::session_event::Of::Snapshot(snapshot)) = &mut idle.of {
        snapshot.phase = wire::Phase::Idle as i32;
    }
    streams.feed(b"a").send(idle);
    let woke = until(fleet.changed(), || {
        let changed = fleet.take_changed();
        (!changed.is_empty()).then_some(changed)
    })
    .await;
    assert_eq!(woke, [key(b"a")]);
}

/// A Codex command row at `order`, running or finished.
fn command(order: u64, revision: u64, command: &str, state: wire::ToolState) -> SessionEvent {
    let body = wire::CodexItem {
        kind: Some(wire::codex_item::Kind::Work(wire::Work {
            of: Some(wire::work::Of::Command(wire::CommandWork {
                command: command.into(),
                ..Default::default()
            })),
            state: state as i32,
            ..Default::default()
        })),
    };
    ev(wire::Item {
        body: body.encode_to_vec(),
        ..text_item(Kind::Codex, order, revision, "")
    })
}

/// Waits for a wake of home after which home draws `step` for the agent.
async fn home_woken_with(fleet: &Fleet, held: &ui_runtime::Session, step: &str) {
    until(fleet.changed(), || {
        let drawn = ui_view::session_line(&held.state(), 0)
            .step
            .and_then(|line| line.step);
        (drawn.as_deref() == Some(step) && fleet.take_changed() == [key(b"a")]).then_some(())
    })
    .await;
}

#[tokio::test]
async fn home_wakes_when_a_working_agent_moves_to_its_next_step() {
    let (fleet, mut calls, _inventory) = fleet_of(vec![named(b"a")]).await;
    let mut streams = Streams::default();
    let (_, tail) = streams.answer(&mut calls).await;
    serve(streams.feed(b"a"), 3, tail);
    let held = session_of(&fleet, &key(b"a")).await;
    fleet.take_changed();

    // No snapshot moves: only the rows say the agent is on a new step.
    let feed = streams.feed(b"a");
    feed.send(command(4, 4, "cargo build", wire::ToolState::Running));
    home_woken_with(&fleet, &held, "cargo build").await;
    feed.send(command(4, 5, "cargo build", wire::ToolState::Succeeded));
    feed.send(command(5, 6, "cargo test", wire::ToolState::Running));
    home_woken_with(&fleet, &held, "cargo test").await;
}

#[tokio::test]
async fn an_agent_that_exits_loses_its_session_unless_a_chat_shows_it() {
    let (fleet, mut calls, inventory) = fleet_of(vec![named(b"a"), named(b"c")]).await;
    let mut streams = Streams::default();
    for _ in 0..2 {
        let (agent, tail) = streams.answer(&mut calls).await;
        serve(streams.feed(&agent), 3, tail);
    }
    session_of(&fleet, &key(b"a")).await;
    let shown = session_of(&fleet, &key(b"c")).await;
    let fleet = Arc::new(fleet);
    let chat = fleet.open(&key(b"c"), CHAT).await.expect("the chat opens");
    assert!(Arc::ptr_eq(&chat, &shown));

    inventory.send(inventory_agent(exited(named(b"a"))));
    inventory.send(inventory_agent(exited(named(b"c"))));
    streams.until_closed(b"a").await;
    assert!(fleet.session(&key(b"a")).is_none());
    // The chat keeps its session, and the fleet feeds it the exit.
    until(chat.changed(), || {
        (chat.state().agent().lifecycle() == wire::Lifecycle::Exited).then_some(())
    })
    .await;
    assert_eq!(streams.open(b"c"), 1);
    drop((chat, shown));
    fleet.close(&key(b"c"));
    streams.until_closed(b"c").await;
    assert!(fleet.session(&key(b"c")).is_none());
}

/// A Codex snapshot naming the catalogue `hash`.
fn offering(revision: u64, hash: &[u8]) -> SessionEvent {
    let mut event = snapshot(Kind::Codex, revision, &[]);
    if let Some(wire::session_event::Of::Snapshot(snapshot)) = &mut event.of {
        snapshot.catalogue = Some(hash.to_vec());
    }
    event
}

/// The catalogue blob offering `model`, as GetBlob answers it.
fn catalogue(model: &str) -> wire::GetBlobResponse {
    use prost::Message as _;
    let catalogue = wire::Catalogue {
        models: vec![wire::OfferedModel {
            value: model.into(),
            display_name: model.into(),
            ..wire::OfferedModel::default()
        }],
        ..wire::Catalogue::default()
    };
    wire::GetBlobResponse {
        bytes: catalogue.encode_to_vec(),
    }
}

fn models(session: &ui_runtime::Session) -> Vec<String> {
    let state = session.state();
    state
        .agent_state()
        .models
        .iter()
        .map(|model| model.value.clone())
        .collect()
}

#[tokio::test]
async fn a_chat_on_screen_fetches_its_catalogue_once_per_hash_and_home_never_does() {
    let (fleet, mut calls, _inventory) = fleet_of(vec![named(b"a"), named(b"b")]).await;
    let fleet = Arc::new(fleet);
    let mut streams = Streams::default();
    for _ in 0..2 {
        let (agent, _) = streams.answer(&mut calls).await;
        let feed = streams.feed(&agent);
        feed.send(offering(1, b"first"));
        feed.send(caught_up(1));
    }
    session_of(&fleet, &key(b"a")).await;
    session_of(&fleet, &key(b"b")).await;
    calls.none().await;

    // Opening a chat fetches the blob its snapshot names.
    let opening = tokio::spawn({
        let fleet = fleet.clone();
        async move { fleet.open(&key(b"a"), CHAT).await }
    });
    let (request, reply) = calls.get_blob().await;
    assert_eq!(request.agent_id, b"a");
    assert_eq!(request.hash, b"first");
    reply.send(Ok(catalogue("gpt-1"))).ok();
    let chat = opening.await.unwrap().expect("the chat opens");
    until(chat.changed(), || {
        (models(&chat) == ["gpt-1"]).then_some(())
    })
    .await;
    calls.none().await;

    // Another agent offering the same is not asked again.
    let other = fleet.open(&key(b"b"), CHAT).await.expect("b's chat opens");
    assert_eq!(models(&other), ["gpt-1"]);
    calls.none().await;

    // The catalogue changes mid-session: the new hash is fetched once.
    streams.feed(b"a").send(offering(2, b"second"));
    let (request, reply) = calls.get_blob().await;
    assert_eq!(request.hash, b"second");
    reply.send(Ok(catalogue("gpt-2"))).ok();
    until(chat.changed(), || {
        (models(&chat) == ["gpt-2"]).then_some(())
    })
    .await;
    calls.none().await;

    // Off screen a change fetches nothing; reopening finds it cached.
    fleet.close(&key(b"b"));
    streams.feed(b"b").send(offering(2, b"second"));
    until(other.changed(), || {
        (other.state().agent_state().catalogue.as_deref() == Some(&b"second"[..])).then_some(())
    })
    .await;
    calls.none().await;
    let reopened = fleet.open(&key(b"b"), CHAT).await.expect("b reopens");
    assert_eq!(models(&reopened), ["gpt-2"]);
    calls.none().await;
}

fn inventory_host(reach: wire::trusted::Reach) -> wire::InventoryEvent {
    wire::InventoryEvent {
        of: Some(wire::inventory_event::Of::Host(
            wire::HostEntry::trusted_host(b"host-a".to_vec(), "desk", reach),
        )),
    }
}

/// A stop whose host cannot be reached never left: the fleet holds it,
/// the agent's row says so, and it goes once the host is seen back. One
/// cancelled before then never goes.
#[tokio::test]
async fn a_stop_its_host_could_not_take_is_held_and_sent_when_the_host_is_back() {
    let clock = ManualClock::new(0);
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Fleet::connect(client, clock.clone()));
    let feed = calls.inventory().await;
    feed.send(inventory_host(wire::trusted::Reach::Online(
        wire::RouteKind::Direct as i32,
    )));
    feed.send(inventory_agent(named(b"a")));
    feed.send(inventory_caught_up());
    let fleet = Arc::new(open.await.unwrap().expect("the fleet opens"));
    let mut streams = Streams::default();
    streams.answer(&mut calls).await;
    let key = AgentKey {
        host: b"host-a".to_vec(),
        agent: b"a".to_vec(),
    };
    let stopping = tokio::spawn({
        let fleet = fleet.clone();
        let key = key.clone();
        async move { fleet.stop(&key).await }
    });
    let (request, reply) = calls.stop().await;
    assert_eq!(request.agent_id, b"a");
    reply
        .send(Err(client::RpcError::Refused(wire::Error {
            code: wire::ErrorCode::Unavailable as i32,
            message: "desk cannot be reached".into(),
            details: Vec::new(),
        })))
        .ok();
    assert_eq!(stopping.await.unwrap().unwrap(), ui_runtime::Stop::Held);
    assert!(fleet.state().stop_held(&key), "the row shows the stop held");

    // The host goes and comes back: the stop goes again.
    feed.send(inventory_host(wire::trusted::Reach::Away(wire::Empty {})));
    feed.send(inventory_host(wire::trusted::Reach::Online(
        wire::RouteKind::Direct as i32,
    )));
    let (request, reply) = calls.stop().await;
    assert_eq!(request.agent_id, b"a");
    reply.send(Ok(())).ok();
    until(fleet.changed(), || {
        (!fleet.state().stop_held(&key)).then_some(())
    })
    .await;

    // Cancelled while held, it never goes.
    let stopping = tokio::spawn({
        let fleet = fleet.clone();
        let key = key.clone();
        async move { fleet.stop(&key).await }
    });
    let (_, reply) = calls.stop().await;
    reply
        .send(Err(client::RpcError::Refused(wire::Error {
            code: wire::ErrorCode::Unavailable as i32,
            message: "desk cannot be reached".into(),
            details: Vec::new(),
        })))
        .ok();
    assert_eq!(stopping.await.unwrap().unwrap(), ui_runtime::Stop::Held);
    fleet.cancel_stop(&key);
    feed.send(inventory_host(wire::trusted::Reach::Away(wire::Empty {})));
    feed.send(inventory_host(wire::trusted::Reach::Online(
        wire::RouteKind::Direct as i32,
    )));
    calls.none().await;
    assert!(!fleet.state().stop_held(&key));
}
