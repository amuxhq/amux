//! When terminal Claude first takes a prompt, read from its output.
//!
//! Claude reads keys once bracketed paste is on, but turning it on is not
//! enough: 2.1.283 turns it on as its very first output, queries the
//! terminal, turns it off and on again while it resets its input, and only
//! then draws. Keys typed before the reset are dropped. 2.1.251 turns it on
//! once and draws later. What both share: the first text Claude draws with
//! bracketed paste on is its first screen, and its input is live by then.
//! The SessionStart hook is no signal on its own: 2.1.251 runs it only after
//! the first prompt.

/// Watches terminal output for the first text drawn while bracketed paste
/// is on. Escape sequences, including window titles, are not drawn text.
#[derive(Debug, Default)]
pub(crate) struct InputLive {
    state: Scan,
    /// A control sequence's parameters, as far as they matter here.
    params: Vec<u8>,
    paste: bool,
    seen: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Scan {
    #[default]
    Ground,
    Escape,
    /// ESC with intermediates (`ESC ( B` and the like).
    EscapeIntermediate,
    Csi,
    /// OSC, DCS, SOS, PM or APC: runs to BEL or ST.
    String,
    StringEscape,
}

/// Longer parameters than any mode set carries are not a mode set this
/// watcher needs; they stop being kept.
const PARAMS_KEPT: usize = 64;

impl InputLive {
    /// True the first time drawn text follows bracketed paste turning on.
    pub(crate) fn watch(&mut self, bytes: &[u8]) -> bool {
        if self.seen {
            return false;
        }
        for &byte in bytes {
            self.state = match self.state {
                Scan::Ground => match byte {
                    0x1b => Scan::Escape,
                    0x20..=0x7e | 0x80.. => {
                        if self.paste {
                            self.seen = true;
                            self.params = Vec::new();
                            return true;
                        }
                        Scan::Ground
                    }
                    _ => Scan::Ground,
                },
                Scan::Escape => match byte {
                    b'[' => {
                        self.params.clear();
                        Scan::Csi
                    }
                    b']' | b'P' | b'X' | b'^' | b'_' => Scan::String,
                    0x20..=0x2f => Scan::EscapeIntermediate,
                    0x1b => Scan::Escape,
                    _ => Scan::Ground,
                },
                Scan::EscapeIntermediate => match byte {
                    0x20..=0x2f => Scan::EscapeIntermediate,
                    0x1b => Scan::Escape,
                    _ => Scan::Ground,
                },
                Scan::Csi => match byte {
                    0x20..=0x3f => {
                        if self.params.len() < PARAMS_KEPT {
                            self.params.push(byte);
                        }
                        Scan::Csi
                    }
                    0x40..=0x7e => {
                        self.mode_set(byte);
                        Scan::Ground
                    }
                    0x1b => Scan::Escape,
                    _ => Scan::Ground,
                },
                Scan::String => match byte {
                    0x07 => Scan::Ground,
                    0x1b => Scan::StringEscape,
                    _ => Scan::String,
                },
                Scan::StringEscape => match byte {
                    b'\\' => Scan::Ground,
                    0x1b => Scan::StringEscape,
                    _ => Scan::String,
                },
            };
        }
        false
    }

