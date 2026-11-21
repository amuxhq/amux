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
//! through a [`client::Client`] of the profile's node, in memory on
//! the phone and over the profile's socket on the Mac. Reads ask that
//! node; every act on an agent is addressed to the host that owns it.

mod chat;
mod coalesce;
mod delta;
mod review;
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
use prost::Message as _;
pub use review::ReviewSession;
use tokio::task::JoinHandle;
pub use ui_runtime::OpenError;
use ui_runtime::{Fleet, Window};
use ui_state::AgentKey;
use ui_view::{FamilyHeader, FleetCard, FleetView, Repository, SettleWarning};
use values::{AgentAct, Directories, Directory, Draft, HostProvider, NewAgent};
pub use values::{FleetDelta, HostView};
use wire::{DumpRequest, Filing, Kind};

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
    client: Client,
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
    /// Opens the fleet on `client`, a client of the profile's node, and
    /// resolves once it has caught up with the local runtime. `local_host`
    /// is this device's host id.
    pub async fn open(
        client: Client,
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

    /// The client of the profile's node the fleet and its chats run on.
    pub fn client(&self) -> &Client {
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
        // Read after this agent's earlier saves, so a chat reopened at once
        // finds what was left in it. A draft that cannot be read is one the
        // person never sees again, not a chat that cannot open.
        let kept = self
            .fleet
            .kept_draft(agent)
            .await
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

    pub fn settle_warning(&self, agent: &AgentKey) -> Option<SettleWarning> {
        ui_view::settle_warning(&self.fleet.state(), agent)
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
                link: fleet.links(host.host_id()),
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
                through: host
                    .candidate()
                    .filter(|candidate| !candidate.nearby())
                    .and_then(|candidate| candidate.through_paired().next())
                    .map(|through| through.name.clone()),
                relay_only: host.candidate().is_some_and(wire::Candidate::relay_only),
                pairing_open: host
                    .candidate()
                    .is_some_and(|candidate| candidate.pairing_open),
                signed_in: host.account_signed_in(),
                current: host.current(),
                providers: host.providers.iter().map(HostProvider::from).collect(),
            })
            .collect();
        hosts.sort_by(|a, b| (!a.local, &a.name).cmp(&(!b.local, &b.name)));
        hosts
    }

    /// Home's view and every host, for the host opening on the fleet; every
    /// delta after it says what moved since. A delta asked for meanwhile,
    /// on the step's queue, waits for what this sends.
    pub fn fleet_opening(&self) -> FleetDelta {
        let mut sent = self.fleet_sent.lock().unwrap();
        self.fleet_changes.take();
        let (view, hosts) = (self.fleet_view(), self.hosts());
        *sent = Some((view.clone(), hosts.clone()));
        FleetDelta {
            view: Some(view),
            hosts: Some(hosts),
        }
    }

    /// What moved in the fleet since the host last heard, or None when
    /// nothing did or it has not opened on the fleet; either way the
    /// changes are taken, so the next one wakes the host again.
    pub fn fleet_delta(&self) -> Option<FleetDelta> {
        let mut sent = self.fleet_sent.lock().unwrap();
        let hosts_moved = self.fleet_changes.take().other;
        let (view, hosts) = sent.as_mut()?;
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

    /// Starts an agent on a host and returns it as the fleet names it: an
    /// agent of the program the person picked, of the kind the host's entry
    /// says that program starts as there, so an agent started here is the
    /// same as one started at the host. A pick the kind takes no value of at
    /// start is left out; what is not picked is the host's remembered pick,
    /// else the program's own default.
    pub async fn create_agent(&self, agent: &NewAgent) -> Result<AgentKey, RpcError> {
        let kind = self
            .fleet
            .state()
            .host(&agent.host_id)
            .and_then(|host| host.provider(&agent.provider))
            .map_or(Kind::Unspecified, wire::ProviderOnHost::kind);
        let chosen = ui_view::NewAgentChoices {
            model: agent.model.clone(),
            effort: agent.effort.clone(),
            permission: agent.permission.clone(),
            mode: agent.mode.clone(),
        };
        // None: the host lists no such program, or names no kind for it.
        let Some(config) = ui_view::create_config(kind, &agent.provider, &chosen) else {
            return Err(RpcError::Refused(wire::Error {
                code: wire::ErrorCode::FailedPrecondition as i32,
                message: format!(
                    "the computer has not said it runs {}; try again once it is reachable",
                    agent.provider
                ),
                details: Vec::new(),
            }));
        };
        let created = self
            .client
            .host(&agent.host_id)
            .agents()
            .create(wire::CreateAgentRequest {
                agent_id: ui_runtime::inputs::input_id(),
                name: Some(agent.name.clone()).filter(|name| !name.is_empty()),
                cwd: agent.cwd.clone(),
                agent: Some(config),
                new_workspace: agent.new_worktree,
                // A value is present here only because the person picked it.
                remember_model: agent.model.is_some(),
                remember_effort: agent.effort.is_some(),
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
        provider: &str,
    ) -> Result<wire::Catalogue, RpcError> {
        let blob = self.client.host(host_id).catalogue(provider).await?;
        wire::Catalogue::decode(blob.bytes.as_slice()).map_err(|error| {
            RpcError::Refused(wire::Error {
                code: wire::ErrorCode::Internal as i32,
                message: format!("the host's catalogue does not decode: {error}"),
                details: Vec::new(),
            })
        })
    }

    /// Where a host offers to start an agent, as its own entry in the
    /// inventory says, filtered by `query` (by name or path, ignoring case)
    /// and at most `limit` in all, in the host's order: its repositories,
    /// where its agents started lately, then where Claude Code and Codex
    /// ran. A host this device has no entry for offers nothing.
    pub async fn directories(
        &self,
        host_id: &[u8],
        query: &str,
        limit: u32,
    ) -> Result<Directories, RpcError> {
        let query = query.to_lowercase();
        let folders: Vec<wire::Folder> = self
            .fleet
            .state()
            .host(host_id)
            .map(|entry| entry.folders.clone())
            .unwrap_or_default();
        let mut directories = Directories::default();
        let matching = folders.into_iter().filter(|folder| {
            folder.name.to_lowercase().contains(&query)
                || folder.path.to_lowercase().contains(&query)
        });
        for folder in matching.take(limit as usize) {
            let kind = folder.kind();
            let directory = Directory {
                repository: Repository::of_checkout(folder.checkout.as_ref(), &folder.path),
                path: folder.path,
                name: folder.name,
            };
            match kind {
                wire::FolderKind::Pinned => directories.repositories.push(directory),
                wire::FolderKind::History => directories.history.push(directory),
                wire::FolderKind::Recent | wire::FolderKind::Unspecified => {
                    directories.recent.push(directory)
                }
            }
        }
        Ok(directories)
    }

    /// Pins a folder among a host's repositories, or unpins it. The host's
    /// entry says the change once it has made it.
    pub async fn pin_folder(
        &self,
        host_id: &[u8],
        path: &str,
        pinned: bool,
    ) -> Result<(), RpcError> {
        self.client.host(host_id).pin_folder(path, pinned).await
    }

    /// What a delete's confirmation says of the workspace that goes with
    /// the agent, as the agent's host previews it: none when the workspace
    /// stays, `Unknown` when the host could not be asked. The confirmation
    /// says it is checking until this answers, and its consent is the
    /// [`ui_state::DeleteWarning::discards`] of what it drew.
    pub async fn delete_warning(&self, agent: &AgentKey) -> Option<ui_state::DeleteWarning> {
        let preview = self.client.agent(&address(agent)).preview_delete().await;
        ui_state::DeleteWarning::previewed(preview.as_ref())
    }

    /// Renames, files or deletes an agent.
    pub async fn agent_act(&self, agent: &AgentKey, act: &AgentAct) -> Result<(), RpcError> {
        match act {
            AgentAct::Rename(name) => self
                .client
                .agent(&address(agent))
                .rename(name)
                .await
                .map(drop),
            AgentAct::Close => self.fleet.close_agent(agent).await.map(drop),
            AgentAct::CancelClose => {
                self.fleet.cancel_close(agent);
                Ok(())
            }
            AgentAct::Pin => self.fleet.file(agent, Filing::Pinned).await.map(drop),
            AgentAct::Unfile => self.fleet.file(agent, Filing::Open).await.map(drop),
            AgentAct::Delete | AgentAct::DeleteDiscardingChanges => {
                self.client
                    .agent(&address(agent))
                    .delete(*act == AgentAct::DeleteDiscardingChanges)
                    .await
            }
        }
    }

    /// Writes a dump with the fleet's part and the record of every chat
    /// on screen, and returns the bundle's directory: the same bundle
    /// whether it is then saved or sent.
    pub async fn dump(&self, reason: &str) -> Result<PathBuf, RpcError> {
        let response = self
            .client
            .admin()
            .dump(DumpRequest {
                reason: reason.to_owned(),
                sessions: self.fleet.on_screen_records(),
                ..DumpRequest::default()
            })
            .await?;
        let bundle = PathBuf::from(response.report_path);
        ui_runtime::write_part(&bundle, &self.fleet.dump_part())
            .map_err(|error| RpcError::Transport(format!("writing the dump: {error}")))?;
        Ok(bundle)
    }

    /// Writes a person's report around the dump `dump_path` names (or the
    /// report a failed send left there, to send it again), with its header
    /// and frozen frame, and sends it when the profile is signed in.
    pub async fn submit_report(
        &self,
        dump_path: String,
        header: Vec<u8>,
        frame_png: Vec<u8>,
    ) -> Result<values::ReportSent, RpcError> {
        let file = |name: &str, contents: Vec<u8>| wire::DumpFile {
            name: name.to_owned(),
            contents,
        };
        let mut files = vec![file("report.json", header)];
        if !frame_png.is_empty() {
            files.push(file("frame.png", frame_png));
        }
        let sent = self
            .client
            .admin()
            .submit_report(wire::SubmitReportRequest { dump_path, files })
            .await?;
        Ok(values::ReportSent {
            report_path: sent.report_path,
            id: sent.id,
            received_at_ms: sent.received_at_ms,
            not_sent: sent.not_sent,
        })
    }
}

/// An agent's key as the address of its host's services: the host that
/// owns it and its id there, so an act goes to that host.
fn address(agent: &AgentKey) -> wire::AgentRef {
    wire::AgentRef {
        host_id: agent.host.clone(),
        agent_id: agent.agent.clone(),
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
