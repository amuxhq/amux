//! The unsent drafts of this machine's clients, which the daemon core keeps
//! (`daemon_core::drafts`): the file is in the profile's directory, read
//! here at start for the core.

use std::path::Path;

/// The drafts file, in the profile's directory.
pub(crate) const DRAFTS_FILE: &str = "drafts";

/// The drafts the profile's file holds; a missing or unreadable file holds
/// none, since losing a draft must never stop the profile.
pub(crate) fn load(profile_dir: &Path) -> daemon_core::drafts::DraftList {
    match std::fs::read(profile_dir.join(DRAFTS_FILE)) {
        Ok(bytes) => daemon_core::drafts::decode(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            tracing::warn!(%error, "cannot read the drafts; starting empty");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use daemon_core::payload::{ClientRequest, Response};
    use daemon_core::{CallId, Daemon, Input, Job, Output, StartOptions, StepBudget};

    use super::*;

    fn core() -> Daemon {
        let store = store::Sqlite::open_in_memory(vec![1; 16]).unwrap();
        let options = StartOptions {
            budget: StepBudget::UNLIMITED,
            ..StartOptions::default()
        };
        Daemon::new(store, options)
    }

    /// Saves `draft` on a core started from the profile's file, and writes
    /// the file the core asks for.
    fn save(profile: &Path, draft: wire::Draft) {
        let mut core = core();
        let mut out = Vec::new();
        core.step(
            Input::Drafts {
                drafts: load(profile),
            },
            &mut out,
        );
        let request = ClientRequest::UpdateDraft {
            agent_id: b"agent".to_vec(),
            draft,
        };
        core.step(
            Input::ClientCall {
                call: CallId(1),
                request,
            },
            &mut out,
        );
        for output in out {
            if let Output::Job {
                op: Job::Fsync { bytes, .. },
                ..
            } = output
            {
                std::fs::write(profile.join(DRAFTS_FILE), bytes).unwrap();
            }
        }
    }

    fn get(profile: &Path) -> wire::Draft {
        let mut core = core();
        let mut out = Vec::new();
        core.step(
            Input::Drafts {
                drafts: load(profile),
            },
            &mut out,
        );
        let request = ClientRequest::GetDraft {
            agent_id: b"agent".to_vec(),
        };
        core.step(
            Input::ClientCall {
                call: CallId(2),
                request,
            },
            &mut out,
        );
        out.into_iter()
            .find_map(|output| match output {
                Output::Answer {
                    response: Response::Draft(draft),
                    ..
                } => Some(draft),
                _ => None,
            })
            .expect("the core answers with a draft")
    }

    /// A draft saved is read back by the next start, and an empty one is
    /// forgotten.
    #[test]
    fn a_draft_outlives_a_restart_and_an_empty_one_is_forgotten() {
        let temp = tempfile::tempdir().unwrap();
        let draft = wire::Draft {
            text: "half typed".into(),
            attachments: Vec::new(),
        };
        save(temp.path(), draft.clone());
        assert_eq!(get(temp.path()), draft);
        save(temp.path(), wire::Draft::default());
        assert_eq!(get(temp.path()), wire::Draft::default());
    }
}
