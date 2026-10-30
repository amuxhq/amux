use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use claude_protocol::stream::Output;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::driver::sdk::abort::{Shutdown, ShutdownReason};
use crate::driver::sdk::control::{
    CanUseToolRequest, ControlOutcome, ControlRequest, ControlRequestBody, ControlResponseInner,
    ElicitationRequestBody, HookCallbackRequest, InitializeRequestBody,
};
use crate::driver::sdk::init::InitializationResult;
use crate::driver::sdk::message::Message;
use crate::driver::sdk::options::{
    ElicitationMode, ElicitationRequest, HookCallbackContext, HookEventData, HookInput,
    UserDialogRequest,
};
use crate::driver::sdk::session::SdkEvent;
use crate::driver::sdk::types::PermissionUpdate;
use crate::driver::sdk::{Error, ProtocolError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IncomingRequestKind {
    Permission,
    Hook,
    Elicitation,
    UserDialog,
}

// ── QueryInner ───────────────────────────────────────────────────

pub(crate) struct QueryInner {
    pub session_id: String,
    pub stdin_tx: mpsc::UnboundedSender<WriteCommand>,
    pub pending_controls: Mutex<HashMap<String, oneshot::Sender<ControlResponseInner>>>,
    pub init_result: OnceLock<InitializationResult>,
    pub request_counter: AtomicU64,
    pub initialize_request: InitializeRequestBody,
    pub pending_incoming: Mutex<HashMap<String, IncomingRequestKind>>,
    pub hook_callback_ids: HashSet<String>,
}

impl QueryInner {
    pub(crate) async fn write(&self, data: Vec<u8>) -> Result<(), Error> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.stdin_tx
            .send(WriteCommand::Data {
                data,
                ack: Some(ack_tx),
            })
            .map_err(|_| Error::Send("Claude stdin is closed".to_owned()))?;
        ack_rx
            .await
            .map_err(|_| Error::Send("Claude stdin writer stopped".to_owned()))?
            .map_err(Error::Send)
    }

    /// Send a control request and wait for the matching response.
    /// Answers with the response's payload, null when it has none.
    pub async fn send_control(&self, body: ControlRequestBody) -> Result<serde_json::Value, Error> {
        let id = format!(
            "req_{}",
            self.request_counter.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, rx) = oneshot::channel();
        self.pending_controls.lock().await.insert(id.clone(), tx);

        let json = crate::driver::sdk::encode(&crate::driver::sdk::Input::ControlRequest(
            ControlRequest::new(id.clone(), body),
        ));

        if let Err(error) = self.write(json).await {
            self.pending_controls.lock().await.remove(&id);
            return Err(error);
        }

        let resp = rx
            .await
            .map_err(|_| Error::Control("reader closed before control response".into()))?;

        if let Some(err) = &resp.error {
            return Err(Error::Control(err.clone()));
        }
        if resp.subtype != ControlOutcome::Success {
            return Err(Error::Control(format!(
                "unexpected control response subtype {:?}",
                resp.subtype
            )));
        }

        Ok(resp.response.unwrap_or_default())
    }

    pub(crate) async fn answer_incoming(
        &self,
        id: String,
        expected: IncomingRequestKind,
        response: serde_json::Value,
    ) -> Result<(), Error> {
        let mut pending = self.pending_incoming.lock().await;
        match pending.get(&id).copied() {
            Some(kind) if kind == expected => {
                pending.remove(&id);
            }
            _ => return Err(Error::UnknownRequest(id)),
        }
        drop(pending);
        send_control_success(self, &id, response)
            .await
            .map_err(|()| Error::Send("failed to answer Claude control request".into()))
    }
}

// ── Background tasks ───────────────────────────────────────────────

/// Spawn the background reader task that demuxes stdout into turn messages
/// and control responses.
pub(crate) fn spawn_reader_task(
    reader: impl AsyncBufRead + Unpin + Send + 'static,
    turn_tx: mpsc::Sender<Result<SdkEvent, Error>>,
    inner: Arc<QueryInner>,
    shutdown: Arc<Shutdown>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        reader_loop(reader, turn_tx, inner, shutdown).await;
    })
}

