//! Defaults for fields Claude always writes.
//!
//! amux's own test inputs are written by hand and leave out fields a real
//! line always carries. Production reads such a line as it always has,
//! with the field at its default (empty, zero, or none for a field amux
//! treats as optional); a strict decode refuses it, so a recording that
//! drops one of these fields still reads as drift.

macro_rules! absent {
    ($($name:ident => $wire:literal,)*) => {
        $(
            pub(crate) fn $name<T: Default>() -> T {
                crate::strictness::unknown(|| concat!("missing field `", $wire, "`").to_owned());
                T::default()
            }
        )*
    };
}

absent! {
    api_key_source => "apiKeySource",
    claude_code_version => "claude_code_version",
    content => "content",
    cwd => "cwd",
    description => "description",
    errors => "errors",
    is_error => "is_error",
    mcp_servers => "mcp_servers",
    message_type => "type",
    model => "model",
    model_usage => "modelUsage",
    parent_tool_use_id => "parent_tool_use_id",
    permission_mode => "permissionMode",
    plugins => "plugins",
    result => "result",
    role => "role",
    session_id => "session_id",
    skills => "skills",
    stop_reason => "stop_reason",
    task_id => "task_id",
    task_type => "task_type",
    tasks => "tasks",
    tools => "tools",
    total_cost_usd => "total_cost_usd",
    usage => "usage",
    uuid => "uuid",
    compact_metadata => "compactMetadata",
    duration_ms => "durationMs",
    id => "id",
    is_meta => "isMeta",
    level => "level",
    message_count => "messageCount",
    parent_uuid => "parentUuid",
    pre_tokens => "preTokens",
    prompt => "prompt",
    prompt_id => "promptId",
    row_session_id => "sessionId",
    status => "status",
    timestamp => "timestamp",
    transcript_path => "transcript_path",
    trigger => "trigger",
    version => "version",
}
