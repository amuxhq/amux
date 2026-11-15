//! The amux client crates the browser extension runs, compiled to
//! WebAssembly: the fleet model and the rows its agent picker draws, and the
//! `amux.v1` messages it sends and reads.
//!
//! The extension's TypeScript plays the part the drivers play in the other
//! clients: it subscribes, resubscribes when a stream drops, and moves bytes
//! through `amux internal browser`. It calls this crate for everything that
//! knows `amux.v1`, so the extension needs no protobuf code of its own, and
//! the views it draws ship with it, at its own version.
//!
//! One entry point, [`handle`]: a JSON request naming an operation, and a
//! JSON answer. Bytes travel as base64, ids as lowercase hex. The WebAssembly
//! module exports it as `call`, with `alloc` and `free` for the memory a
//! request and its answer pass through.
//!
//! Using WebAssembly is a convenience, never a constraint on the crates it
//! links: a change to a shared crate that stops it building here wins, and
//! the extension does in TypeScript what it lost. Nothing WebAssembly needs
//! lives outside this crate.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Value, json};
use ui_state::{FleetMsg, FleetState};

thread_local! {
    static FLEETS: RefCell<HashMap<u32, FleetState>> = RefCell::new(HashMap::new());
    static NEXT: RefCell<u32> = const { RefCell::new(1) };
}

