//! View data goldens for all three kinds, derived from every interpreter
//! fixture: the interpreter's emission is committed the way the daemon
//! commits it (an order once per new key, a revision per record), reduced
//! into a session. Each frame records where the ask card and the composer
//! moved; the rows, the overview, the context and the settings are
//! recorded once, as the fixture leaves them. The rows' streaming rules
//! are held by the session spec and the views tests, not frame by frame
//! here.
//!
//! Goldens live in `tests/goldens/<kind>/<fixture>.golden` and are only
//! rewritten with `UI_VIEW_UPDATE_GOLDENS=1`; review the diff.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::{Interpreter, Replayed, replay};
use support::Committer;
use ui_state::{InputOutcome, Msg, SessionState};
use ui_view::{
    ToolRows, ask_card, composer, context, overview, prompts_underway, queue_rows, refused_prompts,
    settings, sign_in,
};
use wire::{Kind, SessionEvent, session_event};

use crate::support;

const UPDATE: &str = "UI_VIEW_UPDATE_GOLDENS";

fn fixtures(kind: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../interpret/fixtures")
        .join(kind)
}

fn goldens(kind: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens")
        .join(kind)
}

fn agent(kind: Kind) -> wire::Agent {
    wire::Agent {
        agent_id: b"agent".to_vec(),
        host_id: b"host".to_vec(),
        kind: kind as i32,
        name: "worker".into(),
        lifecycle: wire::Lifecycle::Live as i32,
        phase: wire::Phase::NeedsYou as i32,
        incarnation: 1,
        ..wire::Agent::default()
    }
}

fn clip(line: String) -> String {
    const MAX: usize = 400;
    if line.chars().count() <= MAX {
        return line;
    }
    let mut clipped: String = line.chars().take(MAX).collect();
    clipped.push('…');
    clipped
}

/// A question row's answers, ahead of the clipped row so they stay legible.
fn answered(kind: &ui_view::RowKind) -> String {
    let ui_view::RowKind::Ask(ui_view::AskRow::Questions {
        questions,
        answers,
        skipped,
        reply,
        resolution,
    }) = kind
    else {
        return String::new();
    };
    let answers: Vec<String> = questions
        .iter()
        .zip(answers)
        .map(|(question, answer)| {
            let mut said = answer.picked.join(", ");
            if let Some(other) = &answer.other {
                if !said.is_empty() {
                    said.push_str(", ");
                }
                let _ = write!(said, "{other:?}");
            }
            if answer.hidden {
                said.push_str("(hidden)");
            }
            if answer.skipped {
                said.push_str("(skipped)");
            }
            if let Some(note) = &answer.note {
                let _ = write!(said, " note={note:?}");
            }
            format!("{} = {said}", question.header)
        })
        .collect();
    let reply = reply
        .as_ref()
        .map(|reply| format!(" reply={reply:?}"))
        .unwrap_or_default();
    format!(
        "[{resolution:?}: {} skipped={skipped}{reply}] ",
        answers.join("; ")
    )
}

fn describe_row(row: &ui_view::Row) -> String {
    let mut line = format!(
        "{:>4} {} {}{:?}",
        row.order,
        row.id,
        answered(&row.kind),
        row.kind
    );
    if let Some(run) = &row.run {
        let _ = write!(
            line,
            " run[{}..{} x{}{}{}{}{}{}]",
            run.first,
            run.last,
            run.steps,
            run.recent
                .map(|after| format!(" recent={after}"))
                .unwrap_or_default(),
            if run.live { " live" } else { "" },
            if run.open_below { " open_below" } else { "" },
            if run.unresolved_failure {
                " unresolved"
            } else {
                ""
            },
            run.counts
                .as_ref()
                .map(|counts| format!(" {counts:?}"))
                .unwrap_or_default(),
        );
    }
    if let Some(decision) = &row.decision {
        let _ = write!(line, " decision={decision:?}");
    }
    if row.collapsed {
        line.push_str(" collapsed");
    }
    if row.attention {
        line.push_str(" attention");
    }
    if let Some(parent) = &row.parent {
        let _ = write!(line, " parent={parent}");
    }
    clip(line)
}

