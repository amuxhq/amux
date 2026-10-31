//! fake-codex as a host sees it: every frame it composes has a recorded
//! shape, injected items are acknowledged and drained, steering and
//! interrupting act on the running turn, and every ask blocks the turn.

use provider_fakes::Kind;
use serde_json::{Value, json};
use support::Host as Line;

use crate::support;

/// Steering is not in any recording yet; the host's interpreter reads only
/// whether the request succeeded. The offered lists are pinned from a
/// read-only probe of codex-cli 0.157.0 in the interpreter's fixtures.
/// Codex's refusal to resume a thread before it is on disk is read from
/// its 0.160.0 source.
const EXEMPT: &[&str] = &[
    "result/turn/steer",
    "error/turn/steer",
    "error/thread/resume",
    "result/model/list",
    "result/skills/list",
];

struct Host {
    line: Line,
    next_id: u64,
    thread: String,
}

impl Host {
    async fn start(script: Value) -> Self {
        let line = Line::spawn(
            Kind::Codex,
            env!("CARGO_BIN_EXE_fake-codex"),
            &["app-server", "--listen", "stdio://"],
            script,
            EXEMPT,
        )
        .await;
        let mut host = Host {
            line,
            next_id: 0,
            thread: String::new(),
        };
        host.call(
            "initialize",
            json!({"capabilities": {"experimentalApi": true},
                   "clientInfo": {"name": "amux", "title": null, "version": "0.1.0"}}),
        )
        .await;
        host.line.send(json!({"method": "initialized"})).await;
        let started = host
            .call(
                "thread/start",
                json!({"approvalPolicy": "on-request", "cwd": "/tmp", "model": "gpt-fake",
                       "sandbox": "workspace-write"}),
            )
            .await;
        host.thread = started["result"]["thread"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        host.line
            .until(|frame| frame["method"] == "thread/started")
            .await;
        host
    }

    /// Send a request and return its response.
    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.request(method, params).await;
        self.response(&id).await
    }

    async fn request(&mut self, method: &str, params: Value) -> String {
        self.next_id += 1;
        let id = format!("amux-{}", self.next_id);
        self.line
            .send(json!({"id": id, "method": method, "params": params}))
            .await;
        id
    }

    async fn response(&mut self, id: &str) -> Value {
        self.line
            .until(|frame| frame["id"] == id && frame.get("method").is_none())
            .await
    }

    async fn turn(&mut self, text: &str) -> String {
        let input = if text.is_empty() {
            json!([])
        } else {
            json!([{"type": "text", "text": text}])
        };
        let thread = self.thread.clone();
        // Every client names its message, as amux and Codex's own app do.
        let message = format!("host-message-{}", self.next_id);
        let started = self
            .call(
                "turn/start",
                json!({"threadId": thread, "input": input, "clientUserMessageId": message}),
            )
            .await;
        started["result"]["turn"]["id"].as_str().unwrap().to_owned()
    }

    async fn completed(&mut self) -> Value {
        self.line
            .until(|frame| frame["method"] == "turn/completed")
            .await
    }

    async fn inject(&mut self, text: &str) -> Value {
        let thread = self.thread.clone();
        self.call(
            "thread/inject_items",
            json!({"threadId": thread, "items": [{"type": "message", "role": "user",
                   "content": [{"type": "input_text", "text": text}]}]}),
        )
        .await
    }
}

fn items(frames: &[Value], kind: &str) -> Vec<Value> {
    frames
        .iter()
        .filter(|frame| frame["method"] == "item/completed")
        .map(|frame| frame["params"]["item"].clone())
        .filter(|item| item["type"] == kind)
        .collect()
}

