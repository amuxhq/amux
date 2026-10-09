//! The session driver against an in-memory runtime: every call it makes is
//! answered by hand, and time moves only when the test moves it.

mod support;

use std::sync::Arc;

use client::{ManualClock, RpcError};
use support::*;
use ui_runtime::{DriverEvent, InputError, PageError, Session, TRACE_EVENTS, TraceEvent};
use ui_state::{Composer, Connection, InputOutcome, InputState, RefusalReason, Waiting};
use wire::{
    ErrorCode, FetchResponse, GetBlobResponse, Input, Kind, SendInputRequest, claude_pty_input,
    claude_sdk_input, codex_input,
};

const KIND: Kind = Kind::ClaudeSdk;
/// The window's cap in cases about something else.
const CAP: u32 = 200;

/// Opens a session and brings it to caught up with `rows` held rows.
async fn open_caught_up(
    clock: &ManualClock,
    rows: u64,
) -> (Arc<Session>, Calls, Feed<wire::SessionEvent>) {
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Session::open(client, agent(KIND), 40, CAP, clock.clone()));
    let (_, feed) = calls.subscribe().await;
    feed.send(snapshot(KIND, rows, &[]));
    for order in 1..=rows {
        feed.send(ev(text_item(KIND, order, order, &format!("row {order}"))));
    }
    feed.send(caught_up(rows));
    let session = Arc::new(open.await.unwrap().expect("the session opens"));
    until(session.changed(), || {
        session.state().caught_up().then_some(())
    })
    .await;
    (session, calls, feed)
}

/// Ends the stream and lets the first backoff pass; returns the new feed.
async fn reconnect(
    clock: &ManualClock,
    session: &Session,
    calls: &mut Calls,
    feed: Feed<wire::SessionEvent>,
) -> Feed<wire::SessionEvent> {
    drop(feed);
    let now = clock_now(clock);
    clock.armed(now + 250).await;
    assert_eq!(session.state().connection(), Connection::Reconnecting);
    clock.advance(250);
    let (_, feed) = calls.subscribe().await;
    feed
}

fn clock_now(clock: &ManualClock) -> i64 {
    use client::Clock as _;
    clock.now_ms()
}

#[tokio::test]
async fn open_paints_the_snapshot_and_held_rows_before_it_returns() {
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Session::open(
        client,
        agent(KIND),
        40,
        CAP,
        ManualClock::new(0),
    ));
    let (request, feed) = calls.subscribe().await;
    assert_eq!(request.tail, 40, "a client asks for a tail");
    feed.send(snapshot(KIND, 7, &[]));
    feed.send(ev(text_item(KIND, 6, 6, "older")));
    feed.send(ev(text_item(KIND, 7, 7, "newest")));
    let session = open.await.unwrap().expect("the session opens");
    {
        let state = session.state();
        assert!(state.has_snapshot());
        assert_eq!(state.transcript().len(), 2, "held rows are applied");
        assert!(!state.caught_up(), "the origin has not been reached");
        assert_eq!(state.composer(), Composer::Disabled(Waiting::CatchingUp));
    }
    feed.send(caught_up(7));
    until(session.changed(), || {
        session.state().caught_up().then_some(())
    })
    .await;
    assert_eq!(session.state().composer(), Composer::Send);
}

#[tokio::test]
async fn an_away_host_paints_its_held_rows_then_detached_with_send_disabled() {
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Session::open(
        client,
        agent(KIND),
        40,
        CAP,
        ManualClock::new(0),
    ));
    let (_, feed) = calls.subscribe().await;
    feed.send(snapshot(KIND, 2, &[]));
    feed.send(ev(text_item(KIND, 1, 1, "cached")));
    feed.send(ev(text_item(KIND, 2, 2, "cached too")));
    feed.send(detached());
    let session = open.await.unwrap().unwrap();
    let state = session.state();
    assert_eq!(state.transcript().len(), 2, "the rows stay");
    assert!(!state.can_send());
    assert_eq!(state.composer(), Composer::Disabled(Waiting::Detached));
}

