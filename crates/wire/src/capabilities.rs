//! What each kind of agent can do, for the kinds whose every agent can do
//! the same: Claude's two and Codex. An ACP agent's depend on what its
//! program says at `initialize` and on its known ways, so its interpreter
//! and its host's catalogue helper fill them ([`AgentCapabilities::acp`]).
//!
//! These live beside the wire rather than in each interpreter because a
//! host's daemon says them for the providers it offers, and the daemon
//! links no interpreter. They depend on the platform of the host the agent
//! runs on (Codex's terminal does), which each caller passes: the agent and
//! its host pass their own ([`Platform::here`]), and a client describing
//! another host's agent passes that host's, never the platform it was built
//! for itself.

use crate::{AgentCapabilities, Kind, Platform};

impl AgentCapabilities {
    /// What every agent of `kind` can do on a host running `platform`; None
    /// for an ACP agent, whose program decides, and for a kind this build
    /// does not know.
    pub fn of_kind(kind: Kind, platform: Platform) -> Option<Self> {
        match kind {
            Kind::ClaudePty => Some(Self::claude_pty()),
            Kind::ClaudeSdk => Some(Self::claude_sdk()),
            Kind::Codex => Some(Self::codex(platform)),
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

    /// Codex's app server on a host running `platform`. It steers a running
    /// turn and stops one job, and its own terminal interface attaches to
    /// the same thread, except on Windows, where the app server runs on
    /// stdio with room for amux alone. It refuses a slash command it does
    /// not list.
    ///
    /// A platform nobody said (a host from before hosts said theirs) reads
    /// as not Windows: attachable, as every reader before this read a host
    /// other than its own Windows one.
    pub fn codex(platform: Platform) -> Self {
        Self {
            stops_job: true,
            switches_back: false,
            attachable: platform != Platform::Windows,
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

impl Kind {
    /// Whether a host running `platform` starts agents of this kind:
    /// terminal Claude needs a terminal of its own, which amux gives it
    /// only off Windows; the others run everywhere. A platform nobody said
    /// reads as not Windows.
    pub fn hosted_on(self, platform: Platform) -> bool {
        match self {
            Kind::ClaudePty => platform != Platform::Windows,
            Kind::ClaudeSdk | Kind::Codex | Kind::Acp => true,
            Kind::Unspecified => false,
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
        for platform in [Platform::Macos, Platform::Linux, Platform::Windows] {
            assert_eq!(
                AgentCapabilities::of_kind(Kind::ClaudePty, platform),
                Some(AgentCapabilities::claude_pty())
            );
            assert_eq!(
                AgentCapabilities::of_kind(Kind::ClaudeSdk, platform),
                Some(AgentCapabilities::claude_sdk())
            );
            assert_eq!(
                AgentCapabilities::of_kind(Kind::Codex, platform),
                Some(AgentCapabilities::codex(platform))
            );
            assert_eq!(AgentCapabilities::of_kind(Kind::Acp, platform), None);
            assert_eq!(
                AgentCapabilities::of_kind(Kind::Unspecified, platform),
                None
            );
        }
        let unknown = AgentCapabilities::acp(false, false, false);
        assert!(!unknown.resumable && !unknown.images && !unknown.sends_now && !unknown.forks);
        assert!(unknown.files && unknown.passes_unknown_commands);
    }

    /// What a reader says of a host's Codex depends on that host's
    /// platform and nothing of the reader's: a client built for macOS,
    /// describing a Windows host, says its Codex has no terminal to attach
    /// (it runs on stdio there), and the same call says a Mac's has one.
    #[test]
    fn codex_attaches_on_every_host_but_windows_whoever_reads() {
        let attachable = |platform| {
            AgentCapabilities::of_kind(Kind::Codex, platform)
                .unwrap()
                .attachable
        };
        assert!(!attachable(Platform::Windows));
        assert!(attachable(Platform::Macos));
        assert!(attachable(Platform::Linux));
        // An older host that never said its platform reads as not Windows.
        assert!(attachable(Platform::Unspecified));
        // Nothing else of Codex's depends on where it runs.
        assert_eq!(
            AgentCapabilities {
                attachable: true,
                ..AgentCapabilities::codex(Platform::Windows)
            },
            AgentCapabilities::codex(Platform::Macos)
        );
    }

    /// Terminal Claude is hosted everywhere but Windows; the others
    /// everywhere.
    #[test]
    fn terminal_claude_is_hosted_off_windows_only() {
        assert!(!Kind::ClaudePty.hosted_on(Platform::Windows));
        assert!(Kind::ClaudePty.hosted_on(Platform::Macos));
        assert!(Kind::ClaudePty.hosted_on(Platform::Linux));
        for kind in [Kind::ClaudeSdk, Kind::Codex, Kind::Acp] {
            assert!(kind.hosted_on(Platform::Windows) && kind.hosted_on(Platform::Macos));
        }
        assert!(!Kind::Unspecified.hosted_on(Platform::Macos));
    }

    /// Every platform amux is tested on names itself.
    #[test]
    fn here_is_named_where_tests_run() {
        assert_ne!(Platform::here(), Platform::Unspecified);
    }
}
