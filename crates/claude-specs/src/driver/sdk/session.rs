use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_core::Stream;
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::sync::watch;

use crate::driver::sdk::Error;
use crate::driver::sdk::abort::{AbortHandle, Shutdown, ShutdownReason};
use crate::driver::sdk::control::{
    self, ControlRequestBody, FlagSettings, InterruptResult, McpPermissionMode,
    McpPermissionModeOverrideResult, McpServerStatus, McpSetServersResult, ReloadPluginsResult,
    ReloadSkillsResult, RewindFilesResult,
};
use crate::driver::sdk::dispatch::{IncomingRequestKind, QueryInner};
use crate::driver::sdk::init::{AppliedSettings, ContextUsage, InitializationResult};
use crate::driver::sdk::message::Message;
use crate::driver::sdk::options::{
    ElicitationRequest, ElicitationResult, HookCallbackContext, HookInput, HookOutput,
    McpServerConfig, QueryOptions, UserDialogRequest, UserDialogResult,
};
use crate::driver::sdk::query::{self, ProcessExit, Query, UserMessage};
use crate::driver::sdk::types::{PermissionMode, PermissionResult, PermissionUpdate};

/// The identifier Claude assigns to one incoming control request.
pub type RequestId = String;

/// The typed permission suggestion sent with a permission request.
pub type PermissionSuggestion = PermissionUpdate;

/// One ordered item emitted by a Claude SDK session.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum SdkEvent {
    Message(Message),
    PermissionRequest {
        id: RequestId,
        tool_name: String,
        input: serde_json::Value,
        suggestions: Vec<PermissionSuggestion>,
        blocked_path: Option<String>,
    },
    HookCallback {
        id: RequestId,
        input: HookInput,
        context: HookCallbackContext,
    },
    Elicitation {
        id: RequestId,
        request: ElicitationRequest,
    },
    UserDialog {
        id: RequestId,
        request: UserDialogRequest,
    },
    Exited(ProcessExit),
}

/// One owned event stream and its independently cloneable control handle.
pub struct Session {
    pub events: EventStream,
    pub control: Control,
}

/// The sole consumer of a session's ordered SDK events.
pub struct EventStream {
    query: Query,
}

impl Stream for EventStream {
    type Item = Result<SdkEvent, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.query).poll_next(cx)
    }
}

/// Commands and introspection for one Claude SDK session.
#[derive(Clone)]
pub struct Control {
    inner: Arc<QueryInner>,
    shutdown: Arc<Shutdown>,
    exit_rx: watch::Receiver<Option<ProcessExit>>,
}

impl Control {
    pub async fn prompt(&self, message: UserMessage) -> Result<(), Error> {
        query::send_user_message(&self.inner, &message).await
    }

    pub async fn interrupt(&self) -> Result<Option<InterruptResult>, Error> {
        let response = self
            .inner
            .send_control(ControlRequestBody::Interrupt(control::InterruptRequest {
                cancel_queued: None,
                extensions: Default::default(),
            }))
            .await?;
        if response.is_null() || response.as_object().is_some_and(serde_json::Map::is_empty) {
            return Ok(None);
        }
        serde_json::from_value(response)
            .map(Some)
            .map_err(|error| Error::Control(format!("failed to parse interrupt receipt: {error}")))
    }

    pub async fn answer_permission(
        &self,
        id: RequestId,
        result: PermissionResult,
    ) -> Result<(), Error> {
        self.inner
            .answer_incoming(
                id,
                IncomingRequestKind::Permission,
                serde_json::to_value(result)?,
            )
            .await
    }

    pub async fn answer_hook(&self, id: RequestId, output: HookOutput) -> Result<(), Error> {
        let response = serde_json::to_value(output)?;
        self.inner
            .answer_incoming(id, IncomingRequestKind::Hook, response)
            .await
    }

    pub async fn answer_elicitation(
        &self,
        id: RequestId,
        result: ElicitationResult,
    ) -> Result<(), Error> {
        let response = serde_json::to_value(result)?;
        self.inner
            .answer_incoming(id, IncomingRequestKind::Elicitation, response)
            .await
    }

    pub async fn answer_user_dialog(
        &self,
        id: RequestId,
        result: UserDialogResult,
    ) -> Result<(), Error> {
        let response = serde_json::to_value(result)?;
        self.inner
            .answer_incoming(id, IncomingRequestKind::UserDialog, response)
            .await
    }