#[tokio::test]
async fn lagged_reopens_with_a_tail_at_once_and_dedupes_the_overlap() {
    let clock = ManualClock::new(0);
    let (session, mut calls, feed) = open_caught_up(&clock, 3).await;
    feed.send(lagged());
    let (request, feed) = calls.subscribe().await;
    assert_eq!(request.tail, 40);
    assert!(clock.sleeping().is_empty(), "no backoff after Lagged");
    assert!(!session.state().caught_up());
    assert_eq!(session.state().connection(), Connection::Live);
    feed.send(snapshot(KIND, 4, &[]));
    for order in 2..=4 {
        feed.send(ev(text_item(KIND, order, order, &format!("row {order}"))));
    }
    feed.send(caught_up(4));
    until(session.changed(), || {
        session.state().caught_up().then_some(())
    })
    .await;
    let keys: Vec<_> = session.state().transcript().keys().cloned().collect();
    assert_eq!(
        keys,
        ["k1", "k2", "k3", "k4"],
        "the overlap is deduped by key"
    );
    let trace = session.state().trace();
    assert!(
        trace
            .events
            .iter()
            .any(|traced| traced.event == TraceEvent::Driver(DriverEvent::Retail))
    );
}

#[tokio::test]
async fn the_end_of_the_stream_reconnects_on_the_sessions_clock_with_doubling_backoff() {
    let clock = ManualClock::new(10_000);
    let (session, mut calls, feed) = open_caught_up(&clock, 1).await;
    drop(feed);
    clock.armed(10_250).await;
    assert_eq!(session.state().connection(), Connection::Reconnecting);
    assert!(!session.state().caught_up());
    calls.none().await;
    clock.advance(250);
    calls.refuse_subscribe(transport()).await;
    clock.armed(10_750).await;
    calls.none().await;
    clock.advance(500);
    let (_, feed) = calls.subscribe().await;
    until(session.changed(), || {
        (session.state().connection() == Connection::Live).then_some(())
    })
    .await;
    feed.send(snapshot(KIND, 1, &[]));
    feed.send(caught_up(1));
    until(session.changed(), || {
        session.state().caught_up().then_some(())
    })
    .await;
    // Caught up again: the next outage starts from the first wait.
    drop(feed);
    clock.armed(11_000).await;
}

#[tokio::test]
async fn an_uncertain_input_is_settled_from_the_queue_or_the_items_and_otherwise_left() {
    // The connection dropping with the input in flight, and the daemon
    // saying it handed the input on and lost the agent's answer, are the
    // same uncertainty.
    for (lost, found) in ["transport", "answer lost"]
        .into_iter()
        .flat_map(|lost| ["queue", "items", "nowhere"].map(|found| (lost, found)))
    {
        let clock = ManualClock::new(0);
        let (session, mut calls, feed) = open_caught_up(&clock, 2).await;
        let sending = tokio::spawn({
            let session = session.clone();
            async move { session.send_prompt("hello", Vec::new()).await }
        });
        let (request, reply) = calls.send_input().await;
        let id = request.input.unwrap().input_id;
        let error = match lost {
            "transport" => transport(),
            _ => RpcError::Refused(wire::Error {
                code: ErrorCode::Aborted as i32,
                message: "the agent's answer was lost; the input may or may not have arrived"
                    .into(),
                details: Vec::new(),
            }),
        };
        reply.send(Err(error)).ok();
        let sent = sending.await.unwrap();
        assert_eq!(sent.outcome, InputOutcome::Lost, "{lost}");
        assert_eq!(
            session.state().input_state(&id),
            Some(InputState::Uncertain)
        );
        let feed = reconnect(&clock, &session, &mut calls, feed).await;
        let queue: &[&[u8]] = if found == "queue" { &[&id] } else { &[] };
        feed.send(snapshot(KIND, 3, queue));
        feed.send(ev(text_item(KIND, 2, 2, "row 2")));
        if found == "items" {
            feed.send(ev(reflection(KIND, 3, 3, &id)));
        }
        // Before CaughtUp nothing is judged.
        assert_eq!(
            session.state().input_state(&id),
            Some(InputState::Uncertain),
            "{found}"
        );
        feed.send(caught_up(3));
        until(session.changed(), || {
            session.state().caught_up().then_some(())
        })
        .await;
        let expected = match found {
            "queue" => InputState::Queued,
            "items" => InputState::Settled,
            _ => InputState::Uncertain,
        };
        assert_eq!(
            session.state().input_state(&id),
            Some(expected),
            "{lost}: {found}"
        );
        if found == "nowhere" {
            assert_eq!(session.state().not_confirmed().count(), 1);
        }
        calls.none().await;
        drop(feed);
    }
}

