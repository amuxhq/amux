//! The phone's chats and fleet over the local runtime.
//!
//! [`AppRuntime`] hosts the fleet driver, which holds one session per live
//! agent, and gathers what they change until the host's main thread runs
//! next: a host is woken once per turn with the chat or the fleet that
//! moved, and takes every changed key together. A chat borrows the fleet's
//! session for its agent; leaving the foreground closes every session. Views are asked for by row key, so a
//! host reconfigures only the cells an update changed.
//!
//! Nothing here opens a store or knows the node: the runtime is reached
//! through a [`client::Client`], in process on the phone.

mod chat;
mod coalesce;
pub mod values;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

pub use chat::Chat;
use client::{Client, RpcError};
pub use coalesce::{Batch, Coalescer, WakeFn};
use model::AgentKey;
use prost::Message as _;
use tokio::task::JoinHandle;
pub use ui_runtime::OpenError;
use ui_runtime::{Fleet, Window};
use ui_view::{FamilyHeader, FleetCard, FleetView};
use values::{AgentAct, Directories, Directory, NewAgent, ProviderSignIn};
pub use values::{FleetChanges, HostView};
use wire::{
    CreateAgentRequest, DeleteAgentRequest, DumpRequest, Kind, ListRepositoriesRequest,
    ProjectEntry, RenameAgentRequest, StopAgentRequest, StopMode, Trust,
};

/// How many rows a chat opens with unless the host says otherwise.
pub const DEFAULT_TAIL: u32 = 200;
/// The most rows a chat's window keeps while the reader follows the newest:
/// the tail it opened with. The list pages only at a top marker it can see,
/// which a following reader sees only when the whole window fits on screen,
/// far fewer rows than this; a longer tail raises the cap with it.
pub const DEFAULT_CAP: u32 = DEFAULT_TAIL;

/// What moved: the fleet, or the chat with this id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    Fleet,
    Chat(u64),
}

/// The host's wake. Called on a worker thread; it schedules the host's
/// read on its main thread and returns.
pub type HostWake = Arc<dyn Fn(Wake) + Send + Sync>;

/// The drivers of one profile's chats and fleet.
pub struct AppRuntime {
    client: Arc<dyn Client>,
    clock: Arc<dyn client::Clock>,
    local_host: Vec<u8>,
    wake: HostWake,
    fleet: Arc<Fleet>,
    fleet_changes: Arc<Coalescer<AgentKey>>,
    fleet_watcher: JoinHandle<()>,
    next_chat: AtomicU64,
}

impl AppRuntime {
    /// Opens the fleet and resolves once it has caught up with the local
    /// runtime. `local_host` is this device's host id.
    pub async fn open(
        client: Arc<dyn Client>,
        clock: Arc<dyn client::Clock>,
        local_host: Vec<u8>,
        wake: HostWake,
    ) -> Result<AppRuntime, RpcError> {
        let fleet = Arc::new(Fleet::connect(client.clone(), clock.clone()).await?);
        let fleet_wake = wake.clone();
        let fleet_changes = Arc::new(Coalescer::new(Arc::new(move || fleet_wake(Wake::Fleet))));
        let fleet_watcher =
            tokio::spawn(watch_fleet(Arc::downgrade(&fleet), fleet_changes.clone()));
        Ok(AppRuntime {
            client,
            clock,
            local_host,
            wake,
            fleet,
            fleet_changes,
            fleet_watcher,
            next_chat: AtomicU64::new(1),
        })
    }

    pub fn client(&self) -> &Arc<dyn Client> {
        &self.client
    }

    pub fn fleet(&self) -> &Fleet {
        &self.fleet
    }

    /// The phone leaving (false) or returning to (true) the foreground:
    /// every session's stream closes, or reopens with a tail.
    pub fn set_foreground(&self, foreground: bool) {
        self.fleet.set_foreground(foreground);
    }

