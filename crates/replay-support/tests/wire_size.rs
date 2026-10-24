//! The size meter's numbers against records whose encoded sizes are
//! counted by hand, and against a five-line recording replayed through the
//! headless Claude interpreter.

use std::path::Path;

use prost::Message as _;
use replay_support::wire_size::{CommandOutput, Snapshots, measure, wire_size};
use wire::claude_sdk_item::Kind;
use wire::{Append, ClaudeSdkItem, Item, Snapshot, Step, Text, ToolCall};

fn body(kind: Kind) -> Vec<u8> {
    ClaudeSdkItem { kind: Some(kind) }.encode_to_vec()
}

/// The input is a shell call whose subject is `ls`: 2 + 2 for the
/// subject and 2 for the empty shell arm, 6. The call is 17: its name
/// (2 + 4), its input (a two-byte tag, 1 + 6) and its background flag (2).
/// The item body wraps it in 2 more: 19.
fn bash_ls() -> Vec<u8> {
    body(Kind::Tool(ToolCall {
        name: "Bash".into(),
        input: Some(wire::ToolInput {
            subject: "ls".into(),
            of: Some(wire::tool_input::Of::Shell(wire::ShellInput::default())),
        }),
        background: true,
        ..ToolCall::default()
    }))
}

#[test]
fn measures_hand_counted_records() {
    assert_eq!(bash_ls().len(), 19);
    let steps = vec![
        Step {
            // The kind, 1 + 1.
            snapshot: Some(Snapshot {
                kind: wire::Kind::ClaudePty as i32,
                ..Snapshot::default()
            }),
            // The key, 2 + 1, and the body, 2 + 19: 24.
            items: vec![Item {
                key: "t".into(),
                body: bash_ls(),
                ..Item::default()
            }],
            ..Step::default()
        },
        Step {
            // The kind, 1 + 1, and what it works on, 2 + 8.
            snapshot: Some(Snapshot {
                kind: wire::Kind::ClaudeSdk as i32,
                working_on: Some("the docs".into()),
                ..Snapshot::default()
            }),
            // Not a tool call: counted nowhere, nor its append.
            items: vec![Item {
                key: "m".into(),
                body: body(Kind::Message(Text::default())),
                ..Item::default()
            }],
            appends: vec![
                // The key, 2 + 1, and the text, 2 + 2: 7.
                Append {
                    key: "t".into(),
                    text: "ab".into(),
                    ..Append::default()
                },
                Append {
                    key: "m".into(),
                    text: "ignored".into(),
                    ..Append::default()
                },
            ],
            ..Step::default()
        },
        // No snapshot: not counted.
        Step {
            // The first item's 24 and its text, 2 + 1: 27.
            items: vec![Item {
                key: "t".into(),
                body: bash_ls(),
                text: "x".into(),
                ..Item::default()
            }],
            ..Step::default()
        },
        Step {
            // The kind, 1 + 1, and the clock, 1 + 1.
            snapshot: Some(Snapshot {
                kind: wire::Kind::ClaudePty as i32,
                at_ms: 1,
                ..Snapshot::default()
            }),
            ..Step::default()
        },
        Step {
            // 1 + 1 again.
            snapshot: Some(Snapshot {
                kind: wire::Kind::ClaudePty as i32,
                ..Snapshot::default()
            }),
            ..Step::default()
        },
    ];

    let measured = measure(&steps);

    // Sizes 2, 12, 4, 2: sorted 2, 2, 4, 12; the lower middle is 2.
    assert_eq!(
        measured.snapshots,
        Snapshots {
            count: 4,
            largest_bytes: 12,
            median_bytes: 2,
        }
    );
    assert_eq!(
        measured.commands,
        vec![CommandOutput {
            key: "t".into(),
            tool: "Bash".into(),
            command: Some("ls".into()),
            background: true,
            items: 2,
            appends: 1,
            bytes: 24 + 7 + 27,
        }]
    );
}

#[test]
fn measures_a_recording_replayed_through_the_interpreter() {
    let recording = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wire_size/io.jsonl");

    let measured = wire_size(&recording).unwrap();

    // One per change: the initial one, the prompt that starts the turn,
    // the session's init, and the turn's end.
    assert_eq!(measured.snapshots.count, 4, "{measured:#?}");
    assert!(measured.snapshots.largest_bytes >= measured.snapshots.median_bytes);
    // One command: sent whole when it starts and again when it finishes.
    let [command] = measured.commands.as_slice() else {
        panic!("one command: {measured:#?}");
    };
    assert_eq!(
        (
            command.key.as_str(),
            command.tool.as_str(),
            command.command.as_deref()
        ),
        ("toolu_1", "Bash", Some("ls"))
    );
    assert_eq!((command.items, command.appends), (2, 0), "{command:#?}");
    // The same input gives the same numbers.
    assert_eq!(wire_size(&recording).unwrap(), measured);
}