async fn reader_loop(
    mut reader: impl AsyncBufRead + Unpin,
    turn_tx: mpsc::Sender<Result<SdkEvent, Error>>,
    inner: Arc<QueryInner>,
    shutdown: Arc<Shutdown>,
) {
    let cancel = shutdown.token();
    let mut line = String::new();
    loop {
        line.clear();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            tokio::select! {
                                biased;
                                _ = cancel.cancelled() => break,
                                _ = turn_tx.send(Err(Error::Protocol(ProtocolError::new(
                                    "empty line from Claude stdout",
                                )))) => {}
                            }
                        } else {
                            tokio::select! {
                                biased;
                                _ = cancel.cancelled() => break,
                                _ = dispatch_line(trimmed, &turn_tx, &inner) => {}
                            }
                        }
                    }
                    Err(error) => {
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => {},
                            _ = turn_tx.send(Err(Error::Stream(format!(
                                "I/O error reading Claude stdout: {error}"
                            )))) => {}
                        }
                        shutdown.request(ShutdownReason::TransportFailed);
                        break;
                    }
                }
            }
        }
    }

    drop_pending_controls(inner).await;
}

async fn drop_pending_controls(inner: Arc<QueryInner>) {
    let pending = {
        let mut guard = inner.pending_controls.lock().await;
        std::mem::take(&mut *guard)
    };
    drop(pending);
}

async fn dispatch_line(
    line: &str,
    turn_tx: &mpsc::Sender<Result<SdkEvent, Error>>,
    inner: &QueryInner,
) {
    // The frame as written, for an error that quotes it.
    let frame = || serde_json::from_str::<serde_json::Value>(line).unwrap_or_default();
    let output = match claude_protocol::stream::decode(line.as_bytes()) {
        Ok(output) => output,
        Err(error) => {
            let _ = turn_tx
                .send(Err(Error::Protocol(ProtocolError::new(format!(
                    "invalid JSON from Claude: {error}; line: {line}"
                )))))
                .await;
            return;
        }
    };

    match output {
        Output::ControlResponse(envelope) => {
            let request_id = envelope.response.request_id.clone();
            if let Some(tx) = inner.pending_controls.lock().await.remove(&request_id) {
                let _ = tx.send(envelope.response);
            } else {
                let _ = turn_tx
                    .send(Err(Error::Protocol(ProtocolError::with_frame(
                        format!("control response has no pending request `{request_id}`"),
                        frame(),
                    ))))
                    .await;
            }
        }
        Output::ControlRequest(request) => {
            handle_incoming_control_request(request, frame, inner, turn_tx).await;
        }
        Output::Message(message) => {
            let _ = turn_tx.send(Ok(SdkEvent::Message(message))).await;
        }
        // Withdrawn requests reach the caller as a frame of their own, as
        // the published SDK passes them on.
        Output::ControlCancelRequest(_) => {
            if let Ok(message) = Message::parse(frame()) {
                let _ = turn_tx.send(Ok(SdkEvent::Message(message))).await;
            }
        }
        Output::Unknown(unknown) => {
            let error = match unknown.kind.as_deref() {
                Some(kind @ ("control_request" | "control_response")) => {
                    ProtocolError::with_frame(format!("malformed {kind}"), frame())
                }
                // A frame type this driver does not know still reaches the
                // caller, as the published SDK passes it on.
                _ => match Message::parse(frame()) {
                    Ok(message) => {
                        let _ = turn_tx.send(Ok(SdkEvent::Message(message))).await;
                        return;
                    }
                    Err(error) => {
                        ProtocolError::new(format!("invalid Claude message: {error}; line: {line}"))
                    }
                },
            };
            let _ = turn_tx.send(Err(Error::Protocol(error))).await;
        }
    }
}

