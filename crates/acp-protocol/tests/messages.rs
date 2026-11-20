//! Hand-written lines, in the shapes the schema and the three built-in
//! agents write, read into the fields amux uses; what amux writes comes
//! out as the bytes it means; and what the crate does not know is kept in
//! production and refused strictly.

use acp_protocol::agent::{PermissionOptionKind, PromptResponse, SessionSetup, StopReason};
use acp_protocol::client::{
    InitializeRequest, NewSessionRequest, PROTOCOL_VERSION, PromptRequest,
    RequestPermissionResponse, SessionRequest, SetSessionConfigOptionRequest,
};
use acp_protocol::content::ContentBlock;
use acp_protocol::elicitation::{
    CreateElicitationRequest, CreateElicitationResponse, ElicitationAccept,
    ElicitationPropertySchema, ElicitationValue,
};
use acp_protocol::session::{
    AuthMethod, ClientCapabilities, ElicitationCapabilities, Implementation, McpServer,
    McpServerStdio, SessionConfigOption, SessionConfigOptionCategory,
};
use acp_protocol::update::{SessionUpdate, ToolCallStatus, ToolKind};
use acp_protocol::{
    AgentMessage, AgentNotification, AgentRequest, AgentResponse, ClientMessage,
    ClientNotification, ClientRequest, ClientResponse, Empty, Extra, RequestId,
};
use serde_json::{Value, json};

/// Which side writes a line.
#[derive(Clone, Copy, Debug)]
enum From {
    Client,
    Agent,
}

