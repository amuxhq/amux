//! One workspace and the agents in it: when amux claims it, starts it, stops
//! it and removes it.
//!
//! The core is the real `workspace_core::Workspaces`, held between steps as
//! its value. Around it is one workspace on disk of a chosen origin: a wt
//! tree or git worktree this host makes for its first agent, a wt tree a
//! person made, another host's wt tree, or a plain folder. Agents are
//! created in it and launched, suspend, are settled or exit, and are
//! deleted, with or without consent to discard its changes (by a person, or
//! without consent by retention or the family rule). The second agent is
//! the first's child, so it follows the first's delete; any later one is
//! nobody's. A person opens and
//! closes a `wt shell` in a tree, pins it with `wt start`, unpins it, runs a
//! hard `wt stop` and edits its files; another host's daemon holds the tree
//! and lets it go, and stops a tree it made. The create hook finishes or
//! fails; a start, stop or delete succeeds or fails (a timeout is a failure
//! with a detail, so the core sees it as one), and a delete's answer reaches
//! the core after the backend has run it, so an agent may wake between; the
//! daemon restarts.
//!
//! The backend's holds are modelled by their meaning. A claim the core
//! takes counts at once. It is granted at once unless a stop hook runs, in
//! which case it waits for the hook and is granted as the hook ends. A
//! stop-when-unheld call waits until nothing holds the tree (amux's claim,
//! a shell, another daemon); it then completes without stopping if the tree
//! is pinned, and otherwise runs the stop hook, which a running create
//! hook delays. A removal is refused, keeping the tree and clearing its
//! owner, if anything holds or pins it (a running stop hook holds it
//! exclusively), or if it is dirty and the delete does not discard. A restart keeps the agents' processes and loses the
//! daemon's claims and the calls still waiting; hooks wt has begun run to
//! their end, and a make in flight leaves a half-made tree whose agent is
//! marked could-not-start. The restarted daemon adds every live agent from
//! its folder, the running ones first, then sweeps the trees it owns.

use workspace_core::{Input, Key, Kind, MemberId, Output, Plan, Removal, Workspaces};

use crate::{Machine, Property};

const TREE: &str = "/trees/fix";

/// Where the one workspace comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    /// A wt tree this host makes for its first agent.
    MadeWt,
    /// A git worktree this host makes for its first agent.
    MadeGit,
    /// A wt tree a person made with `wt new`.
    HandWt,
    /// A wt tree another host made, which it holds, starts and stops.
    OtherWt,
    /// A plain folder.
    Folder,
}

impl Origin {
    /// Every origin, in the order the tests check them.
    pub const ALL: [Origin; 5] = [
        Origin::MadeWt,
        Origin::MadeGit,
        Origin::HandWt,
        Origin::OtherWt,
        Origin::Folder,
    ];

    fn kind(self) -> Kind {
        match self {
            Origin::MadeWt | Origin::HandWt | Origin::OtherWt => Kind::Wt,
            Origin::MadeGit => Kind::Git,
            Origin::Folder => Kind::Folder,
        }
    }

    /// This host makes it.
    fn made_here(self) -> bool {
        matches!(self, Origin::MadeWt | Origin::MadeGit)
    }
}

/// One workspace's lifecycle, within its bounds.
#[derive(Clone, Debug)]
pub struct WorkspaceLifecycle {
    pub origin: Origin,
    /// Agents created in the workspace, all told.
    pub agents: u8,
    /// Launches of an agent after its first, all told.
    pub launches: u8,
    pub restarts: u8,
    /// `wt shell`s a person opens, all told.
    pub shells: u8,
    /// Times a person pins the tree, all told.
    pub pins: u8,
    /// Hard `wt stop`s, all told.
    pub hard_stops: u8,
    /// Times another host's daemon holds the tree, all told.
    pub other_holds: u8,
    /// Times another host stops a tree it made, all told.
    pub other_stops: u8,
    /// Hooks and makes that fail, all told.
    pub failures: u8,
    /// Times a person or an agent edits the tree, leaving it dirty or
    /// committing it clean again, all told.
    pub edits: u8,
}

impl WorkspaceLifecycle {
    /// Every part of the world once, beside two agents with two launches
    /// after their first: 54,881,148 states in 95 s on the 12 threads of an
    /// M-series Mac for the wt tree this host makes, and at most 59,940 for
    /// any other origin.
    pub const fn world(origin: Origin) -> Self {
        Self {
            origin,
            agents: 2,
            launches: 2,
            restarts: 1,
            shells: 1,
            pins: 1,
            hard_stops: 1,
            other_holds: 1,
            other_stops: 1,
            failures: 1,
            edits: 1,
        }
    }

