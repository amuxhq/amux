//! The agent's part of a dump, asked for over ctl.sock: its record alone,
//! which carries none of what the provider, the person or the spec said.

use agent::ExitCause;
use provider_fakes::Step;
use support::*;
use wire::Kind;

use crate::support;

/// Every planted secret contains this, so one search finds any survivor.
const PLANTED: &str = "PLANTED";

fn dumps_without_its_secrets(kind: Kind) {
    terminal_test(async move {
        let agent = Agent::start(Setup {
            kind,
            env: vec![("OPENAI_API_KEY", "env-PLANTED-4f1c9a")],
            steps: vec![
                Step::Text {
                    chunks: vec!["the key is sk-proj-PLANTEDreply0123456789abcdef".into()],
                },
                Step::TurnEnd,
            ],
            ..Setup::sdk()
        })
        .await;
        let mut daemon = agent.dial().await;
        agent.ready().await;
        assert_eq!(
            daemon
                .prompt(b"p1", "use ghp_PLANTEDprompt0123456789abcdefABCDEF")
                .await,
            Verdict::Accepted
        );
        agent
            .wait("the turn ends", |log| log.turn_ends() == 1)
            .await;
        assert!(
            agent.log().has_text(PLANTED),
            "the journal itself is not redacted: the secret is really there"
        );

        let part = daemon.dump(b"d1").await;
        let names: Vec<&str> = part.files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(names, [agent::RECORD], "the record alone");

        let hex = interpret::to_hex(PLANTED.as_bytes());
        let text = String::from_utf8_lossy(&part.files[0].contents);
        assert!(
            !text.contains(PLANTED) && !text.contains(&hex),
            "the record keeps a planted secret:\n{text}"
        );
        let record: serde_json::Value =
            serde_json::from_slice(&part.files[0].contents).expect("the record is JSON");
        assert!(
            record["segments"]
                .as_array()
                .is_some_and(|segments| !segments.is_empty()),
            "{record}"
        );

        daemon.stop().await;
        assert_eq!(agent.exit().await, ExitCause::Stopped);
    });
}

#[test]
fn a_headless_claude_agent_dumps_without_its_secrets() {
    dumps_without_its_secrets(Kind::ClaudeSdk);
}

// Unix only: Windows does not host terminal Claude (ConPTY re-renders its output; see
// docs/ARCHITECTURE.md, "Windows, as a stated cost").
#[cfg(unix)]
#[test]
fn a_terminal_claude_agent_dumps_without_its_secrets() {
    dumps_without_its_secrets(Kind::ClaudePty);
}

#[test]
fn a_codex_agent_dumps_without_its_secrets() {
    dumps_without_its_secrets(Kind::Codex);
}
