//! What a resumed link sends before its handshake completes, and sending
//! it again when the far end turns that early data away.
//!
//! A dial holding the peer's session ticket uses the connection at once:
//! the control stream's Hello and the early calls' streams go out as 0-RTT
//! data. The far end may still refuse the early data (it lost the ticket,
//! or the ticket was spent) and finish a full handshake; quinn then forgets
//! every stream opened before, numbering streams from the start again. So
//! every stream opened while the outcome is unknown keeps what it wrote,
//! and on a refusal is opened again in the order it first was, the control
//! stream first, and writes it all again as ordinary data. The far end
//! takes the first stream it sees as the control stream, so nothing else
//! opens a stream until that is done.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker, ready};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::watch;

use super::link_log::EarlyData;

/// Where a connection's early data stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// The handshake is under way: streams write early data.
    Pending,
    Accepted,
    /// Refused, and every stream opened before has been opened again.
    Rejected,
    /// The handshake failed, or did not finish in time.
    Failed,
}

/// One connection's early data: whether the far end took it, and the
/// streams that wrote it.
pub(crate) struct Early {
    connection: quinn::Connection,
    /// Held across every stream opened on the connection while early data
    /// may still be refused, so a stream opened then cannot take a number
    /// a stream sent again needs.
    opening: tokio::sync::Mutex<()>,
    state: Mutex<State>,
    settled: watch::Sender<Phase>,
}

struct State {
    phase: Phase,
    /// Every stream opened in early data, in the order it was opened: the
    /// control stream first.
    opened: Vec<Weak<Reopened>>,
    waiting: Vec<Waker>,
}

/// What a stream wrote in early data, and the stream it continues on if
/// the far end refused it.
#[derive(Default)]
struct Reopened {
    /// Everything written in early data, and whether the stream was
    /// finished there.
    sent: Mutex<(Vec<u8>, bool)>,
    send: Mutex<Option<quinn::SendStream>>,
    recv: Mutex<Option<quinn::RecvStream>>,
}

impl Early {
    /// Watches the outcome of `connection`'s early data, closing the
    /// connection if its handshake has not finished within `deadline`.
    pub(crate) fn start(
        connection: quinn::Connection,
        accepted: quinn::ZeroRttAccepted,
        deadline: Duration,
    ) -> Arc<Self> {
        let (settled, _) = watch::channel(Phase::Pending);
        let early = Arc::new(Self {
            connection,
            opening: tokio::sync::Mutex::new(()),
            state: Mutex::new(State {
                phase: Phase::Pending,
                opened: Vec::new(),
                waiting: Vec::new(),
            }),
            settled,
        });
        let watched = Arc::downgrade(&early);
        tokio::spawn(async move {
            let accepted = tokio::time::timeout(deadline, accepted).await;
            let Some(early) = watched.upgrade() else {
                return;
            };
            let _opening = early.opening.lock().await;
            if early.phase() != Phase::Pending {
                return;
            }
            match accepted {
                Ok(true) => early.settle(Phase::Accepted),
                Err(_) => {
                    early
                        .connection
                        .close(0_u32.into(), b"the handshake did not finish in time");
                    early.settle(Phase::Failed);
                }
                // The far end refused the early data, or the handshake
                // failed: only a failure leaves the connection closed.
                Ok(false) if early.connection.close_reason().is_some() => {
                    early.settle(Phase::Failed);
                }
                Ok(false) => {
                    early.reopen(None).await;
                }
            }
        });
        early
    }

    /// What became of the early data, as the link log says it; `None`
    /// until the handshake has finished.
    pub(crate) fn outcome(&self) -> Option<EarlyData> {
        match self.phase() {
            Phase::Pending => None,
            Phase::Accepted => Some(EarlyData::Accepted),
            Phase::Rejected => Some(EarlyData::Rejected),
            Phase::Failed => Some(EarlyData::None),
        }
    }

    /// Waits for the handshake to finish: the early data's outcome, or why
    /// the connection failed.
    pub(crate) async fn handshake(&self) -> Result<EarlyData, quinn::ConnectionError> {
        let mut settled = self.settled.subscribe();
        let phase = *settled
            .wait_for(|phase| *phase != Phase::Pending)
            .await
            .expect("the early data's watch lives as long as it");
        match phase {
            Phase::Accepted => Ok(EarlyData::Accepted),
            Phase::Rejected => Ok(EarlyData::Rejected),
            Phase::Pending | Phase::Failed => Err(self
                .connection
                .close_reason()
                .unwrap_or(quinn::ConnectionError::TimedOut)),
        }
    }