    /// Opens a chat on a fleet agent: the fleet's session for it, widened
    /// to `tail` rows. Resolves once the snapshot and those rows are
    /// applied, so the host's first read is correct: at once for rows this
    /// device already holds, even with the agent's host away. Dropping the
    /// chat narrows the session again.
    pub async fn open_chat(&self, agent: &AgentKey, tail: u32) -> Result<Arc<Chat>, OpenError> {
        let window = Window {
            tail,
            cap: DEFAULT_CAP.max(tail),
        };
        let session = self.fleet.open(agent, window).await?;
        let id = self.next_chat.fetch_add(1, Ordering::Relaxed);
        let wake = self.wake.clone();
        let chat = Chat::start(
            id,
            agent.clone(),
            session,
            Arc::downgrade(&self.fleet),
            Arc::new(move || wake(Wake::Chat(id))),
            self.clock.clone(),
        );
        Ok(chat)
    }

    /// Home's sections: families under their roots, expanded where
    /// `expand` names the root's agent id, each row with its second line.
    pub fn fleet_view(&self, expand: &[Vec<u8>]) -> FleetView {
        let expand = expand.iter().cloned().collect();
        let now_ms = self.clock.now_ms();
        // Each session is read on its own, before the fleet is.
        let lines = self
            .fleet
            .sessions()
            .into_iter()
            .map(|session| {
                let state = session.state();
                (
                    ui_state::agent_key(state.agent()),
                    ui_view::session_line(&state, now_ms),
                )
            })
            .collect();
        ui_view::fleet_view(
            &self.fleet.state(),
            &self.local_host,
            &lines,
            &expand,
            &|_| true,
        )
    }

    pub fn fleet_card(&self, agent: &AgentKey) -> Option<FleetCard> {
        ui_view::fleet_card(&self.fleet.state(), &self.local_host, &agent.agent)
    }

    pub fn family_header(&self, agent: &AgentKey) -> Option<FamilyHeader> {
        ui_view::family_header(&self.fleet.state(), &self.local_host, &agent.agent)
    }

    pub fn hosts(&self) -> Vec<HostView> {
        let fleet = self.fleet.state();
        let mut hosts: Vec<HostView> = fleet
            .hosts()
            .map(|host| HostView {
                host_id: host.host_id.clone(),
                name: host.name.clone(),
                local: host.host_id == self.local_host,
                trusted: host.trust() == Trust::Trusted,
                candidate: host.trust() == Trust::Candidate,
                reach: ui_view::reach(&fleet, &self.local_host, &host.host_id),
                platform: host.platform.clone(),
                version: host.version.clone(),
                last_dial_error: host.last_dial_error.clone(),
                addrs: host.addrs.clone(),
                signed_in: host.signed_in,
                current: host.current.unwrap_or(false),
                providers: host
                    .providers
                    .iter()
                    .map(|offer| ProviderSignIn {
                        provider: offer.provider(),
                        signed_in: offer.signed_in,
                    })
                    .collect(),
            })
            .collect();
        hosts.sort_by(|a, b| (!a.local, &a.name).cmp(&(!b.local, &b.name)));
        hosts
    }

    pub fn take_fleet_changes(&self) -> FleetChanges {
        let batch = self.fleet_changes.take();
        FleetChanges {
            agents: batch.keys,
            hosts: batch.other,
        }
    }

    /// Starts an agent on a host and returns it as the fleet names it.
    pub async fn create_agent(&self, agent: &NewAgent) -> Result<AgentKey, RpcError> {
        let config = match agent.kind {
            Kind::Codex => wire::CodexCreateConfig {
                model: agent.model.clone(),
                effort: agent.effort.clone(),
                permission: agent.permission.clone(),
                mode: agent.mode.clone(),
            }
            .encode_to_vec(),
            Kind::ClaudeSdk | Kind::ClaudePty => wire::ClaudeCreateConfig {
                model: agent.model.clone(),
                effort: agent.effort.clone(),
                permission: agent.permission.clone(),
                ..Default::default()
            }
            .encode_to_vec(),
            Kind::Unspecified => Vec::new(),
        };
        let created = self
            .client
            .create_agent(CreateAgentRequest {
                agent_id: ui_runtime::inputs::input_id(),
                host: Some(wire::HostRef::id(agent.host_id.clone())),
                name: Some(agent.name.clone()).filter(|name| !name.is_empty()),
                cwd: agent.cwd.clone(),
                kind: agent.kind as i32,
                config,
                new_worktree: agent.new_worktree,
                ..Default::default()
            })
            .await?;
        Ok(ui_state::agent_key(&created))
    }

