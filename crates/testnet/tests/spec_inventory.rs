//! The inventory between hosts: the one host set, reconciliation and
//! generations, across real daemons on loopback links with real agents on
//! the fakes.
//!
//! Every trusted host is in every inventory snapshot whatever its
//! presence, and discovery adds candidates only from this host's scope. A
//! replica whose origin no longer lists the agent is dropped and its chats
//! close; an origin whose generation changed has every replica dropped and
//! rebuilt from a fresh tail, and nothing of another host's is touched.

#![cfg(unix)]

mod support;

use std::time::Duration;

use provider_fakes::script::Step;
use testnet::observe::{self, Mark, inventory_agents, inventory_hosts, marks};
use testnet::{
    AgentDecl, HostDecl, JournalCut, Net, PATIENCE, Stuck, Topology, holds_for, withdraw,
};
use uuid::Uuid;
use wire::{HostEntry, InventoryEvent, SessionEvent, inventory_event, session_event};

fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

/// One turn saying `label`.
fn says(label: &str) -> Vec<Step> {
    vec![text(label), Step::TurnEnd]
}

fn desk_and_laptop() -> Topology {
    Topology::new()
        .host("desk")
        .host("laptop")
        .link("desk", "laptop")
}

fn host_named<'a>(net: &Net, hosts: &'a [HostEntry], name: &str) -> Option<&'a HostEntry> {
    let id = net.host(name).unwrap().host_id;
    hosts.iter().find(|host| host.host_id() == id.as_bytes())
}

/// How a host is listed: trusted and online or , or a candidate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Listed {
    Online,
    Offline,
    Candidate,
}

fn listed(host: &HostEntry) -> Option<Listed> {
    if host.candidate().is_some() {
        Some(Listed::Candidate)
    } else if host.online().is_some() {
        Some(Listed::Online)
    } else {
        host.offline().map(|_| Listed::Offline)
    }
}

/// Whether the inventory seen so far lists `name` so.
fn lists_host(net: &Net, events: &[InventoryEvent], name: &str, how: Listed) -> bool {
    host_named(net, &inventory_hosts(events), name).is_some_and(|host| listed(host) == Some(how))
}

fn lists_agent(net: &Net, events: &[InventoryEvent], agent: &str) -> bool {
    let id = net.agent(agent).unwrap().id;
    inventory_agents(events)
        .iter()
        .any(|row| row.agent_id == id.as_bytes())
}

/// Whether the stream removed the agent with this id.
fn removed(events: &[InventoryEvent], id: Uuid, reason: wire::RemovalReason) -> bool {
    events.iter().any(|event| {
        matches!(&event.of, Some(inventory_event::Of::AgentRemoved(gone))
            if gone.agent_id == id.as_bytes() && gone.reason == reason as i32)
    })
}

/// A trusted host that cannot be reached stays in the host set, and its
/// agents stay listed: a reconnect snapshot never forgets them. Only its
/// presence says it is away.
#[tokio::test(flavor = "multi_thread")]
async fn a_trusted_host_stays_in_every_snapshot_while_unreachable() {
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(says("hello"))
            .prompt("go"),
    );
    let mut net = Net::start(topology).await.unwrap();
    net.current("laptop", "worker").await.unwrap();
    let mut fleet = net.observe_inventory("laptop").await.unwrap();
    fleet
        .observe_until(
            |events| {
                observe::inventory_caught_up(events)
                    && lists_host(&net, events, "desk", Listed::Online)
                    && lists_host(&net, events, "laptop", Listed::Online)
                    && lists_agent(&net, events, "worker")
            },
            PATIENCE,
        )
        .await
        .unwrap();

    net.sever_link("desk", "laptop").unwrap();
    fleet
        .observe_until(
            |events| lists_host(&net, events, "desk", Listed::Offline),
            PATIENCE,
        )
        .await
        .unwrap();
    // A call that has to reach it says which host could not be reached, by
    // the name a person knows it by.
    let refused = wire::agent_service_server::AgentService::stop_agent(
        &net.client("laptop").unwrap(),
        tonic::Request::new(wire::StopAgentRequest {
            agent_id: net.agent("worker").unwrap().id.as_bytes().to_vec(),
        }),
    )
    .await
    .expect_err("the desk cannot be reached");
    assert_eq!(refused.message(), "desk cannot be reached");
    let mut reopened = net.observe_inventory("laptop").await.unwrap();
    let opening = reopened
        .observe_until(observe::inventory_caught_up, PATIENCE)
        .await
        .unwrap();
    assert!(
        lists_host(&net, opening, "desk", Listed::Offline) && lists_agent(&net, opening, "worker"),
        "an unreachable trusted host and its agents are in the snapshot:\n{}",
        reopened.transcript()
    );
    assert!(
        !opening
            .iter()
            .any(|event| matches!(event.of, Some(inventory_event::Of::HostRemoved(_)))),
        "{}",
        reopened.transcript()
    );

    net.restore_link("desk", "laptop").await.unwrap();
    fleet
        .observe_until(
            |events| {
                let hosts = inventory_hosts(events);
                host_named(&net, &hosts, "desk").is_some_and(|desk| desk.online().is_some())
                    && lists_agent(&net, events, "worker")
            },
            PATIENCE,
        )
        .await
        .unwrap();
    net.shutdown().await.unwrap();
}

