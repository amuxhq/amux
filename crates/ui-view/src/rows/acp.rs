//! An ACP agent's rows. One envelope serves every ACP agent, so nothing here
//! knows which agent wrote an item: a call is drawn from the kind the agent
//! gave it (read, edit, execute…), the files it names and the changes it
//! carries, and an agent's own words stand where it gives no more.

use serde_json::Value;
use ui_state::{Held, SessionState};
use wire::{AcpToolKind, acp_item};

use super::{
    AskRow, CallPhase, Decision, ExploreVerb, FileChangeView, FileRow, HunkStart, NoticeReason,
    NoticeSeverity, PatchHunk, PatchLine, RowKind, agent_message, amux_call, ask_row, boundary,
    call_phase, chat_text, compaction, continued, counts, decision_view, duration, error,
    explore_result, first_change, output_head, output_tail, prose, thinking, turn_row,
    unrecognized,
};
use crate::code::LineKind;
use crate::text::{terminal_text, within};

/// The row kind, the decision meta, and whether it failed.
pub(super) fn kind_of(
    state: &SessionState,
    held: &Held,
    kind: &acp_item::Kind,
) -> (RowKind, Option<Decision>, bool) {
    use acp_item::Kind as Acp;
    let plain = |kind| (kind, None, false);
    match kind {
        Acp::Prompt(_) if wire::is_continued_input(&held.item.input_id) => plain(continued()),
        Acp::Prompt(_) => plain(RowKind::Prompt {
            text: chat_text(held, false),
            steered: false,
        }),
        Acp::Steer(_) => plain(RowKind::Prompt {
            text: chat_text(held, false),
            steered: true,
        }),
        Acp::Message(m) => plain(prose(held, m.complete, false)),
        Acp::Thought(t) => plain(thinking(state, held, t.complete)),
        // A question tool's call: the ask's row it names stands for it.
        Acp::Tool(tool) if !tool.ask_item_key.is_empty() => plain(RowKind::Hidden),
        Acp::Tool(tool) => tool_row(state, held, tool),
        Acp::Turn(turn) => plain(turn_row(state, held, turn)),
        // An ACP agent says its compaction as an item of its own.
        Acp::Compaction(c) => plain(compaction(c)),
        Acp::Notice(notice) => notice_row(held, notice),
        Acp::AgentMessage(m) => plain(agent_message(held, m)),
        Acp::Error(e) => error(e),
        Acp::Ask(ask) => {
            let mut row = ask_row(ask);
            // The agent asks its own form or link, which names no server.
            if let RowKind::Ask(AskRow::Form { server, .. } | AskRow::Link { server, .. }) =
                &mut row
                && server.is_empty()
            {
                *server = state.provider_name();
            }
            plain(row)
        }
        Acp::Boundary(b) => plain(boundary(b, false)),
        Acp::Unrecognized(u) => plain(unrecognized(u)),
    }
}

/// A notice: the agent's own, or why its turn stopped short. One the agent
/// marks an error is drawn as a failure.
fn notice_row(held: &Held, notice: &wire::AcpNotice) -> (RowKind, Option<Decision>, bool) {
    use wire::{AcpNoticeKind as Kind, AcpNoticeSeverity as Severity};
    let reason = match notice.kind() {
        Kind::Refusal => NoticeReason::Refusal,
        Kind::MaxTokens => NoticeReason::MaxTokens,
        Kind::MaxTurnRequests => NoticeReason::MaxTurnRequests,
        Kind::Agent | Kind::Unspecified => NoticeReason::Agent,
    };
    let severity = match notice.severity() {
        Severity::Error => NoticeSeverity::Error,
        Severity::Warning => NoticeSeverity::Warning,
        // A turn cut short is worth a look even when the agent did not say
        // how bad it is.
        Severity::Unspecified | Severity::Info if reason != NoticeReason::Agent => {
            NoticeSeverity::Warning
        }
        Severity::Unspecified | Severity::Info => NoticeSeverity::Info,
    };
    (
        RowKind::Notice {
            reason,
            severity,
            title: notice.title.clone(),
            // Why a turn stopped short is a typed reason, which each client
            // words in its own copy; the words the record carries for it
            // are amux's, not the agent's, so they are not handed over.
            text: if reason == NoticeReason::Agent {
                held.item.text.clone()
            } else {
                String::new()
            },
        },
        None,
        severity == NoticeSeverity::Error,
    )
}

