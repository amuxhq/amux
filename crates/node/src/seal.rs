//! Sealing a push to one phone's key, so the account service and the
//! platform's push service carry something they can't read.
//!
//! What is sealed is JSON because the reader is a small extension on the phone
//! with no protobuf or Rust in it. HPKE in base mode needs only the phone's
//! public key, and the sealed form is the encapsulated key, then the ciphertext.

use base64::Engine as _;
use daemon_core::payload::Push;
use hpke::aead::ChaCha20Poly1305;
use hpke::kdf::HkdfSha256;
use hpke::kem::X25519HkdfSha256;
use hpke::{Deserializable as _, OpModeS, Serializable as _};
use prost::Message as _;
use serde::Serialize;
use uuid::Uuid;
use wire::Said;

/// Binds a sealed push to what it is for. The phone opens it with the same words.
const INFO: &[u8] = b"amux push 1";

/// What the account service takes for one recipient, base64 included: past it
/// the platform's few kilobytes run out.
const MOST_SEALED_CHARS: usize = 3000;

#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("the push key can't be read")]
    Key,
    #[error("the notice can't be read: {0}")]
    Notice(String),
    #[error("sealing failed: {0}")]
    Seal(String),
    #[error("the notice is too long to send, even without its reason")]
    TooLong,
}

#[derive(Debug, Serialize)]
struct Sealed<'a> {
    host: String,
    agent: String,
    name: &'a str,
    notice: Said,
}

/// The same notice without its reason, the first thing to go when a push is
/// too long: what the agent wants still says what it needs.
fn without_why(said: Said) -> Said {
    match said {
        Said::Ask {
            key, wants, more, ..
        } => Said::Ask {
            key,
            wants,
            why: String::new(),
            more,
        },
        said => said,
    }
}

fn seal_bytes(plain: &[u8], push_key: &[u8]) -> Result<String, SealError> {
    let key = <X25519HkdfSha256 as hpke::Kem>::PublicKey::from_bytes(push_key)
        .map_err(|_| SealError::Key)?;
    let (encapsulated, ciphertext) = hpke::single_shot_seal::<
        ChaCha20Poly1305,
        HkdfSha256,
        X25519HkdfSha256,
    >(&OpModeS::Base, &key, INFO, plain, b"")
    .map_err(|error| SealError::Seal(error.to_string()))?;
    let mut sealed = encapsulated.to_bytes().to_vec();
    sealed.extend_from_slice(&ciphertext);
    Ok(base64::engine::general_purpose::STANDARD.encode(sealed))
}

fn uuid(bytes: &[u8]) -> Result<String, SealError> {
    Uuid::from_slice(bytes)
        .map(|id| id.to_string())
        .map_err(|error| SealError::Notice(error.to_string()))
}

/// `push`, sealed to one recipient's public key, as base64.
pub(crate) fn seal_push(push: &Push, push_key: &[u8]) -> Result<String, SealError> {
    let notice = wire::Notice::decode(push.notice.as_slice())
        .map_err(|error| SealError::Notice(error.to_string()))?;
    let said = Said::of(notice).ok_or_else(|| SealError::Notice("it says nothing".to_owned()))?;
    let (host, agent) = (uuid(&push.host_id)?, uuid(&push.agent_id)?);
    let plain = |notice: Said| -> Result<Vec<u8>, SealError> {
        serde_json::to_vec(&Sealed {
            host: host.clone(),
            agent: agent.clone(),
            name: &push.name,
            notice,
        })
        .map_err(|error| SealError::Notice(error.to_string()))
    };
    let whole = seal_bytes(&plain(said.clone())?, push_key)?;
    if whole.len() <= MOST_SEALED_CHARS {
        return Ok(whole);
    }
    let shorter = seal_bytes(&plain(without_why(said))?, push_key)?;
    if shorter.len() <= MOST_SEALED_CHARS {
        Ok(shorter)
    } else {
        Err(SealError::TooLong)
    }
}

#[cfg(test)]
mod tests {
    use hpke::{Kem as _, OpModeR};
    use wire::{AskNotice, Notice, RunWant, ask_notice, blocked_notice, notice};

    use super::*;

