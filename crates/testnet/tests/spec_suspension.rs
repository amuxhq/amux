//! Suspension across hosts, under the `suspension/1` experiment, with real
//! daemons on a loopback link and real agents on the fakes. A parent that
//! suspends keeps a live row, and a report from its far child wakes it; a
//! power cut of the parent's machine leaves each agent
//! as it was, idle ones suspended and one caught in its turn continued.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use node::Launch;
use provider_fakes::script::Step;
use settings::experiments::suspension::Suspension;
use store::AgentRow;
use testnet::{AgentDecl, Net, NetOptions, Topology, until};
use wire::{ExitCause, Lifecycle, Phase};

fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

/// A turn that waits on gate `name` in the net's gate directory.
fn waits(name: &str) -> Step {
    Step::WaitFor { path: name.into() }
}

/// The lead on the desk: one turn that plans, then idle, taking what its
/// children send it as turns of their own.
fn lead() -> AgentDecl {
    AgentDecl::new("lead", "desk")
        .prompt("Plan the release survey.")
        .steps(vec![text("A helper will run the suite."), Step::TurnEnd])
}

/// The lead's helper on the server, whose turn ends once `gate` opens.
fn helper(gate: &str) -> AgentDecl {
    AgentDecl::new("helper", "server")
        .prompt("Run the test suite and report.")
        .steps(vec![waits(gate), text("Suite green."), Step::TurnEnd])
}

fn desk_and_server() -> Topology {
    Topology::new()
        .host("desk")
        .host("server")
        .link("desk", "server")
        .agent(lead())
}

/// Every host runs the experiment, an agent with no parent suspending
/// after `after` of quiet.
fn options(after: Duration) -> NetOptions {
    NetOptions {
        launch: Some(Arc::new(move |_host: &str, launch: &mut Launch| {
            launch.experiments =
                settings::experiments::Enabled::of(&[&settings::experiments::SUSPENSION]);
            launch.suspension = Suspension { after };
        })),
        ..NetOptions::default()
    }
}

/// `agent`'s row as `host` holds it: its own row there, or a replica.
async fn row(net: &Net, host: &str, agent: &str) -> Option<AgentRow> {
    let key = net.agent(agent).unwrap().key();
    net.runtime(host)
        .unwrap()
        .read_store(move |store| store.agent(&key).unwrap())
        .await
        .unwrap()
}

/// Everything `agent`'s own host holds for it, oldest first.
async fn items(net: &Net, agent: &str) -> Vec<wire::Item> {
    let at = net.agent(agent).unwrap().clone();
    let key = at.key();
    net.runtime(&at.host)
        .unwrap()
        .read_store(move |store| store.page(&key, None, 1_000).unwrap().items)
        .await
        .unwrap()
}

/// What `agent` accepted as an input other than its first prompt.
async fn received(net: &Net, agent: &str) -> Vec<wire::Item> {
    items(net, agent)
        .await
        .into_iter()
        .filter(|item| !item.input_id.is_empty() && item.input_id != b"testnet-first")
        .collect()
}

fn suspended(row: &AgentRow) -> bool {
    row.lifecycle == Lifecycle::Live as i32 && row.exit_cause == ExitCause::Suspended as i32
}

async fn until_row(
    net: &Net,
    host: &str,
    agent: &str,
    what: &str,
    holds: impl Fn(&AgentRow) -> bool,
) {
    until(&format!("{agent} on {host} to be {what}"), || async {
        match row(net, host, agent).await {
            Some(row) if holds(&row) => Ok(()),
            other => Err(format!(
                "{:?}",
                other.map(|row| (row.lifecycle, row.exit_cause, row.phase, row.incarnation))
            )),
        }
    })
    .await
    .unwrap();
}

