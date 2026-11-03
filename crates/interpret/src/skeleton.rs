//! An event's skeleton: every field it has, with each value replaced by
//! what it is, so the agent's record can stand for the event without
//! holding anything that was said.
//!
//! A provider's fact is JSON and keeps its protocol's field names, as
//! below; an input, an agent message and the agent's other protobuf events
//! keep theirs by the wire schema's descriptor. Of the values, a skeleton keeps only those
//! that are not content:
//!
//! - the provider's fixed vocabulary: a status-like value its field's
//!   closed set in [`VOCABULARY`] names, and a protocol's own tags (a
//!   `type`, `subtype`, `method` or hook event name, as the record's
//!   variant already keeps them);
//! - bools and nulls, and numbers outside a tool's input and result and
//!   what a data-keyed map holds;
//! - a timestamp, and a built-in tool's name.
//!
//! Every other string becomes an alias of its value with its size,
//! `@<hash>~<bytes>`, and `/<lines>` when it has line breaks, so equal
//! strings alias alike within one record and the size of what was said
//! survives without the words. A provider's fixed phrase that opens a text
//! ([`MARKERS`]) is kept ahead of the alias of the rest,
//! `@<phrase>@<hash>~<bytes>`, whatever the rest holds, as is the `mcp__`
//! that names a tool server's tool. A number inside a
//! tool's input or result becomes `#<characters>`. A kept string is
//! written `=<value>`; a model, which is not a closed set but is not
//! content either, `%<hash>=<value>`, so a value that appears elsewhere
//! aliased stays recognisably the same one.
//!
//! A string that is JSON becomes `{"=json": <skeleton>, "bytes": <n>}`. A
//! text the provider frames with its own fixed phrases or tags
//! (`FRAMES`), a link, and a shell command become `{"=text": [...]}`,
//! the kept frames and separators between the aliases of what they hold;
//! a shell command keeps only the shell's name and flag, never the folder
//! the shell was found in.
//!
//! Field names are the protocol's except in the subtrees its protocol
//! crate names as the data's (`DATA_SUBTREES`: a tool's input and result,
//! a change's files, a server's schema, per-model tallies). Outside them a
//! name that reads as a field's is kept, so a field a new provider version
//! adds shows by name.
//! Inside them a name is kept only at a path `KEPT` lists for that
//! subtree, the protocol's own fields there and a built-in tool's; every
//! other name is aliased like a value, as is every name in JSON written
//! into a string and in a tool server's own input or result.

use std::collections::HashMap;
use std::sync::OnceLock;

use prost::Message as _;
use prost_types::FileDescriptorSet;
use prost_types::field_descriptor_proto::{Label, Type};
use serde_json::{Map, Value};

/// The closed sets of the status-like fields the interpreters branch on,
/// by field name, as each protocol crate spells them.
pub const VOCABULARY: &[(&str, &[&str])] = &[
    // Codex's tool, turn, plan step, review and server states; Claude's
    // task list and background task states; a tool server's.
    (
        "status",
        &[
            "inProgress",
            "in_progress",
            "completed",
            "failed",
            "declined",
            "interrupted",
            "pending",
            "approved",
            "denied",
            "timedOut",
            "aborted",
            "starting",
            "ready",
            "cancelled",
            "running",
            "killed",
            "stopped",
            "connected",
            "needs-auth",
            "disabled",
            "success",
            "error",
            "allowed",
            "allowed_warning",
            "rejected",
            "compacting",
            "deleted",
            "needsAuth",
            "notLoggedIn",
        ],
    ),
    (
        "stop_reason",
        &[
            "end_turn",
            "tool_use",
            "max_tokens",
            "stop_sequence",
            "pause_turn",
            "refusal",
            "model_context_window_exceeded",
        ],
    ),
    ("state", &["started", "stopped", "completed", "failed"]),
    ("limitId", &["codex"]),
    ("edit_mode", &["insert", "replace", "delete"]),
    ("persist", &["session", "always"]),
    ("codex_approval_kind", &["mcp_tool_call"]),
    (
        "task_type",
        &[
            "local_bash",
            "shell",
            "local_agent",
            "remote_agent",
            "subagent",
        ],
    ),
    ("kind", &["task-notification", "human", "peer"]),
    ("commandMode", &["prompt", "bash"]),
    ("promptSource", &["system", "user"]),
    ("checks", &["trustedAccessForCyber"]),
    (
        "codexErrorInfo",
        &[
            "unauthorized",
            "contextWindowExceeded",
            "usageLimitExceeded",
            "internalServerError",
            "badRequest",
            "sandboxError",
            "other",
        ],
    ),
    (
        "rateLimitType",
        &[
            "five_hour",
            "seven_day",
            "seven_day_opus",
            "seven_day_sonnet",
            "seven_day_overage_included",
        ],
    ),
    ("phase", &["commentary", "final_answer"]),
    (
        "permissionMode",
        &[
            "default",
            "acceptEdits",
            "bypassPermissions",
            "plan",
            "dontAsk",
            "auto",
        ],
    ),
    (
        "permission_mode",
        &[
            "default",
            "acceptEdits",
            "bypassPermissions",
            "plan",
            "dontAsk",
            "auto",
        ],
    ),
    (
        "mode",
        &[
            "url",
            "form",
            "default",
            "acceptEdits",
            "bypassPermissions",
            "plan",
            "dontAsk",
            "auto",
        ],
    ),
    (
        "effort",
        &[
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ],
    ),
    (
        "reasoningEffort",
        &[
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ],
    ),
    (
        "defaultReasoningEffort",
        &[
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ],
    ),
    (
        "reasoning_effort",
        &[
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ],
    ),
    ("trigger", &["manual", "auto"]),
    ("source", &["startup", "resume", "clear", "compact"]),
    (
        "terminal_reason",
        &[
            "aborted",
            "aborted_streaming",
            "aborted_tools",
            "completed",
            "max_turns",
        ],
    ),
    (
        "error",
        &[
            "authentication_failed",
            "oauth_org_not_allowed",
            "account_on_hold",
            "billing_error",
            "rate_limit",
            "overloaded",
            "invalid_request",
            "model_not_found",
            "server_error",
            "unknown",
            "max_output_tokens",
        ],
    ),
    ("action", &["accept", "decline", "cancel", "allow", "deny"]),
    (
        "decision",
        &[
            "accept",
            "acceptForSession",
            "decline",
            "cancel",
            "approve",
            "block",
            "allow",
            "deny",
            "ask",
        ],
    ),
    ("behavior", &["allow", "deny", "ask"]),
    (
        "apiKeySource",
        &[
            "ANTHROPIC_API_KEY",
            "apiKeyHelper",
            "/login managed key",
            "none",
            "user",
            "project",
            "org",
            "temporary",
            "oauth",
        ],
    ),
    (
        "destination",
        &[
            "session",
            "localSettings",
            "userSettings",
            "projectSettings",
            "cliArg",
        ],
    ),
    (
        "availableDecisions",
        &["accept", "acceptForSession", "decline", "cancel"],
    ),
    ("scope", &["turn", "session"]),
    (
        "sandbox",
        &["read-only", "workspace-write", "danger-full-access"],
    ),
    (
        "approvalPolicy",
        &["untrusted", "on-request", "on-failure", "never"],
    ),
    (
        "approvalsReviewer",
        &["user", "auto_review", "guardian_subagent"],
    ),
    (
        "planType",
        &[
            "free",
            "go",
            "plus",
            "pro",
            "prolite",
            "promax",
            "team",
            "business",
            "enterprise",
            "edu",
            "unknown",
        ],
    ),
    (
        "authMode",
        &[
            "apikey",
            "chatgpt",
            "chatgptAuthTokens",
            "headers",
            "agentIdentity",
            "personalAccessToken",
            "bedrockApiKey",
            "bedrockAccessKeys",
        ],
    ),
    (
        "tool",
        &[
            "spawnAgent",
            "sendInput",
            "resumeAgent",
            "wait",
            "closeAgent",
            "sendMessage",
            "followupTask",
            "interruptAgent",
            "listAgents",
            // amux's own tool server's tools.
            crate::SEND_TOOL,
            crate::STATUS_TOOL,
            wire::ATTACH_TOOL,
        ],
    ),
    ("role", &["user", "assistant", "system"]),
    (
        "media_type",
        &[
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/webp",
            "application/pdf",
            "text/plain",
        ],
    ),
    (
        "mimeType",
        &[
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/webp",
            "application/pdf",
            "text/plain",
        ],
    ),
    ("operation", &["enqueue", "dequeue", "remove", "popAll"]),
    // A content block's kind inside a tool's result.
    (
        "type",
        &[
            "text",
            "image",
            "document",
            "tool_reference",
            "search_result",
            "resource",
            "resource_link",
            "base64",
            "add",
            "delete",
            "update",
            "create",
        ],
    ),
];

