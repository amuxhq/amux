//! What each kind of agent can do, for the kinds whose every agent can do
//! the same: Claude's two and Codex. An ACP agent's depend on what its
//! program says at `initialize` and on its known ways, so its interpreter
//! and its host's catalogue helper fill them ([`AgentCapabilities::acp`]).
//!
//! These live beside the wire rather than in each interpreter because a
//! host's daemon says them for the providers it offers, and the daemon
//! links no interpreter. Each is built on the host it describes: Codex's
//! terminal depends on the host's platform.

use crate::{AgentCapabilities, Kind};

impl AgentCapabilities {
    /// What every agent of `kind` can do; None for an ACP agent, whose
    /// program decides, and for a kind this build does not know.
    pub fn of_kind(kind: Kind) -> Option<Self> {
        match kind {
            Kind::ClaudePty => Some(Self::claude_pty()),
            Kind::ClaudeSdk => Some(Self::claude_sdk()),
            Kind::Codex => Some(Self::codex()),
            Kind::Acp | Kind::Unspecified => None,
        }
    }

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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind whose agents are alike says what it can do; an ACP
    /// agent's come from its program.
    #[test]
    fn the_fixed_kinds_say_what_they_can_do() {
        assert_eq!(
            AgentCapabilities::of_kind(Kind::ClaudePty),
            Some(AgentCapabilities::claude_pty())
        );
        assert_eq!(
            AgentCapabilities::of_kind(Kind::ClaudeSdk),
            Some(AgentCapabilities::claude_sdk())
        );
        assert_eq!(
            AgentCapabilities::of_kind(Kind::Codex),
            Some(AgentCapabilities::codex())
        );
        assert_eq!(AgentCapabilities::of_kind(Kind::Acp), None);
        assert_eq!(AgentCapabilities::of_kind(Kind::Unspecified), None);
        let unknown = AgentCapabilities::acp(false, false, false);
        assert!(!unknown.resumable && !unknown.images && !unknown.sends_now && !unknown.forks);
        assert!(unknown.files && unknown.passes_unknown_commands);
    }
}