    /// The bounds `just test` explores exhaustively. Every origin but one
    /// explores its whole world (2,515 states for the git worktree, 29,970
    /// for the hand-made tree, 59,940 for another host's, 74 for a folder,
    /// each well under a second). The wt tree this host makes explores its
    /// world in two halves, since the whole of it is nightly's: this one
    /// leaves out the person's pin and another host's hold, 4,054,913 states
    /// in 5.7 s on the Mac's 12 threads, and [`Self::holders`] has them.
    pub const fn ci(origin: Origin) -> Self {
        match origin {
            Origin::MadeWt => Self {
                pins: 0,
                other_holds: 0,
                ..Self::world(origin)
            },
            _ => Self::world(origin),
        }
    }

    /// The wt tree this host makes, held by everything that can hold it (a
    /// shell, a pin, another host's daemon) and stopped by a person, with no
    /// failures, edits or restarts: 1,824,217 states in 2.7 s on the Mac's
    /// 12 threads.
    pub const fn holders() -> Self {
        Self {
            failures: 0,
            edits: 0,
            restarts: 0,
            ..Self::world(Origin::MadeWt)
        }
    }

    /// The bounds a nightly run explores: the whole world for the wt tree
    /// this host makes, and larger bounds for every other origin (at most
    /// 2,927,232 states, another host's tree, in 8 s on the Mac's 12
    /// threads).
    pub const fn nightly(origin: Origin) -> Self {
        if matches!(origin, Origin::MadeWt) {
            return Self::world(origin);
        }
        Self {
            origin,
            agents: 3,
            launches: 3,
            restarts: 2,
            shells: 2,
            pins: 1,
            hard_stops: 1,
            other_holds: 2,
            other_stops: 1,
            failures: 2,
            edits: 2,
        }
    }

    fn key(&self) -> Key {
        Key {
            kind: self.origin.kind(),
            id: TREE.to_owned(),
        }
    }

    /// The core's key for the workspace now: a tree that no longer exists
    /// resolves as a folder at its path as written.
    fn resolved(&self, tree: &Tree) -> Key {
        if tree.exists {
            self.key()
        } else {
            Key {
                kind: Kind::Folder,
                id: TREE.to_owned(),
            }
        }
    }

    fn holds(&self) -> bool {
        self.origin.kind().holds()
    }

    /// Whether nothing more can happen: the world has done all it may.
    fn at_rest(&self, state: &State) -> bool {
        let mut actions = Vec::new();
        self.actions(state, &mut actions);
        actions.is_empty()
    }
}

fn member(index: u8) -> MemberId {
    MemberId([index + 1; 16])
}

/// The agent's parent: the second agent is the first's child.
fn parent(index: u8) -> Option<u8> {
    (index == 1).then_some(0)
}

/// Whether `index` descends from `ancestor`.
fn descends(index: u8, ancestor: u8) -> bool {
    let mut at = index;
    while let Some(up) = parent(at) {
        if up == ancestor {
            return true;
        }
        at = up;
    }
    false
}

fn plan() -> Plan {
    Plan {
        repository: "/source/repo".to_owned(),
        name: "fix".to_owned(),
        branch: "fix".to_owned(),
        from: None,
    }
}

/// What the model does or lets happen.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// The next agent is created in the workspace and launched: in a new
    /// workspace for the first agent of an origin this host makes.
    Create,
    /// A sleeping agent is resumed.
    Launch(u8),
    /// An awake agent suspends, is settled or exits.
    Sleep(u8),
    /// An agent's row is deleted.
    Delete {
        agent: u8,
        discard: bool,
    },
    /// The tree's files change: dirty, or committed clean.
    Edit,
    ShellOpens,
    ShellCloses,
    Pin,
    Unpin,
    /// A person's `wt stop`, whatever holds the tree.
    HardStop,
    OtherHolds,
    OtherReleases,
    /// Another host stops the tree it made, nothing holding it.
    OtherStops,
    CreateHookEnds {
        ok: bool,
    },
    MakeEnds {
        ok: bool,
    },
    StartEnds {
        ok: bool,
    },
    /// A stop-when-unheld call's wait ends.
    StopBegins,
    StopHookEnds {
        ok: bool,
    },
    /// The stop call's answer reaches the core.
    StopAnswered,
    /// A granted claim's answer reaches the core.
    ClaimAnswered,
    /// The backend runs a delete.
    DeleteRuns,
    /// The delete's answer reaches the core.
    DeleteAnswered,
    Restart,
}

/// Who a tree says made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Owner {
    Nobody,
    This,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Hook {
    Idle,
    Running,
    /// It failed, which the start after it reports.
    Failed,
}

/// The workspace on disk, as the backend sees it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Tree {
    exists: bool,
    /// Who it says made it now.
    owner: Owner,
    /// Who made it, whatever it says now.
    made_by: Owner,
    /// Its start hook ran after its stop hook.
    started: bool,
    dirty: bool,
    pinned: bool,
    shells: u8,
    other_hold: bool,
    create_hook: Hook,
    /// Times it was removed.
    removed: u8,
    /// A person's `wt stop` stopped it since its start hook last ran.
    hard_stopped: bool,
}

/// amux's claim, as the backend counts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Claim {
    None,
    /// Waiting for a stop hook that runs.
    Queued,
    Granted {
        answered: bool,
    },
}

