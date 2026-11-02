//! An event's skeleton: every field it has, with each value replaced by
//! what it is, so the agent's record can stand for the event without
//! holding anything that was said.
//!
//! A provider's fact is JSON and keeps every field name; an input, an
//! agent message and the agent's other protobuf events keep theirs by the
//! wire schema's descriptor. Of the values, a skeleton keeps only those
//! that are not content:
//!
//! - the provider's fixed vocabulary: a status-like value its field's
//!   closed set in [`VOCABULARY`] names, and a protocol's own tags (a
//!   `type`, `subtype`, `method` or hook event name, as the record's
//!   variant already keeps them);
//! - bools and nulls, and numbers outside a tool's input and result;
//! - a timestamp, and a built-in tool's name.
//!
//! Every other string becomes an alias of its value with its size,
//! `@<hash>~<bytes>`, and `/<lines>` when it has line breaks, so equal
//! strings alias alike within one record and the size of what was said
//! survives without the words. A provider's fixed phrase that opens a text
//! ([`MARKERS`]) is kept ahead of the alias, `@<phrase>@<hash>~<bytes>`, as
//! is the `mcp__` that names a tool server's tool. A number inside a
//! tool's input or result becomes `#<characters>`. A kept string is
//! written `=<value>`; a model, which is not a closed set but is not
//! content either, `%<hash>=<value>`, so a value that appears elsewhere
//! aliased stays recognisably the same one.
//!
//! A string that is JSON becomes `{"=json": <skeleton>, "bytes": <n>}`. A
//! text the provider frames with its own fixed phrases or tags
//! (`FRAMES`), a link, and a shell command become `{"=text": [...]}`,
//! the kept frames and separators between the aliases of what they hold.
//! A field name that does not read as a schema's is aliased like a value,
//! and every field name inside a tool server's own input or result.

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

/// Fields under which a tool's own input or result sits: their contents are
/// data, so their numbers are sized like their strings.
const DATA: &[&str] = &[
    "input",
    "tool_input",
    "tool_response",
    "tool_use_result",
    "toolUseResult",
    "arguments",
];

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

/// Where in a fact a value sits.
#[derive(Clone, Copy, Default)]
struct At {
    /// Inside a tool's input or result.
    data: bool,
    /// Inside a tool server's own input or result.
    server: bool,
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
        // A fixed phrase and a count (`Reconnecting... 2/5`) says no word
        // of anyone's.
        if marker.len() > 1 && !text[marker.len()..].contains(char::is_alphabetic) {
            return self.named(text);
        }
        self.alias(marker, text)
    }

    /// A provider fact's skeleton; a payload that is not JSON is one text.
    pub fn fact(&self, payload: &[u8]) -> Value {
        match serde_json::from_slice::<Value>(payload) {
            Ok(value) => self.json(&value, At::default(), "", None),
            Err(_) => Value::String(self.text(&String::from_utf8_lossy(payload))),
        }
    }

    /// A JSON value's skeleton, outside any tool's data.
    pub fn value(&self, value: &Value) -> Value {
        self.json(value, At::default(), "", None)
    }

    /// `value` under the field `key` of an object whose `type` is `kind`.
    fn json(&self, value: &Value, at: At, key: &str, kind: Option<&str>) -> Value {
        match value {
            Value::Null | Value::Bool(_) => value.clone(),
            Value::Number(number) if at.data => {
                Value::String(format!("#{}", number.to_string().len()))
            }
            Value::Number(_) => value.clone(),
            Value::String(text) => self
                .embedded(text, at, key, kind)
                .or_else(|| (!at.server).then(|| self.framed(text)).flatten())
                .unwrap_or_else(|| Value::String(self.string(text, at, key, kind))),
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|item| self.json(item, at, key, kind))
                    .collect(),
            ),
            Value::Object(fields) => {
                let kind = fields.get("type").and_then(Value::as_str);
                let server = server_tool(fields, kind);
                let mut out = Map::new();
                for (name, value) in fields {
                    let inner = At {
                        data: at.data || DATA.contains(&name.as_str()) || tool_result(name, kind),
                        server: at.server || (server && DATA.contains(&name.as_str())),
                    };
                    let name_out = if at.server || !schema_name(name) {
                        self.alias("", name)
                    } else {
                        name.clone()
                    };
                    out.insert(name_out, self.json(value, inner, name, kind));
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
        if !at.data {
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
    /// (`/bin/zsh -lc '…'`): the shell kept and the script inside aliased,
    /// so the script aliases as the command the interpreter shows.
    fn shell(&self, text: &str) -> Option<Value> {
        let words = shell_words::split(text).ok()?;
        let [shell, flag, script] = words.as_slice() else {
            return None;
        };
        let known = matches!(flag.as_str(), "-lc" | "-c")
            && matches!(
                shell.rsplit('/').next(),
                Some("bash" | "zsh" | "sh" | "dash" | "ksh" | "fish")
            )
            && !shell.contains(char::is_whitespace);
        // Quoted as Codex quoted it, unless it needed none.
        let quote = if text == format!("{shell} {flag} {script}") {
            ""
        } else {
            "'"
        };
        known.then(|| {
            serde_json::json!({ "=text": [
                format!("={shell} {flag} {quote}"),
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
    fn embedded(&self, text: &str, at: At, key: &str, kind: Option<&str>) -> Option<Value> {
        if !text.starts_with(['{', '[']) {
            return None;
        }
        let json = serde_json::from_str::<Value>(text).ok()?;
        Some(serde_json::json!({
            "=json": self.json(&json, at, key, kind),
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
                    .map(|json| self.value(&json))
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

/// Whether the field `name` of an object of type `kind` holds a tool's
/// result.
fn tool_result(name: &str, kind: Option<&str>) -> bool {
    (name == "content" && kind.is_some_and(|kind| kind.ends_with("tool_result")))
        || (name == "result" && kind == Some("mcpToolCall"))
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
        let out = SKELETON.fact(&serde_json::to_vec(&fact).unwrap());
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
        let out = SKELETON.fact(&serde_json::to_vec(&fact).unwrap());
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
