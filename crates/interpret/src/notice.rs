//! What each kind's state says to a person's lock screen.
//!
//! A notice is a projection of an interpreter's state, built with every
//! snapshot so it cannot drift from the body beside it. The daemon holds and
//! sends it without decoding anything, so this is where a provider's asks
//! and failures become the few shapes a notification has.

use wire::{
    AskNotice, BlockedNotice, ChangeWant, Empty, Notice, OtherWant, PlanWant, QuestionWant,
    RunWant, UsageBlock, UseWant, ask, ask_notice, blocked_notice, codex_ask, notice, tool_input,
};

/// The most characters any one string keeps. A push has a few kilobytes for
/// the whole notice, sealed and encoded, and a lock screen shows a few lines.
const MOST_CHARS: usize = 400;

fn clip(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(MOST_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

fn wants_ask(key: &str, wants: ask_notice::Wants, why: &str, more: u32) -> Notice {
    Notice {
        of: Some(notice::Of::Ask(AskNotice {
            key: key.to_owned(),
            wants: Some(wants),
            why: clip(why),
            more,
        })),
    }
}

fn run(command: &str) -> ask_notice::Wants {
    ask_notice::Wants::Run(RunWant {
        command: clip(command),
    })
}

fn use_tool(server: &str, tool: &str) -> ask_notice::Wants {
    ask_notice::Wants::Use(UseWant {
        server: clip(server),
        tool: clip(tool),
    })
}

fn question(asked: &wire::QuestionAsk) -> ask_notice::Wants {
    let first = asked.questions.first();
    ask_notice::Wants::Question(QuestionWant {
        text: clip(first.map_or("", |question| {
            if question.question.is_empty() {
                &question.header
            } else {
                &question.question
            }
        })),
    })
}

fn plan(title: &str) -> ask_notice::Wants {
    ask_notice::Wants::Plan(PlanWant { title: clip(title) })
}

fn other(text: &str) -> ask_notice::Wants {
    ask_notice::Wants::Other(OtherWant { text: clip(text) })
}

/// The title a plan's first line gives it, when it gives one of its own.
pub(crate) fn plan_title(text: &str) -> String {
    let first = text.trim_start().lines().next().unwrap_or_default();
    let title = first.strip_prefix('#').map_or("", |heading| {
        heading.trim_start_matches('#').trim()
    });
    if title.eq_ignore_ascii_case("plan") {
        return String::new();
    }
    // A notification already says it is a plan.
    title.strip_prefix("Plan:").unwrap_or(title).trim().to_owned()
}

/// Claude's head ask, either kind of Claude. A permission says what it would
/// run or change where the input is that; any other tool says only its name,
/// because its arguments are the tool's own and could be anything.
pub(crate) fn claude_ask(asked: &wire::Ask, more: u32, plan_title: &str) -> Notice {
    let (wants, why) = match &asked.body {
        Some(ask::Body::Permission(permission)) => {
            let input = permission.input.as_ref();
            let subject = input.map_or("", |input| input.subject.as_str());
            match input.and_then(|input| input.of.as_ref()) {
                Some(tool_input::Of::Shell(shell)) => (
                    run(subject),
                    first_said(&[&permission.reason, &permission.description, &shell.description]),
                ),
                Some(tool_input::Of::Edit(_)) => (
                    ask_notice::Wants::Change(ChangeWant {
                        path: clip(subject),
                        files: 1,
                    }),
                    permission.reason.as_str(),
                ),
                _ => (
                    use_tool(&permission.server, &permission.tool_name),
                    permission.reason.as_str(),
                ),
            }
        }
        Some(ask::Body::Question(asked)) => (question(asked), ""),
        Some(ask::Body::Plan(_)) => (plan(plan_title), ""),
        Some(ask::Body::Form(form)) => (other(&form.message), ""),
        Some(ask::Body::Link(link)) => (other(&link.message), ""),
        Some(ask::Body::Unanswerable(unanswerable)) => (other(&unanswerable.reason), ""),
        None => (other(""), ""),
    };
    wants_ask(&asked.key, wants, why, more)
}

pub(crate) fn codex_ask(asked: &wire::CodexAsk, more: u32, plan_title: &str) -> Notice {
    let (wants, why) = match &asked.body {
        Some(codex_ask::Body::Command(command)) => (run(&command.command), command.reason.as_str()),
        Some(codex_ask::Body::FileChange(change)) => (
            ask_notice::Wants::Change(ChangeWant {
                path: clip(change.changes.first().map_or("", |file| file.path.as_str())),
                files: change.changes.len() as u32,
            }),
            change.reason.as_str(),
        ),
        Some(codex_ask::Body::McpTool(tool)) => (use_tool(&tool.server, &tool.tool), ""),
        Some(codex_ask::Body::McpForm(form)) => (other(&form.message), ""),
        Some(codex_ask::Body::McpLink(link)) => (other(&link.message), ""),
        Some(codex_ask::Body::Access(access)) => (other(&access.reason), ""),
        Some(codex_ask::Body::Question(asked)) => (question(asked), ""),
        Some(codex_ask::Body::Plan(_)) => (plan(plan_title), ""),
        None => (other(""), ""),
    };
    wants_ask(&asked.key, wants, why, more)
}

fn first_said<'a>(texts: &[&'a str]) -> &'a str {
    texts
        .iter()
        .copied()
        .find(|text| !text.is_empty())
        .unwrap_or_default()
}