    pub async fn set_permission_mode(
        &self,
        mode: PermissionMode,
    ) -> Result<Option<PermissionMode>, Error> {
        let response = self
            .inner
            .send_control(ControlRequestBody::SetPermissionMode(
                control::SetPermissionModeRequest {
                    mode,
                    extensions: Default::default(),
                },
            ))
            .await?;
        let Some(applied) = response.get("mode") else {
            return Ok(None);
        };
        serde_json::from_value(applied.clone())
            .map(Some)
            .map_err(|error| {
                Error::Control(format!(
                    "failed to parse the acknowledged permission mode: {error}"
                ))
            })
    }

    pub async fn set_model(&self, model: Option<&str>) -> Result<(), Error> {
        self.inner
            .send_control(ControlRequestBody::SetModel(control::SetModelRequest {
                model: model.map(str::to_owned),
                extensions: Default::default(),
            }))
            .await?;
        Ok(())
    }

    pub async fn set_mcp_permission_mode_override(
        &self,
        server_name: &str,
        mode: Option<McpPermissionMode>,
    ) -> Result<McpPermissionModeOverrideResult, Error> {
        let response = self
            .inner
            .send_control(ControlRequestBody::SetMcpPermissionModeOverride(
                control::McpPermissionModeOverrideRequest {
                    server_name: server_name.to_owned(),
                    mode,
                    extensions: Default::default(),
                },
            ))
            .await?;
        if response.is_null() {
            return Ok(McpPermissionModeOverrideResult {
                warning: None,
                extensions: Default::default(),
            });
        }
        serde_json::from_value(response).map_err(|error| {
            Error::Control(format!("failed to parse MCP permission override: {error}"))
        })
    }

    pub async fn apply_flag_settings(&self, settings: FlagSettings) -> Result<(), Error> {
        self.inner
            .send_control(ControlRequestBody::ApplyFlagSettings(
                control::ApplyFlagSettingsRequest {
                    settings,
                    extensions: Default::default(),
                },
            ))
            .await?;
        Ok(())
    }

    pub async fn reinitialize(&self) -> Result<InitializationResult, Error> {
        let response = self
            .inner
            .send_control(ControlRequestBody::Initialize(
                self.inner.initialize_request.clone(),
            ))
            .await?;
        serde_json::from_value(response)
            .map_err(|error| Error::Control(format!("failed to parse init response: {error}")))
    }

    pub fn initialization_result(&self) -> Option<&InitializationResult> {
        self.inner.init_result.get()
    }

    pub fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    pub async fn mcp_server_status(&self) -> Result<Vec<McpServerStatus>, Error> {
        let response = self
            .inner
            .send_control(ControlRequestBody::McpStatus(Default::default()))
            .await?;
        let response = response
            .get("mcpServers")
            .cloned()
            .ok_or_else(|| Error::Control("mcp status response omitted mcpServers".into()))?;
        serde_json::from_value(response)
            .map_err(|error| Error::Control(format!("failed to parse mcp status: {error}")))
    }

    pub async fn get_context_usage(&self) -> Result<ContextUsage, Error> {
        let response = self
            .inner
            .send_control(ControlRequestBody::GetContextUsage(Default::default()))
            .await?;
        serde_json::from_value(response)
            .map_err(|error| Error::Control(format!("failed to parse context usage: {error}")))
    }

    /// The model and effort the session runs with now, as Claude applied
    /// them from every settings source and the session's own changes.
    pub async fn applied_settings(&self) -> Result<AppliedSettings, Error> {
        let response = self
            .inner
            .send_control(ControlRequestBody::GetSettings(Default::default()))
            .await?;
        let applied = response.get("applied").cloned().unwrap_or_default();
        serde_json::from_value(applied)
            .map_err(|error| Error::Control(format!("failed to parse applied settings: {error}")))
    }

    pub async fn reload_plugins(&self) -> Result<ReloadPluginsResult, Error> {
        let response = self
            .inner
            .send_control(ControlRequestBody::ReloadPlugins(Default::default()))
            .await?;
        serde_json::from_value(response)
            .map_err(|error| Error::Control(format!("failed to parse plugin reload: {error}")))
    }

    pub async fn reload_skills(&self) -> Result<ReloadSkillsResult, Error> {
        let response = self
            .inner
            .send_control(ControlRequestBody::ReloadSkills(Default::default()))
            .await?;
        serde_json::from_value(response)
            .map_err(|error| Error::Control(format!("failed to parse skill reload: {error}")))
    }