/// Answers one request. An answer is `{"ok": …}` or `{"error": "…"}`.
pub fn handle(request: &str) -> String {
    let answer = match serde_json::from_str::<Request>(request) {
        Ok(request) => answer(request).map_err(|error| error.to_string()),
        Err(error) => Err(format!("a request that does not read: {error}")),
    };
    match answer {
        Ok(value) => json!({ "ok": value }),
        Err(error) => json!({ "error": error }),
    }
    .to_string()
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum Request {
    /// The front door's profiles, from a `ListProfilesResponse`.
    Profiles {
        message: String,
    },
    /// This machine's host id in a profile, from a
    /// `GetDeviceIdentityResponse`.
    DeviceIdentity {
        message: String,
    },
    /// A new, empty fleet model.
    FleetNew,
    FleetDrop {
        fleet: u32,
    },
    /// One `SubscribeInventoryResponse` applied to a fleet.
    FleetApply {
        fleet: u32,
        message: String,
    },
    /// The fleet as home draws it, for the picker.
    FleetView {
        fleet: u32,
        #[serde(rename = "localHost")]
        local_host: String,
    },
    /// The agents the picker offers, from home's view: each family's head,
    /// in home's order, with what the picker shows of it.
    Picker {
        fleet: u32,
        #[serde(rename = "localHost")]
        local_host: String,
        profile: String,
    },
    /// Whether the fleet lists an agent yet, and its kind if so.
    AgentKind {
        fleet: u32,
        host: String,
        agent: String,
    },
    /// A `PutBlobRequest`.
    PutBlob {
        agent: String,
        name: String,
        mime: String,
        bytes: String,
    },
    /// The `BlobRef` a `PutBlobResponse` holds.
    Blob {
        message: String,
    },
    /// A `SendInputRequest` carrying a prompt in the agent's kind: the words,
    /// with one U+FFFC where each attachment goes, the page review first and
    /// then each piece of text sent from a page.
    SendPrompt {
        fleet: u32,
        host: String,
        agent: String,
        #[serde(rename = "inputId")]
        input_id: String,
        text: String,
        page: Option<wire::PageReview>,
        #[serde(default)]
        texts: Vec<Snippet>,
    },
    /// What a `SendInputResponse` says.
    Verdict {
        message: String,
    },
    /// A `ListRepositoriesRequest` for this host.
    ListRepositories,
    /// The folders a `ListRepositoriesResponse` offers.
    Repositories {
        message: String,
    },
    /// A `CreateAgentRequest` for a Claude agent on this host in `cwd`,
    /// of the kind the host publishes, with no first prompt: the review's
    /// screenshots are stored with it once it exists, then sent to it.
    CreateAgent {
        fleet: u32,
        #[serde(rename = "localHost")]
        local_host: String,
        agent: String,
        cwd: String,
    },
    /// The words of the element a model reads for a page review, so the
    /// extension can show exactly what is sent.
    Element {
        page: wire::PageReview,
    },
}

/// Text sent from a page: a selection, or a page's or link's address.
#[derive(Deserialize)]
struct Snippet {
    name: String,
    text: String,
}

fn answer(request: Request) -> Result<Value, String> {
    Ok(match request {
        Request::Profiles { message } => {
            let response: wire::ListProfilesResponse = decode(&message)?;
            Value::Array(
                response
                    .profiles
                    .into_iter()
                    .map(|profile| {
                        json!({
                            "id": hex(&profile.id),
                            "label": profile.label,
                            "email": profile.email,
                            "accountName": profile.account_name,
                        })
                    })
                    .collect(),
            )
        }
        Request::DeviceIdentity { message } => {
            let response: wire::GetDeviceIdentityResponse = decode(&message)?;
            let identity = response.identity.unwrap_or_default();
            json!({ "hostId": hex(&identity.host_id), "name": identity.name })
        }
        Request::FleetNew => {
            let id = NEXT.with(|next| {
                let mut next = next.borrow_mut();
                let id = *next;
                *next += 1;
                id
            });
            FLEETS.with(|fleets| fleets.borrow_mut().insert(id, FleetState::new()));
            json!(id)
        }
        Request::FleetDrop { fleet } => {
            FLEETS.with(|fleets| fleets.borrow_mut().remove(&fleet));
            Value::Null
        }
        Request::FleetApply { fleet, message } => {
            let response: wire::SubscribeInventoryResponse = decode(&message)?;
            let Some(event) = response.event else {
                return Ok(Value::Null);
            };
            with_fleet(fleet, |state| {
                state.update(FleetMsg::Event(Box::new(event)));
                json!({ "caughtUp": state.caught_up() })
            })?
        }
        Request::FleetView { fleet, local_host } => {
            let local_host = unhex(&local_host)?;
            with_fleet(fleet, |state| {
                let view = ui_view::fleet_view(
                    state,
                    &local_host,
                    &HashMap::new(),
                    &HashSet::new(),
                    &|_| true,
                );
                serde_json::to_value(view).unwrap_or(Value::Null)
            })?
        }
        Request::Picker {
            fleet,
            local_host,
            profile,
        } => {
            let local_host = unhex(&local_host)?;
            with_fleet(fleet, |state| {
                let view = ui_view::fleet_view(
                    state,
                    &local_host,
                    &HashMap::new(),
                    &HashSet::new(),
                    &|_| true,
                );
                let mut choices = Vec::new();
                for section in view.sections {
                    for row in section.rows {
                        let card = row.card;
                        choices.push(json!({
                            "profile": profile,
                            "host": hex(&card.agent.host),
                            "agent": hex(&card.agent.agent),
                            "name": card.name,
                            "handle": card.handle,
                            "hostName": card.host,
                            "cwd": card.cwd,
                            "repository": card.repository.map(|repository| repository.name).unwrap_or_default(),
                            "closed": section.kind == ui_view::SectionKind::Closed,
                            "needsYou": card.attention == ui_state::Attention::NeedsYou,
                            "local": card.agent.host == local_host,
                        }));
                    }
                }
                Value::Array(choices)
            })?
        }
        Request::AgentKind { fleet, host, agent } => {
            let key = ui_state::AgentKey {
                host: unhex(&host)?,
                agent: unhex(&agent)?,
            };
            with_fleet(fleet, |state| {
                state
                    .agent(&key)
                    .map(|agent| json!(agent.kind().as_str_name()))
                    .unwrap_or(Value::Null)
            })?
        }
        Request::PutBlob {
            agent,
            name,
            mime,
            bytes,
        } => encoded(&wire::PutBlobRequest {
            agent_id: unhex(&agent)?,
            name,
            mime,
            bytes: STANDARD.decode(bytes).map_err(|_| "bytes are not base64")?,
        }),
        Request::Blob { message } => {
            let response: wire::PutBlobResponse = decode(&message)?;
            serde_json::to_value(response.blob.ok_or("no blob in the answer")?)
                .map_err(|error| error.to_string())?
        }
        Request::SendPrompt {
            fleet,
            host,
            agent,
            input_id,
            text,
            page,
            texts,
        } => {
            let key = ui_state::AgentKey {
                host: unhex(&host)?,
                agent: unhex(&agent)?,
            };
            let kind = with_fleet(fleet, |state| state.agent(&key).map(|agent| agent.kind()))?
                .ok_or("no such agent in the fleet")?;
            let attachments = page
                .into_iter()
                .map(wire::attachment::Of::Page)
                .chain(texts.into_iter().map(|snippet| {
                    wire::attachment::Of::Text(wire::InlineText {
                        name: snippet.name,
                        text: snippet.text,
                    })
                }))
                .map(|of| wire::Attachment { of: Some(of) })
                .collect();
            let input = prompt(kind, unhex(&input_id)?, text, attachments)
                .ok_or("an agent of a kind this build does not know")?;
            encoded(&wire::SendInputRequest {
                agent_id: key.agent,
                input: Some(input),
            })
        }
        Request::Verdict { message } => {
            let response: wire::SendInputResponse = decode(&message)?;
            match response.of {
                Some(wire::send_input_response::Of::Accepted(accepted)) => {
                    json!({ "accepted": true, "queued": accepted.queued })
                }
                Some(wire::send_input_response::Of::Rejected(rejected)) => json!({
                    "accepted": false,
                    "refusal": rejected.refusal().as_str_name(),
                    "detail": rejected.detail,
                }),
                None => json!({ "accepted": false }),
            }
        }
        Request::ListRepositories => encoded(&wire::ListRepositoriesRequest {
            query: None,
            limit: 50,
            host_id: None,
        }),
        Request::Repositories { message } => {
            let response: wire::ListRepositoriesResponse = decode(&message)?;
            let entry =
                |entry: wire::RepositoryEntry| json!({ "path": entry.path, "name": entry.name });
            json!({
                "recent": response.recent.into_iter().map(entry).collect::<Vec<_>>(),
                "repositories": response.repositories.into_iter().map(entry).collect::<Vec<_>>(),
            })
        }
        Request::CreateAgent {
            fleet,
            local_host,
            agent,
            cwd,
        } => {
            let local_host = unhex(&local_host)?;
            let kind = with_fleet(fleet, |state| {
                state.host(&local_host).map(|host| host.claude_kind())
            })?
            .unwrap_or(wire::Kind::ClaudePty);
            let config = wire::ClaudeCreateConfig::default();
            encoded(&wire::CreateAgentRequest {
                agent_id: unhex(&agent)?,
                cwd,
                agent: Some(match kind {
                    wire::Kind::ClaudeSdk => wire::create_agent_request::Agent::ClaudeSdk(config),
                    _ => wire::create_agent_request::Agent::ClaudePty(config),
                }),
                ..Default::default()
            })
        }
        Request::Element { page } => json!(attachments::element(
            &wire::Attachment {
                of: Some(wire::attachment::Of::Page(page)),
            },
            None,
        )),
    })
}

/// A prompt in the kind's own arm.
fn prompt(
    kind: wire::Kind,
    input_id: Vec<u8>,
    text: String,
    attachments: Vec<wire::Attachment>,
) -> Option<wire::Input> {
    let prompt = wire::PromptInput { text, attachments };
    let of = match kind {
        wire::Kind::ClaudePty => wire::KindInput::ClaudePty(wire::ClaudePtyInput {
            of: Some(wire::claude_pty_input::Of::Prompt(prompt)),
        }),
        wire::Kind::ClaudeSdk => wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(prompt)),
        }),
        wire::Kind::Codex => wire::KindInput::Codex(wire::CodexInput {
            of: Some(wire::codex_input::Of::Prompt(prompt)),
        }),
        wire::Kind::Unspecified => return None,
    };
    Some(wire::Input::new(input_id, of))
}