#[tokio::test]
async fn page_older_asks_below_the_oldest_held_row_and_merges_under_the_window() {
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Session::open(
        client,
        agent(KIND),
        2,
        CAP,
        ManualClock::new(0),
    ));
    let (_, feed) = calls.subscribe().await;
    feed.send(snapshot(KIND, 6, &[]));
    feed.send(ev(text_item(KIND, 5, 5, "five")));
    feed.send(ev(text_item(KIND, 6, 6, "six")));
    feed.send(caught_up(6));
    let session = Arc::new(open.await.unwrap().unwrap());
    let paging = tokio::spawn({
        let session = session.clone();
        async move { session.page_older(3).await }
    });
    let (request, reply) = calls.fetch().await;
    assert_eq!(request.before_order, Some(5));
    assert_eq!(request.limit, 3);
    let items = (2..=4)
        .rev()
        .map(|order| text_item(KIND, order, order, "older"))
        .collect();
    reply
        .send(Ok(FetchResponse {
            items,
            exhausted: false,
        }))
        .ok();
    assert_eq!(paging.await.unwrap(), Ok(3));
    let state = session.state();
    assert_eq!(state.oldest_order(), Some(2));
    assert_eq!(state.transcript().len(), 5);
    assert!(state.transcript().has_older());
    drop(feed);
}

#[tokio::test]
async fn an_unreachable_origin_is_an_error_never_an_empty_page() {
    let clock = ManualClock::new(0);
    let (session, mut calls, _feed) = open_caught_up(&clock, 3).await;
    let paging = tokio::spawn({
        let session = session.clone();
        async move { session.page_older(40).await }
    });
    let (_, reply) = calls.fetch().await;
    reply.send(Err(unreachable())).ok();
    assert_eq!(paging.await.unwrap(), Err(PageError::OriginUnreachable));
    assert_eq!(session.state().transcript().len(), 3);
}

#[tokio::test]
async fn an_append_whose_base_is_not_held_is_answered_with_get() {
    let clock = ManualClock::new(0);
    let (session, mut calls, feed) = open_caught_up(&clock, 2).await;
    feed.send(append("k2", 5, 6, " more"));
    let (request, reply) = calls.get().await;
    assert_eq!(request.key, "k2");
    reply
        .send(Ok(text_item(KIND, 2, 6, "row 2 as of revision 6")))
        .ok();
    feed.send(append("k2", 6, 7, "!"));
    until(session.changed(), || {
        let state = session.state();
        let held = state.transcript().get("k2")?;
        (held.item.revision == 7).then(|| held.item.text.clone())
    })
    .await;
    assert_eq!(
        session.state().transcript().get("k2").unwrap().item.text,
        "row 2 as of revision 6!"
    );
}

/// An ask's answer as a card makes it, under the ask's key: for Codex an
/// approval.
fn answering(kind: Kind) -> Input {
    let answer = wire::ClaudeAnswer {
        ask_key: "ask-1".into(),
        of: None,
    };
    let of = match kind {
        Kind::ClaudePty => wire::KindInput::ClaudePty(wire::ClaudePtyInput {
            of: Some(claude_pty_input::Of::Answer(answer)),
        }),
        Kind::ClaudeSdk => wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(claude_sdk_input::Of::Answer(answer)),
        }),
        _ => wire::KindInput::Codex(wire::CodexInput {
            of: Some(codex_input::Of::Answer(wire::CodexAnswer {
                ask_key: "ask-1".into(),
                of: Some(wire::codex_answer::Of::Approve(
                    wire::Decision::Approve as i32,
                )),
            })),
        }),
    };
    Input::new(Vec::new(), of)
}