fn describe_card(card: Option<&ui_view::AskCard>) -> String {
    let Some(card) = card else {
        return "  ask none".into();
    };
    let choices: Vec<String> = card
        .choices
        .iter()
        .map(|choice| {
            format!(
                "{:?}{}{}",
                choice.outcome,
                if choice.primary { "*" } else { "" },
                if choice.takes_note { "+note" } else { "" }
            )
        })
        .collect();
    format!(
        "  ask {} -> {:?} {}/{} {:?}\n    choices {}\n    body {}",
        card.key,
        card.item_key,
        card.position,
        card.count,
        card.state,
        choices.join(", "),
        clip(format!("{:?}", card.body))
    )
}

/// Replays a fixture's emission through the session and renders the views.
fn render(kind: Kind, frames: &[Replayed]) -> String {
    let mut state = SessionState::new(agent(kind), support::CAP);
    let mut committer = Committer::default();
    let mut out = String::new();
    let open = HashSet::new();
    let tools = ToolRows::Collapse { open: &open };
    let mut last_card = None;
    let mut last_composer = String::new();
    for (index, frame) in frames.iter().enumerate() {
        let mut lines = Vec::new();
        if let Some(input) = &frame.input {
            state.update(Msg::Send(input.clone()));
        }
        for (id, verdict) in &frame.replies {
            state.update(Msg::Sent(id.clone(), InputOutcome::Reply(verdict.clone())));
        }
        for event in committer.commit(&frame.step) {
            let outcome = state.update(Msg::Event(event));
            assert!(
                outcome.need_get.is_none(),
                "a committed append always has its base"
            );
        }
        if index == 0 {
            state.update(Msg::Event(SessionEvent {
                of: Some(session_event::Of::CaughtUp(wire::CaughtUp {
                    revision: committer.revision,
                })),
            }));
        }
        // The catalogue as a client fetches it once the snapshot names it.
        if let Some((hash, catalogue)) = &frame.catalogue {
            state.update(Msg::Catalogue {
                hash: hash.clone(),
                catalogue: catalogue.clone(),
            });
        }
        let card = ask_card(&state);
        if card != last_card {
            lines.push(describe_card(card.as_ref()));
            last_card = card;
        }
        let at = frame
            .step
            .items
            .iter()
            .map(|item| item.at_ms)
            .max()
            .unwrap_or(0);
        // Placed as if each prompt was first drawn in the queue: where a
        // prompt is drawn is the client's memory, not the session's.
        let composing = format!(
            "  composer {:?} activity={:?} queue={:?} underway={:?} refused={:?}",
            composer(&state, at).mode,
            state.activity(at).map(|activity| activity.kind),
            queue_rows(&state),
            prompts_underway(&state, |_| false),
            refused_prompts(&state)
        );
        if composing != last_composer {
            lines.push(clip(composing.clone()));
            last_composer = composing;
        }
        if !lines.is_empty() {
            let _ = writeln!(out, "## {index} {}", clip(frame.label.clone()));
            for line in lines {
                let _ = writeln!(out, "{line}");
            }
        }
    }
    let _ = writeln!(out, "== chat");
    for row in crate::support::all_rows(&state, tools) {
        let _ = writeln!(out, "  {}", describe_row(&row));
    }
    let _ = writeln!(out, "== overview {:?}", overview(&state, None));
    let _ = writeln!(
        out,
        "== context {:?} sign-in {:?}",
        context(&state),
        sign_in(&state)
    );
    describe_settings(&mut out, &state);
    out
}