/// Parse incoming requests without running host code in the transport task.
async fn handle_incoming_control_request(
    request: ControlRequest,
    frame: impl Fn() -> serde_json::Value,
    inner: &QueryInner,
    turn_tx: &mpsc::Sender<Result<SdkEvent, Error>>,
) {
    let ControlRequest {
        request_id,
        request,
        ..
    } = request;
    let refuse = |error: String| async {
        let _ = send_control_error(inner, &request_id, &error).await;
        let _ = turn_tx
            .send(Err(Error::Protocol(ProtocolError::with_frame(
                error,
                frame(),
            ))))
            .await;
    };
    let (kind, event) = match request {
        ControlRequestBody::CanUseTool(body) => (
            IncomingRequestKind::Permission,
            permission_request(body, &request_id),
        ),
        ControlRequestBody::HookCallback(body) => match hook_callback(body, &request_id, inner) {
            Ok((input, context)) => (
                IncomingRequestKind::Hook,
                SdkEvent::HookCallback {
                    id: request_id.clone(),
                    input,
                    context,
                },
            ),
            Err(error) => return refuse(error).await,
        },
        ControlRequestBody::Elicitation(body) => match elicitation_request(body) {
            Ok(request) => (
                IncomingRequestKind::Elicitation,
                SdkEvent::Elicitation {
                    id: request_id.clone(),
                    request,
                },
            ),
            Err(error) => return refuse(error).await,
        },
        ControlRequestBody::UserDialog(body) => (
            IncomingRequestKind::UserDialog,
            SdkEvent::UserDialog {
                id: request_id.clone(),
                request: UserDialogRequest {
                    dialog_kind: body.dialog_kind,
                    payload: serde_json::Value::Object(body.payload),
                    tool_use_id: body.tool_use_id,
                    extensions: body.extensions,
                },
            },
        ),
        other => {
            let error = format!("unsupported control request subtype `{}`", other.kind());
            return refuse(error).await;
        }
    };
    emit_incoming(inner, turn_tx, request_id, kind, event).await;
}

async fn emit_incoming(
    inner: &QueryInner,
    turn_tx: &mpsc::Sender<Result<SdkEvent, Error>>,
    request_id: String,
    kind: IncomingRequestKind,
    event: SdkEvent,
) {
    inner
        .pending_incoming
        .lock()
        .await
        .insert(request_id.clone(), kind);
    if turn_tx.send(Ok(event)).await.is_err() {
        inner.pending_incoming.lock().await.remove(&request_id);
    }
}

fn permission_request(body: CanUseToolRequest, request_id: &str) -> SdkEvent {
    SdkEvent::PermissionRequest {
        id: request_id.to_owned(),
        tool_name: body.tool_name,
        input: body.input,
        suggestions: body.permission_suggestions.unwrap_or_default(),
        blocked_path: body.blocked_path,
    }
}

fn elicitation_request(body: ElicitationRequestBody) -> Result<ElicitationRequest, String> {
    let mode = match body.mode.as_deref() {
        None => None,
        Some("form") => Some(ElicitationMode::Form),
        Some("url") => Some(ElicitationMode::Url),
        Some(other) => return Err(format!("unknown elicitation mode `{other}`")),
    };
    Ok(ElicitationRequest {
        server_name: body.mcp_server_name,
        message: body.message,
        mode,
        url: body.url,
        elicitation_id: body.elicitation_id,
        requested_schema: body.requested_schema,
        title: body.title,
        display_name: body.display_name,
        description: body.description,
        extensions: body.extensions,
    })
}

fn deserialize_permission_updates(
    value: serde_json::Value,
) -> Result<Vec<PermissionUpdate>, String> {
    serde_json::from_value(value)
        .map_err(|error| format!("invalid permission_suggestions: {error}"))
}

fn object_extensions(
    value: &serde_json::Value,
    known: &[&str],
) -> serde_json::Map<String, serde_json::Value> {
    let mut extensions = value.as_object().cloned().unwrap_or_default();
    for field in known {
        extensions.remove(*field);
    }
    extensions
}

fn hook_callback(
    body: HookCallbackRequest,
    request_id: &str,
    inner: &QueryInner,
) -> Result<(HookInput, HookCallbackContext), String> {
    if !inner.hook_callback_ids.contains(&body.callback_id) {
        return Err(format!(
            "no hook subscription found for ID: {}",
            body.callback_id
        ));
    }
    let input = parse_hook_input(&body.input)?;
    let context = HookCallbackContext {
        request_id: request_id.to_owned(),
        tool_use_id: body.tool_use_id,
        extensions: body.extensions,
    };
    Ok((input, context))
}