fn with_fleet<T>(fleet: u32, f: impl FnOnce(&mut FleetState) -> T) -> Result<T, String> {
    FLEETS.with(|fleets| {
        fleets
            .borrow_mut()
            .get_mut(&fleet)
            .map(f)
            .ok_or_else(|| format!("no fleet {fleet}"))
    })
}

fn decode<T: prost::Message + Default>(message: &str) -> Result<T, String> {
    let bytes = STANDARD
        .decode(message)
        .map_err(|_| "a message that is not base64".to_owned())?;
    T::decode(bytes.as_slice()).map_err(|error| error.to_string())
}

fn encoded(message: &impl prost::Message) -> Value {
    json!(STANDARD.encode(message.encode_to_vec()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err(format!("{text:?} is not hex"));
    }
    (0..text.len())
        .step_by(2)
        .map(|at| {
            text.get(at..at + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(|| format!("{text:?} is not hex"))
        })
        .collect()
}

/// The WebAssembly module's exports: `alloc` and `free` for the bytes a
/// request and its answer are, and `call`, which takes a request's UTF-8
/// and answers with its answer's place and length packed into one number,
/// place in the high half.
#[cfg(target_arch = "wasm32")]
mod exports {
    #[unsafe(no_mangle)]
    pub extern "C" fn alloc(len: usize) -> *mut u8 {
        let mut bytes = Vec::<u8>::with_capacity(len);
        let at = bytes.as_mut_ptr();
        std::mem::forget(bytes);
        at
    }

    /// # Safety
    /// `at` and `len` are what `alloc` or `call` gave out, freed once.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn free(at: *mut u8, len: usize) {
        // SAFETY: the caller frees what this module allocated, once.
        drop(unsafe { Vec::from_raw_parts(at, 0, len) });
    }

    /// # Safety
    /// `at` holds `len` bytes the caller wrote after `alloc(len)`; this
    /// frees them.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn call(at: *mut u8, len: usize) -> u64 {
        // SAFETY: the caller wrote `len` bytes at `at`, from `alloc(len)`.
        let request = unsafe { Vec::from_raw_parts(at, len, len) };
        let answer = super::handle(&String::from_utf8_lossy(&request));
        let mut answer = answer.into_bytes().into_boxed_slice();
        let place = answer.as_mut_ptr() as u64;
        let length = answer.len() as u64;
        std::mem::forget(answer);
        (place << 32) | length
    }
}

#[cfg(test)]
mod tests;
