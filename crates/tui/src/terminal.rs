//! Terminal ownership: alternate screen, raw mode, and guaranteed restore.
//!
//! The chrome is alt-screen only and never writes terminal scrollback.
//! Restore is RAII-backed with a process-global panic hook that restores
//! before reporting — guaranteed on orderly exits, best-effort on unwind,
//! and nothing survives SIGKILL. The byte-emitting halves are generic over
//! `Write` so a test can assert the exact sequences.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Once, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::style::ResetColor;
use crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
    disable_raw_mode, enable_raw_mode,
};

use crate::theme::TerminalColors;

/// True while the chrome owns the terminal (alt screen + raw mode). The
/// panic hook restores iff set, and clears it so restore runs once.
static CHROME_OWNS_TERMINAL: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK: Once = Once::new();

/// Whether the terminal answered the kitty keyboard-enhancement probe —
/// probed once per process (the terminal does not change under us), on
/// the first chrome entry, inside the guard lifecycle. Tier gate for the
/// kitty-only chords (Ctrl+Enter, Shift+Enter): hints and the `?`
/// overlay derive from it; dispatch itself trusts the events that
/// actually arrive.
static KITTY_SUPPORTED: OnceLock<bool> = OnceLock::new();

/// Whether enhancement flags are currently pushed (on the alternate
/// screen's stack). Restore pops iff set — kitty keeps per-screen flag
/// stacks, so the pop must happen before leaving the alternate screen.
static KITTY_PUSHED: AtomicBool = AtomicBool::new(false);

/// Whether Shift+Enter reaches the client as itself rather than as Enter:
/// the terminal answered the keyboard-enhancement probe. Until the probe
/// has run (a frame drawn without a terminal) it is taken to.
pub fn shift_enter_reported() -> bool {
    KITTY_SUPPORTED.get().copied().unwrap_or(true)
}

/// Bytes that put the terminal into chrome mode (alternate screen, hidden
/// cursor, bracketed paste, and mouse capture. Bracketed paste prevents a
/// pasted CR from submitting a partial prompt; mouse capture lets the
/// alternate-screen feed own wheel events. Raw mode is termios, not bytes,
/// and is handled by the guard.
pub fn write_enter_chrome(out: &mut impl Write) -> io::Result<()> {
    crossterm::execute!(
        out,
        EnterAlternateScreen,
        Hide,
        EnableBracketedPaste,
        EnableMouseCapture
    )
}

/// Bytes that open one frame: the terminal holds what follows until
/// [`write_end_frame`] and then shows it whole, and the cursor stays hidden
/// while the frame's cells are written. Without this a terminal can show a
/// frame part-drawn, with the cursor caught wherever the last cell went,
/// which reads as the composer's cursor jumping around the chat while it
/// scrolls. Terminals without synchronized output ignore the request, and
/// hiding the cursor still keeps it out of sight there.
pub fn write_begin_frame(out: &mut impl Write) -> io::Result<()> {
    crossterm::queue!(out, BeginSynchronizedUpdate, Hide)
}

const MODIFY_OTHER_KEYS_OFF: &[u8] = b"\x1b[>4m";

/// Bytes that close a frame opened by [`write_begin_frame`].
pub fn write_end_frame(out: &mut impl Write) -> io::Result<()> {
    crossterm::execute!(out, EndSynchronizedUpdate)
}

/// Bytes that restore the terminal from chrome mode. Input modes are
/// disabled before the alternate screen is left, the cursor is shown, and
/// the text style is reset last, on the screen the shell will draw on.
/// Focus reporting, xterm's modifyOtherKeys and the style reset are more
/// than the chrome itself sets: the same bytes put the terminal back after
/// a raw passthrough, where the agent may have switched on anything. An
/// agent that leaves modifyOtherKeys on (terminal Claude does) makes every
/// Ctrl chord arrive as `CSI 27 ; 5 ; key ~`, which the client's key reader
/// does not read. Every exit path — orderly, error, panic, signal — must
/// emit these.
pub fn write_restore(out: &mut impl Write) -> io::Result<()> {
    // `CSI > 4 m` resets modifyOtherKeys; crossterm has no command for it.
    out.write_all(MODIFY_OTHER_KEYS_OFF)?;
    crossterm::execute!(
        out,
        DisableMouseCapture,
        DisableFocusChange,
        DisableBracketedPaste,
        Show,
        LeaveAlternateScreen,
        ResetColor
    )
}