pub(crate) fn signed_out() -> BlockedNotice {
    BlockedNotice {
        why: Some(blocked_notice::Why::SignedOut(Empty {})),
    }
}

pub(crate) fn usage_spent(resets_at_ms: Option<i64>) -> BlockedNotice {
    BlockedNotice {
        why: Some(blocked_notice::Why::Usage(UsageBlock { resets_at_ms })),
    }
}

pub(crate) fn failed(message: &str) -> BlockedNotice {
    BlockedNotice {
        why: Some(blocked_notice::Why::Failed(clip(message))),
    }
}

/// When the spent windows reset: the latest of them, since the agent can't
/// go on until the last one has.
pub(crate) fn spent_until<'a>(
    meters: impl Iterator<Item = &'a wire::UsageMeter>,
) -> Option<i64> {
    meters
        .filter(|meter| meter.state() == wire::UsageState::Blocked)
        .filter_map(|meter| meter.resets_at_ms)
        .max()
}

/// A notice on one line, for the goldens.
pub(crate) fn describe(notice: &Notice) -> String {
    let wants = |wants: &Option<ask_notice::Wants>| match wants {
        Some(ask_notice::Wants::Run(run)) => format!("run {:?}", run.command),
        Some(ask_notice::Wants::Change(change)) => {
            format!("change {:?} files={}", change.path, change.files)
        }
        Some(ask_notice::Wants::Use(tool)) => format!("use {:?} {:?}", tool.server, tool.tool),
        Some(ask_notice::Wants::Question(question)) => format!("question {:?}", question.text),
        Some(ask_notice::Wants::Plan(plan)) => format!("plan {:?}", plan.title),
        Some(ask_notice::Wants::Other(other)) => format!("other {:?}", other.text),
        None => "nothing".to_owned(),
    };
    match &notice.of {
        Some(notice::Of::Ask(ask)) => format!(
            "ask {} {} why={:?} more={}",
            ask.key,
            wants(&ask.wants),
            ask.why,
            ask.more
        ),
        Some(notice::Of::Blocked(blocked)) => match &blocked.why {
            Some(blocked_notice::Why::SignedOut(_)) => "blocked signed-out".to_owned(),
            Some(blocked_notice::Why::Usage(usage)) => {
                format!("blocked usage resets={:?}", usage.resets_at_ms)
            }
            Some(blocked_notice::Why::Failed(message)) => format!("blocked failed {message:?}"),
            None => "blocked".to_owned(),
        },
        Some(notice::Of::Died(died)) => format!("died {} {:?}", died.cause, died.detail),
        None => "nothing".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_string_is_clipped_on_a_character_boundary() {
        let clipped = clip(&"é".repeat(MOST_CHARS + 50));
        assert_eq!(clipped.chars().count(), MOST_CHARS + 1);
        assert!(clipped.ends_with('…'));
        assert_eq!(clip("  short  "), "short");
    }

    #[test]
    fn a_plan_is_titled_by_a_heading_of_its_own() {
        assert_eq!(plan_title("# Update the README\n\n- step"), "Update the README");
        assert_eq!(plan_title("# Plan: Update it\n- step"), "Update it");
        assert_eq!(plan_title("\n## Plan\n- step"), "");
        assert_eq!(plan_title("- step\n- step"), "");
    }

    #[test]
    fn the_spent_windows_reset_when_the_last_of_them_does() {
        let meter = |state: wire::UsageState, resets_at_ms| wire::UsageMeter {
            state: state as i32,
            resets_at_ms,
            ..Default::default()
        };
        let meters = [
            meter(wire::UsageState::Blocked, Some(10)),
            meter(wire::UsageState::Blocked, Some(30)),
            meter(wire::UsageState::Ok, Some(99)),
        ];
        assert_eq!(spent_until(meters.iter()), Some(30));
    }
}
