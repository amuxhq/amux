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
use std::sync::Arc;

use app_runtime::ReviewSession;
use app_runtime::values::{ReviewChange, ReviewChoice, ReviewOutcome};
use serde::Deserialize;
use ui_view::ReviewContextAsk;

use crate::{AmuxChat, Chat, act, guard, held_chat, owned, parse, text};

/// What an act on the review page answers while none is open.
fn not_open() -> ReviewOutcome {
    ReviewOutcome::Failed("no review page is open on the chat".into())
}

/// Starts an act on the chat's open review page.
///
/// # Safety
/// `chat` is from `amux_session_open`.
unsafe fn page_act<F, Fut>(chat: *const AmuxChat, act: F) -> u64
where
    F: FnOnce(Arc<ReviewSession>) -> Fut + Send + 'static,
    Fut: Future<Output = ReviewOutcome> + Send + 'static,
{
    // SAFETY: the caller's contract.
    unsafe {
        self::act(chat, move |chat| async move {
            match chat.review_page() {
                Some(page) => act(page).await,
                None => not_open(),
            }
        })
    }
}

/// Runs `call` on an open chat inside its runtime.
///
/// # Safety
/// `chat` is null or from `amux_session_open`.
unsafe fn on_chat<T: Copy>(
    chat: *const AmuxChat,
    failed: T,
    call: impl FnOnce(&Arc<Chat>) -> T,
) -> T {
    guard(failed, || {
        // SAFETY: the caller's contract.
        let Some(open) = (unsafe { held_chat(chat) }) else {
            return failed;
        };
        let _entered = open.shared.handle().enter();
        call(&open.chat)
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
    let base_branch = unsafe { text(base_branch) }.map(str::to_owned);
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, move |chat| async move {
            chat.open_review(base_branch, refresh).await
        })
    }
}

/// What a sent review's attachment names of its round, as the chat's rows
/// carry it (`SentReviewRef`).
#[derive(Deserialize)]
struct SentRound {
    review_id: Vec<u8>,
    comment_ids: Vec<Vec<u8>>,
}

/// Opens the round a review sent into the chat closed: `sent` is the
/// `SentReviewRef` its attachment draws with, as JSON. The ticket answers
/// the round's `ReviewPage`, its whole diff and the comments it sent, read
/// from the agent's host (or with the host out of reach, from what was
/// loaded, saying so: with none of it loaded, a page with no header and
/// only that banner), or JSON null when the host no longer holds the round
/// and the chip shows its text instead. It opens no page on the chat and
/// moves none open there.
///
/// # Safety
/// `chat` is from `amux_session_open`; `sent` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn amux_review_round(chat: *const AmuxChat, sent: *const c_char) -> u64 {
    // SAFETY: the caller's contract.
    let sent: Option<SentRound> = unsafe { parse(sent) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, move |chat| async move {
            let sent = sent?;
            chat.review_round(&sent.review_id, &sent.comment_ids).await
        })
    }
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
    let paths: Option<Vec<String>> = if paths.is_null() {
        Some(Vec::new())
    } else {
        // SAFETY: the caller's contract.
        unsafe { parse(paths) }
    };
    // SAFETY: the caller's contract.
    unsafe {
        page_act(chat, move |page| async move {
            match paths {
                Some(paths) => page.files(paths).await,
                None => ReviewOutcome::Failed("the paths are not a JSON array of strings".into()),
            }
        })
    }
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
    let ask: Option<ReviewContextAsk> = unsafe { parse(ask) };
    // SAFETY: the caller's contract.
    unsafe {
        page_act(chat, move |page| async move {
            match ask {
                Some(ask) => page.context(ask).await,
                None => ReviewOutcome::Failed("the ask is not a ReviewContextAsk".into()),
            }
        })
    }
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
    let changes: Option<Vec<ReviewChange>> = unsafe { parse(changes) };
    // SAFETY: the caller's contract.
    unsafe {
        act(chat, move |chat| async move {
            match changes {
                Some(changes) => chat.edit_review(changes).await,
                None => ReviewOutcome::Failed("the changes are not ReviewChanges".into()),
            }
        })
    }
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
    let Some(choice) = (unsafe { parse::<ReviewChoice>(choice) }) else {
        return;
    };
    // SAFETY: the caller's contract.
    unsafe {
        on_chat(chat, (), |chat| {
            if let Some(page) = chat.review_page() {
                page.choose(choice);
            }
        })
    }
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
        on_chat(chat, std::ptr::null_mut(), |chat| {
            owned(&chat.review_page().map(|page| page.read()))
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
        on_chat(chat, std::ptr::null_mut(), |chat| {
            owned(
                &chat
                    .review_page()
                    .and_then(|page| page.anchor(from as usize, to as usize)),
            )
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
    unsafe { on_chat(chat, (), |chat| chat.close_review()) }
}
