//! Inputs, agent messages and the deliveries outbox: a person's input
//! relayed with the interpreter's verdict, messages through the
//! recipient's lane, and a parent hearing once that its child finished or
//! failed across daemon crashes and resumes.
//!
//! Most agents here are synthetic (a directory, a journal and a control
//! socket speaking the agent's protocol), so each window can be opened on
//! purpose; the last test runs real agent processes on the fake provider.

mod support;

use std::sync::Arc;
use std::time::Duration;

use node::{ClientApi, Daemon, Launch, ProfileRuntime, RegistryError, RelayError};
use store::AgentKey;
use support::synthetic::*;
use support::*;
use tonic::{Code, Request};
use wire::client_service_server::ClientService as _;
use wire::{
    AgentRef, ClaudeSdkInput, EnvelopeKind, Input, Lifecycle, PromptInput, SendInputRequest,
    SendInputResponse, SendMessageRequest, StopMode, claude_sdk_input, send_input_response, sender,
};

const SEGMENTS: u64 = 1 << 20;

async fn start(install: &Install, boot: &str) -> (Daemon, Arc<ProfileRuntime>) {
    let daemon = install.start(boot, quiet_launch()).await;
    let runtime = runtime(&daemon, install);
    (daemon, runtime)
}

/// A daemon crash: dropped without shutdown. Returns once nothing of it is
/// still running.
async fn crash(daemon: Daemon, runtime: Arc<ProfileRuntime>) {
    let gone = Arc::downgrade(&runtime);
    drop(runtime);
    drop(daemon);
    until("the crashed daemon's tasks to end", || async {
        match gone.upgrade() {
            None => Ok(()),
            Some(live) => Err(format!("{} references live", Arc::strong_count(&live))),
        }
    })
    .await
    .unwrap();
}

fn prompt(id: &[u8], text: &str) -> Input {
    Input::new(
        id.to_vec(),
        ClaudeSdkInput {
            of: Some(claude_sdk_input::Of::Prompt(PromptInput {
                text: text.to_owned(),
                attachments: Vec::new(),
            })),
        },
    )
}

fn interrupt(id: &[u8]) -> Input {
    Input::new(
        id.to_vec(),
        ClaudeSdkInput {
            of: Some(claude_sdk_input::Of::Interrupt(wire::Interrupt {})),
        },
    )
}

fn verdict(response: &SendInputResponse) -> String {
    match &response.of {
        Some(send_input_response::Of::Accepted(accepted)) if accepted.queued => "queued".into(),
        Some(send_input_response::Of::Accepted(_)) => "accepted".into(),
        Some(send_input_response::Of::Rejected(rejected)) => {
            format!("rejected {:?}", rejected.refusal())
        }
        None => "none".into(),
    }
}

fn to(agent: &SyntheticAgent, install: &Install) -> Option<AgentRef> {
    Some(AgentRef {
        host_id: host(install).as_bytes().to_vec(),
        agent_id: agent.id.as_bytes().to_vec(),
    })
}

/// The agent messages an agent was handed, as (kind, text).
fn messages(agent: &SyntheticAgent) -> Vec<(EnvelopeKind, String)> {
    agent
        .inputs()
        .into_iter()
        .filter_map(|handed| match handed {
            Handed::Message(envelope) => Some((
                EnvelopeKind::try_from(envelope.kind).unwrap(),
                envelope.text,
            )),
            Handed::Input(_) => None,
        })
        .collect()
}

/// The outbox holds `n` rows, or what it holds instead.
async fn rows(runtime: &ProfileRuntime, n: usize) -> Result<(), String> {
    let rows = deliveries(runtime).await;
    (rows == n)
        .then_some(())
        .ok_or_else(|| format!("{rows} delivery rows"))
}

/// The agent's row says exited, or what it says instead.
async fn exited(runtime: &ProfileRuntime, id: uuid::Uuid) -> Result<(), String> {
    let lifecycle = runtime.agent(id).await.unwrap().lifecycle;
    (lifecycle == Lifecycle::Exited as i32)
        .then_some(())
        .ok_or_else(|| format!("lifecycle {lifecycle}"))
}

/// The process has been handed `n` inputs, or how many it has.
fn handed(agent: &SyntheticAgent, n: usize) -> Result<(), String> {
    let inputs = agent.inputs().len();
    (inputs == n)
        .then_some(())
        .ok_or_else(|| format!("{inputs} inputs handed"))
}

/// The parent has heard `n` messages, or what it has heard.
fn heard(agent: &SyntheticAgent, n: usize) -> Result<(), String> {
    let heard = messages(agent);
    (heard.len() == n)
        .then_some(())
        .ok_or_else(|| format!("heard {heard:?}"))
}