/// Discovery lists the machines it finds in this host's scope, matched
/// exactly, as candidates; a trusted host found by discovery stays
/// trusted, a host in another scope never appears, and a candidate that
/// goes away leaves the set.
#[tokio::test(flavor = "multi_thread")]
async fn discovery_candidates_appear_only_from_the_same_scope() {
    let found = |name: &str| HostDecl {
        name: name.to_owned(),
        lan: true,
        discovery: true,
        ..HostDecl::default()
    };
    let topology = Topology::new()
        .scope("run-a")
        .host_decl(found("phone"))
        .host_decl(found("desk"))
        .host_decl(found("near"))
        .host_decl(HostDecl {
            scope: Some("run-a-other".to_owned()),
            ..found("stranger")
        })
        .host_decl(HostDecl {
            scope: Some(String::new()),
            ..found("everyday")
        })
        .link("phone", "desk");
    let mut net = Net::start(topology).await.unwrap();
    let mut fleet = net.observe_inventory("phone").await.unwrap();
    fleet
        .observe_until(
            |events| {
                lists_host(&net, events, "near", Listed::Candidate)
                    && lists_host(&net, events, "desk", Listed::Online)
            },
            PATIENCE,
        )
        .await
        .unwrap();
    let outsiders = [
        net.host("stranger").unwrap().host_id,
        net.host("everyday").unwrap().host_id,
    ];
    // A window: nothing the test controls gates an announcement that
    // discovery must not deliver across scopes.
    holds_for("no host from another scope", Duration::from_secs(1), || {
        let hosts = inventory_hosts(fleet.events());
        let seen = outsiders
            .iter()
            .any(|id| hosts.iter().any(|host| host.host_id() == id.as_bytes()));
        async move { !seen }
    })
    .await
    .unwrap();
    let desk = host_named(&net, &inventory_hosts(fleet.events()), "desk")
        .unwrap()
        .clone();
    assert!(desk.trusted().is_some(), "found and trusted is trusted");

    let near = net.host("near").unwrap().host_id;
    net.stop_daemon("near").await.unwrap();
    fleet
        .observe_until(
            |events| {
                events.iter().any(|event| {
                    matches!(&event.of, Some(inventory_event::Of::HostRemoved(gone))
                        if gone.host_id == near.as_bytes())
                })
            },
            PATIENCE,
        )
        .await
        .unwrap();
    net.shutdown().await.unwrap();
}