    /// `CSI ? … h` sets private modes, `CSI ? … l` resets them; 2004 is
    /// bracketed paste.
    fn mode_set(&mut self, last: u8) {
        let on = match last {
            b'h' => true,
            b'l' => false,
            _ => return,
        };
        let Some(modes) = self.params.strip_prefix(b"?") else {
            return;
        };
        if modes
            .split(|byte| *byte == b';')
            .any(|mode| mode == b"2004")
        {
            self.paste = on;
        }
    }
}

/// Watches terminal output for Claude's folder-trust dialog: "Is this a
/// project you created or one you trust?" over "No, exit" and "Yes, I trust
/// this folder". Claude places each word with a cursor move, so drawn text
/// is compared with escapes and spaces taken out.
#[derive(Debug, Default)]
pub(crate) struct TrustDialog {
    state: Scan,
    /// The newest drawn bytes, spaces left out.
    tail: Vec<u8>,
}

/// The dialog's Yes choice as drawn, spaces left out.
const TRUST_CHOICE: &[u8] = b"Yes,Itrustthisfolder";

impl TrustDialog {
    /// True when these bytes finish drawing the dialog's Yes choice.
    pub(crate) fn watch(&mut self, bytes: &[u8]) -> bool {
        let mut found = false;
        for &byte in bytes {
            self.state = match self.state {
                Scan::Ground => match byte {
                    0x1b => Scan::Escape,
                    0x21..=0x7e | 0x80.. => {
                        self.tail.push(byte);
                        if self.tail.len() > 2 * TRUST_CHOICE.len() {
                            self.tail.drain(..self.tail.len() - TRUST_CHOICE.len());
                        }
                        found |= self.tail.ends_with(TRUST_CHOICE);
                        Scan::Ground
                    }
                    _ => Scan::Ground,
                },
                Scan::Escape => match byte {
                    b'[' => Scan::Csi,
                    b']' | b'P' | b'X' | b'^' | b'_' => Scan::String,
                    0x20..=0x2f => Scan::EscapeIntermediate,
                    0x1b => Scan::Escape,
                    _ => Scan::Ground,
                },
                Scan::EscapeIntermediate => match byte {
                    0x20..=0x2f => Scan::EscapeIntermediate,
                    0x1b => Scan::Escape,
                    _ => Scan::Ground,
                },
                Scan::Csi => match byte {
                    0x20..=0x3f => Scan::Csi,
                    0x1b => Scan::Escape,
                    _ => Scan::Ground,
                },
                Scan::String => match byte {
                    0x07 => Scan::Ground,
                    0x1b => Scan::StringEscape,
                    _ => Scan::String,
                },
                Scan::StringEscape => match byte {
                    b'\\' => Scan::Ground,
                    0x1b => Scan::StringEscape,
                    _ => Scan::String,
                },
            };
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claude 2.1.283's trust dialog as it drew it on an untrusted folder,
    /// split across reads mid-escape and mid-word.
    #[test]
    fn the_trust_dialog_is_seen_across_reads() {
        let drawn: &[u8] = b"\x1b[?2004h\x1b[2G\x1b[38;2;177;185;249m\xe2\x9d\xaf\x1b[4GNo,\x1b[8Gexit\x1b[39m\r\r\n\x1b[4GYes,\x1b[9GI\x1b[11Gtrust\x1b[17Gthis\x1b[22Gfolder\r\r\n\x1b]8;;\x07";
        for split in 1..drawn.len() {
            let mut trust = TrustDialog::default();
            let seen = trust.watch(&drawn[..split]) | trust.watch(&drawn[split..]);
            assert!(seen, "split at {split}");
        }
        let mut trust = TrustDialog::default();
        assert!(!trust.watch(b"Yes, I trust this code\r\n\x1b[4Gfolder"));
    }

    fn fires_at(chunks: &[&[u8]]) -> Option<usize> {
        let mut live = InputLive::default();
        chunks.iter().position(|chunk| live.watch(chunk))
    }

    #[test]
    fn claude_2_1_283_is_live_at_its_first_screen_after_the_reset() {
        // Its startup as a terminal sees it, one read per line.
        let startup: &[&[u8]] = &[
            b"\x1b7\x1b[r\x1b8\x1b[?25h",
            b"\x1b[?25l",
            b"\x1b[?2004h\x1b[?2031h\x1b[?1004h",
            b"\x1b[<u\x1b[>5u\x1b[>4;2m",
            b"\x1b[>0q\x1b[?u",
            b"\x1b[c",
            b"\x1b[>4m\x1b[<u\x1b[?1004l\x1b[?2031l\x1b[?2004l",
            b"\x1b[?2004h\x1b[?2031h\x1b[?1004h\x1b[<u\x1b[>5u\x1b[>4;2m",
            b"\x1b]0;\xe2\x9c\xb3 Claude Code\x07",
            b"\x1b[38;2;215;119;87m \xe2\x96\x90\x1b[1mClaude\x1b[19GCode",
        ];
        assert_eq!(fires_at(startup), Some(9));
    }

    #[test]
    fn claude_2_1_251_is_live_at_its_first_screen() {
        let startup: &[&[u8]] = &[
            b"\x1b7\x1b[r\x1b8\x1b[?25h",
            b"\x1b[?25l",
            b"\x1b[?2004h\x1b[?1004h\x1b[?2031h",
            b"\x1b[<u\x1b[>5u\x1b[>4;2m",
            b"\x1b[>0q\x1b[c",
            b"\x1b[?1049h\x1b[2J\x1b[H\x1b[?1000h",
            b"\x1b]0;\xe2\x9c\xb3 Claude Code\x07",
            b"\x1b]9;4;0;\x07",
            b"\x1b[?2026h\x1b[H\r\x1b[1C\x1b[1B\xe2\x96\x90\xe2\x96\x9b",
        ];
        assert_eq!(fires_at(startup), Some(8));
    }

    #[test]
    fn text_before_bracketed_paste_is_not_the_prompt_screen() {
        assert_eq!(
            fires_at(&[b"starting\r\n", b"\x1b[?2004h", b"\x1b[1mready"]),
            Some(2)
        );
    }

    #[test]
    fn a_sequence_split_across_reads_is_still_read() {
        assert_eq!(
            fires_at(&[b"\x1b[?20", b"04h\x1b]0;tit", b"le\x1b", b"\\", b">"]),
            Some(4)
        );
        assert_eq!(fires_at(&[b"\x1b[?1004;2004h", b"x"]), Some(1));
    }

    #[test]
    fn it_fires_once() {
        let mut live = InputLive::default();
        assert!(live.watch(b"\x1b[?2004hone"));
        assert!(!live.watch(b"\x1b[?2004l\x1b[?2004htwo"));
    }

    /// Every recorded session is live before the recorder, which waited
    /// for Claude's screen, typed its first key.
    #[test]
    fn recorded_sessions_are_live_at_their_first_screen() {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../claude-specs/fixtures/live/pty");
        let mut checked = 0;
        for entry in std::fs::read_dir(&fixtures).expect("pty fixtures") {
            let path = entry.expect("fixture").path().join("io.jsonl");
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut live = InputLive::default();
            let mut fired = false;
            for line in text.lines() {
                let line: serde_json::Value = serde_json::from_str(line).expect("io line");
                let Some(hex) = line["line"]
                    .as_str()
                    .and_then(|line| line.strip_prefix("hex:"))
                else {
                    continue;
                };
                if line["dir"] == "stdin" {
                    break;
                }
                let bytes: Vec<u8> = (0..hex.len())
                    .step_by(2)
                    .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
                    .collect();
                if live.watch(&bytes) {
                    fired = true;
                    break;
                }
            }
            assert!(fired, "{}: live before the first key", path.display());
            checked += 1;
        }
        assert!(
            checked > 0,
            "no recorded sessions under {}",
            fixtures.display()
        );
    }
}
