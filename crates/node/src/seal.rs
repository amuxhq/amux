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
use wire::{ask_notice, blocked_notice, notice};

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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Said {
    Ask {
        key: String,
        wants: Wants,
        why: String,
        more: u32,
    },
    SignedOut,
    UsageLimit {
        resets_at_ms: Option<i64>,
    },
    Failed {
        message: String,
    },
    Died {
        detail: String,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Wants {
    Run { command: String },
    Change { path: String, files: u32 },
    Use { server: String, tool: String },
    Question { text: String },
    Plan { title: String },
    Other { text: String },
}

impl Wants {
    fn of(wants: Option<ask_notice::Wants>) -> Self {
        match wants {
            Some(ask_notice::Wants::Run(run)) => Self::Run {
                command: run.command,
            },
            Some(ask_notice::Wants::Change(change)) => Self::Change {
                path: change.path,
                files: change.files,
            },
            Some(ask_notice::Wants::Use(tool)) => Self::Use {
                server: tool.server,
                tool: tool.tool,
            },
            Some(ask_notice::Wants::Question(question)) => Self::Question {
                text: question.text,
            },
            Some(ask_notice::Wants::Plan(plan)) => Self::Plan { title: plan.title },
            Some(ask_notice::Wants::Other(other)) => Self::Other { text: other.text },
            None => Self::Other {
                text: String::new(),
            },
        }
    }
}

impl Said {
    fn of(notice: wire::Notice) -> Result<Self, SealError> {
        Ok(match notice.of {
            Some(notice::Of::Ask(ask)) => Self::Ask {
                key: ask.key,
                wants: Wants::of(ask.wants),
                why: ask.why,
                more: ask.more,
            },
            Some(notice::Of::Blocked(blocked)) => match blocked.why {
                Some(blocked_notice::Why::SignedOut(_)) => Self::SignedOut,
                Some(blocked_notice::Why::Usage(usage)) => Self::UsageLimit {
                    resets_at_ms: usage.resets_at_ms,
                },
                Some(blocked_notice::Why::Failed(message)) => Self::Failed { message },
                None => Self::Failed {
                    message: String::new(),
                },
            },
            Some(notice::Of::Died(died)) => Self::Died {
                detail: died.detail,
            },
            None => return Err(SealError::Notice("it says nothing".to_owned())),
        })
    }

    /// The same notice without its reason, the first thing to go when a push
    /// is too long: what the agent wants still says what it needs.
    fn without_why(self) -> Self {
        match self {
            Self::Ask {
                key, wants, more, ..
            } => Self::Ask {
                key,
                wants,
                why: String::new(),
                more,
            },
            said => said,
        }
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
    let said = Said::of(notice)?;
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
    let shorter = seal_bytes(&plain(said.without_why())?, push_key)?;
    if shorter.len() <= MOST_SEALED_CHARS {
        Ok(shorter)
    } else {
        Err(SealError::TooLong)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hpke::{Kem as _, OpModeR};
    use wire::{AskNotice, Notice, RunWant};

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
        assert_eq!(opened["notice"]["ask"]["key"], "ask:1");
        assert_eq!(
            opened["notice"]["ask"]["wants"]["run"]["command"],
            "deploy --prod"
        );
        assert_eq!(opened["notice"]["ask"]["more"], 1);
    }

    #[test]
    fn a_push_too_long_to_send_loses_its_reason_first() {
        let (private, public) = X25519HkdfSha256::gen_keypair();
        let sealed = seal_push(&push("deploy", &"why ".repeat(1000)), &public.to_bytes()).unwrap();
        assert!(sealed.len() <= MOST_SEALED_CHARS);
        assert_eq!(open(&sealed, &private)["notice"]["ask"]["why"], "");
    }

    #[test]
    fn a_key_that_is_not_one_is_refused() {
        assert!(matches!(
            seal_push(&push("deploy", ""), b"short"),
            Err(SealError::Key)
        ));
    }
}
