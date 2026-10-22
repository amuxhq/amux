//! Families' stories that hold on either net: a far child stopped or
//! deleted on its host's return after its parent was, a stop reaching
//! every descendant on every host, and a one-shot sub-agent staying for
//! its child's last report and telling its parent when it is stopped.
//! Each runs on the real net by default, and under
//! `TESTNET_MODE=simulated` on the simulated one by seed (`just sim`).

#![cfg(unix)]

use testnet::modes::run_story;

#[tokio::test(flavor = "multi_thread")]
async fn a_far_child_whose_parent_stopped_while_its_host_was_away_stops_on_return() {
    run_story(
        stories::desk_and_laptop,
        stories::a_far_child_whose_parent_stopped_while_its_host_was_away_stops_on_return,
        stories::a_far_child_whose_parent_stopped_while_its_host_was_away_stops_on_return,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_far_child_whose_parent_was_deleted_while_its_host_was_away_is_deleted_on_return() {
    run_story(
        stories::desk_and_laptop,
        stories::a_far_child_whose_parent_was_deleted_while_its_host_was_away_is_deleted_on_return,
        stories::a_far_child_whose_parent_was_deleted_while_its_host_was_away_is_deleted_on_return,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_parent_stops_every_descendant_on_every_host() {
    run_story(
        stories::three_hosts,
        stories::stopping_a_parent_stops_every_descendant_on_every_host,
        stories::stopping_a_parent_stops_every_descendant_on_every_host,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sub_agent_waits_for_its_running_child_and_reports_its_result_up() {
    run_story(
        stories::three_hosts,
        stories::a_sub_agent_waits_for_its_running_child_and_reports_its_result_up,
        stories::a_sub_agent_waits_for_its_running_child_and_reports_its_result_up,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sub_agent_waits_for_its_childs_last_report_not_its_mid_way_one() {
    run_story(
        stories::three_hosts,
        stories::a_sub_agent_waits_for_its_childs_last_report_not_its_mid_way_one,
        stories::a_sub_agent_waits_for_its_childs_last_report_not_its_mid_way_one,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sub_agent_stopped_while_it_waits_tells_its_parent_it_stopped() {
    run_story(
        stories::three_hosts,
        stories::a_sub_agent_stopped_while_it_waits_tells_its_parent_it_stopped,
        stories::a_sub_agent_stopped_while_it_waits_tells_its_parent_it_stopped,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_away_parents_rows_wait_and_a_stale_incarnation_is_dropped() {
    run_story(
        stories::desk_and_server,
        stories::an_away_parents_rows_wait_and_a_stale_incarnation_is_dropped,
        stories::an_away_parents_rows_wait_and_a_stale_incarnation_is_dropped,
    )
    .await;
}

mod stories {
    use std::time::Duration;

    use provider_fakes::script::Step;
    use store::AgentRow;
    use testnet::{AgentDecl, NetError, Topology, World};
    use wire::Lifecycle;

    fn text(text: &str) -> Step {
        Step::Text {
            chunks: vec![text.to_owned()],
        }
    }

    fn says(label: &str) -> Vec<Step> {
        vec![text(label), Step::TurnEnd]
    }

    /// The lead: one turn that plans, then idle, taking what its children
    /// send it as turns of their own.
    fn lead() -> AgentDecl {
        AgentDecl::new("lead", "desk")
            .prompt("Plan the release survey.")
            .steps(says("I will have a helper on the server run the suite."))
    }

    fn child(name: &str, host: &str, steps: Vec<Step>) -> AgentDecl {
        AgentDecl::new(name, host)
            .prompt("Run the test suite and report.")
            .steps(steps)
    }

    /// A turn that waits on gate `name` in the net's gate directory.
    fn waits(name: &str) -> Step {
        Step::WaitFor { path: name.into() }
    }

    pub fn desk_and_laptop() -> Topology {
        Topology::new()
            .host("desk")
            .host("laptop")
            .link("desk", "laptop")
            .agent(lead())
    }

    pub fn desk_and_server() -> Topology {
        Topology::new()
            .host("desk")
            .host("server")
            .link("desk", "server")
            .agent(lead())
    }

    pub fn three_hosts() -> Topology {
        Topology::new()
            .host("desk")
            .host("server")
            .host("phone")
            .link("desk", "server")
            .link("desk", "phone")
            .link("server", "phone")
            .agent(lead())
    }

    /// `agent`'s row as `host` holds it: its own row there, or a replica.
    async fn row(net: &impl World, host: &str, agent: &str) -> Option<AgentRow> {
        let key = net.agent(agent).unwrap().key();
        net.read_store(host, move |store| store.agent(&key).unwrap())
            .await
            .unwrap()
    }

    async fn lifecycle(net: &impl World, host: &str, agent: &str) -> Option<Lifecycle> {
        row(net, host, agent)
            .await
            .and_then(|row| Lifecycle::try_from(row.lifecycle).ok())
    }

    /// What `agent`'s own host holds for it that came in as an input other
    /// than its first prompt: the agent messages it accepted.
    async fn received(net: &impl World, agent: &str) -> Vec<wire::Item> {
        let at = net.agent(agent).unwrap().clone();
        let key = at.key();
        net.read_store(&at.host, move |store| {
            store.page(&key, None, 1_000).unwrap().items
        })
        .await
        .unwrap()
        .into_iter()
        .filter(|item| !item.input_id.is_empty() && item.input_id != b"testnet-first")
        .collect()
    }

    async fn with_input(net: &impl World, agent: &str, input_id: &[u8]) -> usize {
        received(net, agent)
            .await
            .iter()
            .filter(|item| item.input_id == input_id)
            .count()
    }

    /// The reports `host` holds for parents it has yet to hand them to.
    async fn outbox(net: &impl World, host: &str) -> usize {
        net.read_store(host, |store| store.deliveries().unwrap().len())
            .await
            .unwrap()
    }

    async fn until_outbox(net: &impl World, what: &str, host: &str, wanted: usize) {
        net.until(what, || async move {
            let rows = outbox(net, host).await;
            (rows == wanted)
                .then_some(())
                .ok_or_else(|| format!("{rows} outbox rows"))
        })
        .await
        .unwrap();
    }

    async fn until_lifecycle(net: &impl World, host: &str, agent: &str, wanted: Lifecycle) {
        net.until(
            &format!("{agent} to be {wanted:?} on {host}"),
            || async move {
                let now = lifecycle(net, host, agent).await;
                (now == Some(wanted))
                    .then_some(())
                    .ok_or_else(|| format!("{agent} {now:?}"))
            },
        )
        .await
        .unwrap();
    }

    async fn until_exited(net: &impl World, host: &str, agent: &str) {
        until_lifecycle(net, host, agent, Lifecycle::Exited).await;
    }

    async fn until_live(net: &impl World, host: &str, agent: &str) {
        until_lifecycle(net, host, agent, Lifecycle::Live).await;
    }

    /// Waits until the lead has received a message carrying `wanted`.
    async fn lead_hears(net: &impl World, what: &str, wanted: &str) -> Result<(), NetError> {
        net.until(what, || async {
            let got = received(net, "lead").await;
            got.iter()
                .any(|item| item.text.contains(wanted))
                .then_some(())
                .ok_or_else(|| {
                    format!(
                        "lead received {:?}",
                        got.iter().map(|item| &item.text).collect::<Vec<_>>()
                    )
                })
        })
        .await
    }

    /// A parent stopped while its far child's host is away: nothing reaches
    /// the child then, and it carries on. Once its host is back and has caught
    /// up with the parent's host, the listing says the parent exited and the
    /// child's own host stops it. The stop to an away host used to be sent once
    /// and lost.
    pub async fn a_far_child_whose_parent_stopped_while_its_host_was_away_stops_on_return(
        mut net: impl World,
    ) {
        net.spawn_child(
            "lead",
            child("scout", "laptop", vec![waits("never"), Step::TurnEnd]),
        )
        .await
        .unwrap();
        until_live(&net, "laptop", "scout").await;
        net.sever_link("desk", "laptop").unwrap();
        net.wait_link("desk", "laptop", false).await.unwrap();
        net.stop("lead").await.unwrap();
        until_exited(&net, "desk", "lead").await;
        assert_eq!(
            lifecycle(&net, "laptop", "scout").await,
            Some(Lifecycle::Live),
            "scout runs on while laptop cannot hear of the stop"
        );
        println!("lead stopped with laptop away: scout runs on");

        net.restore_link("desk", "laptop").await.unwrap();
        until_exited(&net, "laptop", "scout").await;
        let scout = row(&net, "laptop", "scout").await.unwrap();
        println!(
            "laptop back: scout ended {:?}",
            wire::ExitCause::try_from(scout.exit_cause)
        );
        assert_eq!(scout.exit_cause, wire::ExitCause::Stopped as i32);
        net.shutdown().await.unwrap();
    }

    /// A parent deleted while its far child's host is away. Its child on its
    /// own host follows at once, by the same rule; the far child carries on
    /// until its host is back and caught up, where the parent is listed no
    /// more, and then its own host stops and deletes it. A delete used to be
    /// refused outright while part of the family was out of reach.
    pub async fn a_far_child_whose_parent_was_deleted_while_its_host_was_away_is_deleted_on_return(
        mut net: impl World,
    ) {
        net.spawn_child(
            "lead",
            child("notes", "desk", vec![waits("never"), Step::TurnEnd]),
        )
        .await
        .unwrap();
        net.spawn_child(
            "lead",
            child("scout", "laptop", vec![waits("never"), Step::TurnEnd]),
        )
        .await
        .unwrap();
        until_live(&net, "desk", "notes").await;
        until_live(&net, "laptop", "scout").await;
        let notes = net.agent("notes").unwrap().key();
        let scout = net.agent("scout").unwrap().key();
        net.sever_link("desk", "laptop").unwrap();
        net.wait_link("desk", "laptop", false).await.unwrap();

        net.delete("lead").await.expect("the delete is not refused");
        net.until("notes to follow lead on desk", || async {
            let notes = notes.clone();
            let held = net
                .read_store("desk", move |store| store.agent(&notes).unwrap())
                .await
                .map_err(|error| error.to_string())?;
            held.is_none()
                .then_some(())
                .ok_or_else(|| "desk still holds notes".to_owned())
        })
        .await
        .unwrap();
        assert_eq!(
            lifecycle(&net, "laptop", "scout").await,
            Some(Lifecycle::Live),
            "scout runs on while laptop cannot hear of the delete"
        );
        println!("lead deleted with laptop away: notes deleted on desk, scout runs on");

        net.restore_link("desk", "laptop").await.unwrap();
        net.until("laptop to delete scout", || async {
            let scout = scout.clone();
            let held = net
                .read_store("laptop", move |store| store.agent(&scout).unwrap())
                .await
                .map_err(|error| error.to_string())?;
            held.is_none()
                .then_some(())
                .ok_or_else(|| format!("laptop holds scout as {:?}", held.map(|row| row.lifecycle)))
        })
        .await
        .unwrap();
        println!("laptop back: scout stopped and deleted there");
        net.shutdown().await.unwrap();
    }

    /// Stopping a parent stops its whole family: each descendant's own host
    /// stops it once it sees its parent's row say exited, on the parent's own
    /// host as on another, and that stop is a row its own children's hosts
    /// see in turn. Each child here is mid-turn on a gate that never opens;
    /// only a stop ends it, as stopped.
    pub async fn stopping_a_parent_stops_every_descendant_on_every_host(mut net: impl World) {
        net.turn_ended("lead", 1).await.unwrap();
        let busy = || vec![waits("never"), text("Survey part done."), Step::TurnEnd];
        net.spawn_child("lead", child("notes", "desk", busy()))
            .await
            .unwrap();
        net.spawn_child("lead", child("helper", "server", busy()))
            .await
            .unwrap();
        net.spawn_child("helper", child("scout", "phone", busy()))
            .await
            .unwrap();
        let family = [("notes", "desk"), ("helper", "server"), ("scout", "phone")];
        net.until("every descendant to be live on its host", || async {
            let mut held = Vec::new();
            for (name, host) in family {
                held.push((name, lifecycle(&net, host, name).await));
            }
            held.iter()
                .all(|(_, lifecycle)| *lifecycle == Some(Lifecycle::Live))
                .then_some(())
                .ok_or_else(|| format!("{held:?}"))
        })
        .await
        .unwrap();

        net.stop("lead").await.unwrap();
        until_exited(&net, "desk", "lead").await;
        println!("lead stopped");

        let mut causes = Vec::new();
        for (name, host) in family {
            until_exited(&net, host, name).await;
            let row = row(&net, host, name).await.unwrap();
            causes.push((name, wire::ExitCause::try_from(row.exit_cause).ok()));
        }
        println!("how each descendant ended: {causes:?}");
        assert!(
            causes
                .iter()
                .all(|(_, cause)| *cause == Some(wire::ExitCause::Stopped)),
            "stopping lead stopped none of its descendants; they ran on to the end of their turns: {causes:?}"
        );
        net.shutdown().await.unwrap();
    }

    /// A one-shot sub-agent exits only once its turn has ended, its queue is
    /// empty and no child it spawned is still running. Here the middle agent
    /// ends its first turn while its scout still works; it stays, the scout's
    /// report starts its next turn, and it reports up to the lead. QA: the
    /// middle agent exited at once, the scout's report had nowhere to go, and
    /// the lead waited forever.
    pub async fn a_sub_agent_waits_for_its_running_child_and_reports_its_result_up(
        mut net: impl World,
    ) {
        net.turn_ended("lead", 1).await.unwrap();
        net.spawn_child(
            "lead",
            child(
                "mid",
                "server",
                vec![
                    waits("mid-go"),
                    text("Started the scout."),
                    Step::TurnEnd,
                    text("The scout reported; all done."),
                    Step::TurnEnd,
                ],
            ),
        )
        .await
        .unwrap();
        net.spawn_child(
            "mid",
            child(
                "scout",
                "phone",
                vec![waits("scout-go"), text("Scout done."), Step::TurnEnd],
            ),
        )
        .await
        .unwrap();
        net.open_gate("mid-go").unwrap();
        net.turn_ended("mid", 1).await.unwrap();
        net.open_gate("scout-go").unwrap();
        let reached = lead_hears(
            &net,
            "the lead to hear mid report the scout's result",
            "all done",
        )
        .await;
        let mid = row(&net, "server", "mid").await.unwrap();
        println!(
            "mid ended {:?}: {:?}",
            Lifecycle::try_from(mid.lifecycle),
            wire::ExitCause::try_from(mid.exit_cause)
        );
        net.shutdown().await.unwrap();
        reached.expect(
            "mid exited while its scout still ran, and the scout's result never reached the lead",
        );
    }

    /// A one-shot sub-agent waits for its child's last report, not its first.
    /// Its child reports mid-way (it started a scout of its own) and again when
    /// the scout's result is in; the sub-agent takes the mid-way report as a
    /// turn of its own and stays, and reports up only once the child's last
    /// report has come. Every hop crosses hosts.
    pub async fn a_sub_agent_waits_for_its_childs_last_report_not_its_mid_way_one(
        mut net: impl World,
    ) {
        net.turn_ended("lead", 1).await.unwrap();
        net.spawn_child(
            "lead",
            child(
                "planner",
                "server",
                vec![
                    waits("planner-go"),
                    text("Started mid."),
                    Step::TurnEnd,
                    text("Mid is still on it."),
                    Step::TurnEnd,
                    text("Mid reported the scout's result; planner done."),
                    Step::TurnEnd,
                ],
            ),
        )
        .await
        .unwrap();
        net.spawn_child(
            "planner",
            child(
                "mid",
                "phone",
                vec![
                    waits("mid-go"),
                    text("Started the scout."),
                    Step::TurnEnd,
                    text("The scout reported; mid done."),
                    Step::TurnEnd,
                ],
            ),
        )
        .await
        .unwrap();
        net.spawn_child(
            "mid",
            child(
                "scout",
                "desk",
                vec![waits("scout-go"), text("Scout done."), Step::TurnEnd],
            ),
        )
        .await
        .unwrap();
        net.open_gate("planner-go").unwrap();
        net.turn_ended("planner", 1).await.unwrap();
        net.open_gate("mid-go").unwrap();
        net.turn_ended("planner", 2).await.unwrap();
        let planner = row(&net, "server", "planner").await.unwrap();
        assert_eq!(
            Lifecycle::try_from(planner.lifecycle),
            Ok(Lifecycle::Live),
            "the planner left on mid's mid-way report"
        );
        net.open_gate("scout-go").unwrap();
        let reached = lead_hears(
            &net,
            "the lead to hear the planner report mid's result",
            "planner done",
        )
        .await;
        net.shutdown().await.unwrap();
        reached.expect("the planner never reported mid's last result to the lead");
    }

    /// Every child incarnation ends with a last report. A sub-agent stopped
    /// while it waits idle for its own child has no turn to cut short, yet its
    /// parent still hears that it stopped: a parent waiting on it would
    /// otherwise wait for good.
    pub async fn a_sub_agent_stopped_while_it_waits_tells_its_parent_it_stopped(
        mut net: impl World,
    ) {
        net.turn_ended("lead", 1).await.unwrap();
        net.spawn_child(
            "lead",
            child(
                "mid",
                "server",
                vec![waits("mid-go"), text("Started the scout."), Step::TurnEnd],
            ),
        )
        .await
        .unwrap();
        net.spawn_child(
            "mid",
            child(
                "scout",
                "phone",
                vec![waits("never"), text("Scout done."), Step::TurnEnd],
            ),
        )
        .await
        .unwrap();
        net.open_gate("mid-go").unwrap();
        net.turn_ended("mid", 1).await.unwrap();
        net.stop("mid").await.unwrap();
        let reached = net
            .until("the lead to hear that mid stopped", || async {
                let got = received(&net, "lead").await;
                let texts: Vec<&String> = got.iter().map(|item| &item.text).collect();
                (texts.len() >= 2 && texts.iter().any(|text| text.contains("stopped")))
                    .then_some(())
                    .ok_or_else(|| format!("lead received {texts:?}"))
            })
            .await;
        net.shutdown().await.unwrap();
        reached.expect("the lead never heard that mid stopped");
    }

    /// A child whose parent's host is away carries on: its finished message
    /// waits in its host's outbox, and arrives once when the host returns. A
    /// row for a parent incarnation that has since ended is dropped instead;
    /// and the parent's own daemon drops a message for any incarnation but
    /// the current one.
    pub async fn an_away_parents_rows_wait_and_a_stale_incarnation_is_dropped(mut net: impl World) {
        net.spawn_child(
            "lead",
            child(
                "helper",
                "server",
                vec![waits("finish"), text("Suite green."), Step::TurnEnd],
            ),
        )
        .await
        .unwrap();
        net.sever_link("desk", "server").unwrap();
        net.wait_link("desk", "server", false).await.unwrap();
        net.open_gate("finish").unwrap();
        until_outbox(&net, "helper's finished row", "server", 1).await;
        net.advance(Duration::from_secs(120)).unwrap();
        // A window: the retry runs on the policy clock and finds no parent
        // row to hand to; nothing the story controls marks that attempt.
        net.holds_for(
            "the row to wait for its parent",
            Duration::from_secs(1),
            || async { outbox(&net, "server").await == 1 },
        )
        .await
        .unwrap();
        assert!(received(&net, "lead").await.is_empty());
        println!("desk away: helper's finished message waits on server");
        net.restore_link("desk", "server").await.unwrap();
        until_outbox(&net, "the row to be delivered", "server", 0).await;
        let got = received(&net, "lead").await;
        assert_eq!(got.len(), 1, "one finished message: {got:?}");
        println!("desk back: lead received {:?}", got[0].text);

        // A second child finishes while the parent's host is away, and the
        // parent is resumed there meanwhile: the row is for an incarnation
        // that is not waiting for anything.
        net.spawn_child(
            "lead",
            child(
                "second",
                "server",
                vec![waits("second"), text("Done too."), Step::TurnEnd],
            ),
        )
        .await
        .unwrap();
        net.sever_link("desk", "server").unwrap();
        net.wait_link("desk", "server", false).await.unwrap();
        net.open_gate("second").unwrap();
        until_outbox(&net, "second's finished row", "server", 1).await;
        net.stop("lead").await.unwrap();
        net.resume("lead", None).await.unwrap();
        net.restore_link("desk", "server").await.unwrap();
        until_outbox(&net, "the stale row to be dropped", "server", 0).await;
        assert_eq!(
            received(&net, "lead").await.len(),
            1,
            "the resumed lead receives nothing"
        );
        println!("lead resumed while desk was away: second's row dropped as stale");

        // The parent's daemon makes the same check itself.
        let helper = net.agent("helper").unwrap().clone();
        let lead = net.agent("lead").unwrap().parent();
        let current = row(&net, "desk", "lead").await.unwrap().incarnation;
        let stale = wire::Envelope {
            id: b"stale".to_vec(),
            from: Some(wire::Sender {
                value: Some(wire::sender::Value::Agent(wire::AgentSender {
                    agent_id: helper.id.as_bytes().to_vec(),
                    host_id: helper.host_id.as_bytes().to_vec(),
                    name: "helper".into(),
                })),
            }),
            to: Some(lead),
            body: Some(wire::envelope::Body::Report(wire::ChildReport {
                to_incarnation: current - 1,
                end: Some(wire::child_report::End::TurnEnded(wire::TurnEnded {
                    text: "late news".into(),
                    ..Default::default()
                })),
                ..Default::default()
            })),
            ..wire::Envelope::default()
        };
        let refused = match net.peer_message("server", "desk", stale).await {
            Err(NetError::Refused(status)) => status,
            other => panic!("desk took a message for a past incarnation: {other:?}"),
        };
        assert_eq!(refused.code(), tonic::Code::NotFound);
        assert_eq!(with_input(&net, "lead", b"stale").await, 0);
        println!(
            "a message for lead's incarnation {} reaching desk: dropped ({})",
            current - 1,
            refused.message()
        );
        net.shutdown().await.unwrap();
    }
}