/// An agent its host stops listing is dropped from the replica and every
/// chat on it closes, whether the host says so live or the replica learns
/// it at the next inventory catch-up after a break.
#[tokio::test(flavor = "multi_thread")]
async fn a_replica_its_origin_no_longer_lists_is_dropped_and_its_chats_close() {
    let topology = desk_and_laptop()
        .agent(AgentDecl::new("live", "desk").steps(says("a")).prompt("go"))
        .agent(
            AgentDecl::new("quiet", "desk")
                .steps(says("b"))
                .prompt("go"),
        )
        .agent(AgentDecl::new("kept", "desk").steps(says("c")).prompt("go"));
    let mut net = Net::start(topology).await.unwrap();
    for agent in ["live", "quiet", "kept"] {
        net.current("laptop", agent).await.unwrap();
    }
    let mut fleet = net.observe_inventory("laptop").await.unwrap();
    fleet
        .observe_until(observe::inventory_caught_up, PATIENCE)
        .await
        .unwrap();
    let mut live_chat = net.observe("laptop", "live", 10).await.unwrap();
    let mut quiet_chat = net.observe("laptop", "quiet", 10).await.unwrap();
    let mut kept_chat = net.observe("laptop", "kept", 10).await.unwrap();
    for chat in [&mut live_chat, &mut quiet_chat, &mut kept_chat] {
        chat.observe_until(observe::caught_up, PATIENCE)
            .await
            .unwrap();
    }
    let live = net.agent("live").unwrap().clone();
    let quiet = net.agent("quiet").unwrap().clone();

    // Said live: the delta on the host's inventory stream.
    net.delete("live").await.unwrap();
    live_chat.until_closed(PATIENCE).await.unwrap();
    fleet
        .observe_until(
            |events| removed(events, live.id, wire::RemovalReason::NoLongerListed),
            PATIENCE,
        )
        .await
        .unwrap();

    // Learned after a break: the reconciliation at the inventory's CaughtUp.
    net.sever_link("desk", "laptop").unwrap();
    net.wait_link("desk", "laptop", false).await.unwrap();
    net.delete("quiet").await.unwrap();
    // A window: nothing the test controls gates a drop that must not
    // happen while the host is away.
    holds_for(
        "the replica kept while its host is away",
        Duration::from_millis(300),
        || {
            let runtime = net.runtime("laptop").unwrap();
            let key = quiet.key();
            async move {
                runtime
                    .read_store(move |store| store.agent(&key).unwrap().is_some())
                    .await
                    .unwrap()
            }
        },
    )
    .await
    .unwrap();
    net.restore_link("desk", "laptop").await.unwrap();
    quiet_chat.until_closed(PATIENCE).await.unwrap();
    fleet
        .observe_until(
            |events| removed(events, quiet.id, wire::RemovalReason::NoLongerListed),
            PATIENCE,
        )
        .await
        .unwrap();
    let dir = net.host("laptop").unwrap().data_dir.clone();
    let replica_dirs: Vec<_> = walk(&dir)
        .into_iter()
        .filter(|path| path.to_string_lossy().contains(&quiet.id.to_string()))
        .collect();
    assert!(
        replica_dirs.is_empty(),
        "the dropped replica's files go: {replica_dirs:?}"
    );

    // The agent it still lists is untouched: no Reset, still current.
    net.current("laptop", "kept").await.unwrap();
    assert!(
        !marks(kept_chat.events()).contains(&Mark::Reset),
        "{}",
        kept_chat.transcript()
    );
    assert!(!kept_chat.is_closed());
    net.shutdown().await.unwrap();
}

/// Every path under `dir`.
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        }
        out.push(path);
    }
    out
}