/// The settings view, one choice per line: the current one starred, one
/// the catalogue does not list marked unlisted; how each setting changes;
/// and what the composer says of them.
fn describe_settings(out: &mut String, state: &SessionState) {
    let view = settings(state);
    let marks = |current: bool, unlisted: bool| {
        format!(
            "{}{}",
            if current { "*" } else { " " },
            if unlisted { " unlisted" } else { "" }
        )
    };
    let _ = writeln!(out, "== settings");
    for model in &view.models {
        let _ = writeln!(
            out,
            "  model{} {} {:?} efforts=[{}]{}",
            marks(model.current, model.unlisted),
            model.value,
            model.display_name,
            model.efforts.join(","),
            model
                .default_effort
                .as_ref()
                .map(|effort| format!(" default={effort}"))
                .unwrap_or_default()
        );
    }
    for effort in &view.efforts {
        let _ = writeln!(
            out,
            "  effort{} {}{}",
            marks(effort.current, effort.unlisted),
            effort.value,
            if effort.default { " (default)" } else { "" }
        );
    }
    for permission in &view.permissions {
        let _ = writeln!(
            out,
            "  permission{}{} {:?} {:?}{}{}{}",
            marks(permission.current, permission.unlisted),
            if permission.custom { " custom" } else { "" },
            permission.value,
            permission.display_name,
            if permission.normal { " normal" } else { "" },
            if permission.never_asks {
                " never_asks"
            } else {
                ""
            },
            if permission.settable {
                ""
            } else {
                " unsettable"
            }
        );
    }
    for mode in &view.modes {
        let _ = writeln!(
            out,
            "  mode{} {:?} {:?}{}{}",
            marks(mode.current, mode.unlisted),
            mode.value,
            mode.display_name,
            if mode.normal { " normal" } else { "" },
            if mode.settable { "" } else { " unsettable" }
        );
    }
    let _ = writeln!(
        out,
        "  changes: model {:?} · effort {:?} · permission {:?} · mode {:?}",
        view.changeable.model,
        view.changeable.effort,
        view.changeable.permission,
        view.changeable.mode
    );
    for command in &view.commands {
        let _ = writeln!(
            out,
            "  command /{} source={:?} hint={:?} {}",
            command.name,
            command.source,
            command.argument_hint,
            clip(format!("{:?}", command.description))
        );
    }
    let controls = ui_view::controls(state);
    let _ = writeln!(
        out,
        "  controls: model={:?} effort={:?} permission={:?} mode={:?}",
        controls.model,
        controls.effort,
        controls.permission.map(|permission| if permission.custom {
            "custom".to_owned()
        } else {
            permission.value
        }),
        controls.mode.map(|mode| mode.value)
    );
}

fn check(
    kind: Kind,
    dir: &str,
    name: &str,
    frames: Result<Vec<Replayed>, String>,
) -> Option<String> {
    let frames = match frames {
        Ok(frames) => frames,
        Err(error) => return Some(format!("{dir}/{name}: {error}")),
    };
    let rendered = render(kind, &frames);
    let path = goldens(dir).join(format!("{name}.golden"));
    if std::env::var(UPDATE).as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &rendered).unwrap();
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(golden) if golden == rendered => None,
        Ok(golden) => {
            let at = golden
                .lines()
                .zip(rendered.lines())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| golden.lines().count().min(rendered.lines().count()));
            Some(format!(
                "{dir}/{name}: golden differs at line {}\n  golden:   {}\n  rendered: {}",
                at + 1,
                golden.lines().nth(at).unwrap_or("<end>"),
                rendered.lines().nth(at).unwrap_or("<end>")
            ))
        }
        Err(_) => Some(format!(
            "{dir}/{name}: no golden; run with {UPDATE}=1 and review"
        )),
    }
}

fn run_all<I: Interpreter>(kind: Kind, dir: &str) {
    let mut failures = Vec::new();
    let mut names: Vec<String> = std::fs::read_dir(fixtures(dir))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "json")
                .then(|| path.file_stem().unwrap().to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    assert!(!names.is_empty());
    let mut expected = HashSet::new();
    for name in &names {
        let path = fixtures(dir).join(format!("{name}.json"));
        let frames = replay::<I>(&path);
        expected.insert(format!("{name}.golden"));
        failures.extend(check(kind, dir, name, frames));
    }
    // A golden whose fixture is gone is stale.
    if let Ok(entries) = std::fs::read_dir(goldens(dir)) {
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if !expected.contains(&file) {
                failures.push(format!("{dir}/{file}: golden has no fixture"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn claude_pty_view_goldens() {
    run_all::<ClaudePty>(Kind::ClaudePty, "claude_pty");
}

#[test]
fn claude_sdk_view_goldens() {
    run_all::<ClaudeSdk>(Kind::ClaudeSdk, "claude_sdk");
}

#[test]
fn codex_view_goldens() {
    run_all::<Codex>(Kind::Codex, "codex");
}
