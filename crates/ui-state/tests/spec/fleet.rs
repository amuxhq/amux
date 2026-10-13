//! The fleet: hosts, agent rows and families from the inventory stream.

use ui_state::{AgentKey, Attention, Connection, FleetMsg, FleetState};
use wire::{Agent, AgentRef, HostEntry, InventoryEvent, Lifecycle, Phase, inventory_event};

fn row(host: &str, id: &str, phase: Phase, parent: Option<(&str, &str)>) -> Agent {
    Agent {
        agent_id: id.as_bytes().to_vec(),
        host_id: host.as_bytes().to_vec(),
        kind: wire::Kind::ClaudeSdk as i32,
        name: id.into(),
        lifecycle: Lifecycle::Live as i32,
        phase: phase as i32,
        parent: parent.map(|(host, id)| AgentRef {
            host_id: host.as_bytes().to_vec(),
            agent_id: id.as_bytes().to_vec(),
        }),
        incarnation: 1,
        ..Agent::default()
    }
}

fn at(host: &str, id: &str) -> AgentKey {
    AgentKey {
        host: host.as_bytes().to_vec(),
        agent: id.as_bytes().to_vec(),
    }
}

fn ev(of: inventory_event::Of) -> FleetMsg {
    FleetMsg::Event(Box::new(InventoryEvent { of: Some(of) }))
}

fn host(id: &str) -> FleetMsg {
    ev(inventory_event::Of::Host(HostEntry {
        host_id: id.as_bytes().to_vec(),
        name: id.into(),
        ..HostEntry::default()
    }))
}

fn agent(agent: Agent) -> FleetMsg {
    ev(inventory_event::Of::Agent(agent))
}

fn caught_up() -> FleetMsg {
    ev(inventory_event::Of::CaughtUp(wire::CaughtUp {
        revision: 0,
    }))
}

fn family() -> FleetState {
    let mut fleet = FleetState::new();
    for msg in [
        host("a"),
        host("b"),
        agent(row("a", "parent", Phase::Idle, None)),
        agent(row("b", "child", Phase::Working, Some(("a", "parent")))),
        agent(row("a", "grandchild", Phase::Idle, Some(("b", "child")))),
        agent(row("a", "loner", Phase::Working, None)),
        caught_up(),
    ] {
        fleet.update(msg);
    }
    fleet
}

#[test]
fn families_follow_parent_edges_across_hosts() {
    let fleet = family();
    assert!(fleet.caught_up());
    let roots: Vec<&str> = fleet.roots().map(|agent| agent.name.as_str()).collect();
    assert_eq!(roots, ["loner", "parent"]);
    let members: Vec<&str> = fleet
        .family(&at("a", "parent"))
        .iter()
        .map(|agent| agent.name.as_str())
        .collect();
    assert_eq!(members, ["parent", "child", "grandchild"]);
    assert_eq!(fleet.root(&at("a", "grandchild")), at("a", "parent"));
    assert_eq!(fleet.parent(&at("b", "child")).unwrap().name, "parent");
}

#[test]
fn a_family_is_as_loud_as_its_loudest_descendant() {
    let mut fleet = family();
    assert_eq!(
        fleet.family_attention(&at("a", "parent")),
        Some(Attention::Working)
    );
    let changed = fleet.update(agent(row(
        "a",
        "grandchild",
        Phase::NeedsYou,
        Some(("b", "child")),
    )));
    for touched in [at("a", "grandchild"), at("b", "child"), at("a", "parent")] {
        assert!(
            changed.contains(&touched),
            "the grandchild and every ancestor may redraw"
        );
    }
    assert!(!changed.contains(&at("a", "loner")));
    assert_eq!(
        fleet.family_attention(&at("a", "parent")),
        Some(Attention::NeedsYou)
    );
    assert_eq!(
        fleet.family_attention(&at("b", "child")),
        Some(Attention::NeedsYou)
    );
    let mut exited = row("a", "grandchild", Phase::NeedsYou, Some(("b", "child")));
    exited.lifecycle = Lifecycle::Exited as i32;
    fleet.update(agent(exited));
    assert_eq!(
        fleet.family_attention(&at("a", "parent")),
        Some(Attention::Working)
    );
}

#[test]
fn a_removed_parent_leaves_its_children_heading_their_own_family() {
    let mut fleet = family();
    fleet.update(ev(inventory_event::Of::AgentRemoved(wire::AgentRemoved {
        host_id: b"a".to_vec(),
        agent_id: b"parent".to_vec(),
        reason: wire::RemovalReason::Deleted as i32,
    })));
    let roots: Vec<&str> = fleet.roots().map(|agent| agent.name.as_str()).collect();
    assert_eq!(roots, ["loner", "child"]);
}