/// The provider's fixed phrases a text can open with, which the
/// interpreters read: an interrupted or rejected call, a message from
/// another session, a pasted or local command's tags, a slash command, a
/// shell escape, a request the agent process named.
pub const MARKERS: &[&str] = &[
    "[Request interrupted by user",
    "The user doesn't want to proceed with this tool use.",
    "<cross-session-message",
    "Another Claude session sent a message:\n",
    "<pasted_content id=\"",
    "<local-command-caveat>",
    "<local-command-stdout>",
    "<local-command-stderr>",
    "<bash-input>",
    "<bash-stdout>",
    "<bash-stderr>",
    "<command-name>",
    "<task-notification>",
    "Automatic approval review approved (",
    "no rollout found",
    "agent-",
    "amux-",
    "user_note: ",
    "Implement the plan.",
    "Reconnecting... ",
    "/compact",
    "/clear",
    "/review",
    "/init",
    "/plan",
    "/",
    "!",
];

/// The frames of the texts the interpreters read inside, each opening tag
/// with its closing one.
const FRAMES: &[(&str, &str)] = &[
    ("<cross-session-message", "</cross-session-message>"),
    (
        "Another Claude session sent a message:\n",
        "\n\nThis came from another Claude session",
    ),
    (
        crate::claude_answers::REJECTED,
        " To tell you how to proceed, the user said:\n",
    ),
    (
        "The user doesn't want to proceed with this tool use.",
        "the user said:\n",
    ),
    ("<task-notification>", "</task-notification>"),
    ("<task-id>", "</task-id>"),
    ("<tool-use-id>", "</tool-use-id>"),
    ("<output-file>", "</output-file>"),
    ("<status>", "</status>"),
    ("<summary>", "</summary>"),
    ("<result>", "</result>"),
    ("<command-name>", "</command-name>"),
    ("<command-message>", "</command-message>"),
    ("<command-args>", "</command-args>"),
    ("<local-command-caveat>", "</local-command-caveat>"),
    ("<local-command-stdout>", "</local-command-stdout>"),
    ("<local-command-stderr>", "</local-command-stderr>"),
    ("<bash-input>", "</bash-input>"),
    ("<bash-stdout>", "</bash-stdout>"),
    ("<bash-stderr>", "</bash-stderr>"),
    ("<pasted_content id=\"", "</pasted_content id=\""),
];