/// One line for each message the crate types, as a side would write it.
const LINES: &[(From, &str)] = &[
    // What amux asks.
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{"elicitation":{"form":{},"url":{}}},"clientInfo":{"name":"amux","title":"amux","version":"0.1.0"}}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":2,"method":"authenticate","params":{"methodId":"opencode-login"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":3,"method":"logout","params":{}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":4,"method":"session/new","params":{"cwd":"/work","additionalDirectories":["/more"],"mcpServers":[{"name":"amux","command":"/bin/amux","args":["internal","mcp"],"env":[{"name":"AMUX_AGENT","value":"a1"}]},{"type":"http","name":"web","url":"https://example.com/mcp","headers":[{"name":"Authorization","value":"Bearer x"}]}],"systemPrompt":{"mode":"append","content":[{"type":"text","text":"Be brief."}]}}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":5,"method":"session/load","params":{"cwd":"/work","mcpServers":[],"sessionId":"s1"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":6,"method":"session/resume","params":{"cwd":"/work","mcpServers":[{"type":"sse","name":"events","url":"https://example.com/sse","headers":[]}],"sessionId":"s1"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":7,"method":"session/close","params":{"sessionId":"s1"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":8,"method":"session/list","params":{"cwd":"/work","cursor":"c2"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":9,"method":"session/delete","params":{"sessionId":"s1"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":10,"method":"session/prompt","params":{"sessionId":"s1","prompt":[{"type":"text","text":"Look at this."},{"type":"image","data":"aGk=","mimeType":"image/png"},{"type":"resource","resource":{"uri":"file:///work/a.txt","mimeType":"text/plain","text":"hello"}},{"type":"resource","resource":{"uri":"file:///work/a.pdf","blob":"aGk="}},{"type":"resource_link","uri":"file:///work/b.txt","name":"b.txt"},{"type":"audio","data":"aGk=","mimeType":"audio/wav","annotations":{"audience":["user"],"priority":1}}]}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":11,"method":"session/set_mode","params":{"sessionId":"s1","modeId":"plan"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":12,"method":"session/set_config_option","params":{"sessionId":"s1","configId":"model","value":"gemini-3.8-flash"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":13,"method":"session/set_config_option","params":{"sessionId":"s1","configId":"fast","type":"boolean","value":true}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":14,"method":"session/options","params":{"cwd":"/work"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":15,"method":"session/fork","params":{"sessionId":"s1","cwd":"/work"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":16,"method":"auth/status","params":{}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":"7"}}"#,
    ),
    // What amux answers.
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":"1","result":{"outcome":{"outcome":"selected","optionId":"allow"}}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":0,"result":{"outcome":{"outcome":"cancelled"}}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":"2","result":{"action":"accept","content":{"q1":"1","count":3,"ratio":0.5,"sure":true,"picks":["a","b"]}}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":"3","result":{"action":"decline"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32601,"message":"method not found: _x.ai/mcp/elicit"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":4,"result":{"content":"text of the file"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":5,"result":{"terminalId":"t1"}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":6,"result":{"output":"ok\n","truncated":false,"exitStatus":{"exitCode":0}}}"#,
    ),
    (
        From::Client,
        r#"{"jsonrpc":"2.0","id":7,"result":{"exitCode":1,"signal":"SIGTERM"}}"#,
    ),
    (From::Client, r#"{"jsonrpc":"2.0","id":8,"result":{}}"#),
    // What the agent answers.
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","id":1,"result":{"agentCapabilities":{"loadSession":true,"mcpCapabilities":{"http":true,"sse":true},"promptCapabilities":{"embeddedContext":true,"image":true},"sessionCapabilities":{"close":{},"fork":{},"list":{},"resume":{}}},"agentInfo":{"name":"OpenCode","version":"1.18.33"},"authMethods":[{"description":"Run `opencode auth login` in the terminal","id":"opencode-login","name":"Login with opencode"}],"protocolVersion":1}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"Authentication required","data":null}}"#,
    ),
    // What the agent asks.
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{"options":[{"kind":"allow_once","name":"Allow once","optionId":"once"},{"kind":"allow_always","name":"Always allow","optionId":"always"},{"kind":"reject_once","name":"Reject","optionId":"reject"}],"sessionId":"s1","toolCall":{"kind":"execute","locations":[],"rawInput":{"command":"./make.sh"},"status":"pending","title":"./make.sh","toolCallId":"call_1"}}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","id":"1","method":"elicitation/create","params":{"message":"The agent has a question.","mode":"form","requestedSchema":{"properties":{"q1":{"oneOf":[{"const":"1","title":"Tea"},{"const":"2","title":"Coffee"}],"title":"Tea or coffee?","type":"string"},"n":{"type":"integer","minimum":0},"x":{"type":"number","default":0.5},"ok":{"type":"boolean"},"many":{"type":"array","items":{"type":"string","enum":["a","b"]}},"titled":{"type":"array","items":{"anyOf":[{"const":"a","title":"A"}]}}},"required":["q1"],"type":"object"},"sessionId":"s1","toolCallId":"call_2"}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","id":"2","method":"elicitation/create","params":{"message":"Sign in to continue.","mode":"url","elicitationId":"e1","url":"https://example.com/login","requestId":4}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","id":3,"method":"fs/read_text_file","params":{"sessionId":"s1","path":"/work/a.txt","line":10,"limit":20}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","id":4,"method":"fs/write_text_file","params":{"sessionId":"s1","path":"/work/a.txt","content":"new"}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","id":5,"method":"terminal/create","params":{"sessionId":"s1","command":"make","args":["test"],"env":[{"name":"CI","value":"1"}],"cwd":"/work","outputByteLimit":4096}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","id":6,"method":"terminal/output","params":{"sessionId":"s1","terminalId":"t1"}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","id":7,"method":"terminal/wait_for_exit","params":{"sessionId":"s1","terminalId":"t1"}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","id":8,"method":"terminal/release","params":{"sessionId":"s1","terminalId":"t1"}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","id":9,"method":"terminal/kill","params":{"sessionId":"s1","terminalId":"t1"}}"#,
    ),
    // What the agent tells.
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"elicitation/complete","params":{"elicitationId":"e1"}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"auth/status_update","params":{"state":"authenticated","accounts":[{"label":"me@example.com","active":true}]}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":3}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"content":{"text":"Remember kiwi.","type":"text"},"messageId":"msg_1","sessionUpdate":"user_message_chunk"}}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"content":{"text":"ok","type":"text"},"sessionUpdate":"agent_message_chunk"}}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"content":{"text":"Thinking.","type":"text"},"sessionUpdate":"agent_thought_chunk"}}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"kind":"edit","locations":[{"path":"/work/a.txt","line":3}],"rawInput":{"path":"/work/a.txt"},"sessionUpdate":"tool_call","status":"in_progress","title":"Edit a.txt","toolCallId":"call_3","content":[{"type":"diff","path":"/work/a.txt","oldText":"a","newText":"b"},{"type":"content","content":{"type":"text","text":"Edited."}},{"type":"terminal","terminalId":"t1"}]}}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"rawOutput":{"output":"done"},"sessionUpdate":"tool_call_update","status":"completed","toolCallId":"call_3"}}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"entries":[{"content":"Read the file","priority":"high","status":"completed"},{"content":"Edit it","priority":"medium","status":"in_progress"}],"sessionUpdate":"plan"}}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"availableCommands":[{"description":"Compress the conversation","input":null,"name":"compact"},{"description":"Always approve","input":{"hint":"on|off"},"name":"always-approve"},{"description":"Review","name":"review"}],"sessionUpdate":"available_commands_update"}}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"currentModeId":"plan","sessionUpdate":"current_mode_update"}}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"configOptions":[{"category":"thought_level","currentValue":"low","description":"Effort levels","id":"effort","name":"Effort","options":[{"name":"Low","value":"low"},{"name":"High","value":"high"}],"type":"select"},{"currentValue":false,"id":"fast","name":"Fast","type":"boolean"}],"sessionUpdate":"config_option_update"}}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"session_info_update","title":"Create a file"}}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"cost":{"amount":0,"currency":"USD"},"sessionUpdate":"usage_update","size":200000,"used":13010}}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"notice","severity":"warning","title":"Rate limited","description":"Retrying in 5s"}}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"compaction_update","compactionId":"k1","status":"completed","summary":[{"type":"text","text":"So far."}]}}}"#,
    ),
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"compaction_summary_chunk","compactionId":"k1","content":{"type":"text","text":"So"}}}}"#,
    ),
    // A vendor's own method that the crate lists.
    (
        From::Agent,
        r#"{"jsonrpc":"2.0","method":"_x.ai/session/setup","params":{"method":"session/new","phase":"auth","sessionId":null}}"#,
    ),
];