/// A generation change resets that host's replicas, each on its own
/// stream, and records the new generation, and touches nothing of another
/// host's: the other host's chat sees no Reset and its rows stay. An agent
/// the rewound host no longer has is dropped and its chat closes.
#[tokio::test(flavor = "multi_thread")]
async fn a_generation_change_resets_one_hosts_replicas_and_nothing_else() {
    let topology = Topology::new()
        .host("desk")
        .host("lab")
        .host("laptop")
        .link("desk", "laptop")
        .link("lab", "laptop")
        .agent(
            AgentDecl::new("worker", "desk")
                .steps(says("w"))
                .prompt("go"),
        )
        .agent(AgentDecl::new("other", "lab").steps(says("o")).prompt("go"));
    let mut net = Net::start(topology).await.unwrap();
    // Each agent's first turn comes to rest at its host before its replica
    // is taken as current, so the rows read below are all the turn writes:
    // a reply replicated after them would look like a change.
    net.settle("worker").await.unwrap();
    net.settle("other").await.unwrap();
    net.current("laptop", "worker").await.unwrap();
    net.current("laptop", "other").await.unwrap();
    net.checkpoint_host("desk").await.unwrap();
    net.spawn(AgentDecl::new("late", "desk").steps(says("l")).prompt("go"))
        .await
        .unwrap();
    net.current("laptop", "late").await.unwrap();

    let mut fleet = net.observe_inventory("laptop").await.unwrap();
    fleet
        .observe_until(observe::inventory_caught_up, PATIENCE)
        .await
        .unwrap();
    let mut worker_chat = net.observe("laptop", "worker", 10).await.unwrap();
    let mut other_chat = net.observe("laptop", "other", 10).await.unwrap();
    let mut late_chat = net.observe("laptop", "late", 10).await.unwrap();
    for chat in [&mut worker_chat, &mut other_chat, &mut late_chat] {
        chat.observe_until(observe::caught_up, PATIENCE)
            .await
            .unwrap();
    }
    let other_rows = {
        let runtime = net.runtime("laptop").unwrap();
        let key = net.agent("other").unwrap().key();
        runtime
            .read_store(move |store| store.cut(&key, u32::MAX).unwrap().held)
            .await
            .unwrap()
    };
    let generation = net.generation("desk").unwrap();
    let late = net.agent("late").unwrap().clone();

    net.rewind_host("desk", &[]).await.unwrap();
    assert_eq!(net.generation("desk").unwrap(), generation + 1);
    worker_chat
        .observe_until(
            |events| {
                let after = marks(events);
                after
                    .iter()
                    .position(|mark| *mark == Mark::Reset)
                    .is_some_and(|reset| {
                        after[reset..]
                            .iter()
                            .any(|mark| matches!(mark, Mark::CaughtUp(_)))
                    })
            },
            PATIENCE,
        )
        .await
        .unwrap();
    late_chat.until_closed(PATIENCE).await.unwrap();
    fleet
        .observe_until(
            |events| {
                removed(events, late.id, wire::RemovalReason::NoLongerListed)
                    && host_named(&net, &inventory_hosts(events), "desk")
                        .is_some_and(|desk| desk.generation == generation + 1)
            },
            PATIENCE,
        )
        .await
        .unwrap();
    let desk = net.host("desk").unwrap().host_id;
    assert_eq!(
        net.runtime("laptop")
            .unwrap()
            .read_store(move |store| store.host_generation(desk.as_bytes()).unwrap())
            .await
            .unwrap(),
        Some(generation + 1)
    );

    let key = net.agent("other").unwrap().key();
    assert_eq!(
        net.runtime("laptop")
            .unwrap()
            .read_store(move |store| store.cut(&key, u32::MAX).unwrap().held)
            .await
            .unwrap(),
        other_rows,
        "another host's replica is untouched"
    );
    let other = marks(other_chat.events());
    assert!(
        !other
            .iter()
            .any(|mark| matches!(mark, Mark::Reset | Mark::Detached)),
        "{}",
        other_chat.transcript()
    );
    assert!(!other_chat.is_closed());
    net.shutdown().await.unwrap();
}

/// Untrusting a host removes it from the host set and drops its replicas;
/// their chats close.
#[tokio::test(flavor = "multi_thread")]
async fn an_untrusted_host_leaves_the_set_with_its_replicas() {
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(says("hello"))
            .prompt("go"),
    );
    let mut net = Net::start(topology).await.unwrap();
    net.current("laptop", "worker").await.unwrap();
    let mut fleet = net.observe_inventory("laptop").await.unwrap();
    fleet
        .observe_until(observe::inventory_caught_up, PATIENCE)
        .await
        .unwrap();
    let mut chat = net.observe("laptop", "worker", 10).await.unwrap();
    chat.observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();
    let desk = net.host("desk").unwrap().host_id;
    let worker = net.agent("worker").unwrap().clone();

    net.untrust("laptop", "desk").await.unwrap();
    chat.until_closed(PATIENCE).await.unwrap();
    fleet
        .observe_until(
            |events| {
                removed(events, worker.id, wire::RemovalReason::HostUntrusted)
                    && events.iter().any(|event| {
                        matches!(&event.of, Some(inventory_event::Of::HostRemoved(gone))
                            if gone.host_id == desk.as_bytes())
                    })
            },
            PATIENCE,
        )
        .await
        .unwrap();
    let key = worker.key();
    assert!(
        net.runtime("laptop")
            .unwrap()
            .read_store(move |store| store.agent(&key).unwrap().is_none())
            .await
            .unwrap()
    );
    net.shutdown().await.unwrap();
}

