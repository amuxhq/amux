//! What Grok says beside ACP that amux reads: how much of the context its
//! session uses, its tool servers, and its own review of a call before the
//! call runs. Each is read only for an agent whose quirks say it speaks so.

use serde::Deserialize;
use serde_json::Value;
use wire::{ToolServer, ToolServerStatus};

/// Grok's report of one of its tool servers (`_x.ai/mcp/server_status`).
pub(super) const SERVER_STATUS: &str = "_x.ai/mcp/server_status";

/// Grok's list of the models it offers, after it changes
/// (`_x.ai/models/update`).
pub(super) const MODELS_UPDATE: &str = "_x.ai/models/update";

/// The words Grok's automatic mode answers a call it blocked with, in the
/// call's content (`grok-allow`): the call never ran, so it is denied, not
/// failed.
const BLOCKED: &str = "was not executed: Auto mode blocked this action";

/// The context a session uses, in tokens, from a `session/update`'s
/// `_meta` (`grok-allow`): Grok sends no `usage_update`.
pub(super) fn context_used(meta: Option<&Value>) -> Option<u64> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct UpdateMeta {
        #[serde(default)]
        total_tokens: Option<u64>,
    }
    UpdateMeta::deserialize(meta?).ok()?.total_tokens
}

/// A list of models whose `_meta` names each one's window:
/// `initialize`'s `_meta.modelState`, a session answer's `models` and
/// `_x.ai/models/update` all have this shape (`grok-catalogue`,
/// `grok-allow`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Models {
    #[serde(default)]
    available_models: Vec<Model>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Model {
    model_id: String,
    #[serde(rename = "_meta", default)]
    meta: Option<ModelMeta>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelMeta {
    #[serde(default)]
    total_context_tokens: Option<u64>,
}

impl Models {
    fn windows(self) -> Vec<(String, u64)> {
        self.available_models
            .into_iter()
            .filter_map(|model| Some((model.model_id, model.meta?.total_context_tokens?)))
            .collect()
    }
}

/// Each model's context window, by its id, from `_x.ai/models/update`.
pub(super) fn windows(params: &Value) -> Vec<(String, u64)> {
    Models::deserialize(params)
        .map(Models::windows)
        .unwrap_or_default()
}

/// The windows in `initialize`'s `_meta`, under its model state.
pub(super) fn windows_at_start(meta: Option<&Value>) -> Vec<(String, u64)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct InitializeMeta {
        #[serde(default)]
        model_state: Option<Models>,
    }
    meta.and_then(|meta| InitializeMeta::deserialize(meta).ok())
        .and_then(|meta| meta.model_state)
        .map(Models::windows)
        .unwrap_or_default()
}

/// The windows in a session answer, under its models.
pub(super) fn windows_in_session(answer: &Value) -> Vec<(String, u64)> {
    #[derive(Deserialize)]
    struct SessionAnswer {
        #[serde(default)]
        models: Option<Models>,
    }
    SessionAnswer::deserialize(answer)
        .ok()
        .and_then(|answer| answer.models)
        .map(Models::windows)
        .unwrap_or_default()
}

/// One tool server as Grok reports it: ready, or unavailable with why.
/// A server that could not start because it wants a sign-in says so only
/// in its words ("Auth required"), so those words are read for it.
pub(super) fn server(params: &Value) -> Option<ToolServer> {
    #[derive(Deserialize)]
    struct Status {
        name: String,
        status: String,
        #[serde(default)]
        detail: Option<String>,
    }
    let reported = Status::deserialize(params).ok()?;
    let detail = reported.detail.unwrap_or_default();
    let status = match reported.status.as_str() {
        "ready" => ToolServerStatus::Ready,
        "starting" | "initializing" | "connecting" => ToolServerStatus::Starting,
        "unavailable" | "failed" | "error" if detail.contains("Auth required") => {
            ToolServerStatus::NeedsAuth
        }
        "unavailable" | "failed" | "error" => ToolServerStatus::Failed,
        _ => ToolServerStatus::Unspecified,
    };
    Some(ToolServer {
        name: reported.name,
        status: status as i32,
        error: if status == ToolServerStatus::Ready {
            String::new()
        } else {
            detail
        },
    })
}

/// Grok's own events about a call: its reviewer took the call up
/// (`pending_interaction` of kind `permission`) or finished with it
/// (`interaction_resolved`), as in `grok-allow`.
pub(super) enum Review {
    Started(String),
    Resolved(String),
}

/// The review an update of `_x.ai/session_notification` tells of: its tag,
/// its kind and the call it is about.
pub(super) fn review(tag: &str, kind: Option<&str>, call: Option<&str>) -> Option<Review> {
    let id = call?.to_owned();
    match tag {
        "pending_interaction" if kind == Some("permission") => Some(Review::Started(id)),
        "interaction_resolved" => Some(Review::Resolved(id)),
        _ => None,
    }
}

/// Whether a failed call's words say Grok's reviewer blocked it.
pub(super) fn blocked(output: &str) -> bool {
    output.contains(BLOCKED)
}
