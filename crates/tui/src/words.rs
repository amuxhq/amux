//! Words for what the agent reports, in the phone's words (its
//! `ChatWords`): the shared views carry facts, and each client words them.

use ui_view::{Changeable, ControlsSummary, ModeChoice, PermissionChoice};

/// A permission or mode by the name a person reads, lowercase as the
/// terminal's own words are: the catalogue's name, else the provider's
/// value as it is.
pub(crate) fn named(display_name: &str, value: &str) -> String {
    if display_name.is_empty() {
        value.to_owned()
    } else {
        display_name.to_lowercase()
    }
}

/// A permission as the terminal names it: a catalogue's name lowercase,
/// as the terminal's own words are, a provider's value as it is, and Codex
/// settings that match no named permission "custom".
pub(crate) fn permission_words(permission: &PermissionChoice) -> String {
    if permission.custom {
        "custom".to_owned()
    } else {
        lowercase_name(&permission.display_name, &permission.value)
    }
}

/// A mode as the terminal names it, as a permission is.
pub(crate) fn mode_words(mode: &ModeChoice) -> String {
    lowercase_name(&mode.display_name, &mode.value)
}

fn lowercase_name(name: &str, value: &str) -> String {
    if name == value {
        name.to_owned()
    } else {
        name.to_lowercase()
    }
}

/// The permission and mode as the composer's edge names them, "full access
/// · plan": the view hands over only those that are not the agent's normal
/// ones.
pub(crate) fn control_words(controls: &ControlsSummary) -> Option<String> {
    let words: Vec<String> = controls
        .permission
        .iter()
        .map(permission_words)
        .chain(controls.mode.iter().map(mode_words))
        .collect();
    (!words.is_empty()).then(|| words.join(" · "))
}

/// How a setting that changes only by typing the agent's own command is
/// changed, "type /model <name> in the composer"; both when model and
/// effort do.
pub(crate) fn by_typing_words(model: &Changeable, effort: &Changeable) -> Option<String> {
    match (model, effort) {
        (Changeable::ByTyping(model), Changeable::ByTyping(effort)) => Some(format!(
            "To change the model or effort, type {model} <name> or {effort} <level> in the composer."
        )),
        (Changeable::ByTyping(model), _) => Some(format!(
            "To change the model, type {model} <name> in the composer."
        )),
        (_, Changeable::ByTyping(effort)) => Some(format!(
            "To change the effort, type {effort} <level> in the composer."
        )),
        _ => None,
    }
}

/// Why a typed name will not do.
pub(crate) fn name_problem(problem: wire::AgentNameProblem) -> String {
    match problem {
        wire::AgentNameProblem::Empty => "a name cannot be empty".to_owned(),
        wire::AgentNameProblem::TooLong => {
            format!("a name is at most {} characters", wire::AGENT_NAME_MOST)
        }
        wire::AgentNameProblem::Characters => {
            // Short enough to sit beside its way out on an 80-column hint
            // line.
            "a name is a-z, 0-9 and hyphens, starting with a letter or digit".to_owned()
        }
    }
}
