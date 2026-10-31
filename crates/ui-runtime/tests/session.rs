//! The session driver against an in-memory runtime: every call it makes is
//! answered by hand, and time moves only when the test moves it.

use std::sync::Arc;

use client::{ManualClock, RpcError};
use support::*;
use ui_runtime::{
    DriverEvent, InputError, PageError, Session, TRACE_EVENTS, TRANSCRIPT_DUMP, TraceEvent,
};
use ui_state::{Composer, Connection, InputOutcome, InputState, Refusal, Waiting};
use wire::{
    ErrorCode, GetBlobResponse, Input, Kind, ListItemsResponse, SendInputRequest, claude_pty_input,
    claude_sdk_input, codex_input,
};

use crate::support;

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
    let (_, feed) = calls.subscribe_session().await;
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
    let (_, feed) = calls.subscribe_session().await;
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
    let (request, feed) = calls.subscribe_session().await;
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
        assert_eq!(state.composer(), Composer::Holds(Waiting::CatchingUp));
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
    let (_, feed) = calls.subscribe_session().await;
    feed.send(snapshot(KIND, 2, &[]));
    feed.send(ev(text_item(KIND, 1, 1, "cached")));
    feed.send(ev(text_item(KIND, 2, 2, "cached too")));
    feed.send(detached());
    let session = open.await.unwrap().unwrap();
    let state = session.state();
    assert_eq!(state.transcript().len(), 2, "the rows stay");
    assert!(!state.can_send());
    assert_eq!(state.composer(), Composer::Holds(Waiting::Detached));
}

