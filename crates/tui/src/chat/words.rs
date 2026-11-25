//! Text, status lines, key-hint legends and control cycling helpers for an
//! open chat.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use ui_state::{ActivityKind, Composer, JobStop, PhaseView, Refusal, SessionState, Waiting};
use ui_view::{Changeable, ExitCause, Offline, Reach, RowKind, ToolRows, chat_rows_for};

use super::{ChatEffect, ROUNDED, feed, framed};
#[cfg(feature = "fixtures")]
use super::{boxed_composer, hint_line, marked_edge_in};
use crate::editor::Editor;
use crate::text::{self, push};
use crate::theme::Theme;

/// Why a slash command was not sent: the agent does not run it, and what it
/// does run.
pub(super) fn refused_command_words(
    state: &SessionState,
    command: &str,
    offered: &[String],
    hint: Option<&str>,
) -> String {
    let agent = state.provider_name();
    if let Some(hint) = hint {
        return format!("{agent} takes a skill as a mention: type {hint}");
    }
    let commands: Vec<String> = offered.iter().map(|name| format!("/{name}")).collect();
    if commands.is_empty() {
        format!("{agent} does not run /{command} here")
    } else {
        format!(
            "{agent} does not run /{command} here; it runs {}",
            commands.join(" ")
        )
    }
}

/// Takes a queued prompt back; its words return to the composer (after any
/// draft there, on a new line).
pub(super) fn withdraw_effect(state: &SessionState, input_id: &[u8]) -> Option<ChatEffect> {
    state
        .queue()
        .into_iter()
        .find(|row| row.entry.input_id == input_id)
        .map(|row| ChatEffect::Withdraw {
            id: input_id.to_vec(),
            text: row.entry.text.clone(),
            attachments: row.entry.attachments.clone(),
        })
}

/// Where the chat stands, only when that is a problem the person should
/// see: the host away, reconnecting, catching up, a rebuilt history on its
/// way, or the agent exited. Working, idle and needs-you show in the feed.
pub(super) fn problem_words(
    state: &SessionState,
    reach: Reach,
    host: &str,
    theme: Theme,
) -> Option<(String, ratatui::style::Style)> {
    let away_words = || {
        let host = if host.is_empty() { "its device" } else { host };
        match reach {
            Reach::Offline(Offline::SignedOut) => {
                format!("{host} away · this device is signed out")
            }
            Reach::Offline(Offline::Revoked) => format!("{host} unpaired this device"),
            Reach::Away => format!("{host} away · {FREE_PLAN_AWAY}"),
            Reach::Online(_) | Reach::Offline(Offline::Plain) => {
                format!("{host} away · not current")
            }
        }
    };
    Some(match (state.composer(), state.phase()) {
        // Resuming asks the host, so why it is away matters more than why
        // the agent ended.
        (_, PhaseView::Exited { cause }) if !reach.online() => {
            let ended = match cause {
                ExitCause::Failed(_) => "stopped",
                _ if settled(state) => "settled",
                ExitCause::Finished => "finished",
                ExitCause::Ended => "stopped",
            };
            (format!("{ended} · {}", away_words()), theme.warning())
        }
        (_, PhaseView::Exited { cause }) => (exit_words(&cause, settled(state)), theme.faint()),
        (Composer::Waiting(Waiting::Detached), _) => (away_words(), theme.warning()),
        (Composer::Waiting(Waiting::Reconnecting), _) => {
            ("reconnecting".to_owned(), theme.warning())
        }
        (Composer::Waiting(Waiting::CatchingUp), _) => ("catching up".to_owned(), theme.faint()),
        _ if state.reset_pending() => ("refreshing".to_owned(), theme.faint()),
        _ => return None,
    })
}

/// Why a setting cannot change from here, as a notice: how it does
/// change, or that the agent has not offered any yet.
pub(super) fn unchangeable_words(
    view: &ui_view::SettingsView,
    setting: &Changeable,
    item: crate::setup::Item,
) -> String {
    use crate::setup::Item;
    match setting {
        Changeable::ByTyping(_) => {
            crate::words::by_typing_words(&view.changeable.model, &view.changeable.effort)
                .unwrap_or_default()
        }
        Changeable::Cycle => {
            "Terminal Claude changes permission only by cycling through its permissions.".to_owned()
        }
        Changeable::Pick | Changeable::NotOffered => {
            let what = match item {
                Item::Model => "models",
                Item::Effort => "efforts",
                _ => "permissions",
            };
            format!("the agent has not offered its {what} yet")
        }
    }
}