#[tokio::test]
async fn a_turn_streams_its_message_and_completes_with_it() {
    let mut host = Host::start(json!({"steps": [
        {"thinking": {"text": "Consider"}},
        {"tool": {"class": "exploration", "outcome": {"output": "read"}}},
        {"tool": {"class": "consequential", "input": {"command": "false"},
                  "outcome": {"output": "failed", "error": true}}},
        {"tool": {"name": "apply_patch", "class": "consequential"}},
        {"text": {"chunks": ["Do", "ne"]}},
        "turn_end",
    ]}))
    .await;
    host.turn("Go").await;
    let done = host.completed().await;
    assert_eq!(done["params"]["turn"]["status"], "completed");
    assert_eq!(done["params"]["turn"]["items"][0]["text"], "Done");
    let commands = items(&host.line.frames, "commandExecution");
    assert_eq!(commands[0]["commandActions"][0]["type"], "read");
    assert_eq!(commands[1]["exitCode"], 1);
    assert_eq!(commands[1]["status"], "failed");
    assert_eq!(
        items(&host.line.frames, "fileChange")[0]["status"],
        "completed"
    );
    assert_eq!(items(&host.line.frames, "userMessage").len(), 1);
    assert_eq!(host.line.close().await, 0);
}

#[tokio::test]
async fn a_command_printing_in_pieces_streams_them_while_it_runs() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("gate");
    let mut host = Host::start(json!({"steps": [
        {"tool": {"class": "consequential", "input": {"command": "make"},
                  "outcome": {"output": "line {piece}\n", "pieces": 3},
                  "wait_for": gate}},
        "turn_end",
    ]}))
    .await;
    host.turn("Build").await;
    for piece in 1..=3 {
        let delta = host
            .line
            .until(|frame| frame["method"] == "item/commandExecution/outputDelta")
            .await;
        assert_eq!(delta["params"]["delta"], format!("line {piece}\n"));
    }
    assert!(
        items(&host.line.frames, "commandExecution").is_empty(),
        "the command is still running"
    );
    std::fs::write(&gate, "").unwrap();
    host.completed().await;
    let command = &items(&host.line.frames, "commandExecution")[0];
    assert_eq!(command["aggregatedOutput"], "line 1\nline 2\nline 3\n");
    let deltas = host
        .line
        .frames
        .iter()
        .filter(|frame| frame["method"] == "item/commandExecution/outputDelta")
        .count();
    assert_eq!(deltas, 3, "nothing printed again at the end");
    assert_eq!(host.line.close().await, 0);
}

#[tokio::test]
async fn an_item_injected_mid_turn_is_acknowledged_and_drained_by_that_turn() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("gate");
    let mut host = Host::start(json!({"steps": [
        {"wait_for": {"path": gate}},
        {"text": {"chunks": ["first"]}},
        {"text": {"chunks": ["answering the injected item"]}},
        "turn_end",
        {"text": {"chunks": ["idle inject answered"]}},
        "turn_end",
    ]}))
    .await;
    host.turn("Wait").await;
    let ack = host.inject("From another agent").await;
    assert_eq!(ack["result"], json!({}));
    std::fs::write(&gate, "").unwrap();
    let done = host.completed().await;
    assert_eq!(
        done["params"]["turn"]["items"][0]["text"],
        "answering the injected item"
    );
    // Nothing reports the injected item itself.
    assert!(
        !host
            .line
            .frames
            .iter()
            .any(|frame| frame.to_string().contains("From another agent"))
    );
    // Injected into an idle thread, it waits for an empty turn.
    host.inject("While idle").await;
    host.turn("").await;
    let done = host.completed().await;
    assert_eq!(
        done["params"]["turn"]["items"][0]["text"],
        "idle inject answered"
    );
    assert_eq!(items(&host.line.frames, "userMessage").len(), 1);
    assert_eq!(host.line.close().await, 0);
}

#[tokio::test]
async fn steer_adds_to_the_running_turn_and_interrupt_ends_it() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("never");
    let mut host = Host::start(json!({"steps": [
        {"wait_for": {"path": gate}},
        {"text": {"chunks": ["never said"]}},
        "turn_end",
    ]}))
    .await;
    let turn = host.turn("Wait").await;
    let thread = host.thread.clone();
    let steered = host
        .call(
            "turn/steer",
            json!({"threadId": thread, "expectedTurnId": turn,
                   "input": [{"type": "text", "text": "And this", "text_elements": []}],
                   "clientUserMessageId": "host-steer"}),
        )
        .await;
    assert_eq!(steered["result"]["turnId"], turn);
    let reflected = host
        .line
        .until(|frame| frame["method"] == "item/completed")
        .await;
    assert_eq!(
        reflected["params"]["item"]["content"][0]["text"],
        "And this"
    );
    let interrupted = host
        .call(
            "turn/interrupt",
            json!({"threadId": thread, "turnId": turn}),
        )
        .await;
    assert_eq!(interrupted["result"], json!({}));
    let done = host.completed().await;
    assert_eq!(done["params"]["turn"]["status"], "interrupted");
    let refused = host
        .call(
            "turn/steer",
            json!({"threadId": thread, "expectedTurnId": turn, "input": []}),
        )
        .await;
    assert!(refused.get("error").is_some());
    assert_eq!(host.line.close().await, 0);
}

