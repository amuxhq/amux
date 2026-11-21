//! Each agent's kept draft: what a person leaves in a chat's composer, kept
//! with this machine's daemon so it outlives the chat and the client.
//!
//! Saves to one agent go in order and the newest wins: one overtaken before
//! it is sent is never sent. A chat reads the agent's draft only after every
//! save to it sent before, so a chat that reopens at once finds what was left
//! in it, an emptied one empty.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

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
}

impl Saves {
    /// Hands the daemon the newest pending draft, if any; called holding
    /// the turn. A save that fails loses only what the client still shows.
    async fn flush(&self, client: &Client, agent_id: &[u8]) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(draft) = pending {
            let _ = client.drafts().update(agent_id, draft).await;
        }
    }

    /// Saves `draft`, after every save asked for before.
    fn save(self: Arc<Self>, client: Client, agent: &AgentKey, draft: wire::Draft) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(draft);
        let agent_id = agent.agent.clone();
        tokio::spawn(async move {
            let _turn = self.turn.lock().await;
            self.flush(&client, &agent_id).await;
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

    pub(crate) fn keep(&self, client: Client, agent: &AgentKey, draft: wire::Draft) {
        self.saves(agent).save(client, agent, draft);
    }

    pub(crate) async fn kept(&self, client: &Client, agent: &AgentKey) -> Option<wire::Draft> {
        let saves = self.saves(agent);
        let _turn = saves.turn.lock().await;
        saves.flush(client, &agent.agent).await;
        let draft = client.drafts().get(&agent.agent).await.unwrap_or_default();
        Some(draft).filter(|draft| !draft.text.is_empty() || !draft.attachments.is_empty())
    }

    pub(crate) async fn flushed(&self, client: &Client) {
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
