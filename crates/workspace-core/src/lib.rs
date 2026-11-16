//! The workspace lifecycle as one function of its state and an input.
//!
//! A workspace is where an agent runs: a plain folder, a git worktree this
//! host made, or a wt tree. [`Workspaces::step`] takes what happened to a
//! workspace's members (one was added, is about to launch, stopped being
//! awake, was deleted) and what a job it asked for came back with, and says
//! which jobs to run: make a workspace, take or release amux's claim on it,
//! run its start hook, ask the backend to stop it once nothing else holds
//! it, delete it, and when a member may launch. Running the jobs is the
//! driver's, against the git and wt backends; nothing here reads a file, a
//! clock or a process.
//!
//! The rules it decides:
//!
//! - A member launches only while amux's claim on its workspace is held. The
//!   claim is taken when a member wakes and nothing else of amux's holds the
//!   workspace, and released when its last awake member goes.
//! - A workspace this host made is started (its start hook) each time the
//!   claim is taken, a restarted daemon's retaking it for a process it
//!   adopts included, and asked to stop once nothing holds it when its last
//!   awake member goes. Whether it is started is the backend's business; the
//!   hooks are safe to repeat.
//! - At most one stop-when-unheld call is outstanding per workspace, and none
//!   is issued while this host's start job for it runs: the stop waits for
//!   the start to finish and is then reconsidered, so it cannot overtake the
//!   start it follows. A start owed while a stop call is outstanding waits for
//!   that call: either the call never completes while the claim is held, so
//!   nothing was stopped, or it completes and the start follows it.
//! - Only workspaces this host made are made, started, stopped or deleted.
//!   Every wt tree with an awake member is claimed, made here or not.
//! - A workspace this host made is deleted once, after its last member is,
//!   and with its changes discarded only when some member's delete gave
//!   consent since it last gained a member. Removal never waits: the backend
//!   refuses a tree someone holds, or a dirty one without consent, and keeps
//!   it, clearing its owner, so it is no longer this host's.
//!
//! An entry lives while the workspace has a member or a job of the core's is
//! outstanding on it, so the answer to a job always finds the entry that
//! asked for it: the core never has two jobs of one kind outstanding on one
//! workspace, and the workspace names the job. See `docs/ARCHITECTURE.md` for
//! the principles every core follows.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// What a workspace is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Kind {
    /// A plain folder, or a repository's own working tree: never made,
    /// started, stopped or removed, with no backend.
    Folder,
    /// A git worktree amux made without wt.
    Git,
    /// A wt tree, made by amux or by hand.
    Wt,
}

impl Kind {
    /// amux makes and removes workspaces of this kind.
    pub const fn makes(self) -> bool {
        !matches!(self, Kind::Folder)
    }

    /// The backend counts holds on a workspace of this kind beside amux's
    /// claim (a `wt shell`, another daemon), so amux takes its claim with a
    /// job; for any other kind the claim is amux's own count alone.
    pub const fn holds(self) -> bool {
        matches!(self, Kind::Wt)
    }

    /// A workspace of this kind has start and stop hooks; for any other kind
    /// starting and stopping do nothing, so the core asks for neither.
    pub const fn hooks(self) -> bool {
        matches!(self, Kind::Wt)
    }
}

/// A workspace: its kind and id. For every local kind the id is the
/// workspace's root, a canonical path on its host.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Key {
    pub kind: Kind,
    pub id: String,
}

/// A member: an agent of this host, by its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MemberId(pub [u8; 16]);

/// How to make a new workspace, as the backend planned it without touching
/// the network. The core passes it to [`Output::Make`] as it came.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Plan {
    /// The repository the workspace is a checkout of.
    pub repository: String,
    /// The worktree's name, or the wt tree's label.
    pub name: String,
    /// The branch it checks out, new.
    pub branch: String,
    /// What the branch starts from, when not the repository's default.
    pub from: Option<String>,
}

/// What a delete did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Removal {
    /// The workspace is gone.
    Removed,
    /// The backend refused (someone holds it, or it has changes and the
    /// delete did not discard them) and cleared its owner: it stays on disk,
    /// no longer this host's.
    Kept,
}