/// The paths within each data subtree whose field names are kept, by the
/// subtree's path: the fields of the protocol's own objects a subtree holds
/// and, through `@input` and `@result`, a built-in tool's. A `*` is a
/// position whose key is the data's (a model, a file, a question's id),
/// itself aliased. A name anywhere else in data is aliased.
const KEPT: &[(&str, &[&str])] = &[
    // Claude: a tool's input and result, wherever a frame carries them.
    ("message.content[].input", &["@input"]),
    ("event.content_block.input", &["@input"]),
    ("request.input", &["@input"]),
    ("tool_input", &["@input"]),
    ("permission_denials[].tool_input", &["@input"]),
    ("wire_tool_inputs", &["*.@input"]),
    ("wireToolInputs", &["*.@input"]),
    ("tool_use_result", &["@result"]),
    ("toolUseResult", &["@result"]),
    ("tool_response", &["@result"]),
    // A tool result's content blocks.
    (
        "message.content[].content",
        &[
            "[].type",
            "[].text",
            "[].tool_name",
            "[].source",
            "[].source.type",
            "[].source.media_type",
            "[].source.data",
        ],
    ),
    // Per-model tallies, by model.
    (
        "modelUsage",
        &[
            "*.inputTokens",
            "*.outputTokens",
            "*.cacheReadInputTokens",
            "*.cacheCreationInputTokens",
            "*.cacheCreationEphemeral1hInputTokens",
            "*.cacheCreationEphemeral5mInputTokens",
            "*.thinkingTokens",
            "*.webSearchRequests",
            "*.costUSD",
            "*.costBasis",
            "*.contextWindow",
            "*.maxOutputTokens",
            "*.canonicalModel",
            "*.provider",
        ],
    ),
    (
        "model_usage_since_last_result",
        &[
            "*.inputTokens",
            "*.outputTokens",
            "*.cacheReadInputTokens",
            "*.cacheCreationInputTokens",
            "*.cacheCreationEphemeral1hInputTokens",
            "*.cacheCreationEphemeral5mInputTokens",
            "*.thinkingTokens",
            "*.webSearchRequests",
            "*.costUSD",
            "*.costBasis",
            "*.contextWindow",
            "*.maxOutputTokens",
            "*.canonicalModel",
            "*.provider",
        ],
    ),
    // A tool server's form: its fields are the server's own.
    ("request.requested_schema", FORM_SCHEMA),
    ("permission_suggestions", PERMISSION_UPDATES),
    ("request.permission_suggestions", PERMISSION_UPDATES),
    // What a control request answers, either way: initialize's catalogue,
    // a permission's verdict, a form's content (by the server's fields).
    (
        "response.response",
        &[
            "account",
            "account.apiKeySource",
            "account.apiProvider",
            "account.email",
            "account.organization",
            "account.subscriptionType",
            "account.tokenSource",
            "action",
            "agents",
            "agents[].description",
            "agents[].model",
            "agents[].name",
            "analytics_disabled",
            "applied",
            "applied.advisor",
            "applied.effort",
            "applied.model",
            "applied.ultracode",
            "applied.ultracodeAvailable",
            "applied.ultracodeRequested",
            "available_output_styles",
            "behavior",
            "capabilities",
            "claude_code_version",
            "commands",
            "commands[].aliases",
            "commands[].argumentHint",
            "commands[].builtin",
            "commands[].description",
            "commands[].name",
            "content",
            "current_permission_mode",
            "fast_mode_disabled_reason",
            "fast_mode_state",
            "feedback_mode",
            "feedback_mode.kind",
            "ide_rc_auto_enable_gate",
            "interrupt",
            "message",
            "mode",
            "model",
            "models",
            "models[].description",
            "models[].displayName",
            "models[].resolvedModel",
            "models[].supportedEffortLevels",
            "models[].supportsAdaptiveThinking",
            "models[].supportsAutoMode",
            "models[].supportsEffort",
            "models[].supportsFastMode",
            "models[].supportsThinkingOff",
            "models[].value",
            "output_style",
            "pid",
            "remote_control_auto_connect_default",
            "remote_control_auto_enable",
            "remote_control_auto_on_by_default",
            "remote_control_available",
            "session_state",
            "still_queued",
            "updatedInput",
            "updatedInput.@input",
            "updatedPermissions",
            "updatedPermissions[].type",
            "updatedPermissions[].mode",
            "updatedPermissions[].destination",
            "updatedPermissions[].directories",
            "updatedPermissions[].behavior",
            "updatedPermissions[].rules",
            "updatedPermissions[].rules[].toolName",
            "updatedPermissions[].rules[].ruleContent",
            "user_output_styles_dir",
        ],
    ),
    // Terminal Claude's attachment rows and file snapshots.
    (
        "attachment",
        &[
            "type",
            "commandMode",
            "date",
            "decision",
            "delivery_id",
            "exitCode",
            "hookEvent",
            "hookName",
            "humanTurn",
            "identity",
            "identity.knowledgeCutoff",
            "identity.marketingName",
            "identity.modelId",
            "isSubAgent",
            "origin",
            "origin.kind",
            "origin.producer",
            "planExists",
            "planFilePath",
            "processId",
            "prompt",
            "reminderType",
            "response",
            "runId",
            "scrubbed",
            "source_uuid",
            "stderr",
            "stdout",
            "text",
            "timestamp",
            "toolUseID",
        ],
    ),
    (
        "snapshot",
        &[
            "messageId",
            "timestamp",
            "trackedFileBackups",
            "trackedFileBackups.*.backupFileName",
            "trackedFileBackups.*.version",
            "trackedFileBackups.*.backupTime",
        ],
    ),
    // Codex: a tool server's call, its arguments and result.
    ("params.item.arguments", &["@input"]),
    ("params.item.result", &["@result"]),
    // What a command does, as Codex parsed it.
    ("params.item.commandActions", COMMAND_ACTIONS),
    ("params.commandActions", COMMAND_ACTIONS),
    // The files a change touches.
    (
        "params.item.changes",
        &[
            "[].path",
            "[].diff",
            "[].kind",
            "[].kind.type",
            "[].kind.move_path",
        ],
    ),
    // A message's input parts.
    (
        "params.item.content",
        &[
            "[].type",
            "[].text",
            "[].text_elements",
            "[].path",
            "[].detail",
            "[].url",
            "[].name",
        ],
    ),
    (
        "params.availableDecisions",
        &[
            "[].acceptWithExecpolicyAmendment",
            "[].acceptWithExecpolicyAmendment.execpolicy_amendment",
            "[].applyNetworkPolicyAmendment",
            "[].applyNetworkPolicyAmendment.network_policy_amendment",
            "[].applyNetworkPolicyAmendment.network_policy_amendment.action",
            "[].applyNetworkPolicyAmendment.network_policy_amendment.host",
        ],
    ),
    (
        "params.tokenUsage",
        &[
            "last",
            "last.cacheWriteInputTokens",
            "last.cachedInputTokens",
            "last.inputTokens",
            "last.outputTokens",
            "last.reasoningOutputTokens",
            "last.totalTokens",
            "modelContextWindow",
            "total",
            "total.cacheWriteInputTokens",
            "total.cachedInputTokens",
            "total.inputTokens",
            "total.outputTokens",
            "total.reasoningOutputTokens",
            "total.totalTokens",
        ],
    ),
    (
        "params.rateLimits",
        &[
            "credits",
            "credits.balance",
            "credits.hasCredits",
            "credits.unlimited",
            "individualLimit",
            "limitId",
            "limitName",
            "normalModelSlug",
            "planType",
            "primary",
            "primary.resetsAt",
            "primary.usedPercent",
            "primary.windowDurationMins",
            "rateLimitReachedType",
            "secondary",
            "secondary.resetsAt",
            "secondary.usedPercent",
            "secondary.windowDurationMins",
            "spendControlReached",
        ],
    ),
    ("params.requestedSchema", FORM_SCHEMA),
    ("params.permissions", PERMISSIONS),
    ("result.permissions", PERMISSIONS),
    (
        "params._meta",
        &[
            "codex_approval_kind",
            "persist",
            "tool_description",
            "tool_params",
            "tool_params_display",
            "tool_params_display[].display_name",
            "tool_params_display[].name",
            "tool_params_display[].value",
        ],
    ),
    // Answers, by the provider's question ids.
    ("result.answers", &["*.answers"]),
];

const FORM_SCHEMA: &[&str] = &["type", "properties", "properties.*.type", "required"];

/// The fields of the built-in tools' inputs (Claude's, and amux's own
/// tool server's) by path within the input.
pub(crate) const TOOL_INPUT: &[&str] = &[
    // Bash, and the shells it starts.
    "command",
    "description",
    "run_in_background",
    "timeout",
    "dangerouslyDisableSandbox",
    "bash_id",
    "shell_id",
    "task_id",
    "block",
    "filter",
    // Files.
    "file_path",
    "offset",
    "limit",
    "pages",
    "content",
    "old_string",
    "new_string",
    "replace_all",
    "edits",
    "edits[].old_string",
    "edits[].new_string",
    "edits[].replace_all",
    "notebook_path",
    "cell_id",
    "new_source",
    "cell_type",
    "edit_mode",
    "pattern",
    "path",
    "glob",
    "type",
    "output_mode",
    "head_limit",
    "multiline",
    // The web.
    "url",
    "prompt",
    "query",
    "allowed_domains",
    "blocked_domains",
    "max_results",
    // Subagents, skills and tasks.
    "subagent_type",
    "model",
    "name",
    "skill",
    "args",
    "todos",
    "todos[].content",
    "todos[].status",
    "todos[].activeForm",
    "subject",
    "activeForm",
    "taskId",
    "status",
    "owner",
    // Questions and plans.
    "questions",
    "questions[].question",
    "questions[].header",
    "questions[].multiSelect",
    "questions[].options",
    "questions[].options[].label",
    "questions[].options[].description",
    "questions[].options[].preview",
    "answers",
    "annotations",
    "annotations.*.notes",
    "plan",
    "planFilePath",
    // amux's own tools.
    "to",
    "text",
    "context",
    "kind",
    "cwd",
    "host",
    "working_on",
];

