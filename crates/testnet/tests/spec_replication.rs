//! Peer sources and the replica block, across real daemons on loopback
//! links with real agents on the fakes.
//!
//! A replica's rows for another host's agent are empty or one contiguous
//! block ending at the origin's newest row. These cases hold that where a
//! story needs the real net: a stream that dies right after its Snapshot
//! while a page lands a newer revision of an old row, a link back before
//! the follower looks, an exit whose records arrive after its Exited row,
//! trimming after a Reset, the source policy across an exit and a resume,
//! a rewound origin and an ask held open across its daemon's restart; and
//! they hold the markers in sequence with the rows they cover. The stories
//! that hold on either net are in `replication_stories`.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use node::Launch;
use prost::Message as _;
use provider_fakes::script::{Ask, Question, Step};
use store::{Marker, PageEnd};
use testnet::observe::{self, Mark, marks};
use testnet::{AgentDecl, JournalCut, Net, NetOptions, PATIENCE, Topology, until};
use wire::session_service_server::SessionService as _;
use wire::{Item, ListItemsRequest, SessionEvent, session_event};

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

/// A net whose hosts keep a tail of `k`, with `more` for any other
/// parameter by host.
fn options(k: u32, more: impl Fn(&str, &mut Launch) + Send + Sync + 'static) -> NetOptions {
    NetOptions {
        launch: Some(Arc::new(move |host: &str, launch: &mut Launch| {
            launch.tail_rows = k;
            more(host, launch);
        })),
        ..NetOptions::default()
    }
}

fn desk_and_laptop() -> Topology {
    Topology::new()
        .host("desk")
        .host("laptop")
        .link("desk", "laptop")
}

/// Every row the origin holds for `agent`, oldest first.
async fn origin_rows(net: &Net, agent: &str) -> Vec<Item> {
    let agent = net.agent(agent).unwrap().clone();
    let runtime = net.runtime(&agent.host).unwrap();
    let key = agent.key();
    let mut rows = runtime
        .read_store(move |store| store.cut(&key, u32::MAX).unwrap().held)
        .await
        .unwrap();
    rows.sort_by_key(|item| item.order);
    rows
}

/// The replica's source cursor and marker at `host`.
async fn replica_state(net: &Net, host: &str, agent: &str) -> Option<(u64, Option<Marker>)> {
    let key = net.agent(agent).unwrap().key();
    let runtime = net.runtime(host).unwrap();
    runtime
        .read_store(move |store| {
            let row = store.agent(&key).unwrap()?;
            Some((row.source_cursor, store.cut(&key, 0).unwrap().marker))
        })
        .await
        .unwrap()
}