/// This host's own entry says whether its profile is signed in to its
/// account: unset while it was never bound, and flipping with signing out
/// and back in without any link changing, so a client can name the cause
/// when a host only the relay reaches goes away.
#[tokio::test(flavor = "multi_thread")]
async fn the_own_entry_carries_this_profiles_sign_in() {
    let net = Net::start(Topology::new().relay(&["ada"]).host("laptop"))
        .await
        .unwrap();
    let own = |events: &[InventoryEvent]| {
        host_named(&net, &inventory_hosts(events), "laptop").map(|entry| entry.account_signed_in())
    };
    let mut fleet = net.observe_inventory("laptop").await.unwrap();
    let opening = fleet
        .observe_until(observe::inventory_caught_up, PATIENCE)
        .await
        .unwrap();
    assert_eq!(own(opening), Some(None), "{}", fleet.transcript());

    net.sign_in("laptop", "ada").await.unwrap();
    fleet
        .observe_until(|events| own(events) == Some(Some(true)), PATIENCE)
        .await
        .unwrap();

    net.admin("laptop")
        .await
        .unwrap()
        .sign_out(wire::SignOutRequest {})
        .await
        .unwrap();
    fleet
        .observe_until(|events| own(events) == Some(Some(false)), PATIENCE)
        .await
        .unwrap();
    let mut reopened = net.observe_inventory("laptop").await.unwrap();
    let opening = reopened
        .observe_until(observe::inventory_caught_up, PATIENCE)
        .await
        .unwrap();
    assert_eq!(own(opening), Some(Some(false)), "{}", reopened.transcript());

    net.sign_in("laptop", "ada").await.unwrap();
    fleet
        .observe_until(|events| own(events) == Some(Some(true)), PATIENCE)
        .await
        .unwrap();
    net.shutdown().await.unwrap();
}

fn queued(event: &SessionEvent, id: &[u8]) -> bool {
    matches!(&event.of, Some(session_event::Of::Snapshot(snapshot))
        if snapshot.queue.iter().any(|entry| entry.input_id == id))
}

