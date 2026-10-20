//! The call table: every call the core is answering, by the id the core
//! knows it by, with one queue per call for its answers.
//!
//! The core cannot hold a response stream, so the driver holds it and names
//! it with a [`CallId`]. The core's `Answer` and `EndCall` outputs land in
//! the call's queue, and the caller reads the queue. A call's own answers
//! (its opening, the answer to its lag) are always taken; the live answers
//! behind them are held to the queue's room: a stream that falls further
//! behind than that is reported to the core as lagged, and a caller that
//! drops its stream is reported as cancelled, so the core releases what it
//! held for the call.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use daemon_core::payload::{Response, Status};
use daemon_core::{CallId, Input};
use tokio::sync::{Notify, mpsc};

use super::Fed;

/// Every open call's queue.
#[derive(Default)]
pub(crate) struct CallTable {
    /// The last id named. Calls that arrive are the driver's to name, since
    /// it holds the stream before the core hears of it.
    last: AtomicU64,
    open: Mutex<HashMap<CallId, Arc<AnswerQueue>>>,
}

impl CallTable {
    /// Names a call and opens its queue, with room for `room` live answers
    /// the reader has not yet taken.
    pub(crate) fn open(&self, room: usize) -> Arc<AnswerQueue> {
        let call = CallId(self.last.fetch_add(1, Ordering::Relaxed) + 1);
        let queue = Arc::new(AnswerQueue::new(call, room));
        self.open.lock().unwrap().insert(call, queue.clone());
        queue
    }

    pub(crate) fn get(&self, call: CallId) -> Option<Arc<AnswerQueue>> {
        self.open.lock().unwrap().get(&call).cloned()
    }

    /// One answer from the core. A call the caller already dropped takes
    /// nothing.
    pub(crate) fn answer(&self, call: CallId, response: Response) {
        self.answer_all(call, vec![response]);
    }

    /// The answers one step gave a call, queued together so a reader woken
    /// by the first finds the rest.
    pub(crate) fn answer_all(&self, call: CallId, responses: Vec<Response>) {
        if let Some(queue) = self.get(call) {
            queue.answer(responses);
        }
    }

    /// The core ended the call: its queue takes nothing more and the table
    /// forgets it.
    pub(crate) fn end(&self, call: CallId, status: Status) {
        if let Some(queue) = self.open.lock().unwrap().remove(&call) {
            queue.end(status);
        }
    }

    /// Ends every open call: the profile is gone.
    pub(crate) fn end_all(&self, status: Status) {
        let open: Vec<_> = self.open.lock().unwrap().drain().collect();
        for (_, queue) in open {
            queue.end(status.clone());
        }
    }

    fn forget(&self, call: CallId) {
        self.open.lock().unwrap().remove(&call);
    }
}

/// One call's answers, in the order the core gave them, then its end.
pub(crate) struct AnswerQueue {
    call: CallId,
    room: usize,
    state: Mutex<QueueState>,
    ready: Notify,
}

#[derive(Default)]
struct QueueState {
    /// Each answer, and whether it was carried live rather than given by
    /// the core.
    answers: VecDeque<(Response, bool)>,
    /// How many of `answers` were carried live.
    live: usize,
    /// The live answers overflowed: none is carried any more.
    lagged: bool,
    end: Option<Status>,
    /// The reader has taken the end.
    finished: bool,
}

/// What a live answer met.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Carried {
    Queued,
    /// The reader fell more than the queue's room behind: the live answers
    /// it had not taken are gone, and the core must be told.
    Overflowed,
    /// The call lagged or ended before; nothing more is carried.
    Refused,
}

/// What a reader takes from the queue.
#[derive(Debug)]
pub(crate) enum Delivered {
    Answer(Response),
    End(Status),
}

impl AnswerQueue {
    fn new(call: CallId, room: usize) -> Self {
        Self {
            call,
            room: room.max(1),
            state: Mutex::new(QueueState::default()),
            ready: Notify::new(),
        }
    }

    pub(crate) fn call(&self) -> CallId {
        self.call
    }

    /// The call's own answers from one step, queued at once. Always taken:
    /// an opening is bounded by what it reads, and the answer after a lag
    /// is what tells the reader it lagged.
    fn answer(&self, responses: Vec<Response>) {
        let mut state = self.state.lock().unwrap();
        if state.end.is_some() {
            return;
        }
        state
            .answers
            .extend(responses.into_iter().map(|response| (response, false)));
        drop(state);
        self.ready.notify_one();
    }

