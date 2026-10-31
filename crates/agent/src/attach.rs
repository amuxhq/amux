//! Raw attach on pty.sock: a terminal client on the agent's own machine,
//! attached for as long as it holds its connection.
//!
//! Terminal Claude has one terminal, and its bytes are already on disk in
//! pty/. A client is served in files mode: the hello says where the files
//! start and how far they go, the client reads them itself by position,
//! and the agent sends each new end position as the terminal writes. What
//! the client types and its size go to the one terminal, typed in order
//! with whatever the interpreter types; with two clients the last resize
//! wins.
//!
//! Codex's terminal is a view, not the agent: Codex's own app joins the
//! agent's app server as one more client (`codex resume <thread> --remote
//! unix://<socket>`) and repaints the thread from it each time it starts,
//! so nothing is retained. A client is served in stream mode: its
//! connection gets its own view on the agent's live thread, the view's
//! bytes stream over the socket, and the view ends with the connection.
//! Two clients are two views on one thread. Windows runs the server on
//! stdio, which has room for the agent alone, so a view there is refused.

use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use wire::{AgentSpec, PtyFrame, PtyHello, PtyMode, PtyResize, pty_frame};

use crate::ctl::{read_message, write_message};
use crate::local_socket::{LocalListener, LocalStream};

/// How long a view asked to end gets before its process group is killed.
const VIEW_STOP: Duration = Duration::from_secs(2);
/// How long output still in flight is collected after a view exits.
const TRAILING_OUTPUT: Duration = Duration::from_millis(300);

/// What an attached client does to the one terminal in files mode.
#[derive(Debug)]
pub enum Control {
    /// A client shows the terminal: once served, and again when it comes
    /// back to it. Each is followed by [`Control::Left`].
    Joined,
    Keys(Vec<u8>),
    Resize {
        rows: u16,
        cols: u16,
    },
    /// The client no longer shows the terminal, or its connection ended.
    Left,
}

/// Whether a client shows the terminal, saying so as it changes and
/// [`Control::Left`] however its connection ends while it does.
struct Shown {
    control: mpsc::UnboundedSender<Control>,
    shown: bool,
}

impl Shown {
    fn set(&mut self, shown: bool) {
        if shown != self.shown {
            self.shown = shown;
            let _ = self.control.send(if shown {
                Control::Joined
            } else {
                Control::Left
            });
        }
    }
}

impl Drop for Shown {
    fn drop(&mut self) {
        self.set(false);
    }
}

/// How pty.sock serves its clients.
pub enum Serve {
    /// The terminal log in `pty`, its end position, and the terminal.
    Files {
        pty: PathBuf,
        written: watch::Receiver<u64>,
        control: mpsc::UnboundedSender<Control>,
    },
    /// A Codex view per connection, launched from the spec.
    Stream { spec: Arc<AgentSpec>, dir: PathBuf },
}

/// Serves every client until the task is dropped, which ends them all.
pub async fn serve(mut listener: LocalListener, how: Serve) {
    let how = Arc::new(how);
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(stream) => {
                    let how = how.clone();
                    clients.spawn(async move {
                        let _ = match &*how {
                            Serve::Files { pty, written, control } => {
                                files(stream, pty, written.clone(), control.clone()).await
                            }
                            Serve::Stream { spec, dir } => stream_view(stream, spec, dir).await,
                        };
                    });
                }
                Err(_) => return,
            },
            Some(_) = clients.join_next() => {}
        }
    }
}

async fn send<W: AsyncWrite + Unpin>(out: &mut W, of: pty_frame::Of) -> io::Result<()> {
    write_message(out, &PtyFrame { of: Some(of) }).await
}

async fn next<R: AsyncRead + Unpin>(input: &mut R) -> Option<pty_frame::Of> {
    loop {
        match read_message::<_, PtyFrame>(input).await {
            Ok(Some(PtyFrame { of: Some(of) })) => return Some(of),
            // A frame this build does not know is skipped.
            Ok(Some(PtyFrame { of: None })) => {}
            Ok(None) | Err(_) => return None,
        }
    }
}