fn act_arm(request: &SendInputRequest) -> String {
    let of = request.input.as_ref().unwrap().decode_body();
    match of.as_ref().unwrap() {
        wire::KindInput::ClaudePty(pty) => match pty.of.as_ref().unwrap() {
            claude_pty_input::Of::Withdraw(_) => "withdraw",
            claude_pty_input::Of::SendNow(_) => "send_now",
            claude_pty_input::Of::Interrupt(_) => "interrupt",
            claude_pty_input::Of::Answer(_) => "answer",
            _ => "other",
        },
        wire::KindInput::ClaudeSdk(sdk) => match sdk.of.as_ref().unwrap() {
            claude_sdk_input::Of::Withdraw(_) => "withdraw",
            claude_sdk_input::Of::SendNow(_) => "send_now",
            claude_sdk_input::Of::Interrupt(_) => "interrupt",
            claude_sdk_input::Of::Answer(_) => "answer",
            _ => "other",
        },
        wire::KindInput::Codex(codex) => match codex.of.as_ref().unwrap() {
            codex_input::Of::Withdraw(_) => "withdraw",
            codex_input::Of::SendNow(_) => "send_now",
            codex_input::Of::Interrupt(_) => "interrupt",
            codex_input::Of::Answer(_) => "answer",
            _ => "other",
        },
    }
    .into()
}

#[tokio::test]
async fn acts_on_the_chat_go_in_the_kinds_own_arm_and_return_the_verdict() {
    for kind in [Kind::ClaudePty, Kind::ClaudeSdk, Kind::Codex] {
        let (client, mut calls) = runtime();
        let open = tokio::spawn(Session::open(
            client,
            agent(kind),
            40,
            CAP,
            ManualClock::new(0),
        ));
        let (_, feed) = calls.subscribe().await;
        feed.send(snapshot(kind, 1, &[b"queued-1"]));
        feed.send(caught_up(1));
        let session = Arc::new(open.await.unwrap().unwrap());
        for (act, verdict) in [
            ("withdraw", Ok(accepted(false))),
            ("send_now", Ok(accepted(false))),
            ("interrupt", Ok(rejected("unsupported"))),
            ("answer", Err(transport())),
        ] {
            let acting = tokio::spawn({
                let session = session.clone();
                async move {
                    match act {
                        "withdraw" => session.withdraw(b"queued-1").await,
                        "send_now" => session.send_now(b"queued-1").await,
                        "interrupt" => session.interrupt().await,
                        _ => session.answer(answering(kind)).await,
                    }
                }
            });
            let (request, reply) = calls.send_input().await;
            assert_eq!(act_arm(&request), act, "{kind:?}");
            if act == "send_now" {
                // Steering: the queued row reads steered while it is sent.
                let state = session.state();
                let row = &state.queue()[0];
                assert!(
                    row.steered,
                    "{kind:?}: a queued prompt sent now reads steered"
                );
            }
            let expected = match &verdict {
                Ok(response) if *response == accepted(false) => Ok(()),
                Ok(_) => Err(InputError::Rejected(RefusalReason::Unsupported)),
                Err(_) => Err(InputError::Uncertain),
            };
            reply.send(verdict).ok();
            assert_eq!(acting.await.unwrap(), expected, "{kind:?} {act}");
        }
        drop(feed);
    }
}