#[tokio::test]
async fn every_ask_blocks_the_turn_until_the_host_answers() {
    let mut host = Host::start(json!({"steps": [
        {"ask": {"permission": {"class": "consequential", "input": {"command": "touch x"},
                                "outcome": {"output": "ok"}}}},
        {"ask": {"permission": {"class": "consequential", "input": {"command": "rm x"}}}},
        {"ask": {"permission": {"name": "apply_patch", "class": "consequential"}}},
        {"ask": {"question": {"questions": [{"question": "Which?", "header": "Pick",
                                             "options": ["Red", "Blue"]}]}}},
        {"ask": {"grant": {"reason": "Needs the network", "paths": []}}},
        {"ask": {"form": {"server": "spec", "message": "Confirm",
                          "schema": {"type": "object", "properties": {}}}}},
        {"ask": {"link": {"server": "spec", "message": "Sign in", "url": "https://example.com"}}},
        {"text": {"chunks": ["all asked"]}},
        "turn_end",
    ]}))
    .await;
    host.turn("Ask").await;
    let answers = [
        json!({"decision": "accept"}),
        json!({"decision": "decline"}),
        json!({"decision": "acceptForSession"}),
        json!({"answers": {"q0": {"answers": ["Blue"]}}}),
        json!({"permissions": {"fileSystem": null, "network": {"enabled": true}}, "scope": "turn"}),
        // A server's ask comes after Codex's own ask to allow the call.
        json!({"action": "accept", "content": {}}),
        json!({"action": "accept", "content": {}}),
        json!({"action": "accept", "content": {}}),
        json!({"action": "cancel"}),
    ];
    let mut methods = Vec::new();
    for answer in answers {
        let request = host
            .line
            .until(|frame| frame.get("method").is_some() && frame.get("id").is_some())
            .await;
        methods.push(request["method"].as_str().unwrap().to_owned());
        host.line
            .send(json!({"id": request["id"], "result": answer}))
            .await;
        host.line
            .until(|frame| frame["method"] == "serverRequest/resolved")
            .await;
    }
    assert_eq!(
        methods,
        [
            "item/commandExecution/requestApproval",
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
            "item/tool/requestUserInput",
            "item/permissions/requestApproval",
            "mcpServer/elicitation/request",
            "mcpServer/elicitation/request",
            "mcpServer/elicitation/request",
            "mcpServer/elicitation/request",
        ]
    );
    let done = host.completed().await;
    assert_eq!(done["params"]["turn"]["items"][0]["text"], "all asked");
    let commands = items(&host.line.frames, "commandExecution");
    assert_eq!(commands[0]["status"], "completed");
    assert_eq!(commands[1]["status"], "declined");
    let calls = items(&host.line.frames, "mcpToolCall");
    assert!(
        calls[1]["result"]
            .to_string()
            .contains("elicitation cancel")
    );
    assert_eq!(host.line.close().await, 0);
}

#[tokio::test]
async fn compaction_runs_as_its_own_turn_and_exit_ends_the_process() {
    let mut host = Host::start(json!({"steps": [
        {"text": {"chunks": ["bye"]}},
        {"exit": {"code": 4}},
    ]}))
    .await;
    let thread = host.thread.clone();
    host.call("thread/compact/start", json!({"threadId": thread}))
        .await;
    host.completed().await;
    assert_eq!(items(&host.line.frames, "contextCompaction").len(), 1);
    host.turn("Leave").await;
    host.line
        .until(|frame| frame["method"] == "item/completed")
        .await;
    assert_eq!(
        host.line
            .exited_within(std::time::Duration::from_secs(1))
            .await,
        4
    );
}

