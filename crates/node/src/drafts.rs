//! The unsent drafts of this machine's clients, one per agent, kept so a
//! draft outlives the client that was typing it (a terminal client closing
//! or detaching). A file in the profile's directory: drafts are this
//! machine's and never reach the store, other hosts or other machines.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use prost::Message as _;

/// The drafts file, in the profile's directory.
const DRAFTS_FILE: &str = "drafts.json";

/// Each agent's draft, by the agent id in base64, as the encoded
/// `ComposerDraft`, also in base64.
pub(crate) struct Drafts {
    file: PathBuf,
    by_agent: BTreeMap<String, String>,
}

impl Drafts {
    /// Reads the profile's drafts; an unreadable file starts empty, since
    /// losing a draft must never stop the profile.
    pub(crate) fn load(profile_dir: &Path) -> Self {
        let file = profile_dir.join(DRAFTS_FILE);
        let by_agent = match std::fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, "cannot read the drafts; starting empty");
                BTreeMap::new()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => {
                tracing::warn!(%error, "cannot read the drafts; starting empty");
                BTreeMap::new()
            }
        };
        Self { file, by_agent }
    }

    pub(crate) fn get(&self, agent_id: &[u8]) -> wire::ComposerDraft {
        self.by_agent
            .get(&STANDARD.encode(agent_id))
            .and_then(|encoded| STANDARD.decode(encoded).ok())
            .and_then(|bytes| wire::ComposerDraft::decode(bytes.as_slice()).ok())
            .unwrap_or_default()
    }

    /// Keeps `draft` for the agent; an empty one forgets it. A failed save
    /// is logged: the client still holds the words it showed.
    pub(crate) fn save(&mut self, agent_id: &[u8], draft: &wire::ComposerDraft) {
        let key = STANDARD.encode(agent_id);
        if draft.text.trim().is_empty() && draft.attachments.is_empty() {
            if self.by_agent.remove(&key).is_none() {
                return;
            }
        } else {
            self.by_agent
                .insert(key, STANDARD.encode(draft.encode_to_vec()));
        }
        let bytes = serde_json::to_vec(&self.by_agent).expect("drafts serialize");
        if let Err(error) = crate::identity::atomic_replace_private(&self.file, &bytes) {
            tracing::warn!(%error, "cannot save the drafts");
        }
    }

    /// Forgets a deleted agent's draft.
    pub(crate) fn forget(&mut self, agent_id: &[u8]) {
        self.save(agent_id, &wire::ComposerDraft::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A draft saved is read back by the next load, and an empty one is
    /// forgotten.
    #[test]
    fn a_draft_outlives_a_reload_and_an_empty_one_is_forgotten() {
        let temp = tempfile::tempdir().unwrap();
        let draft = wire::ComposerDraft {
            text: "half typed".into(),
            attachments: Vec::new(),
        };
        Drafts::load(temp.path()).save(b"agent", &draft);
        assert_eq!(Drafts::load(temp.path()).get(b"agent"), draft);
        Drafts::load(temp.path()).save(b"agent", &wire::ComposerDraft::default());
        assert_eq!(
            Drafts::load(temp.path()).get(b"agent"),
            wire::ComposerDraft::default()
        );
    }
}