fn size(resize: &PtyResize) -> (u16, u16) {
    let clamp = |n: u32| u16::try_from(n).unwrap_or(u16::MAX).max(1);
    (clamp(resize.rows), clamp(resize.cols))
}

async fn files(
    stream: LocalStream,
    pty: &Path,
    mut written: watch::Receiver<u64>,
    control: mpsc::UnboundedSender<Control>,
) -> io::Result<()> {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let end = *written.borrow_and_update();
    let start = journal::segments(pty)?.first().copied().unwrap_or(end);
    send(
        &mut writer,
        pty_frame::Of::Hello(PtyHello {
            mode: PtyMode::Files as i32,
            start,
            written: end,
        }),
    )
    .await?;
    let mut shown = Shown {
        control: control.clone(),
        shown: false,
    };
    shown.set(true);
    let typing = async {
        while let Some(of) = next(&mut reader).await {
            let typed = match of {
                pty_frame::Of::Shown(now) => {
                    shown.set(now);
                    continue;
                }
                pty_frame::Of::Keys(keys) => Control::Keys(keys),
                pty_frame::Of::Resize(resize) => {
                    let (rows, cols) = size(&resize);
                    Control::Resize { rows, cols }
                }
                _ => continue,
            };
            if control.send(typed).is_err() {
                return;
            }
        }
    };
    let telling = async {
        // The sender goes with the agent, which ends every connection.
        while written.changed().await.is_ok() {
            let end = *written.borrow_and_update();
            if send(&mut writer, pty_frame::Of::Written(end))
                .await
                .is_err()
            {
                return;
            }
        }
    };
    tokio::select! {
        () = typing => {}
        () = telling => {}
    }
    Ok(())
}

/// Ends a view's process group if the connection's task is dropped with the
/// view still running, as it is when the agent exits.
struct View(pty_host::PtyHandle);

impl Drop for View {
    fn drop(&mut self) {
        let _ = self
            .0
            .signal_process_group(pty_host::ProcessGroupSignal::Kill);
    }
}

async fn stream_view(stream: LocalStream, spec: &AgentSpec, dir: &Path) -> io::Result<()> {
    let (mut reader, mut writer) = tokio::io::split(stream);
    send(
        &mut writer,
        pty_frame::Of::Hello(PtyHello {
            mode: PtyMode::Stream as i32,
            ..Default::default()
        }),
    )
    .await?;
    let Some(thread) = crate::provider::codex_thread(dir) else {
        let why = "the agent's Codex thread has not started yet";
        return send(&mut writer, pty_frame::Of::Closed(why.to_owned())).await;
    };
    let Some(launch) =
        crate::provider::codex_view(spec, dir, &thread, pty_host::PtySize::default())
    else {
        let why = "Codex's own app attaches only on macOS and Linux";
        return send(&mut writer, pty_frame::Of::Closed(why.to_owned())).await;
    };
    let process = match pty_host::spawn(launch) {
        Ok(process) => process,
        Err(error) => {
            let why = format!("could not start codex resume: {error}");
            return send(&mut writer, pty_frame::Of::Closed(why)).await;
        }
    };
    let view = View(process.handle.clone());
    let mut output = view.0.output();
    let mut exit = process.exit.clone();
    let typing = async {
        while let Some(of) = next(&mut reader).await {
            match of {
                pty_frame::Of::Keys(keys) => {
                    if view.0.write(&keys).await.is_err() {
                        return;
                    }
                }
                pty_frame::Of::Resize(resize) => {
                    let (rows, cols) = size(&resize);
                    let _ = view.0.resize(pty_host::PtySize { rows, cols });
                }
                _ => {}
            }
        }
    };
    let drawing = async {
        let status = loop {
            tokio::select! {
                bytes = output.recv() => match bytes {
                    Some(bytes) => {
                        if send(&mut writer, pty_frame::Of::Output(bytes.to_vec())).await.is_err() {
                            return;
                        }
                    }
                    None => break exit.wait().await,
                },
                status = exit.wait() => break status,
            }
        };
        while let Ok(Some(bytes)) = tokio::time::timeout(TRAILING_OUTPUT, output.recv()).await {
            let _ = send(&mut writer, pty_frame::Of::Output(bytes.to_vec())).await;
        }
        let why = match status.signal() {
            Some(signal) => format!("codex resume ended by {signal}"),
            None => format!("codex resume exited with code {}", status.exit_code()),
        };
        let _ = send(&mut writer, pty_frame::Of::Closed(why)).await;
    };
    tokio::select! {
        // The client left: the view ends with its connection.
        () = typing => {
            let _ = pty_host::terminate(
                &process,
                pty_host::Terminate::Graceful { grace: VIEW_STOP },
            )
            .await;
        }
        () = drawing => {}
    }
    Ok(())
}