#[tokio::test]
async fn model_and_skill_lists_answer_from_the_script() {
    let mut host = Host::start(json!({"steps": []})).await;
    let models = host.call("model/list", json!({})).await;
    assert_eq!(
        models["result"],
        json!({"data": [{
            "id": "gpt-fake", "model": "gpt-fake", "displayName": "gpt-fake",
            "description": "The scripted model", "hidden": false,
            "supportedReasoningEfforts": [
                {"reasoningEffort": "low", "description": ""},
                {"reasoningEffort": "medium", "description": ""},
                {"reasoningEffort": "high", "description": ""},
            ],
            "defaultReasoningEffort": "medium", "isDefault": true,
        }], "nextCursor": null})
    );
    let skills = host.call("skills/list", json!({})).await;
    assert_eq!(skills["result"]["data"][0]["skills"], json!([]));

    let mut host = Host::start(json!({
        "steps": [],
        "models": [{"value": "gpt-a", "display_name": "GPT A", "efforts": ["low"]},
                   {"value": "gpt-b"}],
        "commands": [{"name": "review", "description": "Review the diff"}],
    }))
    .await;
    let models = host.call("model/list", json!({})).await;
    let data = models["result"]["data"].as_array().unwrap();
    assert_eq!(data.len(), 2);
    assert_eq!(data[0]["displayName"], "GPT A");
    assert_eq!(data[1]["supportedReasoningEfforts"], json!([]));
    let skills = host.call("skills/list", json!({})).await;
    let skill = &skills["result"]["data"][0]["skills"][0];
    assert_eq!(skill["name"], "review");
    assert_eq!(skill["description"], "Review the diff");
    assert_eq!(skill["scope"], "user");
}

#[tokio::test]
async fn account_and_session_facts_play_in_recorded_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let added = dir.path().join("docs/new.md");
    let mut host = Host::start(json!({
        "edit_files": true,
        "chunk_ms": 30,
        "steps": [
            {"usage": {"status": "allowed_warning", "windows": [
                {"name": "five_hour", "used_percent": 86.0, "resets_in_s": 3600},
                {"name": "seven_day", "used_percent": 40.0, "resets_in_s": 86400},
            ]}},
            {"text": {"chunks": ["Wri", "ting"]}},
            {"tool": {"name": "apply_patch", "class": "consequential", "input": [
                {"diff": "fresh\n", "kind": {"type": "add"}, "path": added},
            ]}},
            {"ask": {"question": {"questions": [
                {"question": "The token?", "header": "Token", "options": [], "other": true, "secret": true},
            ]}}},
            "turn_end",
            {"auth_failed": {"message": "unexpected status 401 Unauthorized: Missing bearer or basic authentication in header"}},
        ],
    }))
    .await;
    host.turn("Go").await;
    let limits = host
        .line
        .until(|frame| frame["method"] == "account/rateLimits/updated")
        .await;
    assert_eq!(limits["params"]["rateLimits"]["primary"]["usedPercent"], 86);
    let ask = host
        .line
        .until(|frame| frame["method"] == "item/tool/requestUserInput")
        .await;
    let question = &ask["params"]["questions"][0];
    assert_eq!(question["isSecret"], true);
    assert_eq!(question["isOther"], true);
    assert_eq!(std::fs::read_to_string(&added).unwrap(), "fresh\n");
    host.line
        .send(json!({"id": ask["id"], "result": {"answers": {"q0": {"answers": ["s3cret"]}}}}))
        .await;
    host.line
        .until(|frame| frame["method"] == "thread/tokenUsage/updated")
        .await;
    host.completed().await;
    host.turn("Again").await;
    let failed = host.completed().await;
    assert_eq!(failed["params"]["turn"]["status"], "failed");
    assert_eq!(host.line.close().await, 0);
}

/// fake-codex on a Unix socket, as several clients see it.
#[cfg(unix)]
mod socket {
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::{EXEMPT, Kind};
    use crate::support::{Client, Server};

    const FAKE: &str = env!("CARGO_BIN_EXE_fake-codex");

