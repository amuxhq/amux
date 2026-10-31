use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream};

use crate::{IoDirection, IoEvent, Recording};

const DEFAULT_TRANSPORT_ID: &str = "<default>";

#[derive(Debug, Clone)]
struct ReadEntry {
    us: u64,
    line: String,
    transport_id: String,
}

#[derive(Debug, Clone)]
struct WriteEntry {
    line: String,
    transport_id: String,
}

#[derive(Debug)]
struct ReplayState {
    expected_writes: Vec<WriteEntry>,
    read_groups: Vec<Vec<ReadEntry>>,
    matched_writes: Vec<bool>,
    validated_writes: usize,
    delivered_reads: usize,
    next_group_idx: usize,
    next_read_idx: usize,
    declared_transports: BTreeSet<String>,
    used_transports: BTreeSet<String>,
    write_buffers: BTreeMap<String, Vec<u8>>,
    trailing_writes: Vec<String>,
    write_mismatches: Vec<ReplayWriteMismatch>,
    read_delivery_failures: Vec<String>,
    open_writers: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayWriteMismatch {
    pub index: usize,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayReport {
    pub validated_writes: usize,
    pub delivered_reads: usize,
    pub remaining_reads: usize,
    pub remaining_writes: usize,
    pub unused_transports: Vec<String>,
    pub trailing_writes: Vec<String>,
    pub trailing_output: Option<String>,
    pub write_mismatches: Vec<ReplayWriteMismatch>,
    pub read_delivery_failures: Vec<String>,
}

impl ReplayReport {
    pub fn is_complete(&self) -> bool {
        self.remaining_reads == 0
            && self.remaining_writes == 0
            && self.unused_transports.is_empty()
            && self.trailing_writes.is_empty()
            && self.trailing_output.is_none()
            && self.write_mismatches.is_empty()
            && self.read_delivery_failures.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayError {
    pub report: ReplayReport,
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let report = &self.report;
        write!(
            formatter,
            "replay incomplete: {} unread, {} unwritten, {} unused transports, {} trailing writes, {} write mismatches, {} read delivery failures",
            report.remaining_reads,
            report.remaining_writes,
            report.unused_transports.len(),
            report.trailing_writes.len() + usize::from(report.trailing_output.is_some()),
            report.write_mismatches.len(),
            report.read_delivery_failures.len(),
        )
    }
}

impl std::error::Error for ReplayError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayAdvance {
    Advanced { event_us: u64 },
    BlockedOnWrite,
    Exhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplayPeek {
    Ready { event_us: u64 },
    BlockedOnWrite,
    Exhausted,
}

#[derive(Debug, Clone)]
pub struct ReplayController {
    state: Arc<Mutex<ReplayState>>,
    outputs: BTreeMap<String, Arc<tokio::sync::Mutex<DuplexStream>>>,
    progress: Arc<tokio::sync::Notify>,
}

pub struct ReplayTransport {
    pub reader: Box<dyn AsyncBufRead + Unpin + Send>,
    pub writer: Box<dyn AsyncWrite + Unpin + Send>,
}

pub struct StrictReplay {
    pub transports: BTreeMap<String, ReplayTransport>,
    pub controller: ReplayController,
    pub clock: ReplayClock,
}

#[derive(Debug, Default, Clone)]
pub struct ReplayClock {
    inner: Arc<Mutex<ReplayClockState>>,
}

#[derive(Debug, Default)]
struct ReplayClockState {
    controller: Option<ReplayController>,
    current_us: Option<u64>,
}

/// Build a strict replay with one independently owned I/O pair per named
/// transport in the recording.
pub fn strict_replay(recording: &Recording) -> StrictReplay {
    let transport_ids = recording
        .io
        .iter()
        .map(event_transport_id)
        .collect::<BTreeSet<_>>();
    let state = replay_state(recording.io.clone(), transport_ids.len());
    let progress = Arc::new(tokio::sync::Notify::new());
    let mut outputs = BTreeMap::new();
    let mut transports = BTreeMap::new();

    for transport_id in transport_ids {
        let (read_half, write_half) = tokio::io::duplex(1024 * 1024);
        outputs.insert(
            transport_id.clone(),
            Arc::new(tokio::sync::Mutex::new(write_half)),
        );
        transports.insert(
            transport_id.clone(),
            ReplayTransport {
                reader: Box::new(BufReader::new(read_half)),
                writer: Box::new(ReplayWriter {
                    state: Arc::clone(&state),
                    transport_id,
                    progress: Arc::clone(&progress),
                }),
            },
        );
    }

    let controller = ReplayController {
        state,
        outputs,
        progress,
    };
    let clock = ReplayClock::new(None);
    clock.inner.lock().expect("replay clock lock").controller = Some(controller.clone());

    StrictReplay {
        transports,
        controller,
        clock,
    }
}

fn replay_state(script: Vec<IoEvent>, open_writers: usize) -> Arc<Mutex<ReplayState>> {
    let (expected_writes, read_groups, declared_transports) = split_script(script);
    let matched_writes = vec![false; expected_writes.len()];
    Arc::new(Mutex::new(ReplayState {
        expected_writes,
        read_groups,
        matched_writes,
        validated_writes: 0,
        delivered_reads: 0,
        next_group_idx: 0,
        next_read_idx: 0,
        declared_transports,
        used_transports: BTreeSet::new(),
        write_buffers: BTreeMap::new(),
        trailing_writes: Vec::new(),
        write_mismatches: Vec::new(),
        read_delivery_failures: Vec::new(),
        open_writers,
    }))
}

fn split_script(script: Vec<IoEvent>) -> (Vec<WriteEntry>, Vec<Vec<ReadEntry>>, BTreeSet<String>) {
    let mut expected_writes = Vec::new();
    let mut read_groups: Vec<Vec<ReadEntry>> = Vec::new();
    let mut current_reads = Vec::new();
    let mut declared_transports = BTreeSet::new();

    for event in script {
        let transport_id = event_transport_id(&event);
        declared_transports.insert(transport_id.clone());
        match event.direction {
            IoDirection::Write => {
                read_groups.push(std::mem::take(&mut current_reads));
                expected_writes.push(WriteEntry {
                    line: event.line,
                    transport_id,
                });
            }
            IoDirection::Read => {
                current_reads.push(ReadEntry {
                    us: event.us,
                    line: event.line,
                    transport_id,
                });
            }
        }
    }

    read_groups.push(current_reads);
    (expected_writes, read_groups, declared_transports)
}

fn event_transport_id(event: &IoEvent) -> String {
    event
        .transport_id
        .clone()
        .unwrap_or_else(|| DEFAULT_TRANSPORT_ID.to_string())
}

impl ReplayController {
    /// Deliver recorded reads, sleeping until the application supplies any
    /// expected write. This does not close the streams when the script ends.
    pub async fn drive(&self) {
        loop {
            let notified = self.progress.notified();
            match self.peek_next() {
                ReplayPeek::Ready { .. } => {
                    if matches!(self.advance_one().await, ReplayAdvance::Exhausted) {
                        return;
                    }
                }
                ReplayPeek::BlockedOnWrite => notified.await,
                ReplayPeek::Exhausted => return,
            }
        }
    }

    /// Send EOF after the application has finished its replay assertions.
    /// Controllers and clocks may remain alive for final accounting without
    /// keeping simulated processes waiting for their output streams to close.
    pub async fn close_reads(&self) -> io::Result<()> {
        for output in self.outputs.values() {
            output.lock().await.shutdown().await?;
        }
        Ok(())
    }

    fn peek_next(&self) -> ReplayPeek {
        let state = self.state.lock().expect("replay state lock");
        peek_next_locked(&state)
    }

    pub async fn advance_one(&self) -> ReplayAdvance {
        let entry = {
            let mut state = self.state.lock().expect("replay state lock");
            match peek_next_locked(&state) {
                ReplayPeek::Ready { .. } => take_next_locked(&mut state),
                ReplayPeek::BlockedOnWrite => return ReplayAdvance::BlockedOnWrite,
                ReplayPeek::Exhausted => return ReplayAdvance::Exhausted,
            }
        };

        let Some(entry) = entry else {
            return ReplayAdvance::Exhausted;
        };

        let Some(output) = self.outputs.get(&entry.transport_id).cloned() else {
            self.state
                .lock()
                .expect("replay state lock")
                .read_delivery_failures
                .push(format!(
                    "no replay output for transport {}",
                    entry.transport_id
                ));
            return ReplayAdvance::Exhausted;
        };
        let delivery = async {
            let mut writer = output.lock().await;
            writer.write_all(entry.line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await
        }
        .await;

        let mut state = self.state.lock().expect("replay state lock");
        state.used_transports.insert(entry.transport_id);
        if let Err(error) = delivery {
            state.read_delivery_failures.push(error.to_string());
            return ReplayAdvance::Exhausted;
        }
        state.delivered_reads += 1;

        ReplayAdvance::Advanced { event_us: entry.us }
    }

    /// Prove that every recorded event was accounted for.
    #[allow(
        clippy::result_large_err,
        reason = "ReplayError intentionally carries the complete public accounting report"
    )]
    pub fn finish(&self) -> Result<ReplayReport, ReplayError> {
        let state = self.state.lock().expect("replay state lock");
        let total_reads = state.read_groups.iter().map(Vec::len).sum::<usize>();
        let report = ReplayReport {
            validated_writes: state.validated_writes,
            delivered_reads: state.delivered_reads,
            remaining_reads: total_reads.saturating_sub(state.delivered_reads),
            remaining_writes: state
                .expected_writes
                .len()
                .saturating_sub(state.validated_writes),
            unused_transports: state
                .declared_transports
                .difference(&state.used_transports)
                .cloned()
                .collect(),
            trailing_writes: state.trailing_writes.clone(),
            trailing_output: trailing_output(&state.write_buffers),
            write_mismatches: state.write_mismatches.clone(),
            read_delivery_failures: state.read_delivery_failures.clone(),
        };

        if report.is_complete() {
            Ok(report)
        } else {
            Err(ReplayError { report })
        }
    }
}

fn trailing_output(write_buffers: &BTreeMap<String, Vec<u8>>) -> Option<String> {
    let nonempty = write_buffers
        .iter()
        .filter(|(_, bytes)| !bytes.is_empty())
        .collect::<Vec<_>>();
    match nonempty.as_slice() {
        [] => None,
        [(_, bytes)] => Some(String::from_utf8_lossy(bytes).into_owned()),
        many => Some(
            many.iter()
                .map(|(transport, bytes)| {
                    format!("{transport}: {}", String::from_utf8_lossy(bytes))
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    }
}

impl ReplayClock {
    pub fn new(start_us: Option<u64>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ReplayClockState {
                controller: None,
                current_us: start_us,
            })),
        }
    }

    pub fn current_us(&self) -> Option<u64> {
        self.inner.lock().expect("replay clock lock").current_us
    }

    fn controller(&self) -> Option<ReplayController> {
        self.inner
            .lock()
            .expect("replay clock lock")
            .controller
            .clone()
    }

    pub async fn advance_for(&self, duration: Duration) -> ReplayAdvance {
        let target_us = {
            let mut inner = self.inner.lock().expect("replay clock lock");
            let start_us = inner.current_us.unwrap_or(0);
            let delta_us = duration.as_micros().try_into().unwrap_or(u64::MAX);
            let target_us = start_us.saturating_add(delta_us);
            inner.current_us = Some(target_us);
            target_us
        };
        let Some(controller) = self.controller() else {
            return ReplayAdvance::Exhausted;
        };

        let mut last_result = status(controller.peek_next());
        loop {
            match controller.peek_next() {
                ReplayPeek::Ready { event_us } if event_us <= target_us => {}
                _ => return last_result,
            }
            let result = controller.advance_one().await;
            match result {
                ReplayAdvance::Advanced { .. } => last_result = result,
                ReplayAdvance::BlockedOnWrite | ReplayAdvance::Exhausted => return result,
            }
        }
    }
}

/// What a clock reports when it delivers nothing.
fn status(peek: ReplayPeek) -> ReplayAdvance {
    match peek {
        ReplayPeek::BlockedOnWrite => ReplayAdvance::BlockedOnWrite,
        ReplayPeek::Ready { .. } | ReplayPeek::Exhausted => ReplayAdvance::Exhausted,
    }
}

struct ReplayWriter {
    state: Arc<Mutex<ReplayState>>,
    transport_id: String,
    progress: Arc<tokio::sync::Notify>,
}

impl AsyncWrite for ReplayWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.state
            .lock()
            .expect("replay state lock")
            .write_buffers
            .entry(this.transport_id.clone())
            .or_default()
            .extend_from_slice(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        let this = self.get_mut();
        let mut state = this.state.lock().expect("replay state lock");

        loop {
            let line_bytes = {
                let buffer = state
                    .write_buffers
                    .entry(this.transport_id.clone())
                    .or_default();
                let Some(pos) = buffer.iter().position(|&b| b == b'\n') else {
                    break;
                };
                buffer.drain(..=pos).collect::<Vec<_>>()
            };
            let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1]).to_string();

            if state.validated_writes >= state.expected_writes.len() {
                state.trailing_writes.push(line.clone());
                return std::task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "ReplayWriteMismatch: unexpected replay write at index {}: {line}",
                        state.validated_writes
                    ),
                )));
            }

            let write_idx = state.validated_writes;
            let candidates = concurrent_writes_from(&state, write_idx);
            let mut transport_candidates = candidates.iter().copied().filter(|&candidate| {
                state.expected_writes[candidate].transport_id == this.transport_id
            });
            let expected_idx = transport_candidates.clone().next().unwrap_or(write_idx);
            let Some(hit) = transport_candidates
                .find(|&candidate| write_equals(&line, &state.expected_writes[candidate].line))
            else {
                let expected = state.expected_writes[expected_idx].clone();
                state.write_mismatches.push(ReplayWriteMismatch {
                    index: expected_idx,
                    expected: expected.line,
                    actual: line,
                });
                return std::task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("ReplayWriteMismatch: replay write mismatch at index {expected_idx}"),
                )));
            };

            let expected = state.expected_writes[hit].clone();
            state.used_transports.insert(expected.transport_id);
            state.matched_writes[hit] = true;
            while state
                .matched_writes
                .get(state.validated_writes)
                .copied()
                .unwrap_or(false)
            {
                state.validated_writes += 1;
            }
            this.progress.notify_one();
        }

        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