/// The stop-when-unheld call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Waiter {
    Waiting,
    Stopping,
    /// Done; its answer is on the way.
    Answered {
        ok: bool,
    },
}

/// The jobs the core asked for that the backend has not finished.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Jobs {
    claim: Claim,
    make: bool,
    start: bool,
    waiter: Option<Waiter>,
    /// A delete, and whether it discards.
    delete: Option<bool>,
    /// A delete the backend ran, whose answer is on the way.
    deleted: Option<Removal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Awake {
    Asleep,
    /// The daemon is about to launch it.
    Launching,
    Running,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Agent {
    Unborn,
    Live {
        awake: Awake,
        /// It can be resumed: not marked could-not-start.
        startable: bool,
        /// It woke while a stop hook ran.
        woke_during_stop: bool,
    },
    Deleted,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Counts {
    created: u8,
    launches: u8,
    restarts: u8,
    shells: u8,
    pins: u8,
    hard_stops: u8,
    other_holds: u8,
    other_stops: u8,
    failures: u8,
    edits: u8,
}

/// What the properties judge, as it happened.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Seen {
    /// A check of the core's failed, or it asked for what its driver cannot
    /// do.
    broken: Option<String>,
    stopped_in_use: bool,
    launched_unclaimed: bool,
    launched_absent: bool,
    touched_not_made: bool,
    touched_others: bool,
    stop_rule: bool,
    delete_rule: bool,
    restarted: bool,
    stopped_awake_after_restart: bool,
    /// A member's delete gave consent, when every other member left was its
    /// descendant, since the workspace last gained a member, in this
    /// daemon's life.
    consent: bool,
    last_delete_discarded: bool,
    /// The delete outstanding discards though the member delete that
    /// issued it did not: an earlier member's consent stuck.
    sticky_discard: bool,
    /// A delete was refused only because amux's own stop hook ran, leaving
    /// a tree this host made on disk, nobody's.
    refused_for_own_stop: bool,
    /// The last member was deleted while amux's stop hook ran.
    deleted_during_stop: bool,
    /// The last awake member went in this daemon's life while the tree was
    /// this host's, and no pin has since kept it running.
    owed: bool,
    // What the sometimes properties look for.
    launched: bool,
    our_stop_ended: bool,
    started_after_stop: bool,
    woke_during_stop_launched: bool,
    dirty_kept: bool,
    consent_stuck: bool,
    removed_after_stop: bool,
    stop_waited_for_start: bool,
    adopted_claim: bool,
    woke_into_removed: bool,
}

/// The core, the tree, the backend's jobs and the agents.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    core: Workspaces,
    tree: Tree,
    jobs: Jobs,
    agents: Vec<Agent>,
    counts: Counts,
    seen: Seen,
}

impl State {
    fn awake(&self) -> usize {
        self.agents
            .iter()
            .filter(|agent| matches!(agent, Agent::Live { awake, .. } if *awake != Awake::Asleep))
            .count()
    }

    fn running(&self) -> usize {
        self.agents
            .iter()
            .filter(|agent| {
                matches!(
                    agent,
                    Agent::Live {
                        awake: Awake::Running,
                        ..
                    }
                )
            })
            .count()
    }

    fn live(&self) -> usize {
        self.agents
            .iter()
            .filter(|agent| matches!(agent, Agent::Live { .. }))
            .count()
    }

    fn set_awake(&mut self, index: u8, to: Awake) {
        if let Agent::Live { awake, .. } = &mut self.agents[usize::from(index)] {
            *awake = to;
        }
    }

    fn broke(&mut self, what: String) {
        self.seen.broken.get_or_insert(what);
    }
}

impl WorkspaceLifecycle {
    /// Steps the core on one input and performs what it asks, in order.
    fn feed(&self, state: &mut State, input: Input) {
        let mut out = Vec::new();
        state.core.step(input, &mut out);
        if let Err(broken) = state.core.check_invariants() {
            state.broke(broken.to_string());
        }
        for output in out {
            self.perform(state, output);
        }
    }

    /// A start, stop or delete of the tree the core asked for.
    fn touch(&self, state: &mut State) {
        if state.tree.made_by != Owner::This {
            state.seen.touched_not_made = true;
        }
        if state.tree.made_by == Owner::Other {
            state.seen.touched_others = true;
        }
    }

