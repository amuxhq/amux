//! Inputs, agent messages and the deliveries outbox: a person's input
//! relayed with the interpreter's verdict, messages through the
//! recipient's lane, and a parent hearing once that its child finished or
//! failed across daemon crashes and resumes.
//!
//! Most agents here are synthetic (a directory, a journal and a control
//! socket speaking the agent's protocol), so each window can be opened on
//! purpose; the last test runs real agent processes on the fake provider.

use std::sync::Arc;
use std::time::Duration;

use node::{Daemon, Launch, ProfileRuntime, RegistryError, RelayError};
use store::AgentKey;
use support::synthetic::*;
use support::*;
use tonic::Code;
use wire::{
    AgentRef, ClaudeSdkInput, Envelope, Input, Lifecycle, PromptInput, SendInputRequest,
    SendInputResponse, claude_sdk_input, send_input_response, sender,
};

use crate::support;

const SEGMENTS: u64 = 1 << 20;

async fn start(install: &Install, boot: &str) -> (Daemon, Arc<ProfileRuntime>) {
    let daemon = install.start(boot, quiet_launch()).await;
    let runtime = runtime(&daemon, install);
    (daemon, runtime)
}

/// What a closed recipient's refusal of a message says.
fn closed() -> String {
    RelayError::Rejected(wire::Rejected {
        refusal: wire::Refusal::AgentClosed as i32,
        detail: None,
    })
    .to_string()
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

/// What an envelope's body is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Is {
    Message,
    TurnEnded,
    Exited,
}

/// What an envelope is, and its text: a message's or a turn report's,
/// empty for an exit report.
fn told(envelope: &wire::Envelope) -> (Is, String) {
    use wire::child_report::End;
    use wire::envelope::Body;
    match envelope.body.as_ref().expect("an envelope has a body") {
        Body::Message(message) => (Is::Message, message.text.clone()),
        Body::Report(report) => match report.end.as_ref().expect("a report has an end") {
            End::TurnEnded(turn) => (Is::TurnEnded, turn.text.clone()),
            End::Exited(_) => (Is::Exited, String::new()),
        },
    }
}