fn parse_hook_input(value: &serde_json::Value) -> Result<HookInput, String> {
    let event_name = string_field(value, "hook_event_name")
        .or_else(|| string_field(value, "hookEventName"))
        .ok_or_else(|| "hook callback is missing hook_event_name".to_string())?;

    let event = match event_name.as_str() {
        "PreToolUse" => HookEventData::PreToolUse {
            tool_name: required_string_field(value, "tool_name")?,
            tool_input: value.get("tool_input").cloned().unwrap_or_default(),
            tool_use_id: required_string_field(value, "tool_use_id")?,
        },
        "PostToolUse" => HookEventData::PostToolUse {
            tool_name: required_string_field(value, "tool_name")?,
            tool_input: value.get("tool_input").cloned().unwrap_or_default(),
            tool_response: value.get("tool_response").cloned().unwrap_or_default(),
            tool_use_id: required_string_field(value, "tool_use_id")?,
        },
        "PostToolUseFailure" => HookEventData::PostToolUseFailure {
            tool_name: required_string_field(value, "tool_name")?,
            tool_input: value.get("tool_input").cloned().unwrap_or_default(),
            tool_use_id: required_string_field(value, "tool_use_id")?,
            error: required_string_field(value, "error")?,
            is_interrupt: bool_field(value, "is_interrupt"),
        },
        "PostToolBatch" => HookEventData::PostToolBatch {
            tool_calls: deserialize_field(value, "tool_calls")?,
        },
        "Notification" => HookEventData::Notification {
            message: required_string_field(value, "message")?,
            title: string_field(value, "title"),
            notification_type: required_string_field(value, "notification_type")?,
        },
        "UserPromptSubmit" => HookEventData::UserPromptSubmit {
            prompt: required_string_field(value, "prompt")?,
        },
        "UserPromptExpansion" => HookEventData::UserPromptExpansion {
            expansion_type: required_string_field(value, "expansion_type")?,
            command_name: required_string_field(value, "command_name")?,
            command_args: required_string_field(value, "command_args")?,
            command_source: string_field(value, "command_source"),
            prompt: required_string_field(value, "prompt")?,
        },
        "SessionStart" => HookEventData::SessionStart {
            source: deserialize_field(value, "source")?,
            model: string_field(value, "model"),
        },
        "SessionEnd" => HookEventData::SessionEnd {
            reason: required_string_field(value, "reason")?,
        },
        "Stop" => HookEventData::Stop {
            stop_hook_active: required_bool_field(value, "stop_hook_active")?,
            last_assistant_message: string_field(value, "last_assistant_message"),
        },
        "StopFailure" => HookEventData::StopFailure {
            error: required_string_field(value, "error")?,
            error_details: string_field(value, "error_details"),
            last_assistant_message: string_field(value, "last_assistant_message"),
        },
        "SubagentStart" => HookEventData::SubagentStart {
            agent_id: required_string_field(value, "agent_id")?,
            agent_type: required_string_field(value, "agent_type")?,
        },
        "SubagentStop" => HookEventData::SubagentStop {
            stop_hook_active: required_bool_field(value, "stop_hook_active")?,
            agent_id: required_string_field(value, "agent_id")?,
            agent_transcript_path: required_string_field(value, "agent_transcript_path")?,
            agent_type: required_string_field(value, "agent_type")?,
            last_assistant_message: string_field(value, "last_assistant_message"),
        },
        "PreCompact" => HookEventData::PreCompact {
            trigger: deserialize_field(value, "trigger")?,
            custom_instructions: string_field(value, "custom_instructions"),
        },
        "PostCompact" => HookEventData::PostCompact {
            trigger: deserialize_field(value, "trigger")?,
            compact_summary: required_string_field(value, "compact_summary")?,
        },
        "PermissionRequest" => HookEventData::PermissionRequest {
            tool_name: required_string_field(value, "tool_name")?,
            tool_input: value.get("tool_input").cloned().unwrap_or_default(),
            permission_suggestions: value
                .get("permission_suggestions")
                .cloned()
                .map(deserialize_permission_updates)
                .transpose()?
                .filter(|items| !items.is_empty()),
        },
        "PermissionDenied" => HookEventData::PermissionDenied {
            tool_name: required_string_field(value, "tool_name")?,
            tool_input: value.get("tool_input").cloned().unwrap_or_default(),
            tool_use_id: required_string_field(value, "tool_use_id")?,
            reason: required_string_field(value, "reason")?,
        },
        "Setup" => HookEventData::Setup {
            trigger: deserialize_field(value, "trigger")?,
        },
        "TeammateIdle" => HookEventData::TeammateIdle {
            teammate_name: required_string_field(value, "teammate_name")?,
            team_name: required_string_field(value, "team_name")?,
        },
        "TaskCreated" => HookEventData::TaskCreated {
            task_id: required_string_field(value, "task_id")?,
            task_subject: required_string_field(value, "task_subject")?,
            task_description: string_field(value, "task_description"),
            teammate_name: string_field(value, "teammate_name"),
            team_name: string_field(value, "team_name"),
        },
        "TaskCompleted" => HookEventData::TaskCompleted {
            task_id: required_string_field(value, "task_id")?,
            task_subject: required_string_field(value, "task_subject")?,
            task_description: string_field(value, "task_description"),
            teammate_name: string_field(value, "teammate_name"),
            team_name: string_field(value, "team_name"),
        },
        "Elicitation" => HookEventData::Elicitation {
            mcp_server_name: required_string_field(value, "mcp_server_name")?,
            message: required_string_field(value, "message")?,
            mode: value
                .get("mode")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| format!("invalid mode: {error}"))?,
            url: string_field(value, "url"),
            elicitation_id: string_field(value, "elicitation_id"),
            requested_schema: value.get("requested_schema").cloned(),
        },
        "ElicitationResult" => HookEventData::ElicitationResult {
            mcp_server_name: required_string_field(value, "mcp_server_name")?,
            elicitation_id: string_field(value, "elicitation_id"),
            mode: value
                .get("mode")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| format!("invalid mode: {error}"))?,
            action: required_string_field(value, "action")?,
            content: value.get("content").cloned(),
        },
        "ConfigChange" => HookEventData::ConfigChange {
            source: deserialize_field(value, "source")?,
            file_path: string_field(value, "file_path"),
        },
        "WorktreeCreate" => HookEventData::WorktreeCreate {
            name: required_string_field(value, "name")?,
        },
        "WorktreeRemove" => HookEventData::WorktreeRemove {
            worktree_path: required_string_field(value, "worktree_path")?,
        },
        "InstructionsLoaded" => HookEventData::InstructionsLoaded {
            file_path: required_string_field(value, "file_path")?,
            memory_type: required_string_field(value, "memory_type")?,
            load_reason: required_string_field(value, "load_reason")?,
            globs: value
                .get("globs")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| format!("invalid globs: {error}"))?,
            trigger_file_path: string_field(value, "trigger_file_path"),
            parent_file_path: string_field(value, "parent_file_path"),
        },
        "CwdChanged" => HookEventData::CwdChanged {
            old_cwd: required_string_field(value, "old_cwd")?,
            new_cwd: required_string_field(value, "new_cwd")?,
        },
        "FileChanged" => HookEventData::FileChanged {
            file_path: required_string_field(value, "file_path")?,
            event: required_string_field(value, "event")?,
        },
        "DirectoryAdded" => HookEventData::DirectoryAdded {
            directory: required_string_field(value, "directory")?,
            source: required_string_field(value, "source")?,
        },
        "MessageDisplay" => HookEventData::MessageDisplay {
            turn_id: required_string_field(value, "turn_id")?,
            message_id: required_string_field(value, "message_id")?,
            index: deserialize_field(value, "index")?,
            final_delta: required_bool_field(value, "final")?,
            delta: required_string_field(value, "delta")?,
        },
        _ => HookEventData::Unknown(crate::driver::sdk::types::RawFrame::new(value.clone())),
    };

    Ok(HookInput {
        session_id: required_string_field(value, "session_id")?,
        transcript_path: required_string_field(value, "transcript_path")?,
        cwd: required_string_field(value, "cwd")?,
        prompt_id: string_field(value, "prompt_id"),
        permission_mode: string_field(value, "permission_mode"),
        agent_id: string_field(value, "agent_id"),
        agent_type: string_field(value, "agent_type"),
        effort: value
            .get("effort")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| format!("invalid effort: {error}"))?,
        event,
        extensions: object_extensions(
            value,
            &[
                "hook_event_name",
                "hookEventName",
                "session_id",
                "transcript_path",
                "cwd",
                "prompt_id",
                "permission_mode",
                "agent_id",
                "agent_type",
                "effort",
            ],
        ),
    })
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_owned)
}