/// Waits until the origin's journal for `agent` holds a message saying
/// `wanted`.
async fn wait_origin_says(net: &Net, agent: &str, wanted: &str) {
    until(&format!("{agent} to say {wanted}"), || async {
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

async fn fetch(
    net: &Net,
    host: &str,
    agent: &str,
    before_order: Option<u64>,
    limit: u32,
) -> Result<wire::ListItemsResponse, node::ServeError> {
    let id = net.agent(agent).unwrap().id;
    net.runtime(host)
        .unwrap()
        .list_items(&ListItemsRequest {
            agent_id: id.as_bytes().to_vec(),
            before_order,
            page_size: limit,
        })
        .await
}

async fn sever(net: &mut Net) {
    net.sever_link("desk", "laptop").unwrap();
    net.wait_link("desk", "laptop", false).await.unwrap();
}

async fn restore(net: &mut Net) {
    net.restore_link("desk", "laptop").await.unwrap();
    net.wait_link("desk", "laptop", true).await.unwrap();
}

/// What crosses from one host to another is compressed: a transcript of
/// build output reaches the replica in fewer bytes on the link, counted
/// with everything the link carried beside it, than the origin's rows hold.
#[tokio::test(flavor = "multi_thread")]
async fn a_replica_is_carried_compressed() {
    let output = (0..80)
        .map(|at| {
            let lines: String = (0..20)
                .map(|line| {
                    format!(
                        "   Compiling crate-{at}-{line} v0.{line}.{at} (/src/crates/c{at}{line})\n"
                    )
                })
                .collect();
            text(&format!(
                "{lines}Finished in {}.{:02}s",
                at % 7,
                at * 13 % 100
            ))
        })
        .chain([Step::TurnEnd])
        .collect();
    let topology =
        desk_and_laptop().agent(AgentDecl::new("worker", "desk").steps(output).prompt("go"));
    let net = Net::start_with(topology, options(200, |_, _| {}))
        .await
        .unwrap();
    wait_origin_says(&net, "worker", "Finished in 2.27s").await;
    net.current("laptop", "worker").await.unwrap();

    let rows: usize = origin_rows(&net, "worker")
        .await
        .iter()
        .map(|item| item.encoded_len())
        .sum();
    let carried = net
        .link_carried("desk", "laptop")
        .expect("desk and laptop are linked");
    println!("the origin holds {rows} bytes of rows; the link carried {carried}");
    assert!(
        carried * 2 < rows as u64,
        "the link carried {carried} bytes for {rows} bytes of rows"
    );
}

/// After a Reset the old block's rows lie below the new one; the replica
/// retention sweep takes them first and then trims the block to K, and
/// paging down from the top never skips an order. The sweep runs on the
/// retention interval of the policy clock.
#[tokio::test(flavor = "multi_thread")]
async fn trimming_after_a_reset_keeps_the_block_whole_and_runs_on_the_retention_interval() {
    const K: u32 = 4;
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(turns(6, 2))
            .prompt("go"),
    );
    let mut net = Net::start_with(
        topology,
        options(K, |host, launch| {
            if host == "laptop" {
                launch.replica_budget_bytes = 1;
            }
        }),
    )
    .await
    .unwrap();
    let key = net.agent("worker").unwrap().key();
    wait_origin_says(&net, "worker", "t0-1").await;
    net.current("laptop", "worker").await.unwrap();
    net.send("worker", "two").await.unwrap();
    wait_origin_says(&net, "worker", "t1-1").await;
    net.current("laptop", "worker").await.unwrap();
    // The whole history, paged down from the origin into the block.
    let all = fetch(&net, "laptop", "worker", None, 100).await.unwrap();
    assert!(all.exhausted);

    // Beyond the cap while away: the reconnect is a Reset, and the old
    // rows stay below the fresh tail.
    sever(&mut net).await;
    for turn in 2..5 {
        net.send("worker", &format!("turn {turn}")).await.unwrap();
        wait_origin_says(&net, "worker", &format!("t{turn}-1")).await;
    }
    restore(&mut net).await;
    net.current("laptop", "worker").await.unwrap();
    let (floor, stored) = {
        let runtime = net.runtime("laptop").unwrap();
        let (key, item) = (key.clone(), all.items.last().unwrap().key.clone());
        runtime
            .read_store(move |store| {
                let row = store.agent(&key).unwrap().unwrap();
                (
                    row.complete_from_order.unwrap(),
                    store.get(&key, &item).unwrap(),
                )
            })
            .await
            .unwrap()
    };
    assert!(
        stored.is_some(),
        "the old rows are still stored below the block"
    );
    assert!(stored.unwrap().order < floor);

    let sweep = net
        .runtime("laptop")
        .unwrap()
        .sweep_retention()
        .await
        .unwrap()
        .replicas
        .unwrap();
    assert!(!sweep.steps.is_empty(), "{sweep:?}");
    {
        let runtime = net.runtime("laptop").unwrap();
        let (key, item) = (key.clone(), all.items.last().unwrap().key.clone());
        let (orders, end, stale) = runtime
            .read_store(move |store| {
                let mut orders = Vec::new();
                let mut before = None;
                let end = loop {
                    let page = store.page(&key, before, 2).unwrap();
                    orders.extend(page.items.iter().map(|item| item.order));
                    before = page.items.last().map(|item| item.order);
                    if page.end != PageEnd::More {
                        break page.end;
                    }
                };
                (orders, end, store.get(&key, &item).unwrap())
            })
            .await
            .unwrap();
        assert_eq!(end, PageEnd::Boundary);
        assert_eq!(orders.len(), K as usize, "{orders:?}");
        assert!(
            orders.windows(2).all(|pair| pair[0] == pair[1] + 1),
            "paging down yields no gap: {orders:?}"
        );
        assert!(stale.is_none(), "the stale rows went first");
    }
    net.assert_block_invariant("laptop", "worker")
        .await
        .unwrap();
    // Below the trimmed block the origin still answers.
    let older = fetch(&net, "laptop", "worker", None, 100).await.unwrap();
    assert!(older.exhausted);
    assert_eq!(older.items.len(), origin_rows(&net, "worker").await.len());

    // The sweep is scheduled: one retention interval of policy time runs it.
    let runtime = net.runtime("laptop").unwrap();
    let mut retention = runtime.retention();
    let runs = retention.borrow().runs;
    net.advance(Duration::from_millis(
        (Launch::default().retention_interval_ms + 1) as u64,
    ))
    .unwrap();
    tokio::time::timeout(
        PATIENCE,
        retention.wait_for(|seen| seen.runs > runs && seen.replicas.is_some()),
    )
    .await
    .expect("the scheduled sweep runs")
    .unwrap();
    drop(retention);
    drop(runtime);
    net.assert_block_invariant("laptop", "worker")
        .await
        .unwrap();
    net.shutdown().await.unwrap();
}

/// A replica of a host that lost power holds rows the host no longer has,
/// under a cursor the host will mint again for other content. A source
/// resumes naming the generation its cursor was taken under, and the
/// rewound origin answers that with a fresh tail: every chat sees a Reset
/// before its next CaughtUp and never a row the origin lost, and the
/// replica's cursor ends up under the new generation.
#[tokio::test(flavor = "multi_thread")]
async fn a_rewound_origin_resets_every_source_that_resumes_under_its_old_generation() {
    const K: u32 = 6;
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(turns(4, 1))
            .prompt("go"),
    );
    let mut net = Net::start_with(topology, options(K, |_, _| {}))
        .await
        .unwrap();
    let key = net.agent("worker").unwrap().key();
    wait_origin_says(&net, "worker", "t0-0").await;
    net.current("laptop", "worker").await.unwrap();
    until("the journal read to its end", || async {
        let end = net.journal_end("worker").unwrap();
        let key = key.clone();
        let cursor = net
            .runtime("desk")
            .unwrap()
            .read_store(move |store| store.cursor(&key).unwrap())
            .await
            .unwrap();
        (cursor == end)
            .then_some(())
            .ok_or_else(|| format!("cursor {cursor} of {end}"))
    })
    .await
    .unwrap();
    let durable = net.journal_end("worker").unwrap();
    net.checkpoint_host("desk").await.unwrap();
    net.send("worker", "lost").await.unwrap();
    wait_origin_says(&net, "worker", "t1-0").await;
    net.current("laptop", "worker").await.unwrap();
    let mut chat = net.observe("laptop", "worker", 10).await.unwrap();
    chat.observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();
    let generation = net.generation("desk").unwrap();
    let before = {
        let key = key.clone();
        net.runtime("laptop")
            .unwrap()
            .read_store(move |store| store.agent(&key).unwrap().unwrap())
            .await
            .unwrap()
    };
    assert_eq!(before.source_generation, generation);

    // The laptop loses desk before desk comes back rewound.
    sever(&mut net).await;
    until("the laptop to detach desk's agents", || async {
        let state = replica_state(&net, "laptop", "worker").await;
        (state.is_some_and(|(_, marker)| marker == Some(Marker::Detached)))
            .then_some(())
            .ok_or_else(|| format!("replica (cursor, marker): {state:?}"))
    })
    .await
    .unwrap();
    net.rewind_host(
        "desk",
        &[JournalCut {
            agent: "worker".to_owned(),
            byte: durable,
        }],
    )
    .await
    .unwrap();
    assert_eq!(net.generation("desk").unwrap(), generation + 1);

    // A chat opened while desk is away is served as it stands, detached.
    let mut away = net.observe("laptop", "worker", 10).await.unwrap();
    away.observe_until(|events| marks(events).contains(&Mark::Detached), PATIENCE)
        .await
        .unwrap();
    restore(&mut net).await;
    for chat in [&mut away, &mut chat] {
        let events = chat
            .observe_until(
                |events| {
                    since_last_detached(events)
                        .iter()
                        .any(|m| matches!(m, Mark::CaughtUp(_)))
                },
                PATIENCE,
            )
            .await
            .unwrap();
        let since = since_last_detached(events);
        let reset = since.iter().position(|m| *m == Mark::Reset);
        let caught_up = since
            .iter()
            .position(|m| matches!(m, Mark::CaughtUp(_)))
            .unwrap();
        assert!(
            reset.is_some_and(|reset| reset < caught_up),
            "a Reset before the first CaughtUp after the break: {}",
            chat.transcript()
        );
        let from = events
            .iter()
            .rposition(|event| matches!(event.of, Some(session_event::Of::Detached(_))))
            .unwrap_or(0);
        assert!(
            !events[from..].iter().any(|event| matches!(
                &event.of,
                Some(session_event::Of::Item(item)) if item.text.contains("t1-0")
            )),
            "the chat never sees a row the origin lost again: {}",
            chat.transcript()
        );
    }
    net.current("laptop", "worker").await.unwrap();
    let laptop = net.runtime("laptop").unwrap();
    let (held, row, recorded) = {
        let key = key.clone();
        let desk_id = net.host("desk").unwrap().host_id;
        laptop
            .read_store(move |store| {
                (
                    store.cut(&key, u32::MAX).unwrap().held,
                    store.agent(&key).unwrap().unwrap(),
                    store.host_generation(desk_id.as_bytes()).unwrap(),
                )
            })
            .await
            .unwrap()
    };
    assert!(
        !held.iter().any(|item| item.text.contains("t1-0")),
        "a row the origin lost never returns: {held:#?}"
    );
    assert_eq!(row.source_generation, generation + 1);
    assert_eq!(recorded, Some(generation + 1));

    drop(laptop);
    net.shutdown().await.unwrap();
}

/// An exited agent's replica has settled: its source closed for good once
/// the exit landed. When its host comes back rewound the settled source
/// looks again, and the origin resets it like any other, so the rows the
/// origin lost go here too.
#[tokio::test(flavor = "multi_thread")]
async fn a_rewound_origin_resets_a_settled_replica_too() {
    let topology = desk_and_laptop().agent(
        AgentDecl::new("brief", "desk")
            .steps(turns(2, 1))
            .prompt("go"),
    );
    let mut net = Net::start_with(topology, options(6, |_, _| {}))
        .await
        .unwrap();
    let brief = net.agent("brief").unwrap().clone();
    let key = brief.key();
    wait_origin_says(&net, "brief", "t0-0").await;
    net.current("laptop", "brief").await.unwrap();
    let durable = net.journal_end("brief").unwrap();
    net.checkpoint_host("desk").await.unwrap();
    net.send("brief", "lost").await.unwrap();
    wait_origin_says(&net, "brief", "t1-0").await;
    net.runtime("desk").unwrap().stop(brief.id).await.unwrap();
    let laptop = net.runtime("laptop").unwrap();
    until("the laptop to settle brief exited", || async {
        let key = key.clone();
        let row = laptop
            .read_store(move |store| store.agent(&key).unwrap())
            .await
            .unwrap();
        let exited = row.is_some_and(|row| row.lifecycle == wire::Lifecycle::Exited as i32);
        let open = laptop.open_sources();
        (exited && open.is_empty())
            .then_some(())
            .ok_or_else(|| format!("exited {exited}, open sources {open:?}"))
    })
    .await
    .unwrap();
    let generation = net.generation("desk").unwrap();

    sever(&mut net).await;
    net.rewind_host(
        "desk",
        &[JournalCut {
            agent: "brief".to_owned(),
            byte: durable,
        }],
    )
    .await
    .unwrap();
    restore(&mut net).await;
    net.current("laptop", "brief").await.unwrap();
    let (held, row) = laptop
        .read_store(move |store| {
            (
                store.cut(&key, u32::MAX).unwrap().held,
                store.agent(&key).unwrap().unwrap(),
            )
        })
        .await
        .unwrap();
    assert!(
        !held.iter().any(|item| item.text.contains("t1-0")),
        "a row the origin lost never returns: {held:#?}"
    );
    assert_eq!(row.source_generation, generation + 1);
    assert_eq!(row.lifecycle, wire::Lifecycle::Exited as i32);
    drop(laptop);
    net.shutdown().await.unwrap();
}

/// A person on the laptop works with the blobs of the desk's agent as with
/// its own: an attachment stored from the laptop is written into the
/// agent's directory on the desk, a diff of its working tree is made on the
/// desk (its file list always, its patch when asked for), and the laptop reads each blob from the desk once and keeps the
/// bytes under the replica, so the next read needs no link.
#[tokio::test(flavor = "multi_thread")]
async fn blobs_and_diffs_of_a_peers_agent_are_made_at_its_origin_and_kept_by_the_reader() {
    use wire::agent_service_server::AgentService as _;
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(turns(1, 1))
            .prompt("go"),
    );
    let mut net = Net::start_with(topology, options(8, |_, _| {}))
        .await
        .unwrap();
    wait_origin_says(&net, "worker", "t0-0").await;
    net.current("laptop", "worker").await.unwrap();
    let worker = net.agent("worker").unwrap().clone();
    let agent_id = worker.id.as_bytes().to_vec();
    let own = net.host("desk").unwrap().data_dir.clone();

    let work = net.host("desk").unwrap().work.clone();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&work)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(work.join("deploy.sh"), "rsync build/ prod:/srv\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "first"]);
    std::fs::write(work.join("deploy.sh"), "rsync --delete build/ prod:/srv\n").unwrap();

    let laptop = net.client("laptop").unwrap();
    let stored = laptop
        .put_blob(tonic::Request::new(wire::PutBlobRequest {
            agent_id: agent_id.clone(),
            name: "notes.txt".to_owned(),
            mime: "text/plain".to_owned(),
            bytes: b"from the laptop".to_vec(),
        }))
        .await
        .expect("the desk stores the laptop's attachment")
        .into_inner();
    let hex = |hash: &[u8]| hash.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let on_desk = |hash: &[u8]| {
        walk(&own).into_iter().find(|path| {
            path.file_name()
                .is_some_and(|name| name == hex(hash).as_str())
        })
    };
    let written = on_desk(&stored.hash).expect("the attachment is in the desk's data");
    assert_eq!(std::fs::read(written).unwrap(), b"from the laptop");

    let diff = |with_patch| {
        let laptop = laptop.clone();
        let agent_id = agent_id.clone();
        async move {
            laptop
                .get_diff(tonic::Request::new(wire::GetDiffRequest {
                    agent_id,
                    base: Some(wire::DiffBase {
                        base: Some(wire::diff_base::Base::Head(wire::Empty {})),
                    }),
                    with_patch,
                }))
                .await
                .expect("the desk diffs its agent's working tree")
                .into_inner()
        }
    };
    let listed = diff(false).await;
    assert_eq!(listed.patch, None, "no patch unless asked");
    assert_eq!(
        listed
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.added, file.removed))
            .collect::<Vec<_>>(),
        [("deploy.sh", 1, 1)]
    );
    let diff = diff(true).await;
    assert_eq!(diff.files, listed.files);
    let patch = diff.patch.unwrap();
    assert!(
        on_desk(&patch.hash).is_some(),
        "the patch is the desk's blob"
    );

    let read = |hash: Vec<u8>| {
        let laptop = laptop.clone();
        let agent_id = agent_id.clone();
        async move {
            laptop
                .get_blob(tonic::Request::new(wire::GetBlobRequest { agent_id, hash }))
                .await
                .map(|response| response.into_inner().bytes)
        }
    };
    let bytes = read(patch.hash.clone())
        .await
        .expect("the laptop reads the patch");
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("+rsync --delete build/ prod:/srv"), "{text}");

    // Kept: with the desk away the laptop still has the patch it read, and
    // nothing it never read.
    sever(&mut net).await;
    assert_eq!(
        read(patch.hash.clone())
            .await
            .expect("kept under the replica"),
        text.as_bytes()
    );
    let unread = read(stored.hash.clone()).await;
    assert!(unread.is_err(), "{unread:?}");
    net.shutdown().await.unwrap();
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// A dump taken where another host's agent is only a replica holds what
/// this host has of it, its row and slice, and asks the agent's host for
/// nothing: that host's side is in its own dump. Planted secrets stay out of
/// every file.
#[tokio::test(flavor = "multi_thread")]
async fn a_dump_of_a_replica_holds_only_this_hosts_side() {
    const TOKEN: &str = "ghp_PLANTEDgathertoken0001abcdef";
    const KEY: &str = "sk-ant-api03-PLANTEDgatherkey0002";
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .steps(vec![text(&format!("pushed with {TOKEN}")), Step::TurnEnd])
            .prompt(&format!("deploy using {KEY}")),
    );
    let net = Net::start_with(topology, NetOptions::default())
        .await
        .unwrap();
    wait_origin_says(&net, "worker", "pushed with").await;
    net.current("laptop", "worker").await.unwrap();
    let worker = net.agent("worker").unwrap().clone();
    let request = wire::DumpRequest {
        reason: "gathered".to_owned(),
        ..wire::DumpRequest::default()
    };

    let bundle = net.runtime("laptop").unwrap().dump(request).await.unwrap();
    let files: Vec<String> = walk(&bundle)
        .iter()
        .map(|path| {
            path.strip_prefix(&bundle)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let has = |prefix: &str| files.iter().any(|file| file.starts_with(prefix));
    let at = format!("agents/{}", worker.id);
    assert!(has(&format!("{at}/row.pb")), "{files:#?}");
    assert!(has(&format!("{at}/store.pb")), "{files:#?}");
    for elsewhere in [
        format!("{at}/journal/"),
        format!("{at}/part/"),
        format!("{at}/host/"),
        "hosts/".to_owned(),
    ] {
        assert!(!has(&elsewhere), "{elsewhere} is the desk's: {files:#?}");
    }
    for path in walk(&bundle) {
        let bytes = std::fs::read(&path).unwrap();
        for secret in [TOKEN, KEY] {
            let hex = interpret::to_hex(secret.as_bytes());
            assert!(
                !bytes.windows(secret.len()).any(|w| w == secret.as_bytes())
                    && !bytes.windows(hex.len()).any(|w| w == hex.as_bytes()),
                "{secret} is in {}",
                path.display()
            );
        }
    }
    net.shutdown().await.unwrap();
}

/// The open ask in `host`'s held snapshot of headless Claude `agent`: its
/// key and the snapshot's phase.
async fn held_ask(net: &Net, host: &str, agent: &str) -> (Option<String>, Option<wire::Phase>) {
    let key = net.agent(agent).unwrap().key();
    let runtime = net.runtime(host).unwrap();
    let snapshot = runtime
        .read_store(move |store| store.cut(&key, 0).unwrap().snapshot)
        .await
        .unwrap();
    let Some(snapshot) = snapshot else {
        return (None, None);
    };
    let body = wire::ClaudeSdkSnapshot::decode(snapshot.body.as_slice()).unwrap();
    (
        body.asks.first().map(|ask| ask.key.clone()),
        wire::Phase::try_from(snapshot.phase).ok(),
    )
}

/// A daemon restarted while its agent waits on an open ask, as an update
/// restarts it: the agent process keeps the ask through the gap, the new
/// daemon reads it back from the journal, a peer holding the replica sees
/// the same ask open once it has caught up again, and an answer sent after
/// the restart reaches the same incarnation, which finishes its turn.
#[tokio::test(flavor = "multi_thread")]
async fn an_ask_open_across_a_daemon_restart_is_the_same_ask_and_is_answered_after_it() {
    let topology = desk_and_laptop().agent(
        AgentDecl::new("worker", "desk")
            .prompt("Run the suite you think best.")
            .steps(vec![
                Step::Ask(Ask::Question {
                    questions: vec![Question {
                        question: "Which suite should I run?".into(),
                        header: "Suite".into(),
                        options: vec!["unit".into(), "full".into()],
                        multi_select: false,
                        other: false,
                        secret: false,
                    }],
                }),
                text("running unit"),
                Step::TurnEnd,
            ]),
    );
    let mut net = Net::start(topology).await.unwrap();
    let worker = net.agent("worker").unwrap().clone();
    until("the laptop to hold worker's open ask", || async {
        let held = held_ask(&net, "laptop", "worker").await;
        held.0
            .is_some()
            .then_some(())
            .ok_or_else(|| format!("held ask: {held:?}"))
    })
    .await
    .unwrap();
    net.current("laptop", "worker").await.unwrap();
    let before = held_ask(&net, "laptop", "worker").await;
    assert_eq!(before.1, Some(wire::Phase::NeedsYou));

    net.kill_daemon("desk").await.unwrap();
    net.wait_link("desk", "laptop", false).await.unwrap();
    net.restart_daemon("desk").await.unwrap();
    net.wait_link("desk", "laptop", true).await.unwrap();
    net.current("laptop", "worker").await.unwrap();
    assert_eq!(
        held_ask(&net, "desk", "worker").await,
        before,
        "the new daemon reads the same open ask back"
    );
    assert_eq!(
        held_ask(&net, "laptop", "worker").await,
        before,
        "the replica shows the same open ask after the restart"
    );

    let answer = interpret::claude_sdk_input(
        uuid::Uuid::new_v4().as_bytes().to_vec(),
        &interpret::FixtureInput::Answer {
            ask: before.0.clone().unwrap(),
            answer: serde_json::json!({ "selected": [0] }),
        },
    )
    .unwrap();
    let verdict = net
        .client("laptop")
        .unwrap()
        .send_input(tonic::Request::new(wire::SendInputRequest {
            agent_id: worker.id.as_bytes().to_vec(),
            input: Some(answer),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(
        matches!(verdict.of, Some(wire::send_input_response::Of::Accepted(_))),
        "{verdict:?}"
    );
    wait_origin_says(&net, "worker", "running unit").await;
    net.current("laptop", "worker").await.unwrap();
    assert_eq!(held_ask(&net, "laptop", "worker").await.0, None);
    let row = net.runtime("desk").unwrap().agent(worker.id).await.unwrap();
    assert_eq!(
        row.incarnation, 1,
        "the same process answered, never restarted"
    );

    net.shutdown().await.unwrap();
}