/// Maximum source payload accepted by [`write_osc52`]. Keeping the bound
/// before base64 encoding avoids an unbounded terminal control sequence while
/// preserving the copied text exactly below the limit.
const OSC52_MAX_BYTES: usize = 100 * 1024;

/// Emit an OSC 52 clipboard sequence for `text`.
///
/// Payloads above 100 KiB are truncated at a UTF-8 character boundary. The
/// returned notice lets the event loop tell the user when that happened.
pub fn write_osc52(out: &mut impl Write, text: &str) -> io::Result<Option<String>> {
    let mut end = text.len().min(OSC52_MAX_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = end < text.len();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&text.as_bytes()[..end]);

    out.write_all(b"\x1b]52;c;")?;
    out.write_all(encoded.as_bytes())?;
    out.write_all(b"\x07")?;
    out.flush()?;

    Ok(truncated.then(|| "copied first 100 KiB to clipboard (message truncated)".to_string()))
}

/// The restore as literal bytes: pop keyboard-enhancement flags (before
/// leaving the alternate screen — kitty keeps per-screen stacks; a pop with
/// nothing pushed is a no-op, and unknown-CSI-tolerant terminals ignore it),
/// reset modifyOtherKeys, disable mouse capture, focus reporting and
/// bracketed paste, show the
/// cursor, leave the alternate screen, then reset the text style —
/// mirroring [`restore_now`] (locked to crossterm's actual bytes by a unit
/// test below). Written verbatim by the async-signal-safe handler, and by
/// the CLI's raw attach when it hands the terminal back: the agent behind
/// the passthrough may have switched any of these on, and its own restore is
/// lost when it dies or the client detaches while it is still running. Every
/// sequence is a no-op on a terminal that is already sane, which is what
/// makes writing it unconditionally safe.
pub const RESTORE_BYTES: &[u8] = b"\x1b[<1u\x1b[>4m\x1b[?1006l\x1b[?1015l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1004l\x1b[?2004l\x1b[?25h\x1b[?1049l\x1b[0m";

fn restore_now() {
    // Pop the keyboard-enhancement flags first, while still on the
    // alternate screen (kitty keeps per-screen flag stacks). Guarded by
    // the pushed flag so legacy-Windows consoles never see the CSI.
    if KITTY_PUSHED.swap(false, Ordering::SeqCst) {
        let _ = crossterm::execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = disable_raw_mode();
    let _ = write_restore(&mut io::stdout());
}

/// Install (once) a panic hook that restores the terminal before the
/// default hook reports — a panic report on the alternate screen would
/// vanish with it.
pub fn install_panic_hook() {
    #[cfg(unix)]
    signal_restore::install();
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if CHROME_OWNS_TERMINAL.swap(false, Ordering::SeqCst) {
                restore_now();
            }
            previous(info);
        }));
    });
}

/// RAII terminal ownership for one chrome session.
pub struct TerminalGuard {
    restored: bool,
}

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        if let Err(error) = write_enter_chrome(&mut io::stdout()) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        CHROME_OWNS_TERMINAL.store(true, Ordering::SeqCst);
        // Kitty keyboard protocol, feature-detected: probe once per process — the query
        // needs raw mode and rides crossterm's internal event reader —
        // then push the disambiguate flag each session so Ctrl+Enter and
        // Shift+Enter arrive distinguishable. Pushed on the alternate
        // screen, popped by every restore path before leaving it.
        let supported = *KITTY_SUPPORTED
            .get_or_init(|| crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false));
        if supported
            && crossterm::execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )
            .is_ok()
        {
            KITTY_PUSHED.store(true, Ordering::SeqCst);
        }
        Ok(Self { restored: false })
    }

    /// Orderly restore (also runs on drop; calling it explicitly makes the
    /// handoff points read as what they are).
    pub fn restore(mut self) {
        self.restore_once();
    }

    fn restore_once(&mut self) {
        if !self.restored {
            self.restored = true;
            CHROME_OWNS_TERMINAL.store(false, Ordering::SeqCst);
            restore_now();
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore_once();
    }
}