/// A terminal client's connection to an agent's pty.sock.
pub struct Attached {
    output: AttachedOutput,
    input: AttachedInput,
}

/// What the terminal draws, read from one connection.
pub struct AttachedOutput {
    reader: tokio::io::ReadHalf<LocalStream>,
    mode: PtyMode,
    pty: PathBuf,
    /// Files mode: how far this client has read, and how far the log goes.
    read: u64,
    written: u64,
    /// Where the log ended when this client attached: what comes before is
    /// history, replayed without its terminal queries.
    history: u64,
    closed: Option<String>,
}

/// What the person types and their terminal's size, sent on one connection.
pub struct AttachedInput {
    writer: tokio::io::WriteHalf<LocalStream>,
}

impl Attached {
    /// Attaches to the agent in `dir` and reads its hello.
    pub async fn connect(dir: &Path) -> io::Result<Self> {
        let stream = crate::local_socket::connect(&dir.join(crate::dir::PTY_SOCK)).await?;
        let (mut reader, writer) = tokio::io::split(stream);
        let Some(pty_frame::Of::Hello(hello)) = next(&mut reader).await else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pty.sock did not start with a hello",
            ));
        };
        let mode = hello.mode();
        Ok(Self {
            output: AttachedOutput {
                reader,
                mode,
                pty: dir.join(crate::dir::PTY),
                read: hello.start,
                written: hello.written,
                history: hello.written,
                closed: None,
            },
            input: AttachedInput { writer },
        })
    }

    /// The two directions apart, so one task can read while another types.
    pub fn split(self) -> (AttachedOutput, AttachedInput) {
        (self.output, self.input)
    }

    pub fn mode(&self) -> PtyMode {
        self.output.mode
    }

    /// Why the agent ended the connection, once it has.
    pub fn closed(&self) -> Option<&str> {
        self.output.closed()
    }

    /// The next bytes the terminal drew, or None once the connection ended.
    /// In files mode the first call returns everything the log still holds.
    pub async fn next(&mut self) -> io::Result<Option<Vec<u8>>> {
        self.output.next().await
    }

    pub async fn keys(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.input.keys(bytes).await
    }

    pub async fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        self.input.resize(rows, cols).await
    }

    pub async fn shown(&mut self, shown: bool) -> io::Result<()> {
        self.input.shown(shown).await
    }
}

impl AttachedOutput {
    pub fn mode(&self) -> PtyMode {
        self.mode
    }

    /// Why the agent ended the connection, once it has.
    pub fn closed(&self) -> Option<&str> {
        self.closed.as_deref()
    }

