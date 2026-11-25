//! The app runtime against a real node in process, running an agent on a
//! fake provider: the host is woken once per turn, hears what moved as a
//! delta, reads rows by key and acts on the chat.

use std::sync::Arc;
use std::time::Duration;

use app_runtime::values::{
    ActOutcome, AgentAct, ChatDelta, Draft, DraftAttachment, DraftPart, KeysMoved, NewAgent,
    PageOutcome, ReviewChange, ReviewOutcome,
};
use app_runtime::{AppRuntime, Chat, Wake};
use client::SystemClock;
use provider_fakes::script::{Ask, Question, Step, Tool, ToolClass};
use testnet::{AgentDecl, FakeKind, Net, Topology};
use tokio::sync::mpsc;
use ui_state::{AgentKey, InputState, PhaseView, Refusal};
use ui_view::{
    AskBody, ChoiceOutcome, Comparison, QuestionResponse, ReviewBanner, RowKind, SettingChange,
};

const PATIENCE: Duration = Duration::from_secs(20);

fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

fn topology() -> Topology {
    let steps = vec![
        text("turn one"),
        Step::TurnEnd,
        Step::Ask(Ask::Permission(Tool {
            name: None,
            class: ToolClass::Consequential,
            input: None,
            outcome: Default::default(),
            wait_for: None,
        })),
        text("ran it"),
        Step::TurnEnd,
        Step::Ask(Ask::Question {
            questions: vec![
                Question {
                    question: "Which suite?".into(),
                    header: "Suite".into(),
                    options: vec!["unit".into(), "full".into()],
                    multi_select: false,
                    other: false,
                    secret: false,
                },
                Question {
                    question: "Which platforms?".into(),
                    header: "Platforms".into(),
                    options: vec!["mac".into(), "linux".into(), "windows".into()],
                    multi_select: true,
                    other: false,
                    secret: false,
                },
            ],
        }),
        text("answered"),
        Step::TurnEnd,
        text("turn four"),
        Step::TurnEnd,
    ];
    Topology::new().host("desk").agent(
        AgentDecl::new("worker", "desk")
            .kind(FakeKind::ClaudeSdk)
            .steps(steps)
            .prompt("go"),
    )
}

/// The host's side: wakes land on a channel, as a main thread's queue.
struct Host {
    wakes: mpsc::UnboundedReceiver<Wake>,
    /// Wakes taken off the queue while looking for a chat's, still owed.
    held: Vec<Wake>,
}

async fn open(net: &Net) -> (AppRuntime, Host) {
    let (runtime, host) = connect(net).await;
    runtime.fleet_opening();
    (runtime, host)
}

/// The runtime, with home not yet opened.
async fn connect(net: &Net) -> (AppRuntime, Host) {
    let (sender, wakes) = mpsc::unbounded_channel();
    let wake = Arc::new(move |wake: Wake| {
        let _ = sender.send(wake);
    });
    let local = net.host("desk").unwrap().host_id.as_bytes().to_vec();
    let node = client::Client::in_memory(net.entrance("desk").unwrap(), client::Surface::Ios);
    let runtime = AppRuntime::open(node, Arc::new(SystemClock), local, wake)
        .await
        .unwrap();
    (
        runtime,
        Host {
            wakes,
            held: Vec::new(),
        },
    )
}