/// The fields of the built-in tools' results, by path within the result.
pub(crate) const TOOL_RESULT: &[&str] = &[
    "[].type",
    "[].text",
    "type",
    "content",
    "content[].type",
    "content[].text",
    "structuredContent",
    "isError",
    "stdout",
    "stderr",
    "interrupted",
    "isImage",
    "noOutputExpected",
    "backgroundTaskId",
    "outputFile",
    "filePath",
    "oldString",
    "newString",
    "originalFile",
    "replaceAll",
    "userModified",
    "structuredPatch",
    "structuredPatch[].oldStart",
    "structuredPatch[].oldLines",
    "structuredPatch[].newStart",
    "structuredPatch[].newLines",
    "structuredPatch[].lines",
    "file",
    "file.type",
    "file.filePath",
    "file.content",
    "file.base64",
    "file.numLines",
    "file.startLine",
    "file.totalLines",
    "file.originalSize",
    "file.dimensions",
    "file.dimensions.displayHeight",
    "file.dimensions.displayWidth",
    "file.dimensions.originalHeight",
    "file.dimensions.originalWidth",
    "matches",
    "query",
    "total_deferred_tools",
    "agentId",
    "isAgent",
    "isAsync",
    "canContinueAgent",
    "canReadOutputFile",
    "description",
    "prompt",
    "resolvedModel",
    "status",
    "statusChange",
    "statusChange.from",
    "statusChange.to",
    "success",
    "task",
    "task.id",
    "task.subject",
    "taskId",
    "updatedFields",
    "message",
    "plan",
    "questions",
    "questions[].question",
    "questions[].header",
    "questions[].multiSelect",
    "questions[].options",
    "questions[].options[].label",
    "questions[].options[].description",
    "questions[].options[].preview",
    "answers",
    "annotations",
    "annotations.*.notes",
];

const PERMISSION_UPDATES: &[&str] = &[
    "[].type",
    "[].behavior",
    "[].destination",
    "[].directories",
    "[].mode",
    "[].rules",
    "[].rules[].toolName",
    "[].rules[].ruleContent",
];

const COMMAND_ACTIONS: &[&str] = &["[].type", "[].command", "[].name", "[].path", "[].query"];

const PERMISSIONS: &[&str] = &[
    "fileSystem",
    "fileSystem.read",
    "fileSystem.write",
    "network",
    "network.enabled",
];

/// Whether `path` within data is one `kept` lists: written there, a field
/// of the protocol's (`Some(false)`), or within a built-in tool's input or
/// result a `<prefix>@input` or `<prefix>@result` entry places under
/// `prefix`, a field of the tool's (`Some(true)`). None for a name the
/// lists do not hold, which is aliased.
fn keeps(kept: &[&str], path: &str) -> Option<bool> {
    kept.iter().find_map(|entry| {
        if *entry == path {
            return Some(false);
        }
        [("@input", TOOL_INPUT), ("@result", TOOL_RESULT)]
            .iter()
            .find_map(|(tag, list)| {
                let rest = path.strip_prefix(entry.strip_suffix(tag)?)?;
                (!rest.is_empty() && keeps(list, rest).is_some()).then_some(true)
            })
    })
}

/// Whether a data subtree is a tool's own input or result.
fn tool_data(subtree: &str) -> bool {
    kept(subtree)
        .iter()
        .any(|entry| entry.ends_with("@input") || entry.ends_with("@result"))
}

fn kept(subtree: &str) -> &'static [&'static str] {
    KEPT.iter()
        .find(|(path, _)| *path == subtree)
        .map_or(&[], |(_, kept)| kept)
}

/// The protobuf fields that name a model, effort, permission or mode from
/// the provider's own catalogue, kept as a fact's model is.
const CHOSEN: &[(&str, &str)] = &[
    ("amux.v1.SetPermission", "value"),
    ("amux.v1.SetMode", "value"),
    ("amux.v1.SetModel", "model"),
    ("amux.v1.SetEffort", "effort"),
    ("amux.v1.EffectiveConfig", "model"),
    ("amux.v1.EffectiveConfig", "permission"),
    ("amux.v1.EffectiveConfig", "mode"),
    ("amux.v1.EffectiveConfig", "effort"),
    ("amux.v1.OfferedModel", "value"),
    ("amux.v1.OfferedModel", "efforts"),
    ("amux.v1.OfferedModel", "default_effort"),
    ("amux.v1.OfferedModel", "resolved_model"),
    ("amux.v1.OfferedPermission", "value"),
    ("amux.v1.OfferedPermission", "models"),
    ("amux.v1.OfferedMode", "value"),
];

/// Tool servers the providers and amux run themselves.
const OWN_SERVERS: &[&str] = &[crate::AMUX_TOOL_SERVER, "codex_apps"];

/// The fields that name a model, which a provider's own catalogue
/// chooses from and the record's settings already keep.
const MODEL: &[&str] = &["model", "modelId", "defaultModel"];

/// The fields a protocol tags its messages with.
const TAGS: &[&str] = &["type", "subtype", "method", "hook_event_name"];

/// One leaf of a skeleton, as [`leaf`] reads it back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Leaf<'a> {
    /// A value kept as it was.
    Kept(&'a str),
    /// A value kept as it was that may also stand elsewhere as an alias,
    /// with its alias's hash: a model, a mode, a status.
    Named { hash: &'a str, value: &'a str },
    /// A string's alias: the phrase it opens with, its hash, and the size
    /// and line breaks of the rest.
    Alias {
        prefix: &'a str,
        hash: &'a str,
        bytes: usize,
        lines: usize,
    },
    /// A number inside a tool's data, by how many characters it had.
    Number { characters: usize },
}