async fn deliveries(runtime: &ProfileRuntime) -> usize {
    runtime.store().await.deliveries().unwrap().len()
}

async fn items_with_input(runtime: &ProfileRuntime, key: &AgentKey) -> Vec<wire::Item> {
    runtime
        .store()
        .await
        .page(key, None, 1_000)
        .unwrap()
        .items
        .into_iter()
        .filter(|item| !item.input_id.is_empty())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn send_input_returns_the_interpreters_verdict_or_answers_for_an_exited_agent() {
    let install = Install::new();
    let mut live = SyntheticAgent::new(&install, "live", SEGMENTS);
    live.register_offline(&install);
    live.go_live();
    let gone = SyntheticAgent::new(&install, "gone", SEGMENTS);
    gone.register_offline(&install);
    let (daemon, runtime) = start(&install, "boot-1").await;
    let send = |agent: &SyntheticAgent, id: &[u8]| SendInputRequest {
        agent_id: agent.id.as_bytes().to_vec(),
        input: Some(prompt(id, "hello")),
    };

    let answer = runtime.send_input(&send(&live, b"i1")).await.unwrap();
    assert_eq!(verdict(&answer), "queued");
    live.answer_with(Answer::Reject(wire::Refusal::ClosedAsk));
    let answer = runtime.send_input(&send(&live, b"i2")).await.unwrap();
    assert_eq!(
        verdict(&answer),
        "rejected ClosedAsk",
        "the interpreter's reason, verbatim"
    );
    assert_eq!(live.inputs().len(), 2, "both reached the agent");

    let answer = runtime.send_input(&send(&gone, b"i3")).await.unwrap();
    assert_eq!(
        verdict(&answer),
        "rejected Exited",
        "the daemon answers for an exited agent"
    );
    assert!(gone.inputs().is_empty());

    // The agent takes the input and the connection ends before its answer:
    // the sender learns the answer was lost, never a verdict.
    live.answer_with(Answer::AcceptSilently);
    let pending = tokio::spawn({
        let runtime = runtime.clone();
        let request = send(&live, b"i4");
        async move { runtime.send_input(&request).await }
    });
    until("the input to arrive", || async { handed(&live, 3) })
        .await
        .unwrap();
    live.die().await;
    assert!(matches!(pending.await.unwrap(), Err(RelayError::Lost)));
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_sends_input_only_to_its_own_children() {
    let install = Install::new();
    let caller = SyntheticAgent::new(&install, "caller", SEGMENTS);
    caller.register_offline(&install);
    let mut child = SyntheticAgent::new(&install, "child", SEGMENTS);
    child.parent = Some(caller.key(&install));
    child.register_offline(&install);
    child.go_live();
    let mut stranger = SyntheticAgent::new(&install, "stranger", SEGMENTS);
    stranger.register_offline(&install);
    stranger.go_live();
    let (daemon, runtime) = start(&install, "boot-1").await;
    let tools = ClientApi::new(&runtime, Some(caller.id));
    let send = |agent: &SyntheticAgent, id: &[u8]| {
        Request::new(SendInputRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            input: Some(interrupt(id)),
        })
    };

    let refused = tools.send_input(send(&stranger, b"i1")).await.unwrap_err();
    assert_eq!(refused.code(), Code::PermissionDenied);
    assert!(stranger.inputs().is_empty(), "nothing reached the stranger");

    let answer = tools.send_input(send(&child, b"i2")).await.unwrap();
    assert_eq!(verdict(answer.get_ref()), "queued");
    assert_eq!(child.inputs().len(), 1, "the child's input was relayed");

    // The profile socket is a person's: the same input goes through.
    let person = ClientApi::new(&runtime, None);
    let answer = person.send_input(send(&stranger, b"i3")).await.unwrap();
    assert_eq!(verdict(answer.get_ref()), "queued");
    assert_eq!(stranger.inputs().len(), 1);
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_is_accepted_once_its_item_commits_and_a_retry_is_deduped() {
    let install = Install::new();
    let sender = SyntheticAgent::new(&install, "sender", SEGMENTS);
    sender.register_offline(&install);
    let mut recipient = SyntheticAgent::new(&install, "recipient", SEGMENTS);
    recipient.register_offline(&install);
    recipient.go_live();
    let (daemon, runtime) = start(&install, "boot-1").await;

    let envelope = SendMessageRequest {
        id: b"envelope-1".to_vec(),
        to: to(&recipient, &install),
        text: "can you look at the build?".into(),
        ..SendMessageRequest::default()
    };
    let sent = runtime
        .send_message(envelope.clone(), Some(sender.id))
        .await
        .unwrap();
    assert_eq!(sent.envelope_id, b"envelope-1");
    let accepted = runtime
        .store()
        .await
        .item_by_input(&recipient.key(&install), b"envelope-1")
        .unwrap();
    assert!(
        accepted.is_some(),
        "the answer came after the acceptance item committed"
    );

    let handed = recipient.inputs();
    let Handed::Message(handed) = &handed[0] else {
        panic!("an agent message");
    };
    match handed.from.as_ref().and_then(|from| from.value.as_ref()) {
        Some(sender::Value::Agent(from)) => {
            assert_eq!(from.agent_id, sender.id.as_bytes().to_vec());
            assert_eq!(from.name, "sender");
        }
        other => panic!("the sender is the calling agent, not {other:?}"),
    }
    assert_eq!(handed.kind, EnvelopeKind::Message as i32);

    // A retry of the same envelope: the lane finds the item and answers
    // without handing it over again.
    runtime
        .send_message(envelope, Some(sender.id))
        .await
        .unwrap();
    assert_eq!(recipient.inputs().len(), 1, "the retry was deduped");
    assert_eq!(
        items_with_input(&runtime, &recipient.key(&install))
            .await
            .len(),
        1
    );

    // An exited recipient, and a sender that is not its parent.
    recipient.die().await;
    until("the recipient's exit", || exited(&runtime, recipient.id))
        .await
        .unwrap();
    let refused = runtime
        .send_message(
            SendMessageRequest {
                id: b"envelope-2".to_vec(),
                to: to(&recipient, &install),
                text: "still there?".into(),
                ..SendMessageRequest::default()
            },
            Some(sender.id),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(refused, RelayError::Rejected(ref rejected) if rejected.refusal() == wire::Refusal::Exited)
    );
    crash(daemon, runtime).await;
}

/// The sender on what the recipient is handed is whoever called: the agent
/// whose tools socket the call came in on, or the person on the profile
/// socket. The request has nowhere to name anyone else, and what it sends
/// is an ordinary message.
#[tokio::test(flavor = "multi_thread")]
async fn send_message_sender_is_the_caller() {
    let install = Install::new();
    let sender = SyntheticAgent::new(&install, "sender", SEGMENTS);
    sender.register_offline(&install);
    let mut recipient = SyntheticAgent::new(&install, "recipient", SEGMENTS);
    recipient.register_offline(&install);
    recipient.go_live();
    let (daemon, runtime) = start(&install, "boot-1").await;

    let on_tools = ClientApi::new(&runtime, Some(sender.id));
    let on_profile = ClientApi::new(&runtime, None);
    for (api, id) in [
        (&on_tools, b"from-the-agent".as_slice()),
        (&on_profile, b"from-the-person".as_slice()),
    ] {
        let sent = api
            .send_message(Request::new(SendMessageRequest {
                id: id.to_vec(),
                to: to(&recipient, &install),
                text: "a word".into(),
                context: None,
            }))
            .await
            .unwrap();
        assert_eq!(sent.get_ref().envelope_id, id);
    }

    let handed = recipient
        .inputs()
        .into_iter()
        .map(|handed| match handed {
            Handed::Message(envelope) => envelope,
            Handed::Input(input) => panic!("an agent message, not {input:?}"),
        })
        .collect::<Vec<_>>();
    let [from_agent, from_person] = handed.as_slice() else {
        panic!("two messages, got {handed:?}");
    };
    match from_agent
        .from
        .as_ref()
        .and_then(|from| from.value.as_ref())
    {
        Some(sender::Value::Agent(from)) => {
            assert_eq!(from.agent_id, sender.id.as_bytes().to_vec());
            assert_eq!(from.host_id, host(&install).as_bytes().to_vec());
            assert_eq!(from.name, "sender");
        }
        other => panic!("the calling agent sent it, not {other:?}"),
    }
    assert!(
        matches!(
            from_person
                .from
                .as_ref()
                .and_then(|from| from.value.as_ref()),
            Some(sender::Value::Human(_))
        ),
        "the person sent it: {from_person:?}"
    );
    for envelope in &handed {
        assert_eq!(envelope.kind, EnvelopeKind::Message as i32);
        assert_eq!(envelope.incarnation, None);
    }
    crash(daemon, runtime).await;
}

/// A caller that resends with the id it chose, as one whose answer was
/// lost does, makes one item on the recipient and one hand-off.
#[tokio::test(flavor = "multi_thread")]
async fn resend_with_same_id_is_one_item() {
    let install = Install::new();
    let mut recipient = SyntheticAgent::new(&install, "recipient", SEGMENTS);
    recipient.register_offline(&install);
    recipient.go_live();
    let (daemon, runtime) = start(&install, "boot-1").await;
    let api = ClientApi::new(&runtime, None);
    let request = SendMessageRequest {
        id: b"chosen-by-the-caller".to_vec(),
        to: to(&recipient, &install),
        text: "can you look at the build?".into(),
        context: Some(b"build".to_vec()),
    };
    for _ in 0..2 {
        let sent = api
            .send_message(Request::new(request.clone()))
            .await
            .unwrap();
        assert_eq!(sent.get_ref().envelope_id, request.id);
    }
    assert_eq!(recipient.inputs().len(), 1, "handed over once");
    let items = items_with_input(&runtime, &recipient.key(&install)).await;
    assert_eq!(
        items
            .iter()
            .map(|item| item.input_id.as_slice())
            .collect::<Vec<_>>(),
        vec![request.id.as_slice()],
        "one item for the message"
    );
    crash(daemon, runtime).await;
}

/// A child whose journal already holds a finished turn: its last message,
/// then the turn end.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_waits_for_its_item_to_commit_and_one_envelope_is_handed_over_once() {
    let install = Install::new();
    let mut recipient = SyntheticAgent::new(&install, "recipient", SEGMENTS);
    recipient.register_offline(&install);
    recipient.go_live();
    recipient.answer_with(Answer::AcceptWithoutNudge);
    let (daemon, runtime) = start(&install, "boot-1").await;
    let envelope = SendMessageRequest {
        id: b"envelope-1".to_vec(),
        to: to(&recipient, &install),
        text: "can you look at the build?".into(),
        ..SendMessageRequest::default()
    };
    let send = || {
        let runtime = runtime.clone();
        let envelope = envelope.clone();
        tokio::spawn(async move { runtime.send_message(envelope, None).await })
    };

    let first = send();
    until("the hand-off", || async { handed(&recipient, 1) })
        .await
        .unwrap();
    // The same envelope again, as a sender retrying after a lost answer.
    // A window: a send blocked in the lane leaves no mark to wait on.
    let second = send();
    holds_for(
        "the retry to wait in the lane",
        Duration::from_millis(300),
        || async { !first.is_finished() && !second.is_finished() && recipient.inputs().len() == 1 },
    )
    .await
    .expect("the first send waits for its item, the retry waits in the lane, and one hand-off");

    recipient.nudge().await;
    first
        .await
        .unwrap()
        .expect("accepted once its item committed");
    second
        .await
        .unwrap()
        .expect("the retry finds the committed item");
    assert_eq!(recipient.inputs().len(), 1, "one hand-off");
    assert_eq!(
        items_with_input(&runtime, &recipient.key(&install))
            .await
            .len(),
        1,
        "one item"
    );
    crash(daemon, runtime).await;
}

/// A daemon that starts real agent processes on the fake provider, for
/// resumes of synthetic agents.
async fn start_resuming(install: &Install) -> (Daemon, Arc<ProfileRuntime>) {
    let daemon = install
        .start(
            "boot-1",
            install.launch("resumed", vec![text("done"), wire_turn_end()]),
        )
        .await;
    let runtime = runtime(&daemon, install);
    (daemon, runtime)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_answering_exiting_to_its_parents_message_is_resumed_with_it() {
    let install = Install::new();
    let parent = SyntheticAgent::new(&install, "parent", SEGMENTS);
    parent.register_offline(&install);
    let mut child = SyntheticAgent::new(&install, "child", SEGMENTS);
    child.parent = Some(parent.key(&install));
    child.register_offline(&install);
    child.write_spec(&install);
    child.go_live();
    child.answer_with(Answer::Reject(wire::Refusal::Exiting));
    let (daemon, runtime) = start_resuming(&install).await;

    let envelope = SendMessageRequest {
        id: b"wrap-up".to_vec(),
        to: to(&child, &install),
        text: "wrap up and report".into(),
        ..SendMessageRequest::default()
    };
    let sent = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.send_message(envelope, Some(parent.id)).await }
    });
    until("the hand-off", || async { handed(&child, 1) })
        .await
        .unwrap();
    // The child was on its way out; its process ends.
    child.die().await;
    sent.await
        .unwrap()
        .expect("the parent's message resumes the child");

    assert_eq!(runtime.agent(child.id).await.unwrap().incarnation, 2);
    let spec = std::fs::read(child.dir.join("spec.2")).unwrap();
    let spec = <wire::AgentSpec as prost::Message>::decode(spec.as_slice()).unwrap();
    let first = spec.bootstrap.expect("the message starts the incarnation");
    match first.of {
        Some(wire::bootstrap::Of::AgentMessage(message)) => {
            assert_eq!(message.id, b"wrap-up");
            assert_eq!(message.text, "wrap up and report");
        }
        other => panic!("the parent's message, not {other:?}"),
    }
    assert!(
        runtime
            .store()
            .await
            .item_by_input(&child.key(&install), b"wrap-up")
            .unwrap()
            .is_some(),
        "the new incarnation accepted it"
    );
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_input_answered_exiting_lets_a_resume_wait_for_the_lock() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "leaving", SEGMENTS);
    agent.register_offline(&install);
    agent.write_spec(&install);
    agent.go_live();
    agent.answer_with(Answer::Reject(wire::Refusal::Exiting));
    let (daemon, runtime) = start_resuming(&install).await;

    let answer = runtime
        .send_input(&SendInputRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            input: Some(prompt(b"i1", "one more thing")),
        })
        .await
        .unwrap();
    assert_eq!(verdict(&answer), "rejected Exiting");
    let resumed = tokio::spawn({
        let runtime = runtime.clone();
        let id = agent.id;
        async move { runtime.resume(id, None).await }
    });
    // A window: a resume blocked on the lock leaves no mark to wait on.
    holds_for("the resume to wait", Duration::from_millis(300), || async {
        !resumed.is_finished()
    })
    .await
    .expect("the resume waits for the leaving process instead of refusing a live agent");
    agent.die().await;
    let resumed = resumed
        .await
        .unwrap()
        .expect("resumed once the lock is free");
    assert_eq!(resumed.incarnation, 2);
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delivery_waiting_on_its_parents_lane_is_dropped_when_the_parent_resumes() {
    let install = Install::new();
    let mut parent = SyntheticAgent::new(&install, "parent", SEGMENTS);
    parent.register_offline(&install);
    parent.go_live();
    let mut child = SyntheticAgent::new(&install, "child", SEGMENTS);
    child.parent = Some(parent.key(&install));
    child.register_offline(&install);
    child.go_live();
    let (daemon, runtime) = start(&install, "boot-1").await;

    // A person's message holds the parent's lane until its item commits.
    parent.answer_with(Answer::AcceptWithoutNudge);
    let held = tokio::spawn({
        let runtime = runtime.clone();
        let envelope = SendMessageRequest {
            id: b"hold".to_vec(),
            to: to(&parent, &install),
            text: "a word".into(),
            ..SendMessageRequest::default()
        };
        async move { runtime.send_message(envelope, None).await }
    });
    until("the hand-off", || async { handed(&parent, 1) })
        .await
        .unwrap();
    parent.answer_with(Answer::Accept);

    // The child finishes a turn for the parent's first incarnation, and the
    // drain queues behind the held lane.
    child.append(&snapshot(wire::Phase::Working, &[], 2_000));
    child.append(&item("last", "the tests pass"));
    child.append(&turn_end(1, "last"));
    child.nudge().await;
    until("the delivery row", || rows(&runtime, 1))
        .await
        .unwrap();
    let drained = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.drain_deliveries().await }
    });
    // A window: a drain blocked on the lane leaves no mark to wait on.
    holds_for("the drain to wait", Duration::from_millis(300), || async {
        !drained.is_finished()
    })
    .await
    .expect("the drain waits for the parent's lane");

    // While it waits, the parent becomes its second incarnation.
    {
        let mut store = runtime.store().await;
        let mut row = store.agent(&parent.key(&install)).unwrap().unwrap();
        row.incarnation = 2;
        store.put_agent(&row).unwrap();
    }
    parent.nudge().await;
    held.await.unwrap().unwrap();
    let report = drained.await.unwrap();
    assert_eq!(
        (report.delivered, report.stale),
        (0, 1),
        "the row was for an incarnation that is gone"
    );
    until("the outbox to empty", || rows(&runtime, 0))
        .await
        .unwrap();
    assert_eq!(
        messages(&parent),
        vec![(EnvelopeKind::Message, "a word".to_owned())],
        "the new incarnation never hears of the old one's child"
    );
    crash(daemon, runtime).await;
}