#[tokio::test]
async fn the_exited_composer_resumes_with_the_draft_as_the_first_prompt() {
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Session::open(
        client,
        exited(agent(KIND)),
        40,
        CAP,
        ManualClock::new(0),
    ));
    let (_, feed) = calls.subscribe().await;
    feed.send(snapshot(KIND, 1, &[]));
    feed.send(caught_up(1));
    let session = Arc::new(open.await.unwrap().unwrap());
    assert_eq!(session.state().composer(), Composer::Resume);

    // A failed resume keeps nothing: the draft is still the composer's.
    let draft = ui_runtime::inputs::submission("carry on", Vec::new());
    let resuming = tokio::spawn({
        let (session, draft) = (session.clone(), draft.clone());
        async move { session.resume_with(draft).await }
    });
    let (_, reply) = calls.resume().await;
    reply.send(Err(transport())).ok();
    assert!(resuming.await.unwrap().is_err());
    assert_eq!(session.state().inputs().iter().count(), 0);
    assert_eq!(session.state().composer(), Composer::Resume);

    let resuming = tokio::spawn({
        let (session, draft) = (session.clone(), draft.clone());
        async move { session.resume_with(draft).await }
    });
    let (request, reply) = calls.resume().await;
    assert_eq!(request.initial_prompt.as_ref(), Some(&draft));
    let mut live = agent(KIND);
    live.incarnation = 2;
    reply.send(Ok(live)).ok();
    resuming.await.unwrap().expect("resumed");
    let state = session.state();
    assert_eq!(state.composer(), Composer::Send);
    assert_eq!(state.sending().count(), 1, "the draft shows until it lands");
    drop(feed);
}

#[tokio::test]
async fn a_send_that_raced_the_exit_is_rejected_and_the_composer_offers_resume() {
    let clock = ManualClock::new(0);
    let (session, mut calls, _feed) = open_caught_up(&clock, 1).await;
    let sending = tokio::spawn({
        let session = session.clone();
        async move { session.send_prompt("too late", Vec::new()).await }
    });
    let (_, reply) = calls.send_input().await;
    reply.send(Ok(rejected("exited"))).ok();
    let sent = sending.await.unwrap();
    assert_eq!(
        session.state().input_state(&sent.id),
        Some(InputState::Rejected(RefusalReason::Exited))
    );
    assert_eq!(session.state().composer(), Composer::Resume);
}

#[tokio::test]
async fn a_refused_call_is_a_rejection_never_uncertain() {
    let clock = ManualClock::new(0);
    let (session, mut calls, _feed) = open_caught_up(&clock, 1).await;
    let sending = tokio::spawn({
        let session = session.clone();
        async move { session.send_prompt("hi", Vec::new()).await }
    });
    let (_, reply) = calls.send_input().await;
    reply
        .send(Err(RpcError::Refused(wire::Error {
            code: ErrorCode::NotFound as i32,
            message: "no such agent".into(),
            details: Vec::new(),
        })))
        .ok();
    let sent = sending.await.unwrap();
    assert_eq!(
        session.state().input_state(&sent.id),
        Some(InputState::Rejected(RefusalReason::Other(
            "no such agent".into()
        )))
    );
}

#[tokio::test]
async fn attachment_bytes_are_fetched_lazily_once_and_redraw_their_row() {
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Session::open(
        client,
        agent(KIND),
        40,
        CAP,
        ManualClock::new(0),
    ));
    let (_, feed) = calls.subscribe().await;
    feed.send(snapshot(KIND, 1, &[]));
    feed.send(ev(image_item(KIND, 1, 1, b"hash-1")));
    feed.send(caught_up(1));
    let session = open.await.unwrap().unwrap();
    session.take_changes();
    calls.none().await;
    assert_eq!(session.blob(b"hash-1"), None, "a placeholder until fetched");
    let (request, reply) = calls.get_blob().await;
    assert_eq!(request.hash, b"hash-1");
    assert_eq!(session.blob(b"hash-1"), None);
    calls.none().await;
    reply
        .send(Ok(GetBlobResponse {
            bytes: b"png".to_vec(),
        }))
        .ok();
    let bytes = until(session.changed(), || session.blob(b"hash-1")).await;
    assert_eq!(&*bytes, b"png");
    assert_eq!(session.take_changes().keys, ["k1"], "the row redraws");
    calls.none().await;
}