/// One of the agent's calls, by the kind it gave it.
fn tool_row(
    state: &SessionState,
    held: &Held,
    tool: &wire::AcpTool,
) -> (RowKind, Option<Decision>, bool) {
    let decision = decision_view(tool.decision.as_ref(), &state.agent_state().permissions);
    let view = call_phase(state, held, tool.state, decision.as_ref());
    let failed = view == CallPhase::Failed;
    let cwd = &state.agent().cwd;
    let output = &held.item.text;
    let input = input_of(tool);
    if let Some((name, arguments)) = amux_tool(tool, &input)
        && let Some(kind) = amux_call(
            state,
            wire::AMUX_TOOL_SERVER,
            name,
            &arguments,
            output,
            view,
        )
    {
        return (kind, decision, failed);
    }
    // An agent often titles a call it has only begun with its tool's name
    // ("read"), which says nothing the verb does not: the row waits for a
    // place or a real title instead of reading "Read read".
    let title = || {
        let generic = ["read", "search", "fetch"]
            .iter()
            .any(|word| tool.title.trim().eq_ignore_ascii_case(word));
        if generic {
            String::new()
        } else {
            tool.title.clone()
        }
    };
    let place = || {
        tool.locations
            .first()
            .map(|location| within(cwd, &location.path))
            .filter(|path| !path.is_empty())
            .unwrap_or_else(title)
    };
    let explore = |verb| RowKind::Explore {
        verb,
        subject: match verb {
            // A search names what it looks for, which its title says.
            ExploreVerb::Search => title(),
            _ => place(),
        },
        state: view,
        result: explore_result(verb, view, output),
    };
    let kind = match tool.kind() {
        AcpToolKind::Read => explore(ExploreVerb::Read),
        AcpToolKind::Search => explore(ExploreVerb::Search),
        AcpToolKind::Fetch => explore(ExploreVerb::Fetch),
        AcpToolKind::Execute => {
            let (command, description) = command_of(tool, &input);
            let output = terminal_text(output);
            let (output_head, more_lines) = output_head(&output);
            RowKind::Command {
                command,
                description,
                state: view,
                exit_code: tool.exit_code,
                output_head,
                more_lines,
                output_trimmed: tool.output_truncated,
                output_tail: output_tail(&output),
                duration_ms: duration(held.item.at_ms, tool.ended_at_ms),
            }
        }
        AcpToolKind::Edit | AcpToolKind::Delete | AcpToolKind::Move
            if !tool.diffs.is_empty() || !tool.locations.is_empty() =>
        {
            RowKind::FileChange {
                files: file_rows(cwd, tool, view),
                state: view,
            }
        }
        _ => RowKind::ToolCall {
            server: String::new(),
            tool: tool.title.clone(),
            fact: String::new(),
            state: view,
            result: output.clone(),
        },
    };
    (kind, decision, failed)
}

/// What the call was given, as the agent wrote it; null until it says.
fn input_of(tool: &wire::AcpTool) -> Value {
    serde_json::from_slice(&tool.input_json).unwrap_or(Value::Null)
}

