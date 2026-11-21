//! Per-profile C ABI calls: pairing, onboarding, roster, agents, folders and reports.

use std::ffi::c_char;

use app_embedded::{PairRequest, ProfileView};
use app_runtime::values::{
    AgentAct, NewAgent, OnboardingAnswer, OnboardingState, PairingStatusView, PairingWindowView,
    SignInAnswer, SignInOfferOutcome,
};
use ui_state::AgentKey;

use crate::helpers::{Answered, fail, guard, live_profile, live_runtime, parse, profile_id, text};
use crate::runtime::{follow, open_profile};
use crate::{ATTACHED, AmuxProfile, AmuxRuntime};

// --- one profile ---------------------------------------------------------

/// Opens a profile's fleet, by the profile's id; blocks, stepping the
/// cores, until it has caught up with the profile's store. Null on failure, with
/// the reason in `error` when it is not null.
///
/// # Safety
/// `runtime` is from `amux_runtime_start`; `profile` is a NUL-terminated
/// string; `error` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_open(
    runtime: *const AmuxRuntime,
    profile: *const c_char,
    error: *mut *mut c_char,
) -> *mut AmuxProfile {
    let opened = guard(Err("opening the profile panicked".to_owned()), || {
        // SAFETY: the caller's contract.
        let runtime = unsafe { live_runtime(runtime) }.ok_or("no runtime")?;
        // SAFETY: the caller's contract.
        let profile = unsafe { profile_id(profile) }.ok_or("the profile is not a profile id")?;
        open_profile(runtime, profile)
    });
    match opened {
        Ok(profile) => Box::into_raw(profile),
        Err(reason) => {
            // SAFETY: the caller's contract.
            unsafe { fail(error, reason) };
            std::ptr::null_mut()
        }
    }
}

/// Closes a profile's fleet. No step hands over its delta, or its chats',
/// once this returns, however many acts are still in flight; they are
/// answered all the same. Close its chats first.
///
/// # Safety
/// `profile` is null or from `amux_profile_open`, and not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_close(profile: *mut AmuxProfile) {
    if !profile.is_null() {
        // SAFETY: the caller's contract.
        let open = unsafe { Box::from_raw(profile) };
        open.shared.outbox.close(open.fleet, None);
        let shared = open.shared.clone();
        let _entered = shared.handle().enter();
        drop(open);
    }
}

/// The number deltas name this fleet by.
///
/// # Safety
/// `profile` is null or from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_fleet_id(profile: *const AmuxProfile) -> u64 {
    // SAFETY: the caller's contract.
    unsafe { live_profile(profile) }.map_or(0, |open| open.fleet)
}

/// Writes a dump of the profile with the fleet's part and the record of
/// every chat on screen: the one bundle, whether it is then saved or sent
/// with a person's report. The ticket answers `{"Ok": "<bundle
/// directory>"}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `reason` is a NUL-terminated
/// string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_dump(
    profile: *const AmuxProfile,
    reason: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let reason = unsafe { text(reason) }.unwrap_or_default().to_owned();
        let app = profile.app.clone();
        profile.act(async move { Answered::from(app.dump(&reason).await) })
    })
}

/// Writes a person's report around the dump at `dump_path` (empty when
/// none was taken; a report's own directory to send it again), with its
/// `report.json` and the frozen frame as PNG, and sends it to the account
/// the profile is signed in to. The ticket answers `{"Ok": ReportSent}` or
/// `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `dump_path` and `header` are
/// NUL-terminated strings or null; `frame` is null or `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_submit_report(
    profile: *const AmuxProfile,
    dump_path: *const c_char,
    header: *const c_char,
    frame: *const u8,
    len: usize,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let dump_path = unsafe { text(dump_path) }.unwrap_or_default().to_owned();
        // SAFETY: the caller's contract.
        let header = unsafe { text(header) }
            .unwrap_or_default()
            .as_bytes()
            .to_vec();
        let frame = if frame.is_null() {
            Vec::new()
        } else {
            // SAFETY: the caller's contract.
            unsafe { std::slice::from_raw_parts(frame, len) }.to_vec()
        };
        let app = profile.app.clone();
        profile
            .act(async move { Answered::from(app.submit_report(dump_path, header, frame).await) })
    })
}