    fn open(
        sealed: &str,
        private: &<X25519HkdfSha256 as hpke::Kem>::PrivateKey,
    ) -> serde_json::Value {
        let sealed = base64::engine::general_purpose::STANDARD
            .decode(sealed)
            .unwrap();
        let (encapsulated, ciphertext) = sealed.split_at(32);
        let encapsulated =
            <X25519HkdfSha256 as hpke::Kem>::EncappedKey::from_bytes(encapsulated).unwrap();
        let plain = hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(
            &OpModeR::Base,
            private,
            &encapsulated,
            INFO,
            ciphertext,
            b"",
        )
        .unwrap();
        serde_json::from_slice(&plain).unwrap()
    }

    fn push(command: &str, why: &str) -> Push {
        Push {
            host_id: vec![1; 16],
            agent_id: vec![2; 16],
            revision: 7,
            name: "asker".to_owned(),
            notice: Notice {
                of: Some(notice::Of::Ask(AskNotice {
                    key: "ask:1".to_owned(),
                    wants: Some(ask_notice::Wants::Run(RunWant {
                        command: command.to_owned(),
                    })),
                    why: why.to_owned(),
                    more: 1,
                })),
            }
            .encode_to_vec(),
            recipients: Vec::new(),
        }
    }

    #[test]
    fn a_sealed_push_opens_only_with_the_phones_key_and_says_what_is_asked() {
        let (private, public) = X25519HkdfSha256::gen_keypair();
        let sealed = seal_push(&push("deploy --prod", "It needs you"), &public.to_bytes()).unwrap();
        let opened = open(&sealed, &private);
        assert_eq!(opened["name"], "asker");
        assert_eq!(opened["host"], "01010101-0101-0101-0101-010101010101");
        assert_eq!(opened["notice"]["Ask"]["key"], "ask:1");
        assert_eq!(
            opened["notice"]["Ask"]["wants"]["Run"]["command"],
            "deploy --prod"
        );
        assert_eq!(opened["notice"]["Ask"]["more"], 1);
    }

    #[test]
    fn a_push_too_long_to_send_loses_its_reason_first() {
        let (private, public) = X25519HkdfSha256::gen_keypair();
        let sealed = seal_push(&push("deploy", &"why ".repeat(1000)), &public.to_bytes()).unwrap();
        assert!(sealed.len() <= MOST_SEALED_CHARS);
        assert_eq!(open(&sealed, &private)["notice"]["Ask"]["why"], "");
    }

