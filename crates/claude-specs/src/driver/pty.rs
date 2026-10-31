//! One Claude PTY event stream paired with its control handle.

use std::collections::HashMap;
use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use claude::version::{VersionError, probe_version};
use claude_protocol::hooks::{self, Payload as HookPayload};
use claude_protocol::stream::ContentBlock;
use claude_protocol::transcript::{self, Row as TranscriptRow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};
#[cfg(test)]
use tokio::sync::oneshot;
use tokio::sync::{mpsc, watch};

use super::hooks::HookReceiver;
use super::transcript::TranscriptTailer;
pub use crate::terminal::*;

const CHANNEL_CAPACITY: usize = 256;
const MAX_DELAY_MS: u32 = 5_000;

pub type EventStream = mpsc::Receiver<PtyEvent>;
type ExitFuture = Pin<Box<dyn Future<Output = pty_host::ExitStatus> + Send>>;
type Relink = Arc<dyn Fn(PathBuf) + Send + Sync>;

pub struct Sources {
    pub pty: PtySource,
    pub hooks: HookSource,
    pub transcript: TranscriptSource,
    pub version: semver::Version,
    pub delays: DelaySource,
}

#[derive(Clone)]
pub struct DelaySource {
    implementation: DelayImplementation,
}

#[derive(Clone)]
enum DelayImplementation {
    Live,
    Replay(replay_support::ReplayClock),
}

impl DelaySource {
    pub fn live() -> Self {
        Self {
            implementation: DelayImplementation::Live,
        }
    }

    #[doc(hidden)]
    pub fn replay(clock: replay_support::ReplayClock) -> Self {
        Self {
            implementation: DelayImplementation::Replay(clock),
        }
    }

    async fn wait(&self, duration: Duration) {
        match &self.implementation {
            DelayImplementation::Live => {
                tokio::time::sleep(duration.min(Duration::from_millis(u64::from(MAX_DELAY_MS))))
                    .await;
            }
            DelayImplementation::Replay(clock) => {
                let _ = clock.advance_for(duration).await;
                tokio::task::yield_now().await;
            }
        }
    }
}

pub struct PtySource {
    pub output: mpsc::Receiver<Bytes>,
    pub writer: Box<dyn AsyncWrite + Unpin + Send>,
    pub handle: Option<pty_host::PtyHandle>,
    pub exit: ExitFuture,
}

pub struct HookSource {
    pub payloads: mpsc::Receiver<HookPayload>,
    receiver: Option<HookReceiver>,
}

impl HookSource {
    pub fn channel(capacity: usize) -> (Self, mpsc::Sender<HookPayload>) {
        let (tx, payloads) = mpsc::channel(capacity);
        (
            Self {
                payloads,
                receiver: None,
            },
            tx,
        )
    }

    fn from_receiver(receiver: HookReceiver) -> Self {
        let payloads = receiver.payloads();
        Self {
            payloads,
            receiver: Some(receiver),
        }
    }
}

pub struct TranscriptSource {
    pub rows: mpsc::Receiver<(PathBuf, TranscriptRow)>,
    pub relink: Relink,
    task: Option<tokio::task::JoinHandle<()>>,
    drain_before_exit: bool,
}

impl TranscriptSource {
    pub fn channel(
        capacity: usize,
    ) -> (
        Self,
        mpsc::Sender<(PathBuf, TranscriptRow)>,
        mpsc::UnboundedReceiver<PathBuf>,
    ) {
        let (row_tx, rows) = mpsc::channel(capacity);
        let (path_tx, path_rx) = mpsc::unbounded_channel();
        let relink = Arc::new(move |path| {
            let _ = path_tx.send(path);
        });
        (
            Self {
                rows,
                relink,
                task: None,
                drain_before_exit: false,
            },
            row_tx,
            path_rx,
        )
    }

    pub fn live() -> Self {
        let (row_tx, rows) = mpsc::channel(CHANNEL_CAPACITY);
        let (path_tx, mut path_rx) = mpsc::unbounded_channel::<PathBuf>();
        let relink = Arc::new(move |path| {
            let _ = path_tx.send(path);
        });
        let task = tokio::spawn(async move {
            let mut current: Option<(PathBuf, TranscriptTailer, mpsc::Receiver<TranscriptRow>)> =
                None;
            loop {
                match current.as_mut() {
                    Some((path, _, source_rows)) => tokio::select! {
                        next = path_rx.recv() => {
                            let Some(next) = next else { break; };
                            if *path != next {
                                let tailer = TranscriptTailer::follow(next.clone());
                                let source_rows = tailer.rows();
                                current = Some((next, tailer, source_rows));
                            }
                        }
                        row = source_rows.recv() => {
                            let Some(row) = row else { current = None; continue; };
                            if row_tx.send((path.clone(), row)).await.is_err() { break; }
                        }
                    },
                    None => {
                        let Some(path) = path_rx.recv().await else {
                            break;
                        };
                        let tailer = TranscriptTailer::follow(path.clone());
                        let source_rows = tailer.rows();
                        current = Some((path, tailer, source_rows));
                    }
                }
            }
        });
        Self {
            rows,
            relink,
            task: Some(task),
            drain_before_exit: false,
        }
    }

