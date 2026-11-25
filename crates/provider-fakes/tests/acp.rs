//! fake-acp as a host sees it, over plain JSON-RPC on its stdio: every
//! frame it writes has a recorded shape and reads with nothing unknown,
//! in each of the ways its script can make it behave.

use std::path::{Path, PathBuf};

use provider_fakes::Kind;
use provider_fakes::playback::{self, Channel};
use provider_fakes::shape::Classifier;
use serde_json::{Value, json};
use support::Host as Line;

use crate::support;

/// A form whose fields are shaped as Antigravity's question (`agy-question`).
fn question_schema() -> Value {
    json!({
        "properties": {
            "q1": {
                "oneOf": [{"const": "1", "title": "Tea"}, {"const": "2", "title": "Coffee"}],
                "title": "Tea or coffee?",
                "type": "string"
            },
            "q1_other": {"description": "Anything else", "title": "Other", "type": "string"}
        },
        "type": "object"
    })
}

struct Agent {
    line: Line,
    next: u64,
    session: String,
    /// Where the fake logs each line it handles ([`provider_fakes::INPUT_LOG_ENV`]).
    handled: tempfile::TempDir,
}

impl Agent {
    /// A fake keeping its sessions in `sessions`.
    async fn spawn(script: Value, sessions: &Path) -> Self {
        let handled = scratch();
        let log = handled.path().join("input");
        let line = Line::spawn_with(
            Kind::Acp,
            source_tree::bin!("fake-acp"),
            &["acp"],
            script,
            &[],
            &[
                (provider_fakes::acp::SESSIONS_ENV, sessions.as_os_str()),
                (provider_fakes::INPUT_LOG_ENV, log.as_os_str()),
            ],
        )
        .await;
        Agent {
            line,
            next: 0,
            session: String::new(),
            handled,
        }
    }

    /// Returns once the fake has taken the request `id` in: it logs a line
    /// as it starts handling it, and handles it whole before its script
    /// goes on, so a gate opened after this cannot overtake it.
    async fn handled(&self, id: u64) {
        let log = self.handled.path().join("input");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            let taken = text.lines().any(|line| {
                serde_json::from_str::<Value>(line)
                    .is_ok_and(|frame| frame["id"] == id && frame.get("method").is_some())
            });
            if taken {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the fake never handled request {id}: {text}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// Initialized, with a new session.
    async fn started(script: Value, sessions: &Path) -> Self {
        let mut agent = Agent::spawn(script, sessions).await;
        agent.initialize().await;
        let opened = agent
            .call("session/new", json!({"cwd": "/tmp", "mcpServers": []}))
            .await;
        agent.session = opened["result"]["sessionId"].as_str().unwrap().to_owned();
        agent
    }

    async fn initialize(&mut self) -> Value {
        self.call(
            "initialize",
            json!({
                "clientCapabilities": {"elicitation": {"form": {}, "url": {}}},
                "clientInfo": {"name": "amux", "title": "amux", "version": "0.1.0"},
                "protocolVersion": 1
            }),
        )
        .await
    }

    async fn request(&mut self, method: &str, params: Value) -> u64 {
        self.next += 1;
        let id = self.next;
        self.line
            .send(json!({"id": id, "jsonrpc": "2.0", "method": method, "params": params}))
            .await;
        id
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.request(method, params).await;
        self.answer_to(id).await
    }

    async fn answer_to(&mut self, id: u64) -> Value {
        self.line
            .until(|frame| frame["id"] == id && frame.get("method").is_none())
            .await
    }

    async fn prompt(&mut self, text: &str) -> u64 {
        let session = self.session.clone();
        self.request(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]}),
        )
        .await
    }

    async fn cancel(&mut self) {
        let session = self.session.clone();
        self.line
            .send(json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session}}))
            .await;
    }

    /// Answer one of the agent's requests.
    async fn reply(&mut self, request: &Value, result: Value) {
        self.line
            .send(json!({"id": request["id"], "jsonrpc": "2.0", "result": result}))
            .await;
    }

    /// The next session update of this kind.
    async fn update(&mut self, kind: &str) -> Value {
        let frame = self
            .line
            .until(|frame| frame["params"]["update"]["sessionUpdate"] == kind)
            .await;
        frame["params"]["update"].clone()
    }

    /// The next request the agent sends.
    async fn asked(&mut self, method: &str) -> Value {
        self.line
            .until(|frame| frame["method"] == method && frame.get("id").is_some())
            .await
    }

    /// Every message chunk's text until the answer to `id`, and the answer.
    async fn said_until(&mut self, id: u64) -> (Vec<String>, Value) {
        let mut said = Vec::new();
        loop {
            let frame = self.line.next().await;
            if frame["id"] == id && frame.get("method").is_none() {
                return (said, frame);
            }
            if frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk" {
                said.push(
                    frame["params"]["update"]["content"]["text"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                );
            }
        }
    }

    /// The tool call updates until the answer to `id`: each one's status.
    async fn statuses_until(&mut self, id: u64) -> Vec<String> {
        let mut statuses = Vec::new();
        loop {
            let frame = self.line.next().await;
            if frame["id"] == id && frame.get("method").is_none() {
                assert_eq!(frame["result"]["stopReason"], "end_turn", "{frame}");
                return statuses;
            }
            let update = &frame["params"]["update"];
            if update["sessionUpdate"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("tool_call"))
            {
                statuses.push(update["status"].as_str().unwrap_or("-").to_owned());
            }
        }
    }
}