/// Reads a skeleton's string leaf; None for one no skeleton writes.
pub fn leaf(text: &str) -> Option<Leaf<'_>> {
    if let Some(kept) = text.strip_prefix('=') {
        return Some(Leaf::Kept(kept));
    }
    if let Some(named) = text.strip_prefix('%') {
        let (hash, value) = named.split_once('=')?;
        return Some(Leaf::Named { hash, value });
    }
    if let Some(characters) = text.strip_prefix('#') {
        return Some(Leaf::Number {
            characters: characters.parse().ok()?,
        });
    }
    let alias = text.strip_prefix('@')?;
    let (named, size) = alias.rsplit_once('~')?;
    let (prefix, hash) = named.rsplit_once('@').unwrap_or(("", named));
    let (bytes, lines) = size.split_once('/').unwrap_or((size, "0"));
    Some(Leaf::Alias {
        prefix,
        hash,
        bytes: bytes.parse().ok()?,
        lines: lines.parse().ok()?,
    })
}

/// Writes skeletons, aliasing each value by its hash salted with `salt`.
pub struct Skeleton {
    pub salt: [u8; 16],
}

/// Where in a frame a value sits.
#[derive(Clone, Copy)]
struct At {
    /// The frame's data subtrees, and its lists of the protocol's items, by
    /// path from its root.
    frame: Frame,
    /// Inside data: the subtree's path, and the paths within it whose keys
    /// are kept.
    data: Option<(&'static str, &'static [&'static str])>,
    /// Inside data that is no field of the protocol's: a tool's own input
    /// or result, or what a data-keyed map holds. Its numbers are sized
    /// like its strings, and no tag is read in it.
    tool: bool,
    /// Inside a tool server's own input or result.
    server: bool,
}

/// A frame's data subtrees and item lists, as its protocol crate names them.
#[derive(Clone, Copy)]
pub struct Frame {
    data: &'static [&'static str],
    items: &'static [&'static str],
}

impl Frame {
    /// A provider's frames, or what amux writes to it.
    pub fn of(kind: wire::Kind) -> Self {
        match kind {
            wire::Kind::ClaudeSdk | wire::Kind::ClaudePty => Frame {
                data: claude_protocol::DATA_SUBTREES,
                items: &[],
            },
            wire::Kind::Codex => Frame {
                data: codex_protocol::DATA_SUBTREES,
                items: codex_protocol::ITEM_LISTS,
            },
            _ => Frame {
                data: &[],
                items: &[],
            },
        }
    }
}

impl At {
    fn root(frame: Frame) -> Self {
        At {
            frame,
            data: None,
            tool: false,
            server: false,
        }
    }

    /// Inside JSON that is all data, whose every key is aliased.
    fn data(frame: Frame) -> Self {
        At {
            frame,
            data: Some(("", &[])),
            tool: true,
            server: false,
        }
    }
}