    pub async fn reconnect_mcp_server(&self, name: &str) -> Result<(), Error> {
        self.inner
            .send_control(ControlRequestBody::McpReconnect(
                control::McpServerRequest {
                    server_name: name.to_owned(),
                    extensions: Default::default(),
                },
            ))
            .await?;
        Ok(())
    }

    pub async fn toggle_mcp_server(&self, name: &str, enabled: bool) -> Result<(), Error> {
        self.inner
            .send_control(ControlRequestBody::McpToggle(control::McpToggleRequest {
                server_name: name.to_owned(),
                enabled,
                extensions: Default::default(),
            }))
            .await?;
        Ok(())
    }

    pub async fn rewind_files(
        &self,
        user_message_id: &str,
        dry_run: Option<bool>,
    ) -> Result<RewindFilesResult, Error> {
        let response = self
            .inner
            .send_control(ControlRequestBody::RewindFiles(
                control::RewindFilesRequest {
                    user_message_id: user_message_id.to_owned(),
                    dry_run,
                    extensions: Default::default(),
                },
            ))
            .await?;
        serde_json::from_value(response)
            .map_err(|error| Error::Control(format!("failed to parse rewind result: {error}")))
    }

    pub async fn seed_read_state(&self, path: &str, mtime: u64) -> Result<(), Error> {
        self.inner
            .send_control(ControlRequestBody::SeedReadState(
                control::SeedReadStateRequest {
                    path: path.to_owned(),
                    mtime,
                    extensions: Default::default(),
                },
            ))
            .await?;
        Ok(())
    }

    pub async fn set_mcp_servers(
        &self,
        servers: HashMap<String, McpServerConfig>,
    ) -> Result<McpSetServersResult, Error> {
        for (name, config) in &servers {
            if let Some(server) = config.sdk_server()
                && server.configured_name() != name
            {
                return Err(Error::InvalidOptions(format!(
                    "SDK MCP server map key `{name}` must match configured name `{}`",
                    server.configured_name()
                )));
            }
        }
        let sdk_servers = servers
            .iter()
            .filter_map(|(name, config)| {
                config
                    .sdk_server()
                    .cloned()
                    .map(|server| (name.clone(), server))
            })
            .collect();
        *self
            .inner
            .sdk_mcp_servers
            .write()
            .expect("SDK MCP server lock poisoned") = sdk_servers;
        let response = self
            .inner
            .send_control(ControlRequestBody::McpSetServers(
                control::McpSetServersRequest {
                    servers: servers
                        .iter()
                        .map(|(name, config)| (name.clone(), config.to_wire()))
                        .collect(),
                    extensions: Default::default(),
                },
            ))
            .await?;
        serde_json::from_value(response)
            .map_err(|error| Error::Control(format!("failed to parse MCP server update: {error}")))
    }

    pub async fn stop_task(&self, task_id: &str) -> Result<(), Error> {
        self.inner
            .send_control(ControlRequestBody::StopTask(control::StopTaskRequest {
                task_id: task_id.to_owned(),
                extensions: Default::default(),
            }))
            .await?;
        Ok(())
    }

    pub async fn background_tasks(&self, tool_use_id: Option<&str>) -> Result<bool, Error> {
        let response = self
            .inner
            .send_control(ControlRequestBody::BackgroundTasks(
                control::BackgroundTasksRequest {
                    tool_use_id: tool_use_id.map(str::to_owned),
                    extensions: Default::default(),
                },
            ))
            .await?;
        match response.get("backgrounded") {
            None | Some(serde_json::Value::Null) => Ok(true),
            Some(serde_json::Value::Bool(backgrounded)) => Ok(*backgrounded),
            Some(other) => Err(Error::Control(format!(
                "background tasks control answered with a non-boolean `backgrounded`: {other}"
            ))),
        }
    }

    pub fn abort_handle(&self) -> AbortHandle {
        AbortHandle::new(self.shutdown.clone())
    }

    pub fn process_exit(&self) -> Option<ProcessExit> {
        self.exit_rx.borrow().clone()
    }

    pub async fn close(mut self) -> ProcessExit {
        self.shutdown.request(ShutdownReason::Closed);
        query::wait_for_exit(&mut self.exit_rx).await
    }
}

