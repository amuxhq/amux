//! Open-chat C ABI calls, blobs, reviews and buffer deallocation.

use std::ffi::{CString, c_char};
use std::sync::Arc;

use app_runtime::Chat;
use app_runtime::values::{ChatAct, Draft, FrozenReview};
use ui_state::{AgentKey, Key};
use ui_view::Comparison;

use crate::helpers::{
    Answered, act, answer, fail, guard, held_chat, input_id, live_profile, owned, parse, read, text,
};
use crate::outbox::View;
use crate::{AmuxBytes, AmuxChat, AmuxProfile};

/// Opens a chat on a fleet agent, named by its `AgentKey` as JSON, with
/// `tail` rows (the configured tail when zero). Blocks until the snapshot
/// and the rows this device holds are applied, so the opening is whole
/// even with the agent's host away. Null on failure, with the
/// reason in `error` when it is not null.
///
/// # Safety
/// `profile` is from `amux_profile_open`; `agent` is a NUL-terminated
/// string; `error` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_open(
    profile: *const AmuxProfile,
    agent: *const c_char,
    tail: u32,
    error: *mut *mut c_char,
) -> *mut AmuxChat {
    let opened = guard(Err("opening the chat panicked".to_owned()), || {
        // SAFETY: the caller's contract.
        let profile = unsafe { live_profile(profile) }.ok_or("no profile")?;
        // SAFETY: the caller's contract.
        let agent: AgentKey = unsafe { parse(agent) }.ok_or("the agent is not an AgentKey")?;
        let tail = if tail == 0 { profile.tail } else { tail };
        let app = profile.app.clone();
        let chat = profile
            .shared
            .phone
            .wait(async move { app.open_chat(&agent, tail).await })
            .ok_or("the runtime has stopped")?
            .map_err(|error| error.to_string())?;
        profile
            .shared
            .outbox
            .opened(profile.fleet, chat.id(), View::Chat(Arc::downgrade(&chat)));
        Ok(AmuxChat {
            chat,
            fleet: profile.fleet,
            shared: profile.shared.clone(),
        })
    });
    match opened {
        Ok(chat) => Box::into_raw(Box::new(chat)),
        Err(reason) => {
            // SAFETY: the caller's contract.
            unsafe { fail(error, reason) };
            std::ptr::null_mut()
        }
    }
}

/// The number deltas name this chat by, within its fleet.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_id(chat: *const AmuxChat) -> u64 {
    // SAFETY: the caller's contract.
    unsafe { held_chat(chat) }.map_or(0, |open| open.chat.id())
}

/// Closes a chat. No step hands over its delta once this returns, however
/// many of its acts are still in flight; they are answered all the same.
///
/// # Safety
/// `chat` is null or from `amux_session_open`, and not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_close(chat: *mut AmuxChat) {
    if !chat.is_null() {
        // SAFETY: the caller's contract.
        let open = unsafe { Box::from_raw(chat) };
        open.shared.outbox.close(open.fleet, Some(open.chat.id()));
        let shared = open.shared.clone();
        let _entered = shared.handle().enter();
        drop(open);
    }
}

/// The chat whole as a `ChatDelta`, for the app's model opening on it:
/// every key, the frame and the views around it. No step hands over a
/// delta of the chat before this; every one after says what moved since.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_opening(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { read(chat, Chat::opening) }
}

/// Tells the chat whether the reader is at the newest row (true) or has
/// scrolled into history (false). A chat opens following.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_follow(chat: *const AmuxChat, following: bool) {
    // SAFETY: the caller's contract.
    let _ = unsafe { read(chat, |chat| chat.follow(following)) };
}

/// The `Draft` this device kept for the agent when the chat opened, or
/// null JSON.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_kept_draft(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { read(chat, Chat::kept_draft) }
}

/// Keeps a `Draft`, as JSON, with this device's daemon as it now stands,
/// so it outlives the chat and the app; an empty one is forgotten.
///
/// # Safety
/// `chat` is from `amux_session_open`; `draft` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_keep_draft(chat: *const AmuxChat, draft: *const c_char) {
    // SAFETY: the caller's contract.
    let Some(draft) = (unsafe { parse::<Draft>(draft) }) else {
        return;
    };
    // SAFETY: the caller's contract.
    let _ = unsafe { read(chat, |chat| chat.keep_draft(&draft)) };
}

/// `[Row]` for these keys (a JSON array of strings), in order, skipping
/// keys not held, each run folded to its newest step unless
/// `amux_session_toggle_run` opened it: for a row drawn for the first
/// time, or after a reload.
///
/// # Safety
/// `chat` is from `amux_session_open`; `keys` is NUL-terminated or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_rows_for(
    chat: *const AmuxChat,
    keys: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let keys: Vec<Key> = unsafe { parse(keys) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe { read(chat, |chat| chat.rows_for(&keys)) }
}

