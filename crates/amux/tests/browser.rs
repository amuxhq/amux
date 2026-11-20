//! `amux internal browser` against a real daemon, as the browser extension
//! drives it: the profiles from the front door, a screenshot stored with an
//! agent, a page review sent to it, and the review read back from its chat.

#![cfg(unix)]

use std::process::Stdio;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use prost::Message as _;
use provider_fakes::Step;
use serde_json::{Value, json};
use support::created_id;
use support::desk::{Desk, LONG_GRACE_SECS};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::{ChildStdin, ChildStdout};
use wire::amux::staging;

use crate::support;

struct Bridge {
    input: ChildStdin,
    output: ChildStdout,
    next: u64,
    _child: tokio::process::Child,
}

impl Bridge {
    fn start(desk: &Desk) -> Bridge {
        let mut child = desk
            .command(&["internal", "browser", "chrome-extension://test/"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the bridge starts");
        Bridge {
            input: child.stdin.take().unwrap(),
            output: child.stdout.take().unwrap(),
            next: 1,
            _child: child,
        }
    }

    async fn send(&mut self, frame: &Value) {
        let bytes = serde_json::to_vec(frame).unwrap();
        self.input
            .write_all(&(bytes.len() as u32).to_ne_bytes())
            .await
            .unwrap();
        self.input.write_all(&bytes).await.unwrap();
        self.input.flush().await.unwrap();
    }

    async fn read(&mut self) -> Value {
        let read = async {
            let mut length = [0u8; 4];
            self.output.read_exact(&mut length).await.unwrap();
            let length = u32::from_ne_bytes(length) as usize;
            assert!(length < 1024 * 1024, "a frame over a browser's 1 MB limit");
            let mut frame = vec![0; length];
            self.output.read_exact(&mut frame).await.unwrap();
            serde_json::from_slice(&frame).unwrap()
        };
        tokio::time::timeout(support::PATIENCE, read)
            .await
            .expect("the bridge answers")
    }

    /// Makes a call and gathers its messages, joining parts, until it ends.
    async fn call(
        &mut self,
        target: &str,
        method: &str,
        request: impl prost::Message,
    ) -> Result<Vec<Vec<u8>>, Value> {
        let id = self.next;
        self.next += 1;
        self.send(&json!({
            "id": id,
            "call": {"target": target, "method": method, "body": STANDARD.encode(request.encode_to_vec())},
        }))
        .await;
        let mut messages = Vec::new();
        let mut part = Vec::new();
        loop {
            let frame = self.read().await;
            assert_eq!(frame["id"], json!(id));
            if let Some(end) = frame.get("end") {
                return match end["code"].as_i64() {
                    Some(0) => Ok(messages),
                    _ => Err(end.clone()),
                };
            }
            part.extend(STANDARD.decode(frame["message"].as_str().unwrap()).unwrap());
            if frame.get("more").is_none() {
                messages.push(std::mem::take(&mut part));
            }
        }
    }

    async fn unary<T: prost::Message + Default>(
        &mut self,
        target: &str,
        method: &str,
        request: impl prost::Message,
    ) -> T {
        let messages = self
            .call(target, method, request)
            .await
            .unwrap_or_else(|end| panic!("{method} ended {end}"));
        assert_eq!(messages.len(), 1, "{method} answers once");
        T::decode(messages[0].as_slice()).unwrap()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_extension_sends_a_page_review_through_the_bridge() {
    let desk = Desk::new(
        false,
        LONG_GRACE_SECS,
        node::version(),
        "http://127.0.0.1:9/",
        vec![
            Step::Text {
                chunks: vec!["ready".to_owned()],
            },
            Step::TurnEnd,
            Step::Text {
                chunks: vec!["seen".to_owned()],
            },
            Step::TurnEnd,
        ],
    );
    desk.run(&["server", "start"]).await;
    let work = desk.work.to_string_lossy().into_owned();
    let created = desk
        .run(&[
            "new", "claude", "--in", "amux", "--name", "site", "--cwd", &work, "--prompt", "start",
        ])
        .await;
    let agent = created_id(&created);

    let mut bridge = Bridge::start(&desk);
    let hello = bridge.read().await;
    assert_eq!(hello["hello"]["version"], json!(node::version()));

    let profiles: wire::ListProfilesResponse = bridge
        .unary(
            "installation",
            "/amux.v1.InstallationService/ListProfiles",
            wire::ListProfilesRequest {},
        )
        .await;
    let profile = hex(&profiles.profiles[0].id);

    // A screenshot larger than gRPC's default 4 MiB message, to come back in
    // parts.
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend((0..5_000_000u32).map(|n| (n % 251) as u8));
    let stored: staging::PutBlobResponse = bridge
        .unary(
            &profile,
            "/amux.staging.AgentService/PutBlob",
            staging::PutBlobRequest {
                agent_id: agent.clone(),
                name: "region-1.png".into(),
                mime: "image/png".into(),
                bytes: png.clone(),
            },
        )
        .await;
    let image = stored.blob.expect("a stored blob");
    let fetched: staging::GetBlobResponse = bridge
        .unary(
            &profile,
            "/amux.staging.SessionService/GetBlob",
            staging::GetBlobRequest {
                agent_id: agent.clone(),
                hash: image.hash.clone(),
            },
        )
        .await;
    assert_eq!(
        fetched.bytes, png,
        "the blob comes back whole from its parts"
    );

    let review = wire::PageReview {
        url: "http://localhost:5173/pricing".into(),
        title: "Pricing".into(),
        comments: vec![
            wire::PageComment {
                text: "Say annual".into(),
                element: Some(wire::PageElement {
                    selector: "#pricing h2".into(),
                    text: "Monthly pricing".into(),
                    source: "src/Pricing.tsx:12".into(),
                }),
                quote: "Monthly".into(),
                image: None,
            },
            wire::PageComment {
                text: "Too much space".into(),
                element: Some(wire::PageElement {
                    selector: "main > section".into(),
                    ..Default::default()
                }),
                quote: String::new(),
                image: Some(image),
            },
        ],
    };
    let input = wire::Input::new(
        uuid::Uuid::new_v4().as_bytes().to_vec(),
        wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
                text: "Fix these \u{FFFC}".into(),
                attachments: vec![wire::Attachment {
                    of: Some(wire::attachment::Of::Page(review.clone())),
                }],
            })),
        },
    );
    let verdict: wire::SendInputResponse = bridge
        .unary(
            &profile,
            "/amux.staging.AgentService/SendInput",
            staging::SendInputRequest {
                agent_id: agent.clone(),
                input: Some(input),
            },
        )
        .await;
    assert!(
        matches!(verdict.of, Some(wire::send_input_response::Of::Accepted(_))),
        "the review was accepted: {verdict:?}"
    );

