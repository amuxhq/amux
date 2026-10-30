use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::DateTime;
use redaction::RedactionSummary;
use replay_support::{
    IoDirection, IoEvent, MANIFEST_SCHEMA_VERSION, Manifest, Recorded, Recording, ReplayAdvance,
    ReplayController, ReplayTransport, strict_replay,
};
use semver::Version;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

fn event(us: u64, direction: IoDirection, line: &str, transport_id: &str) -> IoEvent {
    IoEvent {
        us,
        direction,
        line: line.to_string(),
        transport_id: Some(transport_id.to_string()),
        session_id: None,
    }
}

fn recording(io: Vec<IoEvent>) -> Recording {
    Recording {
        dir: PathBuf::from("synthetic"),
        manifest: Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            spec: "strict_replay".to_string(),
            recorded: Recorded {
                provider: "synthetic".to_string(),
                version: Version::new(1, 0, 0),
                model: "none".to_string(),
                at: DateTime::UNIX_EPOCH,
            },
            content: BTreeMap::new(),
            redaction: RedactionSummary::default(),
            session_ids: Vec::new(),
            provider_extra: serde_json::Map::new(),
        },
        io,
    }
}

/// The strict replay of a script whose events are all on `session-a`.
fn session(script: Vec<IoEvent>) -> (ReplayTransport, ReplayController) {
    let mut replay = strict_replay(&recording(script));
    let transport = replay
        .transports
        .remove("session-a")
        .expect("the script's transport");
    (transport, replay.controller)
}

#[tokio::test]
async fn strict_replay_reports_complete_consumption() {
    let script = vec![
        event(10, IoDirection::Read, r#"{"ready":true}"#, "session-a"),
        event(
            20,
            IoDirection::Write,
            r#"{"request":"turn","id":7}"#,
            "session-a",
        ),
        event(30, IoDirection::Read, r#"{"done":true}"#, "session-a"),
    ];
    let (mut transport, controller) = session(script);

    assert_eq!(
        controller.advance_one().await,
        ReplayAdvance::Advanced { event_us: 10 }
    );
    let mut line = String::new();
    transport.reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, "{\"ready\":true}\n");

    transport
        .writer
        .write_all(b"{\"id\":7,\"request\":\"turn\"}\n")
        .await
        .unwrap();
    transport.writer.flush().await.unwrap();

    assert_eq!(
        controller.advance_one().await,
        ReplayAdvance::Advanced { event_us: 30 }
    );
    line.clear();
    transport.reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, "{\"done\":true}\n");

    let report = controller.finish().unwrap();
    assert!(report.is_complete());
    assert_eq!(report.validated_writes, 1);
    assert_eq!(report.delivered_reads, 2);
}

#[test]
fn strict_replay_rejects_leftover_reads() {
    let (_, controller) = session(vec![event(10, IoDirection::Read, "unread", "session-a")]);

    let error = controller.finish().unwrap_err();
    assert_eq!(error.report.remaining_reads, 1);
}

#[test]
fn strict_replay_rejects_unwritten_expectations() {
    let (_, controller) = session(vec![event(10, IoDirection::Write, "expected", "session-a")]);

    let error = controller.finish().unwrap_err();
    assert_eq!(error.report.remaining_writes, 1);
}

#[tokio::test]
async fn strict_replay_rejects_trailing_writes() {
    let (mut transport, controller) =
        session(vec![event(10, IoDirection::Write, "expected", "session-a")]);

    transport
        .writer
        .write_all(b"expected\nunexpected\n")
        .await
        .unwrap();
    assert!(transport.writer.flush().await.is_err());

    let error = controller.finish().unwrap_err();
    assert_eq!(error.report.trailing_writes, vec!["unexpected"]);
}

#[tokio::test]
async fn strict_replay_rejects_partial_trailing_output() {
    let (mut transport, controller) =
        session(vec![event(10, IoDirection::Write, "expected", "session-a")]);

    transport.writer.write_all(b"partial").await.unwrap();

    let error = controller.finish().unwrap_err();
    assert_eq!(error.report.trailing_output.as_deref(), Some("partial"));
}

#[test]
fn strict_replay_rejects_unused_transports() {
    let replay = strict_replay(&recording(vec![event(
        10,
        IoDirection::Read,
        "unread",
        "unused-session",
    )]));

    let error = replay.controller.finish().unwrap_err();
    assert_eq!(error.report.unused_transports, vec!["unused-session"]);
}