/// A parent suspends once its child's last report is in, and its row stays
/// live, so the server's replica reads it live. The person then brings the
/// finished child back for another turn, and the child's report resumes the
/// suspended parent across hosts, which takes it.
#[tokio::test(flavor = "multi_thread")]
async fn a_suspended_parent_is_woken_by_its_far_childs_report() {
    let mut net = Net::start_with(desk_and_server(), options(Duration::from_millis(500)))
        .await
        .unwrap();
    net.spawn_child("lead", helper("finish")).await.unwrap();
    net.open_gate("finish").unwrap();
    until_row(&net, "desk", "lead", "suspended", suspended).await;
    let replica = row(&net, "server", "lead").await.unwrap();
    assert_eq!(replica.lifecycle, Lifecycle::Live as i32, "{replica:?}");
    let taken = received(&net, "lead").await.len();
    assert_eq!(taken, 1, "the first report came before the suspension");
    println!("lead took helper's report, then suspended; the server reads it live");

    until_row(&net, "server", "helper", "finished", |row| {
        row.lifecycle == Lifecycle::Exited as i32
    })
    .await;
    net.resume("helper", Some("Run it once more."))
        .await
        .unwrap();
    until("lead to take helper's second report", || {
        let net = &net;
        async move {
            let got = received(net, "lead").await.len();
            (got > taken)
                .then_some(())
                .ok_or_else(|| format!("{got} received"))
        }
    })
    .await
    .unwrap();
    let lead = row(&net, "desk", "lead").await.unwrap();
    assert_eq!(lead.lifecycle, Lifecycle::Live as i32);
    assert!(lead.incarnation >= 2, "resumed: {lead:?}");
    println!(
        "helper's second report resumed lead as incarnation {}",
        lead.incarnation
    );
    net.shutdown().await.unwrap();
}

/// The desk loses power with the lead idle (waiting on its far child), a
/// quiet agent idle and a worker in its turn: it comes back under a new
/// boot with the idle two suspended and the worker continued with the
/// note that the machine restarted. No row reads exited, the far child
/// is not stopped, and its report reaches the lead.
#[tokio::test(flavor = "multi_thread")]
async fn a_power_cut_suspends_the_idle_and_continues_the_turn_it_cut_off() {
    let topology = desk_and_server()
        .agent(
            AgentDecl::new("quiet", "desk")
                .prompt("Say hello.")
                .steps(vec![text("Hello."), Step::TurnEnd]),
        )
        .agent(
            AgentDecl::new("worker", "desk")
                .prompt("Migrate the schema.")
                .steps(vec![waits("migrate"), text("Migrated."), Step::TurnEnd]),
        );
    // Long enough that nothing suspends on its own here.
    let mut net = Net::start_with(topology, options(Duration::from_secs(3_600)))
        .await
        .unwrap();
    net.spawn_child("lead", helper("finish")).await.unwrap();
    for agent in ["lead", "quiet"] {
        until_row(&net, "desk", agent, "idle", |row| {
            row.phase == Phase::Idle as i32
        })
        .await;
    }
    until_row(&net, "desk", "worker", "in its turn", |row| {
        row.phase == Phase::Working as i32
    })
    .await;

    net.checkpoint_host("desk").await.unwrap();
    net.rewind_host("desk", &[]).await.unwrap();
    println!("desk lost power and came back under a new boot");

    for agent in ["lead", "quiet"] {
        until_row(&net, "desk", agent, "suspended", suspended).await;
    }
    until_row(&net, "desk", "worker", "continued", |row| {
        row.lifecycle == Lifecycle::Live as i32 && row.incarnation == 2
    })
    .await;
    until("worker's continuation to be in its transcript", || {
        let net = &net;
        async move {
            items(net, "worker")
                .await
                .iter()
                .any(|item| wire::is_continued_input(&item.input_id))
                .then_some(())
                .ok_or_else(|| "no continuation yet".to_owned())
        }
    })
    .await
    .unwrap();
    let helper = row(&net, "server", "helper").await.unwrap();
    assert_eq!(helper.lifecycle, Lifecycle::Live as i32, "{helper:?}");
    println!("idle agents suspended, worker continued, helper carries on");

    net.open_gate("finish").unwrap();
    until("lead to take helper's report", || {
        let net = &net;
        async move {
            let got = received(net, "lead").await;
            got.iter()
                .any(|item| item.text.contains("Suite green."))
                .then_some(())
                .ok_or_else(|| format!("{} received", got.len()))
        }
    })
    .await
    .unwrap();
    println!("helper's report reached the lead it was sent to before the power cut");
    net.shutdown().await.unwrap();
}