/// The origin-rewind journey. An origin checkpoints while a prompt waits
/// in its queue, the prompt is withdrawn and a paired laptop's chat sees
/// that, and then the origin loses power: its store goes back to the
/// checkpoint, where the prompt is still queued, and it reboots under a
/// new generation. The laptop's chat sees Detached, then Reset, the fresh
/// tail and CaughtUp, and no Snapshot after the Reset ever shows the
/// withdrawn prompt as queued.
#[tokio::test(flavor = "multi_thread")]
async fn origin_rewind() {
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(vec![
                text("ready"),
                Step::TurnEnd,
                text("working on it"),
                Step::WaitFor {
                    path: "hold".into(),
                },
                Step::TurnEnd,
            ])
            .prompt("go"),
    );
    let mut net = Net::start(topology).await.unwrap();
    let kind = net.agent("worker").unwrap().kind;
    net.current("laptop", "worker").await.unwrap();
    let mut chat = net.observe("laptop", "worker", 20).await.unwrap();
    chat.observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();

    net.send("worker", "next").await.unwrap();
    chat.observe_until(
        |events| {
            events.iter().any(|event| {
                matches!(&event.of, Some(session_event::Of::Item(item))
                    if item.text.contains("working on it"))
            })
        },
        PATIENCE,
    )
    .await
    .unwrap();
    let later = Uuid::new_v4();
    let accepted = net
        .input("worker", testnet::prompt(kind, later.as_bytes(), "later"))
        .await
        .unwrap();
    assert!(
        matches!(
            accepted.of,
            Some(wire::send_input_response::Of::Accepted(wire::Accepted {
                queued: true
            }))
        ),
        "{accepted:?}"
    );
    chat.observe_until(
        |events| events.iter().any(|event| queued(event, later.as_bytes())),
        PATIENCE,
    )
    .await
    .unwrap();
    // What reaches the drive: the store with the prompt still queued.
    net.checkpoint_host("desk").await.unwrap();

    net.input(
        "worker",
        withdraw(kind, Uuid::new_v4().as_bytes(), later.as_bytes()),
    )
    .await
    .unwrap();
    chat.observe_until(
        |events| {
            events
                .last()
                .is_some_and(|event| matches!(event.of, Some(session_event::Of::Snapshot(_))))
                && !queued(events.last().unwrap(), later.as_bytes())
        },
        PATIENCE,
    )
    .await
    .unwrap();
    let before = chat.events().len();
    let generation = net.generation("desk").unwrap();

    let journal = net.journal_end("worker").unwrap();
    net.rewind_host(
        "desk",
        &[JournalCut {
            agent: "worker".to_owned(),
            byte: journal,
        }],
    )
    .await
    .unwrap();
    assert_eq!(net.generation("desk").unwrap(), generation + 1);
    let events = chat
        .observe_until(
            |events| {
                let after = marks(&events[before..]);
                after
                    .iter()
                    .position(|mark| *mark == Mark::Reset)
                    .is_some_and(|reset| {
                        after[reset..]
                            .iter()
                            .any(|mark| matches!(mark, Mark::CaughtUp(_)))
                    })
            },
            PATIENCE,
        )
        .await
        .unwrap()
        .to_vec();
    let after = &events[before..];
    let reset = after
        .iter()
        .position(|event| matches!(event.of, Some(session_event::Of::Reset(_))))
        .unwrap();
    let rebuilt = &after[reset..];
    let shown_queued = rebuilt
        .iter()
        .filter(|event| queued(event, later.as_bytes()))
        .count();
    let saw_detached = after[..reset]
        .iter()
        .any(|event| matches!(event.of, Some(session_event::Of::Detached(_))));

    let mut transcript = String::new();
    let line = |out: &mut String, text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    line(
        &mut transcript,
        format!(
            "origin-rewind: desk generation {generation} -> {}; laptop chat on worker, tail 20",
            generation + 1
        ),
    );
    line(&mut transcript, "before the power cut:".to_owned());
    for event in &events[..before] {
        line(&mut transcript, format!("  {}", describe(event)));
    }
    line(&mut transcript, "after the power cut:".to_owned());
    for event in after {
        line(&mut transcript, format!("  {}", describe(event)));
    }
    line(
        &mut transcript,
        format!(
            "check: Detached before the Reset: {saw_detached}; snapshots after the Reset showing the withdrawn prompt queued: {shown_queued}"
        ),
    );
    println!("{transcript}");

    assert!(saw_detached, "{transcript}");
    assert_eq!(shown_queued, 0, "{transcript}");
    assert!(
        rebuilt
            .iter()
            .any(|event| matches!(event.of, Some(session_event::Of::Snapshot(_)))),
        "the rebuilt block leads with a Snapshot: {transcript}"
    );
    assert!(
        rebuilt.iter().any(|event| matches!(&event.of,
            Some(session_event::Of::Item(item)) if item.text.contains("working on it"))),
        "the fresh tail carries the rows the journal kept: {transcript}"
    );
    net.shutdown().await.unwrap();
}

fn describe(event: &SessionEvent) -> String {
    use testnet::observe::Describe as _;
    event.describe()
}

/// A peer that untrusts this host says so as it closes the link, and this
/// host's inventory keeps that on the peer's entry, so a client can say the
/// peer no longer trusts this machine rather than guess at why it is away.
#[tokio::test(flavor = "multi_thread")]
async fn a_peer_that_revokes_trust_is_listed_as_having_revoked_it() {
    let mut net = Net::start(desk_and_laptop()).await.unwrap();
    let mut fleet = net.observe_inventory("laptop").await.unwrap();
    fleet
        .observe_until(
            |events| lists_host(&net, events, "desk", Listed::Online),
            PATIENCE,
        )
        .await
        .unwrap();
    let desk_id = net.host("desk").unwrap().host_id;
    let desk = move |events: &[InventoryEvent]| {
        inventory_hosts(events)
            .into_iter()
            .find(|host| host.host_id() == desk_id.as_bytes())
    };
    assert!(!desk(fleet.events()).unwrap().revoked());

    net.untrust("desk", "laptop").await.unwrap();
    fleet
        .observe_until(
            |events| {
                desk(events)
                    .is_some_and(|entry| entry.revoked() && listed(&entry) == Some(Listed::Offline))
            },
            PATIENCE,
        )
        .await
        .unwrap();
    net.shutdown().await.unwrap();
}

