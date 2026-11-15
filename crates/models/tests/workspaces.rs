//! The workspace lifecycle explored over the real workspace core, for a
//! workspace of each origin, at the bounds `just test` runs.

use models::workspaces::Origin;
use models::{Bounds, WorkspaceLifecycle, check};

fn holds(name: &str, model: &WorkspaceLifecycle) {
    let verdict = check(model, Bounds::exhaustive().threads(models::threads()));
    println!("{name}: {verdict}");
    verdict.assert_holds();
}

#[test]
fn every_workspace_promise_holds_for_a_wt_tree_this_host_makes_at_ci_bounds() {
    holds("made wt tree", &WorkspaceLifecycle::ci(Origin::MadeWt));
}

#[test]
fn every_workspace_promise_holds_for_a_wt_tree_this_host_makes_held_by_everything_at_ci_bounds() {
    holds("made wt tree, held", &WorkspaceLifecycle::holders());
}

#[test]
fn every_workspace_promise_holds_for_a_git_worktree_this_host_makes_at_ci_bounds() {
    holds(
        "made git worktree",
        &WorkspaceLifecycle::ci(Origin::MadeGit),
    );
}

#[test]
fn every_workspace_promise_holds_for_a_wt_tree_made_by_hand_at_ci_bounds() {
    holds("hand-made wt tree", &WorkspaceLifecycle::ci(Origin::HandWt));
}

#[test]
fn every_workspace_promise_holds_for_another_hosts_wt_tree_at_ci_bounds() {
    holds(
        "another host's wt tree",
        &WorkspaceLifecycle::ci(Origin::OtherWt),
    );
}

#[test]
fn every_workspace_promise_holds_for_a_plain_folder_at_ci_bounds() {
    holds("folder", &WorkspaceLifecycle::ci(Origin::Folder));
}

/// The nightly bounds, run by hand or by the nightly recipe: 33 million
/// states in about 70 s on the enrolled Mac's cores, nearly all of them the
/// wt tree this host makes.
#[test]
#[ignore = "nightly bounds: minutes on every core"]
fn every_workspace_promise_holds_at_nightly_bounds() {
    for origin in Origin::ALL {
        holds(&format!("{origin:?}"), &WorkspaceLifecycle::nightly(origin));
    }
}
