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
        host: Some(wire::Host {
            host_id: id.as_bytes().to_vec(),
            name: id.into(),
            ..Default::default()
        }),
        ..Default::default()
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
    ev(inventory_event::Of::Host(HostEntry::trusted_host(
        id.as_bytes().to_vec(),
        id,
        wire::trusted::Reach::Online(wire::RouteKind::Unspecified as i32),
    )))
}

/// A stop that could not reach a host this client still read as online,
/// as when its own daemon restarts, is held; the stream that reopens names
/// the host online again with no away in between, and the stop goes.
#[test]
fn a_close_held_while_its_host_read_online_goes_when_the_host_is_listed_again() {
    let mut fleet = FleetState::new();
    for msg in [
        online("a"),
        agent(row("a", "worker", Phase::Working, None)),
        caught_up(),
    ] {
        fleet.update(msg);
    }
    let listing = fleet.listings();
    fleet.update(FleetMsg::HoldClose {
        agent: at("a", "worker"),
        listing,
    });
    assert!(fleet.close_held(&at("a", "worker")));
    assert!(fleet.take_released_closes().is_empty());
    for msg in [
        FleetMsg::Connection(Connection::Reconnecting),
        FleetMsg::Connection(Connection::Live),
        online("a"),
        agent(row("a", "worker", Phase::Working, None)),
        caught_up(),
    ] {
        fleet.update(msg);
    }
    assert_eq!(fleet.take_released_closes(), [at("a", "worker")]);
    assert!(!fleet.close_held(&at("a", "worker")));
}

/// The stream can reopen and list the host online before the failed stop
/// is held: that listing already said the host is back, and the stop goes
/// at once. One held mid-listing goes when the listing ends.
#[test]
fn a_close_held_after_its_host_was_listed_again_goes_at_once() {
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
    fleet.update(FleetMsg::HoldClose {
        agent: at("a", "worker"),
        listing: left,
    });
    assert_eq!(fleet.take_released_closes(), [at("a", "worker")]);
    assert!(!fleet.close_held(&at("a", "worker")));

    let left = fleet.listings();
    fleet.update(FleetMsg::Connection(Connection::Reconnecting));
    fleet.update(FleetMsg::Connection(Connection::Live));
    fleet.update(online("a"));
    fleet.update(FleetMsg::HoldClose {
        agent: at("a", "other"),
        listing: left,
    });
    assert!(fleet.close_held(&at("a", "other")));
    fleet.update(agent(row("a", "other", Phase::Working, None)));
    fleet.update(caught_up());
    assert_eq!(fleet.take_released_closes(), [at("a", "other")]);
}

/// A stop that failed under the newest listing waits for the next word of
/// its host, so a host that cannot take it is not asked again and again.
#[test]
fn a_close_held_under_the_newest_listing_waits_for_its_host() {
    let mut fleet = FleetState::new();
    for msg in [
        online("a"),
        agent(row("a", "worker", Phase::Working, None)),
        caught_up(),
    ] {
        fleet.update(msg);
    }
    let listing = fleet.listings();
    fleet.update(FleetMsg::HoldClose {
        agent: at("a", "worker"),
        listing,
    });
    assert!(fleet.take_released_closes().is_empty());
    assert!(fleet.close_held(&at("a", "worker")));
}

/// A held stop the end of a listing sends again leaves its row changed, so
/// clients stop drawing it held with Cancel.
#[test]
fn a_stop_released_at_a_listings_end_changes_its_row() {
    let mut fleet = FleetState::new();
    for msg in [
        online("a"),
        agent(row("a", "worker", Phase::Working, None)),
        caught_up(),
    ] {
        fleet.update(msg);
    }
    let left = fleet.listings();
    fleet.update(FleetMsg::Connection(Connection::Reconnecting));
    fleet.update(FleetMsg::Connection(Connection::Live));
    fleet.update(online("a"));
    fleet.update(FleetMsg::HoldClose {
        agent: at("a", "worker"),
        listing: left,
    });
    fleet.update(agent(row("a", "worker", Phase::Working, None)));
    let changed = fleet.update(caught_up());
    assert_eq!(fleet.take_released_closes(), [at("a", "worker")]);
    assert!(
        changed.contains(&at("a", "worker")),
        "the released stop's row did not change: {changed:?}"
    );
}