#[tokio::test]
async fn the_clock_delivers_what_falls_due_as_it_advances() {
    let mut replay = strict_replay(&recording(vec![
        event(10, IoDirection::Read, "fast", "fast"),
        event(20, IoDirection::Read, "slow", "slow"),
    ]));
    let mut fast = replay.transports.remove("fast").unwrap();
    let mut slow = replay.transports.remove("slow").unwrap();

    assert_eq!(
        replay.clock.advance_one().await,
        ReplayAdvance::Advanced { event_us: 10 }
    );
    let mut line = String::new();
    fast.reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, "fast\n");

    assert_eq!(
        replay.clock.advance_for(Duration::from_micros(10)).await,
        ReplayAdvance::Advanced { event_us: 20 }
    );
    line.clear();
    slow.reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, "slow\n");

    assert_eq!(replay.clock.current_us(), Some(20));
    replay.controller.finish().unwrap();
}

#[tokio::test]
async fn strict_replay_accepts_concurrent_writes_in_either_order() {
    let script = vec![
        event(10, IoDirection::Read, r#"{"ask":"handshake"}"#, "session-a"),
        event(
            20,
            IoDirection::Write,
            r#"{"type":"user","message":"hello"}"#,
            "session-a",
        ),
        event(
            30,
            IoDirection::Write,
            r#"{"type":"control_response","response":{"request_id":"r1"}}"#,
            "session-a",
        ),
        event(40, IoDirection::Read, r#"{"done":true}"#, "session-a"),
    ];
    let (mut transport, controller) = session(script);

    controller.advance_one().await;
    let mut line = String::new();
    transport.reader.read_line(&mut line).await.unwrap();

    transport
        .writer
        .write_all(b"{\"type\":\"control_response\",\"response\":{\"request_id\":\"r1\"}}\n")
        .await
        .unwrap();
    transport
        .writer
        .write_all(b"{\"type\":\"user\",\"message\":\"hello\"}\n")
        .await
        .unwrap();
    transport.writer.flush().await.unwrap();

    assert_eq!(
        controller.advance_one().await,
        ReplayAdvance::Advanced { event_us: 40 }
    );
    line.clear();
    transport.reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, "{\"done\":true}\n");
    controller.finish().unwrap();
}

#[tokio::test]
async fn strict_replay_rejects_a_write_that_crosses_a_read() {
    let script = vec![
        event(10, IoDirection::Write, r#"{"first":true}"#, "session-a"),
        event(20, IoDirection::Read, r#"{"between":true}"#, "session-a"),
        event(30, IoDirection::Write, r#"{"second":true}"#, "session-a"),
    ];
    let (mut transport, controller) = session(script);

    transport
        .writer
        .write_all(b"{\"second\":true}\n")
        .await
        .unwrap();
    assert!(transport.writer.flush().await.is_err());
    assert!(controller.finish().is_err());
}

#[tokio::test]
async fn strict_replay_rejects_reordering_writes_from_one_origin() {
    let script = vec![
        event(
            10,
            IoDirection::Write,
            r#"{"type":"user","message":"go"}"#,
            "session-a",
        ),
        event(
            20,
            IoDirection::Write,
            r#"{"type":"control_request","request":{"subtype":"reload_skills"}}"#,
            "session-a",
        ),
    ];
    let (mut transport, controller) = session(script);

    transport
        .writer
        .write_all(b"{\"type\":\"control_request\",\"request\":{\"subtype\":\"reload_skills\"}}\n")
        .await
        .unwrap();
    assert!(transport.writer.flush().await.is_err());
    assert!(controller.finish().is_err());
}

#[tokio::test]
async fn strict_replay_reports_write_mismatch_without_panicking() {
    let (mut transport, controller) =
        session(vec![event(10, IoDirection::Write, "expected", "session-a")]);

    transport.writer.write_all(b"actual\n").await.unwrap();
    assert!(transport.writer.flush().await.is_err());

    let mismatch = &controller.finish().unwrap_err().report.write_mismatches[0];
    assert_eq!(mismatch.index, 0);
    assert_eq!(mismatch.expected, "expected");
    assert_eq!(mismatch.actual, "actual");
}

#[tokio::test]
async fn strict_replay_builds_named_transports_over_a_recording() {
    let mut replay = strict_replay(&recording(vec![
        event(10, IoDirection::Read, "alpha-ready", "alpha"),
        event(20, IoDirection::Read, "beta-ready", "beta"),
    ]));
    assert_eq!(
        replay.transports.keys().cloned().collect::<Vec<_>>(),
        vec!["alpha", "beta"]
    );
    let mut alpha = replay.transports.remove("alpha").unwrap();
    let mut beta = replay.transports.remove("beta").unwrap();

    assert_eq!(
        replay.clock.advance_one().await,
        ReplayAdvance::Advanced { event_us: 10 }
    );
    let mut line = String::new();
    alpha.reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, "alpha-ready\n");

    assert_eq!(
        replay.clock.advance_one().await,
        ReplayAdvance::Advanced { event_us: 20 }
    );
    line.clear();
    beta.reader.read_line(&mut line).await.unwrap();
    assert_eq!(line, "beta-ready\n");

    replay.controller.finish().unwrap();
}

#[tokio::test]
async fn strict_replay_validates_named_transport_writes_in_recorded_order() {
    let mut replay = strict_replay(&recording(vec![
        event(10, IoDirection::Write, "alpha-write", "alpha"),
        event(20, IoDirection::Write, "beta-write", "beta"),
    ]));
    let mut alpha = replay.transports.remove("alpha").unwrap();
    let mut beta = replay.transports.remove("beta").unwrap();

    alpha.writer.write_all(b"alpha-write\n").await.unwrap();
    alpha.writer.flush().await.unwrap();
    beta.writer.write_all(b"beta-write\n").await.unwrap();
    beta.writer.flush().await.unwrap();

    let report = replay.controller.finish().unwrap();
    assert_eq!(report.validated_writes, 2);
}

#[tokio::test]
async fn strict_replay_validates_named_transport_writes_in_reverse_order() {
    let mut replay = strict_replay(&recording(vec![
        event(10, IoDirection::Write, "alpha-write", "alpha"),
        event(20, IoDirection::Write, "beta-write", "beta"),
    ]));
    let mut alpha = replay.transports.remove("alpha").unwrap();
    let mut beta = replay.transports.remove("beta").unwrap();

    beta.writer.write_all(b"beta-write\n").await.unwrap();
    beta.writer.flush().await.unwrap();
    alpha.writer.write_all(b"alpha-write\n").await.unwrap();
    alpha.writer.flush().await.unwrap();

    let report = replay.controller.finish().unwrap();
    assert_eq!(report.validated_writes, 2);
}

#[tokio::test]
async fn strict_replay_named_transport_mismatch_reports_its_own_expectation() {
    let mut replay = strict_replay(&recording(vec![
        event(10, IoDirection::Write, "alpha-write", "alpha"),
        event(20, IoDirection::Write, "beta-write", "beta"),
    ]));
    let mut beta = replay.transports.remove("beta").unwrap();

    beta.writer.write_all(b"wrong-beta-write\n").await.unwrap();
    assert!(beta.writer.flush().await.is_err());

    let error = replay.controller.finish().unwrap_err();
    assert_eq!(error.report.write_mismatches.len(), 1);
    let mismatch = &error.report.write_mismatches[0];
    assert_eq!(mismatch.index, 1);
    assert_eq!(mismatch.expected, "beta-write");
    assert_eq!(mismatch.actual, "wrong-beta-write");
}

#[tokio::test]
async fn closing_named_replay_reads_delivers_eof_while_clock_stays_alive() {
    let mut replay = strict_replay(&recording(vec![
        event(1, IoDirection::Read, "terminal", "pty"),
        event(2, IoDirection::Read, "hook", "hook"),
    ]));
    replay.controller.drive().await;
    replay.controller.close_reads().await.unwrap();
    for (name, expected) in [("pty", "terminal\n"), ("hook", "hook\n")] {
        let mut transport = replay.transports.remove(name).unwrap();
        let mut received = String::new();
        tokio::time::timeout(
            Duration::from_secs(1),
            transport.reader.read_to_string(&mut received),
        )
        .await
        .expect("closed replay did not deliver EOF")
        .unwrap();
        assert_eq!(received, expected);
    }
    assert!(replay.controller.finish().unwrap().is_complete());
    assert_eq!(replay.clock.advance_one().await, ReplayAdvance::Exhausted);
}

#[tokio::test]
async fn closing_replay_reads_preserves_missing_write_failures() {
    let (mut transport, controller) =
        session(vec![event(1, IoDirection::Write, "required", "session-a")]);
    controller.close_reads().await.unwrap();
    let mut line = String::new();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            transport.reader.read_line(&mut line)
        )
        .await
        .unwrap()
        .unwrap(),
        0
    );
    assert_eq!(controller.finish().unwrap_err().report.remaining_writes, 1);
}

#[tokio::test]
async fn replay_driver_waits_for_expected_writes_then_resumes() {
    let (mut transport, controller) = session(vec![
        event(1, IoDirection::Write, "request", "session-a"),
        event(2, IoDirection::Read, "response", "session-a"),
    ]);
    let driver = controller.drive();
    tokio::pin!(driver);
    assert!(
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::future::Future::poll(driver.as_mut(), cx))
        })
        .await
        .is_pending()
    );
    transport.writer.write_all(b"request\n").await.unwrap();
    transport.writer.flush().await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), driver)
        .await
        .unwrap();
    controller.close_reads().await.unwrap();
    let mut received = String::new();
    transport
        .reader
        .read_to_string(&mut received)
        .await
        .unwrap();
    assert_eq!(received, "response\n");
    assert!(controller.finish().unwrap().is_complete());
}