/// The first of [`LINES`] that holds `needle`.
fn line(needle: &str) -> &'static str {
    LINES
        .iter()
        .map(|(_, line)| *line)
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line holds {needle}"))
}

fn decode(from: From, line: &str) -> Result<Value, String> {
    let bytes = line.as_bytes();
    let (strict, tolerant) = match from {
        From::Client => (
            acp_protocol::strict_client(bytes)
                .map(|message| acp_protocol::encode(&message))
                .map_err(|drift| drift.to_string())?,
            acp_protocol::encode(&acp_protocol::decode_client(bytes).unwrap()),
        ),
        From::Agent => (
            acp_protocol::strict(bytes)
                .map(|message| acp_protocol::encode_server(&message))
                .map_err(|drift| drift.to_string())?,
            acp_protocol::encode_server(&acp_protocol::decode(bytes).unwrap()),
        ),
    };
    assert_eq!(strict, tolerant, "{line}");
    Ok(serde_json::from_slice(&strict).unwrap())
}

#[test]
fn every_line_reads_strictly_and_writes_back() {
    let mut failures = Vec::new();
    for (from, line) in LINES {
        let written: Value = serde_json::from_str(line).unwrap();
        match decode(*from, line) {
            Ok(again) if again == written => {}
            Ok(again) => failures.push(format!("{line}\n  wrote {again}")),
            Err(drift) => failures.push(format!("{line}\n  {drift}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_method_the_crate_types_has_a_line() {
    let mut methods: Vec<&str> = Vec::new();
    for (from, line) in LINES {
        match from {
            From::Client => match acp_protocol::decode_client(line.as_bytes()).unwrap() {
                ClientMessage::Request { request, .. } => methods.push(request.method()),
                ClientMessage::Notification { notification, .. } => {
                    methods.push(notification.method())
                }
                _ => {}
            },
            From::Agent => match acp_protocol::decode(line.as_bytes()).unwrap() {
                AgentMessage::Request { request, .. } => methods.push(request.method()),
                AgentMessage::Notification { notification, .. } => {
                    methods.push(notification.method())
                }
                _ => {}
            },
        }
    }
    let typed = [
        ClientRequest::METHODS,
        ClientNotification::METHODS,
        AgentRequest::METHODS,
        AgentNotification::METHODS,
    ];
    let missing: Vec<_> = typed
        .iter()
        .flat_map(|methods| methods.iter())
        .filter(|method| !methods.contains(method))
        .collect();
    assert!(missing.is_empty(), "no line for {missing:?}");
    let updates: Vec<String> = LINES
        .iter()
        .filter_map(|(_, line)| match acp_protocol::decode(line.as_bytes()) {
            Ok(AgentMessage::Notification {
                notification: AgentNotification::SessionUpdate(notification),
                ..
            }) => Some(notification.update.kind().to_owned()),
            _ => None,
        })
        .collect();
    let missing: Vec<_> = SessionUpdate::TAGS
        .iter()
        .filter(|kind| !updates.iter().any(|update| update == *kind))
        .collect();
    assert!(missing.is_empty(), "no line for the updates {missing:?}");
}

fn answer(line: &str, method: &str) -> AgentResponse {
    let AgentMessage::Response {
        result: Ok(result), ..
    } = acp_protocol::strict(line.as_bytes()).unwrap()
    else {
        panic!("not an answer: {line}");
    };
    AgentResponse::strict_read(method, &result)
        .expect("an answer type")
        .expect("reads")
}

#[test]
fn antigravitys_initialize_answer_reads_what_it_offers() {
    // As agy 1.5.13 writes it, the snake-case copies included.
    let line = r#"{"id":1,"jsonrpc":"2.0","result":{"agentCapabilities":{"_meta":{"antigravity":{"systemPrompt":{"append":true},"tier":true}},"auth":{"logout":{}},"loadSession":true,"load_session":true,"promptCapabilities":{"audio":false,"embeddedContext":true,"image":true},"sessionCapabilities":{"additionalDirectories":{},"close":{},"list":{},"load":{},"resume":{}},"session_capabilities":{"additionalDirectories":{},"close":{},"list":{},"load":{},"resume":{}}},"agentInfo":{"name":"Antigravity ACP Server","version":"1.5.13"},"authMethods":[{"description":"Log in with your Google account","id":"oauth-personal","name":"Log in with Google"}],"protocolVersion":1}}"#;
    let AgentResponse::Initialize(initialize) = answer(line, "initialize") else {
        panic!("not an initialize answer");
    };
    let capabilities = initialize.agent_capabilities.expect("capabilities");
    assert_eq!(capabilities.load_session, Some(true));
    let sessions = capabilities
        .session_capabilities
        .expect("session capabilities");
    assert!(sessions.resume.is_some() && sessions.close.is_some());
    assert!(sessions.fork.is_none());
    let prompts = capabilities
        .prompt_capabilities
        .expect("prompt capabilities");
    assert_eq!(prompts.image, Some(true));
    assert_eq!(prompts.audio, Some(false));
    assert!(capabilities.auth.expect("auth").logout.is_some());
    assert_eq!(
        capabilities.meta,
        Some(json!({ "antigravity": { "systemPrompt": { "append": true }, "tier": true } }))
    );
    assert_eq!(
        initialize.agent_info.flatten().expect("info").version,
        "1.5.13"
    );
    let method = &initialize.auth_methods.expect("auth methods")[0];
    assert!(matches!(method, AuthMethod::Agent(_)));
    assert_eq!(method.id(), Some("oauth-personal"));
}

#[test]
fn a_sign_in_hint_and_an_auth_refusal_read() {
    let AgentResponse::Initialize(initialize) = answer(line("Login with opencode"), "initialize")
    else {
        panic!("not an initialize answer");
    };
    let methods = initialize.auth_methods.expect("auth methods");
    assert_eq!(
        methods[0].description(),
        Some("Run `opencode auth login` in the terminal")
    );
    // Grok, signed out, refuses `session/new`.
    let AgentMessage::Response {
        result: Err(error), ..
    } = acp_protocol::strict(line("Authentication required").as_bytes()).unwrap()
    else {
        panic!("not an error");
    };
    assert_eq!(error.code, acp_protocol::error_code::AUTH_REQUIRED);
    assert_eq!(error.data, Some(Value::Null));
}

#[test]
fn a_permission_request_reads_its_offers_and_its_answer_writes_the_choice() {
    let AgentMessage::Request {
        id,
        request: AgentRequest::RequestPermission(ask),
        ..
    } = acp_protocol::strict(line("session/request_permission").as_bytes()).unwrap()
    else {
        panic!("not a permission request");
    };
    assert_eq!(id, RequestId::Integer(0));
    let kinds: Vec<_> = ask.options.iter().map(|option| &option.kind).collect();
    assert_eq!(
        kinds,
        [
            &PermissionOptionKind::AllowOnce,
            &PermissionOptionKind::AllowAlways,
            &PermissionOptionKind::RejectOnce
        ]
    );
    assert_eq!(ask.tool_call.kind, Some(Some(ToolKind::Execute)));
    assert_eq!(ask.tool_call.status, Some(Some(ToolCallStatus::Pending)));
    assert_eq!(
        ask.tool_call.raw_input,
        Some(json!({ "command": "./make.sh" }))
    );

    let answer = ClientMessage::response(
        id,
        ClientResponse::RequestPermission(RequestPermissionResponse::selected("once")),
    );
    assert_eq!(
        String::from_utf8(acp_protocol::encode(&answer)).unwrap(),
        r#"{"id":0,"jsonrpc":"2.0","result":{"outcome":{"optionId":"once","outcome":"selected"}}}"#
    );
    let cancelled = ClientMessage::response(
        RequestId::String("1".into()),
        ClientResponse::RequestPermission(RequestPermissionResponse::cancelled()),
    );
    assert_eq!(
        String::from_utf8(acp_protocol::encode(&cancelled)).unwrap(),
        r#"{"id":"1","jsonrpc":"2.0","result":{"outcome":{"outcome":"cancelled"}}}"#
    );
}

#[test]
fn config_options_read_their_category_values_and_current_value() {
    let AgentMessage::Notification {
        notification: AgentNotification::SessionUpdate(notification),
        ..
    } = acp_protocol::strict(line("config_option_update").as_bytes()).unwrap()
    else {
        panic!("not an update");
    };
    let SessionUpdate::ConfigOptionUpdate(update) = notification.update else {
        panic!("not a config option update");
    };
    let [effort, fast] = update.config_options.as_slice() else {
        panic!("two options");
    };
    assert_eq!(effort.id(), Some("effort"));
    assert_eq!(
        effort.category(),
        Some(&SessionConfigOptionCategory::ThoughtLevel)
    );
    let SessionConfigOption::Select(select) = effort else {
        panic!("a select");
    };
    assert_eq!(select.current_value, "low");
    let values: Vec<_> = select
        .options
        .values()
        .iter()
        .map(|o| o.value.as_str())
        .collect();
    assert_eq!(values, ["low", "high"]);
    assert!(matches!(fast, SessionConfigOption::Boolean(b) if !b.current_value));

    // Options in named groups.
    let grouped = json!({"configOptions": [{"type": "select", "id": "model", "name": "Model",
        "currentValue": "b", "options": [{"group": "g", "name": "G", "options": [
            {"value": "a", "name": "A"}, {"value": "b", "name": "B"}]}]}]});
    let AgentResponse::SessionLoad(SessionSetup { config_options, .. }) =
        AgentResponse::strict_read("session/load", &grouped)
            .unwrap()
            .unwrap()
    else {
        panic!("not a load answer");
    };
    let Some(SessionConfigOption::Select(select)) = config_options.flatten().unwrap().pop() else {
        panic!("a select");
    };
    let values: Vec<_> = select
        .options
        .values()
        .iter()
        .map(|o| o.value.as_str())
        .collect();
    assert_eq!(values, ["a", "b"]);
}

#[test]
fn a_turns_end_reads_its_reason_and_tokens() {
    let line = r#"{"id":4,"jsonrpc":"2.0","result":{"_meta":{},"stopReason":"end_turn","usage":{"cachedReadTokens":12928,"inputTokens":82,"outputTokens":3,"thoughtTokens":10,"totalTokens":13023}}}"#;
    let AgentResponse::SessionPrompt(PromptResponse {
        stop_reason, usage, ..
    }) = answer(line, "session/prompt")
    else {
        panic!("not a prompt answer");
    };
    assert_eq!(stop_reason, StopReason::EndTurn);
    let usage = usage.flatten().expect("usage");
    assert_eq!(
        (usage.total_tokens, usage.thought_tokens),
        (13023, Some(Some(10)))
    );
}

#[test]
fn usage_keeps_a_whole_cost_whole() {
    let AgentMessage::Notification {
        notification: AgentNotification::SessionUpdate(notification),
        ..
    } = acp_protocol::strict(line("usage_update").as_bytes()).unwrap()
    else {
        panic!("not an update");
    };
    let SessionUpdate::UsageUpdate(usage) = notification.update else {
        panic!("not usage");
    };
    assert_eq!((usage.used, usage.size), (13010, 200000));
    let cost = usage.cost.flatten().expect("cost");
    assert_eq!(serde_json::to_string(&cost.amount).unwrap(), "0");
}

#[test]
fn a_form_reads_its_fields_and_its_answer_writes_them() {
    let AgentMessage::Request {
        request: AgentRequest::ElicitationCreate(CreateElicitationRequest::Form(form)),
        ..
    } = acp_protocol::strict(line(r#""mode":"form""#).as_bytes()).unwrap()
    else {
        panic!("not a form");
    };
    assert_eq!(form.session_id.as_deref(), Some("s1"));
    assert_eq!(form.tool_call_id.flatten().as_deref(), Some("call_2"));
    let fields = form.requested_schema.properties.expect("fields");
    let Some(ElicitationPropertySchema::String(choice)) = fields.get("q1") else {
        panic!("a string field");
    };
    let titles: Vec<_> = choice
        .one_of
        .iter()
        .flatten()
        .flatten()
        .map(|option| (option.value.as_str(), option.title.as_str()))
        .collect();
    assert_eq!(titles, [("1", "Tea"), ("2", "Coffee")]);
    assert_eq!(
        form.requested_schema.required,
        Some(Some(vec!["q1".to_owned()]))
    );

    let answer = CreateElicitationResponse::Accept(ElicitationAccept {
        content: Some(Some(
            [("q1".to_owned(), ElicitationValue::String("1".into()))]
                .into_iter()
                .collect(),
        )),
        meta: None,
        extra: Extra::new(),
    });
    let line = ClientMessage::response(
        RequestId::String("1".into()),
        ClientResponse::ElicitationCreate(answer),
    );
    assert_eq!(
        String::from_utf8(acp_protocol::encode(&line)).unwrap(),
        r#"{"id":"1","jsonrpc":"2.0","result":{"action":"accept","content":{"q1":"1"}}}"#
    );
}

#[test]
fn a_page_to_open_reads_the_request_it_is_scoped_to() {
    let AgentMessage::Request {
        request: AgentRequest::ElicitationCreate(CreateElicitationRequest::Url(page)),
        ..
    } = acp_protocol::strict(line(r#""mode":"url""#).as_bytes()).unwrap()
    else {
        panic!("not a page");
    };
    assert_eq!(page.url, "https://example.com/login");
    assert_eq!(page.request_id, Some(RequestId::Integer(4)));
    assert_eq!(page.session_id, None);
}

#[test]
fn the_handshake_encodes_to_the_bytes_amux_means() {
    let initialize = ClientMessage::request(
        RequestId::Integer(1),
        ClientRequest::Initialize(InitializeRequest {
            protocol_version: PROTOCOL_VERSION,
            client_capabilities: Some(ClientCapabilities {
                elicitation: Some(Some(ElicitationCapabilities {
                    form: Some(Some(Empty::default())),
                    url: Some(Some(Empty::default())),
                    ..Default::default()
                })),
                ..Default::default()
            }),
            client_info: Some(Some(Implementation {
                name: "amux".into(),
                title: Some(Some("amux".into())),
                version: "0.1.0".into(),
                meta: None,
                extra: Extra::new(),
            })),
            meta: None,
            extra: Extra::new(),
        }),
    );
    // What the recording lane's probe wrote to agy.
    assert_eq!(
        String::from_utf8(acp_protocol::encode(&initialize)).unwrap(),
        r#"{"id":1,"jsonrpc":"2.0","method":"initialize","params":{"clientCapabilities":{"elicitation":{"form":{},"url":{}}},"clientInfo":{"name":"amux","title":"amux","version":"0.1.0"},"protocolVersion":1}}"#
    );

    let new = ClientMessage::request(
        RequestId::Integer(2),
        ClientRequest::SessionNew(NewSessionRequest {
            cwd: "/work".into(),
            additional_directories: None,
            mcp_servers: vec![McpServer::Stdio(McpServerStdio {
                name: "amux".into(),
                command: "/bin/amux".into(),
                args: vec!["internal".into(), "mcp".into()],
                env: Vec::new(),
                meta: None,
                extra: Extra::new(),
            })],
            system_prompt: None,
            meta: None,
            extra: Extra::new(),
        }),
    );
    assert_eq!(
        String::from_utf8(acp_protocol::encode(&new)).unwrap(),
        r#"{"id":2,"jsonrpc":"2.0","method":"session/new","params":{"cwd":"/work","mcpServers":[{"args":["internal","mcp"],"command":"/bin/amux","env":[],"name":"amux"}]}}"#
    );

    let prompt = ClientMessage::request(
        RequestId::Integer(3),
        ClientRequest::SessionPrompt(PromptRequest {
            session_id: "s1".into(),
            prompt: vec![ContentBlock::text("hello")],
            meta: None,
            extra: Extra::new(),
        }),
    );
    assert_eq!(
        String::from_utf8(acp_protocol::encode(&prompt)).unwrap(),
        r#"{"id":3,"jsonrpc":"2.0","method":"session/prompt","params":{"prompt":[{"text":"hello","type":"text"}],"sessionId":"s1"}}"#
    );

    let cancel = ClientMessage::notification(ClientNotification::SessionCancel(SessionRequest {
        session_id: "s1".into(),
        meta: None,
        extra: Extra::new(),
    }));
    assert_eq!(
        String::from_utf8(acp_protocol::encode(&cancel)).unwrap(),
        r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}"#
    );

    let effort = ClientMessage::request(
        RequestId::Integer(4),
        ClientRequest::SessionSetConfigOption(SetSessionConfigOptionRequest::select(
            "s1", "effort", "high",
        )),
    );
    assert_eq!(
        String::from_utf8(acp_protocol::encode(&effort)).unwrap(),
        r#"{"id":4,"jsonrpc":"2.0","method":"session/set_config_option","params":{"configId":"effort","sessionId":"s1","value":"high"}}"#
    );
    let fast = ClientMessage::request(
        RequestId::Integer(5),
        ClientRequest::SessionSetConfigOption(SetSessionConfigOptionRequest::boolean(
            "s1", "fast", true,
        )),
    );
    assert_eq!(
        String::from_utf8(acp_protocol::encode(&fast)).unwrap(),
        r#"{"id":5,"jsonrpc":"2.0","method":"session/set_config_option","params":{"configId":"fast","sessionId":"s1","type":"boolean","value":true}}"#
    );
}

#[test]
fn an_answer_read_without_its_request_is_the_answer_its_fields_fill() {
    let read = |line: &str| match acp_protocol::strict_client(line.as_bytes()).unwrap() {
        ClientMessage::Response {
            response: Ok(response),
            ..
        } => response,
        other => panic!("not an answer: {other:?}"),
    };
    assert!(matches!(
        read(r#"{"jsonrpc":"2.0","id":1,"result":{"exitCode":1}}"#),
        ClientResponse::TerminalWaitForExit(_)
    ));
    assert!(matches!(
        read(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
        ClientResponse::WriteTextFile(_)
    ));
    assert!(matches!(
        read(r#"{"jsonrpc":"2.0","id":1,"result":{"action":"cancel"}}"#),
        ClientResponse::ElicitationCreate(CreateElicitationResponse::Cancel(_))
    ));
}

#[test]
fn an_unknown_method_is_kept_whole_and_refused_strictly() {
    let line = br#"{"jsonrpc":"2.0","id":9,"method":"session/teleport","params":{"sessionId":"s1","to":"mars"}}"#;
    let AgentMessage::Unknown(unknown) = acp_protocol::decode(line).unwrap() else {
        panic!("typed");
    };
    assert_eq!(unknown.method.as_deref(), Some("session/teleport"));
    // A request nobody can read still needs its answer.
    assert_eq!(unknown.id, Some(RequestId::Integer(9)));
    assert_eq!(unknown.session_id().as_deref(), Some("s1"));
    assert_eq!(unknown.raw.get().as_bytes(), line);
    assert_eq!(
        acp_protocol::encode_server(&AgentMessage::Unknown(unknown)),
        line.to_vec()
    );
    let drift = acp_protocol::strict(line).expect_err("strict refuses it");
    assert!(drift.to_string().contains("session/teleport"), "{drift}");
}

#[test]
fn an_extension_is_kept_and_refused_strictly_unless_listed() {
    let listed = br#"{"jsonrpc":"2.0","method":"_x.ai/queue/changed","params":{"entries":[]}}"#;
    let AgentMessage::Extension { extension, .. } = acp_protocol::strict(listed).unwrap() else {
        panic!("not an extension");
    };
    assert!(extension.is_known() && extension.id.is_none());
    assert_eq!(extension.params, json!({ "entries": [] }));

    let unlisted = br#"{"jsonrpc":"2.0","id":3,"method":"_zed/hello","params":{"n":1}}"#;
    let AgentMessage::Extension { extension, .. } = acp_protocol::decode(unlisted).unwrap() else {
        panic!("not an extension");
    };
    assert_eq!(extension.id, Some(RequestId::Integer(3)));
    let message = acp_protocol::decode(unlisted).unwrap();
    let again: Value = serde_json::from_slice(&acp_protocol::encode_server(&message)).unwrap();
    assert_eq!(again, serde_json::from_slice::<Value>(unlisted).unwrap());
    let drift = acp_protocol::strict(unlisted).expect_err("strict refuses it");
    assert!(drift.to_string().contains("_zed/hello"), "{drift}");
}

#[test]
fn an_unknown_update_kind_is_kept_in_production_and_refused_strictly() {
    let line = br#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"subagent_update","sessionId":"child"}}}"#;
    let AgentMessage::Notification {
        notification: AgentNotification::SessionUpdate(notification),
        ..
    } = acp_protocol::decode(line).unwrap()
    else {
        panic!("not an update");
    };
    assert!(matches!(&notification.update, SessionUpdate::Unknown(_)));
    assert_eq!(notification.update.kind(), "subagent_update");
    let drift = acp_protocol::strict(line).expect_err("strict refuses it");
    assert!(drift.to_string().contains("subagent_update"), "{drift}");
}

#[test]
fn an_unknown_spelling_is_kept_in_production_and_refused_strictly() {
    let line = br#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"tool_call","toolCallId":"c","title":"t","kind":"teleport"}}}"#;
    let AgentMessage::Notification {
        notification: AgentNotification::SessionUpdate(notification),
        ..
    } = acp_protocol::decode(line).unwrap()
    else {
        panic!("not an update");
    };
    let SessionUpdate::ToolCall(call) = notification.update else {
        panic!("not a tool call");
    };
    assert_eq!(call.kind, Some(ToolKind::Other("teleport".into())));
    let drift = acp_protocol::strict(line).expect_err("strict refuses it");
    assert!(drift.to_string().contains("teleport"), "{drift}");
}

#[test]
fn unknown_fields_are_kept_and_written_back_and_refused_strictly() {
    let line =
        br#"{"jsonrpc":"2.0","id":1,"result":{"stopReason":"end_turn","turnId":"t9"},"trace":"x"}"#;
    let message = acp_protocol::decode(line).unwrap();
    let AgentMessage::Response {
        result: Ok(result),
        extra,
        ..
    } = &message
    else {
        panic!("not an answer");
    };
    assert_eq!(extra.get("trace"), Some(&json!("x")));
    let AgentResponse::SessionPrompt(prompt) = AgentResponse::read("session/prompt", result)
        .unwrap()
        .unwrap()
    else {
        panic!("not a prompt answer");
    };
    assert_eq!(prompt.extra.get("turnId"), Some(&json!("t9")));
    let error = AgentResponse::strict_read("session/prompt", result)
        .unwrap()
        .expect_err("strict refuses it");
    assert!(error.to_string().contains("turnId"), "{error}");
    let drift = acp_protocol::strict(line).expect_err("strict refuses the envelope's");
    assert!(drift.to_string().contains("trace"), "{drift}");
    let again: Value = serde_json::from_slice(&acp_protocol::encode_server(&message)).unwrap();
    assert_eq!(again, serde_json::from_slice::<Value>(line).unwrap());
}

#[test]
fn meta_is_vendor_data_kept_as_written_even_null() {
    let line = br#"{"jsonrpc":"2.0","method":"session/update","params":{"_meta":{"agentTimestampMs":1,"isReplay":true},"sessionId":"s1","update":{"_meta":null,"content":{"text":"hi","type":"text"},"sessionUpdate":"user_message_chunk"}}}"#;
    let message = acp_protocol::strict(line).unwrap();
    let AgentMessage::Notification {
        notification: AgentNotification::SessionUpdate(notification),
        ..
    } = &message
    else {
        panic!("not an update");
    };
    assert_eq!(
        notification.meta,
        Some(json!({ "agentTimestampMs": 1, "isReplay": true }))
    );
    let SessionUpdate::UserMessageChunk(chunk) = &notification.update else {
        panic!("not a chunk");
    };
    assert_eq!(chunk.meta, Some(Value::Null));
    let again: Value = serde_json::from_slice(&acp_protocol::encode_server(&message)).unwrap();
    assert_eq!(again, serde_json::from_slice::<Value>(line).unwrap());
}

#[test]
fn a_line_without_jsonrpc_reads_but_is_refused_strictly() {
    let line = br#"{"method":"session/cancel","params":{"sessionId":"s1"}}"#;
    assert!(matches!(
        acp_protocol::decode_client(line).unwrap(),
        ClientMessage::Notification {
            notification: ClientNotification::SessionCancel(_),
            ..
        }
    ));
    let drift = acp_protocol::strict_client(line).expect_err("strict refuses it");
    assert!(drift.to_string().contains("jsonrpc"), "{drift}");
}

#[test]
fn ids_are_numbers_strings_or_null() {
    for (id, expected) in [
        ("7", RequestId::Integer(7)),
        (r#""7""#, RequestId::String("7".into())),
        ("null", RequestId::Null),
    ] {
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32700,"message":"Parse error"}}}}"#
        );
        let AgentMessage::Response { id, .. } = acp_protocol::strict(line.as_bytes()).unwrap()
        else {
            panic!("not an answer");
        };
        assert_eq!(id, expected);
    }
    let line = br#"{"jsonrpc":"2.0","id":{"n":1},"result":{}}"#;
    assert!(matches!(
        acp_protocol::decode(line).unwrap(),
        AgentMessage::Unknown(_)
    ));
}

#[test]
fn a_line_that_is_not_an_object_is_the_only_decode_error() {
    for line in [&b"[1,2]"[..], b"not json", b"\"text\"", b""] {
        assert!(acp_protocol::decode(line).is_err());
        assert!(acp_protocol::decode_client(line).is_err());
    }
    assert!(matches!(
        acp_protocol::decode(br#"{"jsonrpc":"2.0"}"#).unwrap(),
        AgentMessage::Unknown(_)
    ));
}

#[test]
fn fields_name_what_the_crate_does_not_type_at_its_place() {
    let line = br#"{"jsonrpc":"2.0","method":"session/update","params":{"_meta":{"vendor":1},"sessionId":"s1","update":{"sessionUpdate":"tool_call","toolCallId":"c","title":"t","rawInput":{"anything":1},"glow":true}}}"#;
    let fields = acp_protocol::fields(line).expect("an object");
    assert_eq!(
        fields
            .get("params.update")
            .map(|names| names.iter().map(String::as_str).collect::<Vec<_>>()),
        Some(vec!["glow"])
    );
    let untyped: Vec<_> = fields
        .iter()
        .filter(|(place, names)| place.as_str() != "params.update" && !names.is_empty())
        .collect();
    assert!(untyped.is_empty(), "{untyped:?}");
    // `_meta` and a tool's input are the data's, so no place inside them
    // is the protocol's.
    assert!(
        !fields
            .keys()
            .any(|place| place.contains("_meta") || place.contains("rawInput")),
        "{fields:?}"
    );
    for (from, line) in LINES {
        let fields = match from {
            From::Client => acp_protocol::client_fields(line.as_bytes()),
            From::Agent => acp_protocol::fields(line.as_bytes()),
        }
        .expect("an object");
        let untyped: Vec<_> = fields
            .iter()
            .filter(|(_, names)| !names.is_empty())
            .collect();
        assert!(untyped.is_empty(), "{line}: {untyped:?}");
    }
}