/// `key` under `path`, as a subtree's path is written.
fn child(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

impl Skeleton {
    /// A value's alias: a salted hash, so equal values alias alike within
    /// one record and nothing outside it can look one up.
    pub fn hash(&self, bytes: &[u8]) -> String {
        use sha2::Digest as _;
        let digest = sha2::Sha256::new()
            .chain_update(self.salt)
            .chain_update(bytes)
            .finalize();
        crate::to_hex(&digest[..6])
    }

    /// A string's alias, keeping `prefix`.
    pub fn alias(&self, prefix: &str, text: &str) -> String {
        let rest = &text[prefix.len()..];
        let hash = self.hash(rest.as_bytes());
        let lines = rest.matches('\n').count();
        let named = if prefix.is_empty() {
            hash
        } else {
            format!("{prefix}@{hash}")
        };
        let mut alias = format!("@{named}~{}", rest.len());
        if lines > 0 {
            alias.push_str(&format!("/{lines}"));
        }
        alias
    }

    /// A value kept with its alias's hash, `%<hash>=<value>`, so where the
    /// same value is aliased it stands for this one.
    pub fn named(&self, value: &str) -> String {
        format!("%{}={value}", self.hash(value.as_bytes()))
    }

    /// A text's alias, keeping a fixed phrase it opens with.
    pub fn text(&self, text: &str) -> String {
        if text.is_empty() {
            return "=".into();
        }
        let marker = MARKERS
            .iter()
            .filter(|marker| text.starts_with(*marker))
            .max_by_key(|marker| marker.len())
            .copied()
            .unwrap_or_default();
        // What follows the phrase is anyone's, digits as much as words,
        // but for the count Codex's interpreter reads off its reconnects
        // (`Reconnecting... 2/5`).
        if marker == "Reconnecting... " && count(&text[marker.len()..]) {
            return self.named(text);
        }
        self.alias(marker, text)
    }

    /// A provider fact's skeleton, or one of what amux writes to its
    /// provider; a payload that is not JSON is one text.
    pub fn fact(&self, kind: wire::Kind, payload: &[u8]) -> Value {
        match serde_json::from_slice::<Value>(payload) {
            Ok(value) => self.json(&value, At::root(Frame::of(kind)), "", "", None),
            Err(_) => Value::String(self.text(&String::from_utf8_lossy(payload))),
        }
    }

    /// `value` at `path` under the field `key` of an object whose `type`
    /// is `kind`. Outside data a field name is the protocol's and kept, as
    /// a new field must show by name; inside data only the names at the
    /// paths `KEPT` lists for that subtree are, and every other is aliased
    /// like a value, its path written `*`.
    fn json(&self, value: &Value, at: At, path: &str, key: &str, kind: Option<&str>) -> Value {
        match value {
            Value::Null | Value::Bool(_) => value.clone(),
            Value::Number(number) if at.tool => {
                Value::String(format!("#{}", number.to_string().len()))
            }
            Value::Number(_) => value.clone(),
            Value::String(text) => self
                .embedded(text, at, key, kind)
                .or_else(|| (!at.server).then(|| self.framed(text)).flatten())
                .unwrap_or_else(|| Value::String(self.string(text, at, key, kind))),
            Value::Array(items) => {
                let element = if at.data.is_none()
                    && (at.frame.items.contains(&path)
                        || at
                            .frame
                            .items
                            .iter()
                            .any(|list| path.strip_prefix(list) == Some("[].items")))
                    && path.ends_with("items")
                {
                    "params.item".to_owned()
                } else {
                    format!("{path}[]")
                };
                Value::Array(
                    items
                        .iter()
                        .map(|item| self.json(item, at, &element, key, kind))
                        .collect(),
                )
            }
            Value::Object(fields) => {
                let kind = fields.get("type").and_then(Value::as_str);
                let server = server_tool(fields, kind);
                let mut out = Map::new();
                for (name, value) in fields {
                    let entry = match at.data {
                        Some((_, kept)) => {
                            let named = child(path, name);
                            let keep = if at.server { None } else { keeps(kept, &named) };
                            match keep {
                                Some(tool) => {
                                    (name.clone(), (At { tool, ..at }, named), name.as_str())
                                }
                                None => (
                                    self.alias("", name),
                                    (At { tool: true, ..at }, child(path, "*")),
                                    "",
                                ),
                            }
                        }
                        None => {
                            let path = child(path, name);
                            let name_out = if schema_name(name) {
                                name.clone()
                            } else {
                                self.alias("", name)
                            };
                            let entered = at.frame.data.iter().find(|subtree| **subtree == path);
                            let inner = match entered {
                                Some(subtree) => (
                                    At {
                                        data: Some((subtree, kept(subtree))),
                                        server: at.server || (server && tool_data(subtree)),
                                        ..at
                                    },
                                    String::new(),
                                ),
                                None => (at, path),
                            };
                            (name_out, inner, name.as_str())
                        }
                    };
                    let (name_out, (inner, path), key) = entry;
                    out.insert(name_out, self.json(value, inner, &path, key, kind));
                }
                Value::Object(out)
            }
        }
    }

    fn string(&self, text: &str, at: At, key: &str, kind: Option<&str>) -> String {
        if text.is_empty() {
            return "=".into();
        }
        if at.server {
            return self.alias("", text);
        }
        if VOCABULARY
            .iter()
            .any(|(field, values)| *field == key && values.contains(&text))
        {
            return self.named(text);
        }
        if !at.tool {
            if TAGS.contains(&key) && text.split('/').all(schema_name) {
                return format!("={text}");
            }
            if is_tool_name(key, kind) {
                return self.tool_name(text);
            }
            if timestamp(text) || (key.ends_with("version") && version(text)) {
                return format!("={text}");
            }
            if (MODEL.contains(&key) && (schema_name(text) || text == "<synthetic>"))
                || OWN_SERVERS.contains(&text)
            {
                return self.named(text);
            }
            if key == "userAgent"
                && let Some((agent, _)) = text.split_once(' ')
                && agent
                    .split('/')
                    .all(|part| version(part) || schema_name(part))
            {
                return self.alias(&text[..=agent.len()], text);
            }
        }
        self.text(text)
    }

    /// A text in one of the frames the interpreters read inside (a message
    /// from another session, a background task's notification, a local
    /// command's output): its frame's tags kept, each piece between them
    /// aliased apart from the whitespace around it, so a message's body
    /// aliases as the message it carries. None for any other text.
    fn framed(&self, text: &str) -> Option<Value> {
        if let Some(rest) = text
            .strip_prefix("https://")
            .or_else(|| text.strip_prefix("http://"))
        {
            return Some(self.link(&text[..text.len() - rest.len()], rest));
        }
        if let Some(wrapped) = self.shell(text) {
            return Some(wrapped);
        }
        if !FRAMES.iter().any(|(open, _)| text.contains(open)) {
            return None;
        }
        let mut pieces = Vec::new();
        let mut rest = text;
        let mut after = "";
        while !rest.is_empty() {
            let next = FRAMES
                .iter()
                .flat_map(|(open, close)| [*open, *close])
                .filter_map(|tag| rest.find(tag).map(|at| (at, tag)))
                .min_by_key(|(at, tag)| (*at, std::cmp::Reverse(tag.len())));
            let (at, tag) = next.unwrap_or((rest.len(), ""));
            self.piece(&rest[..at], after, &mut pieces);
            if tag.is_empty() {
                break;
            }
            pieces.push(Value::String(format!("={tag}")));
            rest = &rest[at + tag.len()..];
            after = tag;
            // An opening tag with attributes runs to its `>`.
            if !tag.ends_with('>')
                && tag.starts_with('<')
                && let Some(end) = rest.find('>')
            {
                self.piece(&rest[..end], "", &mut pieces);
                pieces.push(Value::String("=>".into()));
                rest = &rest[end + 1..];
            }
        }
        Some(serde_json::json!({ "=text": pieces }))
    }

    /// A command as Codex reports it, wrapped in the user's shell
    /// (`/bin/zsh -lc '…'`): the shell's name and flag kept, never the
    /// folder it was found in (which names the person's home), and the
    /// script inside aliased, so the script aliases as the command the
    /// interpreter shows.
    fn shell(&self, text: &str) -> Option<Value> {
        let words = shell_words::split(text).ok()?;
        let [shell, flag, script] = words.as_slice() else {
            return None;
        };
        let name = shell.rsplit('/').next().unwrap_or_default();
        let known = matches!(flag.as_str(), "-lc" | "-c")
            && matches!(name, "bash" | "zsh" | "sh" | "dash" | "ksh" | "fish")
            && !shell.contains(char::is_whitespace);
        // Quoted as Codex quoted it, unless it needed none.
        let quote = if text == format!("{shell} {flag} {script}") {
            ""
        } else {
            "'"
        };
        known.then(|| {
            serde_json::json!({ "=text": [
                format!("={name} {flag} {quote}"),
                self.text(script),
                format!("={quote}"),
            ]})
        })
    }

    /// A link: its scheme and punctuation kept, each name between them
    /// aliased, so an id a link carries aliases as the id does.
    fn link(&self, scheme: &str, rest: &str) -> Value {
        let mut pieces = vec![Value::String(format!("={scheme}"))];
        let mut start = 0;
        for (at, c) in rest.char_indices() {
            if "/?&=#:".contains(c) {
                if at > start {
                    pieces.push(Value::String(self.alias("", &rest[start..at])));
                }
                pieces.push(Value::String(format!("={c}")));
                start = at + 1;
            }
        }
        if start < rest.len() {
            pieces.push(Value::String(self.alias("", &rest[start..])));
        }
        serde_json::json!({ "=text": pieces })
    }

    /// One piece of a framed text: its whitespace kept, the rest aliased,
    /// or kept when it is a status after `<status>`.
    fn piece(&self, piece: &str, after: &str, pieces: &mut Vec<Value>) {
        if piece.is_empty() {
            return;
        }
        let body = piece.trim();
        let start = piece.len() - piece.trim_start().len();
        if start > 0 {
            pieces.push(Value::String(format!("={}", &piece[..start])));
        }
        if !body.is_empty() {
            let status = VOCABULARY
                .iter()
                .any(|(field, values)| *field == "status" && values.contains(&body));
            pieces.push(Value::String(if after == "<status>" && status {
                self.named(body)
            } else {
                self.text(body)
            }));
        }
        let end = start + body.len();
        if end < piece.len() && !body.is_empty() {
            pieces.push(Value::String(format!("={}", &piece[end..])));
        }
    }

    /// A string that holds JSON, as amux's own tool server answers and
    /// some providers write a call's arguments: its skeleton, beside the
    /// text's size. Any other string is None.
    /// JSON written into a string: the data's, never the protocol's.
    fn embedded(&self, text: &str, at: At, key: &str, kind: Option<&str>) -> Option<Value> {
        if !text.starts_with(['{', '[']) {
            return None;
        }
        let json = serde_json::from_str::<Value>(text).ok()?;
        let inner = At {
            data: Some(("", &[])),
            tool: true,
            ..at
        };
        Some(serde_json::json!({
            "=json": self.json(&json, inner, "", key, kind),
            "bytes": text.len(),
        }))
    }

    /// A tool's name as Claude writes it: a built-in tool's, or amux's own
    /// server's, kept; a tool server's aliased behind its `mcp__`.
    fn tool_name(&self, name: &str) -> String {
        let (server, _) = crate::claude_common::split_tool_name(name);
        if (server.is_empty() || server == crate::AMUX_TOOL_SERVER) && schema_name(name) {
            format!("={name}")
        } else if name.starts_with("mcp__") {
            self.alias("mcp__", name)
        } else {
            self.alias("", name)
        }
    }

    /// A protobuf message's skeleton, `message` its full name
    /// (`amux.v1.Envelope`), read by the wire schema. A field this build
    /// does not know is left out.
    pub fn proto(&self, message: &str, bytes: &[u8]) -> Value {
        let Some(fields) = schema().get(message) else {
            return Value::Null;
        };
        let mut out = Map::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            let Ok(key) = prost::encoding::decode_varint(&mut rest) else {
                break;
            };
            let (number, wire) = ((key >> 3) as i32, key & 7);
            let value = match wire {
                0 => prost::encoding::decode_varint(&mut rest)
                    .ok()
                    .map(Raw::Varint),
                1 if rest.len() >= 8 => {
                    let (head, tail) = rest.split_at(8);
                    rest = tail;
                    Some(Raw::Fixed(u64::from_le_bytes(
                        head.try_into().unwrap_or_default(),
                    )))
                }
                5 if rest.len() >= 4 => {
                    let (head, tail) = rest.split_at(4);
                    rest = tail;
                    Some(Raw::Fixed(u64::from(u32::from_le_bytes(
                        head.try_into().unwrap_or_default(),
                    ))))
                }
                2 => prost::encoding::decode_varint(&mut rest)
                    .ok()
                    .and_then(|len| usize::try_from(len).ok())
                    .filter(|len| *len <= rest.len())
                    .map(|len| {
                        let (head, tail) = rest.split_at(len);
                        rest = tail;
                        Raw::Bytes(head)
                    }),
                _ => None,
            };
            let Some(value) = value else {
                break;
            };
            let Some(field) = fields.get(&number) else {
                continue;
            };
            let values = self.proto_field(message, field, value);
            if field.repeated {
                let list = out
                    .entry(field.name.clone())
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Value::Array(list) = list {
                    list.extend(values);
                }
            } else if let Some(value) = values.into_iter().next() {
                out.insert(field.name.clone(), value);
            }
        }
        Value::Object(out)
    }

    fn proto_field(&self, message: &str, field: &Field, value: Raw<'_>) -> Vec<Value> {
        match (field.ty, value) {
            (Type::Bool, Raw::Varint(value)) => vec![Value::Bool(value != 0)],
            (_, Raw::Varint(value) | Raw::Fixed(value)) => vec![Value::from(value)],
            (Type::Message, Raw::Bytes(bytes)) => vec![self.proto(&field.type_name, bytes)],
            (Type::String, Raw::Bytes(bytes)) => {
                let text = String::from_utf8_lossy(bytes);
                vec![Value::String(self.proto_string(
                    message,
                    &field.name,
                    &text,
                ))]
            }
            (Type::Bytes, Raw::Bytes(bytes)) if field.name.ends_with("_json") => {
                let json = serde_json::from_slice::<Value>(bytes)
                    .map(|json| {
                        self.json(
                            &json,
                            At::data(Frame::of(wire::Kind::Unspecified)),
                            "",
                            "",
                            None,
                        )
                    })
                    .unwrap_or(Value::Null);
                vec![serde_json::json!({ "=json": json, "bytes": bytes.len() })]
            }
            (Type::Bytes, Raw::Bytes(bytes)) => {
                vec![Value::String(format!(
                    "@{}~{}",
                    self.hash(bytes),
                    bytes.len()
                ))]
            }
            // A packed run of scalars.
            (ty, Raw::Bytes(mut bytes)) => {
                let mut values = Vec::new();
                while !bytes.is_empty() {
                    let value = match ty {
                        Type::Fixed64 | Type::Sfixed64 | Type::Double if bytes.len() >= 8 => {
                            let (head, tail) = bytes.split_at(8);
                            bytes = tail;
                            u64::from_le_bytes(head.try_into().unwrap_or_default())
                        }
                        Type::Fixed32 | Type::Sfixed32 | Type::Float if bytes.len() >= 4 => {
                            let (head, tail) = bytes.split_at(4);
                            bytes = tail;
                            u64::from(u32::from_le_bytes(head.try_into().unwrap_or_default()))
                        }
                        _ => match prost::encoding::decode_varint(&mut bytes) {
                            Ok(value) => value,
                            Err(_) => break,
                        },
                    };
                    values.push(if ty == Type::Bool {
                        Value::Bool(value != 0)
                    } else {
                        Value::from(value)
                    });
                }
                values
            }
        }
    }

    /// A protobuf string: a launch flag and the vocabulary a few flags
    /// take, a model or mode chosen from the provider's own, an ask or
    /// item key with its kind's prefix, else an alias.
    fn proto_string(&self, message: &str, field: &str, text: &str) -> String {
        if text.is_empty() {
            return "=".into();
        }
        if CHOSEN
            .iter()
            .any(|(of, named)| *of == message && *named == field)
            && schema_name(text)
        {
            return self.named(text);
        }
        if field.ends_with("key") {
            return match key_prefix(text) {
                Some(prefix) => self.alias(prefix, text),
                None => self.alias("", text),
            };
        }
        self.text(text)
    }

    /// The launch arguments: each flag kept, and the value of a flag the
    /// interpreters read when it is the provider's own fixed vocabulary
    /// (a permission mode, an effort; never a model, which is named), given
    /// after an `=` or as the next argument.
    pub fn launch_args(&self, args: &[String]) -> Value {
        let mut previous = "";
        let mut out = Vec::new();
        for arg in args {
            let (name, value) = arg.split_once('=').unwrap_or((arg, ""));
            let flag = name.starts_with('-') && schema_name(name.trim_start_matches('-'));
            let kept = if flag {
                value.is_empty() || launch_value(name, value)
            } else {
                launch_value(previous, arg)
            };
            out.push(Value::String(if kept {
                format!("={arg}")
            } else if flag {
                self.alias(&arg[..=name.len()], arg)
            } else {
                self.alias("", arg)
            }));
            previous = if flag && value.is_empty() { arg } else { "" };
        }
        Value::Array(out)
    }
}