/// Reaches and authenticates a machine by a `PairRequest` as JSON; the
/// ticket answers `{"Ok": PendingPair}` or `{"Err": ..}`. Nothing is trusted
/// until `amux_profile_confirm_pair`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `request` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_begin_pair(
    profile: *const AmuxProfile,
    request: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let request: Option<PairRequest> = unsafe { parse(request) };
        let Some(embedded) = profile.shared.embedded().cloned() else {
            return profile.act(async { Answered::<()>::Err(ATTACHED.into()) });
        };
        let id = profile.profile;
        profile.act(async move {
            let Some(request) = request else {
                return Answered::Err("the request is not a PairRequest".into());
            };
            Answered::from(embedded.begin_pair(id, &request).await)
        })
    })
}

/// Offers the paired machine whose host id `host_id` names, as a JSON byte
/// array, this profile's account: the phone signed in first offers the
/// computer it paired with. The ticket answers `{"Ok": null}` or
/// `{"Err": ..}`; the offer stands until that machine answers it.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `host_id` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_offer_sign_in(
    profile: *const AmuxProfile,
    host_id: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        let client = profile.app.client().clone();
        profile.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(client.offer_sign_in(host_id).await)
        })
    })
}

/// Answers a paired machine's offer of its account, a `SignInAnswer` as
/// JSON. The ticket answers `{"Ok": SignInOfferOutcome}`: accepting signs
/// this profile in to that account and answers it signed in, with the
/// profile list moving too; declining ends the offer; accepting while the
/// offering machine cannot be reached answers that, and the offer stays in
/// the profile's list for as long as it still stands. Any other refusal is
/// `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `answer` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_answer_sign_in_offer(
    profile: *const AmuxProfile,
    answer: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let answer: Option<SignInAnswer> = unsafe { parse(answer) };
        let client = profile.app.client().clone();
        profile.act(async move {
            let Some(answer) = answer else {
                return Answered::Err("the answer is not a SignInAnswer".into());
            };
            let answered = client
                .answer_sign_in_offer(wire::AnswerSignInOfferRequest {
                    host_id: answer.host_id,
                    accept: answer.accept,
                })
                .await;
            match answered {
                Ok(profile) if answer.accept => {
                    Answered::Ok(SignInOfferOutcome::SignedIn(ProfileView::from(&profile)))
                }
                Ok(_) => Answered::Ok(SignInOfferOutcome::Declined),
                // Said apart from every other refusal: the person's next
                // step is on the other machine, not on this one.
                Err(error) if error.code() == Some(wire::ErrorCode::Unavailable) => {
                    Answered::Ok(SignInOfferOutcome::Unreachable)
                }
                Err(error) => Answered::Err(error.to_string()),
            }
        })
    })
}

/// Follows the profile's setting up: the next step hands over the steps
/// as the daemon keeps them, and every step after one that moved them
/// does, as an `OnboardingState` under the profile's fleet number, until
/// the profile closes. Following twice changes nothing.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_watch_onboarding(profile: *const AmuxProfile) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        let mut watch = profile.onboarding.lock().unwrap_or_else(|p| p.into_inner());
        if watch.is_some() {
            return;
        }
        let installation = profile.shared.installation.clone();
        let outbox = profile.shared.outbox.clone();
        let (fleet, id) = (profile.fleet, profile.profile);
        *watch = Some(profile.shared.handle().spawn(async move {
            follow(
                || {
                    installation.subscribe_onboarding(wire::SubscribeOnboardingRequest {
                        profile_id: id.as_bytes().to_vec(),
                    })
                },
                |now: wire::Onboarding| {
                    outbox.post_onboarding(fleet, OnboardingState::from(&now));
                },
            )
            .await;
        }));
    });
}

/// Tells the daemon what the person did with setting up this profile, an
/// `OnboardingAnswer` as JSON, counted for this app; the ticket answers
/// `{"Ok": OnboardingState}`, the steps after it, or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `answer` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_answer_onboarding(
    profile: *const AmuxProfile,
    answer: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let answer: Option<OnboardingAnswer> = unsafe { parse(answer) };
        let installation = profile.shared.installation.clone();
        let id = profile.profile;
        profile.act(async move {
            let Some(answer) = answer else {
                return Answered::Err("the answer is not an OnboardingAnswer".into());
            };
            Answered::from(
                installation
                    .answer_onboarding(wire::AnswerOnboardingRequest {
                        profile_id: id.as_bytes().to_vec(),
                        answer: Some(answer.into()),
                    })
                    .await
                    .map(|now| OnboardingState::from(&now)),
            )
        })
    })
}