    /// The file the phone's own tests open, so one fixture holds the sealing
    /// here and the opening there to the same format.
    fn fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../apps/apple/Packages/AmuxPush/Tests/AmuxPushTests/Fixtures/sealed-pushes.json",
        )
    }

    const FIXTURE_KEY: [u8; 32] = [0x41; 32];

    fn fixture_key() -> <X25519HkdfSha256 as hpke::Kem>::PrivateKey {
        <X25519HkdfSha256 as hpke::Kem>::PrivateKey::from_bytes(&FIXTURE_KEY).unwrap()
    }

    fn fixture_key_text() -> String {
        base64::engine::general_purpose::STANDARD.encode(FIXTURE_KEY)
    }

    fn ask_of(wants: ask_notice::Wants, why: &str, more: u32) -> notice::Of {
        notice::Of::Ask(AskNotice {
            key: "ask:7".to_owned(),
            wants: Some(wants),
            why: why.to_owned(),
            more,
        })
    }

    fn blocked_of(why: blocked_notice::Why) -> notice::Of {
        notice::Of::Blocked(wire::BlockedNotice { why: Some(why) })
    }

    /// Every shape a notice takes, by the name the phone's tests know it by.
    fn fixture_pushes() -> Vec<(&'static str, Push)> {
        use ask_notice::Wants;
        let of = |of: notice::Of| Push {
            host_id: vec![0x11; 16],
            agent_id: vec![0x22; 16],
            revision: 1,
            name: "refactor-auth".to_owned(),
            notice: Notice { of: Some(of) }.encode_to_vec(),
            recipients: Vec::new(),
        };
        let run = |command: &str| {
            Wants::Run(RunWant {
                command: command.to_owned(),
            })
        };
        let change = |path: &str, files| {
            Wants::Change(wire::ChangeWant {
                path: path.to_owned(),
                files,
                created: Some(false),
            })
        };
        let plan = |title: &str| {
            Wants::Plan(wire::PlanWant {
                title: title.to_owned(),
            })
        };
        vec![
            (
                "run",
                of(ask_of(run("deploy --prod\n--force"), "It ships", 0)),
            ),
            ("run and more", of(ask_of(run("deploy --prod"), "", 2))),
            (
                "change one file",
                of(ask_of(change("src/lib.rs", 1), "", 0)),
            ),
            (
                "change many files",
                of(ask_of(change("src/lib.rs", 4), "", 0)),
            ),
            (
                "use a tool",
                of(ask_of(
                    Wants::Use(wire::UseWant {
                        server: "github".to_owned(),
                        tool: "create_issue".to_owned(),
                    }),
                    "",
                    0,
                )),
            ),
            (
                "question",
                of(ask_of(
                    Wants::Question(wire::QuestionWant {
                        text: "Which suite?\nPick one".to_owned(),
                    }),
                    "",
                    0,
                )),
            ),
            (
                "plan with a title",
                of(ask_of(plan("Move the cache"), "", 0)),
            ),
            ("plan without one", of(ask_of(plan(""), "", 0))),
            (
                "the provider's sentence",
                of(ask_of(
                    Wants::Other(wire::OtherWant {
                        text: "Sign in to Linear to go on".to_owned(),
                        server: String::new(),
                    }),
                    "",
                    1,
                )),
            ),
            (
                "signed out",
                of(blocked_of(blocked_notice::Why::SignedOut(
                    wire::SignInState::Failed as i32,
                ))),
            ),
            (
                "usage limit with a reset",
                of(blocked_of(blocked_notice::Why::Usage(wire::UsageBlock {
                    resets_at_ms: Some(1_800_000_000_000),
                }))),
            ),
            (
                "usage limit without one",
                of(blocked_of(blocked_notice::Why::Usage(wire::UsageBlock {
                    resets_at_ms: None,
                }))),
            ),
            (
                "failed",
                of(blocked_of(blocked_notice::Why::Failed(
                    "Connection refused".to_owned(),
                ))),
            ),
            (
                "died",
                of(notice::Of::Died(wire::DiedNotice {
                    detail: "exited with status 1".to_owned(),
                    ..wire::DiedNotice::default()
                })),
            ),
        ]
    }

    /// The committed fixture still opens, and what this code seals now still
    /// says the same thing. A failure here is a change to what the phone reads.
    #[test]
    fn the_phones_fixture_is_what_this_code_seals() {
        let fixture: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixture_path()).unwrap()).unwrap();
        let private = fixture_key();
        let public = X25519HkdfSha256::sk_to_pk(&private).to_bytes();
        assert_eq!(fixture["private_key"], fixture_key_text());
        let cases = fixture["cases"].as_array().unwrap();
        let pushes = fixture_pushes();
        assert_eq!(cases.len(), pushes.len());
        for (case, (name, push)) in cases.iter().zip(&pushes) {
            assert_eq!(case["name"], *name);
            assert_eq!(
                open(case["sealed"].as_str().unwrap(), &private),
                case["plain"]
            );
            let sealed = seal_push(push, &public).unwrap();
            assert_eq!(open(&sealed, &private), case["plain"], "{name}");
        }
    }

    /// Writes the fixture again. Sealing takes randomness, so this is run by
    /// hand when the sealed format changes, and its output is committed.
    #[test]
    #[ignore = "rewrites the committed fixture"]
    fn write_the_phones_fixture() {
        let private = fixture_key();
        let public = X25519HkdfSha256::sk_to_pk(&private).to_bytes();
        let cases: Vec<_> = fixture_pushes()
            .iter()
            .map(|(name, push)| {
                let sealed = seal_push(push, &public).unwrap();
                let plain = open(&sealed, &private);
                serde_json::json!({ "name": name, "sealed": sealed, "plain": plain })
            })
            .collect();
        let fixture = serde_json::json!({
            "private_key": fixture_key_text(),
            "cases": cases,
        });
        let path = fixture_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = serde_json::to_string_pretty(&fixture).unwrap();
        text.push('\n');
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_key_that_is_not_one_is_refused() {
        assert!(matches!(
            seal_push(&push("deploy", ""), b"short"),
            Err(SealError::Key)
        ));
    }
}