    /// A command that needs the operator's approval, then a reply.
    fn asks_twice() -> Value {
        json!({"steps": [
            {"ask": {"permission": {"class": "consequential", "input": {"command": "touch x"}}}},
            {"text": {"chunks": ["made x"]}},
            "turn_end",
            {"ask": {"permission": {"class": "consequential", "input": {"command": "rm x"}}}},
            {"text": {"chunks": ["removed x"]}},
            "turn_end",
        ]})
    }

    struct Peer {
        client: Client,
        name: &'static str,
        next_id: u64,
    }

    impl Peer {
        async fn connect(server: &Server, name: &'static str) -> Self {
            let mut peer = Peer {
                client: server.connect(Kind::Codex, EXEMPT).await,
                name,
                next_id: 0,
            };
            peer.call(
                "initialize",
                json!({"capabilities": {"experimentalApi": true},
                       "clientInfo": {"name": name, "title": null, "version": "0.1.0"}}),
            )
            .await;
            peer.client.send(json!({"method": "initialized"})).await;
            peer
        }

        async fn call(&mut self, method: &str, params: Value) -> Value {
            self.next_id += 1;
            let id = format!("{}-{}", self.name, self.next_id);
            self.client
                .send(json!({"id": id, "method": method, "params": params}))
                .await;
            self.client
                .until(|frame| frame["id"] == id && frame.get("method").is_none())
                .await
        }

        async fn start_thread(&mut self) -> String {
            let started = self
                .call(
                    "thread/start",
                    json!({"approvalPolicy": "on-request", "cwd": "/tmp", "model": "gpt-fake",
                           "sandbox": "workspace-write"}),
                )
                .await;
            started["result"]["thread"]["id"]
                .as_str()
                .unwrap()
                .to_owned()
        }

        async fn resume(&mut self, thread: &str) -> Value {
            self.call("thread/resume", json!({"threadId": thread}))
                .await
        }

        async fn turn(&mut self, thread: &str, text: &str) -> Value {
            self.call(
                "turn/start",
                json!({"threadId": thread, "input": [{"type": "text", "text": text}],
                       "clientUserMessageId": format!("client-message-{text}")}),
            )
            .await
        }

        async fn approval(&mut self) -> Value {
            self.client
                .until(|frame| {
                    frame["method"] == "item/commandExecution/requestApproval"
                        && frame.get("id").is_some()
                })
                .await
        }

        async fn answer(&mut self, request: &Value, decision: &str) {
            self.client
                .send(json!({"id": request["id"], "result": {"decision": decision}}))
                .await;
        }

        async fn resolved(&mut self, request: &Value) -> Value {
            let id = request["id"].clone();
            self.client
                .until(|frame| {
                    frame["method"] == "serverRequest/resolved"
                        && frame["params"]["requestId"] == id
                })
                .await
        }

        async fn completed(&mut self) -> Value {
            self.client
                .until(|frame| frame["method"] == "turn/completed")
                .await
        }

        fn said(&self, kind: &str) -> Vec<Value> {
            super::items(&self.client.frames, kind)
        }
    }

    #[tokio::test]
    async fn a_thread_with_no_turn_can_be_joined_once_named_and_not_before() {
        let server = Server::start(FAKE, json!({"steps": []}));
        let mut amux = Peer::connect(&server, "amux").await;
        let thread = amux.start_thread().await;
        let mut app = Peer::connect(&server, "app").await;
        let refused = app.resume(&thread).await;
        assert_eq!(refused["error"]["code"], -32600);
        assert_eq!(
            refused["error"]["message"],
            format!("no rollout found for thread id {thread}")
        );
        amux.call(
            "thread/name/set",
            json!({"threadId": thread, "name": "worker"}),
        )
        .await;
        let named = amux
            .client
            .until(|frame| frame["method"] == "thread/name/updated")
            .await;
        assert_eq!(named["params"]["threadName"], "worker");
        let joined = app.resume(&thread).await;
        assert_eq!(joined["result"]["thread"]["id"], thread.as_str());
    }