fn required_string_field(value: &serde_json::Value, key: &str) -> Result<String, String> {
    string_field(value, key).ok_or_else(|| format!("hook callback is missing {key}"))
}

fn bool_field(value: &serde_json::Value, key: &str) -> Option<bool> {
    value.get(key).and_then(|v| v.as_bool())
}

fn required_bool_field(value: &serde_json::Value, key: &str) -> Result<bool, String> {
    bool_field(value, key).ok_or_else(|| format!("hook callback is missing {key}"))
}

fn deserialize_field<T: serde::de::DeserializeOwned>(
    value: &serde_json::Value,
    key: &str,
) -> Result<T, String> {
    serde_json::from_value(
        value
            .get(key)
            .cloned()
            .ok_or_else(|| format!("hook callback is missing {key}"))?,
    )
    .map_err(|error| format!("failed to parse {key}: {error}"))
}

async fn send_control_success(
    inner: &QueryInner,
    request_id: &str,
    response: serde_json::Value,
) -> Result<(), ()> {
    let envelope = serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response,
        }
    });
    let json = serde_json::to_vec(&envelope).map_err(|_| ())?;
    inner.write(json).await.map_err(|_| ())
}

async fn send_control_error(inner: &QueryInner, request_id: &str, error: &str) -> Result<(), ()> {
    let envelope = serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "error",
            "request_id": request_id,
            "error": error,
        }
    });
    let json = serde_json::to_vec(&envelope).map_err(|_| ())?;
    inner.write(json).await.map_err(|_| ())
}