/// Opens or closes the run the row `member` belongs to; the next step
/// hands over its rows drawn so. The chat keeps an open run open as it
/// grows, older steps page in, or the window trims it.
///
/// # Safety
/// `chat` is from `amux_session_open`; `member` is NUL-terminated or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_toggle_run(chat: *const AmuxChat, member: *const c_char) {
    // SAFETY: the caller's contract.
    let member = unsafe { text(member) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let _ = unsafe { read(chat, |chat| chat.toggle_run(&member)) };
}

/// Opens or closes the group of calls that opened agents the row `member`
/// belongs to ("3 subagents"); the next step hands over its rows drawn so.
/// Opening a row's agent is a separate gesture: `amux_session_open` on the
/// agent the delta's `agents` names for the row's `opens`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `member` is NUL-terminated or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_toggle_group(chat: *const AmuxChat, member: *const c_char) {
    // SAFETY: the caller's contract.
    let member = unsafe { text(member) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let _ = unsafe { read(chat, |chat| chat.toggle_group(&member)) };
}

/// Folds each finished turn to its prompt, the line over its work and its
/// answer (true), or only its runs (false, as a chat opens); the next step
/// hands over every row drawn so.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_fold_turns(chat: *const AmuxChat, on: bool) {
    // SAFETY: the caller's contract.
    let _ = unsafe { read(chat, |chat| chat.fold_turns(on)) };
}

/// Opens or closes the finished turn whose end is the row `end`; the next
/// step hands over its rows drawn so.
///
/// # Safety
/// `chat` is from `amux_session_open`; `end` is NUL-terminated or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_toggle_turn(chat: *const AmuxChat, end: *const c_char) {
    // SAFETY: the caller's contract.
    let end = unsafe { text(end) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let _ = unsafe { read(chat, |chat| chat.toggle_turn(&end)) };
}

/// Starts a `ChatAct`, as JSON, on the chat; the ticket answers what the
/// act says it answers.
///
/// # Safety
/// `chat` is from `amux_session_open`; `act` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_act(chat: *const AmuxChat, act: *const c_char) -> u64 {
    // SAFETY: the caller's contract.
    let chat_act: Option<ChatAct> = unsafe { parse(act) };
    // SAFETY: the caller's contract.
    unsafe {
        self::act(chat, |chat| async move {
            match chat_act {
                Some(chat_act) => acted(&chat, chat_act).await,
                None => answer(Answered::<()>::Err("the act is not a ChatAct".into())),
            }
        })
    }
}

/// Runs an act on the chat and answers it as JSON.
async fn acted(chat: &Chat, act: ChatAct) -> serde_json::Value {
    match act {
        ChatAct::Send(draft) => answer(Answered::Ok(chat.send(&draft).await)),
        ChatAct::Command(draft) => answer(chat.command(&draft).await),
        ChatAct::Answer { ask, choice, note } => {
            answer(chat.answer_choice(&ask, choice as usize, &note).await)
        }
        ChatAct::AnswerForm { ask, values } => answer(chat.answer_form(&ask, &values).await),
        ChatAct::AnswerQuestions { ask, responses } => {
            answer(chat.answer_questions(&ask, &responses).await)
        }
        ChatAct::ReplyInstead { ask, reply, so_far } => {
            answer(chat.reply_instead(&ask, &reply, &so_far).await)
        }
        ChatAct::ChangeSetting(change) => answer(chat.change_setting(&change).await),
        ChatAct::Withdraw(id) => answer(chat.withdraw(&id).await),
        ChatAct::SendNow(id) => answer(chat.send_now(&id).await),
        ChatAct::Resend(id) => answer(chat.resend(&id).await),
        ChatAct::Interrupt => answer(chat.interrupt().await),
        ChatAct::StopJob(key) => answer(chat.stop_job(&key).await),
        ChatAct::SwitchBack => answer(chat.switch_back().await),
        ChatAct::Resume(draft) => answer(chat.resume_with(&draft).await),
        ChatAct::PageOlder(n) => answer(chat.page_older(n).await),
    }
}

/// A queued or sent prompt as the `Draft` it came from, its words and every
/// attachment whole, or null JSON: what a withdraw or an edit puts back in
/// the composer.
///
/// # Safety
/// `chat` is from `amux_session_open`; `input_id` is a NUL-terminated JSON
/// byte array.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_draft_of(
    chat: *const AmuxChat,
    input_id: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let id: Vec<u8> = unsafe { parse(input_id) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe { read(chat, |chat| chat.draft_of(&id)) }
}

/// Forgets a not-confirmed input.
///
/// # Safety
/// `chat` is from `amux_session_open`; `input_id` is a NUL-terminated JSON
/// byte array.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_discard(chat: *const AmuxChat, input_id: *const c_char) {
    // SAFETY: the caller's contract.
    let id = unsafe { self::input_id(input_id) };
    // SAFETY: the caller's contract.
    let _ = unsafe { read(chat, |chat| chat.discard(&id)) };
}

/// Fetches the files changed for `comparison` (a JSON `Comparison`), with
/// no patch, as the overview opens; the ticket answers `{"Ok": Overview}` or
/// `{"Err": ..}`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `comparison` is a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_open_overview(
    chat: *const AmuxChat,
    comparison: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    let comparison: Comparison = unsafe { parse(comparison) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, move |chat| async move {
            Answered::from(chat.open_overview(comparison).await)
        })
    }
}