/// Async-signal-safe terminal restore on SIGINT/SIGTERM/SIGHUP.
///
/// A tokio signal arm in the chrome loop would leave the process deaf to
/// signals while suspended for an attach (tokio's handler stays installed
/// process-wide, but nothing polls the stream mid-attach). Instead: a
/// low-level handler that restores the saved cooked termios (`tcsetattr`
/// is async-signal-safe), writes the leave-alt-screen/show-cursor bytes
/// (raw `write` is async-signal-safe), then re-raises the default
/// disposition so the process still dies exactly as expected. The restore
/// is deliberately unconditional: on a terminal that is already sane it is
/// a no-op, and that is what lets it cover both chrome mode and the
/// mid-attach passthrough phase.
#[cfg(unix)]
mod signal_restore {
    use std::sync::{Once, OnceLock};

    static INSTALL: Once = Once::new();
    static SAVED_TERMIOS: OnceLock<libc::termios> = OnceLock::new();

    use super::RESTORE_BYTES;

    pub(super) fn install() {
        INSTALL.call_once(|| {
            // Save the cooked termios once, before raw mode first engages.
            let mut term = unsafe { std::mem::zeroed::<libc::termios>() };
            if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut term) } == 0 {
                let _ = SAVED_TERMIOS.set(term);
            }
            for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                // Safety: the handler body calls only async-signal-safe
                // functions (tcsetattr, write) and signal_hook's re-raise.
                unsafe {
                    let _ = signal_hook::low_level::register(sig, move || {
                        if let Some(term) = SAVED_TERMIOS.get() {
                            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, term);
                        }
                        libc::write(
                            libc::STDOUT_FILENO,
                            RESTORE_BYTES.as_ptr().cast(),
                            RESTORE_BYTES.len(),
                        );
                        let _ = signal_hook::low_level::emulate_default_handler(sig);
                    });
                }
            }
        });
    }
}

// --- what the terminal is painting with ---------------------------------

/// The sixteen colours xterm ships with, for a terminal that answers about
/// its ground and text but not about its palette. A slot it did not name is
/// better filled with the conventional colour than left to guesswork about
/// the scheme.
const XTERM_SLOTS: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00),
    (0xcd, 0x00, 0x00),
    (0x00, 0xcd, 0x00),
    (0xcd, 0xcd, 0x00),
    (0x00, 0x00, 0xee),
    (0xcd, 0x00, 0xcd),
    (0x00, 0xcd, 0xcd),
    (0xe5, 0xe5, 0xe5),
    (0x7f, 0x7f, 0x7f),
    (0xff, 0x00, 0x00),
    (0x00, 0xff, 0x00),
    (0xff, 0xff, 0x00),
    (0x5c, 0x5c, 0xff),
    (0xff, 0x00, 0xff),
    (0x00, 0xff, 0xff),
    (0xff, 0xff, 0xff),
];

/// One of the colours a terminal will report when asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorQuery {
    /// OSC 11: the default background.
    Background,
    /// OSC 10: the default foreground.
    Foreground,
    /// OSC 4;n: palette slot `n`, 0-15.
    Slot(usize),
}

/// What has come back so far.
#[derive(Default)]
struct Answers {
    background: Option<(u8, u8, u8)>,
    foreground: Option<(u8, u8, u8)>,
    slots: [Option<(u8, u8, u8)>; 16],
}

impl Answers {
    fn record(&mut self, query: ColorQuery, rgb: (u8, u8, u8)) {
        match query {
            ColorQuery::Background => self.background = Some(rgb),
            ColorQuery::Foreground => self.foreground = Some(rgb),
            ColorQuery::Slot(slot) => self.slots[slot] = Some(rgb),
        }
    }