pub(crate) enum WriteCommand {
    Data {
        data: Vec<u8>,
        ack: Option<oneshot::Sender<Result<(), String>>>,
    },
    Close,
}

/// Spawn the background writer task that serializes stdin writes.
pub(crate) fn spawn_writer_task(
    mut stdin: impl AsyncWrite + Unpin + Send + 'static,
    mut rx: mpsc::UnboundedReceiver<WriteCommand>,
    shutdown: Arc<Shutdown>,
    output_tx: mpsc::Sender<Result<SdkEvent, Error>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let cancel = shutdown.token();
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                msg = rx.recv() => {
                    match msg {
                        Some(WriteCommand::Data { data, ack }) => {
                            let result = async {
                                stdin.write_all(&data).await?;
                                stdin.write_all(b"\n").await?;
                                stdin.flush().await
                            }.await;
                            if let Err(error) = result {
                                let message = format!("failed writing Claude stdin: {error}");
                                if let Some(ack) = ack {
                                    let _ = ack.send(Err(message.clone()));
                                }
                                let _ = output_tx.send(Err(Error::Send(format!(
                                    "failed writing Claude stdin: {error}"
                                )))).await;
                                shutdown.request(ShutdownReason::TransportFailed);
                                break;
                            }
                            if let Some(ack) = ack {
                                let _ = ack.send(Ok(()));
                            }
                        }
                        Some(WriteCommand::Close) => break,
                        None => break,
                    }
                }
            }
        }
        drop(stdin);
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;

    use tokio::io::AsyncWriteExt;
    use tokio::sync::{Mutex, mpsc};
    use tokio::time::timeout;

    use super::*;
    use crate::driver::sdk::control::{self, ControlRequestBody};

    /// A wait here proves a task completes at all, not that it completes
    /// quickly; a loaded machine can stall a runtime for seconds, so only a
    /// genuine hang should trip this budget.
    const TEST_DEADLINE: Duration = Duration::from_secs(30);

    fn test_inner(stdin_tx: mpsc::UnboundedSender<WriteCommand>) -> Arc<QueryInner> {
        Arc::new(QueryInner {
            session_id: "test-session".to_owned(),
            stdin_tx,
            pending_controls: Mutex::new(HashMap::new()),
            init_result: OnceLock::new(),
            request_counter: AtomicU64::new(0),
            initialize_request: InitializeRequestBody::default(),
            pending_incoming: Mutex::new(HashMap::new()),
            hook_callback_ids: HashSet::new(),
        })
    }

    #[tokio::test]
    async fn reader_reports_each_malformed_line_and_continues() {
        let (stdin_tx, _stdin_rx) = mpsc::unbounded_channel();
        let inner = test_inner(stdin_tx);
        let (turn_tx, mut turn_rx) = mpsc::channel(4);
        let shutdown = Shutdown::new();
        let (mut writer, reader) = tokio::io::duplex(4096);
        writer
            .write_all(
                concat!(
                    "not-json\n",
                    r#"{"type":"prompt_suggestion"}"#,
                    "\n",
                    r#"{"type":"prompt_suggestion","suggestion":"next","uuid":"11111111-1111-4111-8111-111111111111","session_id":"s"}"#,
                    "\n",
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        drop(writer);

        reader_loop(tokio::io::BufReader::new(reader), turn_tx, inner, shutdown).await;

        assert!(matches!(
            turn_rx.recv().await,
            Some(Err(Error::Protocol(_)))
        ));
        assert!(matches!(
            turn_rx.recv().await,
            Some(Err(Error::Protocol(_)))
        ));
        assert!(matches!(
            turn_rx.recv().await,
            Some(Ok(SdkEvent::Message(Message::PromptSuggestion(_))))
        ));
    }

    #[tokio::test]
    async fn malformed_control_response_is_not_dropped() {
        let (stdin_tx, _stdin_rx) = mpsc::unbounded_channel();
        let inner = test_inner(stdin_tx);
        let (turn_tx, mut turn_rx) = mpsc::channel(1);

        dispatch_line(
            r#"{"type":"control_response","response":{}}"#,
            &turn_tx,
            &inner,
        )
        .await;

        assert!(matches!(
            turn_rx.recv().await,
            Some(Err(Error::Protocol(_)))
        ));
    }

    #[tokio::test]
    async fn reader_accounts_for_empty_lines() {
        let (stdin_tx, _stdin_rx) = mpsc::unbounded_channel();
        let inner = test_inner(stdin_tx);
        let (turn_tx, mut turn_rx) = mpsc::channel(1);
        let shutdown = Shutdown::new();
        let (mut writer, reader) = tokio::io::duplex(16);
        writer.write_all(b"\n").await.unwrap();
        drop(writer);

        reader_loop(tokio::io::BufReader::new(reader), turn_tx, inner, shutdown).await;

        assert!(matches!(
            turn_rx.recv().await,
            Some(Err(Error::Protocol(_)))
        ));
    }

    #[tokio::test]
    async fn send_control_removes_pending_request_on_send_failure() {
        let (stdin_tx, stdin_rx) = mpsc::unbounded_channel();
        drop(stdin_rx);
        let inner = test_inner(stdin_tx);

        let error = inner
            .send_control(ControlRequestBody::Interrupt(control::InterruptRequest {
                cancel_queued: None,
                extensions: Default::default(),
            }))
            .await
            .unwrap_err();

        assert!(matches!(error, Error::Send(_)));
        assert!(inner.pending_controls.lock().await.is_empty());
    }

    #[tokio::test]
    async fn dropping_pending_controls_unblocks_waiters() {
        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel();
        let inner = test_inner(stdin_tx);
        let task_inner = inner.clone();
        let task = tokio::spawn(async move {
            task_inner
                .send_control(ControlRequestBody::Interrupt(control::InterruptRequest {
                    cancel_queued: None,
                    extensions: Default::default(),
                }))
                .await
        });

        let command = stdin_rx
            .recv()
            .await
            .expect("control request should be sent");
        let WriteCommand::Data { ack: Some(ack), .. } = command else {
            panic!("expected acknowledged control request data")
        };
        ack.send(Ok(())).unwrap();
        drop_pending_controls(inner.clone()).await;

        let error = timeout(TEST_DEADLINE, task)
            .await
            .expect("send_control should not hang")
            .expect("task should complete")
            .unwrap_err();
        assert!(matches!(error, Error::Control(_)));
        assert!(inner.pending_controls.lock().await.is_empty());
    }
}