/// The agent messages an agent was handed, as what each is and its text.
fn messages(agent: &SyntheticAgent) -> Vec<(Is, String)> {
    agent
        .inputs()
        .into_iter()
        .filter_map(|handed| match handed {
            Handed::Message(envelope) => Some(told(&envelope)),
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
    runtime
        .read_store(|store| store.deliveries().unwrap().len())
        .await
        .unwrap()
}

async fn items_with_input(runtime: &ProfileRuntime, key: &AgentKey) -> Vec<wire::Item> {
    let key = key.clone();
    runtime
        .read_store(move |store| store.page(&key, None, 1_000).unwrap().items)
        .await
        .unwrap()
        .into_iter()
        .filter(|item| !item.input_id.is_empty())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn send_input_returns_the_interpreters_verdict_or_answers_for_an_exited_agent() {
    let install = Install::new();
    let mut live = SyntheticAgent::new(&install, "live", SEGMENTS);
    live.register(&install);
    live.go_live();
    let gone = SyntheticAgent::new(&install, "gone", SEGMENTS);
    gone.register(&install);
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
async fn a_message_is_accepted_once_its_item_commits_and_a_retry_is_deduped() {
    let install = Install::new();
    let sender = SyntheticAgent::new(&install, "sender", SEGMENTS);
    sender.register(&install);
    let mut recipient = SyntheticAgent::new(&install, "recipient", SEGMENTS);
    recipient.register(&install);
    recipient.go_live();
    let (daemon, runtime) = start(&install, "boot-1").await;

    let envelope = Envelope {
        id: b"envelope-1......".to_vec(),
        to: to(&recipient, &install),
        body: Some(wire::envelope::Body::Message(wire::MessageBody {
            text: "can you look at the build?".into(),
            ..Default::default()
        })),
        ..Envelope::default()
    };
    let sent = runtime
        .send_message(envelope.clone(), Some(sender.id))
        .await
        .unwrap();
    assert_eq!(sent.envelope_id, b"envelope-1......");
    let key = recipient.key(&install);
    let accepted = runtime
        .read_store(move |store| store.item_by_input(&key, b"envelope-1......").unwrap())
        .await
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
            assert_eq!(
                from.agent.as_ref().unwrap().agent_id,
                sender.id.as_bytes().to_vec()
            );
            assert_eq!(from.name, "sender");
        }
        other => panic!("the sender is the calling agent, not {other:?}"),
    }
    assert_eq!(told(handed).0, Is::Message);

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

    // An exited recipient the person settled, and a sender that is not its
    // parent.
    recipient.die().await;
    until("the recipient's exit", || exited(&runtime, recipient.id))
        .await
        .unwrap();
    runtime
        .file(recipient.id, daemon_core::Filing::Closed)
        .await
        .unwrap();
    let refused = runtime
        .send_message(
            Envelope {
                id: b"envelope-2......".to_vec(),
                to: to(&recipient, &install),
                body: Some(wire::envelope::Body::Message(wire::MessageBody {
                    text: "still there?".into(),
                    ..Default::default()
                })),
                ..Envelope::default()
            },
            Some(sender.id),
        )
        .await
        .unwrap_err();
    assert!(
        refused.code() == Code::FailedPrecondition && refused.message() == closed(),
        "{refused:?}"
    );
    crash(daemon, runtime).await;
}

/// A child whose journal already holds a finished turn: its last message,
/// then the turn end.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_waits_for_its_item_to_commit_and_one_envelope_is_handed_over_once() {
    let install = Install::new();
    let mut recipient = SyntheticAgent::new(&install, "recipient", SEGMENTS);
    recipient.register(&install);
    recipient.go_live();
    recipient.answer_with(Answer::AcceptWithoutNudge);
    let (daemon, runtime) = start(&install, "boot-1").await;
    let envelope = Envelope {
        id: b"envelope-1......".to_vec(),
        to: to(&recipient, &install),
        body: Some(wire::envelope::Body::Message(wire::MessageBody {
            text: "can you look at the build?".into(),
            ..Default::default()
        })),
        ..Envelope::default()
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

/// A message whose verdict comes after reply patience is concluded lost;
/// the sender's retry is written again and concluded by the verdict that
/// comes, not lost on arrival because the first write stopped waiting.
/// Reply patience is policy time, so the clock is driven: nothing but the
/// test moves it past the patience.
#[tokio::test(flavor = "multi_thread")]
async fn a_retry_after_reply_patience_is_concluded_by_the_verdict_that_comes() {
    const T0: i64 = 1_000_000;
    const REPLY_MS: i64 = 400;
    let install = Install::new();
    let mut recipient = SyntheticAgent::new(&install, "recipient", SEGMENTS);
    recipient.register(&install);
    recipient.go_live();
    recipient.freeze();
    let clock = agent_dir::ManualClock::new(T0);
    let mut launch = quiet_launch();
    launch.core.reply_patience_ms = REPLY_MS as u64;
    let daemon = node::start(
        node::StartOptions {
            settings: Default::default(),
            clock: Arc::new(clock.clone()),
            ..install.options("boot-1", launch)
        },
        None,
    )
    .await
    .unwrap();
    let runtime = runtime(&daemon, &install);
    let envelope = Envelope {
        id: b"envelope-1......".to_vec(),
        to: to(&recipient, &install),
        body: Some(wire::envelope::Body::Message(wire::MessageBody {
            text: "can you look at the build?".into(),
            ..Default::default()
        })),
        ..Envelope::default()
    };

    let first = tokio::spawn({
        let runtime = runtime.clone();
        let envelope = envelope.clone();
        async move { runtime.send_message(envelope, None).await }
    });
    // The frozen recipient takes the message in and answers nothing.
    until("the message handed over", || async {
        handed(&recipient, 1)
    })
    .await
    .unwrap();
    // The core armed its patience as it wrote the message, at the time
    // it wrote it, which only the test moves.
    clock.set(T0 + REPLY_MS + 1);
    let first = first.await.unwrap();
    assert!(
        matches!(&first, Err(status) if status.code() == Code::Aborted),
        "{first:?}"
    );
    until(
        "the first write to stop waiting for its verdict",
        || async {
            (!runtime.in_hand(recipient.id))
                .then_some(())
                .ok_or("still in hand")
        },
    )
    .await
    .unwrap();

    // The retry is written to the recipient, which reads it once it has
    // answered the first; the verdict that comes answers the retry, which
    // a first write reporting no verdict as it stopped waiting would
    // conclude lost. Thawed only once the retry is written: thawed before,
    // the recipient's item can commit first and the retry, finding it, is
    // never written.
    let retry = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.send_message(envelope, None).await }
    });
    until("the retry written", || async {
        runtime
            .in_hand(recipient.id)
            .then_some(())
            .ok_or("not in hand")
    })
    .await
    .unwrap();
    recipient.thaw();
    retry
        .await
        .unwrap()
        .expect("the retry is accepted on the verdict that came");
    until("the retry written again", || async {
        handed(&recipient, 2)
    })
    .await
    .unwrap();
    crash(daemon, runtime).await;
}