    /// Opens a stream on the connection: one in early data while the
    /// handshake is under way, which is sent again should the far end
    /// refuse it.
    pub(crate) async fn open(
        self: &Arc<Self>,
    ) -> Result<(EarlySend, EarlyRecv), quinn::ConnectionError> {
        let _opening = self.opening.lock().await;
        let (send, recv) = self.connection.open_bi().await?;
        // What the stream is, decided under the state's lock and acted on
        // once it is let go.
        enum Opened {
            Plain,
            Early(Arc<Reopened>),
            AfterHandshake { refused: bool },
        }
        let opened = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.phase != Phase::Pending {
                Opened::Plain
            } else if recv.is_0rtt() {
                let reopened = Arc::new(Reopened::default());
                state.opened.push(Arc::downgrade(&reopened));
                Opened::Early(reopened)
            } else {
                // The handshake finished under this open before its
                // outcome was taken. Refused early data numbers streams
                // from the start again, so a number below the streams
                // opened early says it was refused, and this stream is the
                // first of those sent again.
                Opened::AfterHandshake {
                    refused: send.id().index() < state.opened.len() as u64,
                }
            }
        };
        let refused = match opened {
            Opened::Plain => return Ok(plain(send, recv)),
            Opened::AfterHandshake { refused } => refused,
            Opened::Early(reopened) => {
                return Ok((
                    EarlySend {
                        send,
                        replay: Some((self.clone(), reopened.clone())),
                        finished: false,
                    },
                    EarlyRecv {
                        recv,
                        replay: Some((self.clone(), reopened)),
                    },
                ));
            }
        };
        if !refused {
            self.settle(Phase::Accepted);
            return Ok(plain(send, recv));
        }
        if let Some((send, recv)) = self.reopen(Some((send, recv))).await {
            return Ok(plain(send, recv));
        }
        let (send, recv) = self.connection.open_bi().await?;
        Ok(plain(send, recv))
    }

    /// Opens again every stream opened in refused early data, in the order
    /// they first were, starting with `first` where the caller already
    /// holds the stream that comes first, and writes on each what was
    /// written early. Hands `first` back if no stream still wants one.
    async fn reopen(
        &self,
        mut first: Option<(quinn::SendStream, quinn::RecvStream)>,
    ) -> Option<(quinn::SendStream, quinn::RecvStream)> {
        let live: Vec<Arc<Reopened>> = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            std::mem::take(&mut state.opened)
        }
        .iter()
        .filter_map(Weak::upgrade)
        .collect();
        let mut streams = Vec::with_capacity(live.len());
        for _ in &live {
            streams.push(match first.take() {
                Some(stream) => stream,
                None => match self.connection.open_bi().await {
                    Ok(stream) => stream,
                    Err(_) => {
                        self.settle(Phase::Failed);
                        return None;
                    }
                },
            });
        }
        // Written again side by side, so a stream the far end is slow to
        // read holds up no other.
        let replays = live
            .iter()
            .zip(streams)
            .map(|(reopened, (mut send, recv))| async move {
                let (sent, finished) = reopened
                    .sent
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                let _ = send.write_all(&sent).await;
                if finished {
                    let _ = send.finish();
                }
                *reopened.send.lock().unwrap_or_else(|p| p.into_inner()) = Some(send);
                *reopened.recv.lock().unwrap_or_else(|p| p.into_inner()) = Some(recv);
            });
        futures_util::future::join_all(replays).await;
        self.settle(Phase::Rejected);
        first
    }

    fn phase(&self) -> Phase {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).phase
    }

    /// The phase, arranging for `cx` to be woken when it changes from
    /// pending.
    fn poll_phase(&self, cx: &Context<'_>) -> Phase {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.phase == Phase::Pending
            && !state
                .waiting
                .iter()
                .any(|waker| waker.will_wake(cx.waker()))
        {
            state.waiting.push(cx.waker().clone());
        }
        state.phase
    }

    fn settle(&self, phase: Phase) {
        let waiting = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.phase = phase;
            state.opened.clear();
            self.settled.send_replace(phase);
            std::mem::take(&mut state.waiting)
        };
        for waker in waiting {
            waker.wake();
        }
    }
}

fn plain(send: quinn::SendStream, recv: quinn::RecvStream) -> (EarlySend, EarlyRecv) {
    (EarlySend::plain(send), EarlyRecv { recv, replay: None })
}

/// The sending half of a link stream: what it writes in early data is
/// kept until the far end has taken it, and if the far end refused it the
/// stream continues on the one opened in its place, which was given it all
/// again.
pub(crate) struct EarlySend {
    send: quinn::SendStream,
    replay: Option<(Arc<Early>, Arc<Reopened>)>,
    finished: bool,
}

impl EarlySend {
    pub(crate) fn plain(send: quinn::SendStream) -> Self {
        Self {
            send,
            replay: None,
            finished: false,
        }
    }