fn finished_child(install: &Install, parent: &SyntheticAgent) -> SyntheticAgent {
    let mut child = SyntheticAgent::new(install, "child", SEGMENTS);
    child.parent = Some(parent.key(install));
    child.register_offline(install);
    child.append(&snapshot(wire::Phase::Working, &[], 2_000));
    child.append(&item("last", "the tests pass"));
    child.append(&turn_end(1, "last"));
    child
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_finishing_while_the_daemon_is_down_reaches_its_parent_once() {
    let install = Install::new();
    let mut parent = SyntheticAgent::new(&install, "parent", SEGMENTS);
    parent.register_offline(&install);
    parent.go_live();
    let child = finished_child(&install, &parent);
    // The child finished and exited while no daemon ran.
    let (daemon, runtime) = start(&install, "boot-1").await;
    until("the delivery", || rows(&runtime, 0)).await.unwrap();
    assert_eq!(
        messages(&parent),
        vec![(EnvelopeKind::Finished, "the tests pass".to_owned())],
        "one finished message, carrying the child's last message, and no failed one"
    );
    let delivered = items_with_input(&runtime, &parent.key(&install)).await;
    assert_eq!(delivered.len(), 1);
    assert_eq!(
        runtime.agent(child.id).await.unwrap().lifecycle,
        Lifecycle::Exited as i32
    );
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_between_hand_off_and_delete_yields_one_item() {
    let install = Install::new();
    let mut parent = SyntheticAgent::new(&install, "parent", SEGMENTS);
    parent.register_offline(&install);
    // The parent accepts, but its answer never reaches the daemon.
    parent.answer_with(Answer::AcceptSilently);
    parent.go_live();
    finished_child(&install, &parent);
    let (daemon, runtime) = start(&install, "boot-1").await;
    until("the parent's acceptance item to commit", || async {
        let items = items_with_input(&runtime, &parent.key(&install)).await;
        (items.len() == 1)
            .then_some(())
            .ok_or_else(|| format!("{} items carry an input", items.len()))
    })
    .await
    .unwrap();
    assert_eq!(deliveries(&runtime).await, 1, "the row is still there");
    crash(daemon, runtime).await;

    // On the next start the drain sends the row again; the parent's daemon
    // finds the envelope id among the parent's items and does not hand it
    // over a second time.
    parent.answer_with(Answer::Accept);
    let (daemon, runtime) = start(&install, "boot-1").await;
    until("the row to be settled", || rows(&runtime, 0))
        .await
        .unwrap();
    assert_eq!(parent.inputs().len(), 1, "handed to the parent once");
    assert_eq!(
        items_with_input(&runtime, &parent.key(&install))
            .await
            .len(),
        1,
        "one item"
    );
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_parent_receives_nothing() {
    let install = Install::new();
    let mut parent = SyntheticAgent::new(&install, "parent", SEGMENTS);
    parent.register_offline(&install);
    finished_child(&install, &parent);
    // The parent is exited when its child finishes: the row waits.
    let (daemon, runtime) = start(&install, "boot-1").await;
    assert_eq!(runtime.drain_deliveries().await.kept, 1);
    assert_eq!(
        runtime.store().await.deliveries().unwrap()[0].parent_incarnation,
        1
    );
    crash(daemon, runtime).await;

    // The parent is resumed: a new incarnation, waiting for nothing.
    parent.incarnation = 2;
    parent.register_offline(&install);
    parent.go_live();
    let (daemon, runtime) = start(&install, "boot-1").await;
    until("the stale row to go", || rows(&runtime, 0))
        .await
        .unwrap();
    // The row is the only thing a hand-off could come from, and a
    // hand-off would have removed it as delivered: gone undelivered, it
    // left nothing to hand.
    assert!(
        parent.inputs().is_empty(),
        "the resumed parent was handed nothing"
    );
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_whose_parent_row_is_missing_keeps_its_row_until_the_parent_is_known() {
    let install = Install::new();
    // The parent's row is not held yet: on another daemon's inventory
    // still to arrive, say, or dropped by a rewind.
    let mut parent = SyntheticAgent::new(&install, "parent", SEGMENTS);
    parent.incarnation = 3;
    finished_child(&install, &parent);
    let (daemon, runtime) = start(&install, "boot-1").await;
    let report = runtime.drain_deliveries().await;
    assert_eq!(report.kept, 1, "never dropped and never sent");
    let row = runtime.store().await.deliveries().unwrap().remove(0);
    assert_eq!(
        row.parent_incarnation, 0,
        "the incarnation is unknown, not guessed"
    );
    crash(daemon, runtime).await;

    // The first daemon removed the parent's directory, which no row
    // listed; now the parent exists here.
    parent.reopen_journal();
    parent.register_offline(&install);
    parent.go_live();
    let (daemon, runtime) = start(&install, "boot-1").await;
    until("the delivery", || rows(&runtime, 0)).await.unwrap();
    assert_eq!(
        messages(&parent),
        vec![(EnvelopeKind::Finished, "the tests pass".to_owned())],
        "stamped with the parent's incarnation once its row was held, then delivered"
    );
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_gone_without_a_turn_end_tells_its_parent_it_failed() {
    let install = Install::new();
    let mut parent = SyntheticAgent::new(&install, "parent", SEGMENTS);
    parent.register_offline(&install);
    parent.go_live();
    let mut crashed = SyntheticAgent::new(&install, "crashed", SEGMENTS);
    crashed.parent = Some(parent.key(&install));
    crashed.register_offline(&install);
    crashed.append(&snapshot(wire::Phase::Working, &[], 2_000));
    crashed.go_live();
    let mut finished = SyntheticAgent::new(&install, "finished", SEGMENTS);
    finished.parent = Some(parent.key(&install));
    finished.register_offline(&install);
    finished.append(&item("last", "done"));
    finished.append(&turn_end(1, "last"));
    finished.go_live();
    let (daemon, runtime) = start(&install, "boot-1").await;
    until("the finished message", || async { heard(&parent, 1) })
        .await
        .unwrap();

    // One dies mid-turn without a word; the other exits by the one-shot
    // rule after its finished turn.
    crashed.die().await;
    finished.exit(wire::ExitCause::Finished, None).await;
    until("both exits", || async {
        exited(&runtime, crashed.id).await?;
        exited(&runtime, finished.id).await
    })
    .await
    .unwrap();
    until("the outbox to drain", || rows(&runtime, 0))
        .await
        .unwrap();
    let mut heard = messages(&parent);
    heard.sort();
    assert_eq!(
        heard,
        vec![
            (EnvelopeKind::Finished, "done".to_owned()),
            (EnvelopeKind::Failed, String::new()),
        ],
        "one thing per event: finished once, failed only for the child that never finished"
    );
    // The failed report carries the cause as data, for the parent's
    // interpreter to word.
    let ended = parent
        .inputs()
        .into_iter()
        .filter_map(|handed| match handed {
            Handed::Message(envelope) if envelope.kind == EnvelopeKind::Failed as i32 => {
                Some(envelope.ended)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        ended,
        vec![Some(wire::IncarnationEnd {
            cause: wire::ExitCause::EndedUnexpectedly as i32,
            detail: None,
        })]
    );
    // The row says which one finished its work.
    assert_eq!(
        runtime.agent(finished.id).await.unwrap().exit_cause(),
        wire::ExitCause::Finished
    );
    assert_eq!(
        runtime.agent(crashed.id).await.unwrap().exit_cause(),
        wire::ExitCause::EndedUnexpectedly,
        "a process gone without writing its incarnation's end said nothing of why"
    );
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_stops_reading_holds_up_neither_other_parents_nor_a_kill() {
    const WRITE_MS: i64 = 300;
    const REPLY_MS: i64 = 1_500;
    const STOP_MS: i64 = 1_000;
    let install = Install::new();
    let mut wedged = SyntheticAgent::new(&install, "wedged", SEGMENTS);
    wedged.register_offline(&install);
    wedged.go_live();
    wedged.stop_reading();
    let mut other = SyntheticAgent::new(&install, "other", SEGMENTS);
    other.register_offline(&install);
    other.go_live();
    // Four children that finished while no daemon ran. The outbox is read
    // in child id order: the wedged parent's three rows come first.
    let mut children: Vec<_> = (0..4)
        .map(|_| SyntheticAgent::new(&install, "child", SEGMENTS))
        .collect();
    children.sort_by_key(|child| child.id);
    for (n, child) in children.iter_mut().enumerate() {
        let parent = if n < 3 { &wedged } else { &other };
        child.parent = Some(parent.key(&install));
        child.register_offline(&install);
        child.append(&snapshot(wire::Phase::Working, &[], 2_000));
        child.append(&item("last", "the tests pass"));
        child.append(&turn_end(1, "last"));
    }
    let launch = Launch {
        ctl_write_ms: WRITE_MS,
        reply_patience_ms: REPLY_MS,
        stop_deadline_ms: STOP_MS,
        ..quiet_launch()
    };
    let started = tokio::time::Instant::now();
    let daemon = install.start("boot-1", launch).await;
    let runtime = runtime(&daemon, &install);

    until("the other parent's delivery", || async { heard(&other, 1) })
        .await
        .unwrap();
    let waited = started.elapsed();
    assert!(
        waited < Duration::from_millis(2 * REPLY_MS as u64),
        "the wedged parent cost one lost answer, not one per row: {waited:?}"
    );
    assert_eq!(
        messages(&other),
        vec![(EnvelopeKind::Finished, "the tests pass".to_owned())]
    );
    // The other parent's row goes once its item commits; the wedged
    // parent's rows wait.
    until("only the wedged parent's rows", || rows(&runtime, 3))
        .await
        .unwrap();

    // An input too large for the socket's buffer blocks its write, and a
    // kill arrives while it is stuck.
    let stuck = tokio::spawn({
        let runtime = runtime.clone();
        let request = SendInputRequest {
            agent_id: wedged.id.as_bytes().to_vec(),
            input: Some(prompt(b"big", &"x".repeat(8 << 20))),
        };
        async move { runtime.send_input(&request).await }
    });
    // A forced delay: the send must be under way (its write wedged in the
    // agent's socket) when the kill arrives, and the wedge itself is what
    // leaves nothing to wait on.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let bound = Duration::from_millis((2 * WRITE_MS + STOP_MS) as u64 + 2_000);
    let stopped = tokio::time::timeout(bound, runtime.stop(wedged.id, StopMode::Kill))
        .await
        .expect("the kill returns within its deadlines");
    assert!(
        matches!(stopped, Err(RegistryError::StopTimeout(_))),
        "the agent is served from this process, which a kill must not end: {stopped:?}"
    );
    let stuck = tokio::time::timeout(bound, stuck)
        .await
        .expect("the stuck write gave up")
        .unwrap();
    assert!(matches!(stuck, Err(RelayError::Lost)), "{stuck:?}");
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_parents_message_resumes_its_exited_child_and_the_child_finishes_again() {
    let install = Install::new();
    let daemon = install
        .start(
            "boot-1",
            install.launch("oneshot", vec![text("done"), wire_turn_end()]),
        )
        .await;
    let runtime = runtime(&daemon, &install);
    let parent = id_of(
        &runtime
            .spawn(create(&install.work, "parent", None), None)
            .await
            .unwrap(),
    );
    let child = id_of(
        &runtime
            .spawn(create(&install.work, "child", Some("go")), Some(parent))
            .await
            .unwrap(),
    );
    let parent_key = AgentKey::new(
        runtime.host().as_bytes().to_vec(),
        parent.as_bytes().to_vec(),
    );
    let finished = |n: usize| {
        let runtime = runtime.clone();
        let parent_key = parent_key.clone();
        async move {
            let items = items_with_input(&runtime, &parent_key).await;
            (items.len() >= n)
                .then_some(())
                .ok_or_else(|| format!("{} of {n} finished messages", items.len()))
        }
    };
    until("the child's first finished message", || finished(1))
        .await
        .unwrap();
    until("the one-shot child to exit", || exited(&runtime, child))
        .await
        .unwrap();

    let envelope = SendMessageRequest {
        id: b"follow-up".to_vec(),
        to: Some(AgentRef {
            host_id: runtime.host().as_bytes().to_vec(),
            agent_id: child.as_bytes().to_vec(),
        }),
        text: "one more thing".into(),
        ..SendMessageRequest::default()
    };
    runtime
        .send_message(envelope, Some(parent))
        .await
        .expect("the parent's message resumes the child");
    let row = runtime.agent(child).await.unwrap();
    assert_eq!(row.incarnation, 2, "a new incarnation");
    let child_key = AgentKey::new(
        runtime.host().as_bytes().to_vec(),
        child.as_bytes().to_vec(),
    );
    assert!(
        runtime
            .store()
            .await
            .item_by_input(&child_key, b"follow-up")
            .unwrap()
            .is_some(),
        "accepted once the child's item for the message committed"
    );
    until("the child's second finished message", || finished(2))
        .await
        .unwrap();
    until("the child to exit again", || exited(&runtime, child))
        .await
        .unwrap();
    until("the outbox to drain", || rows(&runtime, 0))
        .await
        .unwrap();

    // Anyone but the parent is told the child has exited.
    let refused = runtime
        .send_message(
            SendMessageRequest {
                id: b"stranger".to_vec(),
                to: Some(AgentRef {
                    host_id: runtime.host().as_bytes().to_vec(),
                    agent_id: child.as_bytes().to_vec(),
                }),
                text: "hello?".into(),
                ..SendMessageRequest::default()
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(refused, RelayError::Rejected(ref rejected) if rejected.refusal() == wire::Refusal::Exited)
    );

    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

fn wire_turn_end() -> provider_fakes::script::Step {
    provider_fakes::script::Step::TurnEnd
}