    fn perform(&self, state: &mut State, output: Output) {
        match output {
            Output::Make { .. } => {
                if state.jobs.make || state.tree.exists {
                    state.broke("a make of a tree that exists or is being made".to_owned());
                }
                state.jobs.make = true;
            }
            Output::TakeClaim { .. } => {
                if state.jobs.claim != Claim::None {
                    state.broke("a claim taken while one is held".to_owned());
                }
                state.jobs.claim = if state.jobs.waiter == Some(Waiter::Stopping) {
                    Claim::Queued
                } else {
                    Claim::Granted { answered: false }
                };
            }
            Output::ReleaseClaim { .. } => {
                if state.jobs.claim != (Claim::Granted { answered: true }) {
                    state.broke(format!("a release of a claim {:?}", state.jobs.claim));
                }
                state.jobs.claim = Claim::None;
            }
            Output::Start { .. } => {
                self.touch(state);
                if state.jobs.waiter.is_some() {
                    state.seen.stop_rule = true;
                }
                if state.jobs.start {
                    state.broke("a start beside a start".to_owned());
                }
                state.jobs.start = true;
            }
            Output::StopWhenUnheld { .. } => {
                self.touch(state);
                if state.jobs.waiter.is_some() || state.jobs.start {
                    state.seen.stop_rule = true;
                }
                state.jobs.waiter = Some(Waiter::Waiting);
            }
            Output::Delete { discard, .. } => {
                self.touch(state);
                // A claim of amux's own would make the backend refuse it.
                // A stop call outstanding may be running its hook, whose
                // exclusive hold would make the backend refuse it too.
                if state.jobs.delete.is_some()
                    || state.jobs.deleted.is_some()
                    || state.live() > 0
                    || state.jobs.claim != Claim::None
                    || state.jobs.waiter.is_some()
                    || (discard && !state.seen.consent)
                {
                    state.seen.delete_rule = true;
                }
                state.jobs.delete = Some(discard);
                state.seen.sticky_discard = discard && !state.seen.last_delete_discarded;
            }
            Output::Launch { member: launched } => {
                let Some(index) = (0..self.agents).find(|index| member(*index) == launched) else {
                    return state.broke(format!("a launch of {launched:?}"));
                };
                let agent = state.agents[usize::from(index)];
                let Agent::Live {
                    awake: Awake::Launching,
                    woke_during_stop,
                    ..
                } = agent
                else {
                    return state.broke(format!("a launch of agent {index}, {agent:?}"));
                };
                state.set_awake(index, Awake::Running);
                if woke_during_stop {
                    state.seen.woke_during_stop_launched = true;
                }
                if self.holds() && state.jobs.claim != (Claim::Granted { answered: true }) {
                    state.seen.launched_unclaimed = true;
                }
                if !state.tree.exists {
                    state.seen.launched_absent = true;
                }
                state.seen.launched = true;
            }
            Output::NotMade {
                member: refused, ..
            } => {
                if let Some(index) = (0..self.agents).find(|index| member(*index) == refused)
                    && let Agent::Live {
                        awake, startable, ..
                    } = &mut state.agents[usize::from(index)]
                {
                    if *awake == Awake::Launching && state.tree.removed > 0 && !state.tree.exists {
                        state.seen.woke_into_removed = true;
                    }
                    *awake = Awake::Asleep;
                    *startable = false;
                }
            }
            Output::Publish { .. } => {}
        }
    }

    /// An agent is about to launch.
    fn launching(&self, state: &mut State, index: u8) {
        let stopping = state.jobs.waiter == Some(Waiter::Stopping);
        if let Agent::Live {
            awake,
            woke_during_stop,
            ..
        } = &mut state.agents[usize::from(index)]
        {
            *awake = Awake::Launching;
            *woke_during_stop = stopping;
        }
        self.feed(
            state,
            Input::Launching {
                member: member(index),
            },
        );
    }

    fn can_create(&self, state: &State) -> bool {
        if state.counts.created >= self.agents || state.jobs.make {
            return false;
        }
        if self.origin.made_here() && state.counts.created == 0 {
            return true;
        }
        state.tree.exists
    }

    fn restart(&self, state: &mut State) {
        state.counts.restarts += 1;
        state.seen.restarted = true;
        state.seen.owed = false;
        state.seen.consent = false;
        // The daemon's holds go with its process, and so do its calls still
        // waiting; hooks wt has begun run to their end.
        state.jobs.claim = Claim::None;
        if std::mem::take(&mut state.jobs.start)
            && state.tree.create_hook != Hook::Running
            && state.tree.exists
        {
            state.tree.started = true;
        }
        if state.jobs.waiter.take() == Some(Waiter::Stopping) {
            state.tree.started = false;
        }
        state.jobs.delete = None;
        state.jobs.deleted = None;
        // A restarted daemon finds a removed tree's folder gone: its agents'
        // launches would fail.
        if !state.tree.exists {
            for agent in &mut state.agents {
                if let Agent::Live { startable, .. } = agent {
                    *startable = false;
                }
            }
        }
        // A make in flight leaves a half-made tree, and its agent a row with
        // no process the restarted daemon marks could-not-start.
        if std::mem::take(&mut state.jobs.make) {
            state.tree.exists = true;
            state.tree.owner = Owner::This;
            state.tree.create_hook = Hook::Idle;
            for agent in &mut state.agents {
                if let Agent::Live {
                    awake: Awake::Launching,
                    startable,
                    ..
                } = agent
                {
                    *startable = false;
                }
            }
        }
        // A launch the old daemon had not made is never made.
        for agent in &mut state.agents {
            if let Agent::Live { awake, .. } = agent
                && *awake == Awake::Launching
            {
                *awake = Awake::Asleep;
            }
        }
        state.core = Workspaces::new();
        let key = self.resolved(&state.tree);
        let made_here = state.tree.exists && state.tree.owner == Owner::This;
        for running in [true, false] {
            for index in 0..self.agents {
                if let Agent::Live { awake, .. } = state.agents[usize::from(index)]
                    && (awake == Awake::Running) == running
                {
                    self.feed(
                        state,
                        Input::Added {
                            member: member(index),
                            workspace: key.clone(),
                            made_here,
                            running,
                        },
                    );
                }
            }
        }
        if state.running() > 0 && state.jobs.claim != Claim::None {
            state.seen.adopted_claim = true;
        }
        let owned = if made_here {
            vec![self.key()]
        } else {
            Vec::new()
        };
        self.feed(state, Input::Swept { owned });
    }
}