#[tokio::test]
async fn the_trace_is_bounded_and_replays_from_its_starting_state() {
    let clock = ManualClock::new(0);
    let (session, _calls, feed) = open_caught_up(&clock, 1).await;
    for revision in 2..=1_000u64 {
        feed.send(ev(text_item(
            KIND,
            1,
            revision,
            &format!("revision {revision}"),
        )));
    }
    until(session.changed(), || {
        let state = session.state();
        (state.transcript().get("k1")?.item.revision == 1_000).then_some(())
    })
    .await;
    let state = session.state();
    let trace = state.trace();
    assert!(trace.events.len() <= TRACE_EVENTS);
    assert!(trace.events.len() >= TRACE_EVENTS / 2);
    assert_eq!(trace.replay(), *state, "the trace replays to the state");
    drop(state);
    let seqs: Vec<u64> = trace.events.iter().map(|traced| traced.seq).collect();
    assert!(
        seqs.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "in order, no gaps"
    );
}

#[tokio::test]
async fn the_dump_part_carries_the_state_and_the_trace() {
    let clock = ManualClock::new(0);
    let (session, _calls, _feed) = open_caught_up(&clock, 2).await;
    let part = session.dump_part();
    let names: Vec<&str> = part.files.iter().map(|file| file.name.as_str()).collect();
    let dir = "client/sessions/6167656e742d31";
    assert_eq!(
        names,
        [format!("{dir}/state.txt"), format!("{dir}/trace.txt")]
    );
    let trace = String::from_utf8(part.files[1].contents.clone()).unwrap();
    assert!(trace.starts_with("start:\n"));
    assert!(trace.contains("Subscribed { tail: 40 }"));
    assert!(trace.contains("CaughtUp"));

    let bundle = tempfile::tempdir().unwrap();
    ui_runtime::write_part(bundle.path(), &part).unwrap();
    assert!(bundle.path().join(dir).join("trace.txt").is_file());
    let mut outside = part.clone();
    outside.files[0].name = "../escape".into();
    assert!(ui_runtime::write_part(bundle.path(), &outside).is_err());
}

