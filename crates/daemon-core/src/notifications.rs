//! The notifications outbox: what an agent wants of the person, pushed to
//! their devices.
//!
//! A row is written in the transaction that raises a snapshot's notice, due
//! a delay away, and replaced or deleted unsent if the notice moves on first,
//! at the agent's exit or with the agent; a process that dies raises its own.
//! The core reads the rows afresh whenever an envelope changes, when the
//! start's sweep is done and when the next row falls due: each due row not
//! already on its way becomes a [`Job::SendPush`] to the paired hosts that
//! want notifications, and the row is deleted once the job says the push was
//! handed over. With nobody to tell, it is deleted then and there. A push
//! that failed is tried again after
//! [`Settings::push_retry_ms`](crate::Settings::push_retry_ms). Only a row
//! is ever sent, so a push cancelled before it fell due is never sent.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use store::Notification;

use crate::Daemon;
use crate::ids::{JobId, TimerId};
use crate::io::{Job, Output};
use crate::payload::{JobResult, Push, PushRecipient};

/// The pushes handed to the driver and not yet done.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Notifications {
    /// The row each push job sends, by agent id and revision.
    sending: BTreeMap<JobId, (Vec<u8>, u64)>,
    /// When the armed timer fires, if one is armed.
    armed_at: Option<u64>,
}

impl Notifications {
    pub(crate) fn jobs(&self, out: &mut Vec<JobId>) {
        out.extend(self.sending.keys().copied());
    }

    pub(crate) fn canonical(&self, ids: &crate::fingerprint::Ids) -> Notifications {
        Notifications {
            sending: self
                .sending
                .iter()
                .map(|(job, row)| (ids.jobs.get(*job), row.clone()))
                .collect(),
            armed_at: self.armed_at,
        }
    }

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
            let recipients = self.push_recipients();
            if recipients.is_empty() {
                self.forget_notification(row.agent_id, row.revision);
                continue;
            }
            let job = self.pending.mint();
            self.notifications
                .sending
                .insert(job, (row.agent_id.clone(), row.revision));
            out.push(Output::Job {
                id: job,
                op: Job::SendPush(self.push_for(row, recipients)),
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
            JobResult::Done => self.forget_notification(agent_id, revision),
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

    fn forget_notification(&mut self, agent_id: Vec<u8>, revision: u64) {
        let row = Notification {
            agent_id,
            revision,
            due_at_ms: 0,
            name: String::new(),
            notice: Vec::new(),
        };
        if let Err(error) = self.store.remove_notification(&row) {
            tracing::warn!(%error, "deleting a sent notification failed");
        }
    }

    /// The hosts this one trusts that said they want notifications. Trust is
    /// not transitive, and nor is this: a host paired only with a neighbor
    /// has no entry here, however this host came to hear of it.
    pub(crate) fn push_recipients(&self) -> Vec<PushRecipient> {
        self.trust
            .store()
            .entries()
            .filter_map(|(host, entry)| {
                entry.push_key.as_ref().map(|push_key| PushRecipient {
                    host_id: host.as_bytes().to_vec(),
                    push_key: push_key.clone(),
                })
            })
            .collect()
    }

    fn push_for(&self, row: Notification, recipients: Vec<PushRecipient>) -> Push {
        Push {
            host_id: self.store.own_host().to_vec(),
            agent_id: row.agent_id,
            revision: row.revision,
            name: row.name,
            notice: row.notice,
            recipients,
        }
    }
}

#[cfg(test)]
mod tests {
    use store::Sqlite;

    use super::*;
    use crate::{StartOptions, StepBudget};

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