/// A row in `workspace` on its host, which made it when `made`, with the
/// git facts its last turn end read.
fn in_workspace(
    mut agent: Agent,
    kind: wire::experimental::WorkspaceKind,
    workspace: &str,
    made: bool,
    git: Option<wire::Git>,
) -> Agent {
    let entry = wire::experimental::AgentWorkspace {
        kind: kind as i32,
        id: workspace.into(),
        made_by_amux: made,
        ..Default::default()
    };
    wire::experimental::WORKSPACE.put(&mut agent.experimental, &entry);
    agent.git = git;
    agent
}

fn totals(files: u32, added: u32, removed: u32) -> Option<wire::ChangeTotals> {
    Some(wire::ChangeTotals {
        files,
        added,
        removed,
    })
}

fn git(
    branch: &str,
    uncommitted: Option<wire::ChangeTotals>,
    on_branch: Option<wire::ChangeTotals>,
) -> Option<wire::Git> {
    Some(wire::Git {
        branch: Some(branch.into()),
        uncommitted,
        on_branch,
        ..Default::default()
    })
}

#[test]
fn a_delete_removes_the_workspace_only_with_its_last_member_counting_its_descendants_gone() {
    use wire::experimental::WorkspaceKind::{Folder, Git, Wt};
    let changed = git("fix-scroll", totals(3, 42, 7), totals(5, 60, 9));
    let mut fleet = FleetState::new();
    for msg in [
        host("a"),
        host("b"),
        // A parent and its child share a tree; another agent shares a
        // worktree with a stranger; one is alone in a folder of its own.
        agent(in_workspace(
            row("a", "parent", Phase::Idle, None),
            Wt,
            "/t/fix",
            true,
            changed.clone(),
        )),
        agent(in_workspace(
            row("a", "child", Phase::Idle, Some(("a", "parent"))),
            Wt,
            "/t/fix",
            true,
            None,
        )),
        agent(in_workspace(
            row("a", "one", Phase::Idle, None),
            Git,
            "/w/pair",
            true,
            None,
        )),
        agent(in_workspace(
            row("a", "two", Phase::Idle, None),
            Git,
            "/w/pair",
            true,
            None,
        )),
        agent(in_workspace(
            row("a", "notes", Phase::Idle, None),
            Folder,
            "/n",
            false,
            None,
        )),
        agent(in_workspace(
            row("a", "hand", Phase::Idle, None),
            Wt,
            "/t/mine",
            false,
            None,
        )),
        // The same path on another host is another workspace.
        agent(in_workspace(
            row("b", "away", Phase::Idle, None),
            Git,
            "/w/solo",
            true,
            None,
        )),
        agent(in_workspace(
            row("a", "solo", Phase::Idle, None),
            Git,
            "/w/solo",
            true,
            None,
        )),
        agent(row("a", "old", Phase::Idle, None)),
        caught_up(),
    ] {
        fleet.update(msg);
    }

    let goes = fleet
        .delete_removes_workspace(&at("a", "parent"))
        .expect("its child goes with it");
    assert_eq!(goes.uncommitted, totals(3, 42, 7));
    assert_eq!(goes.branch.as_deref(), Some("fix-scroll"));
    assert_eq!(
        fleet.delete_removes_workspace(&at("a", "child")),
        None,
        "its parent stays in the tree"
    );
    assert_eq!(
        fleet.delete_removes_workspace(&at("a", "one")),
        None,
        "a stranger shares it"
    );
    assert_eq!(
        fleet.delete_removes_workspace(&at("a", "notes")),
        None,
        "a folder is never removed"
    );
    assert_eq!(
        fleet.delete_removes_workspace(&at("a", "hand")),
        None,
        "a hand-made tree is kept"
    );
    assert_eq!(
        fleet.delete_removes_workspace(&at("a", "solo")),
        Some(ui_state::WorkspaceGoes {
            uncommitted: None,
            branch: None,
        }),
        "the other host's agent is in another workspace; no git facts read yet"
    );
    assert_eq!(
        fleet.delete_removes_workspace(&at("a", "old")),
        None,
        "no entry, no workspace"
    );
}

