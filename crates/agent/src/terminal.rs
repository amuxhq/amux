//! Typing into Claude's terminal. Claude's asks are answered through its
//! hooks, so what is typed is only what has no hook: a prompt, the keys
//! that interrupt and cycle the permission mode, and the folder-trust
//! dialog Claude shows before any hook runs. Those keys are Claude's own
//! and have not changed across the versions amux hosts, so they are fixed
//! here. One task types them in order, with the pauses Claude needs to
//! take each paste whole.

use std::io;
use std::time::Duration;

use interpret::claude_pty::TerminalInput;
use tokio::sync::mpsc;

const ESCAPE: &[u8] = b"\x1b";
const ENTER: &[u8] = b"\r";
const DOWN: &[u8] = b"\x1b[B";
const SHIFT_TAB: &[u8] = b"\x1b[Z";
const PASTE_BEGIN: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// One paste of about 1,100 characters reaches the model wrapped as pasted
/// content, which it will not take instructions from; one of about 750 is
/// kept as typed. A longer prompt goes as several pastes of at most this.
const PASTE_CHARS: usize = 700;
const BETWEEN_PASTES: Duration = Duration::from_millis(100);
/// Claude takes a paste's end, then Enter, as one submission only once it
/// has drawn the paste.
const AFTER_PASTE: Duration = Duration::from_millis(400);
const AFTER_MOVE: Duration = Duration::from_millis(300);

/// Bytes to write, or a pause before the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyStep {
    Write(Vec<u8>),
    Delay(Duration),
}

/// The keystrokes for a semantic input; `text` is a prompt's, its
/// attachments written in.
pub fn steps(input: &TerminalInput, text: &str) -> io::Result<Vec<KeyStep>> {
    Ok(match input {
        TerminalInput::Prompt { .. } | TerminalInput::SendNow { .. } => prompt(text)?,
        TerminalInput::Clear => prompt("/clear")?,
        TerminalInput::Interrupt => vec![KeyStep::Write(ESCAPE.to_vec())],
        TerminalInput::CyclePermissionMode => vec![KeyStep::Write(SHIFT_TAB.to_vec())],
        // The trust dialog preselects No: Enter exits, Down and Enter
        // trusts the folder.
        TerminalInput::Trust { trust: true } => vec![
            KeyStep::Write(DOWN.to_vec()),
            KeyStep::Delay(AFTER_MOVE),
            KeyStep::Write(ENTER.to_vec()),
        ],
        TerminalInput::Trust { trust: false } => vec![KeyStep::Write(ENTER.to_vec())],
    })
}

/// Pastes `text` and submits it. Text with control characters other than
/// line breaks is refused: they would reach Claude as keys.
pub fn prompt(text: &str) -> io::Result<Vec<KeyStep>> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if text.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a prompt must not be empty",
        ));
    }
    if text
        .chars()
        .any(|character| character.is_control() && character != '\n')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a prompt must not contain control characters",
        ));
    }
    let mut steps = Vec::new();
    for (index, piece) in paste_pieces(&text, PASTE_CHARS).into_iter().enumerate() {
        if index > 0 {
            steps.push(KeyStep::Delay(BETWEEN_PASTES));
        }
        let mut bytes = PASTE_BEGIN.to_vec();
        bytes.extend_from_slice(piece.as_bytes());
        bytes.extend_from_slice(PASTE_END);
        steps.push(KeyStep::Write(bytes));
    }
    steps.push(KeyStep::Delay(AFTER_PASTE));
    steps.push(KeyStep::Write(ENTER.to_vec()));
    Ok(steps)
}