    fn recorded(rows: mpsc::Receiver<(PathBuf, TranscriptRow)>) -> Self {
        Self {
            rows,
            relink: Arc::new(|_| {}),
            task: None,
            drain_before_exit: true,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum PtyEvent {
    Ready {
        version: semver::Version,
    },
    Transcript {
        path: PathBuf,
        row: TranscriptRow,
    },
    Hook(HookPayload),
    Ask(AskFacts),
    Relink {
        transcript_path: PathBuf,
        reason: RelinkReason,
    },
    InputResult(InputResult),
    Exited(pty_host::ExitStatus),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskFacts {
    pub id: AskId,
    pub kind: AskKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelinkReason {
    Initial,
    Compact,
    Clear,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputResult {
    pub intent: Intent,
    pub program: crate::terminal::keymap::ProgramName,
    pub bytes_written: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error(transparent)]
    Version(#[from] VersionError),
    #[error("could not bind Claude hook receiver: {0}")]
    Hook(#[source] std::io::Error),
    #[error(transparent)]
    Pty(#[from] pty_host::PtyError),
    #[error("recording omitted the `{0}` transport")]
    MissingTransport(&'static str),
}

pub struct Session {
    pub events: EventStream,
    pub control: Control,
}

#[derive(Clone)]
pub struct Control {
    writer: Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
    terminal_output: Arc<Mutex<Option<mpsc::Receiver<Bytes>>>>,
    terminal: Option<pty_host::PtyHandle>,
    events: mpsc::Sender<PtyEvent>,
    keymap: Arc<crate::terminal::keymap::Keymap>,
    asks: Arc<Mutex<HashMap<AskId, AskKind>>>,
    send_lock: Arc<tokio::sync::Mutex<()>>,
    exit: watch::Receiver<Option<pty_host::ExitStatus>>,
    write_observer: Arc<Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>>,
    delays: DelaySource,
}

impl Control {
    /// Attach the executable-spec recorder to the actual PTY write boundary.
    /// Normal hosts never install an observer.
    #[doc(hidden)]
    pub fn observe_writes(&self, observer: mpsc::UnboundedSender<Vec<u8>>) {
        *self
            .write_observer
            .lock()
            .expect("PTY write observer mutex poisoned") = Some(observer);
    }

    fn observed_write(&self, bytes: &[u8]) {
        if let Some(observer) = self
            .write_observer
            .lock()
            .expect("PTY write observer mutex poisoned")
            .as_ref()
        {
            let _ = observer.send(bytes.to_vec());
        }
    }

    pub async fn send(&self, intent: Intent) -> Result<InputResult, InputError> {
        let _send = self.send_lock.lock().await;
        let ask = match &intent {
            Intent::Answer { ask_id, .. } => Some(
                self.asks
                    .lock()
                    .expect("ask table mutex poisoned")
                    .get(ask_id)
                    .cloned()
                    .ok_or_else(|| InputError::UnknownAsk(ask_id.clone()))?,
            ),
            _ => None,
        };
        let program = crate::terminal::keymap::program_for(&intent, ask.as_ref())?;
        let prompt = match &intent {
            Intent::Prompt { text } => Some(text.as_str()),
            _ => None,
        };
        let answer = match &intent {
            Intent::Answer { answer, .. } => Some(answer),
            _ => None,
        };
        let steps = crate::terminal::keymap::encode(
            &self.keymap,
            program,
            &crate::terminal::keymap::Environment {
                ask: ask.as_ref(),
                answer,
                prompt,
            },
        )?;
        let bytes_written = self.write_key_steps(&steps).await?;
        if let Intent::Answer { ask_id, .. } = &intent {
            self.asks
                .lock()
                .expect("ask table mutex poisoned")
                .remove(ask_id);
        }
        let result = InputResult {
            intent,
            program,
            bytes_written,
        };
        let _ = self
            .events
            .send(PtyEvent::InputResult(result.clone()))
            .await;
        Ok(result)
    }

    /// Writes daemon-owned terminal or delivery bytes without representing
    /// them as semantic user input.
    pub async fn send_program(&self, program: Vec<PtyInput>) -> Result<usize, InputError> {
        let mut writer = self.writer.lock().await;
        let mut bytes_written = 0;
        for step in &program {
            match step {
                PtyInput::Bytes(bytes) => {
                    writer
                        .write_all(bytes)
                        .await
                        .map_err(pty_host::PtyError::Io)?;
                    writer.flush().await.map_err(pty_host::PtyError::Io)?;
                    self.observed_write(bytes);
                    bytes_written += bytes.len();
                }
                PtyInput::Delay(ms) => {
                    self.delays
                        .wait(Duration::from_millis(u64::from(*ms)))
                        .await;
                }
            }
        }
        Ok(bytes_written)
    }

    async fn write_key_steps(
        &self,
        steps: &[crate::terminal::keymap::KeyStep],
    ) -> Result<usize, InputError> {
        let mut writer = self.writer.lock().await;
        let mut bytes_written = 0;
        for step in steps {
            match step {
                crate::terminal::keymap::KeyStep::Write(bytes) => {
                    writer
                        .write_all(bytes)
                        .await
                        .map_err(pty_host::PtyError::Io)?;
                    writer.flush().await.map_err(pty_host::PtyError::Io)?;
                    self.observed_write(bytes);
                    bytes_written += bytes.len();
                }
                crate::terminal::keymap::KeyStep::Delay(delay) => {
                    self.delays.wait(*delay).await;
                }
            }
        }
        Ok(bytes_written)
    }

    pub fn resize(&self, size: pty_host::PtySize) -> Result<(), pty_host::PtyError> {
        self.terminal
            .as_ref()
            .ok_or_else(unavailable_pty_error)?
            .resize(size)
    }

    pub async fn stop(mut self, policy: pty_host::Terminate) -> pty_host::ExitStatus {
        if let Some(status) = self.exit.borrow().clone() {
            return status;
        }
        if let Some(handle) = &self.terminal {
            match policy {
                pty_host::Terminate::Kill => {
                    let _ = handle.signal_process_group(pty_host::ProcessGroupSignal::Kill);
                }
                pty_host::Terminate::Graceful { grace } => {
                    let _ = handle.signal_process_group(pty_host::ProcessGroupSignal::Terminate);
                    if let Ok(status) = tokio::time::timeout(grace, wait_exit(&mut self.exit)).await
                    {
                        return status;
                    }
                    let _ = handle.signal_process_group(pty_host::ProcessGroupSignal::Kill);
                }
            }
        }
        wait_exit(&mut self.exit).await
    }

    pub fn terminal(&self) -> Option<pty_host::PtyHandle> {
        self.terminal.clone()
    }

    pub fn terminal_output(&self) -> Option<mpsc::Receiver<Bytes>> {
        self.terminal_output
            .lock()
            .expect("terminal output mutex poisoned")
            .take()
    }
}

pub async fn spawn(
    launch: &Launch,
    size: pty_host::PtySize,
    env: &[(OsString, OsString)],
) -> Result<Session, SpawnError> {
    let version = probe_version(&launch.binary).await?;
    spawn_with_version(launch, size, version, env)
}

/// Spawn a live session when the host has already completed its shared
/// version probe.
pub fn spawn_with_version(
    launch: &Launch,
    size: pty_host::PtySize,
    version: semver::Version,
    env: &[(OsString, OsString)],
) -> Result<Session, SpawnError> {
    let session = launch.session_id.simple().to_string();
    let hook_dir = std::env::temp_dir().join(format!("ac-{}", &session[..8]));
    let receiver = HookReceiver::bind_sync(&hook_dir).map_err(SpawnError::Hook)?;
    let hook_path = receiver.path.clone();
    let process = crate::terminal::spawn(launch, size, &hook_path, env)?;
    let output = process.handle.output();
    let handle = process.handle.clone();
    let writer = writer_for_handle(handle.clone());
    let mut exit = process.exit;
    Ok(from_sources(Sources {
        pty: PtySource {
            output,
            writer,
            handle: Some(handle),
            exit: Box::pin(async move { exit.wait().await }),
        },
        hooks: HookSource::from_receiver(receiver),
        transcript: TranscriptSource::live(),
        version,
        delays: DelaySource::live(),
    }))
}

pub fn from_sources(sources: Sources) -> Session {
    let Sources {
        pty,
        hooks,
        transcript,
        version,
        delays,
    } = sources;
    let PtySource {
        output,
        writer,
        handle,
        exit,
    } = pty;
    let HookSource {
        mut payloads,
        receiver,
    } = hooks;
    let TranscriptSource {
        mut rows,
        relink,
        task,
        drain_before_exit,
    } = transcript;
    let (event_tx, events) = mpsc::channel(CHANNEL_CAPACITY);
    let (exit_tx, exit_rx) = watch::channel(None);
    let asks = Arc::new(Mutex::new(HashMap::new()));

    event_tx
        .try_send(PtyEvent::Ready {
            version: version.clone(),
        })
        .expect("new PTY event channel has capacity for readiness");

    let tx = event_tx.clone();
    let asks_from_hooks = asks.clone();
    let hook_forwarder = tokio::spawn(async move {
        let _receiver = receiver;
        let mut current_path = None;
        while let Some(hook) = payloads.recv().await {
            // A hook this build does not know names no transcript to follow.
            let relinking = hook.common().map(|common| {
                let path = common.transcript_path.clone();
                let path_changed = current_path.as_ref() != Some(&path);
                let reason = relink_reason(&hook, current_path.is_none());
                (path, path_changed, reason)
            });
            if let Some((path, path_changed, reason)) = relinking
                // Claude can compact a transcript in place. Its SessionStart
                // source is therefore the relink boundary even when the path
                // is unchanged.
                && (path_changed || matches!(reason, RelinkReason::Compact | RelinkReason::Clear))
            {
                if path_changed {
                    relink(path.clone());
                }
                current_path = Some(path.clone());
                if tx
                    .send(PtyEvent::Relink {
                        transcript_path: path,
                        reason,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            let ask = ask_from_hook(&hook);
            if tx.send(PtyEvent::Hook(hook)).await.is_err() {
                break;
            }
            if let Some(ask) = ask {
                asks_from_hooks
                    .lock()
                    .expect("ask table mutex poisoned")
                    .insert(ask.id.clone(), ask.kind.clone());
                if tx.send(PtyEvent::Ask(ask)).await.is_err() {
                    break;
                }
            }
        }
    });

    let tx = event_tx.clone();
    let asks_from_rows = asks.clone();
    let transcript_forwarder = tokio::spawn(async move {
        let _task = task;
        while let Some((path, row)) = rows.recv().await {
            let ask = ask_from_transcript(&row);
            if tx.send(PtyEvent::Transcript { path, row }).await.is_err() {
                break;
            }
            if let Some(ask) = ask {
                asks_from_rows
                    .lock()
                    .expect("ask table mutex poisoned")
                    .insert(ask.id.clone(), ask.kind.clone());
                if tx.send(PtyEvent::Ask(ask)).await.is_err() {
                    break;
                }
            }
        }
    });

    let tx = event_tx.clone();
    tokio::spawn(async move {
        let status = exit.await;
        if drain_before_exit {
            // Recorded sources close together at replay EOF. Joining both
            // forwarders acknowledges that every queued hook and transcript
            // row reached the common event stream before process exit can end
            // backend ingestion.
            let _ = hook_forwarder.await;
            let _ = transcript_forwarder.await;
        }
        exit_tx.send_replace(Some(status.clone()));
        let _ = tx.send(PtyEvent::Exited(status)).await;
    });

    Session {
        events,
        control: Control {
            writer: Arc::new(tokio::sync::Mutex::new(writer)),
            terminal_output: Arc::new(Mutex::new(Some(output))),
            terminal: handle,
            events: event_tx,
            keymap: Arc::new(crate::terminal::keymap::baked()),
            asks,
            send_lock: Arc::new(tokio::sync::Mutex::new(())),
            exit: exit_rx,
            write_observer: Arc::new(Mutex::new(None)),
            delays,
        },
    }
}

#[doc(hidden)]
pub fn from_recording(
    replay: &mut replay_support::StrictReplay,
    manifest: &replay_support::Manifest,
) -> Result<Session, SpawnError> {
    let pty = replay
        .transports
        .remove("pty")
        .ok_or(SpawnError::MissingTransport("pty"))?;
    let hooks = replay
        .transports
        .remove("hook")
        .ok_or(SpawnError::MissingTransport("hook"))?;
    let transcript = replay
        .transports
        .remove("transcript")
        .ok_or(SpawnError::MissingTransport("transcript"))?;
    let (output_tx, output) = mpsc::channel(CHANNEL_CAPACITY);
    let pty_pump = tokio::spawn(async move {
        pump_recorded_bytes(pty.reader, output_tx).await;
    });
    let (hook_source, hook_tx) = HookSource::channel(CHANNEL_CAPACITY);
    let hook_pump = tokio::spawn(async move {
        pump_hooks(hooks.reader, hook_tx).await;
    });
    let (row_tx, rows) = mpsc::channel(CHANNEL_CAPACITY);
    let fallback = PathBuf::from(format!("recording/{}.jsonl", manifest.spec));
    let transcript_pump = tokio::spawn(async move {
        pump_transcript(transcript.reader, row_tx, fallback).await;
    });
    Ok(from_sources(Sources {
        pty: PtySource {
            output,
            writer: Box::new(RecordingFrameWriter::new(pty.writer)),
            handle: None,
            exit: Box::pin(async move {
                let _ = pty_pump.await;
                let _ = hook_pump.await;
                let _ = transcript_pump.await;
                pty_host::ExitStatus::with_exit_code(0)
            }),
        },
        hooks: hook_source,
        transcript: TranscriptSource::recorded(rows),
        version: manifest.recorded.version.clone(),
        delays: DelaySource::replay(replay.clock.clone()),
    }))
}

/// Strict replay is line-oriented. PTY frames are hex-framed so arbitrary
/// terminal bytes (including newlines) retain their exact boundaries.
#[doc(hidden)]
pub fn encode_recording_bytes(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(4 + bytes.len() * 2);
    encoded.push_str("hex:");
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn decode_recording_bytes(line: &str) -> Option<Vec<u8>> {
    let encoded = line.strip_prefix("hex:")?;
    if encoded.len() % 2 != 0 {
        return None;
    }
    (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16).ok())
        .collect()
}

struct RecordingFrameWriter {
    inner: Box<dyn AsyncWrite + Unpin + Send>,
    pending: Vec<u8>,
    framed: Vec<u8>,
    framed_offset: usize,
}

impl RecordingFrameWriter {
    fn new(inner: Box<dyn AsyncWrite + Unpin + Send>) -> Self {
        Self {
            inner,
            pending: Vec::new(),
            framed: Vec::new(),
            framed_offset: 0,
        }
    }
}

impl AsyncWrite for RecordingFrameWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.pending.extend_from_slice(bytes);
        std::task::Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.framed.is_empty() && !self.pending.is_empty() {
            self.framed = encode_recording_bytes(&self.pending).into_bytes();
            self.framed.push(b'\n');
            self.pending.clear();
            self.framed_offset = 0;
        }
        while self.framed_offset < self.framed.len() {
            let offset = self.framed_offset;
            let chunk = self.framed[offset..].to_vec();
            match Pin::new(&mut self.inner).poll_write(cx, &chunk) {
                std::task::Poll::Ready(Ok(0)) => {
                    return std::task::Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "strict replay PTY frame write returned zero",
                    )));
                }
                std::task::Poll::Ready(Ok(written)) => self.framed_offset += written,
                std::task::Poll::Ready(Err(error)) => return std::task::Poll::Ready(Err(error)),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
        self.framed.clear();
        self.framed_offset = 0;
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.as_mut().poll_flush(cx) {
            std::task::Poll::Ready(Ok(())) => Pin::new(&mut self.inner).poll_shutdown(cx),
            other => other,
        }
    }
}

fn writer_for_handle(handle: pty_host::PtyHandle) -> Box<dyn AsyncWrite + Unpin + Send> {
    let (client, mut bridge) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let mut buffer = [0; 4096];
        loop {
            match bridge.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) if handle.write(&buffer[..read]).await.is_err() => break,
                Ok(_) => {}
            }
        }
    });
    Box::new(client)
}

async fn wait_exit(
    exit: &mut watch::Receiver<Option<pty_host::ExitStatus>>,
) -> pty_host::ExitStatus {
    loop {
        if let Some(status) = exit.borrow().clone() {
            return status;
        }
        if exit.changed().await.is_err() {
            return pty_host::ExitStatus::with_signal("exit monitor closed");
        }
    }
}

fn unavailable_pty_error() -> pty_host::PtyError {
    pty_host::PtyError::Io(std::io::Error::new(
        std::io::ErrorKind::NotConnected,
        "session has no live PTY handle",
    ))
}

fn relink_reason(hook: &HookPayload, initial: bool) -> RelinkReason {
    if initial {
        return RelinkReason::Initial;
    }
    let source = match hook {
        HookPayload::SessionStart(start) => Some(start.source.as_str()),
        _ => None,
    };
    match source {
        Some("compact") => RelinkReason::Compact,
        Some("clear") => RelinkReason::Clear,
        Some(other) => RelinkReason::Other(other.to_string()),
        None => RelinkReason::Other("hook transcript path changed".to_string()),
    }
}

fn ask_from_hook(hook: &HookPayload) -> Option<AskFacts> {
    let HookPayload::PermissionRequest(hooks::PermissionRequest {
        common,
        tool_name,
        tool_input,
        tool_use_id,
        permission_suggestions,
        ..
    }) = hook
    else {
        return None;
    };
    let id = tool_use_id
        .clone()
        .or_else(|| common.prompt_id.clone())
        .unwrap_or_else(|| format!("permission:{}:{tool_name}", common.session_id));
    let kind = if tool_name == "AskUserQuestion" {
        AskKind::Question {
            questions: question_facts(tool_input)?,
        }
    } else {
        AskKind::Permission {
            tool_name: tool_name.clone(),
            suggestions: permission_suggestions.as_ref().map_or(0, Vec::len),
            is_plan: tool_name == "ExitPlanMode",
        }
    };
    Some(AskFacts {
        id: AskId(id),
        kind,
    })
}

fn question_facts(input: &Value) -> Option<Vec<QuestionFact>> {
    Some(
        input
            .get("questions")?
            .as_array()?
            .iter()
            .map(|question| QuestionFact {
                options: question
                    .get("options")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len),
                multi_select: question
                    .get("multiSelect")
                    .or_else(|| question.get("multi_select"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                previews: question
                    .get("options")
                    .and_then(Value::as_array)
                    .is_some_and(|options| {
                        options.iter().any(|option| {
                            option
                                .get("preview")
                                .and_then(Value::as_str)
                                .is_some_and(|preview| !preview.is_empty())
                        })
                    }),
            })
            .collect(),
    )
}

fn ask_from_transcript(row: &TranscriptRow) -> Option<AskFacts> {
    let TranscriptRow::Assistant(row) = row else {
        return None;
    };
    for block in &row.message.content {
        let ContentBlock::ToolUse {
            id, name, input, ..
        } = block
        else {
            continue;
        };
        let id = id.clone();
        match name.as_str() {
            "ExitPlanMode" => {
                return Some(AskFacts {
                    id: AskId(id),
                    kind: AskKind::Permission {
                        tool_name: "ExitPlanMode".to_string(),
                        suggestions: 0,
                        is_plan: true,
                    },
                });
            }
            "AskUserQuestion" => {
                let questions = question_facts(input)?;
                return Some(AskFacts {
                    id: AskId(id),
                    kind: AskKind::Question { questions },
                });
            }
            _ => {}
        }
    }
    None
}

async fn pump_recorded_bytes(
    mut reader: Box<dyn tokio::io::AsyncBufRead + Unpin + Send>,
    tx: mpsc::Sender<Bytes>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let Some(bytes) = decode_recording_bytes(line.trim_end_matches('\n')) else {
                    continue;
                };
                if tx.send(Bytes::from(bytes)).await.is_err() {
                    break;
                }
            }
        }
    }
}

async fn pump_hooks(
    mut reader: Box<dyn tokio::io::AsyncBufRead + Unpin + Send>,
    tx: mpsc::Sender<HookPayload>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if let Ok(payload) = hooks::decode(line.trim().as_bytes())
                    && tx.send(payload).await.is_err()
                {
                    break;
                }
            }
        }
    }
}