#[tokio::test]
async fn lagged_reopens_with_a_tail_at_once_and_dedupes_the_overlap() {
    let clock = ManualClock::new(0);
    let (session, mut calls, feed) = open_caught_up(&clock, 3).await;
    feed.send(lagged());
    let (request, feed) = calls.subscribe_session().await;
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
    let (_, feed) = calls.subscribe_session().await;
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
    let (_, feed) = calls.subscribe_session().await;
    feed.send(snapshot(KIND, 6, &[]));
    feed.send(ev(text_item(KIND, 5, 5, "five")));
    feed.send(ev(text_item(KIND, 6, 6, "six")));
    feed.send(caught_up(6));
    let session = Arc::new(open.await.unwrap().unwrap());
    let paging = tokio::spawn({
        let session = session.clone();
        async move { session.page_older(3).await }
    });
    let (request, reply) = calls.list_items().await;
    assert_eq!(request.before_order, Some(5));
    assert_eq!(request.page_size, 3);
    let items = (2..=4)
        .rev()
        .map(|order| text_item(KIND, order, order, "older"))
        .collect();
    reply
        .send(Ok(ListItemsResponse {
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
    let (_, reply) = calls.list_items().await;
    reply.send(Err(unreachable())).ok();
    assert_eq!(paging.await.unwrap(), Err(PageError::OriginUnreachable));
    assert_eq!(session.state().transcript().len(), 3);
}

#[tokio::test]
async fn an_append_whose_base_is_not_held_is_answered_with_get() {
    let clock = ManualClock::new(0);
    let (session, mut calls, feed) = open_caught_up(&clock, 2).await;
    feed.send(append("k2", 5, 6, " more"));
    let (request, reply) = calls.get_item().await;
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
        let (_, feed) = calls.subscribe_session().await;
        feed.send(snapshot(kind, 1, &[b"queued-1"]));
        feed.send(caught_up(1));
        let session = Arc::new(open.await.unwrap().unwrap());
        for (act, verdict) in [
            ("withdraw", Ok(accepted(false))),
            ("send_now", Ok(accepted(false))),
            ("interrupt", Ok(rejected(wire::Refusal::Unsupported))),
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
                Ok(_) => Err(InputError::Rejected(Refusal::of(
                    wire::Refusal::Unsupported,
                ))),
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
    let (_, feed) = calls.subscribe_session().await;
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
    reply.send(Ok(rejected(wire::Refusal::Exited))).ok();
    let sent = sending.await.unwrap();
    assert_eq!(
        session.state().input_state(&sent.id),
        Some(InputState::Rejected(Refusal::of(wire::Refusal::Exited)))
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
        Some(InputState::Rejected(Refusal {
            reason: wire::Refusal::Unspecified,
            detail: Some("no such agent".into()),
        }))
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
    let (_, feed) = calls.subscribe_session().await;
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
async fn a_sessions_history_is_its_trace_in_the_form_a_dump_carries() {
    let clock = ManualClock::new(0);
    let (session, _calls, _feed) = open_caught_up(&clock, 2).await;
    let history = session.history();
    assert_eq!(history.agent_id, agent(KIND).agent_id);
    let start = history
        .start
        .clone()
        .expect("the state the trace began from");
    let events = history.events.clone().expect("the events since").traced;
    assert!(
        events.windows(2).all(|pair| pair[1].seq == pair[0].seq + 1),
        "in order, no gaps"
    );
    let driver: Vec<&str> = events
        .iter()
        .filter_map(|traced| match &traced.event {
            Some(wire::dump::client_traced::Event::Driver(driver)) => Some(driver.as_str()),
            _ => None,
        })
        .collect();
    assert!(driver.contains(&"Subscribed { tail: 40 }"), "{driver:?}");

    // Read back, the start and its messages replay to the session as it
    // stands.
    let mut replayed = ui_state::SessionState::from_wire(start).expect("the start reads back");
    for traced in events {
        if let Some(wire::dump::client_traced::Event::Msg(msg)) = traced.event {
            replayed.update(ui_state::Msg::from_wire(msg).expect("a message reads back"));
        }
    }
    assert_eq!(replayed, *session.state());
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
    let (_, feed) = calls.subscribe_session().await;
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
    let (_, feed) = calls.subscribe_session().await;
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
    let (request, fresh) = calls.subscribe_session().await;
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
    let (_, feed) = calls.subscribe_session().await;
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
    let (request, reply) = calls.list_items().await;
    assert_eq!(request.page_size, 40);
    reply.send(Ok(wire::ListItemsResponse::default())).ok();
    let (_, session) = paging.await.unwrap();
    session.follow(true);
    calls.none().await;
    assert_eq!(keys(&session), ["k3", "k4", "k5"]);
    assert!(!feed.is_closed());
}

// --- decided: sends the host never received are held -------------------------

/// Decided: an input the runtime refused before it was written, because the
/// agent's host could not be reached, certainly never left. The client
/// library holds it as pending (visible, with Cancel), and once the host is
/// reachable again sends it once more under the same id. Covers a prompt
/// and an ask answer; a stop is held the same way once it goes through the
/// library.
#[tokio::test]
async fn an_input_refused_because_the_host_was_unreachable_is_held_and_sent_again_with_its_id() {
    for act in ["prompt", "answer"] {
        let clock = ManualClock::new(0);
        let (session, mut calls, feed) = open_caught_up(&clock, 2).await;
        let _sending = tokio::spawn({
            let session = session.clone();
            async move {
                match act {
                    "prompt" => {
                        session.send_prompt("hello", Vec::new()).await;
                    }
                    _ => {
                        let _ = session.answer(answering(KIND)).await;
                    }
                }
            }
        });
        let (request, reply) = calls.send_input().await;
        let id = request.input.unwrap().input_id;
        reply
            .send(Ok(rejected(wire::Refusal::HostUnreachable)))
            .ok();
        // The verdict lands; the input is held, not rejected.
        let held = until(session.changed(), || {
            session
                .state()
                .input_state(&id)
                .filter(|state| *state != InputState::Sent)
        })
        .await;
        assert_eq!(
            held,
            InputState::Held,
            "{act}: an input that never left is held"
        );
        // The host comes back: the chat reconnects and catches up.
        let feed = reconnect(&clock, &session, &mut calls, feed).await;
        feed.send(snapshot(KIND, 3, &[]));
        feed.send(ev(text_item(KIND, 2, 2, "row 2")));
        feed.send(caught_up(3));
        until(session.changed(), || {
            session.state().caught_up().then_some(())
        })
        .await;
        let again = tokio::time::timeout(std::time::Duration::from_secs(1), calls.next()).await;
        match again {
            Ok(Call::SendInput(request, reply)) => {
                assert_eq!(
                    request.input.unwrap().input_id,
                    id,
                    "{act}: the held input is sent again under its own id"
                );
                reply.send(Ok(accepted(false))).ok();
            }
            Ok(other) => panic!("{act}: expected the held input again, got {other:?}"),
            Err(_) => panic!("{act}: the held input was never sent again once the host was back"),
        }
        drop(feed);
    }
}

#[tokio::test]
async fn a_transcript_that_fails_its_check_asks_for_a_dump_with_its_trace_cut_there() {
    let (client, mut calls) = runtime();
    let open = tokio::spawn(Session::open(
        client,
        agent(KIND),
        40,
        CAP,
        ManualClock::new(0),
    ));
    let (_, feed) = calls.subscribe_session().await;
    feed.send(snapshot(KIND, 3, &[]));
    // An origin skipping an order breaks the block the client holds.
    feed.send(ev(text_item(KIND, 1, 1, "one")));
    feed.send(ev(text_item(KIND, 3, 3, "three")));
    let _session = open.await.unwrap().expect("the session opens");
    feed.send(caught_up(3));
    let Call::Dump(request, reply) = calls.next().await else {
        panic!("the driver asked for no dump");
    };
    assert!(request.automatic);
    assert_eq!(request.agent_ids, vec![agent(KIND).agent_id]);
    assert!(
        request.reason.starts_with(TRANSCRIPT_DUMP) && request.reason.contains("between 1 and 3"),
        "{}",
        request.reason
    );
    // The session's trace rides along, cut at the message that broke the
    // transcript, for the daemon to redact and write before it answers.
    let [history] = &request.sessions[..] else {
        panic!("{} sessions in the request", request.sessions.len());
    };
    let history = <wire::dump::ClientSessionHistory as prost::Message>::decode(history.as_slice())
        .expect("a session");
    assert_eq!(history.agent_id, agent(KIND).agent_id);
    let events = &history.events.as_ref().expect("events").traced;
    let last = events.last().expect("an event");
    assert!(
        matches!(
            &last.event,
            Some(wire::dump::client_traced::Event::Msg(
                wire::dump::ClientSessionMsg {
                    of: Some(wire::dump::client_session_msg::Of::Event(
                        wire::SessionEvent {
                            of: Some(wire::session_event::Of::CaughtUp(_))
                        }
                    ))
                }
            ))
        ),
        "{last:?}"
    );
    reply
        .send(Ok(wire::DumpResponse {
            report_path: "elsewhere".to_owned(),
        }))
        .unwrap();

    // The same transcript fails again at the next CaughtUp: one dump is
    // enough for the session.
    feed.send(caught_up(3));
    feed.send(lagged());
    let (_, _feed) = calls.subscribe_session().await;
}
