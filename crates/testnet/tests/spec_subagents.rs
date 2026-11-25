//! Provider subagents across daemons and clients: the agents Claude's Agent
//! tool and Codex's child threads start inside an agent on one host, read,
//! answered and stopped from clients of that host and of a paired one.
//!
//! The clients are the phone's own runtime (`app-runtime`), each on one
//! host's node called in memory, as the phone calls its own: its fleet, the
//! chats it opens and the rows and cards those draw. A client of the laptop
//! reads the desk's agents through the laptop's replicas and addresses
//! every answer and stop to the desk, which the laptop carries there
//! unread for the desk to answer as a person's on the laptop.
//!
//! Each story reads top to bottom as the promises it holds, each printed as
//! it is shown, so a run's output is the evidence.

use std::sync::Arc;

use app_runtime::{AppRuntime, Chat, DEFAULT_TAIL, Wake};
use client::SystemClock;
use provider_fakes::script::{Ask, Step, Subagent, Tool, ToolClass};
use testnet::{AgentDecl, FakeKind, HostDecl, Net, PATIENCE, Topology};
use tokio::sync::mpsc;
use ui_state::{AgentKey, Attention, Composer, JobStop};
use ui_view::{ChoiceOutcome, Row, RowKind, SectionKind};
use wire::{ExitCause, Lifecycle};

/// How many steps each subagent plays before and after what the story
/// needs of it: together their rows are more than a client's window holds,
/// as four parallel subagents' steps were when they blanked their parent's
/// history.
const STEPS: usize = 30;

fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

fn explore() -> Step {
    Step::Tool(tool(ToolClass::Exploration))
}

fn tool(class: ToolClass) -> Tool {
    Tool {
        name: None,
        class,
        input: None,
        outcome: Default::default(),
        wait_for: None,
    }
}

/// `n` of a subagent's steps, each a word and a call: "alpha step 3".
fn steps(name: &str, from: usize, n: usize) -> Vec<Step> {
    (from..from + n)
        .flat_map(|at| [text(&format!("{name} step {at}")), explore()])
        .collect()
}

/// The task a subagent is given, which its provider makes its title.
fn task(name: &str) -> String {
    format!("Check {name}.")
}

/// A subagent named for its task. Codex gives a child the nickname amux
/// reads as its title once the child's thread is read, so its turn waits
/// for that read, as Codex answers it before the child's first word.
fn subagent(kind: FakeKind, name: &str, steps: Vec<Step>) -> Subagent {
    Subagent {
        task: task(name),
        nickname: Some(task(name)),
        await_read: kind == FakeKind::Codex,
        background: false,
        steps,
    }
}

/// The lead's script: two long turns of its own, then one that starts four
/// subagents together and carries on once they have finished or stopped.
/// `bravo` asks for a command mid-way, and `delta` waits on a gate no one
/// opens until it is stopped. The fake plays the subagents' steps in turn,
/// one each, so an ask or a wait holds them all until it is over.
fn lead_steps(kind: FakeKind) -> Vec<Step> {
    let mut bravo = steps("bravo", 1, 10);
    bravo.push(Step::Ask(Ask::Permission(tool(ToolClass::Consequential))));
    bravo.extend(steps("bravo", 11, STEPS - 10));
    let mut delta = steps("delta", 1, 15);
    delta.push(text("delta waits on the long build"));
    delta.push(Step::WaitFor {
        path: "delta-never".into(),
    });
    delta.extend(steps("delta", 16, STEPS - 15));
    vec![
        Step::Repeat {
            times: 40,
            steps: vec![text("Early turn, finding {pass}."), explore()],
            played: 0,
        },
        text("The first survey is done."),
        Step::TurnEnd,
        Step::Repeat {
            times: 20,
            steps: vec![text("Second turn, note {pass}."), explore()],
            played: 0,
        },
        Step::TurnEnd,
        Step::Subagents {
            agents: vec![
                subagent(kind, "alpha", steps("alpha", 1, STEPS)),
                subagent(kind, "bravo", bravo),
                subagent(kind, "charlie", steps("charlie", 1, STEPS)),
                subagent(kind, "delta", delta),
            ],
        },
        text("All four have reported."),
        Step::TurnEnd,
    ]
}

/// One client: the phone's runtime over one host's daemon, the chats it
/// has open, and the wakes it hands its host.
struct Client {
    name: &'static str,
    runtime: AppRuntime,
    wakes: mpsc::UnboundedReceiver<Wake>,
    chats: Vec<Arc<Chat>>,
}

