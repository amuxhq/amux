//! The composed net explored over two hosts' real daemon cores and one
//! agent's real agent core, at the bounds `just test` runs, and walked by
//! seed.

use models::net::agent_id;
use models::{Bounds, ComposedNet, Machine as _, check, walk};

#[test]
fn every_composed_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    for (name, model) in ComposedNet::ci() {
        let verdict = check(&model, Bounds::exhaustive().threads(threads));
        println!("{name}:\n{verdict}");
        verdict.assert_holds();
    }
}

#[test]
fn one_seed_walks_one_composed_schedule() {
    let model = ComposedNet::reset_while_flipping();
    let bounds = Bounds::depth(200);
    let first = walk(&model, bounds, 7);
    let again = walk(&model, bounds, 7);
    println!("{first}");
    assert_eq!(
        first.actions, again.actions,
        "one seed walked two schedules"
    );
    assert!(first.failure.is_none(), "{first}");
    let other = walk(&model, bounds, 8);
    assert_ne!(
        first.actions, other.actions,
        "two seeds walked one schedule"
    );
}

#[test]
fn a_kill_keeps_the_generation_and_an_unclean_reboot_takes_the_next_and_the_agent_with_it() {
    let model = ComposedNet::quiet();
    let mut state = model.init();
    let net = &mut state.net;
    let agent = agent_id("worker");
    let first = net.process_of(agent).expect("the agent runs");

    net.stop_host(0);
    net.start_host(0, [1; 32]);
    assert_eq!(net.generation(0), 1, "a restart on the same boot");
    assert_eq!(
        net.process_of(agent),
        Some(first),
        "the agent outlived its daemon"
    );

    net.reboot_unclean(0, [2; 32]);
    assert_eq!(net.generation(0), 2, "a new boot after an unclean run");
    assert!(
        !net.processes()[first].running,
        "the agent's process went down with the machine"
    );
    for _ in 0..10_000 {
        let Some(&lane) = net.deliverable().first() else {
            break;
        };
        net.deliver(lane);
    }
    assert!(net.failure().is_none(), "{:?}", net.failure());
    assert_ne!(
        net.process_of(agent),
        Some(first),
        "the restarted daemon adopted a process that did not survive the reboot"
    );
}

#[test]
fn links_come_up_on_the_cores_dials_and_a_cut_one_on_the_redial_waiting_for_it() {
    let model = ComposedNet::quiet();
    let mut state = model.init();
    let net = &mut state.net;
    let settle = |net: &mut models::net::Net| {
        for _ in 0..10_000 {
            let Some(&lane) = net.deliverable().first() else {
                break;
            };
            net.deliver(lane);
        }
        assert!(net.failure().is_none(), "{:?}", net.failure());
    };
    assert!(net.is_live(0), "the hosts dialled each other at start");

    // Cut, the link is noticed gone only after it lived: each end redials
    // at once, and the dial waits on the cut link rather than failing.
    let at = net.now_ms();
    net.sever(0);
    settle(net);
    assert!(!net.is_live(0));
    net.restore(0);
    settle(net);
    assert!(net.is_live(0), "the redial waiting on the link took it");
    assert_eq!(net.now_ms(), at, "with no clock moved");
    assert!(net.reads_direct(0, 1) && net.reads_direct(1, 0));

    // A host that starts again dials the other, which the other's own
    // pause would not have done yet.
    net.stop_host(1);
    settle(net);
    assert!(!net.is_live(0));
    net.start_host(1, [3; 32]);
    settle(net);
    assert!(net.is_live(0), "the started host dialled its peer");
    assert!(net.reads_direct(0, 1) && net.reads_direct(1, 0));
}

/// The nightly bounds, run by hand or by the nightly recipe: minutes on the
/// enrolled Mac's cores.
#[test]
#[ignore = "nightly bounds: minutes on every core"]
fn every_composed_promise_holds_at_nightly_bounds() {
    for (name, model) in ComposedNet::nightly() {
        let verdict = check(&model, Bounds::exhaustive().threads(models::threads()));
        println!("{name}:\n{verdict}");
        verdict.assert_holds();
    }
}