impl Machine for WorkspaceLifecycle {
    type State = State;
    type Action = Action;

    fn init(&self) -> State {
        let (exists, owner) = match self.origin {
            Origin::MadeWt | Origin::MadeGit => (false, Owner::Nobody),
            Origin::HandWt | Origin::Folder => (true, Owner::Nobody),
            Origin::OtherWt => (true, Owner::Other),
        };
        State {
            core: Workspaces::new(),
            tree: Tree {
                exists,
                owner,
                made_by: owner,
                started: false,
                dirty: false,
                pinned: false,
                shells: 0,
                other_hold: false,
                create_hook: Hook::Idle,
                removed: 0,
                hard_stopped: false,
            },
            jobs: Jobs {
                claim: Claim::None,
                make: false,
                start: false,
                waiter: None,
                delete: None,
                deleted: None,
            },
            agents: vec![Agent::Unborn; usize::from(self.agents)],
            counts: Counts::default(),
            seen: Seen::default(),
        }
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        let can_fail = s.counts.failures < self.failures;
        let outcomes = |out: &mut Vec<Action>, make: fn(bool) -> Action| {
            out.push(make(true));
            if can_fail {
                out.push(make(false));
            }
        };
        if self.can_create(s) {
            out.push(Action::Create);
        }
        // A stop or delete while the workspace is being made waits in the
        // agent's lane until making ends.
        if !s.jobs.make {
            for (index, agent) in (0..self.agents).zip(&s.agents) {
                let Agent::Live {
                    awake, startable, ..
                } = agent
                else {
                    continue;
                };
                match awake {
                    Awake::Asleep => {
                        if *startable && s.counts.launches < self.launches {
                            out.push(Action::Launch(index));
                        }
                    }
                    Awake::Launching | Awake::Running => out.push(Action::Sleep(index)),
                }
                out.push(Action::Delete {
                    agent: index,
                    discard: false,
                });
                if self.origin.made_here() {
                    out.push(Action::Delete {
                        agent: index,
                        discard: true,
                    });
                }
            }
        }
        let tree = &s.tree;
        if tree.exists && s.counts.edits < self.edits && self.origin.kind().makes() {
            out.push(Action::Edit);
        }
        if self.holds() && tree.exists {
            if s.counts.shells < self.shells && s.jobs.waiter != Some(Waiter::Stopping) {
                out.push(Action::ShellOpens);
            }
            if tree.shells > 0 {
                out.push(Action::ShellCloses);
            }
            if !tree.pinned && s.counts.pins < self.pins {
                out.push(Action::Pin);
            }
            if tree.pinned {
                out.push(Action::Unpin);
            }
            if tree.started && s.counts.hard_stops < self.hard_stops {
                out.push(Action::HardStop);
            }
            if !tree.other_hold && s.counts.other_holds < self.other_holds {
                out.push(Action::OtherHolds);
            }
            if tree.other_hold {
                out.push(Action::OtherReleases);
            }
            if self.origin == Origin::OtherWt
                && tree.started
                && s.jobs.claim == Claim::None
                && tree.shells == 0
                && !tree.pinned
                && !tree.other_hold
                && s.counts.other_stops < self.other_stops
            {
                out.push(Action::OtherStops);
            }
        }
        if tree.create_hook == Hook::Running {
            outcomes(out, |ok| Action::CreateHookEnds { ok });
        }
        if s.jobs.make {
            outcomes(out, |ok| Action::MakeEnds { ok });
        }
        if s.jobs.start
            && tree.create_hook != Hook::Running
            && s.jobs.waiter != Some(Waiter::Stopping)
        {
            outcomes(out, |ok| Action::StartEnds { ok });
        }
        match s.jobs.waiter {
            Some(Waiter::Waiting)
                if s.jobs.claim == Claim::None
                    && tree.shells == 0
                    && !tree.other_hold
                    && tree.create_hook != Hook::Running =>
            {
                out.push(Action::StopBegins);
            }
            Some(Waiter::Stopping) => outcomes(out, |ok| Action::StopHookEnds { ok }),
            Some(Waiter::Answered { .. }) => out.push(Action::StopAnswered),
            _ => {}
        }
        if s.jobs.claim == (Claim::Granted { answered: false }) {
            out.push(Action::ClaimAnswered);
        }
        if s.jobs.delete.is_some() {
            out.push(Action::DeleteRuns);
        }
        if s.jobs.deleted.is_some() {
            out.push(Action::DeleteAnswered);
        }
        if s.counts.restarts < self.restarts {
            out.push(Action::Restart);
        }
    }