/// Asks the agent's host for its diff for `comparison` (a JSON
/// `Comparison`, or null for the uncommitted work) and the patch it names;
/// the ticket answers `{"Ok": FrozenReview}` or `{"Err": ..}`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `comparison` is a C string or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_review(
    chat: *const AmuxChat,
    comparison: *const c_char,
) -> u64 {
    // SAFETY: the caller's contract.
    let comparison: Comparison = unsafe { parse(comparison) }.unwrap_or_default();
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, move |chat| async move {
            Answered::from(chat.review(comparison).await)
        })
    }
}

/// The review page's document: a `FrozenReview`'s patch parsed into files
/// and hunks, with `comments` (a JSON array of `ReviewComment`) placed on
/// their lines. Null when either argument does not parse.
///
/// # Safety
/// The strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_review_doc(
    review: *const c_char,
    comments: *const c_char,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let review: Option<FrozenReview> = unsafe { parse(review) };
    // SAFETY: the caller's contract.
    let comments: Option<Vec<_>> = unsafe { parse(comments) };
    guard(std::ptr::null_mut(), || match (review, comments) {
        (Some(review), Some(comments)) => owned(&review.doc(&comments)),
        _ => std::ptr::null_mut(),
    })
}

/// Why an attachment of `size` bytes would be refused before anything of
/// it is read or stored, as a JSON string naming the file; JSON `null` when
/// it may go.
///
/// # Safety
/// `chat` is from `amux_session_open`; the strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_attachment_refusal(
    chat: *const AmuxChat,
    name: *const c_char,
    mime: *const c_char,
    size: u64,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let name = unsafe { text(name) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let mime = unsafe { text(mime) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    unsafe { read(chat, |chat| chat.attachment_refusal(&name, &mime, size)) }
}

/// Stores bytes to attach to a prompt; the ticket answers
/// `{"Ok": BlobRef}` or `{"Err": ..}`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `data` points to `len` bytes; the
/// strings are NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_put_blob(
    chat: *const AmuxChat,
    data: *const u8,
    len: usize,
    name: *const c_char,
    mime: *const c_char,
) -> u64 {
    let bytes = if data.is_null() {
        Vec::new()
    } else {
        // SAFETY: the caller's contract.
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    };
    // SAFETY: the caller's contract.
    let name = unsafe { text(name) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    let mime = unsafe { text(mime) }.unwrap_or_default().to_owned();
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, move |chat| async move {
            Answered::from(chat.put_blob(&name, &mime, bytes).await)
        })
    }
}

/// An attachment's bytes, by its hash as a JSON byte array, once fetched.
/// Empty until then: asking starts the fetch, and the rows that show it
/// change when it lands.
///
/// # Safety
/// `chat` is from `amux_session_open`; `hash` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_blob(
    chat: *const AmuxChat,
    hash: *const c_char,
) -> AmuxBytes {
    // SAFETY: the caller's contract.
    let hash: Vec<u8> = unsafe { parse(hash) }.unwrap_or_default();
    guard(AmuxBytes::empty(), || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return AmuxBytes::empty();
        };
        let _entered = open.shared.handle().enter();
        match open.chat.blob(&hash) {
            Some(bytes) => AmuxBytes::leak(&bytes),
            None => AmuxBytes::empty(),
        }
    })
}

/// Whether fetching an attachment's bytes failed, by its hash as a JSON byte
/// array.
///
/// # Safety
/// `chat` is from `amux_session_open`; `hash` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_blob_failed(
    chat: *const AmuxChat,
    hash: *const c_char,
) -> bool {
    // SAFETY: the caller's contract.
    let hash: Vec<u8> = unsafe { parse(hash) }.unwrap_or_default();
    guard(false, || {
        // SAFETY: the caller's contract.
        unsafe { held_chat(chat) }.is_some_and(|open| open.chat.blob_failed(&hash))
    })
}

/// Fetches an attachment's bytes again, after a failure. The rows that show
/// it change when it lands.
///
/// # Safety
/// `chat` is from `amux_session_open`; `hash` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_session_fetch_blob(chat: *const AmuxChat, hash: *const c_char) {
    // SAFETY: the caller's contract.
    let hash: Vec<u8> = unsafe { parse(hash) }.unwrap_or_default();
    guard((), || {
        // SAFETY: the caller's contract.
        if let Some(open) = unsafe { held_chat(chat) } {
            let _entered = open.shared.handle().enter();
            open.chat.fetch_blob(&hash);
        }
    })
}

// --- freeing ---------------------------------------------------------------

/// # Safety
/// `text` is null or a string this library returned, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_string_free(text: *mut c_char) {
    if !text.is_null() {
        // SAFETY: the caller's contract.
        drop(unsafe { CString::from_raw(text) });
    }
}

/// # Safety
/// `bytes` came from this library, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_bytes_free(bytes: AmuxBytes) {
    if !bytes.data.is_null() {
        // SAFETY: the caller's contract: this is the boxed slice we leaked.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(bytes.data, bytes.len)) });
    }
}