/// Equal totals on the branch and uncommitted do not say the branch has no
/// commits of its own: a line committed and then changed again uncommitted
/// gives equal totals over a unique commit. The row carries no count of
/// commits, so the branch is always named when the row has one.
#[test]
fn the_branch_is_named_whenever_the_row_has_one() {
    use wire::experimental::WorkspaceKind::Git;
    let mut fleet = FleetState::new();
    for msg in [
        host("a"),
        agent(in_workspace(
            row("a", "same", Phase::Idle, None),
            Git,
            "/w/same",
            true,
            git("same", totals(1, 1, 1), totals(1, 1, 1)),
        )),
        agent(in_workspace(
            row("a", "clean", Phase::Idle, None),
            Git,
            "/w/clean",
            true,
            git("clean", totals(0, 0, 0), totals(4, 30, 2)),
        )),
        agent(in_workspace(
            row("a", "detached", Phase::Idle, None),
            Git,
            "/w/detached",
            true,
            Some(wire::Git {
                branch: None,
                ..Default::default()
            }),
        )),
        caught_up(),
    ] {
        fleet.update(msg);
    }
    let same = fleet.delete_removes_workspace(&at("a", "same")).unwrap();
    assert_eq!(
        same.branch.as_deref(),
        Some("same"),
        "equal totals may hide a commit"
    );
    assert_eq!(same.uncommitted, totals(1, 1, 1));
    let clean = fleet.delete_removes_workspace(&at("a", "clean")).unwrap();
    assert_eq!(clean.uncommitted, None, "no uncommitted files");
    assert_eq!(clean.branch.as_deref(), Some("clean"));
    let detached = fleet
        .delete_removes_workspace(&at("a", "detached"))
        .unwrap();
    assert_eq!(
        detached.branch, None,
        "a detached head has no branch to name"
    );
}

/// An agent with a running process keeps its row's git facts current, so a
/// delete's warning reads them; one suspended or exited cannot, so the
/// warning starts by checking with the host, then says what the host found,
/// or that a worktree with changes stays when the host could not say.
#[test]
fn a_delete_warning_asks_the_host_when_no_process_keeps_the_row_current() {
    use ui_state::DeleteWarning;
    use wire::experimental::WorkspaceKind::Git;
    let stale = git("fix", None, None);
    let mut suspended = in_workspace(
        row("a", "sleepy", Phase::Idle, None),
        Git,
        "/w/sleepy",
        true,
        stale.clone(),
    );
    suspended.exit_cause = wire::ExitCause::Suspended as i32;
    let mut exited = in_workspace(
        row("a", "gone", Phase::Idle, None),
        Git,
        "/w/gone",
        true,
        stale.clone(),
    );
    exited.lifecycle = wire::Lifecycle::Exited as i32;
    let mut fleet = FleetState::new();
    for msg in [
        host("a"),
        agent(in_workspace(
            row("a", "awake", Phase::Idle, None),
            Git,
            "/w/awake",
            true,
            git("awake", totals(1, 2, 0), None),
        )),
        agent(suspended),
        agent(exited),
        caught_up(),
    ] {
        fleet.update(msg);
    }

    let awake = fleet.delete_warning(&at("a", "awake")).unwrap();
    assert!(
        matches!(&awake, DeleteWarning::Goes(goes) if goes.uncommitted == totals(1, 2, 0)),
        "{awake:?}"
    );
    assert!(awake.discards());

    for name in ["sleepy", "gone"] {
        let checking = fleet.delete_warning(&at("a", name)).unwrap();
        assert!(
            matches!(&checking, DeleteWarning::Checking(goes) if goes.branch.as_deref() == Some("fix")),
            "{checking:?}"
        );
        assert!(!checking.discards(), "nothing named yet");

        let diff = wire::Diff {
            files: vec![
                wire::DiffFile {
                    path: "README.md".into(),
                    added: 1,
                    removed: 0,
                    ..Default::default()
                },
                wire::DiffFile {
                    path: "notes.txt".into(),
                    added: 1,
                    removed: 0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let found = checking.clone().answered::<()>(Ok(&diff));
        assert_eq!(
            found,
            DeleteWarning::Goes(ui_state::WorkspaceGoes {
                uncommitted: totals(2, 2, 0),
                branch: Some("fix".into()),
            })
        );
        assert!(found.discards());

        let clean = checking.clone().answered::<()>(Ok(&wire::Diff::default()));
        assert!(
            matches!(&clean, DeleteWarning::Goes(goes) if goes.uncommitted.is_none()),
            "{clean:?}"
        );
        assert!(!clean.discards());

        let failed = checking.answered(Err("unreachable"));
        assert!(matches!(failed, DeleteWarning::Unknown(_)), "{failed:?}");
        assert!(!failed.discards(), "no consent without an answer");
    }
}