/// "finished", "stopped", "settled", or "stopped · its directory failed:
/// it is gone": how an exited agent's feed ends. An agent the person
/// settled says so, since that is what they did; why one failed outranks
/// it.
pub(super) fn exit_words(cause: &ExitCause, settled: bool) -> String {
    match cause {
        ExitCause::Failed(cause) => format!("stopped · {cause}"),
        _ if settled => "settled".to_owned(),
        ExitCause::Finished => "finished".to_owned(),
        ExitCause::Ended => "stopped".to_owned(),
    }
}

pub(super) fn settled(state: &SessionState) -> bool {
    state.agent().filing() == wire::Filing::Closed
}

/// Whether the agent's newest row is its own exit boundary.
pub(super) fn ends_with_exit(state: &SessionState) -> bool {
    let transcript = state.transcript();
    let Some(held) = transcript.head().and_then(|head| transcript.at(head)) else {
        return false;
    };
    chat_rows_for(
        state,
        std::slice::from_ref(&held.item.key),
        ToolRows::ShowAll,
    )
    .last()
    .is_some_and(|row| {
        matches!(
            row.kind,
            RowKind::Boundary {
                kind: wire::BoundaryKind::Exited,
                ..
            }
        )
    })
}

/// A provider subagent's composer: no field, since it takes no messages,
/// only who runs it, and how its stop stands once one went (stopping, or
/// the provider's refusal).
pub(super) fn native_box(
    who: &str,
    stop: &JobStop,
    controls: &ui_view::ControlsSummary,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    const MARGIN: usize = 2;
    let inner = width.saturating_sub(2 * MARGIN + 4).max(1);
    let said = format!("{who} runs this subagent · it takes no messages");
    let mut body = vec![Line::from(Span::styled(
        text::ellipsize(&said, inner),
        theme.muted(),
    ))];
    match stop {
        JobStop::Stopping => body.push(Line::from(Span::styled("Stopping…", theme.faint()))),
        JobStop::Refused { error, .. } => {
            let words = format!("Could not stop it: {}", text::first_line(error));
            body.extend(
                text::wrap(&words, inner)
                    .into_iter()
                    .map(|line| Line::from(Span::styled(line, theme.error()))),
            );
        }
        JobStop::Offered { .. } | JobStop::Unavailable => {}
    }
    framed(body, theme.hairline(), &ROUNDED, controls, width, theme).0
}

/// The keys under a subagent's composer: its stop while one can go.
pub(super) fn native_hint(stop: &JobStop, leader: char) -> Hint {
    let more = format!("ctrl+{leader}");
    let mut pairs = Vec::new();
    if stop.key().is_some() {
        pairs.push(("ctrl+x", "stop"));
    }
    pairs.push((more.as_str(), "more"));
    Hint::Keys(keys(&pairs))
}

/// A provider subagent's composer and the keys under it, its edge naming
/// `model`, as the component catalogue draws it.
#[cfg(feature = "fixtures")]
pub(crate) fn native_composer_lines(
    who: &str,
    stop: &JobStop,
    model: &str,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let edge = ui_view::ControlsSummary {
        model: Some(model.to_owned()),
        ..Default::default()
    };
    let mut lines = native_box(who, stop, &edge, width, theme);
    lines.push(Line::default());
    lines.push(hint_line(&native_hint(stop, 'a'), width, theme).0);
    lines
}

/// The sign-in notice over an empty composer, as the component catalogue
/// draws it.
#[cfg(feature = "fixtures")]
pub(crate) fn sign_in_composer_lines(
    host: &str,
    sign_in: &ui_view::SignInView,
    name: &str,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = sign_in_notice(host, sign_in, width, theme);
    lines.push(Line::default());
    lines.extend(composer_lines(
        &Editor::default(),
        &Composer::Send,
        name,
        host,
        Reach::Online(wire::RouteKind::Direct),
        width,
        theme,
    ));
    lines
}