/// Everything that can happen to the workspaces, decoded by the driver.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Input {
    /// A member was created in a new workspace this host is to make by
    /// `plan`. Its row is written; it launches once the workspace is made.
    /// A workspace the core already holds is joined, not made: a plan never
    /// names a folder that is taken.
    Create {
        member: MemberId,
        workspace: Key,
        plan: Plan,
    },
    /// A member is in an existing workspace: created in its folder, or
    /// resolved from its folder as the daemon starts. `made_here` is what
    /// the workspace said on disk as it was resolved: this host made it. It
    /// counts for the workspace's first member; after that the core's own
    /// record stands. `running`: its process already runs, as one a
    /// restarted daemon adopts.
    Added {
        member: MemberId,
        workspace: Key,
        made_here: bool,
        running: bool,
    },
    /// The daemon is about to launch the member's process, to start or
    /// resume it, and waits for [`Output::Launch`] (or [`Output::NotMade`]).
    Launching { member: MemberId },
    /// The member stopped being awake: it suspended, was settled or exited,
    /// or a launch it waited for was given up.
    Slept { member: MemberId },
    /// The member's row was deleted. `discard`: the delete agreed to lose
    /// the workspace's uncommitted changes. `descendants`: the workspace's
    /// other members that descend from it, which the family rule deletes
    /// after it.
    Removed {
        member: MemberId,
        discard: bool,
        descendants: Vec<MemberId>,
    },
    /// The daemon's start sweep found these workspaces saying on disk that
    /// this host made them; fed once every member has been added. Each
    /// without a member is deleted, keeping its changes.
    Swept { owned: Vec<Key> },
    /// The claim [`Output::TakeClaim`] asked for is held. A failure is
    /// counted as held, so the members still launch, and shown as the
    /// workspace's problem.
    Claimed {
        workspace: Key,
        result: Result<(), String>,
    },
    /// [`Output::Make`] finished: the workspace exists, or could not be made.
    Made {
        workspace: Key,
        result: Result<(), String>,
    },
    /// [`Output::Start`] finished. After a failure the workspace counts as
    /// started, and the failure is its problem.
    Started {
        workspace: Key,
        result: Result<(), String>,
    },
    /// [`Output::StopWhenUnheld`] finished. After a failure the workspace
    /// counts as stopped, and the failure is its problem.
    Stopped {
        workspace: Key,
        result: Result<(), String>,
    },
    /// [`Output::Delete`] finished.
    Deleted { workspace: Key, removal: Removal },
    /// A launch read the workspace's environment, or it would not load; the
    /// member launches without it and the failure is the workspace's
    /// problem.
    Environment {
        workspace: Key,
        result: Result<(), String>,
    },
}

/// Everything the core wants done, in order. The driver performs each before
/// the next: a claim is registered with the backend before a later output
/// runs, so a stop-when-unheld call that has not begun stopping can no longer
/// begin, and a release is done before the stop or delete after it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Output {
    /// Make the workspace by its plan; answered with [`Input::Made`] once it
    /// exists (wt's create hook runs on after it).
    Make { workspace: Key, plan: Plan },
    /// Take amux's claim on the workspace; answered with [`Input::Claimed`]
    /// once held, which is at once unless a stop hook already runs.
    TakeClaim { workspace: Key },
    /// Release amux's claim, at once.
    ReleaseClaim { workspace: Key },
    /// Run the workspace's start hook; answered with [`Input::Started`].
    Start { workspace: Key },
    /// Stop the workspace once nothing else holds it and it is not pinned;
    /// answered with [`Input::Stopped`]. It cannot complete while amux's
    /// claim is held.
    StopWhenUnheld { workspace: Key },
    /// Remove the workspace, without waiting: kept, and its owner cleared,
    /// if anyone holds it or it has changes and `discard` is false. Never
    /// asked for while amux's claim is held or a stop call is outstanding.
    /// Answered with [`Input::Deleted`].
    Delete { workspace: Key, discard: bool },
    /// The member's process may start now.
    Launch { member: MemberId },
    /// The member cannot launch: its workspace could not be made, or was
    /// removed. The daemon ends its row as could-not-start with `detail`.
    NotMade { member: MemberId, detail: String },
    /// The workspace as its members' rows show it changed.
    Publish { view: View },
}

