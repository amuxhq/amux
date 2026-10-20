//! Replication's stories that hold on either net: a replica's tail, live
//! records, delta and Reset with their markers in sequence, pages served
//! from the block and from the origin, the source policy closing what no
//! client watches, and an exited agent's source closing and reopening on
//! its resume. Each runs on the real net by default, and under
//! `TESTNET_MODE=simulated` on the simulated one by seed (`just sim`).

#![cfg(unix)]

use testnet::modes::run_story;

#[tokio::test(flavor = "multi_thread")]
async fn a_replica_takes_a_tail_live_records_a_delta_and_a_reset_with_markers_in_sequence() {
    run_story(
        stories::tail_topology,
        stories::a_replica_takes_a_tail_live_records_a_delta_and_a_reset_with_markers_in_sequence,
        stories::a_replica_takes_a_tail_live_records_a_delta_and_a_reset_with_markers_in_sequence,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_serves_the_block_locally_extends_it_from_the_origin_and_errors_when_away() {
    run_story(
        stories::fetch_topology,
        stories::fetch_serves_the_block_locally_extends_it_from_the_origin_and_errors_when_away,
        stories::fetch_serves_the_block_locally_extends_it_from_the_origin_and_errors_when_away,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_to_on_demand_closes_every_source_no_client_watches() {
    run_story(
        stories::two_agents_topology,
        stories::switching_to_on_demand_closes_every_source_no_client_watches,
        stories::switching_to_on_demand_closes_every_source_no_client_watches,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn on_demand_warms_one_chat_listed_sweeps_the_rest_and_exited_agents_close() {
    run_story(
        stories::two_agents_topology,
        stories::on_demand_warms_one_chat_listed_sweeps_the_rest_and_exited_agents_close,
        stories::on_demand_warms_one_chat_listed_sweeps_the_rest_and_exited_agents_close,
    )
    .await;
}

mod stories {
    use std::time::Duration;

    use node::SourcePolicy;
    use provider_fakes::script::Step;
    use store::Marker;
    use testnet::observe::{self, Mark, marks};
    use testnet::{AgentDecl, NetError, PATIENCE, Topology, World};
    use wire::{Item, SessionEvent, session_event};

    fn text(text: &str) -> Step {
        Step::Text {
            chunks: vec![text.to_owned()],
        }
    }

    /// `turns` turns of `per_turn` messages each, "t<turn>-<n>".
    fn turns(turns: usize, per_turn: usize) -> Vec<Step> {
        let mut steps = Vec::new();
        for turn in 0..turns {
            for n in 0..per_turn {
                steps.push(text(&format!("t{turn}-{n}")));
            }
            steps.push(Step::TurnEnd);
        }
        steps
    }

    /// A desk and a laptop linked, each keeping a tail of `k`.
    fn desk_and_laptop(k: u32) -> Topology {
        Topology::new()
            .host_keeping("desk", k)
            .host_keeping("laptop", k)
            .link("desk", "laptop")
    }

    /// Every row the origin holds for `agent`, oldest first.
    pub async fn origin_rows(net: &impl World, agent: &str) -> Vec<Item> {
        let at = net.agent(agent).unwrap();
        let key = at.key();
        let mut rows = net
            .read_store(&at.host.clone(), move |store| {
                store.cut(&key, u32::MAX).unwrap().held
            })
            .await
            .unwrap();
        rows.sort_by_key(|item| item.order);
        rows
    }

    /// The origin's newest revision for `agent`: what a CaughtUp carries.
    pub async fn origin_revision(net: &impl World, agent: &str) -> u64 {
        let at = net.agent(agent).unwrap();
        let key = at.key();
        let row = net
            .read_store(&at.host.clone(), move |store| {
                store.agent(&key).unwrap().unwrap()
            })
            .await
            .unwrap();
        row.next_revision - 1
    }

    /// The replica's source cursor and marker at `host`.
    pub async fn replica_state(
        net: &impl World,
        host: &str,
        agent: &str,
    ) -> Option<(u64, Option<Marker>)> {
        let key = net.agent(agent).unwrap().key();
        net.read_store(host, move |store| {
            let row = store.agent(&key).unwrap()?;
            Some((row.source_cursor, store.cut(&key, 0).unwrap().marker))
        })
        .await
        .unwrap()
    }

    /// Waits until the origin's rows for `agent` hold a message saying
    /// `wanted`.
    pub async fn wait_origin_says(net: &impl World, agent: &str, wanted: &str) {
        net.until(&format!("{agent} to say {wanted}"), || async {
            let rows = origin_rows(net, agent).await;
            if rows.iter().any(|item| item.text.contains(wanted)) {
                Ok(())
            } else {
                Err(format!(
                    "origin rows: {:?}",
                    rows.iter()
                        .map(|item| item.text.as_str())
                        .collect::<Vec<_>>()
                ))
            }
        })
        .await
        .unwrap();
    }

    /// The marks after the last `Detached`.
    fn since_last_detached(events: &[SessionEvent]) -> Vec<Mark> {
        let all = marks(events);
        let from = all
            .iter()
            .rposition(|mark| *mark == Mark::Detached)
            .map_or(0, |at| at + 1);
        all[from..].to_vec()
    }

    /// The catch-up after the last `Detached` (or from the start): its marks
    /// up to and including its first CaughtUp. The observer hands over
    /// everything already delivered, so live records may follow; each must be
    /// a record revised after that CaughtUp, never a second opening.
    fn catch_up_since_detached(events: &[SessionEvent]) -> Vec<Mark> {
        let from = events
            .iter()
            .rposition(|event| matches!(event.of, Some(session_event::Of::Detached(_))))
            .map_or(0, |at| at + 1);
        let events = &events[from..];
        let end = events
            .iter()
            .position(|event| matches!(event.of, Some(session_event::Of::CaughtUp(_))))
            .unwrap_or_else(|| panic!("no CaughtUp: {:?}", marks(events)));
        let Some(session_event::Of::CaughtUp(caught_up)) = &events[end].of else {
            unreachable!()
        };
        for event in &events[end + 1..] {
            let revision = match &event.of {
                Some(session_event::Of::Item(item)) => item.revision,
                Some(session_event::Of::Append(append)) => append.revision,
                _ => 0,
            };
            assert!(
                revision > caught_up.revision,
                "only records revised after CaughtUp({}) follow it: {:?}",
                caught_up.revision,
                marks(events)
            );
        }
        marks(&events[..=end])
    }

    fn count_caught_up(events: &[SessionEvent]) -> usize {
        marks(events)
            .iter()
            .filter(|mark| matches!(mark, Mark::CaughtUp(_)))
            .count()
    }

    fn caught_up_since_detached(events: &[SessionEvent]) -> bool {
        since_last_detached(events)
            .iter()
            .any(|mark| matches!(mark, Mark::CaughtUp(_)))
    }

    pub async fn sever(net: &mut impl World) {
        net.sever_link("desk", "laptop").unwrap();
        net.wait_link("desk", "laptop", false).await.unwrap();
    }

    pub async fn restore(net: &mut impl World) {
        net.restore_link("desk", "laptop").await.unwrap();
        net.wait_link("desk", "laptop", true).await.unwrap();
    }

    /// Waits until the host's open sources are exactly `wanted`.
    pub async fn sources_are(net: &impl World, host: &str, what: &str, wanted: &[store::AgentKey]) {
        let mut wanted = wanted.to_vec();
        wanted.sort();
        net.until(what, || async {
            let open = net.open_sources(host).await.map_err(|e| e.to_string())?;
            (open == wanted)
                .then_some(())
                .ok_or_else(|| format!("open sources: {open:?}"))
        })
        .await
        .unwrap();
    }

    const TAIL_K: u32 = 6;

    pub fn tail_topology() -> Topology {
        desk_and_laptop(TAIL_K).agent(
            AgentDecl::new("worker", "desk")
                .steps(turns(8, 2))
                .prompt("go"),
        )
    }

    /// A replica starts from a tail of K, follows live records, takes a delta
    /// after a break that fits the cap and a Reset with a fresh tail after one
    /// that does not; its markers arrive in sequence with the rows they cover,
    /// again after every reconnect, and never while the host is away. A client
    /// asking for more than K rows gets K.
    pub async fn a_replica_takes_a_tail_live_records_a_delta_and_a_reset_with_markers_in_sequence(
        mut net: impl World,
    ) {
        const K: u32 = TAIL_K;
        wait_origin_says(&net, "worker", "t0-1").await;
        net.current("laptop", "worker").await.unwrap();

        let mut chat = net.observe("laptop", "worker", 50).await.unwrap();
        chat.observe_until(observe::caught_up, PATIENCE)
            .await
            .unwrap();

        // Live: each record reaches the chat, and the cursor follows it.
        net.send("worker", "two").await.unwrap();
        wait_origin_says(&net, "worker", "t1-1").await;
        chat.observe_until(
            |events| {
                events.iter().any(|event| {
                    matches!(&event.of, Some(session_event::Of::Item(item)) if item.text.contains("t1-1"))
                })
            },
            PATIENCE,
        )
        .await
        .unwrap();
        net.current("laptop", "worker").await.unwrap();

        // N is clamped to K, at the replica and at the origin alike.
        for host in ["laptop", "desk"] {
            let mut wide = net.observe(host, "worker", 50).await.unwrap();
            let opening = wide
                .observe_until(observe::caught_up, PATIENCE)
                .await
                .unwrap();
            let opening = catch_up_since_detached(opening);
            let held = opening
                .iter()
                .filter(|mark| matches!(mark, Mark::Item(_)))
                .count();
            assert_eq!(held, K as usize, "{host}: {}", wide.transcript());
            assert!(origin_rows(&net, "worker").await.len() > K as usize);
            assert!(
                matches!(opening[..], [Mark::Opening(_), Mark::Snapshot, ..]),
                "{host}: {}",
                wide.transcript()
            );
        }

        // A break that fits the cap: Detached, then the delta and CaughtUp,
        // with no Reset and nothing marked current while the host is away.
        sever(&mut net).await;
        chat.observe_until(
            |events| marks(events).last() == Some(&Mark::Detached),
            PATIENCE,
        )
        .await
        .unwrap();
        let caught_up_before = count_caught_up(chat.events());
        net.send("worker", "three").await.unwrap();
        wait_origin_says(&net, "worker", "t2-1").await;
        // A window: nothing the test controls gates a marker the laptop must
        // not mint while the host is away.
        net.holds_for(
            "no CaughtUp while the host is away",
            Duration::from_millis(300),
            || {
                let seen = count_caught_up(chat.events());
                async move { seen == caught_up_before }
            },
        )
        .await
        .unwrap();
        restore(&mut net).await;
        let events = chat
            .observe_until(caught_up_since_detached, PATIENCE)
            .await
            .unwrap();
        let delta = catch_up_since_detached(events);
        assert!(
            !delta.contains(&Mark::Reset),
            "a delta, not a Reset: {delta:?}"
        );
        assert!(
            delta.iter().any(|mark| matches!(mark, Mark::Item(_))),
            "the missed rows arrive before CaughtUp: {delta:?}"
        );
        net.current("laptop", "worker").await.unwrap();

        // A break beyond the cap: Reset, the Snapshot, a fresh tail of K, then
        // CaughtUp.
        sever(&mut net).await;
        chat.observe_until(
            |events| marks(events).last() == Some(&Mark::Detached),
            PATIENCE,
        )
        .await
        .unwrap();
        for turn in 3..7 {
            net.send("worker", &format!("turn {turn}")).await.unwrap();
            wait_origin_says(&net, "worker", &format!("t{turn}-1")).await;
        }
        restore(&mut net).await;
        let events = chat
            .observe_until(caught_up_since_detached, PATIENCE)
            .await
            .unwrap();
        let reset = catch_up_since_detached(events);
        assert_eq!(reset.first(), Some(&Mark::Reset), "{reset:?}");
        assert_eq!(reset.get(1), Some(&Mark::Snapshot), "{reset:?}");
        let tail = reset
            .iter()
            .filter(|mark| matches!(mark, Mark::Item(_)))
            .count();
        assert_eq!(tail, K as usize, "{reset:?}");
        net.current("laptop", "worker").await.unwrap();
        println!(
            "the laptop's chat on the desk's worker, K = {K}:\n{}",
            chat.transcript()
        );
        net.shutdown().await.unwrap();
    }

    const FETCH_K: u32 = 4;

    pub fn fetch_topology() -> Topology {
        desk_and_laptop(FETCH_K)
            .agent(
                AgentDecl::new("worker", "desk")
                    .steps(turns(4, 2))
                    .prompt("go"),
            )
            .agent(
                AgentDecl::new("other", "desk")
                    .steps(turns(4, 2))
                    .prompt("go"),
            )
    }

    /// Fetch serves the block from the replica's own rows; below it the origin
    /// answers and the page extends the block; with the origin away it is an
    /// error, never an empty page.
    pub async fn fetch_serves_the_block_locally_extends_it_from_the_origin_and_errors_when_away(
        mut net: impl World,
    ) {
        const K: u32 = FETCH_K;
        for agent in ["worker", "other"] {
            wait_origin_says(&net, agent, "t0-1").await;
            net.current("laptop", agent).await.unwrap();
        }
        // A break past the cap leaves each replica a block of the newest K.
        sever(&mut net).await;
        for agent in ["worker", "other"] {
            for turn in 1..3 {
                net.send(agent, &format!("turn {turn}")).await.unwrap();
                net.turn_ended(agent, turn + 1).await.unwrap();
            }
        }
        restore(&mut net).await;
        for agent in ["worker", "other"] {
            net.current("laptop", agent).await.unwrap();
        }
        // A replica can be current before the laptop has caught up on the
        // desk's inventory, and until then a page below the block is not
        // asked of the desk.
        net.until("the laptop to catch up on the desk", || async {
            net.host_ready("laptop", "desk")
                .await
                .map_err(|error| error.to_string())?
                .then_some(())
                .ok_or_else(|| "desk not ready at the laptop".to_owned())
        })
        .await
        .unwrap();

        // Inside the block: the replica's own rows.
        let top = net.fetch("laptop", "worker", None, 2).await.unwrap();
        assert_eq!(top.items.len(), 2);
        assert!(!top.exhausted);

        // Below it: the origin, and the block grows down to the first row.
        let origin = origin_rows(&net, "worker").await;
        assert!(origin.len() > K as usize);
        let all = net.fetch("laptop", "worker", None, 100).await.unwrap();
        assert_eq!(all.items.len(), origin.len());
        assert!(all.exhausted);
        let key = net.agent("worker").unwrap().key();
        let page = net
            .read_store("laptop", move |store| store.page(&key, None, 100).unwrap())
            .await
            .unwrap();
        assert_eq!(page.items.len(), origin.len());
        assert_eq!(
            page.end,
            store::PageEnd::Exhausted,
            "the origin said there is no more"
        );
        net.block_whole("laptop", "worker").await.unwrap();

        // With the origin away, what the block holds is still served, and
        // asking below it is an error.
        sever(&mut net).await;
        let held = net.fetch("laptop", "other", None, 100).await.unwrap();
        assert_eq!(
            held.items.len(),
            K as usize,
            "the block, and no claim of more"
        );
        assert!(!held.exhausted);
        let floor = held.items.last().unwrap().order;
        let below = net.fetch("laptop", "other", Some(floor), 10).await;
        assert!(
            matches!(
                below,
                Err(NetError::Serve(node::ServeError::OriginUnreachable))
            ),
            "{below:?}"
        );
        net.shutdown().await.unwrap();
    }

    pub fn two_agents_topology() -> Topology {
        desk_and_laptop(6)
            .agent(
                AgentDecl::new("named", "desk")
                    .steps(turns(4, 1))
                    .prompt("go"),
            )
            .agent(
                AgentDecl::new("quiet", "desk")
                    .steps(turns(4, 1))
                    .prompt("go"),
            )
    }

    /// A runtime switched to OnDemand with its link still up, as a phone put
    /// away while it still runs is, closes every source no client is
    /// subscribed to at once and keeps the watched one: work at the origin
    /// reaches only the watched chat until the runtime is Listed again. The
    /// watched chat's source opens again when its host comes back, without the
    /// client subscribing again.
    pub async fn switching_to_on_demand_closes_every_source_no_client_watches(mut net: impl World) {
        let named = net.agent("named").unwrap().key();
        let quiet = net.agent("quiet").unwrap().key();
        for agent in ["named", "quiet"] {
            wait_origin_says(&net, agent, "t0-0").await;
            net.current("laptop", agent).await.unwrap();
        }
        sources_are(
            &net,
            "laptop",
            "both sources open",
            &[named.clone(), quiet.clone()],
        )
        .await;

        let chat = net.observe("laptop", "named", 10).await.unwrap();
        net.set_source_policy("laptop", SourcePolicy::OnDemand)
            .unwrap();
        sources_are(
            &net,
            "laptop",
            "the unwatched source to close",
            std::slice::from_ref(&named),
        )
        .await;
        for agent in ["named", "quiet"] {
            net.send(agent, "two").await.unwrap();
            wait_origin_says(&net, agent, "t1-0").await;
        }
        net.current("laptop", "named").await.unwrap();
        let stale = replica_state(&net, "laptop", "quiet").await.unwrap();
        assert!(
            stale.0 < origin_revision(&net, "quiet").await,
            "quiet was left alone"
        );

        sever(&mut net).await;
        net.send("named", "three").await.unwrap();
        wait_origin_says(&net, "named", "t2-0").await;
        restore(&mut net).await;
        net.current("laptop", "named").await.unwrap();
        assert_eq!(net.open_sources("laptop").await.unwrap(), vec![named]);

        net.set_source_policy("laptop", SourcePolicy::Listed)
            .unwrap();
        net.current("laptop", "quiet").await.unwrap();
        drop(chat);
        net.shutdown().await.unwrap();
    }

    /// Under OnDemand no agent gets a source until a client subscribes, and
    /// then only that one: a background wake warms one chat. Switching to
    /// Listed sweeps every other agent current. A source for an agent that
    /// exits closes once its catch-up has landed, and opens again when its
    /// origin resumes it.
    pub async fn on_demand_warms_one_chat_listed_sweeps_the_rest_and_exited_agents_close(
        mut net: impl World,
    ) {
        let named = net.agent("named").unwrap().key();
        let quiet = net.agent("quiet").unwrap().key();
        for agent in ["named", "quiet"] {
            wait_origin_says(&net, agent, "t0-0").await;
            net.current("laptop", agent).await.unwrap();
        }
        sources_are(
            &net,
            "laptop",
            "both sources open",
            &[named.clone(), quiet.clone()],
        )
        .await;

        // The phone asleep: the link goes, work happens, the app wakes OnDemand.
        net.set_source_policy("laptop", SourcePolicy::OnDemand)
            .unwrap();
        sever(&mut net).await;
        for agent in ["named", "quiet"] {
            net.send(agent, "two").await.unwrap();
            wait_origin_says(&net, agent, "t1-0").await;
        }
        restore(&mut net).await;
        net.until("the laptop's inventory to catch up", || async {
            net.host_ready("laptop", "desk")
                .await
                .map_err(|error| error.to_string())?
                .then_some(())
                .ok_or_else(|| "desk not ready at the laptop".to_owned())
        })
        .await
        .unwrap();
        // A window: nothing the test controls gates a source that the policy
        // must not open.
        net.holds_for(
            "no source opens by itself",
            Duration::from_millis(300),
            || async {
                net.open_sources("laptop")
                    .await
                    .is_ok_and(|open| open.is_empty())
            },
        )
        .await
        .unwrap();

        // The notification names one chat; opening it brings that one current.
        let mut chat = net.observe("laptop", "named", 10).await.unwrap();
        chat.observe_until(caught_up_since_detached, PATIENCE)
            .await
            .unwrap();
        net.current("laptop", "named").await.unwrap();
        assert_eq!(
            net.open_sources("laptop").await.unwrap(),
            vec![named.clone()]
        );
        let stale = replica_state(&net, "laptop", "quiet").await.unwrap();
        assert!(
            stale.0 < origin_revision(&net, "quiet").await,
            "quiet was left alone"
        );

        // Foreground: every listed agent gets a source.
        net.set_source_policy("laptop", SourcePolicy::Listed)
            .unwrap();
        net.current("laptop", "quiet").await.unwrap();

        // An agent that exits: its source closes once its catch-up has landed.
        net.stop("quiet").await.unwrap();
        net.until("quiet's source to close", || async {
            let open = net
                .open_sources("laptop")
                .await
                .map_err(|e| e.to_string())?;
            (!open.contains(&quiet))
                .then_some(())
                .ok_or_else(|| format!("open sources: {open:?}"))
        })
        .await
        .unwrap();
        net.current("laptop", "quiet").await.unwrap();
        let row = replica_row(&net, "laptop", "quiet").await;
        assert_eq!(row.lifecycle, wire::Lifecycle::Exited as i32);
        assert_eq!(
            net.open_sources("laptop").await.unwrap(),
            vec![named.clone()]
        );

        // Resumed by its origin, it is followed again.
        net.resume("quiet", Some("again")).await.unwrap();
        net.until("quiet's source to reopen", || async {
            let open = net
                .open_sources("laptop")
                .await
                .map_err(|e| e.to_string())?;
            open.contains(&quiet)
                .then_some(())
                .ok_or_else(|| format!("open sources: {open:?}"))
        })
        .await
        .unwrap();
        wait_origin_says(&net, "quiet", "t0-0").await;
        net.current("laptop", "quiet").await.unwrap();
        let row = replica_row(&net, "laptop", "quiet").await;
        assert_eq!(
            (row.lifecycle, row.incarnation),
            (wire::Lifecycle::Live as i32, 2)
        );
        drop(chat);
        net.shutdown().await.unwrap();
    }

    /// The agent's row as `host` holds it.
    async fn replica_row(net: &impl World, host: &str, agent: &str) -> store::AgentRow {
        let key = net.agent(agent).unwrap().key();
        net.read_store(host, move |store| store.agent(&key).unwrap().unwrap())
            .await
            .unwrap()
    }
}