    /// What a provider offers on a host with no agent running: the models,
    /// efforts, permissions and modes a new agent there picks from. Whether
    /// the provider is signed in there is on the host's [`HostView`].
    pub async fn host_catalogue(
        &self,
        host_id: &[u8],
        provider: wire::Provider,
    ) -> Result<wire::Catalogue, RpcError> {
        let blob = self
            .client
            .get_catalogue(wire::GetCatalogueRequest {
                host_id: host_id.to_vec(),
                provider: provider as i32,
            })
            .await?;
        wire::Catalogue::decode(blob.bytes.as_slice()).map_err(|error| {
            RpcError::Refused(wire::Error {
                code: wire::ErrorCode::Internal as i32,
                message: format!("the host's catalogue does not decode: {error}"),
                details: Vec::new(),
            })
        })
    }

    /// Where a host offers to start an agent, filtered by `query` and at
    /// most `limit` in all, recent directories first.
    pub async fn directories(
        &self,
        host_id: &[u8],
        query: &str,
        limit: u32,
    ) -> Result<Directories, RpcError> {
        let listed = self
            .client
            .list_repositories(ListRepositoriesRequest {
                query: Some(query.to_owned()).filter(|query| !query.is_empty()),
                limit,
                host_id: Some(host_id.to_vec()),
            })
            .await?;
        let directory = |entry: ProjectEntry| Directory {
            path: entry.path,
            name: entry.name,
            last_used_ms: entry.last_used_ms,
        };
        Ok(Directories {
            recent: listed.recent.into_iter().map(directory).collect(),
            repositories: listed.repositories.into_iter().map(directory).collect(),
            roots: listed.roots,
        })
    }

    /// Renames, stops or deletes an agent.
    pub async fn agent_act(&self, agent: &AgentKey, act: &AgentAct) -> Result<(), RpcError> {
        let agent_id = agent.agent.clone();
        match act {
            AgentAct::Rename(name) => self
                .client
                .rename_agent(RenameAgentRequest {
                    agent_id,
                    name: name.clone(),
                })
                .await
                .map(drop),
            AgentAct::Stop => {
                self.client
                    .stop_agent(StopAgentRequest {
                        agent_id,
                        mode: StopMode::Graceful as i32,
                    })
                    .await
            }
            AgentAct::Delete => self
                .client
                .delete_agent(DeleteAgentRequest { agent_id })
                .await
                .map(drop),
        }
    }

    /// Writes a dump of the profile, with the fleet's part and every
    /// session's, and returns the bundle's directory.
    pub async fn dump(&self, reason: &str) -> Result<PathBuf, RpcError> {
        let response = self
            .client
            .dump(DumpRequest {
                agent_ids: Vec::new(),
                reason: reason.to_owned(),
                automatic: false,
            })
            .await?;
        let bundle = PathBuf::from(response.report_path);
        let mut parts = vec![self.fleet.dump_part()];
        parts.extend(
            self.fleet
                .sessions()
                .iter()
                .map(|session| session.dump_part()),
        );
        for part in &parts {
            write_part(&bundle, part)?;
        }
        Ok(bundle)
    }
}

fn write_part(bundle: &Path, part: &wire::DumpPart) -> Result<(), RpcError> {
    ui_runtime::write_part(bundle, part)
        .map_err(|error| RpcError::Transport(format!("writing the dump: {error}")))
}

impl Drop for AppRuntime {
    fn drop(&mut self) {
        self.fleet_watcher.abort();
    }
}

/// Gathers the fleet's changes for the host. The fleet keeps its sessions'
/// entries and hosts current itself.
async fn watch_fleet(fleet: Weak<Fleet>, changes: Arc<Coalescer<AgentKey>>) {
    let Some(mut changed) = fleet.upgrade().map(|fleet| fleet.changed()) else {
        return;
    };
    loop {
        let Some(fleet) = fleet.upgrade() else {
            return;
        };
        let agents = fleet.take_changed();
        let hosts = fleet.take_hosts_changed();
        changes.push(agents, false, hosts);
        drop(fleet);
        if changed.changed().await.is_err() {
            return;
        }
    }
}