/// The notice for an agent whose account needs signing in: who, the
/// account and what went wrong, how to sign in, and that sending tries
/// again. Nothing here can sign in for you, so there are no choices; the
/// provider's next good answer takes the notice away.
pub(super) fn sign_in_notice(
    host: &str,
    sign_in: &ui_view::SignInView,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    // The notice's words line up with the composer's text below it.
    const INDENT: usize = 4;
    let inner = width.saturating_sub(INDENT + 2).max(1);
    let who = &sign_in.provider_name;
    let steps = sign_in_steps(who, &sign_in.hint);
    let indented = || Line::from(" ".repeat(INDENT));
    let mut lines = Vec::new();
    let mut head = Line::from(vec![
        Span::raw(" ".repeat(INDENT - 2)),
        Span::styled("● ", theme.attention()),
    ]);
    push(
        &mut head,
        format!("{who} needs you to sign in"),
        theme.text(),
        width,
    );
    lines.push(head);
    let what = match sign_in.state {
        wire::SignInState::Failed => "Sign-in failed",
        _ => "Signed out",
    };
    let mut status = indented();
    push(&mut status, what, theme.muted(), width);
    if !sign_in.account.is_empty() {
        push(
            &mut status,
            format!(" · {}", sign_in.account),
            theme.muted(),
            width,
        );
    }
    lines.push(status);
    for part in text::wrap(&sign_in.message, inner) {
        if part.is_empty() {
            continue;
        }
        let mut line = indented();
        push(&mut line, part, theme.faint(), width);
        lines.push(line);
    }
    let mut how = indented();
    for (words, code) in steps {
        push(
            &mut how,
            words,
            if code { theme.code() } else { theme.text() },
            width,
        );
    }
    push(&mut how, format!(" on {host}."), theme.text(), width);
    lines.push(how);
    let mut retry = indented();
    push(
        &mut retry,
        "Signed in elsewhere? Send a message to try again.",
        theme.muted(),
        width,
    );
    lines.push(retry);
    lines
}

/// How to sign in, in the provider's words as its host gives them, said on
/// the agent's host: its closing stop gives way to where. What the words
/// mark as code (`opencode auth login`) is drawn as code. Without any,
/// amux can only say to sign in.
fn sign_in_steps(who: &str, hint: &str) -> Vec<(String, bool)> {
    let hint = hint.trim().trim_end_matches('.');
    if hint.is_empty() {
        return vec![(format!("Sign in to {who}"), false)];
    }
    hint.split('`')
        .enumerate()
        .filter(|(_, part)| !part.is_empty())
        .map(|(at, part)| (part.to_owned(), at % 2 == 1))
        .collect()
}

/// Why a host is away when its daemon lists it so: the relay alone sees
/// it, and on the free plan the relay opens no connection between hosts.
pub(crate) const FREE_PLAN_AWAY: &str = "relayed connections need Pro";

/// Why the composer cannot send while the link to the agent's host is
/// down, for the box's edge; None when it can.
pub(super) fn waiting_words(composer: &Composer, host: &str, reach: Reach) -> Option<String> {
    Some(match composer {
        Composer::Waiting(Waiting::Detached) => match reach {
            Reach::Offline(Offline::Revoked) => format!("{host} unpaired this device"),
            Reach::Offline(Offline::SignedOut) => {
                format!("{host} is away · this device is signed out")
            }
            Reach::Away => format!("{host} is away · {FREE_PLAN_AWAY}"),
            Reach::Online(_) | Reach::Offline(Offline::Plain) => format!("{host} is away"),
        },
        // The client talks only to this machine's daemon: reconnecting is
        // that daemon gone, never the agent's host, as home says it.
        Composer::Waiting(Waiting::Reconnecting) => "reconnecting to amux".to_owned(),
        Composer::Waiting(Waiting::CatchingUp) => "catching up".to_owned(),
        // Its box says who runs it, in place of a field.
        Composer::Native { .. } | Composer::Send | Composer::Resume | Composer::Ended => {
            return None;
        }
    })
}

