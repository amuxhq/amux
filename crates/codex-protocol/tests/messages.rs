//! Recorded lines read into the fields amux uses, and what amux writes
//! comes out as the bytes it has always written.

use std::path::Path;

use codex_protocol::client::{
    ClientInfo, InitializeParams, ModelListParams, ThreadStartParams, TurnStartParams,
};
use codex_protocol::items::{CommandAction, ThreadItem, ToolStatus, UserInput, WebSearchAction};
use codex_protocol::server::{
    CommandDecision, Decision, Persist, ServerNotification as N, ServerRequest as R, ThreadResponse,
};
use codex_protocol::thread::{ModeKind, ReasoningEffort, SandboxMode, TurnStatus};
use codex_protocol::{
    ClientMessage, ClientNotification, ClientRequest, ClientResponse, Extra, RequestId,
    ServerMessage,
};
use serde_json::{Value, json};

/// The first line Codex sent in a recording that contains every needle.
fn recorded(spec: &str, needles: &[&str]) -> ServerMessage {
    codex_protocol::strict(recorded_line(spec, needles).as_bytes()).expect("known")
}

fn recorded_line(spec: &str, needles: &[&str]) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../codex-specs/fixtures/runtime")
        .join(spec)
        .join("io.jsonl");
    let text = std::fs::read_to_string(&path).expect("recording");
    text.lines()
        .map(|record| serde_json::from_str::<Value>(record).expect("record"))
        .filter(|record| record["dir"] == "stdout")
        .map(|record| record["line"].as_str().expect("line").to_owned())
        .find(|line| needles.iter().all(|needle| line.contains(needle)))
        .unwrap_or_else(|| panic!("{spec}: no line with {needles:?}"))
}

fn notification(message: ServerMessage) -> N {
    match message {
        ServerMessage::Notification { notification, .. } => notification,
        other => panic!("not a notification: {other:?}"),
    }
}

fn request(message: ServerMessage) -> R {
    match message {
        ServerMessage::Request { request, .. } => request,
        other => panic!("not a request: {other:?}"),
    }
}