/// Opens a pairing window on the profile that takes the PIN or the QR
/// code's secret, for `ttl_seconds` (0 for the daemon's own), closing any
/// window open first, and follows its status: every step after it moved
/// hands over a `PairingStatusView` under the profile's fleet number until
/// `amux_profile_close_pairing`. The ticket answers
/// `{"Ok": PairingWindowView}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_open_pairing(
    profile: *const AmuxProfile,
    ttl_seconds: u64,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        let client = profile.app.client().clone();
        {
            let mut watch = profile.pairing.lock().unwrap_or_else(|p| p.into_inner());
            if watch.is_none() {
                let (watching, outbox, fleet) =
                    (client.clone(), profile.shared.outbox.clone(), profile.fleet);
                *watch = Some(profile.shared.handle().spawn(async move {
                    follow(
                        || watching.subscribe_pairing(),
                        |status: wire::PairingStatus| {
                            outbox.post_pairing(fleet, PairingStatusView::from(&status));
                        },
                    )
                    .await;
                }));
            }
        }
        profile.act(async move {
            // One window is open at a time: a renewal replaces the one
            // before, which may already have ended.
            let _ = client.cancel_pairing().await;
            Answered::from(
                client
                    .start_pairing(wire::StartPairingRequest {
                        method: Some(wire::start_pairing_request::Method::PinOrQr(wire::Empty {})),
                        ttl_seconds: (ttl_seconds > 0).then_some(ttl_seconds),
                    })
                    .await
                    .map(|window| PairingWindowView::from(&window)),
            )
        })
    })
}

/// Closes the profile's pairing window and stops following it; the ticket
/// answers `{"Ok": null}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_close_pairing(profile: *const AmuxProfile) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        if let Some(watch) = profile
            .pairing
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            watch.abort();
        }
        let client = profile.app.client().clone();
        profile.act(async move { Answered::from(client.cancel_pairing().await) })
    })
}

/// Trusts the machine a pending pairing reached, named by its token as a
/// JSON byte array; the ticket answers `{"Ok": null}` or `{"Err": ..}`.
///
/// # Safety
/// As for `amux_profile_begin_pair`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_confirm_pair(
    profile: *const AmuxProfile,
    token: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let token: Option<Vec<u8>> = unsafe { parse(token) };
        let Some(embedded) = profile.shared.embedded().cloned() else {
            return profile.act(async { Answered::<()>::Err(ATTACHED.into()) });
        };
        let id = profile.profile;
        profile.act(async move {
            let Some(token) = token else {
                return Answered::Err("the token is not a byte array".into());
            };
            Answered::from(embedded.confirm_pair(id, &token).await.map(drop))
        })
    })
}

/// Turns away the machine a pending pairing reached.
///
/// # Safety
/// As for `amux_profile_begin_pair`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_abandon_pair(
    profile: *const AmuxProfile,
    token: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let token: Option<Vec<u8>> = unsafe { parse(token) };
        let Some(embedded) = profile.shared.embedded().cloned() else {
            return profile.act(async { Answered::<()>::Err(ATTACHED.into()) });
        };
        let id = profile.profile;
        profile.act(async move {
            let Some(token) = token else {
                return Answered::Err("the token is not a byte array".into());
            };
            Answered::from(embedded.abandon_pair(id, &token).await)
        })
    })
}

/// Stops trusting a paired machine, by its host id as a JSON byte array.
///
/// # Safety
/// As for `amux_profile_begin_pair`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_unpair(
    profile: *const AmuxProfile,
    host_id: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        let Some(embedded) = profile.shared.embedded().cloned() else {
            return profile.act(async { Answered::<()>::Err(ATTACHED.into()) });
        };
        let id = profile.profile;
        profile.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(embedded.unpair(id, &host_id).await)
        })
    })
}

/// The profile's identity and the machines it trusts; the ticket answers
/// `{"Ok": Roster}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_roster(profile: *const AmuxProfile) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        let Some(embedded) = profile.shared.embedded().cloned() else {
            return profile.act(async { Answered::<()>::Err(ATTACHED.into()) });
        };
        let id = profile.profile;
        profile.act(async move { Answered::from(embedded.roster(id)) })
    })
}

/// Starts an agent from a `NewAgent` as JSON; the ticket answers
/// `{"Ok": AgentKey}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `agent` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_create_agent(
    profile: *const AmuxProfile,
    agent: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let agent: Option<NewAgent> = unsafe { parse(agent) };
        let app = profile.app.clone();
        profile.act(async move {
            let Some(agent) = agent else {
                return Answered::Err("the agent is not a NewAgent".into());
            };
            Answered::from(app.create_agent(&agent).await)
        })
    })
}