/// The files a call changes: each change it carries, else each file it
/// names. A change it has not landed wrote nothing, so a file it would
/// make is not said to be made.
fn file_rows(cwd: &str, tool: &wire::AcpTool, view: CallPhase) -> Vec<FileRow> {
    let landed = view.in_flight() || view == CallPhase::Succeeded;
    if tool.diffs.is_empty() {
        let change = match tool.kind() {
            AcpToolKind::Delete => FileChangeView::Deleted,
            AcpToolKind::Move if tool.locations.len() > 1 => FileChangeView::Moved {
                to: within(cwd, &tool.locations[1].path),
            },
            _ => FileChangeView::Edited,
        };
        let moved = matches!(change, FileChangeView::Moved { .. });
        return tool
            .locations
            .iter()
            .take(if moved { 1 } else { usize::MAX })
            .map(|location| FileRow {
                path: within(cwd, &location.path),
                change: change.clone(),
                added: 0,
                removed: 0,
                line: location.line,
            })
            .collect();
    }
    tool.diffs
        .iter()
        .map(|diff| {
            let lines = diff_patch(diff);
            let (added, removed) = counts(&lines);
            let change = match (&diff.old_text, tool.kind()) {
                (_, AcpToolKind::Delete) => FileChangeView::Deleted,
                (None, _) if landed => FileChangeView::Created { lines: added },
                (None, _) => FileChangeView::Writing { lines: added },
                (Some(_), _) => FileChangeView::Edited,
            };
            FileRow {
                path: within(cwd, &diff.path),
                change,
                added,
                removed,
                line: first_change(&lines),
            }
        })
        .collect()
}

/// A change an ACP agent gives, as one hunk ([`diff_lines`]), numbered
/// from where its texts start in the file when the agent says.
pub(crate) fn diff_patch(diff: &wire::AcpDiff) -> Vec<PatchHunk> {
    let starts = diff.old_line.zip(diff.new_line);
    diff_lines(diff.old_text.as_deref(), &diff.new_text, starts)
}

/// A change an ACP agent gives as text before and after, as one hunk:
/// what the two share at their start and end is left out, and what lies
/// between is removed then added. A new file's every line is added.
///
/// ACP's texts carry no place in the file, and agents differ: OpenCode sends
/// the whole file, Antigravity and Grok only the fragment they replace. A
/// call's location says which file it touches and perhaps a line in it,
/// not where these texts start, so it is no hunk start either. Only a new
/// file's lines have a known place, from its first line, and a change whose
/// agent said beside ACP where its texts start (`starts`, the old line and
/// the new); any other change's lines go unnumbered and claim no line.
fn diff_lines(old: Option<&str>, new: &str, starts: Option<(u32, u32)>) -> Vec<PatchHunk> {
    let created = old.is_none();
    let old: Vec<&str> = old.map(|old| old.lines().collect()).unwrap_or_default();
    let new: Vec<&str> = new.lines().collect();
    let start = old
        .iter()
        .zip(&new)
        .take_while(|(old, new)| old == new)
        .count();
    let end = old[start..]
        .iter()
        .rev()
        .zip(new[start..].iter().rev())
        .take_while(|(old, new)| old == new)
        .count();
    // Where the change starts on each side, past the lines it shares.
    let first = starts
        .or(created.then_some((1, 1)))
        .map(|(old, new)| (old + start as u32, new + start as u32));
    let line = |from: Option<u32>, offset: usize, kind, text: &str| PatchLine {
        number: from.map(|from| from + offset as u32),
        kind,
        text: text.to_owned(),
    };
    let mut lines = Vec::new();
    for (offset, text) in old[start..old.len() - end].iter().enumerate() {
        lines.push(line(
            first.map(|(old, _)| old),
            offset,
            LineKind::Removed,
            text,
        ));
    }
    for (offset, text) in new[start..new.len() - end].iter().enumerate() {
        lines.push(line(
            first.map(|(_, new)| new),
            offset,
            LineKind::Added,
            text,
        ));
    }
    if lines.is_empty() {
        return Vec::new();
    }
    // Where the change starts, so a removal alone still says its line.
    vec![PatchHunk {
        start: first.map(|(old, new)| HunkStart { old, new }),
        lines,
    }]
}