/// A message waits for its recipient to connect for at most the connect
/// patience. When that patience runs out just as the recipient's Hello
/// comes, the firing can wait behind the Hello, whose step writes the
/// message and replaces the patience with the reply patience. The firing
/// is of an arm the core replaced: the message is answered by its
/// verdict, not concluded lost.
#[tokio::test(flavor = "multi_thread")]
async fn connect_patience_running_out_behind_the_hello_does_not_lose_the_message() {
    let install = Install::new();
    let mut recipient = SyntheticAgent::new(&install, "recipient", SEGMENTS);
    recipient.register(&install);
    // Running but not answering yet: the daemon keeps dialing it.
    recipient.hold_lock();
    let (daemon, runtime) = start(&install, "boot-1").await;
    let envelope = Envelope {
        id: b"envelope-1......".to_vec(),
        to: to(&recipient, &install),
        body: Some(wire::envelope::Body::Message(wire::MessageBody {
            text: "can you look at the build?".into(),
            ..Default::default()
        })),
        ..Envelope::default()
    };
    let sent = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.send_message(envelope, None).await }
    });
    // A read queued behind the call returns once the call is stepped and
    // its connect patience armed.
    tokio::time::sleep(Duration::from_millis(200)).await;
    runtime.read_store(|_| ()).await.unwrap();
    let armed = tokio::time::Instant::now();

    // A read waiting on `release` holds the core while the Hello is
    // queued and then the patience fires behind it.
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let mut hold = Box::pin(runtime.read_store(move |_| gate.recv().is_ok()));
    assert!(poll_once(&mut hold).await.is_pending());
    recipient.go_live();
    until("the recipient's Hello", || async {
        (recipient.hellos() == 1)
            .then_some(())
            .ok_or("no Hello yet")
    })
    .await
    .unwrap();
    tokio::time::sleep_until(
        armed + Duration::from_millis(daemon_core::deliveries::CONNECT_PATIENCE_MS + 1_000),
    )
    .await;
    release.send(()).unwrap();
    assert!(hold.await.unwrap());

    sent.await
        .unwrap()
        .expect("the message is accepted on its verdict, not concluded lost");
    assert_eq!(recipient.inputs().len(), 1, "the message was written once");
    crash(daemon, runtime).await;
}