/// A workspace as its members' rows show it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct View {
    pub workspace: Key,
    /// This host made it, and may remove it.
    pub made_here: bool,
    /// It is being made.
    pub creating: bool,
    /// A hook that failed, a claim that could not be taken or an environment
    /// that would not load, as the newest of them said.
    pub problem: Option<String>,
}

/// One cheap invariant the core checks after a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Check {
    /// A stable token, `workspace.<name>`, as the other cores' checks have.
    pub id: &'static str,
    /// The invariant, as the sentence it checks.
    pub sentence: &'static str,
}

/// A check that failed, and what it saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Broken {
    pub check: Check,
    pub detail: String,
}

impl fmt::Display for Broken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.check.sentence, self.detail)
    }
}

pub const RUNNING_WITHOUT_CLAIM: Check = Check {
    id: "workspace.running_without_claim",
    sentence: "a running member of a workspace that exists holds amux's claim or is taking it",
};
pub const CLAIM_WITHOUT_MEMBER: Check = Check {
    id: "workspace.claim_without_member",
    sentence: "a held claim has an awake member",
};
pub const STOP_BESIDE_START: Check = Check {
    id: "workspace.stop_beside_start",
    sentence: "no stop-when-unheld call is outstanding beside this host's start job",
};
pub const HOOKS_WITHOUT_HOOKS: Check = Check {
    id: "workspace.hooks_without_hooks",
    sentence: "a start or stop is outstanding only on a kind with hooks",
};
pub const CLAIMED_ABSENT: Check = Check {
    id: "workspace.claimed_absent",
    sentence: "a workspace being made holds no claim, and one gone holds none beyond a claim \
               asked for before it went",
};
pub const DELETE_BESIDE_STOP: Check = Check {
    id: "workspace.delete_beside_stop",
    sentence: "no delete is outstanding beside a stop-when-unheld call",
};
pub const IDLE_ENTRY: Check = Check {
    id: "workspace.idle_entry",
    sentence: "a workspace without members is kept only while a job of the core's is outstanding on it",
};
pub const MEMBER_INDEX: Check = Check {
    id: "workspace.member_index",
    sentence: "each member is in exactly the workspace the index names",
};

/// Every check [`Workspaces::check_invariants`] makes.
pub const CHECKS: &[Check] = &[
    RUNNING_WITHOUT_CLAIM,
    CLAIM_WITHOUT_MEMBER,
    STOP_BESIDE_START,
    HOOKS_WITHOUT_HOOKS,
    CLAIMED_ABSENT,
    DELETE_BESIDE_STOP,
    IDLE_ENTRY,
    MEMBER_INDEX,
];

/// Whether a member is awake: its process runs or is being launched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Awake {
    Asleep,
    /// About to launch, waiting for the claim or for the workspace to be
    /// made.
    Waiting,
    /// Launched, or adopted running.
    Running,
}

/// Where the workspace is in being made and removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Phase {
    /// Its make is outstanding.
    Creating,
    /// It exists.
    Ready,
    /// Its delete is outstanding.
    Deleting,
    /// Nothing of it exists that the core knows of: its make failed, or it
    /// was removed.
    Absent,
}

/// amux's claim on the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Claim {
    None,
    /// [`Output::TakeClaim`] is outstanding.
    Taking,
    Held,
}

/// What a problem came from: a later success of the same clears it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Source {
    Claim,
    Start,
    Stop,
    Environment,
}

/// One workspace: only what amux itself decides.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Workspace {
    made_here: bool,
    phase: Phase,
    members: BTreeMap<MemberId, Awake>,
    claim: Claim,
    /// This host's start job is outstanding.
    starting: bool,
    /// The claim was taken and the start it calls for is not yet issued.
    start_owed: bool,
    /// A stop-when-unheld call is outstanding.
    waiter: bool,
    /// The last awake member went and the stop it calls for is not yet
    /// issued.
    stop_owed: bool,
    /// A member's delete agreed to discard changes, when every other member
    /// then was its descendant, since the workspace last gained a member.
    consent: bool,
    problem: Option<(Source, String)>,
}

impl Workspace {
    fn new(made_here: bool, phase: Phase) -> Self {
        Self {
            made_here,
            phase,
            members: BTreeMap::new(),
            claim: Claim::None,
            starting: false,
            start_owed: false,
            waiter: false,
            stop_owed: false,
            consent: false,
            problem: None,
        }
    }