impl Drop for ReplayWriter {
    fn drop(&mut self) {
        let mut state = self.state.lock().expect("replay state lock");
        state.open_writers = state.open_writers.saturating_sub(1);
        drop(state);
        self.progress.notify_one();
    }
}

/// Compare written lines as JSON so object key order and insignificant
/// whitespace do not decide whether replay succeeds.
fn write_equals(actual: &str, expected: &str) -> bool {
    match (
        serde_json::from_str::<serde_json::Value>(actual),
        serde_json::from_str::<serde_json::Value>(expected),
    ) {
        (Ok(actual), Ok(expected)) => actual == expected,
        _ => actual.trim() == expected.trim(),
    }
}

/// Return the unmatched writes that the recording did not causally order:
/// the first of each transport's before the next read.
fn concurrent_writes_from(state: &ReplayState, frontier: usize) -> Vec<usize> {
    let mut candidates: Vec<usize> = Vec::new();
    let mut index = frontier;
    while index < state.expected_writes.len() {
        if index > frontier && !state.read_groups[index].is_empty() {
            break;
        }
        if !state.matched_writes[index] {
            let transport = &state.expected_writes[index].transport_id;
            if !candidates
                .iter()
                .any(|&candidate| state.expected_writes[candidate].transport_id == *transport)
            {
                candidates.push(index);
            }
        }
        index += 1;
    }
    candidates
}

fn peek_next_locked(state: &ReplayState) -> ReplayPeek {
    let mut group_idx = state.next_group_idx;
    let mut read_idx = state.next_read_idx;

    loop {
        if group_idx >= state.read_groups.len() {
            return ReplayPeek::Exhausted;
        }
        if group_idx > state.validated_writes {
            return if state.open_writers == 0 {
                ReplayPeek::Exhausted
            } else {
                ReplayPeek::BlockedOnWrite
            };
        }

        let group = &state.read_groups[group_idx];
        if read_idx >= group.len() {
            group_idx += 1;
            read_idx = 0;
            continue;
        }

        return ReplayPeek::Ready {
            event_us: group[read_idx].us,
        };
    }
}

fn take_next_locked(state: &mut ReplayState) -> Option<ReadEntry> {
    loop {
        if state.next_group_idx >= state.read_groups.len() {
            return None;
        }
        if state.next_group_idx > state.validated_writes {
            return None;
        }

        let group = &state.read_groups[state.next_group_idx];
        if state.next_read_idx >= group.len() {
            state.next_group_idx += 1;
            state.next_read_idx = 0;
            continue;
        }

        let entry = group[state.next_read_idx].clone();
        state.next_read_idx += 1;
        return Some(entry);
    }
}
