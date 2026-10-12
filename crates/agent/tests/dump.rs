//! The agent's part of a dump, asked for over ctl.sock: the facts ring,
//! the checkpoint at each segment's start and the specs, with the secrets
//! the provider, the person and the spec carried taken out by the kind's
//! own redactor.

mod support;

use agent::ExitCause;
use prost::Message as _;
use provider_fakes::Step;
use support::*;
use wire::{AgentSpec, Kind};

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
        assert!(
            names.contains(&"facts/0000000000"),
            "the ring's segment: {names:?}"
        );
        assert!(
            names.contains(&"facts/0000000000.checkpoint"),
            "the checkpoint at its start: {names:?}"
        );
        assert!(names.contains(&"spec.1"), "the spec: {names:?}");
        assert!(!names.contains(&agent::DUMP_ERRORS), "{names:?}");

        let hex = interpret::to_hex(PLANTED.as_bytes());
        for file in &part.files {
            let text = String::from_utf8_lossy(&file.contents);
            assert!(
                !text.contains(PLANTED) && !text.contains(&hex),
                "{} keeps a planted secret:\n{text}",
                file.name
            );
        }

        let facts = part
            .files
            .iter()
            .find(|file| file.name == "facts/0000000000")
            .unwrap();
        let facts = String::from_utf8(facts.contents.clone()).unwrap();
        assert!(facts.contains(r#""event":"input""#), "{facts}");
        assert!(facts.contains("the key is"), "text around a secret stays");
        assert!(!facts.contains("unreadable"), "{facts}");

        let spec = part
            .files
            .iter()
            .find(|file| file.name == "spec.1")
            .unwrap();
        let spec = AgentSpec::decode(spec.contents.as_slice()).expect("the spec stays a spec");
        assert_eq!(spec.kind(), kind);
        assert!(
            spec.provider_env.contains_key("OPENAI_API_KEY"),
            "the name stays, the value goes"
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