    /// The next bytes the terminal drew, or None once the connection ended.
    /// In files mode the first call returns everything the log still holds.
    /// Not cancel safe: a frame half read when the future is dropped is lost.
    pub async fn next(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            if self.mode == PtyMode::Files && self.read < self.written {
                let (from, mut bytes) = read_terminal_log(&self.pty, self.read, self.written)?;
                self.read = from + bytes.len() as u64;
                if !bytes.is_empty() {
                    if from < self.history {
                        // The terminal the history replays on would answer
                        // the queries in it, and the answers would reach the
                        // agent as typed keys.
                        let past = ((self.history - from) as usize).min(bytes.len());
                        let mut replay = without_queries(&bytes[..past]);
                        replay.extend_from_slice(&bytes[past..]);
                        bytes = replay;
                    }
                    return Ok(Some(bytes));
                }
                self.read = self.written;
            }
            match next(&mut self.reader).await {
                Some(pty_frame::Of::Written(written)) => self.written = written,
                Some(pty_frame::Of::Output(bytes)) => return Ok(Some(bytes)),
                Some(pty_frame::Of::Closed(why)) => self.closed = Some(why),
                Some(_) => {}
                None => return Ok(None),
            }
        }
    }
}

impl AttachedInput {
    pub async fn keys(&mut self, bytes: &[u8]) -> io::Result<()> {
        send(&mut self.writer, pty_frame::Of::Keys(bytes.to_vec())).await
    }

    pub async fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        send(
            &mut self.writer,
            pty_frame::Of::Resize(PtyResize {
                rows: rows.into(),
                cols: cols.into(),
            }),
        )
        .await
    }

    /// Says whether this client's terminal shows the agent's screen now.
    pub async fn shown(&mut self, shown: bool) -> io::Result<()> {
        send(&mut self.writer, pty_frame::Of::Shown(shown)).await
    }
}

/// `bytes` with every terminal query taken out: device attributes, status
/// and cursor reports, version, keyboard-protocol and mode queries, window
/// size reports, colour queries and setting requests. Everything else, the
/// drawing and the modes it sets, is kept byte for byte.
pub fn without_queries(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let rest = &bytes[at..];
        let sequence = match rest {
            [0x1b, b'[', ..] => csi(rest),
            [0x1b, b']', ..] => string(rest).map(|(len, body)| (len, body.ends_with(b"?"))),
            [0x1b, b'P', ..] => string(rest)
                .map(|(len, body)| (len, body.starts_with(b"$q") || body.starts_with(b"+q"))),
            _ => None,
        };
        match sequence {
            Some((len, query)) => {
                if !query {
                    out.extend_from_slice(&rest[..len]);
                }
                at += len;
            }
            None => {
                out.push(bytes[at]);
                at += 1;
            }
        }
    }
    out
}

/// A control sequence at the front of `bytes`: its length, and whether it
/// asks the terminal for an answer.
fn csi(bytes: &[u8]) -> Option<(usize, bool)> {
    let body = &bytes[2..];
    let params = body
        .iter()
        .take_while(|b| (0x30..=0x3f).contains(*b))
        .count();
    let middle = body[params..]
        .iter()
        .take_while(|b| (0x20..=0x2f).contains(*b))
        .count();
    let last = *body.get(params + middle)?;
    if !(0x40..=0x7e).contains(&last) {
        return None;
    }
    let (params, middle) = (&body[..params], &body[params..params + middle]);
    let query = match (last, middle) {
        // Primary, secondary and tertiary device attributes; `CSI ? … c`
        // is an answer, not a question.
        (b'c', b"") => !params.starts_with(b"?"),
        // Status and cursor position reports.
        (b'n', b"") => matches!(params, b"5" | b"6" | b"?6" | b"?15" | b"?26"),
        // The terminal's name and version.
        (b'q', b"") => params.starts_with(b">"),
        // The keyboard protocol's flags.
        (b'u', b"") => params == b"?",
        // A mode's state.
        (b'p', b"$") => true,
        // Window and cell sizes.
        (b't', b"") => matches!(
            params,
            b"11" | b"13" | b"14" | b"15" | b"16" | b"18" | b"19"
        ),
        _ => false,
    };
    Some((2 + params.len() + middle.len() + 1, query))
}