/// Waits until `pred` holds over the inventory, moving the policy clock on
/// a second at a time so a stream that ended is opened again past its
/// backoff.
async fn reconnecting_until(
    net: &Net,
    fleet: &mut testnet::observe::InventoryObserver,
    pred: impl Fn(&[InventoryEvent]) -> bool,
) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while fleet
        .observe_until(&pred, Duration::from_millis(250))
        .await
        .is_err()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the inventory never came to what was waited for: {}",
            fleet.transcript()
        );
        net.advance(Duration::from_secs(1)).unwrap();
    }
}

/// A host on `account` that reaches others through the relay alone.
fn on_account(name: &str, account: &str) -> HostDecl {
    HostDecl {
        name: name.to_owned(),
        account: Some(account.to_owned()),
        ..HostDecl::default()
    }
}

/// Over the relay no link carries the peer's word, so a host learns it no
/// longer is trusted from the peer's refusal of the next stream it opens,
/// its own reconnect of what the revocation closed. A relay route that
/// merely goes away is no such word, and trusting the host again takes the
/// mark away.
#[tokio::test(flavor = "multi_thread")]
async fn a_peer_that_revokes_trust_over_the_relay_is_listed_as_having_revoked_it() {
    let mut net = Net::start(
        Topology::new()
            .relay(&["ada"])
            .host_decl(on_account("desk", "ada"))
            .host_decl(on_account("tablet", "ada")),
    )
    .await
    .unwrap();
    net.trust("desk", "tablet").await.unwrap();
    net.trust("tablet", "desk").await.unwrap();
    let desk_id = net.host("desk").unwrap().host_id;
    let desk = move |events: &[InventoryEvent]| {
        inventory_hosts(events)
            .into_iter()
            .find(|host| host.host_id() == desk_id.as_bytes())
    };
    let ever_revoked = move |events: &[InventoryEvent]| {
        events.iter().any(|event| {
            matches!(&event.of, Some(inventory_event::Of::Host(host))
                if host.host_id() == desk_id.as_bytes() && host.revoked())
        })
    };
    let online = |events: &[InventoryEvent]| {
        desk(events).is_some_and(|entry| entry.online().is_some() && !entry.revoked())
    };
    let mut fleet = net.observe_inventory("tablet").await.unwrap();
    fleet.observe_until(online, PATIENCE).await.unwrap();

    // The desk going away takes its relay route with it; that is not a
    // revocation, nor is its coming back.
    net.stop_daemon("desk").await.unwrap();
    fleet
        .observe_until(
            |events| desk(events).is_some_and(|entry| entry.online().is_none()),
            PATIENCE,
        )
        .await
        .unwrap();
    net.restart_daemon("desk").await.unwrap();
    reconnecting_until(&net, &mut fleet, online).await;
    // Each second's retry runs; a window after each, since no outcome
    // marks the revocation that must not come.
    for _ in 0..5 {
        net.advance(Duration::from_secs(1)).unwrap();
        let revoked = fleet
            .observe_until(ever_revoked, Duration::from_millis(100))
            .await;
        assert!(
            matches!(revoked, Err(Stuck::Deadline { .. })),
            "a relay route that goes away is not a revocation"
        );
    }

    net.untrust("desk", "tablet").await.unwrap();
    reconnecting_until(&net, &mut fleet, |events| {
        desk(events).is_some_and(|entry| entry.revoked() && listed(&entry) == Some(Listed::Offline))
    })
    .await;
    // The relay still says the desk is online, which is what pairing again
    // rides on.
    assert_eq!(
        net.edge("tablet").unwrap().via(desk_id).await,
        node::harness::LiveRoute::Relay
    );

    net.trust("desk", "tablet").await.unwrap();
    reconnecting_until(&net, &mut fleet, online).await;
    net.shutdown().await.unwrap();
}