impl Host {
    /// Waits for the next wake naming `wanted`.
    async fn next(&mut self, wanted: Wake) {
        if let Some(at) = self.held.iter().position(|wake| *wake == wanted) {
            self.held.remove(at);
            return;
        }
        tokio::time::timeout(PATIENCE, async {
            loop {
                if self.wakes.recv().await.expect("the runtime is open") == wanted {
                    return;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("never woken for {wanted:?}"))
    }

    /// One of the host's turns: the next wake naming `chat`, and what it
    /// brought, if anything drawn moved. Every wake for the chat already
    /// queued is answered by this take too. A second one can be there
    /// without the runtime waking twice for one batch: an update landing
    /// between the last turn's drain and its take sent a wake whose batch
    /// that take carried off, and the next update woke the host again. One
    /// wake per take is the coalescer's own promise, tested beside it.
    async fn turn(&mut self, chat: &Chat) -> Option<ChatDelta> {
        let wanted = Wake::Chat(chat.id());
        self.next(wanted).await;
        while let Ok(wake) = self.wakes.try_recv() {
            if wake != wanted {
                // Another wake's turn is still owed; keep it for its taker.
                self.held.push(wake);
            }
        }
        chat.delta()
    }

    /// Takes what is pending for `chat` on the wakes already queued for it,
    /// so no wake is left behind for a batch taken without one.
    fn settle(&mut self, chat: &Chat) {
        let wanted = Wake::Chat(chat.id());
        while let Ok(wake) = self.wakes.try_recv() {
            self.held.push(wake);
        }
        self.held.retain(|wake| *wake != wanted);
        chat.delta();
    }
}

/// Opens a chat as the host's model does: the opening hands it over whole.
async fn opened(runtime: &AppRuntime, agent: &AgentKey, tail: u32) -> Arc<Chat> {
    let chat = runtime.open_chat(agent, tail).await.unwrap();
    chat.opening();
    chat
}

fn worker(net: &Net) -> AgentKey {
    let agent = net.agent("worker").unwrap();
    AgentKey {
        host: agent.host_id.as_bytes().to_vec(),
        agent: agent.id.as_bytes().to_vec(),
    }
}

/// Takes changes on every wake until `check` holds over the held rows.
async fn until(host: &mut Host, chat: &Chat, what: &str, check: impl Fn(&Chat) -> bool) {
    let wake = Wake::Chat(chat.id());
    let found = tokio::time::timeout(PATIENCE, async {
        loop {
            if check(chat) {
                return;
            }
            host.next(wake).await;
            chat.delta();
        }
    })
    .await;
    if found.is_err() {
        let keys = chat.keys();
        let rows: Vec<String> = chat
            .rows_for(&keys)
            .iter()
            .map(|row| format!("{:?}", row.kind))
            .collect();
        panic!(
            "never saw {what}; caught up {}, rows: {rows:?}",
            chat.frame().caught_up
        );
    }
}

fn says(chat: &Chat, wanted: &str) -> bool {
    let keys = chat.keys();
    chat.rows_for(&keys)
        .iter()
        .any(|row| format!("{:?}", row.kind).contains(wanted))
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_wait_for_the_hosts_turn_and_rows_are_read_by_key() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let fleet = runtime.fleet_view();
    assert_eq!(fleet.sections.len(), 1, "{fleet:?}");
    let rows = &fleet.sections[0].rows;
    assert_eq!(rows.len(), 1, "{fleet:?}");
    assert_eq!(rows[0].card.name, "worker");
    let hosts = runtime.hosts();
    assert!(
        hosts.iter().any(|host| host.local && host.name == "desk"),
        "{hosts:?}"
    );

    let chat = opened(&runtime, &worker(&net), 50).await;
    // The whole first turn, its end and the idle that follows it: a prompt
    // sent before the turn ends queues behind it instead of being sent.
    until(&mut host, &chat, "the first turn to end", |chat| {
        let frame = chat.frame();
        frame.caught_up
            && matches!(frame.phase, PhaseView::Idle)
            && says(chat, "turn one")
            && says(chat, "TurnEnd")
    })
    .await;
    // What the turn left is handed over before the next one starts.
    host.settle(&chat);
    let before = chat.keys();

    let sent = chat.send(&Draft::words("please run it")).await;
    assert!(
        matches!(sent.state, InputState::Settled | InputState::Sent),
        "{sent:?}"
    );
    // Many updates land between the host's turns; each turn is one wake,
    // and no wake is owed until the host takes what the last one brought.
    // The ids only grow above the newest the host holds, and a delta's rows
    // are only of keys the host already held: a new one is read when drawn.
    // The ask is waited for as a delta brings it, not in the live state: an
    // ask landing just after a turn's take is live before any delta says so.
    let mut above: Vec<String> = Vec::new();
    let card = tokio::time::timeout(PATIENCE, async {
        loop {
            if let Some(delta) = host.turn(&chat).await {
                for row in &delta.rows {
                    assert!(
                        before.contains(&row.id) || above.contains(&row.id),
                        "a row for {} the host did not hold",
                        row.id
                    );
                }
                match delta.keys {
                    Some(KeysMoved::Edges {
                        above: new,
                        below,
                        dropped: 0,
                    }) if below.is_empty() => above.extend(new),
                    None => {}
                    other => panic!("the window moved other than above: {other:?}"),
                }
                if let Some(card) = delta.ask.and_then(|moved| moved.card) {
                    return card;
                }
            }
        }
    })
    .await
    .expect("the permission ask");
    assert!(!above.is_empty());
    assert_eq!(chat.keys(), [before.clone(), above.clone()].concat());
    let rows = chat.rows_for(&above);
    assert_eq!(
        rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>(),
        above
    );
    assert!(
        rows.iter()
            .any(|row| matches!(row.kind, RowKind::Prompt { .. }))
    );

    // Allow once, by the choice's position on the card.
    assert!(matches!(card.body, AskBody::Command { .. }), "{card:?}");
    let allow = card
        .choices
        .iter()
        .position(|choice| choice.outcome == ChoiceOutcome::AllowOnce)
        .expect("allow once is offered");
    assert_eq!(
        chat.answer_choice("some other ask", allow, "").await,
        ActOutcome::Rejected(Refusal::of(wire::Refusal::ClosedAsk))
    );
    assert_eq!(
        chat.answer_choice(&card.key, allow, "").await,
        ActOutcome::Done
    );
    until(&mut host, &chat, "the call to run", |chat| {
        chat.ask_card().is_none() && says(chat, "ran it")
    })
    .await;
    let decided = chat
        .rows_for(&chat.keys())
        .into_iter()
        .find(|row| row.decision.is_some())
        .expect("the decision lands as a row");
    assert_eq!(decided.id, card.item_key);

    // Questions: one pick per question, in order.
    chat.send(&Draft::words("ask me")).await;
    let card = tokio::time::timeout(PATIENCE, async {
        loop {
            if let Some(card) = chat.ask_card() {
                return card;
            }
            host.next(Wake::Chat(chat.id())).await;
            chat.delta();
        }
    })
    .await
    .expect("the question ask");
    let AskBody::Question(questions) = &card.body else {
        panic!("{card:?}");
    };
    assert_eq!(questions.len(), 2);
    assert!(questions[1].multi_select);
    let given = [vec![1], vec![0, 2]].map(|picked| QuestionResponse {
        picked,
        ..QuestionResponse::default()
    });
    assert_eq!(
        chat.answer_questions(&card.key, &given).await,
        ActOutcome::Done
    );
    until(&mut host, &chat, "the answers", |chat| {
        chat.ask_card().is_none() && says(chat, "answered")
    })
    .await;
    let rows = chat.rows_for(&chat.keys());
    let answered = format!("{rows:?}");
    assert!(answered.contains("full"), "{answered}");
    assert!(answered.contains("windows"), "{answered}");

    drop(chat);
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn older_rows_arrive_below_the_oldest_the_host_holds() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let chat = opened(&runtime, &worker(&net), 50).await;
    // The whole first turn, its end included: a row landing after the list
    // is taken would be paged in below but missing from it.
    until(&mut host, &chat, "the first turn to end", |chat| {
        chat.frame().caught_up && says(chat, "turn one") && says(chat, "TurnEnd")
    })
    .await;
    let everything = chat.keys();
    assert!(everything.len() >= 2, "{everything:?}");
    drop(chat);

    // A live agent's session stays with the fleet, holding its newest
    // rows: a chat opened again starts from them.
    let chat = opened(&runtime, &worker(&net), 1).await;
    assert_eq!(chat.keys(), everything);
    drop(chat);

    // An exited agent's session goes with its last chat; the next chat
    // opens a fresh one with its own tail.
    runtime
        .agent_act(&worker(&net), &AgentAct::Close)
        .await
        .unwrap();
    // The fleet's state takes the exit before the fleet drops the session,
    // and it says it changed once it has done both: read between the two,
    // the state says exited while the session is still held. So the wait is
    // for both, and a session that never goes fails it.
    let mut changed = runtime.fleet().changed();
    tokio::time::timeout(PATIENCE, async {
        loop {
            let exited = runtime
                .fleet()
                .state()
                .agent(&worker(&net))
                .is_some_and(|agent| agent.lifecycle() == wire::Lifecycle::Exited);
            if exited && runtime.fleet().session(&worker(&net)).is_none() {
                return;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("the worker exits and its session goes with it");
    let chat = opened(&runtime, &worker(&net), 50).await;
    until(&mut host, &chat, "the exit", |chat| chat.frame().caught_up).await;
    let everything = chat.keys();
    drop(chat);

    let chat = runtime.open_chat(&worker(&net), 1).await.unwrap();
    let Some(KeysMoved::Reload(held)) = chat.opening().keys else {
        panic!("an opening hands the window over whole");
    };
    assert_eq!(held, chat.keys());
    assert_eq!(held.len(), 1);
    assert!(chat.frame().has_older);
    let mut pages = 0;
    while chat.frame().has_older {
        assert!(matches!(chat.page_older(1).await, PageOutcome::Arrived(_)));
        pages += 1;
        assert!(pages < 20, "paging never reached the first row");
    }
    // Older rows arrive below the oldest the host holds, their rows read
    // when drawn.
    let delta = chat.delta().expect("the pages moved the window");
    let Some(KeysMoved::Edges {
        above,
        below,
        dropped: 0,
    }) = delta.keys
    else {
        panic!("{:?}", delta.keys);
    };
    assert!(above.is_empty());
    assert!(delta.rows.iter().all(|row| !below.contains(&row.id)));
    assert_eq!([below, held].concat(), everything);
    net.shutdown().await.unwrap();
}

/// A chat resolves its opening only once the snapshot and the rows asked
/// for are applied, so the host's first read is whole: here the chat of an
/// exited agent, whose session goes with each chat and is opened anew.
/// Opened again and again, every first read holds every row.
#[tokio::test(flavor = "multi_thread")]
async fn a_reopened_chat_of_an_exited_agent_holds_its_rows_at_once() {
    // A turn of many rows, so an opening is many answers.
    let steps = (0..60)
        .flat_map(|row| {
            let tool = Tool {
                name: None,
                class: ToolClass::Exploration,
                input: None,
                outcome: Default::default(),
                wait_for: None,
            };
            [text(&format!("row {row}")), Step::Tool(tool)]
        })
        .chain([Step::TurnEnd])
        .collect();
    let topology = Topology::new().host("desk").agent(
        AgentDecl::new("worker", "desk")
            .kind(FakeKind::ClaudeSdk)
            .steps(steps)
            .prompt("go"),
    );
    let net = Net::start(topology).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let chat = opened(&runtime, &worker(&net), 200).await;
    until(&mut host, &chat, "the turn to end", |chat| {
        chat.frame().caught_up && says(chat, "row 59") && says(chat, "TurnEnd")
    })
    .await;
    drop(chat);
    runtime
        .agent_act(&worker(&net), &AgentAct::Close)
        .await
        .unwrap();
    let mut changed = runtime.fleet().changed();
    tokio::time::timeout(PATIENCE, async {
        loop {
            let exited = runtime
                .fleet()
                .state()
                .agent(&worker(&net))
                .is_some_and(|agent| agent.lifecycle() == wire::Lifecycle::Exited);
            if exited {
                return;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("the worker exits");
    let chat = opened(&runtime, &worker(&net), 200).await;
    until(&mut host, &chat, "the exit", |chat| chat.frame().caught_up).await;
    let everything = chat.keys();
    assert!(everything.len() > 100, "{}", everything.len());
    drop(chat);

    for opening in 0..100 {
        assert!(runtime.fleet().session(&worker(&net)).is_none());
        let chat = opened(&runtime, &worker(&net), 200).await;
        assert_eq!(chat.keys(), everything, "opening {opening}");
        drop(chat);
    }
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_outlives_its_chat_with_this_devices_daemon() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, _host) = open(&net).await;
    let agent = worker(&net);
    let chat = opened(&runtime, &agent, 50).await;
    assert_eq!(chat.kept_draft(), None, "nothing was kept yet");
    chat.keep_draft(&Draft::words("first"));
    chat.keep_draft(&Draft::words("half typed"));
    drop(chat);

    // Reopened at once, the chat reads its draft after the saves before it.
    let chat = opened(&runtime, &agent, 50).await;
    assert_eq!(chat.kept_draft(), Some(Draft::words("half typed")));
    chat.keep_draft(&Draft::default());
    drop(chat);
    let chat = opened(&runtime, &agent, 50).await;
    assert_eq!(chat.kept_draft(), None, "an emptied draft is forgotten");
    net.shutdown().await.unwrap();
}

/// The phone's dump, which Export Diagnostics shares and a person's report
/// sends, is the one bundle: the fleet's part, the record of the chat on
/// screen, the core's and the agent's records, and none of what anybody
/// wrote (the agent's name, folder and branch, its prompts, a tool's file
/// name, input and output, and the replies) in any of its files.
#[tokio::test(flavor = "multi_thread")]
async fn the_phones_dump_carries_the_records_and_no_word_of_the_chat_on_screen() {
    const MARKER: &str = "pangolin-teapot-marker";
    let name = format!("worker-{MARKER}");
    let net = Net::start(
        Topology::new().host("desk").agent(
            AgentDecl::new(&name, "desk")
                .kind(FakeKind::ClaudeSdk)
                .cwd(&format!("work-{MARKER}"))
                .branch(&format!("{MARKER}-branch"))
                .steps(vec![
                    Step::Tool(Tool {
                        name: Some("Read".into()),
                        class: ToolClass::Exploration,
                        input: Some(serde_json::json!({
                            "file_path": format!("notes/{MARKER}.txt"),
                        })),
                        outcome: provider_fakes::script::Outcome {
                            output: format!("the file says {MARKER}"),
                            ..Default::default()
                        },
                        wait_for: None,
                    }),
                    text(&format!("turn one {MARKER}")),
                    Step::TurnEnd,
                ])
                .prompt(&format!("go, {MARKER}")),
        ),
    )
    .await
    .unwrap();
    let (runtime, mut host) = open(&net).await;
    let agent = net.agent(&name).unwrap();
    let key = AgentKey {
        host: agent.host_id.as_bytes().to_vec(),
        agent: agent.id.as_bytes().to_vec(),
    };
    let chat = opened(&runtime, &key, 50).await;
    until(&mut host, &chat, "the first turn", |chat| {
        says(chat, "turn one")
    })
    .await;
    let bundle = runtime.dump("report").await.unwrap();
    assert!(bundle.join("manifest.json").is_file());
    assert!(bundle.join("client/fleet/state.txt").is_file());
    assert!(
        bundle
            .join(format!("client/sessions/{}/events", agent.id))
            .is_file(),
        "the record of the chat on screen"
    );
    assert!(bundle.join(format!("agents/{}/record", agent.id)).is_file());
    assert!(bundle.join("core/checkpoint").is_file());
    let forms = [
        MARKER.as_bytes().to_vec(),
        MARKER.to_uppercase().into_bytes(),
        MARKER
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            .into_bytes(),
    ];
    let mut dirs = vec![bundle.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            for form in &forms {
                assert!(
                    !bytes.windows(form.len()).any(|window| window == form),
                    "{} holds what was written",
                    path.display()
                );
            }
        }
    }
    drop(chat);
    net.shutdown().await.unwrap();
}

/// The host's queue can answer a fleet wake while home is still being
/// built on the main thread, before it has opened: that answer is
/// nothing, but it still takes the changes that woke the host, or no
/// change after them would wake it again.
#[tokio::test(flavor = "multi_thread")]
async fn a_fleet_wake_answered_before_home_opens_leaves_later_ones_coming() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = connect(&net).await;
    host.next(Wake::Fleet).await;
    assert!(runtime.fleet_delta().is_none(), "home has not opened");
    let chat = opened(&runtime, &worker(&net), 50).await;
    chat.send(&Draft::words("please run it")).await;
    host.next(Wake::Fleet).await;
    assert!(runtime.fleet_delta().is_none(), "home has still not opened");
    runtime.fleet_opening();
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_fleet_wakes_the_host_when_an_agent_moves() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let chat = opened(&runtime, &worker(&net), 50).await;
    chat.send(&Draft::words("please run it")).await;
    tokio::time::timeout(PATIENCE, async {
        loop {
            host.next(Wake::Fleet).await;
            let moved = runtime.fleet_delta().and_then(|delta| delta.view);
            let needs_you = moved.is_some_and(|view| {
                view.sections
                    .iter()
                    .flat_map(|section| &section.rows)
                    .any(|row| {
                        row.card.agent == worker(&net)
                            && row.card.attention == ui_state::Attention::NeedsYou
                    })
            });
            if needs_you {
                return;
            }
        }
    })
    .await
    .expect("the card turns to needs-you");
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_settings_pick_reaches_the_agent_and_the_frame_shows_it() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let chat = opened(&runtime, &worker(&net), 50).await;
    // The host's catalogue is fetched once the snapshot names it, and can
    // land after the first turn does.
    until(
        &mut host,
        &chat,
        "the first turn and the catalogue",
        |chat| {
            chat.frame().caught_up
                && says(chat, "turn one")
                && chat.settings().models.iter().any(|model| !model.unlisted)
        },
    )
    .await;
    let view = chat.settings();
    assert!(
        view.models
            .iter()
            .any(|model| model.current && !model.unlisted),
        "the offered model is marked current: {view:?}"
    );
    assert_eq!(
        view.changeable.effort,
        ui_view::Changeable::Pick,
        "headless Claude takes an effort pick"
    );
    let plan = view
        .permissions
        .iter()
        .find(|permission| permission.value == "plan")
        .expect("plan is offered");
    assert!(!plan.current && plan.settable);
    let pick = SettingChange::Permission(plan.value.clone());
    assert_eq!(chat.change_setting(&pick).await, ActOutcome::Done);
    until(&mut host, &chat, "the plan permission", |chat| {
        chat.frame()
            .controls
            .permission
            .is_some_and(|permission| permission.value == "plan")
    })
    .await;
    assert!(chat.settings().permissions.iter().any(|permission| {
        permission.current && SettingChange::Permission(permission.value.clone()) == pick
    }));
    assert_eq!(
        chat.change_setting(&SettingChange::Effort("high".into()))
            .await,
        ActOutcome::Done
    );
    until(&mut host, &chat, "the high effort", |chat| {
        chat.settings()
            .efforts
            .iter()
            .any(|effort| effort.current && effort.value == "high")
    })
    .await;
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn opening_the_overview_lists_the_changed_files_by_folder() {
    let topology = Topology::new().host("desk").agent(
        AgentDecl::new("worker", "desk")
            .kind(FakeKind::ClaudeSdk)
            .cwd("project")
            .branch("main")
            .steps(vec![text("turn one"), Step::TurnEnd])
            .prompt("go"),
    );
    let net = Net::start(topology).await.unwrap();
    let folder = net.host("desk").unwrap().work.join("project");
    std::fs::create_dir_all(folder.join("notes")).unwrap();
    std::fs::write(folder.join("notes/plan.md"), "one\ntwo\n").unwrap();
    std::fs::write(folder.join("README.md"), "hello\n").unwrap();
    let (runtime, mut host) = open(&net).await;
    let chat = opened(&runtime, &worker(&net), 50).await;
    until(&mut host, &chat, "the first turn", |chat| {
        chat.frame().caught_up && says(chat, "turn one")
    })
    .await;
    assert_eq!(chat.overview().changes, None, "nothing fetched yet");
    let opened = chat.open_overview(Comparison::Uncommitted).await.unwrap();
    let changes = opened.changes.expect("the files were fetched");
    let folders: Vec<(&str, Vec<&str>)> = changes
        .folders
        .iter()
        .map(|folder| {
            let names = folder.files.iter().map(|file| file.name.as_str()).collect();
            (folder.path.as_str(), names)
        })
        .collect();
    assert_eq!(
        folders,
        vec![("", vec!["README.md"]), ("notes/", vec!["plan.md"])]
    );
    assert_eq!((changes.totals.files, changes.totals.added), (2, 3));
    assert_eq!(
        chat.overview().changes.map(|changes| changes.totals),
        Some(changes.totals),
        "the overview keeps what it fetched"
    );
    assert_eq!(
        changes.base, None,
        "the uncommitted work has no base branch"
    );
    let on_branch = chat
        .open_overview(Comparison::OnBranch)
        .await
        .unwrap()
        .changes
        .expect("the files were fetched");
    // On its base branch with no upstream, the host's rule counts the
    // uncommitted work alone, from HEAD, and reports no base branch.
    assert_eq!(on_branch.totals, changes.totals, "{on_branch:?}");
    assert_eq!(on_branch.base, None, "{on_branch:?}");
    until(&mut host, &chat, "the agent's branch", |chat| {
        chat.frame()
            .git
            .is_some_and(|git| git.branch.as_deref() == Some("main"))
    })
    .await;
    net.shutdown().await.unwrap();
}

/// Runs git in `dir` as a committer named amux.
fn git(dir: &std::path::Path, args: &[&str]) {
    let ran = std::process::Command::new("git")
        .args(["-c", "user.name=amux", "-c", "user.email=amux@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(ran.status.success(), "git {args:?}: {ran:?}");
}

/// The worker's folder on desk, on `main` with an upstream it was pushed
/// to (README.md) and one commit not pushed yet (plan.md).
fn main_ahead_of_its_upstream(net: &Net) {
    let work = net.host("desk").unwrap().work.clone();
    let folder = work.join("project");
    std::fs::write(folder.join("README.md"), "hello\n").unwrap();
    git(&folder, &["add", "README.md"]);
    git(&folder, &["commit", "--quiet", "-m", "pushed"]);
    git(&work, &["init", "--quiet", "--bare", "remote.git"]);
    git(&folder, &["remote", "add", "origin", "../remote.git"]);
    git(&folder, &["push", "--quiet", "-u", "origin", "main"]);
    std::fs::write(folder.join("plan.md"), "one\ntwo\n").unwrap();
    git(&folder, &["add", "plan.md"]);
    git(&folder, &["commit", "--quiet", "-m", "not pushed"]);
}

/// An agent working on its base branch, with commits not yet pushed to
/// the branch's upstream: the host counts the branch from the upstream,
/// and the overview lists the commits not pushed yet.
#[tokio::test(flavor = "multi_thread")]
async fn the_overview_lists_what_is_not_pushed_on_a_branch_with_an_upstream() {
    let topology = Topology::new().host("desk").agent(
        AgentDecl::new("worker", "desk")
            .kind(FakeKind::ClaudeSdk)
            .cwd("project")
            .branch("main")
            .steps(vec![text("turn one"), Step::TurnEnd])
            .prompt("go"),
    );
    let net = Net::start(topology).await.unwrap();
    main_ahead_of_its_upstream(&net);

    let (runtime, mut host) = open(&net).await;
    let chat = opened(&runtime, &worker(&net), 50).await;
    until(&mut host, &chat, "the first turn", |chat| {
        chat.frame().caught_up && says(chat, "turn one")
    })
    .await;
    let listed = chat
        .open_overview(Comparison::OnBranch)
        .await
        .unwrap()
        .changes
        .expect("the files were fetched");
    assert_eq!(listed.base.as_deref(), Some("origin/main"), "{listed:?}");
    let paths: Vec<&str> = listed
        .folders
        .iter()
        .flat_map(|folder| folder.files.iter().map(|file| file.path.as_str()))
        .collect();
    assert_eq!(paths, vec!["plan.md"]);
    net.shutdown().await.unwrap();
}

/// A delete's warning is the agent's host's preview: an agent in a folder
/// of its own takes no workspace with it, and one whose host cannot be
/// asked gets a warning that says so.
#[tokio::test(flavor = "multi_thread")]
async fn a_delete_warning_is_the_hosts_preview() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, _host) = open(&net).await;
    assert_eq!(
        runtime.delete_warning(&worker(&net)).await,
        None,
        "a folder of its own stays"
    );
    let elsewhere = AgentKey {
        host: vec![7; 16],
        agent: worker(&net).agent,
    };
    assert_eq!(
        runtime.delete_warning(&elsewhere).await,
        Some(ui_state::DeleteWarning::Unknown),
        "a host that cannot be asked"
    );
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_catches_up_after_the_app_returns_to_the_foreground() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let chat = opened(&runtime, &worker(&net), 50).await;
    until(&mut host, &chat, "the first turn", |chat| {
        chat.frame().caught_up && says(chat, "turn one")
    })
    .await;
    runtime.set_foreground(false);
    runtime.set_foreground(true);
    chat.send(&Draft::words("please run it")).await;
    until(&mut host, &chat, "the ask after the return", |chat| {
        chat.ask_card().is_some()
    })
    .await;
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_agent_starts_from_the_hosts_catalogue_in_a_worktree_of_its_own() {
    let topology = Topology::new().host("desk").agent(
        AgentDecl::new("worker", "desk")
            .kind(FakeKind::ClaudeSdk)
            .cwd("project")
            .branch("main")
            .steps(vec![text("turn one"), Step::TurnEnd])
            .prompt("go"),
    );
    let net = Net::start(topology).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let desk = net.host("desk").unwrap().host_id.as_bytes().to_vec();
    let catalogue = runtime.host_catalogue(&desk, "claude").await.unwrap();
    assert!(!catalogue.models.is_empty(), "{catalogue:?}");
    let plan = catalogue
        .permissions
        .iter()
        .find(|permission| permission.value == "plan")
        .expect("plan is offered");
    tokio::time::timeout(PATIENCE, async {
        loop {
            let signed_in = runtime.hosts().into_iter().any(|host| {
                host.host_id == desk
                    && host
                        .providers
                        .iter()
                        .any(|offer| offer.provider == "claude" && offer.signed_in == Some(true))
            });
            if signed_in {
                return;
            }
            host.next(Wake::Fleet).await;
            runtime.fleet_delta();
        }
    })
    .await
    .expect("the desk says Claude is signed in there");
    // A worktree branches from a commit.
    let folder = net.host("desk").unwrap().work.join("project");
    let committed = std::process::Command::new("git")
        .args(["-c", "user.name=amux", "-c", "user.email=amux@example.com"])
        .args(["commit", "--allow-empty", "--quiet", "-m", "start"])
        .current_dir(&folder)
        .status()
        .unwrap();
    assert!(committed.success());
    let created = runtime
        .create_agent(&NewAgent {
            host_id: desk,
            provider: "claude".into(),
            cwd: folder.display().to_string(),
            name: "fresh".into(),
            model: None,
            effort: None,
            permission: Some(plan.value.clone()),
            mode: None,
            new_worktree: true,
        })
        .await
        .unwrap();
    let card = tokio::time::timeout(PATIENCE, async {
        loop {
            if let Some(card) = runtime.fleet_card(&created) {
                return card;
            }
            host.next(Wake::Fleet).await;
            runtime.fleet_delta();
        }
    })
    .await
    .expect("the new agent reaches the fleet");
    assert_eq!(card.name, "fresh");
    assert_ne!(
        std::path::Path::new(&card.cwd),
        folder,
        "it works in a worktree of its own"
    );
    net.shutdown().await.unwrap();
}

/// An agent program the host offers beyond Claude and Codex, here the fake
/// ACP agent every test host is configured with: the host's entry lists it
/// by the name its config gives, with what its agents can do; a new agent
/// of it starts as an ACP agent by its id alone, on its own defaults; and
/// its chat names it as the host does.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_the_host_offers_by_id_starts_and_is_named_as_the_host_names_it() {
    let script = provider_fakes::script::Script {
        steps: vec![text("hello from the fake agent"), Step::TurnEnd],
        acp: provider_fakes::script::AcpAgent {
            name: testnet::FAKE_ACP_AGENT.into(),
            resume: true,
            image: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let net = Net::start(Topology::new().host_decl(testnet::HostDecl {
        name: "desk".into(),
        script: Some(script),
        ..testnet::HostDecl::default()
    }))
    .await
    .unwrap();
    let (runtime, mut host) = open(&net).await;
    let desk = net.host("desk").unwrap().host_id.as_bytes().to_vec();
    let offered = tokio::time::timeout(PATIENCE, async {
        loop {
            let listed = runtime
                .hosts()
                .into_iter()
                .filter(|host| host.host_id == desk)
                .flat_map(|host| host.providers)
                .find(|offer| offer.provider == testnet::FAKE_ACP_AGENT);
            if let Some(listed) = listed {
                return listed;
            }
            host.next(Wake::Fleet).await;
            runtime.fleet_delta();
        }
    })
    .await
    .expect("the desk lists the fake agent");
    assert_eq!(offered.name, "Fake ACP Agent");
    assert!(offered.capabilities.resumable, "{offered:?}");
    assert!(!offered.capabilities.attachable, "{offered:?}");
    let ids: Vec<_> = runtime
        .hosts()
        .into_iter()
        .filter(|host| host.host_id == desk)
        .flat_map(|host| host.providers)
        .map(|offer| offer.provider)
        .collect();
    assert_eq!(ids, ["claude", "codex", testnet::FAKE_ACP_AGENT]);

    let created = runtime
        .create_agent(&NewAgent {
            host_id: desk.clone(),
            provider: testnet::FAKE_ACP_AGENT.into(),
            cwd: net.host("desk").unwrap().work.display().to_string(),
            name: "helper".into(),
            model: None,
            effort: None,
            permission: None,
            mode: None,
            new_worktree: false,
        })
        .await
        .expect("the desk starts the fake agent");
    let card = tokio::time::timeout(PATIENCE, async {
        loop {
            if let Some(card) = runtime.fleet_card(&created) {
                return card;
            }
            host.next(Wake::Fleet).await;
            runtime.fleet_delta();
        }
    })
    .await
    .expect("the new agent reaches the fleet");
    assert_eq!(card.kind, wire::Kind::Acp);
    let chat = opened(&runtime, &created, 50).await;
    until(&mut host, &chat, "the agent named and ready", |chat| {
        let frame = chat.frame();
        frame.caught_up && matches!(frame.phase, PhaseView::Idle)
    })
    .await;
    let frame = chat.frame();
    assert_eq!(frame.provider.provider, testnet::FAKE_ACP_AGENT);
    assert_eq!(frame.provider.name, "Fake ACP Agent");
    assert!(frame.capabilities.resumable, "{:?}", frame.capabilities);
    assert!(!frame.capabilities.attachable, "{:?}", frame.capabilities);

    // What it does not take is refused before anything is stored, in words
    // naming the file and the agent, as is anything over the hosts' limit.
    assert!(!frame.capabilities.images, "{:?}", frame.capabilities);
    assert_eq!(
        chat.attachment_refusal("shot.png", "image/png", 16)
            .as_deref(),
        Some("shot.png is not attached: Fake ACP Agent can’t take images.")
    );
    let over = wire::ATTACHMENT_SIZE_LIMIT as u64 + 1;
    assert_eq!(
        chat.attachment_refusal("huge.log", "text/plain", over)
            .as_deref(),
        Some("huge.log is not attached because it is over the 64 MB limit for attachments.")
    );
    assert!(frame.capabilities.files, "{:?}", frame.capabilities);
    assert_eq!(chat.attachment_refusal("notes.txt", "text/plain", 16), None);

    // A prompt carrying a picture all the same is refused at once, and says
    // why in the same words, so the draft that comes back says so.
    let picture = Draft {
        parts: vec![
            DraftPart::Text("What is this?".into()),
            DraftPart::Attachment(DraftAttachment::Image(wire::BlobRef {
                hash: vec![7; 32],
                name: "shot.png".into(),
                mime: "image/png".into(),
                size: 16,
            })),
        ],
    };
    let sent = chat.send(&picture).await;
    let cannot = Refusal {
        reason: wire::Refusal::Unsupported,
        detail: Some("Fake ACP Agent can’t take images".into()),
    };
    assert_eq!(sent.state, InputState::Rejected(cannot.clone()));
    let refused = chat.frame().refused;
    assert!(
        refused
            .iter()
            .any(|refused| refused.input_id == sent.input_id && refused.reason == cannot),
        "{refused:?}"
    );
    chat.discard(&sent.input_id);

    // A program the host does not offer is refused before anything is
    // asked of the host.
    let refused = runtime
        .create_agent(&NewAgent {
            host_id: desk,
            provider: "nobody".into(),
            cwd: "/".into(),
            name: String::new(),
            model: None,
            effort: None,
            permission: None,
            mode: None,
            new_worktree: false,
        })
        .await;
    assert!(refused.is_err(), "{refused:?}");
    net.shutdown().await.unwrap();
}

/// Two subagents the agent starts together: the chat's two calls fold into
/// one row, and how the subagents stand reaches the host from the fleet,
/// as running and then finished, though no row changes when it does. The
/// subagents hold on a gate, and the turn starts once the chat is open, so
/// the chat hands over running before they can finish.
#[tokio::test(flavor = "multi_thread")]
async fn a_chat_hands_over_how_the_subagents_its_rows_open_stand() {
    use provider_fakes::script::Subagent;
    let subagent = |task: &str| Subagent {
        task: task.into(),
        nickname: None,
        await_read: false,
        background: false,
        steps: vec![
            Step::WaitFor {
                path: "report".into(),
            },
            text(&task.to_uppercase()),
        ],
    };
    let net = Net::start(
        Topology::new().host("desk").agent(
            AgentDecl::new("worker", "desk")
                .kind(FakeKind::ClaudeSdk)
                .steps(vec![
                    Step::Subagents {
                        agents: vec![subagent("red"), subagent("blue")],
                    },
                    text("both reported"),
                    Step::TurnEnd,
                ]),
        ),
    )
    .await
    .unwrap();
    let (runtime, mut host) = open(&net).await;
    let chat = opened(&runtime, &worker(&net), 50).await;
    net.send("worker", "delegate").await.unwrap();
    let wake = Wake::Chat(chat.id());
    // The group's state from each delta that carries one, until `done`.
    let mut until_group = async |what: &str, done: &dyn Fn(&ui_view::GroupState) -> bool| {
        tokio::time::timeout(PATIENCE, async {
            loop {
                host.next(wake).await;
                let Some(agents) = chat.delta().and_then(|delta| delta.agents) else {
                    continue;
                };
                let Some(group) = agents.groups.first() else {
                    continue;
                };
                assert_eq!(group.state.agents, 2, "{agents:?}");
                if done(&group.state) {
                    return;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the group never read {what}"));
    };
    until_group("running", &|state| state.running > 0).await;
    net.open_gate("report").unwrap();
    until_group("finished", &|state| state.finished == 2).await;
    let calls: Vec<_> = chat
        .rows_for(&chat.keys())
        .into_iter()
        .filter(|row| row.opens.is_some())
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls.iter().map(|row| row.collapsed).collect::<Vec<_>>(),
        vec![false, true]
    );
    // Each call's agent is a fleet row under the worker, which a tap opens.
    for call in &calls {
        let card = runtime
            .fleet_card(&chat.agents().agents[0].agent)
            .expect("the subagent is in the fleet");
        assert!(card.native);
        assert!(call.group.as_ref().is_some_and(|group| !group.open));
    }
    chat.toggle_group(&calls[0].id);
    host.next(wake).await;
    let delta = chat.delta().expect("opening the group moves its rows");
    assert!(
        delta
            .rows
            .iter()
            .filter(|row| row.opens.is_some())
            .all(|row| !row.collapsed && row.group.as_ref().is_some_and(|group| group.open)),
        "{:?}",
        delta.rows
    );
    net.shutdown().await.unwrap();
}

/// The review page through a chat, on a host that does not offer reviews:
/// the page comes with the chat's deltas and says so, and neither an edit
/// nor a send with the review reaches the agent.
#[tokio::test(flavor = "multi_thread")]
async fn a_review_page_on_a_host_without_reviews_says_so_and_sends_nothing() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let chat = opened(&runtime, &worker(&net), 50).await;
    until(&mut host, &chat, "the first turn to end", |chat| {
        chat.frame().caught_up && says(chat, "TurnEnd")
    })
    .await;
    host.settle(&chat);
    assert_eq!(chat.frame().review_chip, None, "the row has no review");

    assert_eq!(
        chat.open_review(None, false).await,
        ReviewOutcome::Unavailable
    );
    let page = tokio::time::timeout(PATIENCE, async {
        loop {
            if let Some(page) = host.turn(&chat).await.and_then(|delta| delta.review) {
                return page;
            }
        }
    })
    .await
    .expect("a delta carries the page");
    assert_eq!(page.banners, vec![ReviewBanner::NotOffered], "{page:?}");
    assert!(page.header.is_none() && page.rows.is_empty() && !page.opening);
    let open = chat.review_page().expect("the page is open");
    assert_eq!(open.anchor(0, 0), None, "no row to comment on");
    assert_eq!(
        chat.edit_review(vec![ReviewChange::DiscardDrafts]).await,
        ReviewOutcome::Unavailable
    );

    let keys = chat.keys();
    let refused = chat.send_with_review(&Draft::words("see the review")).await;
    assert!(refused.is_err(), "{refused:?}");
    assert!(chat.frame().underway.is_empty(), "nothing was sent");
    assert_eq!(chat.keys(), keys);

    chat.close_review();
    assert!(chat.review_page().is_none());
    assert!(
        chat.opening().review.is_none(),
        "a closed page is not handed over"
    );
    drop(open);
    drop(chat);
    net.shutdown().await.unwrap();
}