/// Whether `value` is one the launch flag `flag` takes from the
/// provider's own vocabulary.
fn launch_value(flag: &str, value: &str) -> bool {
    let permission = [
        "default",
        "acceptEdits",
        "bypassPermissions",
        "plan",
        "dontAsk",
        "auto",
    ];
    let effort = ["low", "medium", "high", "xhigh", "max"];
    match flag {
        "--permission-mode" => permission.contains(&value),
        "--effort" => effort.contains(&value),
        _ => false,
    }
}

/// An interpreter's own key prefix (`ask:`, `turn:`), as the record's
/// aliases keep it.
fn key_prefix(key: &str) -> Option<&str> {
    let (prefix, _) = key.split_once(':')?;
    ((1..=12).contains(&prefix.len())
        && prefix.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
    .then(|| &key[..=prefix.len()])
}

/// Whether the field `key` of an object of type `kind` names a tool.
fn is_tool_name(key: &str, kind: Option<&str>) -> bool {
    key == "tool_name"
        || key == "tools"
        || (key == "name"
            && matches!(
                kind,
                Some("tool_use" | "server_tool_use" | "mcp_tool_use" | "tool_reference")
            ))
}

/// Whether an object is a call of a tool server's tool, whose input and
/// result hold the server's own names.
fn server_tool(fields: &Map<String, Value>, kind: Option<&str>) -> bool {
    if kind == Some("mcpToolCall") {
        return fields
            .get("server")
            .and_then(Value::as_str)
            .is_none_or(|server| server != crate::AMUX_TOOL_SERVER);
    }
    ["name", "tool_name"].iter().any(|field| {
        fields
            .get(*field)
            .and_then(Value::as_str)
            .is_some_and(|name| {
                let (server, _) = crate::claude_common::split_tool_name(name);
                !server.is_empty() && server != crate::AMUX_TOOL_SERVER
            })
    })
}

/// The full name of a kind's input message.
pub fn input_message(kind: wire::Kind) -> &'static str {
    match kind {
        wire::Kind::ClaudePty => "amux.v1.ClaudePtyInput",
        wire::Kind::Codex => "amux.v1.CodexInput",
        wire::Kind::ClaudeSdk | wire::Kind::Unspecified => "amux.v1.ClaudeSdkInput",
    }
}

