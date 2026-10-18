//! The notifications outbox: "needs you", pushed to the person's devices.
//!
//! A row is written in the transaction that turns an agent's phase to
//! needs_you, due a delay away, and deleted unsent if the phase leaves
//! needs_you first, at the agent's exit or with the agent. The core reads
//! the rows afresh whenever an envelope changes, when the start's sweep is
//! done and when the next row falls due: each due row not already on its
//! way becomes a [`Job::SendPush`], and the row is deleted once the job
//! says the push was handed over. A push that failed is tried again after
//! [`Settings::push_retry_ms`](crate::Settings::push_retry_ms). Only a row
//! is ever sent, so a push cancelled before it fell due is never sent.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use store::Notification;

use crate::Daemon;
use crate::ids::{JobId, TimerId};
use crate::io::{Job, Output};
use crate::payload::{JobResult, Push};

/// The pushes handed to the driver and not yet done.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Notifications {
    /// The row each push job sends, by agent id and revision.
    sending: BTreeMap<JobId, (Vec<u8>, u64)>,
    /// When the armed timer fires, if one is armed.
    armed_at: Option<u64>,
}

impl Notifications {
    fn is_sending(&self, row: &Notification) -> bool {
        self.sending
            .values()
            .any(|(agent, revision)| *agent == row.agent_id && *revision == row.revision)
    }
}

impl Daemon {
    /// Sends every row that is due and arms the timer for the next one.
    pub(crate) fn drain_notifications(&mut self, out: &mut Vec<Output>) {
        let now = self.timers.now_ms;
        let rows = match self.store.notifications() {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "reading the notifications outbox failed");
                return;
            }
        };
        let mut next: Option<u64> = None;
        for row in rows {
            let due = u64::try_from(row.due_at_ms).unwrap_or(0);
            if due > now {
                next = Some(next.map_or(due, |next| next.min(due)));
                continue;
            }
            if self.notifications.is_sending(&row) {
                continue;
            }
            let job = self.pending.mint();
            self.notifications
                .sending
                .insert(job, (row.agent_id.clone(), row.revision));
            out.push(Output::Job {
                id: job,
                op: Job::SendPush(self.push_for(row)),
            });
        }
        self.arm_notifications(next, out);
    }

    fn arm_notifications(&mut self, at: Option<u64>, out: &mut Vec<Output>) {
        if self.notifications.armed_at == at {
            return;
        }
        match at {
            Some(at_ms) => out.push(Output::ArmTimer {
                id: TimerId::Notifications,
                at_ms,
            }),
            None => out.push(Output::CancelTimer {
                id: TimerId::Notifications,
            }),
        }
        self.notifications.armed_at = at;
    }

    /// The notifications timer fired.
    pub(crate) fn notifications_due(&mut self, out: &mut Vec<Output>) {
        self.notifications.armed_at = None;
        self.drain_notifications(out);
    }

    /// A push job finished; returns whether `job` was one. A push handed
    /// over deletes its row; one that failed is tried again later.
    pub(crate) fn push_done(
        &mut self,
        job: JobId,
        result: &JobResult,
        out: &mut Vec<Output>,
    ) -> bool {
        let Some((agent_id, revision)) = self.notifications.sending.remove(&job) else {
            return false;
        };
        match result {
            JobResult::Done => {
                let row = Notification {
                    agent_id,
                    revision,
                    due_at_ms: 0,
                    name: None,
                    working_on: None,
                    text: String::new(),
                };
                if let Err(error) = self.store.remove_notification(&row) {
                    tracing::warn!(%error, "deleting a sent notification failed");
                }
            }
            failed => {
                tracing::warn!(?failed, "a push failed; it is tried again");
                let retry = self.timers.now_ms + self.settings.push_retry_ms;
                let at = self
                    .notifications
                    .armed_at
                    .map_or(retry, |at| at.min(retry));
                self.arm_notifications(Some(at), out);
            }
        }
        true
    }

    fn push_for(&self, row: Notification) -> Push {
        Push {
            host_id: self.store.own_host().to_vec(),
            agent_id: row.agent_id,
            revision: row.revision,
            name: row.name,
            working_on: row.working_on,
            text: row.text,
        }
    }
}

#[cfg(test)]
mod tests {
    use store::Sqlite;

    use super::*;
    use crate::{Input, StartOptions, StepBudget};

    fn daemon() -> Daemon {
        let store = Sqlite::open_in_memory(b"host-a".to_vec()).expect("a store opens");
        Daemon::new(
            store,
            StartOptions {
                budget: StepBudget::UNLIMITED,
                ..StartOptions::default()
            },
        )
    }

    fn pushes(out: &[Output]) -> Vec<(JobId, u64)> {
        out.iter()
            .filter_map(|output| match output {
                Output::Job {
                    id,
                    op: Job::SendPush(push),
                } => Some((*id, push.revision)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn nothing_is_pushed_from_an_empty_outbox_and_no_timer_is_armed() {
        let mut daemon = daemon();
        let mut out = Vec::new();
        daemon.step(Input::Swept, &mut out);
        assert!(pushes(&out).is_empty());
        assert!(!out.iter().any(|output| matches!(
            output,
            Output::ArmTimer {
                id: TimerId::Notifications,
                ..
            }
        )));
    }

    #[test]
    fn a_failed_push_arms_its_retry_and_an_unknown_job_is_not_a_push() {
        let mut daemon = daemon();
        daemon
            .notifications
            .sending
            .insert(JobId(9), (b"agent".to_vec(), 2));
        let mut out = Vec::new();
        assert!(!daemon.push_done(JobId(8), &JobResult::Done, &mut out));
        assert!(daemon.push_done(JobId(9), &JobResult::Failed("down".into()), &mut out));
        assert_eq!(
            out,
            vec![Output::ArmTimer {
                id: TimerId::Notifications,
                at_ms: daemon.settings.push_retry_ms,
            }]
        );
        assert!(daemon.notifications.sending.is_empty());
    }
}