/// Pairing again with a host that refused this one as no longer trusted
/// takes the mark off at once: the host is listed as trusting this one,
/// and its streams reopen then rather than after the backoff their
/// refusals grew. The driven clock never moves after the pairing, so
/// nothing here waits out a backoff.
#[tokio::test(flavor = "multi_thread")]
async fn pairing_again_with_a_peer_that_revoked_trust_clears_the_mark_at_once() {
    let mut net = Net::start(
        Topology::new()
            .relay(&["ada"])
            .host_decl(on_account("desk", "ada"))
            .host_decl(on_account("tablet", "ada")),
    )
    .await
    .unwrap();
    net.trust("desk", "tablet").await.unwrap();
    net.trust("tablet", "desk").await.unwrap();
    let desk_id = net.host("desk").unwrap().host_id;
    let desk = move |events: &[InventoryEvent]| {
        inventory_hosts(events)
            .into_iter()
            .find(|host| host.host_id() == desk_id.as_bytes())
    };
    let online = move |events: &[InventoryEvent]| {
        desk(events).is_some_and(|entry| entry.online().is_some() && !entry.revoked())
    };
    let mut fleet = net.observe_inventory("tablet").await.unwrap();
    fleet.observe_until(online, PATIENCE).await.unwrap();

    net.untrust("desk", "tablet").await.unwrap();
    reconnecting_until(&net, &mut fleet, |events| {
        desk(events).is_some_and(|entry| entry.revoked())
    })
    .await;
    // Refused, the tablet cannot see what the desk starts meanwhile.
    net.spawn(AgentDecl::new("late", "desk").steps(says("l")).prompt("go"))
        .await
        .unwrap();
    let late = net.agent("late").unwrap().id;

    // The desk shows a code and the tablet pairs with it over the relay;
    // the tablet's pairing commits the key it already held.
    let started = support::start_pairing(&net, "desk", support::qr_window())
        .await
        .unwrap();
    let Some(wire::pairing_window::Secret::QrSecret(secret)) = started.secret else {
        panic!("a QR window hands out a secret");
    };
    let pending = support::begin_pair(
        &net,
        "tablet",
        Some(desk_id),
        wire::begin_pair_request::Secret::QrSecret(secret),
        Vec::new(),
    )
    .await
    .expect("pairing through the relay");
    assert_eq!(pending.via, wire::RouteKind::Relay as i32);
    support::confirm_pair(&net, "tablet", pending.token)
        .await
        .unwrap();
    fleet
        .observe_until(
            |events| {
                online(events)
                    && inventory_agents(events)
                        .iter()
                        .any(|agent| agent.agent_id == late.as_bytes())
            },
            PATIENCE,
        )
        .await
        .unwrap();
    net.shutdown().await.unwrap();
}

/// A trusted host's entry says whether this machine's copy of its agents
/// has caught up with it on a live stream: true once the inventory stream
/// reaches CaughtUp, false again when the host is lost, true again after
/// the stream reopens and catches up. A client that reads every trusted
/// host current has reconciled with its fleet, which is what the phone's
/// cold-start reconciliation measures.
#[tokio::test(flavor = "multi_thread")]
async fn a_hosts_entry_says_whether_its_inventory_is_current() {
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("tablet")
            .link("desk", "tablet"),
    )
    .await
    .unwrap();
    net.trust("desk", "tablet").await.unwrap();
    net.trust("tablet", "desk").await.unwrap();
    let desk_id = net.host("desk").unwrap().host_id;
    let current = move |events: &[InventoryEvent], wanted: bool| {
        inventory_hosts(events)
            .into_iter()
            .find(|host| host.host_id() == desk_id.as_bytes())
            .is_some_and(|entry| entry.current() == wanted)
    };
    let mut fleet = net.observe_inventory("tablet").await.unwrap();
    fleet
        .observe_until(|events| current(events, true), PATIENCE)
        .await
        .unwrap();

    net.sever_link("desk", "tablet").unwrap();
    fleet
        .observe_until(|events| current(events, false), PATIENCE)
        .await
        .unwrap();

    net.restore_link("desk", "tablet").await.unwrap();
    reconnecting_until(&net, &mut fleet, |events| current(events, true)).await;
    net.shutdown().await.unwrap();
}
