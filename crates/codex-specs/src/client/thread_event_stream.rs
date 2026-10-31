use std::fmt;
use std::sync::Arc;

use crate::client::dispatch::{ThreadChannelState, ThreadEventReceiver};
use crate::client::error::Error;
use crate::client::event::ThreadEvent;
use crate::client::thread::{ThreadInner, restore_event_receiver};

/// Continuous receiver for every event routed to one Codex thread.
#[must_use = "thread events must be consumed to avoid bounded-queue overflow"]
pub struct ThreadEventStream {
    rx: Option<ThreadEventReceiver>,
    thread_inner: Arc<ThreadInner>,
    done: bool,
}

impl fmt::Debug for ThreadEventStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadEventStream")
            .field("thread_id", &self.thread_inner.thread_id)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl ThreadEventStream {
    pub(crate) fn new(rx: ThreadEventReceiver, thread_inner: Arc<ThreadInner>) -> Self {
        Self {
            rx: Some(rx),
            thread_inner,
            done: false,
        }
    }

    /// Receive the next raw+typed thread event.
    ///
    /// Buffered events are returned before an overflow error. The stream then
    /// ends; callers recover by resuming the thread, which creates a fresh
    /// registration and atomically replays history into it.
    pub async fn next(&mut self) -> Result<Option<ThreadEvent>, Error> {
        if self.done {
            return Ok(None);
        }
        let registration = self.thread_inner.registration.clone();
        let Some(rx) = self.rx.as_mut() else {
            return Ok(None);
        };
        loop {
            match rx.try_recv() {
                Ok(event) => return Ok(Some(event)),
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    self.done = true;
                    return Ok(None);
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
            }

            let state_changed = registration.state_changed().notified();
            tokio::pin!(state_changed);
            match registration.state() {
                ThreadChannelState::Overflow => {
                    self.done = true;
                    return Err(Error::ThreadQueueOverflow(
                        self.thread_inner.thread_id.clone(),
                    ));
                }
                ThreadChannelState::Closed => {
                    self.done = true;
                    return Ok(None);
                }
                ThreadChannelState::Open => {}
            }

            tokio::select! {
                biased;
                _ = &mut state_changed => continue,
                event = rx.recv() => match event {
                    Some(event) => return Ok(Some(event)),
                    None => {
                        self.done = true;
                        return Ok(None);
                    }
                }
            }
        }
    }
}

impl Drop for ThreadEventStream {
    fn drop(&mut self) {
        if let Some(rx) = self.rx.take() {
            restore_event_receiver(self.thread_inner.clone(), rx);
        }
    }
}