/// The command a call runs and what the agent said it is for. Agents name
/// the command's field differently, so the usual names are read; without
/// one, the call's title is the command.
pub(crate) fn command_of(tool: &wire::AcpTool, input: &Value) -> (String, String) {
    let text = |name: &str| input.get(name).and_then(Value::as_str).map(str::to_owned);
    let command = ["command", "CommandLine", "cmd"]
        .into_iter()
        .find_map(text)
        .or_else(|| {
            input.get("command").and_then(Value::as_array).map(|words| {
                words
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
        })
        .filter(|command| !command.is_empty());
    match command {
        Some(command) => (command, text("description").unwrap_or_default()),
        None => (tool.title.clone(), String::new()),
    }
}

/// The tool of amux's own server a call runs, with the arguments it was
/// given. ACP gives a call only a title, which agents write as the server's
/// tool in a few ways (`mcp_amux_spawn`, `Run mcp_amux_spawn`, `amux_spawn`,
/// `amux__spawn`), or as a tool that runs another by name, whose input
/// names the tool and holds its arguments.
fn amux_tool(tool: &wire::AcpTool, input: &Value) -> Option<(&'static str, Value)> {
    let server = wire::AMUX_TOOL_SERVER;
    let named = |name: &str| {
        let name = name.strip_prefix("Run ").unwrap_or(name);
        // Each by its name now and by the one a release before 0.11 gave
        // it, which a resumed agent's history still holds.
        [
            (wire::START_TOOL, wire::former_tools::SPAWN_TOOL),
            (wire::STOP_TOOL, wire::former_tools::STOP_TOOL),
            (wire::AGENTS_TOOL, wire::former_tools::AGENTS_TOOL),
            (wire::HOSTS_TOOL, wire::former_tools::HOSTS_TOOL),
        ]
        .into_iter()
        .find(|(now, before)| {
            [now, before].into_iter().any(|tool| {
                name == format!("mcp_{server}_{tool}")
                    || name == format!("{server}__{tool}")
                    || name == format!("{server}_{tool}")
            })
        })
        .map(|(now, _)| now)
    };
    if let Some(found) = named(&tool.title) {
        return Some((found, input.clone()));
    }
    let runs = input.get("tool_name").and_then(Value::as_str)?;
    let arguments = ["tool_input", "arguments", "input"]
        .into_iter()
        .find_map(|field| input.get(field))
        .cloned()
        .unwrap_or(Value::Null);
    Some((named(runs)?, arguments))
}

/// What a step acted on, for the activity line: the command it runs or the
/// file it names, else its title.
pub(super) fn subject(tool: &wire::AcpTool) -> String {
    match tool.kind() {
        AcpToolKind::Execute => command_of(tool, &input_of(tool)).0,
        _ => tool
            .locations
            .first()
            .map(|location| location.path.clone())
            .unwrap_or_else(|| tool.title.clone()),
    }
}

/// The first lines of the patch a landed change made: its first file's.
pub(super) fn patch(tool: &wire::AcpTool) -> Option<Vec<PatchHunk>> {
    let diff = tool.diffs.first()?;
    Some(diff_patch(diff))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(hunks: &[PatchHunk]) -> Vec<(Option<u32>, LineKind, String)> {
        hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .map(|line| (line.number, line.kind, line.text.clone()))
            .collect()
    }

    /// A change reads as the lines it changed, not as the whole text
    /// removed and written again; a new file's lines are numbered from its
    /// first.
    #[test]
    fn a_change_reads_as_the_lines_it_changed() {
        let lines = diff_lines(Some("a\nb\nc\nd\n"), "a\nB\nc\nd\n", None);
        assert_eq!(
            kinds(&lines),
            [
                (None, LineKind::Removed, "b".to_owned()),
                (None, LineKind::Added, "B".to_owned()),
            ]
        );
        let made = diff_lines(None, "one\ntwo\n", None);
        assert_eq!(counts(&made), (2, 0));
        assert_eq!(first_change(&made), Some(1));
        assert_eq!(
            kinds(&made),
            [
                (Some(1), LineKind::Added, "one".to_owned()),
                (Some(2), LineKind::Added, "two".to_owned()),
            ]
        );
        let grown = diff_lines(Some("a\n"), "a\nb\n", None);
        assert_eq!(kinds(&grown), [(None, LineKind::Added, "b".to_owned())]);
    }

    /// Antigravity sends only the fragment it replaces: the change is shown
    /// without numbers and claims no line, where counting from the
    /// fragment's start would say line 1 for a change further down. A line
    /// the call's location names says what the tool touches, not where the
    /// texts start, so it numbers nothing either.
    #[test]
    fn a_change_with_no_place_in_the_file_claims_no_line() {
        let lines = diff_lines(Some("Line two."), "Line two, edited.", None);
        assert_eq!(
            kinds(&lines),
            [
                (None, LineKind::Removed, "Line two.".to_owned()),
                (None, LineKind::Added, "Line two, edited.".to_owned()),
            ]
        );
        assert_eq!(first_change(&lines), None);
        let tool = wire::AcpTool {
            kind: AcpToolKind::Edit as i32,
            locations: vec![wire::AcpLocation {
                path: "/work/notes.txt".into(),
                line: Some(7),
            }],
            diffs: vec![wire::AcpDiff {
                path: "/work/notes.txt".into(),
                old_text: Some("Line two.".into()),
                new_text: "Line two, edited.".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let rows = file_rows("/work", &tool, CallPhase::Succeeded);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].line, None);
        let patch = patch(&tool).unwrap();
        assert!(kinds(&patch).iter().all(|(number, _, _)| number.is_none()));
    }

    /// The usual names agents give a command's field are read, and the
    /// title stands in for one that gives none.
    #[test]
    fn a_command_is_read_from_the_agents_own_field() {
        let tool = wire::AcpTool {
            title: "Run it".into(),
            ..Default::default()
        };
        for input in [
            serde_json::json!({"command": "ls -la", "description": "list"}),
            serde_json::json!({"CommandLine": "ls -la", "description": "list"}),
            serde_json::json!({"command": ["ls", "-la"], "description": "list"}),
        ] {
            assert_eq!(
                command_of(&tool, &input),
                ("ls -la".to_owned(), "list".to_owned())
            );
        }
        assert_eq!(
            command_of(&tool, &Value::Null),
            ("Run it".to_owned(), String::new())
        );
    }

    /// amux's own tools are found under every way agents name them.
    #[test]
    fn amuxs_own_tools_are_found_however_an_agent_names_them() {
        for title in [
            "mcp_amux_start_subagent",
            "Run mcp_amux_start_subagent",
            "amux_start_subagent",
            "amux__start_subagent",
            // As a release before 0.11 named it.
            "mcp_amux_spawn",
            "amux__spawn",
        ] {
            let tool = wire::AcpTool {
                title: title.into(),
                ..Default::default()
            };
            assert_eq!(
                amux_tool(&tool, &serde_json::json!({"name": "a"})).map(|(tool, _)| tool),
                Some(wire::START_TOOL),
                "{title}"
            );
        }
        let runs = wire::AcpTool {
            title: "use_tool".into(),
            ..Default::default()
        };
        let input = serde_json::json!({"tool_name": "amux__stop", "tool_input": {"name": "b"}});
        assert_eq!(
            amux_tool(&runs, &input),
            Some((wire::STOP_TOOL, serde_json::json!({"name": "b"})))
        );
        let other = wire::AcpTool {
            title: "spec__ask_the_operator".into(),
            ..Default::default()
        };
        assert_eq!(amux_tool(&other, &Value::Null), None);
    }
}