    /// The ground and the text are the two things nothing can stand in
    /// for; a palette slot the terminal kept quiet about takes xterm's.
    fn into_colors(self) -> Option<TerminalColors> {
        let mut ansi = XTERM_SLOTS;
        for (slot, answer) in self.slots.iter().enumerate() {
            if let Some(rgb) = answer {
                ansi[slot] = *rgb;
            }
        }
        Some(TerminalColors {
            background: self.background?,
            foreground: self.foreground?,
            ansi,
        })
    }
}

/// The bytes that ask a terminal for its ground, its text colour and its
/// sixteen palette slots, in the order the answers are wanted, followed by
/// a question every terminal answers.
///
/// Terminals answer in the order asked, and a terminal that does not know
/// OSC 4 says nothing rather than saying no. The trailing primary device
/// attributes request (`CSI c`) is the full stop: once its reply arrives,
/// every colour reply that will ever come has come, and nothing is left in
/// the input for the key reader to mistake for typing.
fn color_queries() -> String {
    let mut request = String::from("\x1b]11;?\x1b\\\x1b]10;?\x1b\\");
    for slot in 0..16 {
        request.push_str(&format!("\x1b]4;{slot};?\x1b\\"));
    }
    request.push_str(DEVICE_ATTRIBUTES);
    request
}

/// Primary device attributes: the request, and how its reply starts and
/// ends (`CSI ? ... c`).
const DEVICE_ATTRIBUTES: &str = "\x1b[c";

/// Whether the device-attributes reply is somewhere in `bytes`.
fn device_attributes_answered(bytes: &[u8]) -> bool {
    bytes
        .windows(3)
        .enumerate()
        .filter(|(_, window)| *window == b"\x1b[?")
        .any(|(start, _)| {
            bytes[start + 3..]
                .iter()
                .take_while(|byte| byte.is_ascii_digit() || **byte == b';')
                .count()
                .checked_add(start + 3)
                .and_then(|end| bytes.get(end))
                == Some(&b'c')
        })
}

/// Once a terminal has started answering, how long a silence means it has
/// finished. A round trip over a slow link can take longer than the whole
/// initial wait; this is the gap between two bytes of one reply.
const REPLY_QUIET: Duration = Duration::from_millis(100);

/// The most a talkative-but-slow terminal is waited for after it has
/// started answering.
const REPLY_CAP: Duration = Duration::from_secs(2);

/// Ask the terminal what it is painting with.
///
/// The terminal answers OSC 10, 11 and 4 with the colours it is actually
/// using, which is the one way a program can learn the scheme it was
/// started inside. The answers arrive on stdin, so this runs in raw mode
/// before the chrome takes the terminal, and it waits at most `timeout`:
/// a terminal that does not answer gets no palette derived from it, and
/// the caller falls back to a shipped one. Keys typed while the question
/// is out are consumed with the answers — it is a fraction of a second at
/// startup, before anything is on screen to type at.
///
/// `None` when stdin or stdout is not a terminal, when raw mode cannot be
/// entered, or when the ground or the text colour went unanswered.
pub fn query_terminal_colors(timeout: Duration) -> Option<TerminalColors> {
    use std::io::IsTerminal;
    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        return None;
    }
    // The signal handlers that put the terminal back on SIGINT, SIGTERM and
    // SIGHUP save the cooked termios when they are installed, so they have
    // to be in place before raw mode first engages here — a quarter of a
    // second is long enough for someone to press Ctrl+C, and a default
    // handler exiting then would leave the shell in raw mode.
    install_panic_hook();
    enable_raw_mode().ok()?;
    // Restored on every way out, a panic included: a shell left in raw mode
    // is a worse outcome than any answer this could have produced.
    struct RawMode;
    impl Drop for RawMode {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
        }
    }
    let _raw = RawMode;
    ask_terminal(timeout).and_then(Answers::into_colors)
}