#[tokio::test]
async fn the_dump_part_writes_structure_and_no_content() {
    const ITEM_TEXT: &str = "PLANTEDitemtext0001";
    const ITEM_BODY: &str = "PLANTEDitembody0002";
    const APPENDED: &str = "PLANTEDappended0003";
    const SNAPSHOT_BODY: &str = "PLANTEDsnapshotbody0004";
    const WORKING_ON: &str = "PLANTEDworkingon0005";
    const QUEUED: &str = "PLANTEDqueuedtext0006";
    const PROMPT: &str = "PLANTEDprompt0007";
    const HIDDEN_ANSWER: &str = "PLANTEDhiddenanswer0008";
    const ENTRY: &str = "PLANTEDentry0009";
    const HOST: &str = "PLANTEDhostname0010";

    let clock = ManualClock::new(0);
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Session::open(client, agent(KIND), 40, CAP, clock.clone()));
    let (_, feed) = calls.subscribe().await;
    let mut first = snapshot(KIND, 3, &[b"queued-1"]);
    if let Some(wire::session_event::Of::Snapshot(snapshot)) = &mut first.of {
        snapshot.body = SNAPSHOT_BODY.as_bytes().to_vec();
        snapshot.working_on = Some(WORKING_ON.into());
        snapshot.queue[0].text = QUEUED.into();
    }
    feed.send(first);
    feed.send(ev(wire::Item {
        body: ITEM_BODY.as_bytes().to_vec(),
        ..text_item(KIND, 1, 1, ITEM_TEXT)
    }));
    feed.send(ev(text_item(KIND, 2, 2, "streaming ")));
    feed.send(append("k2", 2, 3, APPENDED));
    feed.send(caught_up(3));
    let session = Arc::new(open.await.unwrap().unwrap());
    session.set_entry(wire::Agent {
        name: ENTRY.into(),
        cwd: format!("/Users/{ENTRY}"),
        working_on: Some(wire::WorkingOn {
            text: ENTRY.into(),
            updated_at_ms: 1,
        }),
        ..agent(KIND)
    });
    session.set_host(wire::HostEntry {
        host_id: b"host-a".to_vec(),
        name: HOST.into(),
        last_dial_error: Some(HOST.into()),
        ..wire::HostEntry::default()
    });

    let prompting = tokio::spawn({
        let session = session.clone();
        async move { session.send_prompt(PROMPT, Vec::new()).await }
    });
    let (_, reply) = calls.send_input().await;
    reply.send(Ok(accepted(true))).ok();
    let prompt = prompting.await.unwrap();
    let answering = tokio::spawn({
        let session = session.clone();
        async move {
            let answer = wire::ClaudeAnswer {
                ask_key: "ask-1".into(),
                of: Some(wire::claude_answer::Of::Reply(wire::ReplyInstead {
                    text: HIDDEN_ANSWER.into(),
                    answers_so_far: Vec::new(),
                })),
            };
            session
                .answer(Input::new(
                    Vec::new(),
                    wire::ClaudeSdkInput {
                        of: Some(claude_sdk_input::Of::Answer(answer)),
                    },
                ))
                .await
        }
    });
    let (_, reply) = calls.send_input().await;
    reply.send(Ok(accepted(false))).ok();
    answering.await.unwrap().unwrap();

    let part = session.dump_part();
    for secret in [
        ITEM_TEXT,
        ITEM_BODY,
        APPENDED,
        SNAPSHOT_BODY,
        WORKING_ON,
        QUEUED,
        PROMPT,
        HIDDEN_ANSWER,
        ENTRY,
        HOST,
    ] {
        assert_eq!(part_holds(&part, secret), None, "{secret} is in the dump");
    }
    // The structure stays: keys, revisions, input ids and the order of events.
    let text: String = part
        .files
        .iter()
        .map(|file| String::from_utf8_lossy(&file.contents).into_owned())
        .collect();
    let id: String = prompt.id.iter().map(|byte| format!("{byte:02x}")).collect();
    for expected in [
        "k1 order=1 rev=1",
        "Append k2 base=2 rev=3",
        "CaughtUp rev=3",
        "Subscribed { tail: 40 }",
        "answer ask=ask-1",
    ] {
        assert!(text.contains(expected), "{expected} is missing:\n{text}");
    }
    assert!(text.contains(&format!("Send {id} prompt attachments=0")));
    assert!(text.contains(&format!("Sent {id} accepted queued=true")));
}