impl Client {
    /// Opens the runtime on `host`'s daemon, on home.
    async fn connect(net: &Net, host: &str, name: &'static str) -> Client {
        let (sender, wakes) = mpsc::unbounded_channel();
        let wake = Arc::new(move |wake: Wake| {
            let _ = sender.send(wake);
        });
        let local = net.host(host).unwrap().host_id.as_bytes().to_vec();
        let node = client::Client::in_memory(net.entrance(host).unwrap(), client::Surface::Ios);
        let runtime = AppRuntime::open(node, Arc::new(SystemClock), local, wake)
            .await
            .unwrap();
        runtime.fleet_opening();
        Client {
            name,
            runtime,
            wakes,
            chats: Vec::new(),
        }
    }

    /// Opens `agent`'s chat as the phone's model does, with the window a
    /// chat opens with, once its fleet lists the agent: a person opens a
    /// chat from a row they see.
    async fn open(&mut self, agent: &AgentKey) -> Arc<Chat> {
        let id = agent.agent.clone();
        self.until("the agent's row in the fleet", |runtime| {
            self::agent(runtime, &id)
                .map(|_| ())
                .ok_or_else(|| "not listed".to_owned())
        })
        .await;
        let chat = self.runtime.open_chat(agent, DEFAULT_TAIL).await.unwrap();
        chat.opening();
        self.chats.push(chat.clone());
        chat
    }

    /// Waits until `check` passes over what this client holds, taking every
    /// change on each wake as the phone's host does, so each change wakes it
    /// again. Fails with what `check` last said once `PATIENCE` is out.
    async fn until<T>(
        &mut self,
        what: &str,
        mut check: impl FnMut(&AppRuntime) -> Result<T, String>,
    ) -> T {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            self.runtime.fleet_delta();
            for chat in &self.chats {
                chat.delta();
            }
            let last = match check(&self.runtime) {
                Ok(value) => return value,
                Err(last) => last,
            };
            if tokio::time::timeout_at(deadline, self.wakes.recv())
                .await
                .is_err()
            {
                panic!("{}: never saw {what}; last: {last}", self.name);
            }
        }
    }
}

/// The fleet's row for the agent `id`, as `wire` carries it.
fn agent(runtime: &AppRuntime, id: &[u8]) -> Option<wire::Agent> {
    runtime.fleet().state().find(id).cloned()
}

/// The provider subagents the fleet lists under `parent`, by title.
fn subagents(runtime: &AppRuntime, parent: &[u8]) -> Vec<wire::Agent> {
    let fleet = runtime.fleet().state();
    let mut found: Vec<wire::Agent> = fleet
        .agents()
        .filter(|agent| agent.native.is_some())
        .filter(|agent| {
            agent
                .parent
                .as_ref()
                .is_some_and(|at| at.agent_id == parent)
        })
        .cloned()
        .collect();
    found.sort_by(|a, b| a.title().cmp(&b.title()));
    found
}

fn key_of(agent: &wire::Agent) -> AgentKey {
    ui_state::agent_key(agent)
}

fn lead_key(net: &Net) -> AgentKey {
    let lead = net.agent("lead").unwrap();
    AgentKey {
        host: lead.host_id.as_bytes().to_vec(),
        agent: lead.id.as_bytes().to_vec(),
    }
}

fn rows(chat: &Chat) -> Vec<Row> {
    chat.rows_for(&chat.keys())
}

fn shown(row: &Row) -> String {
    format!("{:?}", row.kind)
}

/// Whether any row the chat holds says `wanted`.
fn says(chat: &Chat, wanted: &str) -> bool {
    rows(chat).iter().any(|row| shown(row).contains(wanted))
}

/// The rows that open an agent: the calls that started subagents.
fn calls(chat: &Chat) -> Vec<Row> {
    rows(chat)
        .into_iter()
        .filter(|row| row.opens.is_some())
        .collect()
}