fn completed_item(spec: &str, kind: &str) -> ThreadItem {
    match notification(recorded(
        spec,
        &["item/completed", &format!(r#""type":"{kind}""#)],
    )) {
        N::ItemCompleted(completed) => completed.item,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_command_approval_reads_its_offers() {
    let R::CommandApproval(ask) = request(recorded(
        "approval_scopes",
        &["commandExecution/requestApproval"],
    )) else {
        panic!("not a command approval");
    };
    assert!(ask.command.is_some_and(|command| command.contains("touch")));
    assert!(ask.reason.is_some());
    let offered = ask.available_decisions.expect("offers");
    assert!(offered.contains(&CommandDecision::Plain(Decision::Accept)));
    assert!(offered.iter().any(|decision| matches!(
        decision,
        CommandDecision::AcceptWithExecpolicyAmendment { .. }
    )));
    assert!(
        ask.proposed_execpolicy_amendment
            .is_some_and(|prefix| !prefix.is_empty())
    );
    assert!(matches!(
        ask.command_actions.as_deref(),
        Some([CommandAction::Unrecognized(_), ..])
    ));
}

#[test]
fn a_permissions_request_reads_the_access_asked_for() {
    let R::PermissionsApproval(ask) =
        request(recorded("access_grant", &["permissions/requestApproval"]))
    else {
        panic!("not a permissions request");
    };
    assert_eq!(
        ask.permissions
            .network
            .flatten()
            .and_then(|network| network.enabled),
        Some(true)
    );
    // Codex writes the part it does not ask for as null.
    assert_eq!(ask.permissions.file_system, Some(None));
}

#[test]
fn a_question_reads_its_options() {
    let R::RequestUserInput(ask) = request(recorded("plan_mode", &["requestUserInput"])) else {
        panic!("not a question");
    };
    let question = &ask.questions[0];
    assert_eq!(question.id, "color");
    assert_eq!(question.is_other, Some(true));
    assert_eq!(question.is_secret, Some(false));
    assert_eq!(question.options.as_ref().map(Vec::len), Some(2));
}

#[test]
fn an_elicitation_reads_its_meta_and_schema() {
    let R::Elicitation(ask) = request(recorded(
        "tool_server_form",
        &["elicitation/request", "persist"],
    )) else {
        panic!("not an elicitation");
    };
    let meta = ask.meta.expect("meta");
    assert_eq!(
        meta.persist.as_ref().map(Persist::scopes),
        Some(&["session".to_owned(), "always".to_owned()][..])
    );
    assert_eq!(meta.tool_params, Some(json!({ "word": "BLUE" })));
    assert_eq!(
        ask.requested_schema,
        Some(json!({ "properties": {}, "type": "object" }))
    );
    assert_eq!(ask.mode, "form");
}

/// Codex writes `persist` as one string when it offers one lifetime and as
/// a list when it offers both; each reads and writes back as it came.
#[test]
fn an_elicitation_reads_persist_as_a_string_or_a_list() {
    let recorded = recorded_line("tool_server_form", &["elicitation/request", "persist"]);
    let list = r#""persist":["session","always"]"#.to_owned();
    assert!(recorded.contains(&list), "{recorded}");
    for (written, scopes) in [
        (list.clone(), vec!["session", "always"]),
        (r#""persist":"session""#.to_owned(), vec!["session"]),
        (r#""persist":"always""#.to_owned(), vec!["always"]),
    ] {
        let line = recorded.replace(&list, &written);
        let message = codex_protocol::strict(line.as_bytes()).expect("known");
        let R::Elicitation(ask) = request(message.clone()) else {
            panic!("not an elicitation: {line}");
        };
        let persist = ask.meta.and_then(|meta| meta.persist).expect("persist");
        assert_eq!(persist.scopes(), &scopes[..], "{written}");
        let again: Value =
            serde_json::from_slice(&codex_protocol::encode_server(&message)).unwrap();
        assert_eq!(
            again,
            serde_json::from_str::<Value>(&line).unwrap(),
            "{written}"
        );
    }
}

#[test]
fn a_tool_call_reads_its_arguments() {
    let R::ToolCall(call) = request(recorded("dynamic_tools", &["item/tool/call"])) else {
        panic!("not a tool call");
    };
    assert_eq!(call.tool, "send");
    assert_eq!(call.arguments["to"], "probe");
}

#[test]
fn a_failed_turn_reads_its_error_kind() {
    let N::TurnCompleted(done) = notification(recorded("turn_error", &["turn/completed"])) else {
        panic!("not a turn completion");
    };
    assert_eq!(done.turn.status, TurnStatus::Failed);
    let error = done.turn.error.expect("error");
    assert_eq!(error.codex_error_info.expect("info").kind(), "other");
    assert!(error.message.contains("not supported"));
}

#[test]
fn rate_limits_and_token_usage_read_their_numbers() {
    let N::AccountRateLimitsUpdated(limits) = notification(recorded(
        "access_grant",
        &["rateLimits/updated", r#""primary":{"#],
    )) else {
        panic!("not rate limits");
    };
    let limits = limits.rate_limits;
    assert_eq!(limits.limit_id.as_deref(), Some("codex"));
    assert!(
        limits
            .primary
            .expect("primary")
            .window_duration_mins
            .is_some()
    );
    assert!(!limits.credits.expect("credits").unlimited);
    assert!(limits.plan_type.is_some());

    let N::ThreadTokenUsageUpdated(usage) =
        notification(recorded("access_grant", &["tokenUsage/updated"]))
    else {
        panic!("not token usage");
    };
    assert!(usage.token_usage.last.total_tokens > 0);
    assert!(usage.token_usage.model_context_window.is_some());
}

#[test]
fn thread_settings_read_mode_effort_and_sandbox() {
    let N::ThreadSettingsUpdated(update) =
        notification(recorded("plan_mode", &["thread/settings/updated"]))
    else {
        panic!("not settings");
    };
    let settings = update.thread_settings;
    assert_eq!(settings.collaboration_mode.mode, ModeKind::Plan);
    assert!(
        settings
            .collaboration_mode
            .settings
            .developer_instructions
            .is_some()
    );
    assert!(settings.sandbox_policy.mode().is_some());
}

#[test]
fn completed_items_read_their_outcomes() {
    let ThreadItem::CommandExecution(command) = completed_item("access_grant", "commandExecution")
    else {
        panic!("not a command");
    };
    assert_eq!(command.exit_code, Some(0));
    assert!(
        command
            .aggregated_output
            .is_some_and(|output| output.contains("HTTP"))
    );
    assert!(command.duration_ms.is_some());

    let ThreadItem::McpToolCall(call) = completed_item("tool_server_form", "mcpToolCall") else {
        panic!("not an MCP call");
    };
    assert_eq!(call.status, ToolStatus::Completed);
    assert_eq!(
        call.result.expect("result").texts().collect::<Vec<_>>(),
        ["BLUE"]
    );

    let ThreadItem::DynamicToolCall(call) = completed_item("dynamic_tools", "dynamicToolCall")
    else {
        panic!("not a dynamic call");
    };
    assert_eq!(call.success, Some(true));
    assert_eq!(call.content_items.map(|items| items.len()), Some(1));

    let ThreadItem::WebSearch(search) = completed_item("web_search", "webSearch") else {
        panic!("not a search");
    };
    assert!(matches!(
        search.action,
        Some(WebSearchAction::Search(ref action)) if action.query.as_deref() == Some("capital city of Australia")
    ));

    let ThreadItem::CollabAgentToolCall(spawn) = completed_item("subagent", "collabAgentToolCall")
    else {
        panic!("not a subagent call");
    };
    assert_eq!(spawn.receiver_thread_ids.len(), 1);
    assert_eq!(spawn.reasoning_effort, Some(ReasoningEffort::Medium));

    let ThreadItem::FileChange(change) = completed_item("file_changes", "fileChange") else {
        panic!("not a file change");
    };
    assert!(!change.changes.is_empty());
}

#[test]
fn a_thread_start_answer_reads_as_the_thread_response() {
    let ServerMessage::Response {
        result: Ok(result), ..
    } = recorded("turn_round_trip", &[r#""id":2,"result""#])
    else {
        panic!("not a response");
    };
    let thread: ThreadResponse = codex_protocol::result(&result).expect("a thread response");
    assert_eq!(thread.model, "gpt-5.6-luna");
    assert!(!thread.thread.id.is_empty());
}

#[test]
fn unknown_fields_on_a_known_message_are_kept_and_written_back() {
    let line = br#"{"method":"turn/diff/updated","params":{"threadId":"t","turnId":"u","diff":"d","colour":"blue"},"emittedAtMs":5,"trace":1}"#;
    let message = codex_protocol::strict(line).expect("known");
    let ServerMessage::Notification {
        notification: N::TurnDiffUpdated(diff),
        emitted_at_ms,
        extra,
    } = &message
    else {
        panic!("{message:?}");
    };
    assert_eq!(diff.extra["colour"], "blue");
    assert_eq!(extra["trace"], 1);
    assert_eq!(*emitted_at_ms, Some(5));
    let again: Value = serde_json::from_slice(&codex_protocol::encode_server(&message)).unwrap();
    assert_eq!(again, serde_json::from_slice::<Value>(line).unwrap());
}

#[test]
fn an_unknown_item_type_is_kept_in_production_and_refused_strictly() {
    let line = br#"{"method":"item/started","params":{"threadId":"t","turnId":"u","item":{"type":"hologram","id":"h1"}}}"#;
    let N::ItemStarted(started) = notification(codex_protocol::decode(line).expect("decodes"))
    else {
        panic!("not an item");
    };
    assert_eq!(started.item.kind(), "hologram");
    assert_eq!(started.item.id(), "h1");
    let drift = codex_protocol::strict(line).expect_err("refused");
    assert!(drift.to_string().contains("item/started"), "{drift}");
    assert!(drift.to_string().contains("hologram"), "{drift}");
}

#[test]
fn a_line_that_is_not_an_object_is_the_only_decode_error() {
    assert!(codex_protocol::decode(b"[1,2]").is_err());
    assert!(codex_protocol::decode(b"not json").is_err());
    assert!(matches!(
        codex_protocol::decode(br#"{"neither":true}"#),
        Ok(ServerMessage::Unknown(_))
    ));
}

/// The handshake as the agent process writes it today.
#[test]
fn the_handshake_encodes_to_the_bytes_amux_writes() {
    let initialize = ClientMessage::request(
        RequestId::String("amux-initialize".into()),
        ClientRequest::Initialize(InitializeParams {
            client_info: ClientInfo {
                name: "amux".into(),
                title: None,
                version: "1.2.3".into(),
                extra: Extra::new(),
            },
            capabilities: Some(codex_protocol::client::Capabilities {
                experimental_api: Some(true),
                opt_out_notification_methods: Vec::new(),
                extra: Extra::new(),
            }),
            extra: Extra::new(),
        }),
    );
    let written = json!({
        "id": "amux-initialize",
        "method": "initialize",
        "params": {
            "clientInfo": { "name": "amux", "title": null, "version": "1.2.3" },
            "capabilities": { "experimentalApi": true },
        },
    });
    assert_eq!(
        codex_protocol::encode(&initialize),
        written.to_string().into_bytes()
    );

    let initialized = ClientMessage::notification(ClientNotification::Initialized(()));
    assert_eq!(
        codex_protocol::encode(&initialized),
        br#"{"method":"initialized"}"#
    );

    let start = ClientMessage::request(
        RequestId::String("amux-thread".into()),
        ClientRequest::ThreadStart(ThreadStartParams {
            cwd: Some("/work".into()),
            model: Some("gpt".into()),
            ..Default::default()
        }),
    );
    assert_eq!(
        codex_protocol::encode(&start),
        json!({ "id": "amux-thread", "method": "thread/start", "params": { "cwd": "/work", "model": "gpt" } })
            .to_string()
            .into_bytes()
    );
}

/// A prompt as the interpreter writes it today.
#[test]
fn a_turn_and_a_model_page_encode_to_the_bytes_amux_writes() {
    let turn = ClientMessage::request(
        RequestId::Integer(7),
        ClientRequest::TurnStart(TurnStartParams {
            thread_id: "t".into(),
            input: vec![UserInput::text("hello")],
            effort: Some(Some(ReasoningEffort::High)),
            ..Default::default()
        }),
    );
    let written = json!({
        "id": 7,
        "method": "turn/start",
        "params": {
            "threadId": "t",
            "input": [{ "type": "text", "text": "hello", "text_elements": [] }],
            "effort": "high",
        },
    });
    assert_eq!(
        codex_protocol::encode(&turn),
        written.to_string().into_bytes()
    );

    let models = ClientMessage::request(
        RequestId::String("amux-models-1".into()),
        ClientRequest::ModelList(ModelListParams::default()),
    );
    assert_eq!(
        codex_protocol::encode(&models),
        br#"{"id":"amux-models-1","method":"model/list","params":{}}"#
    );
}

#[test]
fn answers_read_back_as_what_they_answer() {
    for (line, expected) in [
        (r#"{"id":0,"result":{"decision":"decline"}}"#, "command"),
        (
            r#"{"id":0,"result":{"decision":{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["touch"]}}}}"#,
            "command",
        ),
        (
            r#"{"id":0,"result":{"permissions":{"fileSystem":null,"network":{"enabled":true}},"scope":"turn"}}"#,
            "permissions",
        ),
        (
            r#"{"id":0,"result":{"contentItems":[{"text":"sent","type":"inputText"}],"success":true}}"#,
            "tool",
        ),
        (
            r#"{"id":3,"result":{"action":"cancel","content":null}}"#,
            "elicitation",
        ),
        (
            r#"{"id":0,"result":{"answers":{"color":{"answers":["Blue"]}}}}"#,
            "input",
        ),
    ] {
        let ClientMessage::Response {
            response: Ok(response),
            ..
        } = codex_protocol::strict_client(line.as_bytes()).expect("known")
        else {
            panic!("{line}");
        };
        let kind = match response {
            ClientResponse::CommandApproval(_) => "command",
            ClientResponse::PermissionsApproval(_) => "permissions",
            ClientResponse::ToolCall(_) => "tool",
            ClientResponse::Elicitation(_) => "elicitation",
            ClientResponse::UserInput(_) => "input",
        };
        assert_eq!(kind, expected, "{line}");
    }
}

#[test]
fn sandbox_modes_are_written_as_thread_start_names_them() {
    assert_eq!(
        serde_json::to_value(SandboxMode::WorkspaceWrite).unwrap(),
        "workspace-write"
    );
    assert_eq!(
        serde_json::from_value::<SandboxMode>(json!("future-mode")).unwrap(),
        SandboxMode::Other("future-mode".into())
    );
}