#[tokio::test]
async fn closing_drops_the_stream_and_a_late_result_changes_nothing() {
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Session::open(
        client,
        agent(KIND),
        40,
        CAP,
        ManualClock::new(0),
    ));
    let (_, feed) = calls.subscribe().await;
    feed.send(snapshot(KIND, 1, &[]));
    feed.send(ev(image_item(KIND, 1, 1, b"hash-1")));
    feed.send(caught_up(1));
    let session = open.await.unwrap().unwrap();
    assert_eq!(session.blob(b"hash-1"), None);
    let (_, reply) = calls.get_blob().await;
    session.close();
    tokio::time::timeout(PATIENCE, async {
        while !feed.is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the stream is dropped");
    reply
        .send(Ok(GetBlobResponse {
            bytes: b"late".to_vec(),
        }))
        .ok();
    calls.none().await;
}

#[tokio::test]
async fn an_agent_the_runtime_no_longer_serves_ends_the_session() {
    let clock = ManualClock::new(0);
    let (session, mut calls, feed) = open_caught_up(&clock, 1).await;
    drop(feed);
    clock.armed(250).await;
    clock.advance(250);
    calls
        .refuse_subscribe(RpcError::Refused(wire::Error {
            code: ErrorCode::NotFound as i32,
            message: "no such agent".into(),
            details: Vec::new(),
        }))
        .await;
    let ended = until(session.changed(), || session.ended()).await;
    assert_eq!(ended.code(), Some(ErrorCode::NotFound));
    calls.none().await;
    assert!(clock.sleeping().is_empty(), "nothing retries");
}

fn keys(session: &Session) -> Vec<String> {
    session.state().transcript().keys().cloned().collect()
}

#[tokio::test]
async fn a_following_window_trims_to_its_cap_and_a_return_past_the_held_rows_reloads_the_head() {
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Session::open(
        client,
        agent(KIND),
        3,
        3,
        ManualClock::new(0),
    ));
    let (_, feed) = calls.subscribe().await;
    feed.send(snapshot(KIND, 3, &[]));
    for order in 1..=3 {
        feed.send(ev(text_item(KIND, order, order, &format!("row {order}"))));
    }
    feed.send(caught_up(3));
    let session = open.await.unwrap().expect("the session opens");
    until(session.changed(), || {
        session.state().caught_up().then_some(())
    })
    .await;

    // Following, a live row trims the top, and the full window fetches no
    // page however often the client asks.
    feed.send(ev(text_item(KIND, 4, 4, "row 4")));
    until(session.changed(), || {
        (session.state().transcript().head() == Some(4)).then_some(())
    })
    .await;
    assert_eq!(keys(&session), ["k2", "k3", "k4"]);
    assert_eq!(session.page_older(40).await, Ok(0));
    calls.none().await;

    // In history, arrivals are held apart until they pass the cap.
    session.follow(false);
    for order in 5..=8 {
        feed.send(ev(text_item(KIND, order, order, &format!("row {order}"))));
    }
    until(session.changed(), || {
        session.state().head_moved_on().then_some(())
    })
    .await;
    assert!(session.state().arrivals_held());
    assert_eq!(
        keys(&session),
        ["k2", "k3", "k4"],
        "the reader's window stays"
    );
    session.take_changes();

    // The return reopens the stream with a fresh tail, keeping the old
    // window on screen until the fresh one is caught up.
    session.follow(true);
    let (request, fresh) = calls.subscribe().await;
    assert_eq!(request.tail, 3);
    assert!(feed.is_closed(), "the old stream is dropped");
    fresh.send(snapshot(KIND, 9, &[]));
    for order in 7..=9 {
        fresh.send(ev(text_item(KIND, order, order, &format!("row {order}"))));
    }
    until(session.changed(), || {
        session.state().reset_pending().then_some(())
    })
    .await;
    assert_eq!(keys(&session), ["k2", "k3", "k4"]);
    fresh.send(caught_up(9));
    until(session.changed(), || {
        (!session.state().reset_pending()).then_some(())
    })
    .await;
    assert_eq!(keys(&session), ["k7", "k8", "k9"]);
    assert!(session.take_changes().reloaded, "the swap reports reloaded");
    assert!(!session.state().arrivals_held());

    let state = session.state();
    let trace = state.trace();
    assert!(
        trace
            .events
            .iter()
            .any(|traced| traced.event == TraceEvent::Driver(DriverEvent::Reload))
    );
    assert_eq!(trace.replay(), *state, "the trace rebuilds the same window");
}

#[tokio::test]
async fn a_return_whose_held_rows_fit_releases_them_without_a_fetch() {
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Session::open(
        client,
        agent(KIND),
        3,
        3,
        ManualClock::new(0),
    ));
    let (_, feed) = calls.subscribe().await;
    feed.send(snapshot(KIND, 3, &[]));
    for order in 1..=3 {
        feed.send(ev(text_item(KIND, order, order, &format!("row {order}"))));
    }
    feed.send(caught_up(3));
    let session = open.await.unwrap().expect("the session opens");
    session.follow(false);
    for order in 4..=5 {
        feed.send(ev(text_item(KIND, order, order, &format!("row {order}"))));
    }
    until(session.changed(), || {
        (session.state().held_arrivals() == 2).then_some(())
    })
    .await;
    // In history a page is not bounded by the cap.
    let paging = tokio::spawn({
        let session = Arc::new(session);
        let paged = session.clone();
        async move { (paged.page_older(40).await, session) }
    });
    let (request, reply) = calls.fetch().await;
    assert_eq!(request.limit, 40);
    reply.send(Ok(wire::FetchResponse::default())).ok();
    let (_, session) = paging.await.unwrap();
    session.follow(true);
    calls.none().await;
    assert_eq!(keys(&session), ["k3", "k4", "k5"]);
    assert!(!feed.is_closed());
}
