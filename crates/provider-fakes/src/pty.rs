//! `fake-claude-pty`: Claude Code in a terminal, without the TUI.
//!
//! What the host reads from a terminal Claude is not the screen but the
//! session transcript Claude appends to, the hook payloads it hands the
//! commands its `--settings` name, and its messaging socket. This fake keeps
//! all three and draws only plain text on the terminal.

use std::io::{Read, Write};
use std::path::Path;

pub use engine::{NO_MESSAGING_ENV, RAISES};
use serde_json::Value;

use crate::claude::Args;
use crate::playback::{self, Channel, Process};
use crate::{DRIFT_EXIT, Mode};

pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if crate::claude::answered_version(&args) {
        return 0;
    }
    if let Some(refusal) = Args::parse(&args).managed_refusal() {
        eprintln!("Error: {refusal}");
        return 1;
    }
    let mode = match crate::mode_from_env() {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("fake-claude-pty: {error}");
            return DRIFT_EXIT;
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    match mode {
        Mode::Playback(process) => match play(&process, &Args::parse(&args)) {
            Ok(code) => code.unwrap_or(0),
            Err(error) => {
                eprintln!("fake-claude-pty: {error}");
                DRIFT_EXIT
            }
        },
        Mode::Script(script) => runtime.block_on(engine::run(script, Args::parse(&args))),
    }
}

/// Run each hook command for the payload's event with the payload on
/// stdin, one after another, waiting for each so the host sees payloads in
/// the order they were raised.
pub fn run_hooks(args: &Args, payload: &[u8], env: &[(String, String)]) -> std::io::Result<()> {
    let event = serde_json::from_slice::<Value>(payload)
        .ok()
        .and_then(|payload| payload.get("hook_event_name")?.as_str().map(str::to_owned))
        .unwrap_or_default();
    for command in args.hook_commands(&event) {
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .envs(env.iter().map(|(key, value)| (key, value)))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(payload)?;
        child.wait()?;
    }
    Ok(())
}

/// Append one row to a session transcript, creating it and its directory.
pub fn append_row(path: &Path, row: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut line = row.to_vec();
    line.push(b'\n');
    file.write_all(&line)
}

/// Put the terminal into raw mode, so keys arrive byte for byte and output
/// leaves unchanged. Not a terminal (a test driving stdio) is left alone.
pub fn raw_mode() {
    #[cfg(unix)]
    // SAFETY: tcgetattr/tcsetattr on stdin with a zeroed, then filled,
    // termios owned by this frame.
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut termios) == 0 {
            libc::cfmakeraw(&mut termios);
            libc::tcsetattr(0, libc::TCSANOW, &termios);
        }
    }
    // A console reads whole echoed lines and turns Ctrl-C into a signal
    // until those modes are off; then keys arrive as the bytes typed.
    #[cfg(windows)]
    // SAFETY: console mode calls on this process's standard input handle,
    // with a mode owned by this frame.
    unsafe {
        use windows_sys::Win32::System::Console::{
            ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
            ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE,
            SetConsoleMode,
        };
        let input = GetStdHandle(STD_INPUT_HANDLE);
        let mut mode = 0;
        if GetConsoleMode(input, &mut mode) != 0 {
            mode &= !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT);
            SetConsoleMode(input, mode | ENABLE_VIRTUAL_TERMINAL_INPUT);
        }
    }
}

/// The terminal's size as `size <rows>x<cols>`.
pub fn size_line() -> String {
    #[cfg(unix)]
    {
        // SAFETY: TIOCGWINSZ fills the zeroed winsize owned by this frame.
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut size) } == 0 {
            return format!("size {}x{}", size.ws_row, size.ws_col);
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE,
        };
        // SAFETY: the zeroed buffer info owned by this frame is filled by
        // the call on this process's standard output handle.
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
        if unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) } != 0 {
            let window = info.srWindow;
            return format!(
                "size {}x{}",
                window.Bottom - window.Top + 1,
                window.Right - window.Left + 1
            );
        }
    }
    "size unknown".to_owned()
}

/// Plays a recorded terminal session; a recorded exit ends it there with
/// its code, returned.
fn play(process: &Process, args: &Args) -> Result<Option<i32>, String> {
    raw_mode();
    let config = playback::claude_config_dir();
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut session = String::from("unnamed");
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    for (index, event) in process.events.iter().enumerate() {
        match event.channel {
            Channel::Output => {
                stdout
                    .write_all(&event.bytes)
                    .and_then(|()| stdout.flush())
                    .map_err(|error| format!("event {index}: writing: {error}"))?;
            }
            Channel::Input => {
                let mut typed = vec![0; event.bytes.len()];
                stdin
                    .read_exact(&mut typed)
                    .map_err(|error| format!("event {index}: reading: {error}"))?;
                if typed != event.bytes {
                    return Err(format!(
                        "event {index}: host typed {:?}, recording has {:?}",
                        String::from_utf8_lossy(&typed),
                        String::from_utf8_lossy(&event.bytes)
                    ));
                }
            }
            Channel::Transcript => {
                if let Some(named) = playback::row_session(&event.bytes) {
                    session = named;
                }
                append_row(
                    &playback::transcript_path(&config, &cwd, &session),
                    &event.bytes,
                )
                .map_err(|error| format!("event {index}: transcript: {error}"))?;
            }
            Channel::Exit => return Ok(Some(playback::exit_code(event))),
            Channel::Hook => {
                if let Some(named) = playback::row_session(&event.bytes) {
                    session = named;
                }
                let payload = playback::localize_hook(
                    &event.bytes,
                    &playback::transcript_path(&config, &cwd, &session),
                );
                run_hooks(args, &payload, &[])
                    .map_err(|error| format!("event {index}: hook: {error}"))?;
            }
        }
    }
    Ok(None)
}

mod engine;