/// `text` in runs of at most `limit` characters. Each cut falls between
/// two characters that are not whitespace where the run has such a place,
/// so no piece starts or ends with a space or a line break for Claude to
/// trim from a paste.
fn paste_pieces(text: &str, limit: usize) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut rest = text;
    loop {
        let head: Vec<(usize, char)> = rest.char_indices().take(limit + 1).collect();
        if head.len() <= limit {
            break;
        }
        let cut = (1..=limit)
            .rev()
            .find(|&at| !head[at - 1].1.is_whitespace() && !head[at].1.is_whitespace())
            .unwrap_or(limit);
        let (piece, after) = rest.split_at(head[cut].0);
        pieces.push(piece);
        rest = after;
    }
    pieces.push(rest);
    pieces
}

/// Starts the task that types into the terminal. Keystrokes go in the
/// order they were handed over; a pause holds everything after it without
/// holding the agent.
pub fn typist(terminal: pty_host::PtyHandle) -> mpsc::UnboundedSender<Vec<KeyStep>> {
    let (keys, mut typed) = mpsc::unbounded_channel::<Vec<KeyStep>>();
    tokio::spawn(async move {
        while let Some(steps) = typed.recv().await {
            for step in steps {
                match step {
                    KeyStep::Write(bytes) => {
                        if terminal.write(&bytes).await.is_err() {
                            return;
                        }
                    }
                    KeyStep::Delay(pause) => tokio::time::sleep(pause).await,
                }
            }
        }
    });
    keys
}

/// A message pasted into the terminal when the messaging socket cannot
/// take it, wrapped as Claude wraps what arrives on the socket, so the
/// interpreter reads its reflection the same way.
pub fn pasted_message(text: &str) -> String {
    format!("<cross-session-message from=\"amux\">\n{text}\n</cross-session-message>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(steps: &[KeyStep]) -> Vec<Vec<u8>> {
        steps
            .iter()
            .filter_map(|step| match step {
                KeyStep::Write(bytes) => Some(bytes.clone()),
                KeyStep::Delay(_) => None,
            })
            .collect()
    }

    #[test]
    fn a_prompt_is_one_paste_then_enter() {
        assert_eq!(
            prompt("hello\r\nthere").expect("keys"),
            [
                KeyStep::Write(b"\x1b[200~hello\nthere\x1b[201~".to_vec()),
                KeyStep::Delay(AFTER_PASTE),
                KeyStep::Write(b"\r".to_vec()),
            ]
        );
    }

    #[test]
    fn a_long_prompt_is_pasted_in_pieces_claude_keeps_as_typed() {
        let text = "word ".repeat(300).trim_end().to_owned();
        let pastes = written(&prompt(&text).expect("keys"));
        assert_eq!(pastes.len(), 4, "three pastes and Enter");
        let rejoined: String = pastes[..3]
            .iter()
            .map(|paste| {
                let paste = std::str::from_utf8(paste).expect("utf-8");
                let inner = paste
                    .strip_prefix("\x1b[200~")
                    .and_then(|paste| paste.strip_suffix("\x1b[201~"))
                    .expect("a bracketed paste");
                assert!(inner.chars().count() <= PASTE_CHARS);
                inner.to_owned()
            })
            .collect();
        assert_eq!(rejoined, text);
    }

    #[test]
    fn paste_pieces_cut_between_letters_and_rejoin_exactly() {
        let text = "ab cd ef";
        let pieces = paste_pieces(text, 4);
        assert_eq!(pieces.concat(), text);
        assert!(
            pieces
                .iter()
                .all(|piece| !piece.starts_with(' ') && !piece.ends_with(' '))
        );
    }

    #[test]
    fn a_prompt_with_control_characters_is_refused() {
        assert!(prompt("ok\x1b[Bno").is_err());
        assert!(prompt("  \n ").is_err());
    }

    #[test]
    fn trusting_the_folder_moves_to_yes_before_enter() {
        assert_eq!(
            written(&steps(&TerminalInput::Trust { trust: true }, "").expect("keys")),
            [b"\x1b[B".to_vec(), b"\r".to_vec()]
        );
        assert_eq!(
            written(&steps(&TerminalInput::Trust { trust: false }, "").expect("keys")),
            [b"\r".to_vec()]
        );
    }
}