#[test]
fn a_relist_after_reconnect_drops_what_was_not_listed_again() {
    let mut fleet = family();
    fleet.update(FleetMsg::Connection(Connection::Reconnecting));
    assert!(!fleet.caught_up());
    for msg in [
        host("a"),
        agent(row("a", "parent", Phase::Idle, None)),
        agent(row("a", "loner", Phase::Idle, None)),
    ] {
        fleet.update(msg);
    }
    assert_eq!(
        fleet.agents().count(),
        4,
        "nothing is dropped before CaughtUp"
    );
    fleet.update(caught_up());
    let names: Vec<&str> = fleet.agents().map(|agent| agent.name.as_str()).collect();
    assert_eq!(names, ["loner", "parent"]);
    assert!(fleet.host(b"b").is_none());
}

fn online(id: &str) -> FleetMsg {
    ev(inventory_event::Of::Host(HostEntry {
        host_id: id.as_bytes().to_vec(),
        name: id.into(),
        presence: wire::Presence::Online as i32,
        ..HostEntry::default()
    }))
}

/// A stop that could not reach a host this client still read as online,
/// as when its own daemon restarts, is held; the stream that reopens names
/// the host online again with no away in between, and the stop goes.
#[test]
fn a_stop_held_while_its_host_read_online_goes_when_the_host_is_listed_again() {
    let mut fleet = FleetState::new();
    for msg in [
        online("a"),
        agent(row("a", "worker", Phase::Working, None)),
        caught_up(),
    ] {
        fleet.update(msg);
    }
    let listing = fleet.listings();
    fleet.update(FleetMsg::HoldStop {
        agent: at("a", "worker"),
        listing,
    });
    assert!(fleet.stop_held(&at("a", "worker")));
    assert!(fleet.take_released_stops().is_empty());
    for msg in [
        FleetMsg::Connection(Connection::Reconnecting),
        FleetMsg::Connection(Connection::Live),
        online("a"),
        agent(row("a", "worker", Phase::Working, None)),
        caught_up(),
    ] {
        fleet.update(msg);
    }
    assert_eq!(fleet.take_released_stops(), [at("a", "worker")]);
    assert!(!fleet.stop_held(&at("a", "worker")));
}

/// The stream can reopen and list the host online before the failed stop
/// is held: that listing already said the host is back, and the stop goes
/// at once. One held mid-listing goes when the listing ends.
#[test]
fn a_stop_held_after_its_host_was_listed_again_goes_at_once() {
    let mut fleet = FleetState::new();
    for msg in [
        online("a"),
        agent(row("a", "worker", Phase::Working, None)),
        agent(row("a", "other", Phase::Working, None)),
        caught_up(),
    ] {
        fleet.update(msg);
    }
    let left = fleet.listings();
    for msg in [
        FleetMsg::Connection(Connection::Reconnecting),
        FleetMsg::Connection(Connection::Live),
        online("a"),
        agent(row("a", "worker", Phase::Working, None)),
        agent(row("a", "other", Phase::Working, None)),
        caught_up(),
    ] {
        fleet.update(msg);
    }
    fleet.update(FleetMsg::HoldStop {
        agent: at("a", "worker"),
        listing: left,
    });
    assert_eq!(fleet.take_released_stops(), [at("a", "worker")]);
    assert!(!fleet.stop_held(&at("a", "worker")));

    let left = fleet.listings();
    fleet.update(FleetMsg::Connection(Connection::Reconnecting));
    fleet.update(FleetMsg::Connection(Connection::Live));
    fleet.update(online("a"));
    fleet.update(FleetMsg::HoldStop {
        agent: at("a", "other"),
        listing: left,
    });
    assert!(fleet.stop_held(&at("a", "other")));
    fleet.update(agent(row("a", "other", Phase::Working, None)));
    fleet.update(caught_up());
    assert_eq!(fleet.take_released_stops(), [at("a", "other")]);
}

/// A stop that failed under the newest listing waits for the next word of
/// its host, so a host that cannot take it is not asked again and again.
#[test]
fn a_stop_held_under_the_newest_listing_waits_for_its_host() {
    let mut fleet = FleetState::new();
    for msg in [
        online("a"),
        agent(row("a", "worker", Phase::Working, None)),
        caught_up(),
    ] {
        fleet.update(msg);
    }
    let listing = fleet.listings();
    fleet.update(FleetMsg::HoldStop {
        agent: at("a", "worker"),
        listing,
    });
    assert!(fleet.take_released_stops().is_empty());
    assert!(fleet.stop_held(&at("a", "worker")));
}