    /// Codex's own app resumes without the turns and pages them from the
    /// history on disk, which a thread that has run no turn does not have,
    /// named or not, until a client reads it with its turns. Codex 0.160.0
    /// refuses the app's resume until then, with this message.
    #[tokio::test]
    async fn codexs_app_joins_a_thread_with_no_turn_once_it_is_read_with_its_turns() {
        let server = Server::start(FAKE, json!({"steps": []}));
        let mut amux = Peer::connect(&server, "amux").await;
        let thread = amux.start_thread().await;
        amux.call(
            "thread/name/set",
            json!({"threadId": thread, "name": "worker"}),
        )
        .await;
        let mut app = Peer::connect(&server, "app").await;
        let paged = json!({"threadId": thread, "excludeTurns": true});
        let refused = app.call("thread/resume", paged.clone()).await;
        assert_eq!(refused["error"]["code"], -32600);
        assert_eq!(
            refused["error"]["message"],
            format!("invalid paginated history lineage for {thread}: missing source rollout")
        );
        let read = amux
            .call(
                "thread/read",
                json!({"threadId": thread, "includeTurns": true}),
            )
            .await;
        assert_eq!(read["result"]["thread"]["id"], thread.as_str());
        let joined = app.call("thread/resume", paged).await;
        assert_eq!(joined["result"]["thread"]["id"], thread.as_str());
    }

    #[tokio::test]
    async fn every_client_sees_the_turn_and_any_may_answer_its_approval() {
        let server = Server::start(FAKE, asks_twice());
        let mut amux = Peer::connect(&server, "amux").await;
        let thread = amux.start_thread().await;
        amux.call(
            "thread/name/set",
            json!({"threadId": thread, "name": "worker"}),
        )
        .await;
        let mut app = Peer::connect(&server, "app").await;
        app.resume(&thread).await;

        // amux prompts; the app answers the approval.
        amux.turn(&thread, "Make x").await;
        let asked = amux.approval().await;
        let seen = app.approval().await;
        assert_eq!(asked, seen, "both clients get the same request");
        app.answer(&seen, "accept").await;
        amux.resolved(&asked).await;
        app.resolved(&seen).await;
        // An answer to a resolved request changes nothing.
        amux.answer(&asked, "decline").await;
        for peer in [&mut amux, &mut app] {
            let done = peer.completed().await;
            assert_eq!(done["params"]["turn"]["items"][0]["text"], "made x");
            let commands = peer.said("commandExecution");
            assert_eq!(commands.last().unwrap()["status"], "completed");
        }

        // The app prompts; amux answers.
        app.turn(&thread, "Remove x").await;
        let asked = amux.approval().await;
        let seen = app.approval().await;
        amux.answer(&asked, "decline").await;
        app.resolved(&seen).await;
        amux.resolved(&asked).await;
        for peer in [&mut amux, &mut app] {
            peer.completed().await;
            let prompts = peer.said("userMessage");
            assert_eq!(prompts.last().unwrap()["content"][0]["text"], "Remove x");
            let commands = peer.said("commandExecution");
            assert_eq!(commands.last().unwrap()["status"], "declined");
        }
    }

    #[tokio::test]
    async fn a_client_that_leaves_leaves_its_turns_request_open() {
        let mut server = Server::start(FAKE, asks_twice());
        let mut amux = Peer::connect(&server, "amux").await;
        let thread = amux.start_thread().await;
        // Named, as amux names every thread, so the resume answer has the
        // shape Codex's recorded one has.
        amux.call(
            "thread/name/set",
            json!({"threadId": thread, "name": "worker"}),
        )
        .await;
        amux.turn(&thread, "Make x").await;
        let asked = amux.approval().await;
        amux.client.leave().await;

        // A client joining later is sent what is still waiting.
        let mut app = Peer::connect(&server, "app").await;
        let joined = app.resume(&thread).await;
        assert!(joined.get("result").is_some(), "{joined}");
        let seen = app.approval().await;
        assert_eq!(seen, asked);
        app.answer(&seen, "accept").await;
        app.resolved(&seen).await;
        let done = app.completed().await;
        assert_eq!(done["params"]["turn"]["status"], "completed");
        assert!(server.running(), "the server outlives its clients");
    }