/// In place of the composer once an agent that cannot be resumed has
/// exited: its program cannot reopen the conversation, so there is nothing
/// to type into, and nothing is passed off as a resume.
pub(super) fn ended_box(
    who: &str,
    controls: &ui_view::ControlsSummary,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    const MARGIN: usize = 2;
    let inner = width.saturating_sub(2 * MARGIN + 4).max(1);
    let said = format!("Ended · {who} can't reopen this conversation");
    let body = text::wrap(&said, inner)
        .into_iter()
        .map(|line| Line::from(Span::styled(line, theme.muted())))
        .collect();
    framed(body, theme.hairline(), &ROUNDED, controls, width, theme).0
}

/// The composer's top edge while terminal Claude's own view is on another
/// conversation: prompts are refused until it is back on this one.
pub(super) const SWITCHED_AWAY_WORDS: &str =
    "Claude's own view switched to another conversation · ctrl+b switch back";

/// What the plain composer's top edge says stands in the way of sending,
/// most pressing first, with its ink: the last prompt refused, Claude's own
/// view on another conversation, the link to the host down, an exited agent
/// that Enter resumes, a usage limit reached (sending stays open).
pub(super) fn edge_title(
    not_sent: Option<&str>,
    switched_away: bool,
    composer: &Composer,
    host: &str,
    reach: Reach,
    limit: Option<String>,
    theme: Theme,
) -> Option<(String, ratatui::style::Style)> {
    if let Some(reason) = not_sent {
        return Some((reason.to_owned(), theme.error()));
    }
    if switched_away {
        return Some((SWITCHED_AWAY_WORDS.to_owned(), theme.warning()));
    }
    waiting_words(composer, host, reach)
        .map(|words| (words, theme.faint()))
        .or_else(|| match composer {
            Composer::Resume => Some(("Enter resumes".to_owned(), theme.muted())),
            _ => None,
        })
        .or_else(|| limit.map(|words| (words, theme.warning())))
}

/// The plain composer as the chat draws it for `name` on `host`: the draft
/// or the invitation in its box, and its top edge saying what stands in the
/// way. For the vocabulary.
#[cfg(feature = "fixtures")]
pub(crate) fn composer_lines(
    editor: &Editor,
    composer: &Composer,
    name: &str,
    host: &str,
    reach: Reach,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let invite = format!("Message {name}");
    let edge = ui_view::ControlsSummary::default();
    let mut lines = boxed_composer(editor, &invite, &edge, true, width, theme).lines;
    if let Some((words, ink)) = edge_title(None, false, composer, host, reach, None, theme)
        && let Some(top) = lines.first_mut()
    {
        *top = marked_edge_in(&words, ink, theme.muted(), &ROUNDED, width);
    }
    lines
}

/// Why the agent or its host refused an input, in words: the refusal's own
/// when it has them.
pub(crate) fn refusal_words(refusal: &Refusal) -> String {
    if let Some(detail) = &refusal.detail {
        return detail.clone();
    }
    match refusal.reason {
        ui_state::RefusalReason::Exited => "it had exited",
        ui_state::RefusalReason::AgentClosed => "it was settled",
        ui_state::RefusalReason::Exiting => "it was exiting",
        ui_state::RefusalReason::Draining => "its device is shutting down",
        ui_state::RefusalReason::HostUnreachable => "its device is away",
        ui_state::RefusalReason::Unsupported => "this agent can't take it",
        ui_state::RefusalReason::ClosedAsk => "the ask had already closed",
        ui_state::RefusalReason::NotQueued => "it was no longer queued",
        ui_state::RefusalReason::ConversationElsewhere => {
            "Claude's own view switched to another conversation"
        }
        ui_state::RefusalReason::TooLong => "it is longer than the agent takes",
        ui_state::RefusalReason::Unspecified => "it was refused",
    }
    .to_owned()
}

/// Whether the chat has a draft to type into. A provider subagent's has
/// none: its composer is no text box, so a key, a paste or a quote there
/// would land in a draft nobody sees, one that later swallows Ctrl+C and
/// `?` while the screen shows nothing to clear.
pub(super) fn has_draft(state: &SessionState) -> bool {
    !matches!(state.composer(), Composer::Native { .. })
}