    fn awake(&self) -> bool {
        self.members.values().any(|awake| *awake != Awake::Asleep)
    }

    /// A new member: consent given before it does not cover its changes.
    fn join(&mut self, member: MemberId, awake: Awake) {
        self.members.insert(member, awake);
        self.consent = false;
    }

    /// Nothing is left to decide: no member, no claim, no job outstanding,
    /// and nothing this host must still remove.
    fn idle(&self) -> bool {
        self.members.is_empty()
            && self.claim == Claim::None
            && !self.starting
            && !self.waiter
            && !self.stop_owed
            && !self.made_here
            && matches!(self.phase, Phase::Ready | Phase::Absent)
    }

    fn note(&mut self, source: Source, result: Result<(), String>) {
        match result {
            Err(detail) => self.problem = Some((source, detail)),
            Ok(())
                if self
                    .problem
                    .as_ref()
                    .is_some_and(|(from, _)| *from == source) =>
            {
                self.problem = None;
            }
            Ok(()) => {}
        }
    }

    /// Every member waiting to launch is told it cannot.
    fn not_made(&mut self, detail: &str, out: &mut Vec<Output>) {
        for (member, awake) in &mut self.members {
            if *awake != Awake::Asleep {
                *awake = Awake::Asleep;
                out.push(Output::NotMade {
                    member: *member,
                    detail: detail.to_owned(),
                });
            }
        }
    }
}

/// Every workspace this host's agents are in, and those it still has a job
/// outstanding on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Workspaces {
    workspaces: BTreeMap<Key, Workspace>,
    /// Which workspace each member is in.
    members: BTreeMap<MemberId, Key>,
}

impl Workspaces {
    /// The core a daemon starts with: it learns its members as each is
    /// resolved ([`Input::Added`]), then sweeps ([`Input::Swept`]). Nothing
    /// carries over from before a restart: the stop calls and start jobs of
    /// the daemon before were lost with its process.
    pub fn new() -> Self {
        Self::default()
    }

    /// One input in, outputs appended. The daemon core composing this one
    /// checks [`Workspaces::check_invariants`] at the end of its own step.
    pub fn step(&mut self, input: Input, out: &mut Vec<Output>) {
        let touched = self.touched_by(&input);
        let before: Vec<Option<View>> = touched.iter().map(|key| self.view(key)).collect();
        self.take(input, out);
        for key in &touched {
            self.settle(key, out);
        }
        for (key, before) in touched.iter().zip(before) {
            let after = self.view(key);
            if after != before
                && let Some(view) = after
                && !self.workspaces[key].members.is_empty()
            {
                out.push(Output::Publish { view });
            }
        }
    }

    /// The workspace as its members' rows show it, while the core holds it.
    pub fn view(&self, key: &Key) -> Option<View> {
        let workspace = self.workspaces.get(key)?;
        Some(View {
            workspace: key.clone(),
            made_here: workspace.made_here,
            creating: workspace.phase == Phase::Creating,
            problem: workspace.problem.as_ref().map(|(_, detail)| detail.clone()),
        })
    }

    /// The member's workspace, as its row shows it.
    pub fn view_of(&self, member: MemberId) -> Option<View> {
        self.view(self.members.get(&member)?)
    }

    /// The workspace the member is in.
    pub fn workspace_of(&self, member: MemberId) -> Option<&Key> {
        self.members.get(&member)
    }

    /// The workspace's members, in id order.
    pub fn members(&self, key: &Key) -> Vec<MemberId> {
        self.workspaces
            .get(key)
            .map(|workspace| workspace.members.keys().copied().collect())
            .unwrap_or_default()
    }

