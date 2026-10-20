//! The app runtime against a real node in process, running an agent on a
//! fake provider: the host is woken once per turn, takes the changed keys,
//! reads rows by key and acts on the chat.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use app_runtime::values::{
    ActOutcome, AgentAct, ChatChanges, Draft, NewAgent, PageOutcome, RowOptions,
};
use app_runtime::{AppRuntime, Chat, Wake};
use client::{Client, InProcess, SystemClock};
use model::{AgentKey, InputState, PhaseView, Refusal, RefusalReason};
use provider_fakes::script::{Ask, Question, Step, Tool, ToolClass};
use testnet::{AgentDecl, FakeKind, Net, Topology};
use tokio::sync::mpsc;
use ui_view::{AskBody, ChoiceOutcome, Comparison, QuestionResponse, RowKind, SettingChange};

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
    let client: Arc<dyn Client> = Arc::new(InProcess::new(net.client("desk").unwrap()));
    let (sender, wakes) = mpsc::unbounded_channel();
    let wake = Arc::new(move |wake: Wake| {
        let _ = sender.send(wake);
    });
    let local = net.host("desk").unwrap().host_id.as_bytes().to_vec();
    let runtime = AppRuntime::open(client, Arc::new(SystemClock), local, wake)
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
    /// brought. The wake was owed something, and nothing else was queued
    /// for the chat before this take: however many updates land between
    /// turns, the host is woken once and takes them all together.
    async fn turn(&mut self, chat: &Chat) -> ChatChanges {
        let wanted = Wake::Chat(chat.id());
        self.next(wanted).await;
        let mut queued = Vec::new();
        while let Ok(wake) = self.wakes.try_recv() {
            queued.push(wake);
        }
        assert!(
            !queued.contains(&wanted),
            "a second wake for the chat before its take: {queued:?}"
        );
        for wake in queued {
            // Another wake's turn is still owed; keep it for its taker.
            self.held.push(wake);
        }
        let changes = chat.take_changes();
        assert!(
            changes.session || changes.reloaded || !changes.keys.is_empty(),
            "a wake that brought nothing"
        );
        changes
    }
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
            chat.take_changes();
        }
    })
    .await;
    if found.is_err() {
        let keys = chat.keys();
        let rows: Vec<String> = chat
            .rows_for(&keys, &RowOptions::default())
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
    chat.rows_for(&keys, &RowOptions::default())
        .iter()
        .any(|row| format!("{:?}", row.kind).contains(wanted))
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_wait_for_the_hosts_turn_and_rows_are_read_by_key() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let fleet = runtime.fleet_view(&[]);
    assert_eq!(fleet.sections.len(), 1, "{fleet:?}");
    let rows = &fleet.sections[0].rows;
    assert_eq!(rows.len(), 1, "{fleet:?}");
    assert_eq!(rows[0].card.name, "worker");
    let hosts = runtime.hosts();
    assert!(
        hosts.iter().any(|host| host.local && host.name == "desk"),
        "{hosts:?}"
    );

    let chat = runtime.open_chat(&worker(&net), 50).await.unwrap();
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
    let before = chat.keys();
    let newest = before.last().unwrap().clone();
    assert_eq!(chat.keys_above(&newest), Some(Vec::new()));
    assert_eq!(chat.keys_above("no such key"), None);

    let sent = chat.send(&Draft::words("please run it")).await;
    assert!(
        matches!(sent.state, InputState::Settled | InputState::Sent),
        "{sent:?}"
    );
    // Many updates land between the host's turns; each turn is one wake,
    // and no wake is owed until the host takes what the last one brought.
    let mut session_moved = false;
    let card = tokio::time::timeout(PATIENCE, async {
        loop {
            session_moved |= host.turn(&chat).await.session;
            if let Some(card) = chat.ask_card() {
                return card;
            }
        }
    })
    .await
    .expect("the permission ask");
    assert!(session_moved, "the ask moved the frame");

    // The ids only grew above the newest the host held.
    let above = chat.keys_above(&newest).unwrap();
    assert!(!above.is_empty());
    assert_eq!(chat.keys(), [before.clone(), above.clone()].concat());
    let rows = chat.rows_for(&above, &RowOptions::default());
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
        ActOutcome::Rejected(Refusal::of(RefusalReason::ClosedAsk))
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
        .rows_for(&chat.keys(), &RowOptions::default())
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
            chat.take_changes();
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
    let rows = chat.rows_for(&chat.keys(), &RowOptions::default());
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
    let chat = runtime.open_chat(&worker(&net), 50).await.unwrap();
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
    let chat = runtime.open_chat(&worker(&net), 1).await.unwrap();
    assert_eq!(chat.keys(), everything);
    drop(chat);

    // An exited agent's session goes with its last chat; the next chat
    // opens a fresh one with its own tail.
    runtime
        .agent_act(&worker(&net), &AgentAct::Stop)
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
    assert!(runtime.fleet().session(&worker(&net)).is_none());
    let chat = runtime.open_chat(&worker(&net), 50).await.unwrap();
    until(&mut host, &chat, "the exit", |chat| chat.frame().caught_up).await;
    let everything = chat.keys();
    drop(chat);

    let chat = runtime.open_chat(&worker(&net), 1).await.unwrap();
    let held = chat.keys();
    assert_eq!(held.len(), 1);
    assert!(chat.frame().has_older);
    assert_eq!(chat.keys_below(&held[0]), Some(Vec::new()));
    let mut pages = 0;
    while chat.frame().has_older {
        assert!(matches!(chat.page_older(1).await, PageOutcome::Arrived(_)));
        pages += 1;
        assert!(pages < 20, "paging never reached the first row");
    }
    let below = chat.keys_below(&held[0]).unwrap();
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
    let chat = runtime.open_chat(&worker(&net), 200).await.unwrap();
    until(&mut host, &chat, "the turn to end", |chat| {
        chat.frame().caught_up && says(chat, "row 59") && says(chat, "TurnEnd")
    })
    .await;
    drop(chat);
    runtime
        .agent_act(&worker(&net), &AgentAct::Stop)
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
    let chat = runtime.open_chat(&worker(&net), 200).await.unwrap();
    until(&mut host, &chat, "the exit", |chat| chat.frame().caught_up).await;
    let everything = chat.keys();
    assert!(everything.len() > 100, "{}", everything.len());
    drop(chat);

    for opening in 0..100 {
        assert!(runtime.fleet().session(&worker(&net)).is_none());
        let chat = runtime.open_chat(&worker(&net), 200).await.unwrap();
        assert_eq!(chat.keys(), everything, "opening {opening}");
        drop(chat);
    }
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dump_carries_the_fleet_and_every_open_chat() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let chat = runtime.open_chat(&worker(&net), 50).await.unwrap();
    until(&mut host, &chat, "the first turn", |chat| {
        says(chat, "turn one")
    })
    .await;
    let bundle = runtime.dump("asked for in a test").await.unwrap();
    assert!(bundle.join("manifest.json").is_file());
    assert!(bundle.join("client/fleet/state.txt").is_file());
    let sessions = std::fs::read_dir(bundle.join("client/sessions"))
        .expect("the open chat's part")
        .count();
    assert_eq!(sessions, 1);
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_fleet_wakes_the_host_when_an_agent_moves() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    runtime.take_fleet_changes();
    let chat = runtime.open_chat(&worker(&net), 50).await.unwrap();
    chat.send(&Draft::words("please run it")).await;
    tokio::time::timeout(PATIENCE, async {
        loop {
            host.next(Wake::Fleet).await;
            let changes = runtime.take_fleet_changes();
            if changes.agents.contains(&worker(&net))
                && runtime.fleet_card(&worker(&net)).unwrap().attention
                    == model::Attention::NeedsYou
            {
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
    let chat = runtime.open_chat(&worker(&net), 50).await.unwrap();
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
    let chat = runtime.open_chat(&worker(&net), 50).await.unwrap();
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
    until(&mut host, &chat, "the agent's branch", |chat| {
        chat.frame()
            .git
            .is_some_and(|git| git.branch.as_deref() == Some("main"))
    })
    .await;
    let review = chat.review(Comparison::Uncommitted).await.unwrap();
    assert!(review.patch.contains("README.md"), "{}", review.patch);
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_catches_up_after_the_app_returns_to_the_foreground() {
    let net = Net::start(topology()).await.unwrap();
    let (runtime, mut host) = open(&net).await;
    let chat = runtime.open_chat(&worker(&net), 50).await.unwrap();
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
    let catalogue = runtime
        .host_catalogue(&desk, wire::Provider::Claude)
        .await
        .unwrap();
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
                        .any(|offer| offer.provider == wire::Provider::Claude && offer.signed_in)
            });
            if signed_in {
                return;
            }
            host.next(Wake::Fleet).await;
            runtime.take_fleet_changes();
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
            kind: wire::Kind::ClaudeSdk,
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
            runtime.take_fleet_changes();
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