/// What an agent is doing, as the activity line names it.
pub(crate) fn activity_words(kind: &ActivityKind) -> String {
    match kind {
        ActivityKind::Thinking => "Thinking".to_owned(),
        ActivityKind::Subagents { count } => format!(
            "{count} subagent{} working",
            if *count == 1 { "" } else { "s" }
        ),
        ActivityKind::Compacting => "Compacting".to_owned(),
        ActivityKind::SafetyCheck => "Running safety checks".to_owned(),
        ActivityKind::Retrying {
            attempt,
            max_attempts: 0,
            ..
        } => format!("Retrying · attempt {attempt}"),
        ActivityKind::Retrying {
            attempt,
            max_attempts,
            ..
        } => format!("Retrying · attempt {attempt} of {max_attempts}"),
        ActivityKind::Working | ActivityKind::Running { .. } => "Working".to_owned(),
    }
}

/// While the agent works, one quiet line that it is, and for how long, in
/// whole seconds once there is one. What it is doing shows as live steps in
/// the feed, so a call in flight is the turn's work, timed with the turn.
/// While the agent waits on its subagents and one of them waits on the
/// person, the line says how they stand, as their group's row does, from
/// `live`: the subagents the chat's rows opened that are still live.
pub(super) fn quiet_activity(
    activity: &ui_state::Activity,
    live: &ui_view::GroupState,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let ms = match activity.kind {
        ActivityKind::Working | ActivityKind::Running { .. } => activity.turn_elapsed_ms,
        _ => activity.elapsed_ms,
    };
    let mut line = Line::from(Span::raw("    "));
    if matches!(activity.kind, ActivityKind::Subagents { .. }) && live.needs_you > 0 {
        for (at, (words, ink)) in feed::group_words(live, theme).into_iter().enumerate() {
            if at > 0 {
                push(&mut line, " · ", theme.faint(), width);
            }
            push(&mut line, words, ink, width);
        }
    } else {
        push(
            &mut line,
            activity_words(&activity.kind),
            theme.muted(),
            width,
        );
    }
    if let ActivityKind::Retrying { retry_at_ms, .. } = activity.kind {
        // How long until the next attempt says more than how long since the
        // last. The activity's own clock is since plus elapsed.
        let left = retry_at_ms.map_or(0, |at| at - activity.since_ms - activity.elapsed_ms);
        if left >= 1_000 {
            let left = text::duration(left - left % 1_000);
            push(&mut line, format!(" · in {left}"), theme.faint(), width);
        }
    } else if ms >= 1_000 {
        let elapsed = text::duration(ms - ms % 1_000);
        push(&mut line, format!(" · {elapsed}"), theme.faint(), width);
    }
    line
}

/// The keys under the composer, by what is happening, few enough to read
/// at a glance and always ending with the way to more (the leader's
/// panel). What everyone knows (Enter sends, pasting attaches) is not
/// said; Enter is named only when it does something else.
pub(super) fn turn_hint(state: &SessionState, editor: &Editor, reach: Reach, leader: char) -> Hint {
    let working = state.phase() == PhaseView::Working;
    let shift_tab = next_control(&ui_view::settings(state)).map(|change| match change {
        ui_view::SettingChange::Mode(_) => ("shift+tab", "mode"),
        _ => ("shift+tab", "permission"),
    });
    let more = format!("ctrl+{leader}");
    let mut pairs = Vec::new();
    // Not current, Enter sends nothing: the draft stays until it can go.
    let waits = |until| Hint::Keys(keys(&[("enter", until)]));
    match state.composer() {
        Composer::Send => {
            if working {
                pairs.push(("enter", "queue"));
                pairs.push(("ctrl+x", "interrupt"));
            } else if !editor.is_empty() && !crate::terminal::shift_enter_reported() {
                // Shift+Enter is the newline everyone expects; where the
                // terminal cannot tell it from Enter, say the key that works.
                pairs.push(("ctrl+j", "newline"));
            }
            pairs.extend(shift_tab);
        }
        Composer::Resume => pairs.push(("enter", "resume")),
        Composer::Native { .. } | Composer::Ended => {}
        Composer::Waiting(Waiting::Detached) if reach == Reach::Offline(Offline::SignedOut) => {
            return waits("can't send until this device signs in");
        }
        Composer::Waiting(Waiting::Detached) if reach == Reach::Offline(Offline::Revoked) => {
            return waits("can't send until you pair again");
        }
        Composer::Waiting(Waiting::Detached) => {
            return waits("can't send until its device is back");
        }
        Composer::Waiting(Waiting::Reconnecting) => {
            return waits("can't send until amux is back");
        }
        Composer::Waiting(Waiting::CatchingUp) => {
            return waits("can't send until caught up");
        }
    }
    pairs.push((&more, "more"));
    Hint::Keys(keys(&pairs))
}

