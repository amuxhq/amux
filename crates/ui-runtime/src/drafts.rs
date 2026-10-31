//! Each agent's kept draft: what a person leaves in a chat's composer, kept
//! with this machine's daemon so it outlives the chat and the client.
//!
//! Saves to one agent go in order and the newest wins: one overtaken before
//! it is sent is never sent. A chat reads the agent's draft only after every
//! save to it sent before, so a chat that reopens at once finds what was left
//! in it, an emptied one empty.
//!
//! The words of each prompt the agent's session holds for a host that cannot
//! be reached are kept too, oldest first and ahead of the composer's: a held prompt
//! lives only as long as the client, and a client killed before the host is
//! back would otherwise lose it. They leave the kept draft as the prompt goes
//! or is taken back into the composer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use client::Client;
use ui_state::AgentKey;

#[derive(Default)]
pub(crate) struct Drafts {
    agents: Mutex<HashMap<AgentKey, Arc<Saves>>>,
}

#[derive(Default)]
struct Saves {
    /// The newest draft not yet handed to the daemon.
    pending: Mutex<Option<wire::Draft>>,
    /// Held while a save or a read is with the daemon, so they go one at a
    /// time and in the order they were asked for.
    turn: tokio::sync::Mutex<()>,
    words: Mutex<Words>,
}

/// What an agent's kept draft is made of.
#[derive(Default)]
struct Words {
    /// The composer's, as the client last kept or read it.
    typed: wire::Draft,
    /// The prompts the agent's session holds for an unreachable host,
    /// oldest first.
    held: Vec<wire::Draft>,
}

impl Words {
    /// The held prompts' words ahead of the composer's, each on its own
    /// line, and their attachments in the same order.
    fn kept(&self) -> wire::Draft {
        let mut kept = wire::Draft::default();
        for draft in self.held.iter().chain([&self.typed]) {
            if !draft.text.is_empty() {
                if !kept.text.is_empty() {
                    kept.text.push('\n');
                }
                kept.text.push_str(&draft.text);
            }
            kept.attachments.extend(draft.attachments.iter().cloned());
        }
        kept
    }
}

impl Saves {
    fn words(&self) -> MutexGuard<'_, Words> {
        self.words.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hands the daemon the newest pending draft, if any; called holding
    /// the turn. A save that fails loses only what the client still shows.
    async fn flush(&self, client: &dyn Client, agent_id: &[u8]) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(draft) = pending {
            let request = wire::UpdateDraftRequest {
                agent_id: agent_id.to_vec(),
                draft: Some(draft),
            };
            let _ = client.update_draft(request).await;
        }
    }

    /// Saves what the words make now, after every save asked for before.
    fn save(self: Arc<Self>, client: Arc<dyn Client>, agent: &AgentKey) {
        let kept = self.words().kept();
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(kept);
        let agent_id = agent.agent.clone();
        tokio::spawn(async move {
            let _turn = self.turn.lock().await;
            self.flush(&*client, &agent_id).await;
        });
    }
}

impl Drafts {
    fn saves(&self, agent: &AgentKey) -> Arc<Saves> {
        self.agents
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(agent.clone())
            .or_default()
            .clone()
    }

    pub(crate) fn keep(&self, client: Arc<dyn Client>, agent: &AgentKey, draft: wire::Draft) {
        let saves = self.saves(agent);
        saves.words().typed = draft;
        saves.save(client, agent);
    }

    /// The prompts the agent's session holds now, each time they change.
    pub(crate) fn held(&self, client: Arc<dyn Client>, agent: &AgentKey, held: Vec<wire::Draft>) {
        let saves = self.saves(agent);
        saves.words().held = held;
        saves.save(client, agent);
    }

    /// The agent's session is gone, and with it what it held: those words
    /// stay in the kept draft as the person's own.
    pub(crate) fn session_gone(&self, agent: &AgentKey) {
        let saves = self
            .agents
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(agent)
            .cloned();
        if let Some(saves) = saves {
            let mut words = saves.words();
            if !words.held.is_empty() {
                words.typed = words.kept();
                words.held.clear();
            }
        }
    }

    pub(crate) async fn kept(&self, client: &dyn Client, agent: &AgentKey) -> Option<wire::Draft> {
        let saves = self.saves(agent);
        let _turn = saves.turn.lock().await;
        saves.flush(client, &agent.agent).await;
        // The daemon's copy carries the held prompts' words too, and the
        // chat shows those prompts held: its composer opens with its own.
        let typed = {
            let words = saves.words();
            (!words.held.is_empty()).then(|| words.typed.clone())
        };
        let draft = match typed {
            Some(typed) => typed,
            None => {
                let request = wire::GetDraftRequest {
                    agent_id: agent.agent.clone(),
                };
                let draft = client.get_draft(request).await.unwrap_or_default();
                // What the composer opens with is the client's own from now.
                saves.words().typed = draft.clone();
                draft
            }
        };
        Some(draft).filter(|draft| !draft.text.is_empty() || !draft.attachments.is_empty())
    }

    pub(crate) async fn flushed(&self, client: &dyn Client) {
        let agents: Vec<(AgentKey, Arc<Saves>)> = self
            .agents
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(agent, saves)| (agent.clone(), saves.clone()))
            .collect();
        for (agent, saves) in agents {
            let _turn = saves.turn.lock().await;
            saves.flush(client, &agent.agent).await;
        }
    }
}