pub(crate) fn from_query(query: Query) -> Session {
    let control = Control {
        inner: query.inner.clone(),
        shutdown: query.shutdown.clone(),
        exit_rx: query.exit_rx.clone(),
    };
    Session {
        events: EventStream { query },
        control,
    }
}

/// Open an initialized session over caller-owned stream-JSON transport.
pub async fn from_io(
    reader: impl AsyncBufRead + Unpin + Send + 'static,
    writer: impl AsyncWrite + Unpin + Send + 'static,
    options: QueryOptions,
) -> Result<Session, Error> {
    let warm = query::WarmQuery::from_io(options, reader, writer, Duration::from_secs(60)).await?;
    Ok(from_query(warm.into_query()))
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;

    use futures_core::Stream;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};

    use super::*;
    use crate::driver::sdk::options::{
        HookEvent, HookOutput, HookSubscription, SyncHookOutput, UserDialogResult,
    };

    async fn next_event(events: &mut EventStream) -> Result<SdkEvent, Error> {
        std::future::poll_fn(|cx| Pin::new(&mut *events).poll_next(cx))
            .await
            .expect("event stream ended")
    }

    async fn read_json_line(reader: &mut BufReader<tokio::io::DuplexStream>) -> serde_json::Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn write_json_line(writer: &mut tokio::io::DuplexStream, value: serde_json::Value) {
        writer
            .write_all(value.to_string().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
    }

    #[tokio::test]
    async fn control_requests_are_ordered_events_answered_once_through_control() {
        let (sdk_stdin, server_stdin) = duplex(16 * 1024);
        let (server_stdout, sdk_stdout) = duplex(16 * 1024);
        let server = tokio::spawn(async move {
            let mut stdin = BufReader::new(server_stdin);
            let mut stdout = server_stdout;

            let init = read_json_line(&mut stdin).await;
            let init_id = init["request_id"].as_str().unwrap();
            let hook_id = init["request"]["hooks"]["PreToolUse"][0]["hookCallbackIds"][0]
                .as_str()
                .unwrap()
                .to_owned();
            write_json_line(
                &mut stdout,
                serde_json::json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "success",
                        "request_id": init_id,
                        "response": {
                            "commands": [],
                            "agents": [],
                            "output_style": "default",
                            "available_output_styles": [],
                            "models": [],
                            "account": {}
                        }
                    }
                }),
            )
            .await;

            let prompt = read_json_line(&mut stdin).await;
            assert_eq!(prompt["message"]["content"], "hello");
            write_json_line(
                &mut stdout,
                serde_json::json!({
                    "type": "prompt_suggestion",
                    "suggestion": "next",
                    "uuid": "00000000-0000-0000-0000-000000000000",
                    "session_id": "00000000-0000-0000-0000-000000000001"
                }),
            )
            .await;

            write_json_line(
                &mut stdout,
                serde_json::json!({
                    "type": "control_request",
                    "request_id": "permission-1",
                    "request": {
                        "subtype": "can_use_tool",
                        "tool_name": "Bash",
                        "input": {"command": "pwd"},
                        "permission_suggestions": [],
                        "blocked_path": "/tmp/blocked",
                        "tool_use_id": "tool-1"
                    }
                }),
            )
            .await;
            let permission = read_json_line(&mut stdin).await;
            assert_eq!(permission["response"]["request_id"], "permission-1");
            assert_eq!(permission["response"]["response"]["behavior"], "allow");

            write_json_line(
                &mut stdout,
                serde_json::json!({
                    "type": "control_request",
                    "request_id": "hook-1",
                    "request": {
                        "subtype": "hook_callback",
                        "callback_id": hook_id,
                        "tool_use_id": "tool-1",
                        "input": {
                            "hook_event_name": "PreToolUse",
                            "session_id": "00000000-0000-0000-0000-000000000001",
                            "transcript_path": "/tmp/transcript.jsonl",
                            "cwd": "/tmp",
                            "tool_name": "Bash",
                            "tool_input": {"command": "pwd"},
                            "tool_use_id": "tool-1"
                        }
                    }
                }),
            )
            .await;
            let hook = read_json_line(&mut stdin).await;
            assert_eq!(hook["response"]["request_id"], "hook-1");
            assert_eq!(hook["response"]["response"], serde_json::json!({}));

            write_json_line(
                &mut stdout,
                serde_json::json!({
                    "type": "control_request",
                    "request_id": "elicitation-1",
                    "request": {
                        "subtype": "elicitation",
                        "mcp_server_name": "forms",
                        "message": "Pick one"
                    }
                }),
            )
            .await;
            let elicitation = read_json_line(&mut stdin).await;
            assert_eq!(elicitation["response"]["request_id"], "elicitation-1");
            assert_eq!(elicitation["response"]["response"]["action"], "decline");

            write_json_line(
                &mut stdout,
                serde_json::json!({
                    "type": "control_request",
                    "request_id": "dialog-1",
                    "request": {
                        "subtype": "request_user_dialog",
                        "dialog_kind": "confirm",
                        "payload": {"title": "Continue?"},
                        "tool_use_id": "tool-1"
                    }
                }),
            )
            .await;
            let dialog = read_json_line(&mut stdin).await;
            assert_eq!(dialog["response"]["request_id"], "dialog-1");
            assert_eq!(dialog["response"]["response"]["behavior"], "cancelled");
        });

        let mut options = QueryOptions {
            session_id: Some("00000000-0000-0000-0000-000000000001".into()),
            ..QueryOptions::default()
        };
        options.hook_subscriptions.push(HookSubscription {
            event: HookEvent::PreToolUse,
            matcher: Some("Bash".into()),
        });
        let Session {
            mut events,
            control,
        } = from_io(BufReader::new(sdk_stdout), sdk_stdin, options)
            .await
            .unwrap();

        assert_eq!(control.session_id(), "00000000-0000-0000-0000-000000000001");
        assert!(control.initialization_result().is_some());
        control.prompt(UserMessage::text("hello")).await.unwrap();
        assert!(matches!(
            next_event(&mut events).await.unwrap(),
            SdkEvent::Message(Message::PromptSuggestion(_))
        ));

        let permission_id = match next_event(&mut events).await.unwrap() {
            SdkEvent::PermissionRequest {
                id,
                tool_name,
                input,
                suggestions,
                blocked_path,
            } => {
                assert_eq!(tool_name, "Bash");
                assert_eq!(input["command"], "pwd");
                assert!(suggestions.is_empty());
                assert_eq!(blocked_path.as_deref(), Some("/tmp/blocked"));
                id
            }
            other => panic!("unexpected event: {other:?}"),
        };
        control
            .answer_permission(
                permission_id.clone(),
                PermissionResult::Allow {
                    updated_input: None,
                    updated_permissions: None,
                    tool_use_id: Some("tool-1".into()),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            control
                .answer_permission(
                    permission_id,
                    PermissionResult::Deny {
                        message: "late".into(),
                        interrupt: None,
                        tool_use_id: None,
                    },
                )
                .await,
            Err(Error::UnknownRequest(_))
        ));

        let hook_id = match next_event(&mut events).await.unwrap() {
            SdkEvent::HookCallback { id, context, .. } => {
                assert_eq!(context.tool_use_id.as_deref(), Some("tool-1"));
                id
            }
            other => panic!("unexpected event: {other:?}"),
        };
        control
            .answer_hook(
                hook_id,
                HookOutput::Sync(SyncHookOutput {
                    r#continue: None,
                    suppress_output: None,
                    stop_reason: None,
                    decision: None,
                    system_message: None,
                    reason: None,
                    hook_specific_output: None,
                }),
            )
            .await
            .unwrap();

        let elicitation_id = match next_event(&mut events).await.unwrap() {
            SdkEvent::Elicitation { id, request } => {
                assert_eq!(request.server_name, "forms");
                id
            }
            other => panic!("unexpected event: {other:?}"),
        };
        control
            .answer_elicitation(
                elicitation_id,
                ElicitationResult::Decline {
                    extensions: Default::default(),
                },
            )
            .await
            .unwrap();

        let dialog_id = match next_event(&mut events).await.unwrap() {
            SdkEvent::UserDialog { id, request } => {
                assert_eq!(request.dialog_kind, "confirm");
                id
            }
            other => panic!("unexpected event: {other:?}"),
        };
        control
            .answer_user_dialog(
                dialog_id,
                UserDialogResult::Cancelled {
                    extensions: Default::default(),
                },
            )
            .await
            .unwrap();

        server.await.unwrap();
        assert!(matches!(
            next_event(&mut events).await.unwrap(),
            SdkEvent::Exited(ProcessExit {
                termination: crate::driver::sdk::Termination::Exited,
                ..
            })
        ));
    }
}