    /// Whether the stream no longer depends on early data, moving onto
    /// the stream opened in its place where the far end refused it. While
    /// the outcome is unknown, `cx` is woken once it is.
    fn settled(&mut self, cx: Option<&Context<'_>>) -> io::Result<bool> {
        let Some((early, reopened)) = &self.replay else {
            return Ok(true);
        };
        let phase = match cx {
            Some(cx) => early.poll_phase(cx),
            None => early.phase(),
        };
        match phase {
            Phase::Pending => return Ok(false),
            Phase::Accepted | Phase::Failed => {}
            Phase::Rejected => {
                let reopened = reopened
                    .send
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .take();
                let Some(reopened) = reopened else {
                    self.replay = None;
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "the stream was not opened again after its early data was refused",
                    ));
                };
                self.send = reopened;
                if self.finished {
                    let _ = self.send.finish();
                }
            }
        }
        self.replay = None;
        Ok(true)
    }

    fn poll_settled(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.settled(Some(cx)) {
            Ok(true) => Poll::Ready(Ok(())),
            Ok(false) => Poll::Pending,
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    /// Waits until the far end has everything written on the stream, or
    /// has stopped it.
    pub(crate) async fn stopped(&mut self) {
        if std::future::poll_fn(|cx| self.poll_settled(cx))
            .await
            .is_ok()
        {
            let _ = self.send.stopped().await;
        }
    }

    /// Finishes the stream: what was written is all it carries.
    pub(crate) fn finish(&mut self) {
        self.finished = true;
        if let Ok(false) = self.settled(None)
            && let Some((_, reopened)) = &self.replay
        {
            // Recorded with the write it follows, so a stream opened again
            // is finished too.
            let mut sent = reopened.sent.lock().unwrap_or_else(|p| p.into_inner());
            if self.send.finish().is_ok() {
                sent.1 = true;
            }
            return;
        }
        let _ = self.send.finish();
    }

    pub(crate) fn reset(&mut self, code: quinn::VarInt) {
        self.replay = None;
        let _ = self.send.reset(code);
    }
}

impl AsyncWrite for EarlySend {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        ready!(this.poll_settled_or_early(cx))?;
        match &this.replay {
            Some((_, reopened)) => {
                // The record is held across the write, so a refusal taken
                // between the two cannot miss what the stream wrote.
                let mut sent = reopened.sent.lock().unwrap_or_else(|p| p.into_inner());
                match AsyncWrite::poll_write(Pin::new(&mut this.send), cx, buf) {
                    Poll::Ready(Ok(written)) => {
                        sent.0.extend_from_slice(&buf[..written]);
                        Poll::Ready(Ok(written))
                    }
                    // Refused under the write, or failed: the outcome wakes
                    // this once it is known.
                    Poll::Ready(Err(_)) | Poll::Pending => Poll::Pending,
                }
            }
            None => AsyncWrite::poll_write(Pin::new(&mut this.send), cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        ready!(self.poll_settled_or_early(cx))?;
        Pin::new(&mut self.send).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        ready!(self.poll_settled_or_early(cx))?;
        if self.replay.is_some() {
            self.finish();
            return Poll::Ready(Ok(()));
        }
        self.finished = true;
        Pin::new(&mut self.send).poll_shutdown(cx)
    }
}

impl EarlySend {
    /// Ready while the stream may write: in early data while the outcome
    /// is unknown, after settling once it is known.
    fn poll_settled_or_early(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.settled(Some(cx)) {
            Ok(_) => Poll::Ready(Ok(())),
            Err(error) => Poll::Ready(Err(error)),
        }
    }
}

/// The receiving half of a link stream. Nothing arrives before the
/// handshake finishes, so reads wait for the early data's outcome and, if
/// it was refused, read the stream opened in its place.
pub(crate) struct EarlyRecv {
    recv: quinn::RecvStream,
    replay: Option<(Arc<Early>, Arc<Reopened>)>,
}

impl EarlyRecv {
    pub(crate) fn plain(recv: quinn::RecvStream) -> Self {
        Self { recv, replay: None }
    }

    /// Whether the far end opened the stream in early data.
    pub(crate) fn is_0rtt(&self) -> bool {
        self.recv.is_0rtt()
    }

    pub(crate) fn stop(&mut self, code: quinn::VarInt) {
        self.replay = None;
        let _ = self.recv.stop(code);
    }
}

impl AsyncRead for EarlyRecv {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if let Some((early, reopened)) = &self.replay {
            match early.poll_phase(cx) {
                Phase::Pending => return Poll::Pending,
                Phase::Rejected => {
                    let reopened = reopened
                        .recv
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .take();
                    if let Some(reopened) = reopened {
                        self.recv = reopened;
                    }
                }
                Phase::Accepted | Phase::Failed => {}
            }
            self.replay = None;
        }
        AsyncRead::poll_read(Pin::new(&mut self.recv), cx, buf)
    }
}
