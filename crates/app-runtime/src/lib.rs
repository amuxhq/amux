//! The phone's chats and fleet over the local runtime.
//!
//! [`AppRuntime`] hosts the fleet driver, which holds one session per live
//! agent, and gathers what they change until the host runs next: a host is
//! woken once per turn with the chat or the fleet that moved, and takes
//! what moved as one delta, computed against what it was last handed. A
//! chat borrows the fleet's session for its agent; leaving the foreground
//! closes every session. Rows are handed over by key, so a host
//! reconfigures only the cells an update changed.
//!
//! Nothing here opens a store or knows the node: the runtime is reached
//! through a [`client::Client`], in process on the phone.

mod chat;
mod coalesce;
mod delta;
#[cfg(test)]
mod schedules;
pub mod values;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

pub use chat::Chat;
use client::{Client, RpcError};
pub use coalesce::{Batch, Coalescer, WakeFn};
use model::AgentKey;
use prost::Message as _;
use tokio::task::JoinHandle;
pub use ui_runtime::OpenError;
use ui_runtime::{Fleet, Window};
use ui_view::{FamilyHeader, FleetCard, FleetView};
use values::{AgentAct, Directories, Directory, Draft, NewAgent, ProviderSignIn};
pub use values::{FleetDelta, HostView};
/// The provider whose catalogue `host_catalogue` reads, named here so the
/// phone's bridge passes it on without a dependency of its own on the wire
/// crate.
pub use wire::Provider;
use wire::{
    CreateAgentRequest, DeleteAgentRequest, DumpRequest, Kind, ListRepositoriesRequest,
    ProjectEntry, RenameAgentRequest,
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
    /// The family roots whose members home lists under them, by agent id.
    expanded: Mutex<HashSet<Vec<u8>>>,
    /// What home's list is narrowed to.
    filter: Mutex<ui_view::FleetFilter>,
    /// The view and hosts the host was last handed; None until it opens on
    /// the fleet, and no delta is computed before.
    fleet_sent: Mutex<Option<(FleetView, Vec<HostView>)>>,
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
            expanded: Mutex::new(HashSet::new()),
            filter: Mutex::new(ui_view::FleetFilter::default()),
            fleet_sent: Mutex::new(None),
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
        // A draft that cannot be read is one the person never sees again,
        // not a chat that cannot open.
        let kept = self
            .client
            .get_draft(wire::GetDraftRequest {
                agent_id: agent.agent.clone(),
            })
            .await
            .ok()
            .map(|draft| Draft::kept(&draft))
            .filter(|draft| !draft.parts.is_empty());
        let id = self.next_chat.fetch_add(1, Ordering::Relaxed);
        let wake = self.wake.clone();
        let chat = Chat::start(
            id,
            agent.clone(),
            session,
            Arc::downgrade(&self.fleet),
            Arc::new(move || wake(Wake::Chat(id))),
            self.clock.clone(),
            self.client.clone(),
            kept,
        );
        Ok(chat)
    }

    /// Lists a family's members under its root, by the root's agent id,
    /// or folds them away again.
    pub fn toggle_family(&self, root: &[u8]) {
        let mut expanded = self.expanded.lock().unwrap();
        if !expanded.remove(root) {
            expanded.insert(root.to_vec());
        }
    }

    /// Narrows home's list to what `filter` keeps.
    pub fn set_filter(&self, filter: ui_view::FleetFilter) {
        *self.filter.lock().unwrap() = filter;
    }

    /// Home's sections: families under their roots, expanded where the
    /// person listed their members, each row with its second line, listing
    /// the agents the filter keeps.
    pub fn fleet_view(&self) -> FleetView {
        let expand = self.expanded.lock().unwrap().clone();
        let filter = self.filter.lock().unwrap().clone();
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
        let fleet = self.fleet.state();
        ui_view::fleet_view(&fleet, &self.local_host, &lines, &expand, &|agent| {
            filter.keeps(&fleet, &self.local_host, agent)
        })
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
                host_id: host.host_id().to_vec(),
                name: host.name().to_owned(),
                local: host.host_id() == self.local_host,
                trusted: host.trusted().is_some(),
                candidate: host.candidate().is_some(),
                reach: ui_view::reach(&fleet, &self.local_host, host.host_id()),
                platform: Some(host.platform())
                    .filter(|platform| *platform != wire::Platform::Unspecified),
                version: host.version().map(str::to_owned),
                last_dial_error: host
                    .offline()
                    .and_then(|offline| offline.last_dial_error.clone()),
                addrs: host
                    .candidate()
                    .map(|candidate| candidate.addrs.clone())
                    .unwrap_or_default(),
                signed_in: host.account_signed_in(),
                current: host.current(),
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

    /// Home's view and every host, for the host opening on the fleet; every
    /// delta after it says what moved since.
    pub fn fleet_opening(&self) -> FleetDelta {
        self.fleet_changes.take();
        let (view, hosts) = (self.fleet_view(), self.hosts());
        *self.fleet_sent.lock().unwrap() = Some((view.clone(), hosts.clone()));
        FleetDelta {
            view: Some(view),
            hosts: Some(hosts),
        }
    }

    /// What moved in the fleet since the host last heard, or None when
    /// nothing did or it has not opened on the fleet; the next change wakes
    /// the host again.
    pub fn fleet_delta(&self) -> Option<FleetDelta> {
        let mut sent = self.fleet_sent.lock().unwrap();
        let (view, hosts) = sent.as_mut()?;
        let hosts_moved = self.fleet_changes.take().other;
        let delta = FleetDelta {
            view: Some(self.fleet_view()).filter(|now| now != view),
            hosts: Some(self.hosts()).filter(|now| hosts_moved || now != hosts),
        };
        if let Some(now) = &delta.view {
            *view = now.clone();
        }
        if let Some(now) = &delta.hosts {
            *hosts = now.clone();
        }
        (delta.view.is_some() || delta.hosts.is_some()).then_some(delta)
    }

    /// Starts an agent on a host and returns it as the fleet names it.
    pub async fn create_agent(&self, agent: &NewAgent) -> Result<AgentKey, RpcError> {
        use wire::create_agent_request::Agent;
        let claude = || wire::ClaudeCreateConfig {
            model: agent.model.clone(),
            effort: agent.effort.clone(),
            permission: agent.permission.clone(),
            ..Default::default()
        };
        let config = match agent.kind {
            Kind::Codex => Some(Agent::Codex(wire::CodexCreateConfig {
                model: agent.model.clone(),
                effort: agent.effort.clone(),
                permission: agent.permission.clone(),
                mode: agent.mode.clone(),
            })),
            Kind::ClaudeSdk => Some(Agent::ClaudeSdk(claude())),
            Kind::ClaudePty => Some(Agent::ClaudePty(claude())),
            Kind::Unspecified => None,
        };
        let created = self
            .client
            .create_agent(CreateAgentRequest {
                agent_id: ui_runtime::inputs::input_id(),
                host: Some(wire::HostRef::id(agent.host_id.clone())),
                name: Some(agent.name.clone()).filter(|name| !name.is_empty()),
                cwd: agent.cwd.clone(),
                agent: config,
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
        provider: Provider,
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
            AgentAct::Stop => self.fleet.stop(agent).await.map(drop),
            AgentAct::CancelStop => {
                self.fleet.cancel_stop(agent);
                Ok(())
            }
            AgentAct::Delete => self
                .client
                .delete_agent(DeleteAgentRequest { agent_id })
                .await
                .map(drop),
        }
    }

    /// Writes a dump of the profile, with every open session for the daemon
    /// to redact and write and the fleet's part, and returns the bundle's
    /// directory.
    pub async fn dump(&self, reason: &str) -> Result<PathBuf, RpcError> {
        let sessions = self
            .fleet
            .sessions()
            .iter()
            .map(|session| session.history())
            .collect();
        let response = self
            .client
            .dump(DumpRequest {
                agent_ids: Vec::new(),
                reason: reason.to_owned(),
                automatic: false,
                sessions,
            })
            .await?;
        let bundle = PathBuf::from(response.report_path);
        ui_runtime::write_part(&bundle, &self.fleet.dump_part())
            .map_err(|error| RpcError::Transport(format!("writing the dump: {error}")))?;
        Ok(bundle)
    }
}

impl Drop for AppRuntime {
    fn drop(&mut self) {
        self.fleet_watcher.abort();
    }
}

/// The least time between two of the fleet's wakes for its agents alone. A
/// working agent's step moves its line on home many times a second while
/// it streams, and each delta lays out the whole home on the host's main
/// thread; agents' changes soon after a wake wait out the rest of this and
/// one delta takes them all. A host moving wakes the host at once, since
/// that is what a reconnecting fleet is waiting on.
const FLEET_SPACING: std::time::Duration = std::time::Duration::from_millis(250);

/// Gathers the fleet's changes for the host. The fleet keeps its sessions'
/// entries and hosts current itself.
async fn watch_fleet(fleet: Weak<Fleet>, changes: Arc<Coalescer<AgentKey>>) {
    let Some(mut changed) = fleet.upgrade().map(|fleet| fleet.changed()) else {
        return;
    };
    let mut woke: Option<tokio::time::Instant> = None;
    let mut hosts_owed = false;
    loop {
        let Some(held) = fleet.upgrade() else {
            return;
        };
        let agents = held.take_changed();
        let hosts = std::mem::take(&mut hosts_owed) | held.take_hosts_changed();
        if !agents.is_empty() || hosts {
            changes.push(agents, false, hosts);
            woke = Some(tokio::time::Instant::now());
        }
        drop(held);
        if changed.changed().await.is_err() {
            return;
        }
        let Some(woke) = woke else { continue };
        let spaced = tokio::time::sleep_until(woke + FLEET_SPACING);
        tokio::pin!(spaced);
        loop {
            tokio::select! {
                () = &mut spaced => break,
                moved = changed.changed() => {
                    if moved.is_err() {
                        return;
                    }
                    let Some(held) = fleet.upgrade() else {
                        return;
                    };
                    if held.take_hosts_changed() {
                        hosts_owed = true;
                        break;
                    }
                }
            }
        }
    }
}