/// Polls a future once, as far as it gets before it waits.
async fn poll_once<F: std::future::Future + Unpin>(future: &mut F) -> std::task::Poll<F::Output> {
    std::future::poll_fn(|cx| std::task::Poll::Ready(std::pin::Pin::new(&mut *future).poll(cx)))
        .await
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
    parent.register(&install);
    let mut child = SyntheticAgent::new(&install, "child", SEGMENTS);
    child.parent = Some(parent.key(&install));
    child.register(&install);
    child.write_spec();
    child.go_live();
    child.answer_with(Answer::Reject(wire::Refusal::Exiting));
    let (daemon, runtime) = start_resuming(&install).await;

    let envelope = Envelope {
        id: b"wrap-up.........".to_vec(),
        to: to(&child, &install),
        body: Some(wire::envelope::Body::Message(wire::MessageBody {
            text: "wrap up and report".into(),
            ..Default::default()
        })),
        ..Envelope::default()
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
            assert_eq!(message.id, b"wrap-up.........");
            assert_eq!(
                told(&message),
                (Is::Message, "wrap up and report".to_owned())
            );
        }
        other => panic!("the parent's message, not {other:?}"),
    }
    let key = child.key(&install);
    assert!(
        runtime
            .read_store(move |store| store
                .item_by_input(&key, b"wrap-up.........")
                .unwrap()
                .is_some())
            .await
            .unwrap(),
        "the new incarnation accepted it"
    );
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// An agent refusing a person's input as exiting with no stop asked of it
/// is leaving of its own accord: the input waits for the exit, and an exit
/// that was no suspension leaves it refused as exited. The agent can then
/// be resumed as any exited one.
#[tokio::test(flavor = "multi_thread")]
async fn an_input_answered_exiting_waits_for_the_exit_and_is_refused_as_exited() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "leaving", SEGMENTS);
    agent.register(&install);
    agent.write_spec();
    agent.go_live();
    agent.answer_with(Answer::Reject(wire::Refusal::Exiting));
    let (daemon, runtime) = start_resuming(&install).await;

    let sent = tokio::spawn({
        let runtime = runtime.clone();
        let request = SendInputRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            input: Some(prompt(b"i1", "one more thing")),
        };
        async move { runtime.send_input(&request).await }
    });
    // A window: an input waiting for the exit leaves no mark to wait on.
    holds_for("the input to wait", Duration::from_millis(300), || async {
        !sent.is_finished()
    })
    .await
    .expect("the input waits for the agent's own exit instead of being refused");
    agent.die().await;
    let answer = sent.await.unwrap().unwrap();
    assert_eq!(verdict(&answer), "rejected Exited");
    let resumed = runtime
        .resume(agent.id, None)
        .await
        .expect("resumed once it is gone");
    assert_eq!(resumed.incarnation, 2);
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// Input to an agent the daemon is stopping, refused as exiting, is
/// answered so at once rather than held for the stop's deadline; a resume
/// asked meanwhile waits for the stop and the lock instead of refusing a
/// live agent.
#[tokio::test(flavor = "multi_thread")]
async fn an_input_to_an_agent_being_stopped_is_answered_at_once_and_a_resume_waits() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "leaving", SEGMENTS);
    agent.register(&install);
    agent.write_spec();
    agent.go_live();
    agent.answer_with(Answer::Reject(wire::Refusal::Exiting));
    let (daemon, runtime) = start_resuming(&install).await;
    until("the daemon to connect", || async {
        (agent.hellos() > 0)
            .then_some(())
            .ok_or_else(|| "no Hello".to_owned())
    })
    .await
    .unwrap();

    let stopped = tokio::spawn({
        let runtime = runtime.clone();
        let id = agent.id;
        async move { runtime.kill_agent(id).await }
    });
    until("the stop to reach the agent", || async {
        (agent.stops_asked() > 0)
            .then_some(())
            .ok_or_else(|| "no stop yet".to_owned())
    })
    .await
    .unwrap();
    let answer = tokio::time::timeout(
        Duration::from_secs(2),
        runtime.send_input(&SendInputRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            input: Some(prompt(b"i1", "one more thing")),
        }),
    )
    .await
    .expect("answered at once, not after the stop's deadline")
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
    stopped.await.unwrap().expect("the stop is answered");
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
    parent.register(&install);
    parent.go_live();
    let mut child = SyntheticAgent::new(&install, "child", SEGMENTS);
    child.parent = Some(parent.key(&install));
    child.register(&install);
    child.go_live();
    let (daemon, runtime) = start(&install, "boot-1").await;

    // A person's message holds the parent's lane until its item commits.
    parent.answer_with(Answer::AcceptWithoutNudge);
    let held = tokio::spawn({
        let runtime = runtime.clone();
        let envelope = Envelope {
            id: b"hold............".to_vec(),
            to: to(&parent, &install),
            body: Some(wire::envelope::Body::Message(wire::MessageBody {
                text: "a word".into(),
                ..Default::default()
            })),
            ..Envelope::default()
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
    let key = parent.key(&install);
    {
        let mut store = install.store();
        let mut row = store.agent(&key).unwrap().unwrap();
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
        vec![(Is::Message, "a word".to_owned())],
        "the new incarnation never hears of the old one's child"
    );
    crash(daemon, runtime).await;
}

fn finished_child(install: &Install, parent: &SyntheticAgent) -> SyntheticAgent {
    let mut child = SyntheticAgent::new(install, "child", SEGMENTS);
    child.parent = Some(parent.key(install));
    child.register(install);
    child.append(&snapshot(wire::Phase::Working, &[], 2_000));
    child.append(&item("last", "the tests pass"));
    child.append(&turn_end(1, "last"));
    child
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_finishing_while_the_daemon_is_down_reaches_its_parent_once() {
    let install = Install::new();
    let mut parent = SyntheticAgent::new(&install, "parent", SEGMENTS);
    parent.register(&install);
    parent.go_live();
    let child = finished_child(&install, &parent);
    // The child finished and exited while no daemon ran.
    let (daemon, runtime) = start(&install, "boot-1").await;
    until("the delivery", || rows(&runtime, 0)).await.unwrap();
    assert_eq!(
        messages(&parent),
        vec![(Is::TurnEnded, "the tests pass".to_owned())],
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
    parent.register(&install);
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

/// A child's report is for the parent incarnation the child last heard
/// from, as its row records it, whatever the parent's row says now: one
/// for that incarnation reaches it, stamped so.
#[tokio::test(flavor = "multi_thread")]
async fn a_childs_report_is_for_the_parent_incarnation_it_last_heard_from() {
    let install = Install::new();
    let mut parent = SyntheticAgent::new(&install, "parent", SEGMENTS);
    parent.incarnation = 3;
    parent.register(&install);
    parent.go_live();
    let mut child = SyntheticAgent::new(&install, "child", SEGMENTS);
    child.parent = Some(parent.key(&install));
    child.parent_incarnation = 3;
    child.register(&install);
    child.append(&snapshot(wire::Phase::Working, &[], 2_000));
    child.append(&item("last", "the tests pass"));
    child.append(&turn_end(1, "last"));
    let (daemon, runtime) = start(&install, "boot-1").await;
    until("the delivery", || rows(&runtime, 0)).await.unwrap();
    let stamped: Vec<u32> = parent
        .inputs()
        .into_iter()
        .filter_map(|handed| match handed {
            Handed::Message(envelope) => match envelope.body {
                Some(wire::envelope::Body::Report(report)) => Some(report.to_incarnation),
                _ => None,
            },
            Handed::Input(_) => None,
        })
        .collect();
    assert_eq!(stamped, vec![3], "for the incarnation the child heard from");
    crash(daemon, runtime).await;
}

/// A parent stopped and resumed by the person is a new run: a child its
/// earlier life engaged, and the new one never did, reports to the earlier
/// life, which is gone, so the report is dropped and the resumed parent is
/// handed nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_parent_receives_nothing_from_a_child_of_its_earlier_life() {
    let install = Install::new();
    let mut parent = SyntheticAgent::new(&install, "parent", SEGMENTS);
    parent.incarnation = 2;
    parent.register(&install);
    parent.go_live();
    // The child last heard from the parent's first incarnation.
    finished_child(&install, &parent);
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
async fn a_child_gone_without_a_turn_end_tells_its_parent_it_failed() {
    let install = Install::new();
    let mut parent = SyntheticAgent::new(&install, "parent", SEGMENTS);
    parent.register(&install);
    parent.go_live();
    let mut crashed = SyntheticAgent::new(&install, "crashed", SEGMENTS);
    crashed.parent = Some(parent.key(&install));
    crashed.register(&install);
    crashed.append(&snapshot(wire::Phase::Working, &[], 2_000));
    crashed.go_live();
    let mut finished = SyntheticAgent::new(&install, "finished", SEGMENTS);
    finished.parent = Some(parent.key(&install));
    finished.register(&install);
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
            (Is::TurnEnded, "done".to_owned()),
            (Is::Exited, String::new()),
        ],
        "one thing per event: finished once, failed only for the child that never finished"
    );
    // The failed report carries the cause as data, for the parent's
    // interpreter to word.
    let ended = parent
        .inputs()
        .into_iter()
        .filter_map(|handed| match handed {
            Handed::Message(envelope) => match envelope.body {
                Some(wire::envelope::Body::Report(wire::ChildReport {
                    end: Some(wire::child_report::End::Exited(exited)),
                    ..
                })) => Some(exited.ended),
                _ => None,
            },
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
    // Each report names the child incarnation it is from, and is that
    // incarnation's last (an exit report always is): the parent matches it
    // whichever comes first, the report or its daemon's word of the
    // incarnation.
    let reports = parent
        .inputs()
        .into_iter()
        .filter_map(|handed| match handed {
            Handed::Message(envelope) => match envelope.body {
                Some(wire::envelope::Body::Report(report)) => Some((
                    report.from_incarnation,
                    match report.end {
                        Some(wire::child_report::End::TurnEnded(turn)) => turn.last,
                        Some(wire::child_report::End::Exited(_)) => true,
                        None => false,
                    },
                )),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(reports, vec![(1, true), (1, true)]);
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
    wedged.register(&install);
    wedged.go_live();
    wedged.stop_reading();
    let mut other = SyntheticAgent::new(&install, "other", SEGMENTS);
    other.register(&install);
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
        child.register(&install);
        child.append(&snapshot(wire::Phase::Working, &[], 2_000));
        child.append(&item("last", "the tests pass"));
        child.append(&turn_end(1, "last"));
    }
    let mut launch = Launch {
        ctl_write_ms: WRITE_MS,
        ..quiet_launch()
    };
    launch.core.reply_patience_ms = REPLY_MS as u64;
    launch.core.stop_deadline_ms = STOP_MS as u64;
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
        vec![(Is::TurnEnded, "the tests pass".to_owned())]
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
    let stopped = tokio::time::timeout(bound, runtime.kill_agent(wedged.id))
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

    let envelope = Envelope {
        id: b"follow-up.......".to_vec(),
        to: Some(AgentRef {
            host_id: runtime.host().as_bytes().to_vec(),
            agent_id: child.as_bytes().to_vec(),
        }),
        body: Some(wire::envelope::Body::Message(wire::MessageBody {
            text: "one more thing".into(),
            ..Default::default()
        })),
        ..Envelope::default()
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
            .read_store(move |store| {
                store
                    .item_by_input(&child_key, b"follow-up.......")
                    .unwrap()
                    .is_some()
            })
            .await
            .unwrap(),
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

    // The one-shot child finished, so it settled itself: another agent is
    // told so rather than waking it.
    let stranger = id_of(
        &runtime
            .spawn(create(&install.work, "stranger", None), None)
            .await
            .unwrap(),
    );
    let refused = runtime
        .send_message(
            Envelope {
                id: b"stranger........".to_vec(),
                to: Some(AgentRef {
                    host_id: runtime.host().as_bytes().to_vec(),
                    agent_id: child.as_bytes().to_vec(),
                }),
                body: Some(wire::envelope::Body::Message(wire::MessageBody {
                    text: "hello?".into(),
                    ..Default::default()
                })),
                ..Envelope::default()
            },
            Some(stranger),
        )
        .await
        .unwrap_err();
    assert!(
        refused.code() == Code::FailedPrecondition && refused.message() == closed(),
        "{refused:?}"
    );

    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

fn wire_turn_end() -> provider_fakes::script::Step {
    provider_fakes::script::Step::TurnEnd
}