/// Where a host, by its id as a JSON byte array, offers to start an agent,
/// matching `query` (or everything when it is null or empty), at most
/// `limit` of each; the ticket answers `{"Ok": Directories}` or
/// `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; the strings are NUL-terminated
/// or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_directories(
    profile: *const AmuxProfile,
    host_id: *const c_char,
    query: *const c_char,
    limit: u32,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        // SAFETY: the caller's contract.
        let query = unsafe { text(query) }.unwrap_or_default().to_owned();
        let app = profile.app.clone();
        profile.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(app.directories(&host_id, &query, limit).await)
        })
    })
}

/// Pins `path` among a host's repositories, the host by its id as a JSON
/// byte array, or unpins it; the ticket answers `{"Ok": null}` or
/// `{"Err": ..}`. The host's entry says the change once it has made it.
///
/// # Safety
/// `profile` is from `amux_profile_open`; the strings are NUL-terminated
/// or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_pin_folder(
    profile: *const AmuxProfile,
    host_id: *const c_char,
    path: *const c_char,
    pinned: bool,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        // SAFETY: the caller's contract.
        let path = unsafe { text(path) }.unwrap_or_default().to_owned();
        let app = profile.app.clone();
        profile.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            Answered::from(app.pin_folder(&host_id, &path, pinned).await)
        })
    })
}

/// Renames, stops or deletes an agent, named by its `AgentKey`, with an
/// `AgentAct`, both as JSON; the ticket answers `{"Ok": null}` or
/// `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; the strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_agent_act(
    profile: *const AmuxProfile,
    agent: *const c_char,
    act: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let agent: Option<AgentKey> = unsafe { parse(agent) };
        // SAFETY: the caller's contract.
        let act: Option<AgentAct> = unsafe { parse(act) };
        let app = profile.app.clone();
        profile.act(async move {
            let (Some(agent), Some(act)) = (agent, act) else {
                return Answered::Err("the agent or the act is not readable".into());
            };
            Answered::from(app.agent_act(&agent, &act).await)
        })
    })
}

/// What deleting an agent, named by its `AgentKey` as JSON, does to its
/// workspace, once current: the ticket answers `{"Ok": DeleteWarning}`,
/// `{"Ok": null}` when the workspace stays, or `{"Err": ..}`. For an agent
/// with no process it asks the agent's host first.
///
/// # Safety
/// `profile` is from `amux_profile_open`; the string is NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_delete_warning(
    profile: *const AmuxProfile,
    agent: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let agent: Option<AgentKey> = unsafe { parse(agent) };
        let app = profile.app.clone();
        profile.act(async move {
            let Some(agent) = agent else {
                return Answered::Err("the agent is not readable".into());
            };
            Answered::Ok(app.delete_warning(&agent).await)
        })
    })
}

/// What a provider, by its id as a JSON string ("claude", "codex", an ACP
/// agent's), offers on a
/// host, by its id as a JSON byte array, with no agent running there; the
/// ticket answers `{"Ok": Catalogue}` or `{"Err": ..}`.
///
/// # Safety
/// `profile` is from `amux_profile_open`; the strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_host_catalogue(
    profile: *const AmuxProfile,
    host_id: *const c_char,
    provider: *const c_char,
) -> u64 {
    guard(0, || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return 0;
        };
        // SAFETY: the caller's contract.
        let host_id: Option<Vec<u8>> = unsafe { parse(host_id) };
        // SAFETY: the caller's contract.
        let provider: Option<String> = unsafe { parse(provider) };
        let app = profile.app.clone();
        profile.act(async move {
            let Some(host_id) = host_id else {
                return Answered::Err("the host id is not a byte array".into());
            };
            let Some(provider) = provider else {
                return Answered::Err("the provider is not a provider id".into());
            };
            Answered::from(app.host_catalogue(&host_id, &provider).await)
        })
    })
}

/// The app leaving (false) or returning to (true) the foreground: every
/// agent's stream closes, or reopens. Open chats keep their rows and catch
/// up when the streams reopen.
///
/// # Safety
/// `profile` is from `amux_profile_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_profile_set_foreground(
    profile: *const AmuxProfile,
    foreground: bool,
) {
    guard((), || {
        // SAFETY: the caller's contract.
        let Some(profile) = (unsafe { live_profile(profile) }) else {
            return;
        };
        let _entered = profile.shared.handle().enter();
        profile.app.set_foreground(foreground);
    });
}