    #[tokio::test]
    async fn a_client_that_skips_the_websocket_upgrade_is_hung_up_on() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let server = Server::start(FAKE, json!({"steps": []}));
        // Wait until it listens.
        drop(server.connect(Kind::Codex, EXEMPT).await);
        let mut raw = tokio::net::UnixStream::connect(&server.socket)
            .await
            .unwrap();
        raw.write_all(b"{\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
            .await
            .unwrap();
        let mut read = Vec::new();
        let ended = tokio::time::timeout(Duration::from_secs(5), raw.read_to_end(&mut read)).await;
        assert!(
            matches!(ended, Ok(Ok(_)) | Ok(Err(_))),
            "the fake left the connection open"
        );
        assert!(
            !String::from_utf8_lossy(&read).contains("\"result\""),
            "answered a line without the upgrade: {}",
            String::from_utf8_lossy(&read)
        );
    }

    /// `fake-codex resume THREAD --remote unix://…`, Codex's own app on the
    /// socket, typed into over a pipe.
    struct View {
        child: tokio::process::Child,
        stdin: tokio::process::ChildStdin,
        lines: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    }

    impl View {
        fn open(server: &Server, thread: &str) -> Self {
            let mut child = tokio::process::Command::new(FAKE)
                .args(["resume", thread, "--remote"])
                .arg(format!("unix://{}", server.socket.display()))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let stdin = child.stdin.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            use tokio::io::AsyncBufReadExt;
            let lines = tokio::io::BufReader::new(stdout).lines();
            View {
                child,
                stdin,
                lines,
            }
        }

        async fn type_line(&mut self, text: &str) {
            use tokio::io::AsyncWriteExt;
            self.stdin
                .write_all(format!("{text}\r").as_bytes())
                .await
                .unwrap();
        }

        /// Reads drawn lines until one starts with `prefix`.
        async fn drawn(&mut self, prefix: &str) -> String {
            loop {
                let line = tokio::time::timeout(crate::support::DEADLINE, self.lines.next_line())
                    .await
                    .unwrap_or_else(|_| panic!("the view never drew {prefix:?}"))
                    .unwrap()
                    .unwrap_or_else(|| panic!("the view ended before drawing {prefix:?}"));
                if line.starts_with(prefix) {
                    return line;
                }
            }
        }
    }

    #[tokio::test]
    async fn codexs_app_on_the_socket_prompts_and_answers_beside_amux() {
        let server = Server::start(FAKE, asks_twice());
        let mut amux = Peer::connect(&server, "amux").await;
        let thread = amux.start_thread().await;

        // Not joinable before a turn or a name, nor once named until the
        // thread is read with its turns, as with the real app.
        let refused = |message: &'static str| {
            let server = &server;
            let thread = &thread;
            async move {
                let mut early = View::open(server, thread);
                let drawn = early.drawn("fake codex:").await;
                assert!(drawn.contains(message), "{drawn}");
                let status = tokio::time::timeout(crate::support::DEADLINE, early.child.wait())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(status.code(), Some(1));
            }
        };
        refused("no rollout found").await;
        amux.call(
            "thread/name/set",
            json!({"threadId": thread, "name": "worker"}),
        )
        .await;
        refused("missing source rollout").await;
        amux.call(
            "thread/read",
            json!({"threadId": thread, "includeTurns": true}),
        )
        .await;
        let mut view = View::open(&server, &thread);
        view.drawn(&format!("fake codex resume {thread} in /tmp"))
            .await;

        // A prompt typed in the app; its approval answered from amux.
        view.type_line("Make x").await;
        let asked = amux.approval().await;
        let prompt = amux
            .client
            .frames
            .iter()
            .find(|frame| frame["params"]["item"]["type"] == "userMessage")
            .cloned()
            .unwrap();
        assert_eq!(prompt["params"]["item"]["content"][0]["text"], "Make x");
        view.drawn("approval 0: ").await;
        amux.answer(&asked, "accept").await;
        view.drawn("resolved 0").await;
        view.drawn("codex: made x").await;
        view.drawn("turn completed").await;
        amux.completed().await;

        // A prompt from amux; its approval answered in the app.
        amux.turn(&thread, "Remove x").await;
        view.drawn("user: Remove x").await;
        view.drawn("approval 1: ").await;
        let asked = amux.approval().await;
        view.type_line("n").await;
        amux.resolved(&asked).await;
        amux.completed().await;
        let commands = amux.said("commandExecution");
        assert_eq!(commands.last().unwrap()["status"], "declined");
        view.drawn("turn completed").await;
    }
}
