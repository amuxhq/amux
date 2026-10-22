//! The harness's stories that hold on either net: observations failing at
//! their deadline, fences, a severed link and a killed daemon. Each runs on
//! the real net by default, and under `TESTNET_MODE=simulated` on the
//! simulated one by seed (`just sim`), held to the same verbs, fences and
//! observations.

#![cfg(unix)]

mod stories;

use testnet::modes::run_story;

#[tokio::test(flavor = "multi_thread")]
async fn a_stuck_observation_fails_at_its_deadline_and_a_closed_one_at_once() {
    run_story(
        stories::stuck_topology,
        stories::a_stuck_observation_fails_at_its_deadline_and_a_closed_one_at_once,
        stories::a_stuck_observation_fails_at_its_deadline_and_a_closed_one_at_once,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fence_holds_a_host_to_what_its_own_cursors_report() {
    run_story(
        stories::fence_topology,
        stories::a_fence_holds_a_host_to_what_its_own_cursors_report,
        stories::a_fence_holds_a_host_to_what_its_own_cursors_report,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_severed_link_goes_down_and_a_restored_one_comes_back() {
    run_story(
        stories::severed_topology,
        stories::a_severed_link_goes_down_and_a_restored_one_comes_back,
        stories::a_severed_link_goes_down_and_a_restored_one_comes_back,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_daemon_leaves_its_agents_running_and_its_restart_reads_what_they_wrote() {
    run_story(
        stories::killed_topology,
        stories::a_killed_daemon_leaves_its_agents_running_and_its_restart_reads_what_they_wrote,
        stories::a_killed_daemon_leaves_its_agents_running_and_its_restart_reads_what_they_wrote,
    )
    .await;
}