/// `timeout` is how long a silent terminal is given to say anything at all.
/// A terminal that has begun answering is read until its device-attributes
/// reply closes the exchange, or until it falls quiet, or until a hard cap:
/// replies left unread would reach the key reader as typing.
fn ask_terminal(timeout: Duration) -> Option<Answers> {
    let mut out = io::stdout();
    out.write_all(color_queries().as_bytes()).ok()?;
    out.flush().ok()?;

    let started = Instant::now();
    let mut pending = Vec::new();
    let mut seen = Vec::new();
    let mut answers = Answers::default();
    loop {
        let wait = if seen.is_empty() {
            (started + timeout).saturating_duration_since(Instant::now())
        } else {
            REPLY_QUIET.min((started + REPLY_CAP).saturating_duration_since(Instant::now()))
        };
        if wait.is_zero() || !stdin_readable(wait) {
            break;
        }
        let mut buffer = [0u8; 1024];
        let read = read_stdin(&mut buffer);
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&buffer[..read]);
        seen.extend_from_slice(&buffer[..read]);
        let consumed = parse_color_replies(&pending, &mut answers);
        pending.drain(..consumed);
        if device_attributes_answered(&seen) {
            break;
        }
    }
    Some(answers)
}

#[cfg(unix)]
fn stdin_readable(timeout: Duration) -> bool {
    let mut poll = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: `poll` reads and writes one valid pollfd for the duration of
    // the call.
    let ready = unsafe { libc::poll(&mut poll, 1, millis) };
    ready > 0 && poll.revents & libc::POLLIN != 0
}

#[cfg(unix)]
fn read_stdin(buffer: &mut [u8]) -> usize {
    // SAFETY: the buffer is valid and writable for its whole length, and
    // `read` writes at most that many bytes.
    let read = unsafe {
        libc::read(
            libc::STDIN_FILENO,
            buffer.as_mut_ptr().cast::<libc::c_void>(),
            buffer.len(),
        )
    };
    usize::try_from(read).unwrap_or(0)
}

#[cfg(not(unix))]
fn stdin_readable(_timeout: Duration) -> bool {
    false
}

#[cfg(not(unix))]
fn read_stdin(_buffer: &mut [u8]) -> usize {
    0
}

/// Read every complete OSC colour reply in `bytes` into `answers`, and say
/// how many leading bytes are spoken for: whatever precedes a reply, plus
/// every complete reply. An incomplete reply at the tail is left for the
/// next read to finish, as is a lone trailing escape that might begin one.
fn parse_color_replies(bytes: &[u8], answers: &mut Answers) -> usize {
    let mut consumed = 0;
    while let Some(start) = bytes[consumed..]
        .windows(2)
        .position(|pair| pair == b"\x1b]")
    {
        let body_start = consumed + start + 2;
        let Some((body_end, terminator_len)) = osc_terminator(&bytes[body_start..]) else {
            return consumed + start;
        };
        if let Some((query, rgb)) = parse_color_reply(&bytes[body_start..body_start + body_end]) {
            answers.record(query, rgb);
        }
        consumed = body_start + body_end + terminator_len;
    }
    match bytes.last() {
        Some(0x1b) if bytes.len() > consumed => bytes.len() - 1,
        _ => bytes.len(),
    }
}

/// Where an OSC body ends: BEL, or ESC `\`. The offset of the terminator
/// and its length.
fn osc_terminator(body: &[u8]) -> Option<(usize, usize)> {
    for (index, byte) in body.iter().enumerate() {
        match byte {
            0x07 => return Some((index, 1)),
            0x1b if body.get(index + 1) == Some(&b'\\') => return Some((index, 2)),
            _ => {}
        }
    }
    None
}

/// `11;rgb:1c1c/1d1d/1e1e`, `10;rgb:...`, or `4;3;rgb:...`, as a query and
/// an 8-bit colour.
fn parse_color_reply(body: &[u8]) -> Option<(ColorQuery, (u8, u8, u8))> {
    let body = std::str::from_utf8(body).ok()?;
    let mut parts = body.split(';');
    let query = match parts.next()? {
        "11" => ColorQuery::Background,
        "10" => ColorQuery::Foreground,
        "4" => {
            let slot: usize = parts.next()?.parse().ok()?;
            if slot >= 16 {
                return None;
            }
            ColorQuery::Slot(slot)
        }
        _ => return None,
    };
    Some((query, parse_x_color(parts.next()?)?))
}