async fn pump_transcript(
    mut reader: Box<dyn tokio::io::AsyncBufRead + Unpin + Send>,
    tx: mpsc::Sender<(PathBuf, TranscriptRow)>,
    fallback: PathBuf,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                let path = value
                    .get("path")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| fallback.clone());
                let row = value.get("row").cloned().unwrap_or(value);
                let Ok(row) = transcript::decode(row.to_string().as_bytes()) else {
                    continue;
                };
                if tx.send((path, row)).await.is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An assistant row holding one message with `content`.
    fn assistant_row(content: Value) -> TranscriptRow {
        let row = serde_json::json!({
            "type":"assistant",
            "uuid":"row-1",
            "parentUuid":null,
            "sessionId":"00000000-0000-0000-0000-000000000001",
            "timestamp":"2026-10-06T00:00:00.000Z",
            "version":"2.1.283",
            "cwd":"/tmp",
            "gitBranch":"HEAD",
            "isSidechain":false,
            "userType":"external",
            "entrypoint":"cli",
            "message":{
                "id":"msg-1",
                "type":"message",
                "role":"assistant",
                "model":"claude-haiku-4-5",
                "content":content,
                "usage":{"input_tokens":1,"output_tokens":1}
            }
        });
        let row = transcript::strict(row.to_string().as_bytes()).unwrap();
        assert!(matches!(row, TranscriptRow::Assistant(_)));
        row
    }

    #[tokio::test]
    async fn recording_frames_preserve_arbitrary_pty_bytes() {
        let bytes = b"first\nsecond\0\x1b[Z";
        let encoded = encode_recording_bytes(bytes);
        assert_eq!(
            decode_recording_bytes(&encoded).as_deref(),
            Some(bytes.as_slice())
        );

        let (inner, mut peer) = tokio::io::duplex(1024);
        let mut writer = RecordingFrameWriter::new(Box::new(inner));
        writer.write_all(bytes).await.unwrap();
        writer.flush().await.unwrap();
        let mut line = String::new();
        tokio::io::BufReader::new(&mut peer)
            .read_line(&mut line)
            .await
            .unwrap();
        assert_eq!(line.trim_end(), encoded);
    }

    type TestBundle = (
        Sources,
        mpsc::Sender<HookPayload>,
        mpsc::Sender<(PathBuf, TranscriptRow)>,
        mpsc::UnboundedReceiver<PathBuf>,
        tokio::io::DuplexStream,
        oneshot::Sender<pty_host::ExitStatus>,
    );

    fn source_bundle() -> TestBundle {
        let (session_writer, peer_writer) = tokio::io::duplex(4096);
        let (_output_tx, output) = mpsc::channel(4);
        let (hooks, hook_tx) = HookSource::channel(16);
        let (transcript, row_tx, paths) = TranscriptSource::channel(16);
        let (exit_tx, exit_rx) = oneshot::channel();
        (
            Sources {
                pty: PtySource {
                    output,
                    writer: Box::new(session_writer),
                    handle: None,
                    exit: Box::pin(async move {
                        exit_rx.await.unwrap_or_else(|_| {
                            pty_host::ExitStatus::with_signal("test source closed")
                        })
                    }),
                },
                hooks,
                transcript,
                version: "2.1.312".parse().unwrap(),
                delays: DelaySource::live(),
            },
            hook_tx,
            row_tx,
            paths,
            peer_writer,
            exit_tx,
        )
    }

    fn session_start(path: &str, source: &str) -> HookPayload {
        let raw = serde_json::json!({
            "hook_event_name":"SessionStart",
            "session_id":"00000000-0000-0000-0000-000000000001",
            "transcript_path":path,
            "cwd":"/tmp",
            "source":source,
        });
        hooks::strict(raw.to_string().as_bytes()).unwrap()
    }

    async fn next(events: &mut EventStream) -> PtyEvent {
        tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("event stream remains open")
    }

    #[tokio::test]
    async fn replay_delays_advance_virtual_time_without_waiting() {
        let clock = replay_support::ReplayClock::new(Some(0));
        let delays = DelaySource::replay(clock.clone());
        let started = std::time::Instant::now();

        delays.wait(Duration::from_secs(30)).await;

        assert_eq!(clock.current_us(), Some(30_000_000));
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn sources_emit_ready_compact_clear_relinks_and_exit() {
        let (sources, hooks, _rows, mut paths, _writer, exit) = source_bundle();
        let mut session = from_sources(sources);
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Ready { .. }
        ));

        hooks
            .send(session_start("/tmp/one", "startup"))
            .await
            .unwrap();
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Relink {
                reason: RelinkReason::Initial,
                ..
            }
        ));
        assert_eq!(paths.recv().await.unwrap(), PathBuf::from("/tmp/one"));
        assert!(matches!(next(&mut session.events).await, PtyEvent::Hook(_)));

        hooks
            .send(session_start("/tmp/one", "compact"))
            .await
            .unwrap();
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Relink {
                reason: RelinkReason::Compact,
                ..
            }
        ));
        assert!(matches!(next(&mut session.events).await, PtyEvent::Hook(_)));
        assert!(paths.try_recv().is_err());
        hooks
            .send(session_start("/tmp/three", "clear"))
            .await
            .unwrap();
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Relink {
                reason: RelinkReason::Clear,
                ..
            }
        ));

        exit.send(pty_host::ExitStatus::with_exit_code(7)).unwrap();
        loop {
            if let PtyEvent::Exited(status) = next(&mut session.events).await {
                assert_eq!(status.exit_code(), 7);
                break;
            }
        }
    }

    #[tokio::test]
    async fn hooks_and_transcript_tool_uses_derive_ask_facts() {
        let (sources, hooks, rows, _paths, _writer, _exit) = source_bundle();
        let mut session = from_sources(sources);
        let _ = next(&mut session.events).await;
        let raw = serde_json::json!({
            "hook_event_name":"PermissionRequest",
            "session_id":"00000000-0000-0000-0000-000000000001",
            "transcript_path":"/tmp/one",
            "cwd":"/tmp",
            "tool_name":"Bash",
            "tool_use_id":"permission-1",
            "tool_input":{},
            "permission_suggestions":[{"type":"addDirectories","directories":["/tmp"],"destination":"session"}],
        });
        hooks
            .send(hooks::decode(raw.to_string().as_bytes()).unwrap())
            .await
            .unwrap();
        loop {
            if let PtyEvent::Ask(ask) = next(&mut session.events).await {
                assert_eq!(ask.id, AskId("permission-1".to_owned()));
                assert!(matches!(
                    ask.kind,
                    AskKind::Permission {
                        suggestions: 1,
                        is_plan: false,
                        ..
                    }
                ));
                break;
            }
        }

        let raw = serde_json::json!({
            "hook_event_name":"PermissionRequest",
            "session_id":"00000000-0000-0000-0000-000000000001",
            "transcript_path":"/tmp/one",
            "cwd":"/tmp",
            "tool_name":"AskUserQuestion",
            "tool_use_id":"question-hook-1",
            "tool_input":{"questions":[
                {"options":[{},{}],"multiSelect":false},
                {"options":[{},{},{}],"multiSelect":true}
            ]}
        });
        hooks
            .send(hooks::decode(raw.to_string().as_bytes()).unwrap())
            .await
            .unwrap();
        loop {
            if let PtyEvent::Ask(ask) = next(&mut session.events).await {
                assert_eq!(ask.id, AskId("question-hook-1".to_owned()));
                assert_eq!(
                    ask.kind,
                    AskKind::Question {
                        questions: vec![
                            QuestionFact {
                                options: 2,
                                multi_select: false,
                                previews: false,
                            },
                            QuestionFact {
                                options: 3,
                                multi_select: true,
                                previews: false,
                            },
                        ],
                    }
                );
                break;
            }
        }

        let row = assistant_row(
            serde_json::json!([{"type":"tool_use","id":"ask-1","name":"AskUserQuestion","input":{"questions":[{"options":[{},{}],"multiSelect":true}]}}]),
        );
        rows.send((PathBuf::from("/tmp/one"), row)).await.unwrap();
        loop {
            if let PtyEvent::Ask(ask) = next(&mut session.events).await {
                assert_eq!(ask.id, AskId("ask-1".to_string()));
                assert_eq!(
                    ask.kind,
                    AskKind::Question {
                        questions: vec![QuestionFact {
                            options: 2,
                            multi_select: true,
                            previews: false,
                        }],
                    }
                );
                break;
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn manual_plan_answer_waits_for_the_approval_menu_to_settle() {
        let (sources, hooks, _rows, _paths, mut peer, _exit) = source_bundle();
        let mut session = from_sources(sources);
        let _ = next(&mut session.events).await;
        let raw = serde_json::json!({
            "hook_event_name":"PermissionRequest",
            "session_id":"00000000-0000-0000-0000-000000000001",
            "transcript_path":"/tmp/one",
            "cwd":"/tmp",
            "prompt_id":"plan-prompt-1",
            "permission_mode":"plan",
            "tool_name":"ExitPlanMode",
            "tool_input":{"plan":"Update README.md"},
        });
        hooks
            .send(hooks::decode(raw.to_string().as_bytes()).unwrap())
            .await
            .unwrap();
        let ask = loop {
            if let PtyEvent::Ask(ask) = next(&mut session.events).await {
                break ask;
            }
        };
        assert_eq!(ask.id, AskId("plan-prompt-1".to_owned()));

        let started = tokio::time::Instant::now();
        session
            .control
            .send(Intent::Answer {
                ask_id: ask.id,
                answer: AskAnswer::Plan(PlanAnswer::ApproveManual),
            })
            .await
            .unwrap();

        assert!(started.elapsed() >= Duration::from_millis(800));
        let mut digit = [0];
        peer.read_exact(&mut digit).await.unwrap();
        assert_eq!(&digit, b"2");
    }

    #[tokio::test]
    async fn control_sends_semantic_intent_and_paste_delivery() {
        let (sources, _hooks, _rows, _paths, mut peer, _exit) = source_bundle();
        let mut session = from_sources(sources);
        let _ = next(&mut session.events).await;
        let intent = Intent::Prompt {
            text: "hello".to_owned(),
        };
        let result = session.control.send(intent.clone()).await.unwrap();
        assert_eq!(result.intent, intent);
        assert_eq!(result.program, crate::terminal::keymap::ProgramName::Prompt);
        assert_eq!(result.bytes_written, b"\x1b[200~hello\x1b[201~\r".len());
        let mut bytes = vec![0; result.bytes_written];
        peer.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"\x1b[200~hello\x1b[201~\r");
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::InputResult(event) if event == result
        ));
    }

    #[tokio::test]
    async fn answer_intents_use_and_consume_ask_ids() {
        let (sources, _hooks, rows, _paths, _peer, _exit) = source_bundle();
        let mut session = from_sources(sources);
        let _ = next(&mut session.events).await;
        rows.send((
            PathBuf::from("/tmp/one"),
            assistant_row(serde_json::json!([{
                "type":"tool_use",
                "id":"question-1",
                "name":"AskUserQuestion",
                "input":{"questions":[{"options":[{},{}],"multiSelect":false}]}
            }])),
        ))
        .await
        .unwrap();
        assert!(matches!(
            next(&mut session.events).await,
            PtyEvent::Transcript { .. }
        ));
        assert!(matches!(next(&mut session.events).await, PtyEvent::Ask(_)));

        let intent = Intent::Answer {
            ask_id: AskId("question-1".to_owned()),
            answer: AskAnswer::Question(QuestionResponse {
                answers: vec![QuestionAnswer {
                    selected: vec![1],
                    other: None,
                }],
            }),
        };
        let result = session.control.send(intent.clone()).await.unwrap();
        assert_eq!(result.intent, intent);
        assert_eq!(
            result.program,
            crate::terminal::keymap::ProgramName::QuestionForm
        );
        assert!(matches!(
            session.control.send(result.intent).await,
            Err(InputError::UnknownAsk(AskId(ref id))) if id == "question-1"
        ));
    }

    #[tokio::test]
    async fn semantic_input_errors_remain_typed() {
        let (sources, _hooks, rows, _paths, _peer, _exit) = source_bundle();
        let mut session = from_sources(sources);
        let _ = next(&mut session.events).await;

        assert!(matches!(
            session
                .control
                .send(Intent::Prompt {
                    text: "bad\u{1b}".to_owned()
                })
                .await,
            Err(InputError::UnsafeText { .. })
        ));
        assert!(matches!(
            session
                .control
                .send(Intent::Answer {
                    ask_id: AskId("missing".to_owned()),
                    answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
                })
                .await,
            Err(InputError::UnknownAsk(AskId(ref id))) if id == "missing"
        ));

        rows.send((
            PathBuf::from("/tmp/one"),
            assistant_row(serde_json::json!([{
                "type":"tool_use",
                "id":"plan-1",
                "name":"ExitPlanMode",
                "input":{}
            }])),
        ))
        .await
        .unwrap();
        let _ = next(&mut session.events).await;
        let _ = next(&mut session.events).await;
        assert!(matches!(
            session
                .control
                .send(Intent::Answer {
                    ask_id: AskId("plan-1".to_owned()),
                    answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
                })
                .await,
            Err(InputError::AnswerMismatchesAsk { .. })
        ));

        let (sources, hooks, _rows, _paths, _peer, _exit) = source_bundle();
        let mut session = from_sources(sources);
        let _ = next(&mut session.events).await;
        let raw = serde_json::json!({
            "hook_event_name":"PermissionRequest",
            "session_id":"00000000-0000-0000-0000-000000000001",
            "transcript_path":"/tmp/one",
            "cwd":"/tmp",
            "tool_name":"Bash",
            "tool_use_id":"permission-7",
            "tool_input":{},
            "permission_suggestions": vec![serde_json::json!({
                "type":"addDirectories",
                "directories":["/tmp"],
                "destination":"session"
            }); 7],
        });
        hooks
            .send(hooks::decode(raw.to_string().as_bytes()).unwrap())
            .await
            .unwrap();
        loop {
            if matches!(next(&mut session.events).await, PtyEvent::Ask(_)) {
                break;
            }
        }
        assert!(matches!(
            session
                .control
                .send(Intent::Answer {
                    ask_id: AskId("permission-7".to_owned()),
                    answer: AskAnswer::Permission(PermissionAnswer::AllowScoped { suggestion: 0 }),
                })
                .await,
            Err(InputError::UnverifiedShape { .. })
        ));
    }

    #[test]
    fn paste_program_normalizes_controls_without_exposing_terminal_input() {
        assert_eq!(
            paste_program("tab\there\r\nreturn\r escape\x1b[201~rest\0"),
            vec![
                PtyInput::Bytes(b"\x1b[200~tab here\nreturn\n escape[201~rest\x1b[201~".to_vec()),
                PtyInput::Delay(400),
                PtyInput::Bytes(b"\r".to_vec()),
            ]
        );
    }
}