fn text(chunks: &[&str]) -> Value {
    json!({"text": {"chunks": chunks}})
}

fn wait_for(path: &Path) -> Value {
    json!({"wait_for": {"path": path}})
}

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ---- the corpus the fake is held to ----

/// The corpus check passes every frame the recorded agents wrote, so what
/// it refuses of the fake is the fake's own.
#[test]
fn every_recorded_agent_frame_passes_the_corpus_check() {
    let root = source_tree::crate_dir!().join("../interpret/fixtures/acp/live");
    let corpus = support::corpus(Kind::Acp);
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|dir| dir.join("io.jsonl").exists())
        .collect();
    dirs.sort();
    let mut failures = Vec::new();
    let mut frames = 0;
    for dir in &dirs {
        for process in playback::load(dir).unwrap() {
            let mut classifier = Classifier::default();
            for event in &process.events {
                let frame: Value = serde_json::from_slice(&event.bytes).unwrap();
                match event.channel {
                    Channel::Input => classifier.host(Kind::Acp, &frame),
                    Channel::Output => {
                        frames += 1;
                        let group = classifier.provider(Kind::Acp, &frame);
                        if let Err(drift) = corpus.check(Kind::Acp, &group, &frame) {
                            failures.push(format!("{}: {drift}", dir.display()));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    assert!(frames > 1000, "{frames} frames");
    // Each agent's frames are a whole of what that agent sends, but a
    // frame missing what every agent always sends would fail.
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn a_recording_of_two_processes_plays_as_two() {
    let recording = source_tree::crate_dir!().join("../interpret/fixtures/acp/live/agy-load");
    let processes = playback::load(&recording).unwrap();
    let transports: Vec<&str> = processes.iter().map(|p| p.transport.as_str()).collect();
    assert_eq!(transports, ["amux/1", "amux/2"]);
    assert!(processes.iter().all(|p| p.argv == ["acp"]));
}

// ---- what it says it is ----

#[tokio::test]
async fn it_names_itself_and_offers_every_capability_by_default() {
    let sessions = scratch();
    let mut agent = Agent::spawn(
        json!({"version": "2.3.4", "acp": {"name": "Scripted"}}),
        sessions.path(),
    )
    .await;
    let answer = agent.initialize().await["result"].clone();
    assert_eq!(
        answer["agentInfo"],
        json!({"name": "Scripted", "version": "2.3.4"})
    );
    assert_eq!(answer["protocolVersion"], 1);
    let caps = &answer["agentCapabilities"];
    assert_eq!(caps["loadSession"], true);
    assert_eq!(caps["sessionCapabilities"]["load"], json!({}));
    assert_eq!(caps["sessionCapabilities"]["resume"], json!({}));
    assert_eq!(
        caps["promptCapabilities"],
        json!({"image": true, "embeddedContext": true})
    );
}

#[tokio::test]
async fn an_anonymous_agent_sends_no_agent_info() {
    let sessions = scratch();
    let mut agent = Agent::spawn(json!({"acp": {"anonymous": true}}), sessions.path()).await;
    let answer = agent.initialize().await;
    assert!(answer["result"].get("agentInfo").is_none(), "{answer}");
}

#[tokio::test]
async fn reopening_is_offered_by_load_resume_both_or_neither() {
    for (load, resume) in [(true, false), (false, true), (true, true), (false, false)] {
        let sessions = scratch();
        let agent = Agent::started(
            json!({"acp": {"load": load, "resume": resume}}),
            sessions.path(),
        )
        .await;
        let mut again = Agent::spawn(
            json!({"acp": {"load": load, "resume": resume}}),
            sessions.path(),
        )
        .await;
        let caps = again.initialize().await["result"]["agentCapabilities"].clone();
        assert_eq!(caps["loadSession"], load);
        assert_eq!(caps["sessionCapabilities"].get("load").is_some(), load);
        assert_eq!(caps["sessionCapabilities"].get("resume").is_some(), resume);
        for (method, offered) in [("session/load", load), ("session/resume", resume)] {
            let reopened = again
                .call(
                    method,
                    json!({"sessionId": agent.session, "cwd": "/tmp", "mcpServers": []}),
                )
                .await;
            if offered {
                assert!(reopened.get("result").is_some(), "{method}: {reopened}");
            } else {
                assert_eq!(reopened["error"]["code"], -32601, "{method}: {reopened}");
            }
        }
        agent.line.close().await;
    }
}

#[tokio::test]
async fn an_agent_without_images_says_so_and_refuses_one() {
    let sessions = scratch();
    let mut agent = Agent::started(
        json!({"acp": {"image": false, "embedded_context": false}}),
        sessions.path(),
    )
    .await;
    let caps = agent.initialize().await["result"]["agentCapabilities"].clone();
    assert_eq!(
        caps["promptCapabilities"],
        json!({"image": false, "embeddedContext": false})
    );
    let session = agent.session.clone();
    let refused = agent
        .call(
            "session/prompt",
            json!({"sessionId": session, "prompt": [
                {"type": "text", "text": "what colour?"},
                {"type": "image", "mimeType": "image/png", "data": "iVBORw0KGgo="}
            ]}),
        )
        .await;
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
}

#[tokio::test]
async fn signed_out_it_refuses_a_session_as_grok_does() {
    let sessions = scratch();
    let mut agent = Agent::spawn(json!({"signed_out": true}), sessions.path()).await;
    agent.initialize().await;
    let refused = agent
        .call("session/new", json!({"cwd": "/tmp", "mcpServers": []}))
        .await;
    assert_eq!(
        refused["error"],
        json!({"code": -32000, "message": "Authentication required", "data": "no auth method id provided"})
    );
}

// ---- what the session offers ----

#[tokio::test]
async fn the_session_offers_its_models_thought_levels_and_modes() {
    let sessions = scratch();
    let script = json!({
        "models": [
            {"value": "big", "display_name": "Big", "efforts": ["low", "high"], "default_effort": "high"},
            {"value": "small", "display_name": "Small"}
        ],
        "acp": {"thought_level_id": "effort"}
    });
    let mut agent = Agent::spawn(script, sessions.path()).await;
    agent.initialize().await;
    let opened = agent
        .call("session/new", json!({"cwd": "/tmp", "mcpServers": []}))
        .await;
    agent.session = opened["result"]["sessionId"].as_str().unwrap().to_owned();
    let options = opened["result"]["configOptions"]
        .as_array()
        .unwrap()
        .clone();
    let ids: Vec<(&str, &str, &str)> = options
        .iter()
        .map(|option| {
            (
                option["id"].as_str().unwrap(),
                option["category"].as_str().unwrap(),
                option["currentValue"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        ids,
        [
            ("model", "model", "big"),
            ("effort", "thought_level", "high"),
            ("mode", "mode", "default")
        ]
    );
    let session = agent.session.clone();
    let changed = agent
        .call(
            "session/set_config_option",
            json!({"sessionId": session, "configId": "effort", "value": "low"}),
        )
        .await;
    assert_eq!(changed["result"]["configOptions"][1]["currentValue"], "low");
    // A model without thought levels has no such option.
    let changed = agent
        .call(
            "session/set_config_option",
            json!({"sessionId": session, "configId": "model", "value": "small"}),
        )
        .await;
    let categories: Vec<&str> = changed["result"]["configOptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["category"].as_str().unwrap())
        .collect();
    assert_eq!(categories, ["model", "mode"]);
    let refused = agent
        .call(
            "session/set_config_option",
            json!({"sessionId": session, "configId": "model", "value": "huge"}),
        )
        .await;
    assert_eq!(refused["error"]["code"], -32602);
    let id = agent
        .request(
            "session/set_mode",
            json!({"sessionId": session, "modeId": "plan"}),
        )
        .await;
    assert_eq!(
        agent.update("current_mode_update").await["currentModeId"],
        "plan"
    );
    let updated = agent.update("config_option_update").await;
    assert_eq!(updated["configOptions"][1]["currentValue"], "plan");
    assert_eq!(agent.answer_to(id).await["result"], json!({}));
}

#[tokio::test]
async fn an_agent_without_modes_offers_no_mode_option() {
    let sessions = scratch();
    let mut agent = Agent::spawn(json!({"acp": {"modes": []}}), sessions.path()).await;
    agent.initialize().await;
    let opened = agent
        .call("session/new", json!({"cwd": "/tmp", "mcpServers": []}))
        .await;
    let categories: Vec<&str> = opened["result"]["configOptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["category"].as_str().unwrap())
        .collect();
    assert_eq!(categories, ["model", "thought_level"]);
}

#[tokio::test]
async fn the_session_offers_its_commands() {
    let sessions = scratch();
    let mut agent = Agent::spawn(
        json!({"commands": [
            {"name": "compact", "description": "Compact the conversation"},
            {"name": "goal", "description": "Set a goal", "argument_hint": "<objective>"}
        ]}),
        sessions.path(),
    )
    .await;
    agent.initialize().await;
    agent
        .call("session/new", json!({"cwd": "/tmp", "mcpServers": []}))
        .await;
    let offered = agent.update("available_commands_update").await;
    assert_eq!(
        offered["availableCommands"],
        json!([
            {"name": "compact", "description": "Compact the conversation"},
            {"name": "goal", "description": "Set a goal", "input": {"hint": "<objective>"}}
        ])
    );
}

// ---- a turn ----

#[tokio::test]
async fn a_turn_streams_thoughts_and_messages_and_ends_its_turn() {
    let sessions = scratch();
    let mut agent = Agent::started(
        json!({"steps": [{"thinking": {"text": "hmm"}}, text(&["Hel", "lo"]), "turn_end"]}),
        sessions.path(),
    )
    .await;
    let prompt = agent.prompt("hi").await;
    assert_eq!(
        agent.update("agent_thought_chunk").await["content"]["text"],
        "hmm"
    );
    let (said, answer) = agent.said_until(prompt).await;
    assert_eq!(said, ["Hel", "lo"]);
    assert_eq!(answer["result"], json!({"stopReason": "end_turn"}));
}

#[tokio::test]
async fn a_turn_ends_with_the_stop_reason_it_names() {
    let sessions = scratch();
    let mut agent = Agent::started(
        json!({"steps": [text(&["cut"]), {"stop": {"reason": "max_tokens"}}]}),
        sessions.path(),
    )
    .await;
    let prompt = agent.prompt("go on").await;
    let (_, answer) = agent.said_until(prompt).await;
    assert_eq!(answer["result"]["stopReason"], "max_tokens");
}

#[tokio::test]
async fn a_turn_reports_usage_and_a_title() {
    let sessions = scratch();
    let mut agent = Agent::started(
        json!({"steps": [
            {"title": {"title": "Greeting"}},
            {"context": {"used": 9038, "size": 1048576}},
            "turn_end"
        ]}),
        sessions.path(),
    )
    .await;
    let prompt = agent.prompt("hi").await;
    assert_eq!(
        agent.update("session_info_update").await["title"],
        "Greeting"
    );
    let usage = agent.update("usage_update").await;
    assert_eq!(
        (usage["used"].clone(), usage["size"].clone()),
        (json!(9038), json!(1048576))
    );
    agent.answer_to(prompt).await;
}

#[tokio::test]
async fn a_tool_call_reports_its_kind_locations_and_diff_and_edits_the_file() {
    let sessions = scratch();
    let work = scratch();
    let file = work.path().join("greeting.txt");
    std::fs::write(&file, "hello\n").unwrap();
    let path = file.to_string_lossy().into_owned();
    let mut agent = Agent::started(
        json!({
            "edit_files": true,
            "steps": [{"call": {
                "kind": "edit",
                "title": "File greeting.txt",
                "input": {"Path": path},
                "locations": [path],
                "diff": {"path": path, "old_text": "hello\n", "new_text": "goodbye\n"}
            }}, "turn_end"]
        }),
        sessions.path(),
    )
    .await;
    let prompt = agent.prompt("edit it").await;
    let call = agent.update("tool_call").await;
    assert_eq!(
        (call["kind"].clone(), call["status"].clone()),
        (json!("edit"), json!("pending"))
    );
    assert_eq!(call["locations"], json!([{"path": path}]));
    assert_eq!(
        agent.update("tool_call_update").await["status"],
        "in_progress"
    );
    let done = agent.update("tool_call_update").await;
    assert_eq!(done["status"], "completed");
    assert_eq!(
        done["content"],
        json!([{"type": "diff", "path": path, "oldText": "hello\n", "newText": "goodbye\n"}])
    );
    agent.answer_to(prompt).await;
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "goodbye\n");
}

#[tokio::test]
async fn a_failing_call_ends_failed() {
    let sessions = scratch();
    let mut agent = Agent::started(
        json!({"steps": [
            {"call": {"kind": "execute", "title": "make", "input": {"command": "make"},
                      "outcome": {"output": "no rule", "error": true}}},
            "turn_end"
        ]}),
        sessions.path(),
    )
    .await;
    let prompt = agent.prompt("build").await;
    assert_eq!(
        agent.statuses_until(prompt).await,
        ["pending", "in_progress", "failed"]
    );
}

// ---- asks ----

#[tokio::test]
async fn a_permission_offers_antigravitys_options_and_acts_on_the_answer() {
    let sessions = scratch();
    let ask = json!({"call": {"kind": "execute", "title": "./make-file.sh", "input": {"command": "./make-file.sh"},
                              "ask": "once", "outcome": {"output": "made"}}});
    let mut agent = Agent::started(
        json!({"steps": [ask, "turn_end", ask, "turn_end"]}),
        sessions.path(),
    )
    .await;
    for (option, last) in [("allow", "completed"), ("deny", "failed")] {
        let prompt = agent.prompt("run it").await;
        let asked = agent.asked("session/request_permission").await;
        let offered: Vec<(&str, &str)> = asked["params"]["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|option| {
                (
                    option["kind"].as_str().unwrap(),
                    option["optionId"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(offered, [("allow_once", "allow"), ("reject_once", "deny")]);
        assert_eq!(asked["params"]["toolCall"]["title"], "./make-file.sh");
        agent
            .reply(
                &asked,
                json!({"outcome": {"outcome": "selected", "optionId": option}}),
            )
            .await;
        let statuses = agent.statuses_until(prompt).await;
        assert_eq!(statuses.last().unwrap(), last, "{option}: {statuses:?}");
    }
}

#[tokio::test]
async fn allowed_always_a_call_is_not_asked_about_again() {
    let sessions = scratch();
    let ask = json!({"call": {"kind": "execute", "title": "./again.sh", "ask": "always"}});
    let mut agent = Agent::started(
        json!({"steps": [ask, "turn_end", ask, "turn_end"]}),
        sessions.path(),
    )
    .await;
    let prompt = agent.prompt("run it").await;
    let asked = agent.asked("session/request_permission").await;
    let kinds: Vec<&str> = asked["params"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["allow_once", "allow_always", "reject_once"]);
    agent
        .reply(
            &asked,
            json!({"outcome": {"outcome": "selected", "optionId": "always"}}),
        )
        .await;
    agent.statuses_until(prompt).await;
    let prompt = agent.prompt("again").await;
    assert_eq!(
        agent.statuses_until(prompt).await,
        ["pending", "in_progress", "completed"]
    );
}

#[tokio::test]
async fn a_generic_permission_ask_is_an_acp_permission_request() {
    let sessions = scratch();
    let mut agent = Agent::started(
        json!({"steps": [{"ask": {"permission": {"class": "consequential", "input": {"command": "rm x"}}}}, "turn_end"]}),
        sessions.path(),
    )
    .await;
    let prompt = agent.prompt("go").await;
    let asked = agent.asked("session/request_permission").await;
    assert_eq!(asked["params"]["toolCall"]["kind"], "execute");
    agent
        .reply(&asked, json!({"outcome": {"outcome": "cancelled"}}))
        .await;
    assert_eq!(agent.statuses_until(prompt).await.last().unwrap(), "failed");
}

#[tokio::test]
async fn a_form_asks_through_elicitation_and_goes_on_once_answered() {
    let sessions = scratch();
    let mut agent = Agent::started(
        json!({"steps": [
            {"ask": {"form": {"server": "agent", "message": "The agent has a question.", "schema": question_schema()}}},
            text(&["Tea"]),
            "turn_end"
        ]}),
        sessions.path(),
    )
    .await;
    let prompt = agent.prompt("ask me").await;
    let asked = agent.asked("elicitation/create").await;
    assert_eq!(asked["params"]["mode"], "form");
    assert_eq!(asked["params"]["requestedSchema"], question_schema());
    assert_eq!(asked["params"]["sessionId"], agent.session.as_str());
    agent
        .reply(&asked, json!({"action": "accept", "content": {"q1": "1"}}))
        .await;
    let (said, _) = agent.said_until(prompt).await;
    assert_eq!(said, ["Tea"]);
}

/// Questions go as Grok asks them, in its own request with a number for an
/// id, and the call completes on the labels picked.
#[tokio::test]
async fn questions_ask_through_groks_request_and_go_on_once_answered() {
    let sessions = scratch();
    let mut agent = Agent::started(
        json!({"steps": [
            {"ask": {"question": {"questions": [{
                "question": "Do you prefer tea or coffee?", "header": "",
                "options": [{"label": "Tea", "description": "You prefer tea."},
                            {"label": "Coffee", "description": "You prefer coffee."}]
            }]}}},
            "turn_end"
        ]}),
        sessions.path(),
    )
    .await;
    let prompt = agent.prompt("ask me").await;
    let asked = agent.asked("_x.ai/ask_user_question").await;
    assert!(asked["id"].is_u64(), "{asked}");
    assert_eq!(asked["params"]["sessionId"], agent.session.as_str());
    assert_eq!(
        asked["params"]["questions"][0]["options"][1]["label"],
        "Coffee"
    );
    assert_eq!(asked["params"]["questions"][0]["multiSelect"], Value::Null);
    agent
        .reply(
            &asked,
            json!({"outcome": "accepted", "answers": {"Do you prefer tea or coffee?": "Tea"}}),
        )
        .await;
    assert_eq!(agent.statuses_until(prompt).await, ["completed"]);
}

#[tokio::test]
async fn a_tool_server_call_names_its_server_and_asks_for_it() {
    let sessions = scratch();
    let mut agent = Agent::started(
        json!({"steps": [
            {"tool_server": {"server": "amux", "tool": "start_subagent", "input": {"task": "count"}, "ask": "once",
                             "outcome": {"output": "started"}}},
            "turn_end"
        ]}),
        sessions.path(),
    )
    .await;
    let prompt = agent.prompt("spawn a child").await;
    let call = agent.update("tool_call").await;
    assert_eq!(call["name"], "mcp_amux_start_subagent");
    assert_eq!(call["kind"], "other");
    let asked = agent.asked("session/request_permission").await;
    assert_eq!(
        asked["params"]["toolCall"]["name"],
        "mcp_amux_start_subagent"
    );
    assert_eq!(
        asked["params"]["toolCall"]["title"],
        "Run mcp_amux_start_subagent"
    );
    agent
        .reply(
            &asked,
            json!({"outcome": {"outcome": "selected", "optionId": "allow"}}),
        )
        .await;
    assert_eq!(
        agent.statuses_until(prompt).await,
        ["in_progress", "completed"]
    );
}

// ---- cancel ----

#[tokio::test]
async fn a_cancel_streams_its_trailing_chunks_then_answers_cancelled() {
    let sessions = scratch();
    let gate = scratch();
    let mut agent = Agent::started(
        json!({"steps": [
            text(&["one"]),
            wait_for(&gate.path().join("never")),
            text(&["two", "three", "four"]),
            "turn_end",
            text(&["after"]),
            "turn_end"
        ]}),
        sessions.path(),
    )
    .await;
    let prompt = agent.prompt("story").await;
    agent.update("agent_message_chunk").await;
    agent.cancel().await;
    let (said, answer) = agent.said_until(prompt).await;
    assert_eq!(said, ["two", "three"]);
    assert_eq!(answer["result"]["stopReason"], "cancelled");
    let prompt = agent.prompt("again").await;
    let (said, answer) = agent.said_until(prompt).await;
    assert_eq!(said, ["after"]);
    assert_eq!(answer["result"]["stopReason"], "end_turn");
}

#[tokio::test]
async fn the_trailing_chunks_after_a_cancel_are_chosen() {
    let sessions = scratch();
    let gate = scratch();
    let mut agent = Agent::started(
        json!({
            "acp": {"cancel_chunks": 0},
            "steps": [text(&["one"]), wait_for(&gate.path().join("never")), text(&["two"]), "turn_end"]
        }),
        sessions.path(),
    )
    .await;
    let prompt = agent.prompt("story").await;
    agent.update("agent_message_chunk").await;
    agent.cancel().await;
    let (said, answer) = agent.said_until(prompt).await;
    assert!(said.is_empty(), "{said:?}");
    assert_eq!(answer["result"]["stopReason"], "cancelled");
}

#[tokio::test]
async fn a_cancel_during_a_permission_ask_fails_the_call() {
    let sessions = scratch();
    let mut agent = Agent::started(
        json!({"steps": [{"call": {"kind": "execute", "title": "rm", "ask": "once"}}, "turn_end"]}),
        sessions.path(),
    )
    .await;
    let prompt = agent.prompt("go").await;
    let asked = agent.asked("session/request_permission").await;
    agent.cancel().await;
    agent
        .reply(&asked, json!({"outcome": {"outcome": "cancelled"}}))
        .await;
    let (_, answer) = agent.said_until(prompt).await;
    assert_eq!(answer["result"]["stopReason"], "cancelled");
}

// ---- reopening a session in a new process ----

#[tokio::test]
async fn a_loaded_session_replays_its_history_first() {
    let sessions = scratch();
    let mut first = Agent::started(
        json!({"steps": [text(&["o", "k"]), "turn_end"]}),
        sessions.path(),
    )
    .await;
    let prompt = first.prompt("Remember kiwi").await;
    first.answer_to(prompt).await;
    // Killed, not closed: the session must survive its process.
    drop(first.line);
    let mut second = Agent::spawn(
        json!({"steps": [text(&["kiwi"]), "turn_end"]}),
        sessions.path(),
    )
    .await;
    second.initialize().await;
    let id = second
        .request(
            "session/load",
            json!({"sessionId": first.session, "cwd": "/tmp", "mcpServers": []}),
        )
        .await;
    let mut replayed = Vec::new();
    let answer = loop {
        let frame = second.line.next().await;
        if frame["id"] == id {
            break frame;
        }
        let update = &frame["params"]["update"];
        replayed.push((
            update["sessionUpdate"].as_str().unwrap().to_owned(),
            update["content"]["text"].as_str().unwrap().to_owned(),
        ));
    };
    assert!(answer["result"]["configOptions"].is_array(), "{answer}");
    assert_eq!(
        replayed,
        [
            ("user_message_chunk".to_owned(), "Remember kiwi".to_owned()),
            ("agent_message_chunk".to_owned(), "ok".to_owned())
        ]
    );
    second.session = first.session.clone();
    let prompt = second.prompt("Which word?").await;
    let (said, _) = second.said_until(prompt).await;
    assert_eq!(said, ["kiwi"]);
}

#[tokio::test]
async fn a_resumed_session_replays_nothing_and_keeps_its_settings() {
    let sessions = scratch();
    let script = json!({"models": [{"value": "a"}, {"value": "b"}]});
    let mut first = Agent::started(script.clone(), sessions.path()).await;
    let session = first.session.clone();
    first
        .call(
            "session/set_config_option",
            json!({"sessionId": session, "configId": "model", "value": "b"}),
        )
        .await;
    drop(first.line);
    let mut second = Agent::spawn(script, sessions.path()).await;
    second.initialize().await;
    let id = second
        .request(
            "session/resume",
            json!({"sessionId": session, "cwd": "/tmp", "mcpServers": []}),
        )
        .await;
    let answer = second.line.next().await;
    assert_eq!(answer["id"], id, "the answer comes first: {answer}");
    assert_eq!(answer["result"]["configOptions"][0]["currentValue"], "b");
}

#[tokio::test]
async fn an_agent_that_forgets_reopens_with_its_defaults() {
    let sessions = scratch();
    let script =
        json!({"models": [{"value": "a"}, {"value": "b"}], "acp": {"forgets_settings": true}});
    let mut first = Agent::started(script.clone(), sessions.path()).await;
    let session = first.session.clone();
    first
        .call(
            "session/set_config_option",
            json!({"sessionId": session, "configId": "model", "value": "b"}),
        )
        .await;
    drop(first.line);
    let mut second = Agent::spawn(script, sessions.path()).await;
    second.initialize().await;
    let answer = second
        .call(
            "session/resume",
            json!({"sessionId": session, "cwd": "/tmp", "mcpServers": []}),
        )
        .await;
    assert_eq!(answer["result"]["configOptions"][0]["currentValue"], "a");
}

#[tokio::test]
async fn reopening_an_unknown_or_refused_session_is_an_error() {
    let sessions = scratch();
    let first = Agent::started(json!({}), sessions.path()).await;
    let known = first.session.clone();
    drop(first.line);
    for (script, session) in [
        (json!({}), "no-such-session".to_owned()),
        (json!({"acp": {"refuse_reopen": true}}), known),
    ] {
        let mut agent = Agent::spawn(script, sessions.path()).await;
        agent.initialize().await;
        for method in ["session/load", "session/resume"] {
            let refused = agent
                .call(
                    method,
                    json!({"sessionId": session, "cwd": "/tmp", "mcpServers": []}),
                )
                .await;
            assert_eq!(refused["error"]["code"], -32002, "{method}: {refused}");
        }
    }
}

// ---- a prompt sent mid-turn ----

/// A first turn that holds at `gate` between two messages, and a second.
fn two_turns(mid_turn: &str, gate: &Path) -> Value {
    json!({
        "acp": {"mid_turn": mid_turn},
        "steps": [text(&["one"]), wait_for(gate), text(&["two"]), "turn_end", text(&["three"]), "turn_end"]
    })
}

#[tokio::test]
async fn a_steering_agent_answers_the_first_prompt_and_goes_on_under_the_second() {
    let sessions = scratch();
    let gate = scratch();
    let gate = gate.path().join("gate");
    let mut agent = Agent::started(two_turns("steer", &gate), sessions.path()).await;
    let first = agent.prompt("story").await;
    agent.update("agent_message_chunk").await;
    let second = agent.prompt("and then").await;
    let answered = agent.answer_to(first).await;
    assert_eq!(answered["result"]["stopReason"], "end_turn");
    std::fs::write(&gate, "").unwrap();
    let (said, _) = agent.said_until(second).await;
    assert_eq!(said, ["two"]);
}

#[tokio::test]
async fn a_queueing_agent_runs_the_second_prompt_as_its_own_turn() {
    let sessions = scratch();
    let gate = scratch();
    let gate = gate.path().join("gate");
    let mut agent = Agent::started(two_turns("queue", &gate), sessions.path()).await;
    let first = agent.prompt("story").await;
    agent.update("agent_message_chunk").await;
    let second = agent.prompt("and then").await;
    std::fs::write(&gate, "").unwrap();
    let (said, _) = agent.said_until(first).await;
    assert_eq!(said, ["two"]);
    let (said, _) = agent.said_until(second).await;
    assert_eq!(said, ["three"]);
}

#[tokio::test]
async fn an_agent_running_both_in_one_loop_answers_both_together() {
    let sessions = scratch();
    let gate = scratch();
    let gate = gate.path().join("gate");
    let mut agent = Agent::started(two_turns("together", &gate), sessions.path()).await;
    let first = agent.prompt("story").await;
    agent.update("agent_message_chunk").await;
    let second = agent.prompt("and then").await;
    // Joined only if it reached the fake while the turn still ran.
    agent.handled(second).await;
    std::fs::write(&gate, "").unwrap();
    let (said, answer) = agent.said_until(first).await;
    assert_eq!(said, ["two", "three"]);
    let next = agent.line.next().await;
    assert_eq!(next["id"], second, "answered straight after: {next}");
    assert_eq!(next["result"], answer["result"]);
}

// ---- scripts ----

#[tokio::test]
async fn a_script_asking_what_acp_cannot_raise_is_refused() {
    let sessions = scratch();
    let agent = Agent::spawn(
        json!({"steps": [{"ask": {"plan": {"markdown": "# plan"}}}]}),
        sessions.path(),
    )
    .await;
    assert_eq!(
        agent
            .line
            .exited_within(std::time::Duration::from_secs(20))
            .await,
        provider_fakes::DRIFT_EXIT
    );
}

#[test]
fn only_the_acp_fake_plays_acp_steps() {
    let script: provider_fakes::Script =
        serde_json::from_str(r#"{"steps": [{"title": {"title": "t"}}]}"#).unwrap();
    script
        .check(
            "an ACP agent",
            provider_fakes::acp::RAISES,
            provider_fakes::acp::PLAYS,
        )
        .unwrap();
    assert!(
        script
            .check(
                "Codex",
                provider_fakes::codex::RAISES,
                provider_fakes::codex::PLAYS
            )
            .is_err()
    );
}