    /// The workspaces the core holds, in key order.
    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        self.workspaces.keys()
    }

    /// The cheap predicates checked after every step:
    ///
    /// - a running member of a workspace that exists holds amux's claim or
    ///   is taking it, and a held claim has an awake member;
    /// - no stop-when-unheld call is outstanding beside this host's start
    ///   job, and either is outstanding only on a kind with hooks;
    /// - a workspace being made holds no claim, and one gone holds none
    ///   beyond a claim asked for before it went, whose answer releases it;
    /// - no delete is outstanding beside a stop-when-unheld call;
    /// - a workspace without members is kept only while a job of the core's
    ///   is outstanding on it;
    /// - each member is in exactly the workspace the index names.
    pub fn check_invariants(&self) -> Result<(), Broken> {
        let broken = |check, key: &Key, workspace: &Workspace| Broken {
            check,
            detail: format!("{key:?}: {workspace:?}"),
        };
        for (key, workspace) in &self.workspaces {
            let running = workspace
                .members
                .values()
                .any(|awake| *awake == Awake::Running);
            if running
                && workspace.claim == Claim::None
                && matches!(workspace.phase, Phase::Ready | Phase::Deleting)
            {
                return Err(broken(RUNNING_WITHOUT_CLAIM, key, workspace));
            }
            if workspace.claim == Claim::Held && !workspace.awake() {
                return Err(broken(CLAIM_WITHOUT_MEMBER, key, workspace));
            }
            if workspace.waiter && workspace.starting {
                return Err(broken(STOP_BESIDE_START, key, workspace));
            }
            if (workspace.waiter || workspace.starting) && !key.kind.hooks() {
                return Err(broken(HOOKS_WITHOUT_HOOKS, key, workspace));
            }
            let claimed = match workspace.phase {
                Phase::Creating => workspace.claim != Claim::None,
                Phase::Absent => workspace.claim == Claim::Held,
                Phase::Ready | Phase::Deleting => false,
            };
            if claimed {
                return Err(broken(CLAIMED_ABSENT, key, workspace));
            }
            if workspace.phase == Phase::Deleting && workspace.waiter {
                return Err(broken(DELETE_BESIDE_STOP, key, workspace));
            }
            if workspace.members.is_empty()
                && workspace.claim != Claim::Taking
                && !workspace.starting
                && !workspace.waiter
                && !matches!(workspace.phase, Phase::Creating | Phase::Deleting)
            {
                return Err(broken(IDLE_ENTRY, key, workspace));
            }
            for member in workspace.members.keys() {
                if self.members.get(member) != Some(key) {
                    return Err(broken(MEMBER_INDEX, key, workspace));
                }
            }
        }
        for (member, key) in &self.members {
            if !self
                .workspaces
                .get(key)
                .is_some_and(|workspace| workspace.members.contains_key(member))
            {
                return Err(Broken {
                    check: MEMBER_INDEX,
                    detail: format!("{member:?} is indexed in {key:?}, which does not hold it"),
                });
            }
        }
        Ok(())
    }

    // --- inputs ----------------------------------------------------------

    /// The workspaces an input may change.
    fn touched_by(&self, input: &Input) -> Vec<Key> {
        let mut keys = Vec::new();
        match input {
            Input::Create {
                member, workspace, ..
            }
            | Input::Added {
                member, workspace, ..
            } => {
                keys.push(workspace.clone());
                if let Some(old) = self.members.get(member)
                    && old != workspace
                {
                    keys.push(old.clone());
                }
            }
            Input::Launching { member }
            | Input::Slept { member }
            | Input::Removed { member, .. } => keys.extend(self.members.get(member).cloned()),
            Input::Swept { owned } => keys.extend(owned.iter().cloned()),
            Input::Claimed { workspace, .. }
            | Input::Made { workspace, .. }
            | Input::Started { workspace, .. }
            | Input::Stopped { workspace, .. }
            | Input::Deleted { workspace, .. }
            | Input::Environment { workspace, .. } => keys.push(workspace.clone()),
        }
        keys
    }

    fn take(&mut self, input: Input, out: &mut Vec<Output>) {
        match input {
            Input::Create {
                member,
                workspace,
                plan,
            } => {
                self.detach(member, &workspace);
                let entry = self
                    .workspaces
                    .entry(workspace.clone())
                    .or_insert_with(|| Workspace::new(false, Phase::Absent));
                if entry.phase == Phase::Absent && workspace.kind.makes() {
                    entry.phase = Phase::Creating;
                    entry.made_here = true;
                    out.push(Output::Make {
                        workspace: workspace.clone(),
                        plan,
                    });
                } else if entry.phase == Phase::Absent {
                    // A folder is never made: it is there.
                    entry.phase = Phase::Ready;
                }
                entry.join(member, Awake::Asleep);
                self.members.insert(member, workspace);
            }
            Input::Added {
                member,
                workspace,
                made_here,
                running,
            } => {
                self.detach(member, &workspace);
                let made_here = made_here && workspace.kind.makes();
                let entry = self
                    .workspaces
                    .entry(workspace.clone())
                    .or_insert_with(|| Workspace::new(made_here, Phase::Ready));
                // A workspace resolved from its folder exists, whatever the
                // core last knew of it.
                if entry.phase == Phase::Absent {
                    entry.phase = Phase::Ready;
                    entry.made_here = made_here;
                }
                let awake = if running {
                    Awake::Running
                } else {
                    Awake::Asleep
                };
                entry.join(member, awake);
                self.members.insert(member, workspace);
            }
            Input::Launching { member } => {
                let Some(workspace) = self.workspace_mut(member) else {
                    return;
                };
                if workspace.members[&member] != Awake::Asleep {
                    return;
                }
                if workspace.phase == Phase::Absent {
                    out.push(Output::NotMade {
                        member,
                        detail: "its workspace no longer exists".to_owned(),
                    });
                } else {
                    workspace.members.insert(member, Awake::Waiting);
                }
            }
            Input::Slept { member } => {
                if let Some(workspace) = self.workspace_mut(member) {
                    workspace.members.insert(member, Awake::Asleep);
                }
            }
            Input::Removed {
                member,
                discard,
                descendants,
            } => {
                let Some(key) = self.members.remove(&member) else {
                    return;
                };
                let workspace = self.workspaces.get_mut(&key).expect("an indexed workspace");
                workspace.members.remove(&member);
                // A delete agrees to lose its own changes and those of the
                // descendants that follow it, and no other member's: the
                // clients warn of the loss only when everyone else in the
                // workspace goes with it.
                let covers = workspace
                    .members
                    .keys()
                    .all(|other| descendants.contains(other));
                workspace.consent |= discard && covers;
            }
            Input::Swept { owned } => {
                for key in owned {
                    if key.kind.makes() && !self.workspaces.contains_key(&key) {
                        self.workspaces
                            .insert(key, Workspace::new(true, Phase::Ready));
                    }
                }
            }
            Input::Claimed { workspace, result } => {
                let Some(entry) = self.workspaces.get_mut(&workspace) else {
                    return;
                };
                if entry.claim == Claim::Taking {
                    entry.claim = Claim::Held;
                    entry.note(Source::Claim, result);
                }
            }
            Input::Made { workspace, result } => {
                let Some(entry) = self.workspaces.get_mut(&workspace) else {
                    return;
                };
                if entry.phase != Phase::Creating {
                    return;
                }
                match result {
                    Ok(()) => entry.phase = Phase::Ready,
                    // What is left of it is still this host's to remove
                    // with its last member.
                    Err(detail) => {
                        entry.phase = Phase::Absent;
                        entry.not_made(&detail, out);
                    }
                }
            }
            Input::Started { workspace, result } => {
                if let Some(entry) = self.workspaces.get_mut(&workspace)
                    && entry.starting
                {
                    entry.starting = false;
                    entry.note(Source::Start, result);
                }
            }
            Input::Stopped { workspace, result } => {
                if let Some(entry) = self.workspaces.get_mut(&workspace)
                    && entry.waiter
                {
                    entry.waiter = false;
                    entry.note(Source::Stop, result);
                }
            }
            Input::Deleted { workspace, removal } => {
                let Some(entry) = self.workspaces.get_mut(&workspace) else {
                    return;
                };
                if entry.phase != Phase::Deleting {
                    return;
                }
                // Removed or kept, it is no longer this host's: a kept one
                // had its owner cleared, so a member that joined meanwhile
                // finds it as one made by hand, and it is not started.
                entry.made_here = false;
                entry.consent = false;
                entry.start_owed = false;
                match removal {
                    // The removal happened whatever claim was asked for since:
                    // a member that woke meanwhile cannot launch, and a claim
                    // still being taken is released once its answer comes.
                    Removal::Removed => {
                        entry.phase = Phase::Absent;
                        // The removal ran the stop hook itself.
                        entry.stop_owed = false;
                        entry.not_made("its workspace was removed", out);
                    }
                    // A stop owed as its last member went is issued now.
                    Removal::Kept => entry.phase = Phase::Ready,
                }
            }
            Input::Environment { workspace, result } => {
                if let Some(entry) = self.workspaces.get_mut(&workspace) {
                    entry.note(Source::Environment, result);
                }
            }
        }
    }

    /// Takes a member out of a workspace it is leaving for `to`, as a delete
    /// that keeps changes would.
    fn detach(&mut self, member: MemberId, to: &Key) {
        if let Some(old) = self.members.get(&member)
            && old != to
        {
            let old = self.members.remove(&member).expect("just read");
            if let Some(workspace) = self.workspaces.get_mut(&old) {
                workspace.members.remove(&member);
            }
        }
    }

    fn workspace_mut(&mut self, member: MemberId) -> Option<&mut Workspace> {
        let key = self.members.get(&member)?;
        self.workspaces.get_mut(key)
    }

    // --- decisions -------------------------------------------------------

    /// Everything the workspace's state now calls for, in the order the
    /// driver must perform it.
    fn settle(&mut self, key: &Key, out: &mut Vec<Output>) {
        let Some(workspace) = self.workspaces.get_mut(key) else {
            return;
        };
        let kind = key.kind;
        let awake = workspace.awake();

        // A claim nothing awake needs goes, and the stop it held off is owed.
        if workspace.claim == Claim::Held && !awake {
            if kind.holds() {
                out.push(Output::ReleaseClaim {
                    workspace: key.clone(),
                });
            }
            workspace.claim = Claim::None;
            workspace.start_owed = false;
            workspace.stop_owed = workspace.made_here && kind.hooks();
        }

        // An awake member needs the claim, also while the workspace's delete
        // is outstanding: a member that joined it then keeps it, the backend
        // refusing the removal for amux's hold, and no stop call issued
        // before can begin under it. A start follows every claim taken,
        // unless this host's start job is still outstanding, in which case
        // nothing has stopped since that start was asked for. That includes
        // the claim a restarted daemon retakes for a process it adopts: the
        // daemon before may have lost a start job it had asked for, or one
        // it held off behind a stop call, and nothing records whether a
        // start completed, so the start hook, safe to repeat, runs again.
        if workspace.claim == Claim::None
            && awake
            && matches!(workspace.phase, Phase::Ready | Phase::Deleting)
        {
            workspace.stop_owed = false;
            workspace.start_owed = workspace.made_here && kind.hooks() && !workspace.starting;
            if kind.holds() {
                out.push(Output::TakeClaim {
                    workspace: key.clone(),
                });
                workspace.claim = Claim::Taking;
            } else {
                workspace.claim = Claim::Held;
            }
        }

        if workspace.claim == Claim::Held && workspace.phase == Phase::Ready {
            for (member, awake) in &mut workspace.members {
                if *awake == Awake::Waiting {
                    *awake = Awake::Running;
                    out.push(Output::Launch { member: *member });
                }
            }
            if workspace.start_owed && !workspace.waiter && !workspace.starting {
                workspace.start_owed = false;
                workspace.starting = true;
                out.push(Output::Start {
                    workspace: key.clone(),
                });
            }
        }

        // A workspace about to be deleted is stopped by its removal; one
        // kept is stopped here once its delete comes back.
        let leaving = workspace.members.is_empty() && workspace.made_here;
        if workspace.stop_owed
            && workspace.claim == Claim::None
            && !workspace.starting
            && workspace.phase == Phase::Ready
            && !leaving
        {
            workspace.stop_owed = false;
            // A call already outstanding completes once nothing holds the
            // workspace, which is now.
            if !workspace.waiter {
                workspace.waiter = true;
                out.push(Output::StopWhenUnheld {
                    workspace: key.clone(),
                });
            }
        }

        // Its claim must be gone first, or the backend would refuse the
        // removal for amux's own hold, and so must a stop call: its hook
        // holds the tree exclusively while it runs, and wt refuses a
        // removal under it rather than waiting.
        if leaving
            && workspace.claim == Claim::None
            && !workspace.waiter
            && matches!(workspace.phase, Phase::Ready | Phase::Absent)
        {
            workspace.phase = Phase::Deleting;
            out.push(Output::Delete {
                workspace: key.clone(),
                discard: workspace.consent,
            });
        }

        if workspace.idle() {
            self.workspaces.remove(key);
        }
    }
}