    fn next(&self, s: &State, action: &Action) -> Option<State> {
        let mut state = s.clone();
        let awake_before = s.awake();
        match action {
            Action::Create => {
                let index = state.counts.created;
                state.counts.created += 1;
                state.agents[usize::from(index)] = Agent::Live {
                    awake: Awake::Asleep,
                    startable: true,
                    woke_during_stop: false,
                };
                state.seen.consent = false;
                if self.origin.made_here() && index == 0 {
                    state.tree.made_by = Owner::This;
                    self.feed(
                        &mut state,
                        Input::Create {
                            member: member(index),
                            workspace: self.key(),
                            plan: plan(),
                        },
                    );
                } else {
                    let made_here = state.tree.owner == Owner::This;
                    self.feed(
                        &mut state,
                        Input::Added {
                            member: member(index),
                            workspace: self.key(),
                            made_here,
                            running: false,
                        },
                    );
                }
                self.launching(&mut state, index);
            }
            Action::Launch(index) => {
                state.counts.launches += 1;
                self.launching(&mut state, *index);
            }
            Action::Sleep(index) => {
                state.set_awake(*index, Awake::Asleep);
                self.feed(
                    &mut state,
                    Input::Slept {
                        member: member(*index),
                    },
                );
            }
            Action::Delete { agent, discard } => {
                state.agents[usize::from(*agent)] = Agent::Deleted;
                let descendants: Vec<u8> = (0..self.agents)
                    .filter(|index| {
                        matches!(state.agents[usize::from(*index)], Agent::Live { .. })
                            && descends(*index, *agent)
                    })
                    .collect();
                // Consent covers the changes of the members the delete takes
                // with it, and no one else's.
                let covers = state.live() == descendants.len();
                state.seen.consent |= *discard && covers;
                state.seen.last_delete_discarded = *discard && covers;
                self.feed(
                    &mut state,
                    Input::Removed {
                        member: member(*agent),
                        discard: *discard,
                        descendants: descendants.into_iter().map(member).collect(),
                    },
                );
                if state.live() == 0 && state.jobs.waiter == Some(Waiter::Stopping) {
                    state.seen.deleted_during_stop = true;
                }
            }
            Action::Edit => {
                state.counts.edits += 1;
                state.tree.dirty = !state.tree.dirty;
            }
            Action::ShellOpens => {
                state.counts.shells += 1;
                state.tree.shells += 1;
            }
            Action::ShellCloses => state.tree.shells -= 1,
            Action::Pin => {
                // `wt start` pins the tree and starts it.
                state.counts.pins += 1;
                state.tree.pinned = true;
                state.tree.started = true;
            }
            Action::Unpin => state.tree.pinned = false,
            Action::HardStop => {
                state.counts.hard_stops += 1;
                state.tree.started = false;
                state.tree.hard_stopped = true;
            }
            Action::OtherHolds => {
                state.counts.other_holds += 1;
                state.tree.other_hold = true;
                // Its own member wakes in a tree it made, and starts it.
                if self.origin == Origin::OtherWt {
                    state.tree.started = true;
                }
            }
            Action::OtherReleases => state.tree.other_hold = false,
            Action::OtherStops => {
                state.counts.other_stops += 1;
                state.tree.started = false;
            }
            Action::CreateHookEnds { ok } => {
                if !ok {
                    state.counts.failures += 1;
                }
                state.tree.create_hook = if *ok { Hook::Idle } else { Hook::Failed };
            }
            Action::MakeEnds { ok } => {
                state.jobs.make = false;
                let result = if *ok {
                    state.tree.exists = true;
                    state.tree.owner = Owner::This;
                    state.tree.create_hook = Hook::Running;
                    Ok(())
                } else {
                    state.counts.failures += 1;
                    Err("the branch is taken on origin".to_owned())
                };
                self.feed(
                    &mut state,
                    Input::Made {
                        workspace: self.key(),
                        result,
                    },
                );
            }
            Action::StartEnds { ok } => {
                state.jobs.start = false;
                if !ok {
                    state.counts.failures += 1;
                }
                let failed = !ok || state.tree.create_hook == Hook::Failed;
                if state.tree.create_hook == Hook::Failed {
                    state.tree.create_hook = Hook::Idle;
                }
                // It counts as started after a failure.
                if state.tree.exists {
                    state.tree.started = true;
                    state.tree.hard_stopped = false;
                    if state.seen.our_stop_ended {
                        state.seen.started_after_stop = true;
                    }
                }
                let result = if failed {
                    Err("the start hook exited 1".to_owned())
                } else {
                    Ok(())
                };
                self.feed(
                    &mut state,
                    Input::Started {
                        workspace: self.key(),
                        result,
                    },
                );
                if state.jobs.waiter == Some(Waiter::Waiting) {
                    state.seen.stop_waited_for_start = true;
                }
            }
            Action::StopBegins => {
                if state.tree.pinned {
                    // The pin keeps it running: a person's choice.
                    state.jobs.waiter = None;
                    state.seen.owed = false;
                    self.feed(
                        &mut state,
                        Input::Stopped {
                            workspace: self.key(),
                            result: Ok(()),
                        },
                    );
                } else {
                    let awake = state.awake();
                    if awake > 0 || state.tree.shells > 0 {
                        state.seen.stopped_in_use = true;
                    }
                    if awake > 0 {
                        state.seen.stop_rule = true;
                        if state.seen.restarted {
                            state.seen.stopped_awake_after_restart = true;
                        }
                    }
                    if state.tree.other_hold {
                        state.seen.touched_others = true;
                    }
                    state.jobs.waiter = Some(Waiter::Stopping);
                }
            }
            Action::StopHookEnds { ok } => {
                if !ok {
                    state.counts.failures += 1;
                }
                // It counts as stopped after a failure.
                state.tree.started = false;
                state.seen.our_stop_ended = true;
                if state.running() > 0 {
                    state.seen.stop_rule = true;
                }
                state.jobs.waiter = Some(Waiter::Answered { ok: *ok });
                if state.jobs.claim == Claim::Queued {
                    state.jobs.claim = Claim::Granted { answered: false };
                }
            }
            Action::StopAnswered => {
                let Some(Waiter::Answered { ok }) = state.jobs.waiter.take() else {
                    return None;
                };
                let result = if ok {
                    Ok(())
                } else {
                    Err("the stop hook timed out".to_owned())
                };
                self.feed(
                    &mut state,
                    Input::Stopped {
                        workspace: self.key(),
                        result,
                    },
                );
            }
            Action::ClaimAnswered => {
                state.jobs.claim = Claim::Granted { answered: true };
                self.feed(
                    &mut state,
                    Input::Claimed {
                        workspace: self.key(),
                        result: Ok(()),
                    },
                );
            }
            Action::DeleteRuns => {
                let discard = state.jobs.delete.take()?;
                let tree = &mut state.tree;
                let held = tree.shells > 0 || tree.other_hold || tree.pinned;
                let stopping = state.jobs.waiter == Some(Waiter::Stopping);
                let refused =
                    held || stopping || state.jobs.claim != Claim::None || (tree.dirty && !discard);
                let removal = if !tree.exists {
                    Removal::Removed
                } else if refused {
                    if stopping
                        && !held
                        && state.jobs.claim == Claim::None
                        && !(tree.dirty && !discard)
                        && tree.owner == Owner::This
                    {
                        state.seen.refused_for_own_stop = true;
                    }
                    tree.owner = Owner::Nobody;
                    if tree.dirty && !discard && !held && !stopping {
                        state.seen.dirty_kept = true;
                    }
                    Removal::Kept
                } else {
                    if tree.dirty && discard && state.seen.sticky_discard {
                        state.seen.consent_stuck = true;
                    }
                    if state.seen.deleted_during_stop {
                        state.seen.removed_after_stop = true;
                    }
                    tree.exists = false;
                    tree.owner = Owner::Nobody;
                    tree.started = false;
                    tree.create_hook = Hook::Idle;
                    tree.removed += 1;
                    Removal::Removed
                };
                state.jobs.deleted = Some(removal);
            }
            Action::DeleteAnswered => {
                let removal = state.jobs.deleted.take()?;
                self.feed(
                    &mut state,
                    Input::Deleted {
                        workspace: self.key(),
                        removal,
                    },
                );
                // Its agents' folder is gone, which the daemon now knows: a
                // launch of one would fail.
                if removal == Removal::Removed {
                    for agent in &mut state.agents {
                        if let Agent::Live { startable, .. } = agent {
                            *startable = false;
                        }
                    }
                }
            }
            Action::Restart => self.restart(&mut state),
        }
        // The stop a workspace is owed: its last awake member went while it
        // was this host's.
        let awake_after = state.awake();
        if awake_after > 0 {
            state.seen.owed = false;
        } else if awake_before > 0 && state.tree.owner == Owner::This && *action != Action::Restart
        {
            state.seen.owed = true;
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(
                "the workspace core's own checks hold after every step, and its driver can do \
                 everything it asks",
                |_, s: &State| s.seen.broken.is_none(),
            ),
            Property::always(
                "amux never stops a workspace while it has an awake member, a shell hold or a pin",
                |_, s: &State| !s.seen.stopped_in_use,
            ),
            Property::always(
                "a member launches only while amux's claim is held, and only in a workspace that \
                 exists",
                |_, s: &State| !s.seen.launched_unclaimed && !s.seen.launched_absent,
            ),
            Property::always(
                "amux never starts, stops or removes a workspace it didn't make",
                |_, s: &State| !s.seen.touched_not_made,
            ),
            Property::always(
                "at most one stop-when-unheld is outstanding per workspace, none while this host's \
                 start job for it runs, and none completes while this host has an awake member there",
                |_, s: &State| !s.seen.stop_rule && !(s.jobs.waiter.is_some() && s.jobs.start),
            ),
            Property::always(
                "a workspace is removed at most once, only after its last member and once no stop \
                 call is outstanding, and with its changes discarded only once a member's delete \
                 gave consent while every other member left was its descendant; a dirty \
                 workspace without consent is kept and disowned",
                |_, s: &State| !s.seen.delete_rule && s.tree.removed <= 1,
            ),
            Property::always(
                "a delete is never refused for amux's own stop hook, which would leave a tree this \
                 host made on disk, nobody's",
                |_, s: &State| !s.seen.refused_for_own_stop,
            ),
            Property::always(
                "a host never starts, stops or removes a workspace another host made, and never \
                 stops one while another holder uses it",
                |_, s: &State| !s.seen.touched_others,
            ),
            Property::always(
                "after a daemon restart, no workspace stops while a member is awake",
                |_, s: &State| !s.seen.stopped_awake_after_restart,
            ),
            Property::eventually(
                "a workspace this host made, with no members awake, no shell hold and no pin, and \
                 nothing pending, has stopped, provided it had a stop waiting when it last lost a \
                 member",
                |model, s: &State| {
                    let owed = s.seen.owed
                        && s.tree.exists
                        && s.tree.owner == Owner::This
                        && model.origin.kind().hooks()
                        && s.awake() == 0
                        && s.tree.shells == 0
                        && !s.tree.pinned
                        && !s.tree.other_hold;
                    model.at_rest(s) && (!owed || !s.tree.started)
                },
            ),
            Property::always(
                "a member running in a workspace this host made finds it started whenever the \
                 backend has nothing left to do, unless a person stopped it",
                |model, s: &State| {
                    let idle = !s.jobs.make
                        && !s.jobs.start
                        && s.jobs.waiter.is_none()
                        && s.jobs.delete.is_none()
                        && s.jobs.deleted.is_none()
                        && s.jobs.claim != (Claim::Granted { answered: false })
                        && s.tree.create_hook != Hook::Running;
                    let unstarted = s.running() > 0
                        && s.tree.exists
                        && s.tree.owner == Owner::This
                        && model.origin.kind().hooks()
                        && !s.tree.hard_stopped
                        && !s.tree.started;
                    !(idle && unstarted)
                },
            ),
            Property::sometimes("a member launches", |_, s: &State| s.seen.launched),
            Property::sometimes(
                "a member that wakes as its tree is removed is told it cannot launch",
                |model, s: &State| !model.origin.made_here() || s.seen.woke_into_removed,
            ),
            Property::sometimes(
                "a workspace stops once its last member sleeps and starts again when one wakes, as \
                 a finished child's does on a follow-up",
                |model, s: &State| model.origin != Origin::MadeWt || s.seen.started_after_stop,
            ),
            Property::sometimes(
                "a member wakes during a stop and launches after it",
                |model, s: &State| {
                    model.origin != Origin::MadeWt || s.seen.woke_during_stop_launched
                },
            ),
            Property::sometimes(
                "a person's shell keeps a workspace running after its last member suspends",
                |model, s: &State| {
                    model.origin != Origin::MadeWt
                        || (s.tree.started
                            && s.tree.shells > 0
                            && s.live() > 0
                            && s.awake() == 0
                            && s.jobs.waiter == Some(Waiter::Waiting))
                },
            ),
            Property::sometimes(
                "a stop waits for this host's start job and follows it",
                |model, s: &State| model.origin != Origin::MadeWt || s.seen.stop_waited_for_start,
            ),
            Property::sometimes(
                "a dirty workspace is left on disk by a delete without discard",
                |model, s: &State| {
                    !model.origin.made_here() || model.edits == 0 || s.seen.dirty_kept
                },
            ),
            Property::sometimes(
                "a delete of the last member while amux's stop hook runs removes the tree once \
                 the stop is answered",
                |model, s: &State| model.origin != Origin::MadeWt || s.seen.removed_after_stop,
            ),
            Property::sometimes(
                "consent sticks: a dirty workspace is removed with its changes by its last \
                 member's delete without consent, after its parent's delete gave it",
                |model, s: &State| {
                    !model.origin.made_here() || model.edits == 0 || s.seen.consent_stuck
                },
            ),
            Property::sometimes(
                "a restarted daemon retakes the claim on a tree for a member it adopts running",
                |model, s: &State| !model.holds() || model.restarts == 0 || s.seen.adopted_claim,
            ),
        ]
    }
}