/// An X11 colour spec as terminals report them: `rgb:RRRR/GGGG/BBBB` with
/// one to four hex digits per channel, or `#RRGGBB`.
fn parse_x_color(spec: &str) -> Option<(u8, u8, u8)> {
    // Only ASCII hex digits are sliced or counted below; a byte that is not
    // one is not a colour, whatever else it might be.
    let hex_digits =
        |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_hexdigit());
    if let Some(hex) = spec.strip_prefix('#') {
        if hex.len() != 6 || !hex_digits(hex) {
            return None;
        }
        let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
        return Some((channel(0)?, channel(2)?, channel(4)?));
    }
    let channels = spec.strip_prefix("rgb:")?;
    let mut channels = channels.split('/').map(|channel| {
        let digits = channel.len();
        if !(1..=4).contains(&digits) || !hex_digits(channel) {
            return None;
        }
        let value = u32::from_str_radix(channel, 16).ok()?;
        // Scale whatever precision the terminal used to eight bits.
        let max = (1u32 << (4 * digits as u32)) - 1;
        u8::try_from((value * 255 + max / 2) / max).ok()
    });
    let rgb = (channels.next()??, channels.next()??, channels.next()??);
    if channels.next().is_some() {
        return None;
    }
    Some(rgb)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The signal handler's hardcoded bytes must stay in lockstep with
    /// what the orderly restore path (crossterm) actually emits: the
    /// keyboard-enhancement pop, then `write_restore`.
    #[test]
    fn signal_restore_bytes_match_mouse_aware_write_restore() {
        let mut out: Vec<u8> = Vec::new();
        crossterm::execute!(out, PopKeyboardEnhancementFlags).expect("write to vec");
        write_restore(&mut out).expect("write to vec");
        assert_eq!(out, RESTORE_BYTES);
    }

    #[test]
    fn enter_chrome_enables_mouse_capture() {
        let mut out = Vec::new();
        write_enter_chrome(&mut out).expect("write to vec");
        assert_eq!(
            out,
            b"\x1b[?1049h\x1b[?25l\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1015h\x1b[?1006h"
        );
    }

    #[test]
    fn osc52_writes_the_exact_clipboard_sequence() {
        let mut out = Vec::new();
        assert_eq!(write_osc52(&mut out, "hello").expect("write to vec"), None);
        eprintln!("OSC 52 bytes: {out:?}");
        assert_eq!(out, b"\x1b]52;c;aGVsbG8=\x07");
    }

    #[test]
    fn osc52_caps_source_text_at_100_kib_without_splitting_utf8() {
        let text = format!("{}€", "a".repeat(OSC52_MAX_BYTES - 1));
        let mut out = Vec::new();
        let notice = write_osc52(&mut out, &text)
            .expect("write to vec")
            .expect("oversized text reports truncation");

        assert!(notice.contains("100 KiB"));
        assert!(notice.contains("truncated"));
        assert!(out.starts_with(b"\x1b]52;c;"));
        assert_eq!(out.last(), Some(&b'\x07'));
        let encoded = &out[b"\x1b]52;c;".len()..out.len() - 1];
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("valid base64");
        assert_eq!(decoded.len(), OSC52_MAX_BYTES - 1);
        assert!(decoded.iter().all(|byte| *byte == b'a'));
    }

    #[test]
    fn color_replies_parse_every_form_a_terminal_uses() {
        let mut answers = Answers::default();
        let bytes = b"\x1b]11;rgb:1c1c/1d1d/1e1e\x1b\\\x1b]10;rgb:ab/cd/ef\x07\x1b]4;3;rgb:f/0/8\x07\x1b]4;12;#5c5cff\x1b\\";
        let consumed = parse_color_replies(bytes, &mut answers);
        assert_eq!(consumed, bytes.len());
        assert_eq!(answers.background, Some((0x1c, 0x1d, 0x1e)));
        assert_eq!(answers.foreground, Some((0xab, 0xcd, 0xef)));
        assert_eq!(answers.slots[3], Some((0xff, 0x00, 0x88)));
        assert_eq!(answers.slots[12], Some((0x5c, 0x5c, 0xff)));
    }

    #[test]
    fn a_reply_split_across_reads_waits_for_its_end() {
        let mut answers = Answers::default();
        let first = b"junk\x1b]11;rgb:10";
        let consumed = parse_color_replies(first, &mut answers);
        assert_eq!(&first[consumed..], b"\x1b]11;rgb:10");
        assert_eq!(answers.background, None);

        let mut pending = first[consumed..].to_vec();
        pending.extend_from_slice(b"00/2000/3000\x07");
        let consumed = parse_color_replies(&pending, &mut answers);
        assert_eq!(consumed, pending.len());
        assert_eq!(answers.background, Some((0x10, 0x20, 0x30)));
    }

    #[test]
    fn a_colour_that_is_not_hex_is_not_a_colour() {
        let mut answers = Answers::default();
        for reply in [
            "\x1b]11;#aé123\x07",
            "\x1b]11;#12345\x07",
            "\x1b]11;rgb:1é/00/00\x07",
            "\x1b]11;rgb:12345/0/0\x07",
            "\x1b]11;rgb:0/0\x07",
            "\x1b]11;rgb:0/0/0/0\x07",
            "\x1b]11;rgba:0/0/0/0\x07",
        ] {
            let bytes = reply.as_bytes();
            assert_eq!(
                parse_color_replies(bytes, &mut answers),
                bytes.len(),
                "{reply:?}"
            );
            assert_eq!(answers.background, None, "{reply:?}");
        }
    }

    #[test]
    fn a_trailing_escape_is_kept_and_other_noise_dropped() {
        let mut answers = Answers::default();
        assert_eq!(parse_color_replies(b"abc\x1b", &mut answers), 3);
        assert_eq!(parse_color_replies(b"abc", &mut answers), 3);
        let out_of_range = b"\x1b]4;99;rgb:0/0/0\x07";
        assert_eq!(
            parse_color_replies(out_of_range, &mut answers),
            out_of_range.len()
        );
        assert!(answers.slots.iter().all(Option::is_none));
    }

    #[test]
    fn unanswered_slots_take_xterms_and_a_missing_ground_answers_nothing() {
        let mut answers = Answers::default();
        answers.record(ColorQuery::Background, (1, 2, 3));
        answers.record(ColorQuery::Foreground, (4, 5, 6));
        answers.record(ColorQuery::Slot(2), (7, 8, 9));
        let colors = answers.into_colors().expect("ground and text suffice");
        assert_eq!(colors.background, (1, 2, 3));
        assert_eq!(colors.ansi[2], (7, 8, 9));
        assert_eq!(colors.ansi[1], XTERM_SLOTS[1]);

        let mut answers = Answers::default();
        answers.record(ColorQuery::Foreground, (4, 5, 6));
        assert!(answers.into_colors().is_none());
    }

    #[test]
    fn the_queries_ask_for_the_ground_the_text_and_every_slot_then_a_full_stop() {
        let queries = color_queries();
        assert!(queries.starts_with("\x1b]11;?\x1b\\\x1b]10;?\x1b\\"));
        assert_eq!(queries.matches("\x1b]4;").count(), 16);
        assert!(queries.ends_with("\x1b]4;15;?\x1b\\\x1b[c"));
    }

    #[test]
    fn the_device_attributes_reply_closes_the_exchange() {
        assert!(device_attributes_answered(
            b"\x1b]11;rgb:0/0/0\x07\x1b[?62;22c"
        ));
        assert!(device_attributes_answered(b"\x1b[?1;2c"));
        assert!(!device_attributes_answered(b"\x1b[?62;2"));
        assert!(!device_attributes_answered(b"\x1b]11;rgb:0/0/0\x07"));
        // A colour reply is not a full stop, whatever letters it holds.
        assert!(!device_attributes_answered(b"\x1b]4;12;rgb:cc/cc/cc\x07"));
    }
}
