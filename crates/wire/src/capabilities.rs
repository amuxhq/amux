//! What each kind of agent can do, for the kinds whose every agent can do
//! the same: Claude's two and Codex. An ACP agent's depend on what its
//! program says at `initialize` and on its known ways, so its interpreter
//! and its host's catalogue helper fill them ([`AgentCapabilities::acp`]).
//!
//! These live beside the wire rather than in each interpreter because a
//! host's daemon says them for the providers it offers, and the daemon
//! links no interpreter. Each is built on the host it describes: Codex's
//! terminal depends on the host's platform.

use crate::{AgentCapabilities, SettingChange, SettingControl};

impl AgentCapabilities {
    /// Claude Code in its own terminal. amux types into that terminal, so
    /// a queued prompt can be typed into a running turn and a slash command
    /// amux does not know is Claude's to run; attachments go as paths
    /// Claude reads. It has no input that stops one background job, and its
    /// view can be switched to another conversation and back.
    pub fn claude_pty() -> Self {
        Self {
            stops_job: false,
            switches_back: true,
            attachable: true,
            resumable: true,
            sends_now: true,
            images: true,
            files: true,
            passes_unknown_commands: true,
            forks: true,
            // Terminal Claude takes no setting input: it runs its own
            // commands, typed as any prompt, and steps its permission with
            // its own cycle key.
            model: Some(SettingChange::command("/model")),
            effort: Some(SettingChange::command("/effort")),
            permission: Some(SettingChange::step()),
            mode: None,
        }
    }

    /// Claude Code headless, behind amux's chat: it stops one job by its
    /// id, takes a message into its running turn, and has no terminal.
    pub fn claude_sdk() -> Self {
        Self {
            stops_job: true,
            switches_back: false,
            attachable: false,
            resumable: true,
            sends_now: true,
            images: true,
            files: true,
            passes_unknown_commands: true,
            forks: true,
            model: Some(SettingChange::pick()),
            effort: Some(SettingChange::pick()),
            permission: Some(SettingChange::pick()),
            mode: None,
        }
    }

    /// Codex's app server. It steers a running turn and stops one job, and
    /// its own terminal interface attaches to the same thread, except on
    /// Windows, where the app server runs on stdio with room for amux
    /// alone. It refuses a slash command it does not list.
    pub fn codex() -> Self {
        Self {
            stops_job: true,
            switches_back: false,
            attachable: !cfg!(windows),
            resumable: true,
            sends_now: true,
            images: true,
            files: true,
            passes_unknown_commands: false,
            forks: true,
            model: Some(SettingChange::pick()),
            effort: Some(SettingChange::pick()),
            permission: Some(SettingChange::pick()),
            mode: Some(SettingChange::pick()),
        }
    }

    /// An ACP agent: `resumable` when its `initialize` answer offered to
    /// resume or load a session (unknown counts as no), `images` when its
    /// prompt capabilities take them, and `sends_now` only for an agent
    /// known to fold a prompt into its running turn. Files always go, as a
    /// reference in the prompt's text and embedded where the agent takes
    /// that. ACP has no input that stops one job, no terminal of its own
    /// for amux to attach, and a prompt's slash command is the agent's to
    /// read. It forks only where its `initialize` answer offered
    /// `session/fork`, which the caller says by setting `forks`.
    pub fn acp(resumable: bool, images: bool, sends_now: bool) -> Self {
        Self {
            stops_job: false,
            switches_back: false,
            attachable: false,
            resumable,
            sends_now,
            images,
            files: true,
            passes_unknown_commands: true,
            forks: false,
            // Each takes a pick of what the agent's catalogue lists, which
            // may be nothing.
            model: Some(SettingChange::pick()),
            effort: Some(SettingChange::pick()),
            permission: Some(SettingChange::pick()),
            mode: Some(SettingChange::pick()),
        }
    }
}

impl SettingChange {
    /// A Set input naming a value the catalogue lists.
    pub fn pick() -> Self {
        Self {
            control: SettingControl::Pick as i32,
            command: String::new(),
        }
    }

    /// One step input to the next value.
    pub fn step() -> Self {
        Self {
            control: SettingControl::Step as i32,
            command: String::new(),
        }
    }

    /// Typed as `command` and the value.
    pub fn command(command: &str) -> Self {
        Self {
            control: SettingControl::Command as i32,
            command: command.to_owned(),
        }
    }
}

impl AgentCapabilities {
    /// How a client changes each setting, unset ones as not offered.
    pub fn model_change(&self) -> SettingControl {
        self.model
            .as_ref()
            .map_or(SettingControl::Unspecified, SettingChange::control)
    }

    pub fn effort_change(&self) -> SettingControl {
        self.effort
            .as_ref()
            .map_or(SettingControl::Unspecified, SettingChange::control)
    }

    pub fn permission_change(&self) -> SettingControl {
        self.permission
            .as_ref()
            .map_or(SettingControl::Unspecified, SettingChange::control)
    }

    pub fn mode_change(&self) -> SettingControl {
        self.mode
            .as_ref()
            .map_or(SettingControl::Unspecified, SettingChange::control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ACP agent whose program has said nothing can do only what every
    /// ACP agent can.
    #[test]
    fn an_acp_agent_that_said_nothing_can_do_the_least() {
        let unknown = AgentCapabilities::acp(false, false, false);
        assert!(!unknown.resumable && !unknown.images && !unknown.sends_now && !unknown.forks);
        assert!(unknown.files && unknown.passes_unknown_commands);
    }
}