/// Every message the lead's provider read from its agent process.
fn read_by_provider(net: &Net) -> Vec<serde_json::Value> {
    net.provider_input("lead")
        .unwrap()
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The one answer the provider read, which must be to the request `key`
/// names: headless Claude's control response, or Codex's reply to its
/// approval request.
fn answer_read(net: &Net, kind: FakeKind, key: &str) -> serde_json::Value {
    let read = read_by_provider(net);
    match kind {
        FakeKind::ClaudeSdk => {
            let answers: Vec<&serde_json::Value> = read
                .iter()
                .filter(|line| line["type"] == "control_response")
                .filter(|line| line["response"]["response"].get("behavior").is_some())
                .collect();
            assert_eq!(answers.len(), 1, "Claude read one answer: {answers:?}");
            assert_eq!(answers[0]["response"]["request_id"], key, "bravo's request");
            answers[0]["response"]["response"].clone()
        }
        FakeKind::Codex => {
            let id: serde_json::Value = serde_json::from_str(key).unwrap();
            let answers: Vec<&serde_json::Value> = read
                .iter()
                .filter(|line| line.get("method").is_none())
                .filter(|line| line["result"].get("decision").is_some())
                .collect();
            assert_eq!(answers.len(), 1, "Codex read one answer: {answers:?}");
            assert_eq!(answers[0]["id"], id, "bravo's request");
            answers[0]["result"].clone()
        }
        other => unreachable!("no subagents on {other:?}"),
    }
}

/// What the provider read to stop the subagent its own id `child` names,
/// which must be its one stop: Claude's `stop_task` for its task, or
/// Codex's three steps, an interrupt on the child's turn, its background
/// terminals ended, and a steer into the parent's turn.
fn stop_read(net: &Net, kind: FakeKind, child: &str) -> Vec<String> {
    let read = read_by_provider(net);
    let method = |line: &serde_json::Value| -> String {
        line["method"]
            .as_str()
            .or(line["request"]["subtype"].as_str())
            .unwrap_or_default()
            .to_owned()
    };
    match kind {
        FakeKind::ClaudeSdk => {
            let stops: Vec<&serde_json::Value> = read
                .iter()
                .filter(|line| line["request"]["subtype"] == "stop_task")
                .collect();
            assert_eq!(stops.len(), 1, "one stop reached Claude: {stops:?}");
            assert_eq!(stops[0]["request"]["task_id"], child, "delta's task");
            vec![stops[0].to_string()]
        }
        FakeKind::Codex => {
            let at = read
                .iter()
                .position(|line| line["method"] == "turn/interrupt")
                .expect("an interrupt reached Codex");
            let after: Vec<&serde_json::Value> = read[at..]
                .iter()
                .filter(|line| line.get("method").is_some())
                .collect();
            let interrupts: Vec<&&serde_json::Value> = after
                .iter()
                .filter(|line| line["method"] == "turn/interrupt")
                .collect();
            assert_eq!(interrupts.len(), 1, "one interrupt: {interrupts:?}");
            assert_eq!(
                interrupts[0]["params"]["threadId"], child,
                "on delta's thread"
            );
            let steps: Vec<String> = after.iter().map(|line| method(line)).collect();
            assert_eq!(
                steps,
                [
                    "turn/interrupt",
                    "thread/backgroundTerminals/list",
                    "turn/steer"
                ],
                "Codex's three steps, once each"
            );
            assert_eq!(after[1]["params"]["threadId"], child, "delta's terminals");
            assert_ne!(
                after[2]["params"]["threadId"], child,
                "the steer goes to the parent"
            );
            after
                .iter()
                .map(|line| format!("{} {}", method(line), line["params"]))
                .collect()
        }
        other => unreachable!("no subagents on {other:?}"),
    }
}

fn ended(agent: &wire::Agent) -> Option<ExitCause> {
    (agent.lifecycle() == Lifecycle::Exited).then(|| agent.exit_cause())
}

/// Four parallel headless Claude subagents on the desk, read from clients
/// of the desk and of the laptop, one answered and one stopped from the
/// laptop's, and a client of a third host that joins once it is all over.
#[tokio::test(flavor = "multi_thread")]
async fn four_parallel_claude_subagents_across_two_hosts_and_three_clients() {
    four_parallel_subagents(FakeKind::ClaudeSdk).await;
}

/// The same story with Codex child threads: the ask is a command's
/// approval, and the stop is Codex's three steps.
#[tokio::test(flavor = "multi_thread")]
async fn four_parallel_codex_subagents_across_two_hosts_and_three_clients() {
    four_parallel_subagents(FakeKind::Codex).await;
}

async fn four_parallel_subagents(kind: FakeKind) {
    println!("-- {kind:?}: four parallel subagents --");
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("laptop")
            .link("desk", "laptop")
            .agent(
                AgentDecl::new("lead", "desk")
                    .kind(kind)
                    .prompt("Survey the repository.")
                    .steps(lead_steps(kind)),
            ),
    )
    .await
    .unwrap();
    let lead = lead_key(&net);

    // The lead's long history, before any subagent.
    net.turn_ended("lead", 1).await.unwrap();
    net.send("lead", "Take notes on the second pass.")
        .await
        .unwrap();
    net.turn_ended("lead", 2).await.unwrap();

    // A client of each host has the lead's chat open while its subagents
    // run, with the window a chat opens with.
    let mut desk = Client::connect(&net, "desk", "desk's client").await;
    let mut laptop = Client::connect(&net, "laptop", "laptop's client").await;
    let desk_lead = desk.open(&lead).await;
    let laptop_lead = laptop.open(&lead).await;
    for (client, chat) in [(&mut desk, &desk_lead), (&mut laptop, &laptop_lead)] {
        client
            .until("the lead's first turn", |_| {
                (says(chat, "Early turn, finding 1.") && says(chat, "Second turn, note 20."))
                    .then_some(())
                    .ok_or_else(|| format!("{} rows", chat.keys().len()))
            })
            .await;
        println!(
            "{} opens the lead's chat: {} rows, its first turn in view",
            client.name,
            chat.keys().len()
        );
    }

    net.send("lead", "Have four subagents check the modules.")
        .await
        .unwrap();

    // Each subagent is a fleet row of its own under the lead, on both
    // clients, its title the provider's.
    let titles = [
        "Check alpha.",
        "Check bravo.",
        "Check charlie.",
        "Check delta.",
    ];
    let mut subs = Vec::new();
    for client in [&mut desk, &mut laptop] {
        subs = client
            .until("four subagents under the lead", |runtime| {
                let found = subagents(runtime, &lead.agent);
                (found.len() == 4)
                    .then(|| found.clone())
                    .ok_or_else(|| format!("{} so far", found.len()))
            })
            .await;
        let seen: Vec<String> = subs
            .iter()
            .map(|sub| sub.title().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(seen, titles);
        for sub in &subs {
            assert!(sub.name.starts_with("sub-"), "{}", sub.name);
            assert_eq!(
                sub.host_id, lead.host,
                "a subagent lives on its host's host"
            );
        }
        println!(
            "{}: the lead's fleet row has four subagents under it: {:?}",
            client.name,
            subs.iter()
                .map(|sub| format!("{} \"{}\"", sub.name, sub.title().unwrap_or_default()))
                .collect::<Vec<_>>()
        );
    }
    let [alpha, bravo, charlie, delta] = <[wire::Agent; 4]>::try_from(subs).unwrap();

    // Bravo asks, and the lead's row reads waiting on you on both clients,
    // from the fleet: the family sits among those that need the person,
    // its head speaking for bravo.
    for client in [&mut desk, &mut laptop] {
        let bravo_id = bravo.agent_id.clone();
        client
            .until("the lead's row waiting on you for bravo", |runtime| {
                let card = runtime.fleet_card(&lead).ok_or("no card for the lead")?;
                let view = runtime.fleet_view();
                let row = view
                    .sections
                    .iter()
                    .find(|section| section.kind == SectionKind::NeedsYou)
                    .and_then(|section| section.rows.iter().find(|row| row.card.agent == lead))
                    .ok_or_else(|| format!("the lead is not in need-you: {:?}", card.attention))?;
                let loud = row.loud.as_ref().ok_or("the row speaks for no member")?;
                (card.family_attention == Attention::NeedsYou && loud.agent.agent == bravo_id)
                    .then_some(())
                    .ok_or_else(|| format!("{:?}, loud {}", card.family_attention, loud.name))
            })
            .await;
        let bravo_card = client.runtime.fleet_card(&key_of(&bravo)).unwrap();
        assert_eq!(bravo_card.attention, Attention::NeedsYou);
        assert!(bravo_card.native);
        println!(
            "{}: the lead's row reads waiting on you, for \"{}\"; need-you counts {}",
            client.name,
            bravo_card.name,
            client.runtime.fleet_view().need_you
        );
    }

    // The ask is in bravo's own chat, on both clients, opened from its call
    // row in the lead's chat, and nowhere in the lead's.
    let desk_bravo = desk.open(&key_of(&bravo)).await;
    let laptop_bravo = laptop.open(&key_of(&bravo)).await;
    let mut ask_key = String::new();
    for (client, chat) in [(&mut desk, &desk_bravo), (&mut laptop, &laptop_bravo)] {
        let card = client
            .until("bravo's ask in its own chat", |_| {
                chat.ask_card().ok_or_else(|| "no ask".to_owned())
            })
            .await;
        ask_key = card.key.clone();
        println!(
            "{}: bravo's chat holds its ask {}; the lead's holds none",
            client.name, card.key
        );
    }
    assert!(desk_lead.ask_card().is_none() && laptop_lead.ask_card().is_none());

    // Answered from the laptop's client: it reaches Claude once, keyed to
    // bravo's request, and the ask closes on both clients.
    let card = laptop_bravo.ask_card().unwrap();
    let allow = card
        .choices
        .iter()
        .position(|choice| choice.outcome == ChoiceOutcome::AllowOnce)
        .expect("an allow-once choice");
    let outcome = laptop_bravo.answer_choice(&card.key, allow, "").await;
    assert_eq!(outcome, app_runtime::values::ActOutcome::Done);
    println!("laptop's client allows bravo's command: {outcome:?}");
    for (client, chat) in [(&mut desk, &desk_bravo), (&mut laptop, &laptop_bravo)] {
        let bravo_id = bravo.agent_id.clone();
        client
            .until("bravo's ask to close", |runtime| {
                let waiting = agent(runtime, &bravo_id)
                    .is_some_and(|row| row.phase() == wire::Phase::NeedsYou);
                (chat.ask_card().is_none() && !waiting)
                    .then_some(())
                    .ok_or_else(|| {
                        format!(
                            "ask {:?}, waiting {waiting}",
                            chat.ask_card().map(|c| c.key)
                        )
                    })
            })
            .await;
        println!(
            "{}: bravo's ask is closed, and its row no longer waits",
            client.name
        );
    }
    // Delta is running, so its own chat offers Stop; the laptop's client
    // stops it. Claude hears one stop_task for delta's task, delta ends
    // stopped, and the lead carries on.
    let laptop_delta = laptop.open(&key_of(&delta)).await;
    let stop_key = laptop
        .until("delta waiting, its chat offering Stop", |_| {
            if !says(&laptop_delta, "delta waits on the long build") {
                return Err("delta has not reached its wait".to_owned());
            }
            match laptop_delta.frame().composer.mode {
                Composer::Native {
                    stop: JobStop::Offered { key },
                } => Ok(key),
                other => Err(format!("{other:?}")),
            }
        })
        .await;
    assert_eq!(
        Some(stop_key.as_str()),
        delta
            .native
            .as_ref()
            .map(|native| native.provider_child_id.as_str()),
        "the stop names the provider's task for delta"
    );
    let outcome = laptop_delta.stop_job(&stop_key).await;
    assert_eq!(outcome, app_runtime::values::ActOutcome::Done);
    println!("laptop's client stops delta ({stop_key}): {outcome:?}");
    for client in [&mut desk, &mut laptop] {
        let delta_id = delta.agent_id.clone();
        let cause = client
            .until("delta to end", |runtime| {
                agent(runtime, &delta_id)
                    .and_then(|row| ended(&row))
                    .ok_or_else(|| "delta still live".to_owned())
            })
            .await;
        assert_eq!(cause, ExitCause::Stopped);
        println!("{}: delta ended {cause:?}", client.name);
    }
    // The lead carries on: the other three finish, and its turn ends on
    // its own last words.
    net.turn_ended("lead", 3).await.unwrap();
    let finished = [&alpha, &bravo, &charlie].map(|sub| sub.agent_id.clone());
    for (client, chat) in [(&mut desk, &desk_lead), (&mut laptop, &laptop_lead)] {
        client
            .until(
                "alpha, bravo and charlie to finish and the lead to sum up",
                |runtime| {
                    let causes: Vec<Option<ExitCause>> = finished
                        .iter()
                        .map(|id| agent(runtime, id).and_then(|row| ended(&row)))
                        .collect();
                    (causes
                        .iter()
                        .all(|cause| *cause == Some(ExitCause::Finished))
                        && says(chat, "All four have reported."))
                    .then_some(())
                    .ok_or_else(|| format!("{causes:?}"))
                },
            )
            .await;
        println!(
            "{}: alpha, bravo and charlie finished; the lead says \"All four have reported.\"",
            client.name
        );
    }

    // With the lead's turn over, the provider has read all it will of the
    // answer and the stop: each once.
    let answer = answer_read(&net, kind, &ask_key);
    println!("the provider read one answer, to bravo's request {ask_key}: {answer}");
    for line in stop_read(&net, kind, &stop_key) {
        println!("the provider read, for delta's stop: {line}");
    }

    // The parent's history stays in view: the window each client held open
    // while the subagents ran still holds the lead's first turn, and none
    // of their steps; so does a window opened now.
    let fresh_lead = laptop.open(&lead).await;
    laptop
        .until("a fresh window on the lead", |_| {
            fresh_lead
                .frame()
                .caught_up
                .then_some(())
                .ok_or_else(|| "catching up".to_owned())
        })
        .await;
    for (name, chat) in [
        ("desk's client, open throughout", &desk_lead),
        ("laptop's client, open throughout", &laptop_lead),
        ("laptop's client, opened after", &fresh_lead),
    ] {
        assert!(
            says(chat, "Early turn, finding 1."),
            "{name}: the lead's first turn left the window"
        );
        for sub in ["alpha", "bravo", "charlie", "delta"] {
            assert!(
                !says(chat, &format!("{sub} step 1")),
                "{name}: {sub}'s steps are in the lead's chat"
            );
        }
        println!(
            "{name}: the lead's window holds {} rows, from \"Early turn, finding 1.\" on, and no subagent's steps",
            chat.keys().len()
        );
    }

    // The lead's chat holds one row per subagent, folded into one group of
    // four: the group draws at its first call, its state read from the
    // fleet.
    let rows_opening = calls(&desk_lead);
    let opened: Vec<Vec<u8>> = rows_opening
        .iter()
        .map(|row| row.opens.clone().unwrap())
        .collect();
    let ids = [&alpha, &bravo, &charlie, &delta].map(|sub| sub.agent_id.clone());
    assert_eq!(
        opened, ids,
        "one call row for each subagent, in the order started"
    );
    for (at, row) in rows_opening.iter().enumerate() {
        let group = row.group.as_ref().expect("the calls are grouped");
        assert_eq!(group.agents, ids);
        assert!(!group.open);
        assert_eq!(row.collapsed, at > 0, "the group draws at its first call");
    }
    let groups = desk_lead.agents().groups;
    assert_eq!(groups.len(), 1);
    let state = &groups[0].state;
    assert_eq!(
        (state.agents, state.finished, state.ended, state.running),
        (4, 3, 1, 0)
    );
    println!(
        "the lead's chat: 4 call rows, one group drawn at the first: {} agents, {} finished, {} ended",
        state.agents, state.finished, state.ended
    );

    // Each subagent opens from its row, on the laptop's client, as the
    // phone opens one: its own chat holds its prompt and every step it
    // played, and nobody else's.
    let played = [
        ("alpha", STEPS, STEPS),
        ("bravo", STEPS, STEPS + 1),
        ("charlie", STEPS, STEPS),
        ("delta", 15, 15),
    ];
    let mut held_by_subagents = 0;
    for (row, (name, said, calls_made)) in calls(&laptop_lead).iter().zip(played) {
        let opened = laptop_lead
            .agents()
            .agents
            .into_iter()
            .find(|agent| Some(&agent.agent.agent) == row.opens.as_ref())
            .expect("the fleet holds the agent the row opens");
        // The laptop's client reads the subagent through the laptop's
        // replica, which follows the desk on a stream of its own: the
        // fleet can list the subagent finished, and the lead's chat show
        // its turn over, while that replica still lacks the subagent's
        // last call. So the laptop is held to all the desk has of it
        // first, and the chat then shows what the laptop holds.
        let listed = [&alpha, &bravo, &charlie, &delta]
            .into_iter()
            .find(|sub| Some(&sub.agent_id) == row.opens.as_ref())
            .expect("the row opens one of the four");
        net.current_listed("laptop", listed).await.unwrap();
        let chat = laptop.open(&opened.agent).await;
        let held = laptop
            .until(&format!("{name}'s whole chat"), |_| {
                let rows = rows(&chat);
                (chat.frame().caught_up
                    && rows
                        .iter()
                        .any(|row| shown(row).contains(&format!("{name} step {said}"))))
                .then_some(rows)
                .ok_or_else(|| format!("{} rows", chat.keys().len()))
            })
            .await;
        check_own_steps(kind, name, &held, said, calls_made);
        held_by_subagents += held.len();
        println!(
            "laptop's client opens \"{}\" from its row: {} rows, {}{name} step 1 to {said} and {calls_made} calls",
            opened.name,
            held.len(),
            if kind == FakeKind::ClaudeSdk {
                "its prompt, "
            } else {
                ""
            },
        );
    }
    // Kept in the lead's chat, as they were before subagents were agents of
    // their own, these rows would have pushed its first turn out of every
    // window.
    let lead_rows = desk_lead.keys().len();
    assert!(
        lead_rows + held_by_subagents > DEFAULT_TAIL as usize,
        "the story is too small to show the window held: {lead_rows} + {held_by_subagents}"
    );
    println!(
        "the lead's {lead_rows} rows and its subagents' {held_by_subagents} are more than the {DEFAULT_TAIL} a window holds"
    );

    // A client that comes late, on a host that joins now, catches up on a
    // subagent's whole chat from its row.
    net.add_host(HostDecl {
        name: "tablet".into(),
        ..HostDecl::default()
    })
    .await
    .unwrap();
    net.add_link("desk", "tablet").await.unwrap();
    let mut tablet = Client::connect(&net, "tablet", "tablet's late client").await;
    let tablet_lead = tablet.open(&lead).await;
    let alpha_row = tablet
        .until("the lead's call rows", |_| {
            let found = calls(&tablet_lead);
            (found.len() == 4)
                .then(|| found[0].clone())
                .ok_or_else(|| format!("{} calls", found.len()))
        })
        .await;
    let opened = tablet
        .until("alpha in the tablet's fleet", |_| {
            tablet_lead
                .agents()
                .agents
                .into_iter()
                .find(|agent| Some(&agent.agent.agent) == alpha_row.opens.as_ref())
                .ok_or_else(|| "not yet".to_owned())
        })
        .await;
    net.current_listed("tablet", &alpha).await.unwrap();
    let late_alpha = tablet.open(&opened.agent).await;
    let held = tablet
        .until("alpha's whole chat", |_| {
            let rows = rows(&late_alpha);
            (late_alpha.frame().caught_up
                && rows
                    .iter()
                    .any(|row| shown(row).contains(&format!("alpha step {STEPS}"))))
            .then_some(rows)
            .ok_or_else(|| format!("{} rows", late_alpha.keys().len()))
        })
        .await;
    check_own_steps(kind, "alpha", &held, STEPS, STEPS);
    println!(
        "tablet's late client opens alpha from its row: {} rows, all of them",
        held.len()
    );

    net.shutdown().await.unwrap();
}

/// A subagent's chat holds its prompt first, then each of its `said`
/// words in order and `calls` calls, and no other subagent's words. Codex
/// never shows a client what a parent asked its child, so a Codex child's
/// chat opens on its first step.
fn check_own_steps(kind: FakeKind, name: &str, rows: &[Row], said: usize, calls: usize) {
    let prompt = rows.first().expect("a row");
    let prompted =
        matches!(prompt.kind, RowKind::Prompt { .. }) && shown(prompt).contains(&task(name));
    assert_eq!(
        prompted,
        kind == FakeKind::ClaudeSdk,
        "{name} opens on its prompt: {}",
        shown(prompt)
    );
    let words: Vec<String> = rows
        .iter()
        .filter(|row| matches!(row.kind, RowKind::Prose { .. }))
        .map(shown)
        .collect();
    let expected: Vec<String> = (1..=said).map(|at| format!("{name} step {at}")).collect();
    let mut found = words
        .iter()
        .filter(|word| word.contains(&format!("{name} step ")));
    for want in &expected {
        assert!(
            found.any(|word| word.contains(&format!("\"{want}\""))),
            "{name}'s chat lacks {want:?}, in order"
        );
    }
    for other in ["alpha", "bravo", "charlie", "delta"]
        .iter()
        .filter(|other| **other != name)
    {
        assert!(
            !words
                .iter()
                .any(|word| word.contains(&format!("{other} step"))),
            "{name}'s chat holds {other}'s words"
        );
    }
    let made = rows
        .iter()
        .filter(|row| !matches!(row.kind, RowKind::Prompt { .. } | RowKind::Prose { .. }))
        .count();
    assert_eq!(
        made,
        calls,
        "{name}'s calls: {:?}",
        rows.iter().map(shown).collect::<Vec<_>>()
    );
}

/// Everything `agent`'s own host holds for it, oldest first.
async fn items(net: &Net, agent: &str) -> Vec<wire::Item> {
    let at = net.agent(agent).unwrap().clone();
    let key = at.key();
    let mut items = net
        .runtime(&at.host)
        .unwrap()
        .read_store(move |store| store.page(&key, None, 1_000).unwrap().items)
        .await
        .unwrap();
    items.sort_by_key(|item| item.order);
    items
}

/// A one-shot host stays for its background subagent: an amux-spawned
/// child on the laptop starts one, and its turn ends while the subagent
/// still works. It does not leave then, which would kill the subagent and
/// report "it is counting" as its answer; it stays until the subagent has
/// ended and Claude's follow-up turn on its notification has finished, and
/// reports that.
#[tokio::test(flavor = "multi_thread")]
async fn a_one_shot_host_stays_until_its_background_subagent_and_follow_up_turn_end() {
    println!("-- a one-shot host with a background subagent --");
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("laptop")
            .link("desk", "laptop")
            .agent(
                AgentDecl::new("lead", "desk")
                    .prompt("Plan the count.")
                    .steps(vec![
                        text("A helper on the laptop will count."),
                        Step::TurnEnd,
                    ]),
            ),
    )
    .await
    .unwrap();
    net.turn_ended("lead", 1).await.unwrap();
    let counter = Subagent {
        task: "Count the files.".into(),
        nickname: None,
        await_read: false,
        background: true,
        steps: vec![
            Step::WaitFor {
                path: "count-go".into(),
            },
            explore(),
            text("There are 3 files."),
        ],
    };
    net.spawn_child(
        "lead",
        AgentDecl::new("helper", "laptop")
            .kind(FakeKind::ClaudeSdk)
            .prompt("Count the files with a subagent.")
            .steps(vec![
                Step::Subagents {
                    agents: vec![counter],
                },
                text("The subagent is counting in the background."),
                Step::TurnEnd,
                text("Done: there are 3 files."),
                Step::TurnEnd,
            ]),
    )
    .await
    .unwrap();
    let helper = net.agent("helper").unwrap().clone();
    println!("lead@desk spawns helper on the laptop, a one-shot child");

    // Its turn ends with the subagent still at work, and it stays.
    net.turn_ended("helper", 1).await.unwrap();
    let mut desk = Client::connect(&net, "desk", "desk's client").await;
    let helper_id = helper.id.as_bytes().to_vec();
    let counter = desk
        .until("helper's subagent at work after helper's turn", |runtime| {
            let found = subagents(runtime, &helper_id);
            let live =
                agent(runtime, &helper_id).is_some_and(|row| row.lifecycle() == Lifecycle::Live);
            match found.as_slice() {
                [sub] if live && sub.lifecycle() == Lifecycle::Live => Ok(sub.clone()),
                _ => Err(format!("{} subagents, helper live {live}", found.len())),
            }
        })
        .await;
    println!(
        "helper's turn ended on \"The subagent is counting in the background.\"; helper is live, its subagent {} \"{}\" works on",
        counter.name,
        counter.title().unwrap_or_default()
    );

    // The subagent finishes; Claude's follow-up turn on its notification
    // ends; only then does helper finish, reporting that turn's answer.
    net.open_gate("count-go").unwrap();
    net.turn_ended("helper", 2).await.unwrap();
    let counter_id = counter.agent_id.clone();
    let (helper_end, counter_end) = desk
        .until("helper and its subagent to end", |runtime| {
            let helper = agent(runtime, &helper_id).and_then(|row| ended(&row));
            let counter = agent(runtime, &counter_id).and_then(|row| ended(&row));
            match (helper, counter) {
                (Some(helper), Some(counter)) => Ok((helper, counter)),
                other => Err(format!("{other:?}")),
            }
        })
        .await;
    assert_eq!(
        counter_end,
        ExitCause::Finished,
        "the subagent was not cut short"
    );
    assert_eq!(helper_end, ExitCause::Finished);
    let counter_chat = desk.open(&key_of(&counter)).await;
    desk.until("the subagent's answer", |_| {
        says(&counter_chat, "There are 3 files.")
            .then_some(())
            .ok_or_else(|| format!("{} rows", counter_chat.keys().len()))
    })
    .await;
    let row = agent(&desk.runtime, &helper_id).unwrap();
    assert_eq!(row.incarnation, 1, "helper ran in one process throughout");
    let said: Vec<String> = items(&net, "helper")
        .await
        .iter()
        .filter(|item| item.key.starts_with("turn:"))
        .map(|item| item.key.clone())
        .collect();
    assert_eq!(said.len(), 2, "helper's two turns ended: {said:?}");
    println!(
        "the subagent said \"There are 3 files.\" and ended {counter_end:?}; helper's follow-up turn ended; helper then ended {helper_end:?}, in its first process"
    );

    // The lead hears the follow-up turn's answer as helper's last report.
    let reports = testnet::until("the lead to hear helper's answer", || {
        let net = &net;
        async move {
            let got: Vec<String> = items(net, "lead")
                .await
                .into_iter()
                .filter(|item| !item.input_id.is_empty() && item.input_id != b"testnet-first")
                .map(|item| item.text)
                .collect();
            got.iter()
                .any(|text| text.contains("Done: there are 3 files."))
                .then(|| got.clone())
                .ok_or_else(|| format!("{got:?}"))
        }
    })
    .await
    .unwrap();
    assert_eq!(
        reports,
        [
            "The subagent is counting in the background.",
            "Done: there are 3 files."
        ],
        "a report for each of helper's turns, the follow-up turn's last"
    );
    println!("lead hears from helper: {reports:?}");
    net.shutdown().await.unwrap();
}
