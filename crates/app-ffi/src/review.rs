//! The review page open on a chat: `amux_review_*`, on an `AmuxChat`.
//!
//! `amux_review_open` opens the agent's review page on the chat; from then
//! the chat's deltas carry the page whole whenever it moved (`review`),
//! until `amux_review_close`. The app draws the page as handed: it asks for
//! the bodies the page lists in `to_fetch`, for more context from a gap's
//! asks, and turns a selection of rows into an anchor here. Acts answer by
//! ticket with a `ReviewOutcome`, after the step that hands over what they
//! moved. The chip comes with the chat's frame (`review_chip`), and a send
//! with the review is the chat's `SendWithReview` act. A review sent
//! into the chat opens its round by `amux_review_round`, apart from the
//! page.

use std::ffi::c_char;

use crate::{AmuxChat, guard, held_chat, into_c_string, text};

/// Runs `call` on an open chat, answering `failed` for a null chat or a
/// panic.
///
/// # Safety
/// `chat` is null or from `amux_session_open`.
unsafe fn on_chat<T: Copy>(
    chat: *const AmuxChat,
    failed: T,
    call: impl FnOnce(&AmuxChat) -> T,
) -> T {
    guard(failed, || {
        // SAFETY: the caller's contract.
        match unsafe { held_chat(chat) } {
            Some(open) => call(open),
            None => failed,
        }
    })
}

/// Opens the agent's review page on the chat, or opens the open page's
/// review again. `base_branch` (null to keep the review's) picks its base
/// anew; `refresh` takes a snapshot even with drafts, moving them onto it.
/// The ticket answers a `ReviewOutcome`; the page itself comes with the
/// chat's deltas, and says too what the open learned.
///
/// # Safety
/// `chat` is from `amux_session_open`; `base_branch` is null or a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_review_open(
    chat: *const AmuxChat,
    base_branch: *const c_char,
    refresh: bool,
) -> u64 {
    // SAFETY: the caller's contract.
    let base_branch = unsafe { text(base_branch) };
    // SAFETY: the caller's contract.
    unsafe { on_chat(chat, 0, |open| open.0.review_open(base_branch, refresh)) }
}

/// Opens the round a review sent into the chat closed: `sent` is the
/// `SentReviewRef` its attachment draws with, as JSON. The ticket answers
/// the round's `ReviewPage`, its whole diff and the comments it sent, read
/// from the agent's host (or with the host out of reach, from what was
/// loaded, saying so: with none of it loaded, a page with no header and
/// only that banner), or JSON null when the message closed no round or the
/// host no longer holds it, and the chip shows its text instead. It opens
/// no page on the chat and moves none open there.
///
/// # Safety
/// `chat` is from `amux_session_open`; `sent` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_review_round(chat: *const AmuxChat, sent: *const c_char) -> u64 {
    // SAFETY: the caller's contract.
    let sent = unsafe { text(sent) };
    // SAFETY: the caller's contract.
    unsafe { on_chat(chat, 0, |open| open.0.review_round(sent)) }
}

/// Fetches file bodies for the view the page shows: `paths` as a JSON
/// array of strings, those on screen first, or null for every file the
/// page lists in `to_fetch`. The page moves as each batch lands; the
/// ticket answers a `ReviewOutcome` once all have.
///
/// # Safety
/// `chat` is from `amux_session_open`; `paths` is null or a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_review_files(chat: *const AmuxChat, paths: *const c_char) -> u64 {
    // Null asks for every file the page lists; a list that is not UTF-8 is
    // not a JSON array of strings, and fails the act.
    let paths = if paths.is_null() {
        None
    } else {
        // SAFETY: the caller's contract.
        Some(unsafe { text(paths) }.unwrap_or_default())
    };
    // SAFETY: the caller's contract.
    unsafe { on_chat(chat, 0, |open| open.0.review_files(paths)) }
}

/// Loads more context: a `ReviewContextAsk` as JSON, one of a gap's asks.
/// The gap draws as loading until the lines arrive; the ticket answers a
/// `ReviewOutcome`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `ask` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_review_context(chat: *const AmuxChat, ask: *const c_char) -> u64 {
    // SAFETY: the caller's contract.
    let ask = unsafe { text(ask) };
    // SAFETY: the caller's contract.
    unsafe { on_chat(chat, 0, |open| open.0.review_context(ask)) }
}

/// Changes the review at the agent's host: a JSON array of `ReviewChange`,
/// made all or none. Works with no page open too, as the chip's ✕
/// discarding drafts. The ticket answers a `ReviewOutcome`.
///
/// # Safety
/// `chat` is from `amux_session_open`; `changes` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_review_edit(chat: *const AmuxChat, changes: *const c_char) -> u64 {
    // SAFETY: the caller's contract.
    let changes = unsafe { text(changes) };
    // SAFETY: the caller's contract.
    unsafe { on_chat(chat, 0, |open| open.0.review_edit(changes)) }
}

/// Takes a `ReviewChoice`, as JSON: a view switched to, a file folded or
/// opened, an outdated comment opened or folded. The next step hands over
/// the page drawn so. Nothing happens with no page open.
///
/// # Safety
/// `chat` is from `amux_session_open`; `choice` is a NUL-terminated
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_review_choose(chat: *const AmuxChat, choice: *const c_char) {
    // SAFETY: the caller's contract.
    let choice = unsafe { text(choice) };
    // SAFETY: the caller's contract.
    unsafe { on_chat(chat, (), |open| open.0.review_choose(choice)) }
}

/// The open review page as a `ReviewPage`, or JSON null with none open:
/// for a model opening on a page already open. It is the page handed over
/// from then, which anchors count rows of.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_review_page(chat: *const AmuxChat) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe {
        on_chat(chat, std::ptr::null_mut(), |open| {
            into_c_string(open.0.review_page())
        })
    }
}

/// Where a comment on a selection of the page's rows goes: the rows from
/// index `from` to `to`, in either order, of the page handed over last, as
/// a `ReviewAnchor` for a `Comment` change; JSON null for a selection with
/// no line, across files, or with no page open.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_review_anchor(
    chat: *const AmuxChat,
    from: u32,
    to: u32,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe {
        on_chat(chat, std::ptr::null_mut(), |open| {
            into_c_string(open.0.review_anchor(from, to))
        })
    }
}

/// Closes the review page open on the chat: it stops following the agent's
/// row, and no delta carries it again. Its acts in flight are answered all
/// the same.
///
/// # Safety
/// `chat` is from `amux_session_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_review_close(chat: *const AmuxChat) {
    // SAFETY: the caller's contract.
    unsafe { on_chat(chat, (), |open| open.0.review_close()) }
}