/// A key legend as (key, action) pairs, most needed first.
pub(crate) type Keys = Vec<(String, String)>;

pub(crate) fn keys(pairs: &[(&str, &str)]) -> Keys {
    pairs
        .iter()
        .map(|(key, action)| ((*key).to_owned(), (*action).to_owned()))
        .collect()
}

/// What the line under the composer says: a key legend, or words that are
/// not one.
pub(super) enum Hint {
    Keys(Keys),
    /// A word on what just happened, then keys.
    Noted(&'static str, Keys),
    Sentence(String),
}

/// The key a legend names, as the event pressing it sends: `enter`,
/// `shift+tab`, `ctrl+x`, a letter.
pub(super) fn key_event(key: &str) -> Option<KeyEvent> {
    let (modifiers, name) = match key.split_once('+') {
        Some(("ctrl", name)) => (KeyModifiers::CONTROL, name),
        Some(("shift", "tab")) => {
            return Some(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        }
        Some(_) => return None,
        None => (KeyModifiers::NONE, key),
    };
    let code = match name {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "end" => KeyCode::End,
        "↑" => KeyCode::Up,
        name if name.chars().count() == 1 => KeyCode::Char(name.chars().next()?),
        _ => return None,
    };
    Some(KeyEvent::new(code, modifiers))
}

/// The agent says how much it may do only through its modes, offering no
/// permissions to pick (most ACP agents: `default`, `accept-edits`,
/// `plan`), so asking for its permission picks its mode.
pub(crate) fn modes_stand_for_permissions(view: &ui_view::SettingsView) -> bool {
    view.changeable.permission != Changeable::Pick && view.changeable.mode == Changeable::Pick
}

/// What Shift+Tab changes: the next mode where the agent offers modes;
/// else its own next permission where it only cycles; else the next
/// settable permission that still asks before acting. None where neither
/// can change from here.
pub(crate) fn next_control(view: &ui_view::SettingsView) -> Option<ui_view::SettingChange> {
    let modes: Vec<(&str, bool, bool)> = view
        .modes
        .iter()
        .filter(|mode| mode.settable)
        .map(|mode| (mode.value.as_str(), mode.current, mode.normal))
        .collect();
    if view.changeable.mode == Changeable::Pick
        && let Some(value) = next_of(&modes)
    {
        return Some(ui_view::SettingChange::Mode(value));
    }
    match view.changeable.permission {
        Changeable::Cycle => return Some(ui_view::SettingChange::CyclePermission),
        Changeable::Pick => {}
        _ => return None,
    }
    let permissions: Vec<(&str, bool, bool)> = view
        .permissions
        .iter()
        .filter(|permission| permission.settable && (!permission.never_asks || permission.current))
        .map(|permission| {
            (
                permission.value.as_str(),
                permission.current,
                permission.normal,
            )
        })
        .collect();
    next_of(&permissions).map(ui_view::SettingChange::Permission)
}

/// The value after the current one, (value, current, normal) in order,
/// counting from the normal one when none is current; None with fewer than
/// two.
fn next_of(offered: &[(&str, bool, bool)]) -> Option<String> {
    if offered.len() < 2 {
        return None;
    }
    let at = offered
        .iter()
        .position(|(_, current, _)| *current)
        .or_else(|| offered.iter().position(|(_, _, normal)| *normal));
    let next = at.map_or(0, |at| (at + 1) % offered.len());
    Some(offered[next].0.to_owned())
}