    let deadline = tokio::time::Instant::now() + support::PATIENCE;
    let sent = loop {
        let listed: staging::ListItemsResponse = bridge
            .unary(
                &profile,
                "/amux.staging.SessionService/ListItems",
                staging::ListItemsRequest {
                    agent_id: agent.clone(),
                    before_order: None,
                    page_size: 50,
                },
            )
            .await;
        if let Some(item) = listed
            .items
            .into_iter()
            .find(|item| item.text.starts_with("Fix these"))
        {
            break item;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the review never reached the chat"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert_eq!(
        sent.attachments,
        [wire::Attachment {
            of: Some(wire::attachment::Of::Page(review)),
        }]
    );

    // A stream is cancelled by its id, and ends.
    let id = bridge.next;
    bridge.next += 1;
    bridge
        .send(&json!({
            "id": id,
            "call": {
                "target": profile,
                "method": "/amux.staging.InventoryService/SubscribeInventory",
                "body": STANDARD.encode(staging::SubscribeInventoryRequest::default().encode_to_vec()),
            },
        }))
        .await;
    let first = bridge.read().await;
    assert_eq!(first["id"], json!(id));
    assert!(first.get("message").is_some(), "the fleet streams: {first}");
    bridge.send(&json!({"id": id, "cancel": true})).await;
    loop {
        let frame = bridge.read().await;
        assert_eq!(frame["id"], json!(id));
        if let Some(end) = frame.get("end") {
            assert_eq!(
                end["code"],
                json!(1),
                "a cancelled stream ends cancelled: {frame}"
            );
            break;
        }
    }
}