    /// A live answer, behind the call's own.
    pub(crate) fn carry(&self, response: Response) -> Carried {
        let mut state = self.state.lock().unwrap();
        if state.lagged || state.end.is_some() {
            return Carried::Refused;
        }
        if state.live >= self.room {
            // As a ring that moved on without its reader: the live answers
            // it had not taken are dropped, and what it missed is in the
            // store for the fresh opening it asks for next.
            state.answers.retain(|(_, live)| !live);
            state.live = 0;
            state.lagged = true;
            return Carried::Overflowed;
        }
        state.answers.push_back((response, true));
        state.live += 1;
        drop(state);
        self.ready.notify_one();
        Carried::Queued
    }

    pub(crate) fn end(&self, status: Status) {
        let mut state = self.state.lock().unwrap();
        if state.end.is_none() {
            state.end = Some(status);
        }
        drop(state);
        self.ready.notify_one();
    }

    /// The next answer, then the end, then nothing.
    async fn next(&self) -> Option<Delivered> {
        loop {
            let ready = self.ready.notified();
            {
                let mut state = self.state.lock().unwrap();
                if let Some((response, live)) = state.answers.pop_front() {
                    if live {
                        state.live -= 1;
                    }
                    return Some(Delivered::Answer(response));
                }
                if state.finished {
                    return None;
                }
                if let Some(status) = state.end.take() {
                    state.finished = true;
                    return Some(Delivered::End(status));
                }
            }
            ready.await;
        }
    }

    fn finished(&self) -> bool {
        self.state.lock().unwrap().finished
    }
}

/// The caller's end of a call. Dropping it before the call ends cancels it.
pub(crate) struct Answers {
    queue: Arc<AnswerQueue>,
    calls: Arc<CallTable>,
    inputs: super::Feed,
    /// What [`Answers::opened`] took to see whether the call opened.
    first: Option<Delivered>,
}

impl Answers {
    pub(crate) fn new(queue: Arc<AnswerQueue>, calls: Arc<CallTable>, inputs: super::Feed) -> Self {
        Self {
            queue,
            calls,
            inputs,
            first: None,
        }
    }

    /// Waits for the call's first answer and keeps it for [`Answers::next`]:
    /// the status the call ended with, if it ended without answering.
    pub(crate) async fn opened(&mut self) -> Result<(), Status> {
        if self.first.is_none() {
            self.first = self.queue.next().await;
        }
        match &self.first {
            Some(Delivered::Answer(_)) => Ok(()),
            Some(Delivered::End(status)) => Err(status.clone()),
            None => Err(Status::Ok),
        }
    }

    pub(crate) async fn next(&mut self) -> Option<Delivered> {
        match self.first.take() {
            Some(first) => Some(first),
            None => self.queue.next().await,
        }
    }
}

impl Drop for Answers {
    fn drop(&mut self) {
        if self.queue.finished() {
            return;
        }
        self.calls.forget(self.queue.call);
        let cancelled = Fed::Input(Input::CallCancelled {
            call: self.queue.call,
        });
        if let Err(mpsc::error::TrySendError::Full(cancelled)) = self.inputs.try_send(cancelled) {
            // The core is busy; the cancel waits its turn rather than being
            // lost, so the core still releases the call.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let inputs = self.inputs.clone();
                runtime.spawn(async move {
                    let _ = inputs.send(cancelled).await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use daemon_core::payload::CallError;

    use super::*;

    #[tokio::test]
    async fn a_call_dropped_before_its_end_is_reported_cancelled() {
        let calls = Arc::new(CallTable::default());
        let (inputs, mut fed) = mpsc::channel(4);
        let inputs = super::super::Feed::new(inputs, None);
        let queue = calls.open(2);
        let call = queue.call();
        drop(Answers::new(queue.clone(), calls.clone(), inputs.clone()));
        assert!(matches!(
            fed.try_recv(),
            Ok(Fed::Input(Input::CallCancelled { call: cancelled })) if cancelled == call
        ));
        assert!(calls.get(call).is_none());

        // A call read to its end is not cancelled when dropped.
        let queue = calls.open(2);
        let mut answers = Answers::new(queue.clone(), calls.clone(), inputs);
        calls.end(queue.call(), Status::Failed(CallError::NoAgent));
        assert_eq!(
            answers.opened().await,
            Err(Status::Failed(CallError::NoAgent))
        );
        assert!(matches!(answers.next().await, Some(Delivered::End(_))));
        assert!(answers.next().await.is_none());
        drop(answers);
        assert!(fed.try_recv().is_err());
    }
}
