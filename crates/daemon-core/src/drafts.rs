//! The unsent drafts of this machine's clients, one per agent, kept so a
//! draft outlives the client that was typing it (a terminal client closing
//! or detaching). Drafts are this machine's and never reach other hosts or
//! other machines.
//!
//! The store's `drafts` table holds them; the core reads and writes it in
//! the step that answers for them, and forgets an agent's draft when the
//! agent goes.

use prost::Message as _;

use crate::Daemon;
use crate::ids::CallId;
use crate::io::Output;
use crate::payload::{Response, Status};
use crate::serve::answered;

impl Daemon {
    /// The agent's draft, empty if none is kept.
    pub(crate) fn get_draft(&self, call: CallId, agent_id: &[u8], out: &mut Vec<Output>) {
        let draft = self
            .store
            .draft(agent_id)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "cannot read a draft");
                None
            })
            .and_then(|bytes| wire::Draft::decode(bytes.as_slice()).ok())
            .unwrap_or_default();
        answered(call, Response::Draft(draft), out);
    }

    /// Keeps `draft` for the agent; an empty one forgets it. Answered at
    /// once: a save the store refuses is logged, and the client still holds
    /// the words it showed.
    pub(crate) fn update_draft(
        &mut self,
        call: CallId,
        agent_id: Vec<u8>,
        draft: &wire::Draft,
        out: &mut Vec<Output>,
    ) {
        if draft.text.trim().is_empty() && draft.attachments.is_empty() {
            self.forget_draft(&agent_id);
        } else if let Err(error) = self.store.put_draft(&agent_id, &draft.encode_to_vec()) {
            tracing::warn!(%error, "cannot save a draft");
        }
        out.push(Output::EndCall {
            call,
            status: Status::Ok,
        });
    }

    /// Forgets the agent's draft: the agent is gone, or the draft is empty.
    pub(crate) fn forget_draft(&mut self, agent_id: &[u8]) {
        if let Err(error) = self.store.delete_draft(agent_id) {
            tracing::warn!(%error, "cannot forget a draft");
        }
    }
}

#[cfg(test)]
mod tests {
    use store::Sqlite;

    use super::*;
    use crate::payload::ClientRequest;
    use crate::{Input, StartOptions, StepBudget};

    fn daemon() -> Daemon {
        let store = Sqlite::open_in_memory(vec![1; 16]).expect("a store opens");
        Daemon::new(
            store,
            StartOptions {
                budget: StepBudget::UNLIMITED,
                ..StartOptions::default()
            },
        )
    }

    fn call(daemon: &mut Daemon, request: ClientRequest) -> Vec<Output> {
        let mut out = Vec::new();
        daemon.step(
            Input::ClientCall {
                call: CallId(1),
                request,
            },
            &mut out,
        );
        out
    }

    fn save(daemon: &mut Daemon, text: &str) {
        call(
            daemon,
            ClientRequest::UpdateDraft {
                agent_id: vec![2; 16],
                draft: wire::Draft {
                    text: text.into(),
                    ..Default::default()
                },
            },
        );
    }

    fn kept(daemon: &mut Daemon) -> String {
        let out = call(
            daemon,
            ClientRequest::GetDraft {
                agent_id: vec![2; 16],
            },
        );
        match out.as_slice() {
            [
                Output::Answer {
                    response: Response::Draft(draft),
                    ..
                },
                Output::EndCall { .. },
            ] => draft.text.clone(),
            other => panic!("one draft answer: {other:?}"),
        }
    }

    #[test]
    fn the_last_save_is_kept_and_an_empty_one_forgets() {
        let mut daemon = daemon();
        save(&mut daemon, "half a thought");
        save(&mut daemon, "a whole thought");
        assert_eq!(kept(&mut daemon), "a whole thought");
        assert!(daemon.store().draft(&[2; 16]).unwrap().is_some());
        save(&mut daemon, "  ");
        assert_eq!(kept(&mut daemon), "");
        assert_eq!(daemon.store().draft(&[2; 16]).unwrap(), None);
    }
}