/// An OSC or DCS string at the front of `bytes`, ended by BEL or ST: its
/// length and its body.
fn string(bytes: &[u8]) -> Option<(usize, &[u8])> {
    let body = &bytes[2..];
    let end = body.iter().enumerate().find_map(|(at, byte)| match byte {
        0x07 => Some((at, 1)),
        0x1b if body.get(at + 1) == Some(&b'\\') => Some((at, 2)),
        _ => None,
    })?;
    Some((2 + end.0 + end.1, &body[..end.0]))
}

/// Reads the terminal log in `pty` from `from` up to `to`, as a files-mode
/// client does after a hello or a written frame. Bytes the agent has
/// already rotated away are skipped: the result starts at the returned
/// position, which is `from` unless the oldest file begins after it.
pub fn read_terminal_log(pty: &Path, from: u64, to: u64) -> io::Result<(u64, Vec<u8>)> {
    let starts = journal::segments(pty)?;
    let mut at = from;
    let mut bytes = Vec::new();
    for (index, start) in starts.iter().copied().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(u64::MAX);
        if end <= at || start >= to {
            continue;
        }
        if bytes.is_empty() && start > at {
            at = start;
        }
        let mut file = match File::open(journal::segment_path(pty, start)) {
            Ok(file) => file,
            // Rotated away between listing and opening.
            Err(error) if error.kind() == io::ErrorKind::NotFound && bytes.is_empty() => continue,
            Err(error) => return Err(error),
        };
        let offset = at + bytes.len() as u64;
        file.seek(SeekFrom::Start(offset - start))?;
        let want = (to.min(end) - offset) as usize;
        let mut chunk = Vec::with_capacity(want);
        file.take(want as u64).read_to_end(&mut chunk)?;
        let short = chunk.len() < want;
        bytes.extend_from_slice(&chunk);
        if short {
            break;
        }
    }
    Ok((at, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dir::PtyLog;

    #[test]
    fn replayed_history_keeps_its_drawing_and_loses_its_questions() {
        // Terminal Claude's startup, as the log keeps it.
        let startup = b"\x1b[?2004h\x1b[>0q\x1b[?u\x1b[c\x1b[?2004l\x1b[?2004hClaude Code\r\n";
        assert_eq!(
            without_queries(startup),
            b"\x1b[?2004h\x1b[?2004l\x1b[?2004hClaude Code\r\n"
        );
        let asks =
            b"a\x1b[6nb\x1b]11;?\x07c\x1b]4;1;?\x1b\\d\x1bP$qm\x1b\\e\x1b[?2026$pf\x1b[18tg\x1b[>c";
        assert_eq!(without_queries(asks), b"abcdefg");
        // Drawing, colours, titles, cursor styles and keyboard modes stay.
        let drawing =
            b"\x1b[2J\x1b[1;31mred\x1b[0m\x1b]0;title\x07\x1b[2 q\x1b[>1u\x1b[?25l\x1b[3;4H";
        assert_eq!(without_queries(drawing), drawing);
        // A sequence cut off at the end is kept as it is.
        assert_eq!(without_queries(b"x\x1b[6"), b"x\x1b[6");
        assert_eq!(without_queries(b"x\x1b]11;?"), b"x\x1b]11;?");
    }

    #[test]
    fn the_terminal_log_reads_by_position_across_segments_and_past_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let pty = dir.path().join("pty");
        let mut log = PtyLog::open(pty.clone(), 4, 2).unwrap();
        log.append(b"0123456789").unwrap();
        // Segments 4 and 8 are kept: bytes 0-3 are gone.
        assert_eq!(
            read_terminal_log(&pty, 5, 10).unwrap(),
            (5, b"56789".to_vec())
        );
        assert_eq!(
            read_terminal_log(&pty, 0, 10).unwrap(),
            (4, b"456789".to_vec())
        );
        assert_eq!(read_terminal_log(&pty, 6, 7).unwrap(), (6, b"6".to_vec()));
        assert_eq!(read_terminal_log(&pty, 10, 10).unwrap(), (10, Vec::new()));
    }
}
