//! The sending side of a core's input channel.
//!
//! A core with a thread of its own blocks on the channel and wakes when an
//! input arrives. A core its caller steps has nobody blocked on it, so
//! every send rings the caller's bell as well, after the input is in.

use std::sync::Arc;

use tokio::sync::mpsc::error::{SendError, TrySendError};
use tokio::sync::mpsc::{self};

use super::Fed;

/// Rung whenever an input waits for a core its caller steps.
pub(crate) type Bell = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub(crate) struct Feed {
    inputs: mpsc::Sender<Fed>,
    bell: Option<Bell>,
}

#[derive(Clone)]
pub(crate) struct WeakFeed {
    inputs: mpsc::WeakSender<Fed>,
    bell: Option<Bell>,
}

// The errors are tokio's own, which hand the input back to whoever sent it.
#[expect(clippy::result_large_err)]
impl Feed {
    pub(crate) fn new(inputs: mpsc::Sender<Fed>, bell: Option<Bell>) -> Self {
        Self { inputs, bell }
    }

    pub(crate) async fn send(&self, fed: Fed) -> Result<(), SendError<Fed>> {
        let sent = self.inputs.send(fed).await;
        self.ring();
        sent
    }

    pub(crate) fn try_send(&self, fed: Fed) -> Result<(), TrySendError<Fed>> {
        let sent = self.inputs.try_send(fed);
        self.ring();
        sent
    }

    pub(crate) fn downgrade(&self) -> WeakFeed {
        WeakFeed {
            inputs: self.inputs.downgrade(),
            bell: self.bell.clone(),
        }
    }

    fn ring(&self) {
        if let Some(bell) = &self.bell {
            bell();
        }
    }
}

impl WeakFeed {
    pub(crate) fn upgrade(&self) -> Option<Feed> {
        Some(Feed {
            inputs: self.inputs.upgrade()?,
            bell: self.bell.clone(),
        })
    }
}