/// A name a schema would choose: short, and made of identifier characters.
pub(crate) fn schema_name(name: &str) -> bool {
    (1..=48).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
}

/// An attempt and its limit, `<n>/<m>`, of at most three digits each.
fn count(text: &str) -> bool {
    text.split_once('/').is_some_and(|(attempt, limit)| {
        [attempt, limit]
            .iter()
            .all(|part| (1..=3).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit()))
    })
}

fn timestamp(text: &str) -> bool {
    text.len() <= 40 && chrono::DateTime::parse_from_rfc3339(text).is_ok()
}

/// A version as a provider prints one: digits and dots, with a short tag.
fn version(text: &str) -> bool {
    text.len() <= 32
        && text.starts_with(|c: char| c.is_ascii_digit())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}

/// A protobuf field's value as the wire carries it.
enum Raw<'a> {
    Varint(u64),
    Fixed(u64),
    Bytes(&'a [u8]),
}

/// A field of the wire schema.
pub struct Field {
    pub number: i32,
    pub name: String,
    pub ty: Type,
    /// The full name of its message type, without the leading dot.
    pub type_name: String,
    pub repeated: bool,
}

/// Every message of the wire schema by its full name (`amux.v1.Input`),
/// with its fields by number.
pub fn schema() -> &'static HashMap<String, HashMap<i32, Field>> {
    static SCHEMA: OnceLock<HashMap<String, HashMap<i32, Field>>> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let set = FileDescriptorSet::decode(wire::DESCRIPTOR_SET).expect("descriptor set decodes");
        let mut schema = HashMap::new();
        for file in &set.file {
            for message in &file.message_type {
                add(&mut schema, file.package(), message);
            }
        }
        schema
    })
}

fn add(
    schema: &mut HashMap<String, HashMap<i32, Field>>,
    scope: &str,
    message: &prost_types::DescriptorProto,
) {
    let name = format!("{scope}.{}", message.name());
    let fields = message
        .field
        .iter()
        .map(|field| {
            (
                field.number(),
                Field {
                    number: field.number(),
                    name: field.name().to_owned(),
                    ty: field.r#type(),
                    type_name: field.type_name().trim_start_matches('.').to_owned(),
                    repeated: field.label() == Label::Repeated,
                },
            )
        })
        .collect();
    for nested in &message.nested_type {
        add(schema, &name, nested);
    }
    schema.insert(name, fields);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const SKELETON: Skeleton = Skeleton { salt: [0; 16] };

    /// The alias the skeleton writes for `text`.
    fn h(text: &str, bytes: usize) -> String {
        format!("@{}~{bytes}", SKELETON.hash(text.as_bytes()))
    }

    #[test]
    fn a_fact_keeps_its_fields_tags_and_statuses_and_no_words() {
        let fact = json!({
            "method": "item/completed",
            "params": {"item": {"type": "commandExecution", "id": "call_1",
                "command": "cat secret.txt", "status": "completed", "exitCode": 0,
                "aggregatedOutput": "secret\nwords\n"}},
        });
        let out = SKELETON.fact(wire::Kind::ClaudeSdk, &serde_json::to_vec(&fact).unwrap());
        let output = format!("{}/2", h("secret\nwords\n", 13));
        assert_eq!(
            out,
            json!({
                "method": "=item/completed",
                "params": {"item": {"type": "=commandExecution", "id": h("call_1", 6),
                    "command": h("cat secret.txt", 14), "status": SKELETON.named("completed"),
                    "exitCode": 0,
                    "aggregatedOutput": output}},
            })
        );
        let hash = SKELETON.hash(b"secret\nwords\n");
        assert_eq!(
            leaf(&output),
            Some(Leaf::Alias {
                prefix: "",
                hash: &hash,
                bytes: 13,
                lines: 2
            })
        );
    }

    #[test]
    fn a_tool_servers_input_keeps_no_names_and_a_tools_numbers_only_their_size() {
        let fact = json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "t", "name": "mcp__acme__lookup",
             "input": {"customer": "secret", "limit": 25}},
            {"type": "tool_use", "id": "u", "name": "Read",
             "input": {"file_path": "/x", "offset": 120}},
        ]}});
        let out = SKELETON.fact(wire::Kind::ClaudeSdk, &serde_json::to_vec(&fact).unwrap());
        let blocks = &out["message"]["content"];
        assert_eq!(
            blocks[0]["name"],
            format!("@mcp__@{}~12", SKELETON.hash(b"acme__lookup"))
        );
        let mut input = Map::new();
        input.insert(h("customer", 8), h("secret", 6).into());
        input.insert(h("limit", 5), "#2".into());
        assert_eq!(blocks[0]["input"], Value::Object(input));
        assert_eq!(blocks[1]["name"], "=Read");
        assert_eq!(
            blocks[1]["input"],
            json!({"file_path": format!("@/@{}~1", SKELETON.hash(b"x")), "offset": "#3"})
        );
        let text = "[Request interrupted by user for tool use]";
        assert_eq!(
            SKELETON.text(text),
            format!(
                "@[Request interrupted by user@{}~14",
                SKELETON.hash(b" for tool use]")
            )
        );
    }
}
