//! The supervisor's updates: installing a release, activating it, rolling a
//! bad one back, and restarting the daemon through crashes of either
//! process.
//!
//! The supervisor is the real `supervisor_core::Supervisor`, held between
//! steps as its value. The world around it is what is true on the machine:
//! the binary at the install path, prev beside it, a staged download, the
//! rejected-build record, the daemon processes and what the release feed
//! publishes. What its driver does is the environment too: it performs the
//! core's outputs in order, answers a look at the feed and a download at
//! once, starts a daemon from whatever binary is at the path, kills a daemon
//! at once, and when the core hands over, starts the installed binary as a
//! supervisor that inherits the serving daemon. The supervisor may crash
//! before any file output of a step is performed, and between the two moves
//! an install or a rollback makes, so every prefix of a swap or a rollback
//! is explored; launchd starts it again at once from the binary at the
//! path, and its daemons exit as the activation pipe closes. The moves are
//! the platform's: where a running binary can be renamed over, an install
//! links prev to the binary and renames the staged one over the path; where
//! it cannot (Windows), it renames the binary to prev and the staged one
//! into the empty path, and a rollback over its own running image renames
//! that aside first.
//! A release is good or bad: a daemon from a bad build never prepares. Time
//! moves only when a timer the core armed fires, or the feed is looked at.

use std::collections::{BTreeMap, VecDeque};

use release::{Manifest, Release};
use supervisor_core::{
    DaemonId, INHERITED_DAEMON, Input, JobId, JobResult, Listed, Output, Params, PrevFound,
    Request, Start, Supervisor, TimerId, Version,
};

use crate::{Machine, Property};

const TARGET: &str = "model-target";
const START_MS: u64 = 1_000;

/// The supervisor's updates, within their bounds.
#[derive(Clone, Debug)]
pub struct SupervisorUpdate {
    /// Releases the feed publishes, each newer than the last, good or bad.
    pub releases: u8,
    /// Times the supervisor crashes.
    pub supervisor_crashes: u8,
    /// Times a daemon crashes.
    pub daemon_crashes: u8,
    /// Times a daemon cannot be started at all.
    pub start_failures: u8,
    /// Timers armed for later that fire, all told: a restart's backoff, a
    /// start deadline, a stop deadline.
    pub waits: u8,
    /// Times `amux update` asks for a look.
    pub updates: u8,
    /// A running binary may be renamed over (not on Windows).
    pub replace_while_running: bool,
}

impl SupervisorUpdate {
    /// The bounds `just test` explores exhaustively: 5,819,060 states in
    /// 8 s on 4 threads of an M-series Mac.
    pub const fn ci() -> Self {
        Self {
            releases: 2,
            supervisor_crashes: 2,
            daemon_crashes: 2,
            start_failures: 1,
            waits: 5,
            updates: 1,
            replace_while_running: true,
        }
    }

    /// The same bounds where a running binary cannot be replaced, so the
    /// daemon stops before the swap.
    pub const fn windows() -> Self {
        Self {
            replace_while_running: false,
            ..Self::ci()
        }
    }

    /// The larger bounds a nightly run explores.
    pub const fn nightly(replace_while_running: bool) -> Self {
        Self {
            releases: 3,
            supervisor_crashes: 2,
            daemon_crashes: 2,
            start_failures: 2,
            waits: 6,
            updates: 1,
            replace_while_running,
        }
    }

    fn params(&self) -> Params {
        Params {
            check_interval_ms: 3_600_000,
            rollback_after: 2,
            start_deadline_ms: 60_000,
            stop_deadline_ms: 30_000,
            backoff_first_ms: 1_000,
            backoff_max_ms: 4_000,
            backoff_reset_ms: 60_000,
            replace_while_running: self.replace_while_running,
        }
    }
}

/// A build of amux: its major version, and whether a daemon from it can
/// start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Build {
    major: u64,
    good: bool,
}

impl Build {
    const FIRST: Build = Build {
        major: 1,
        good: true,
    };

    fn version(self) -> Version {
        Version::new(self.major, 0, 0)
    }
}

/// What the model does or lets happen.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// The feed publishes a newer release, good or bad, and the supervisor's
    /// hourly look comes due.
    ReleaseListed { good: bool },
    /// The daemon migrated and looked without writing, and wrote `prepared`.
    Prepare(DaemonId),
    /// The `go` the supervisor wrote reached the daemon.
    Go(DaemonId),
    /// The daemon could not be started.
    StartFailed(DaemonId),
    /// A daemon asked to stop exits.
    DaemonStops(DaemonId),
    /// A daemon dies.
    CrashDaemon(DaemonId),
    /// A timer the core armed fires.
    Timer(TimerId),
    /// `amux update` asks for a look now.
    Update,
    /// The driver performs the next file output of the step under way.
    Perform,
    /// The supervisor dies; launchd starts it again from the binary at the
    /// path.
    CrashSupervisor,
}

/// A daemon process, as the world sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Daemon {
    build: Build,
    prepared: bool,
    /// It was told `go`: it may write.
    told_go: bool,
    /// The driver has fed the core that `go` was written.
    activated: bool,
    stopping: bool,
}

/// What is true on the machine.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct World {
    path: Option<Build>,
    prev: Option<Build>,
    /// prev is a hard link to the binary at the path: an install that
    /// linked prev and has not renamed the staged binary over the path.
    prev_linked: bool,
    /// The path is empty between a Windows install's or rollback's two
    /// renames.
    renaming: bool,
    staged: Option<Build>,
    rejected: Option<Version>,
    /// What the feed publishes.
    published: Option<Build>,
    daemons: BTreeMap<DaemonId, Daemon>,
    /// The binary at the path before the newest release was installed: the
    /// one a rollback must put back.
    before_update: Option<Build>,
}

/// One of the core's outputs not yet performed: what the driver needs of
/// it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Pending {
    Spawn(DaemonId),
    Go(DaemonId),
    Stop(DaemonId),
    Kill(DaemonId),
    List(JobId),
    Fetch(JobId),
    RemovePrev,
    RetirePrev,
    WriteRejected(Version),
    RestorePrev,
    /// A Windows rollback's second rename: prev into the path its own
    /// image was renamed away from.
    RestorePrevInto,
    InstallStaged,
    /// An install's second move: the staged binary renamed over the path,
    /// or into it where the first move emptied it.
    InstallStagedOver,
    BecomeInstalled(DaemonId),
    Arm(TimerId, u64),
    Cancel(TimerId),
    /// The supervisor ends: done, or unable to go on.
    Ends,
    Dump(String),
}

impl Pending {
    fn of(output: Output) -> Option<Self> {
        Some(match output {
            Output::Spawn { daemon } => Pending::Spawn(daemon),
            Output::Go { daemon } => Pending::Go(daemon),
            Output::Stop { daemon } => Pending::Stop(daemon),
            Output::Kill { daemon } => Pending::Kill(daemon),
            Output::List { job, .. } => Pending::List(job),
            Output::Fetch { job, .. } => Pending::Fetch(job),
            Output::RemovePrev => Pending::RemovePrev,
            Output::RetirePrev => Pending::RetirePrev,
            Output::WriteRejected { version } => Pending::WriteRejected(version),
            Output::RestorePrev => Pending::RestorePrev,
            Output::InstallStaged => Pending::InstallStaged,
            Output::BecomeInstalled { daemon } => Pending::BecomeInstalled(daemon),
            Output::ArmTimer { id, at_ms } => Pending::Arm(id, at_ms),
            Output::CancelTimer { id } => Pending::Cancel(id),
            Output::Exit | Output::Fail { .. } => Pending::Ends,
            Output::Dump { reason } => Pending::Dump(format!("{reason:?}")),
            Output::Answer { .. } => return None,
        })
    }

    /// Performed inline and durably on the files beside the binary: the
    /// supervisor may die before any of them.
    fn is_file(&self) -> bool {
        matches!(
            self,
            Pending::RemovePrev
                | Pending::RetirePrev
                | Pending::WriteRejected(_)
                | Pending::RestorePrev
                | Pending::RestorePrevInto
                | Pending::InstallStaged
                | Pending::InstallStagedOver
        )
    }
}

/// What the properties judge, as it happened.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Seen {
    /// A daemon was told `go` before it prepared.
    go_before_prepared: Option<DaemonId>,
    /// A daemon was started while another still lived.
    started_beside: Option<DaemonId>,
    /// A binary was installed at the path while a daemon ran, where a
    /// running binary cannot be replaced.
    installed_under: Option<String>,
    /// A rollback put back something other than the binary from before the
    /// update, or recorded another build as rejected.
    wrong_rollback: Option<String>,
    rolled_back: bool,
    /// A supervisor started to find prev linked to its own binary, and
    /// removed it.
    removed_own_prev: bool,
    dumped: Option<String>,
}

/// What the world has spent of its bounds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Used {
    releases: u8,
    supervisor_crashes: u8,
    daemon_crashes: u8,
    start_failures: u8,
    waits: u8,
    updates: u8,
}

/// The supervisor, the world and the step under way.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    supervisor: Supervisor,
    /// The build the running supervisor was started from.
    image: Build,
    /// Nothing is at the path, so the supervisor could not be started
    /// again: nothing runs.
    stranded: bool,
    world: World,
    timers: BTreeMap<TimerId, u64>,
    now_ms: u64,
    /// The outputs of the step under way still to perform, in order.
    pending: VecDeque<Pending>,
    used: Used,
    seen: Seen,
}

impl State {
    /// Daemons told `go` that still live.
    fn serving(&self) -> usize {
        self.world
            .daemons
            .values()
            .filter(|daemon| daemon.told_go)
            .count()
    }
}

impl SupervisorUpdate {
    /// A supervisor started from the binary at the path: from nothing, or
    /// exec'd by the one before with its serving daemon.
    fn supervisor(
        &self,
        world: &World,
        inherited: bool,
        now_ms: u64,
    ) -> Option<(Supervisor, Vec<Output>)> {
        let running = world.path?.version();
        let prev = match (world.prev, world.prev_linked) {
            (None, _) => PrevFound::None,
            (Some(_), true) => PrevFound::SameAsBinary,
            (Some(_), false) => PrevFound::Other,
        };
        let mut out = Vec::new();
        let supervisor = Supervisor::start(
            Start {
                params: self.params(),
                target: TARGET.to_owned(),
                running,
                prev,
                rejected: world.rejected.clone(),
                inherited,
                now_ms,
            },
            &mut out,
        );
        Some((supervisor, out))
    }

    /// The supervisor's process is gone, and its daemons with their pipe;
    /// launchd starts it again from the binary at the path, unless nothing
    /// is there.
    fn restart(&self, state: &mut State) -> Option<()> {
        state.world.daemons.clear();
        state.timers.clear();
        state.pending.clear();
        let Some((supervisor, out)) = self.supervisor(&state.world, false, state.now_ms) else {
            state.stranded = true;
            return Some(());
        };
        state.supervisor = supervisor;
        state.image = state.world.path?;
        state
            .pending
            .extend(out.into_iter().filter_map(Pending::of));
        Some(())
    }

    /// One input to the core with the clock before it; its outputs join
    /// the step under way.
    fn feed(&self, state: &mut State, input: Input) {
        let mut out = Vec::new();
        state.supervisor.step(
            Input::Tick {
                now_ms: state.now_ms,
            },
            &mut out,
        );
        state.supervisor.step(input, &mut out);
        state
            .pending
            .extend(out.into_iter().filter_map(Pending::of));
    }

    /// Performs outputs until the next is a file output, before which the
    /// supervisor may die.
    fn drain(&self, state: &mut State) -> Option<()> {
        while state.pending.front().is_some_and(|next| !next.is_file()) {
            let next = state.pending.pop_front().expect("a pending output");
            self.perform(state, next)?;
        }
        Some(())
    }

    fn perform(&self, state: &mut State, output: Pending) -> Option<()> {
        let world = &mut state.world;
        match output {
            Pending::Spawn(daemon) => {
                if !world.daemons.is_empty() {
                    state.seen.started_beside.get_or_insert(daemon);
                }
                match world.path {
                    Some(build) => {
                        world.daemons.insert(
                            daemon,
                            Daemon {
                                build,
                                prepared: false,
                                told_go: false,
                                activated: false,
                                stopping: false,
                            },
                        );
                    }
                    None => self.feed(
                        state,
                        Input::StartFailed {
                            daemon,
                            why: "no binary at the path".into(),
                        },
                    ),
                }
            }
            Pending::Go(daemon) => {
                if let Some(running) = world.daemons.get_mut(&daemon) {
                    if !running.prepared {
                        state.seen.go_before_prepared.get_or_insert(daemon);
                    }
                    running.told_go = true;
                }
            }
            Pending::Stop(daemon) => {
                if let Some(running) = world.daemons.get_mut(&daemon) {
                    running.stopping = true;
                }
            }
            Pending::Kill(daemon) => {
                if world.daemons.remove(&daemon).is_some() {
                    self.feed(state, Input::DaemonExited { daemon, code: -9 });
                }
            }
            Pending::List(job) => {
                let listed = match world.published {
                    Some(build) => Listed::Manifest {
                        manifest: manifest(build),
                        host: None,
                    },
                    None => Listed::Nothing("nothing is published".into()),
                };
                self.feed(
                    state,
                    Input::JobDone {
                        job,
                        result: JobResult::Listed(listed),
                    },
                );
            }
            Pending::Fetch(job) => {
                world.staged = world.published;
                self.feed(
                    state,
                    Input::JobDone {
                        job,
                        result: JobResult::Fetched(Ok(())),
                    },
                );
            }
            Pending::RemovePrev | Pending::RetirePrev => {
                if world.prev_linked {
                    state.seen.removed_own_prev = true;
                }
                world.prev = None;
                world.prev_linked = false;
            }
            Pending::WriteRejected(version) => {
                if world.path.map(Build::version) != Some(version.clone()) {
                    state.seen.wrong_rollback.get_or_insert(format!(
                        "recorded {version} as rejected with {:?} at the path",
                        world.path
                    ));
                }
                world.rejected = Some(version);
            }
            // Where the binary at the path is this supervisor's own running
            // image and cannot be replaced, it is renamed aside first.
            Pending::RestorePrev
                if !self.replace_while_running && world.path == Some(state.image) =>
            {
                world.path = None;
                world.renaming = true;
                state.pending.push_front(Pending::RestorePrevInto);
            }
            Pending::RestorePrev | Pending::RestorePrevInto => {
                world.renaming = false;
                world.prev_linked = false;
                if world.prev != world.before_update {
                    state.seen.wrong_rollback.get_or_insert(format!(
                        "put back {:?} where {:?} ran before the update",
                        world.prev, world.before_update
                    ));
                }
                world.path = world.prev.take();
                state.seen.rolled_back = true;
            }
            Pending::InstallStaged => {
                if !self.replace_while_running && !world.daemons.is_empty() {
                    state.seen.installed_under.get_or_insert(format!(
                        "installed {:?} while {:?} ran",
                        world.staged,
                        world.daemons.keys().collect::<Vec<_>>()
                    ));
                }
                world.before_update = world.path;
                world.prev = world.path;
                if self.replace_while_running {
                    // prev is a hard link to the binary at the path.
                    world.prev_linked = true;
                } else {
                    // The binary is renamed to prev, emptying the path.
                    world.path = None;
                    world.renaming = true;
                }
                state.pending.push_front(Pending::InstallStagedOver);
            }
            Pending::InstallStagedOver => {
                world.prev_linked = false;
                world.renaming = false;
                world.path = world.staged.take();
            }
            Pending::BecomeInstalled(daemon) => {
                // The installed binary takes over this process, keeping the
                // daemon and its pipe; what this process had armed is gone.
                let running = world.daemons.remove(&daemon)?;
                world.daemons.insert(INHERITED_DAEMON, running);
                state.timers.clear();
                state.pending.clear();
                let (supervisor, out) = self.supervisor(&state.world, true, state.now_ms)?;
                state.supervisor = supervisor;
                state.image = state.world.path?;
                state
                    .pending
                    .extend(out.into_iter().filter_map(Pending::of));
            }
            Pending::Arm(id, at_ms) => {
                state.timers.insert(id, at_ms);
            }
            Pending::Cancel(id) => {
                state.timers.remove(&id);
            }
            Pending::Ends => self.restart(state)?,
            Pending::Dump(reason) => {
                state.seen.dumped.get_or_insert(reason);
            }
        }
        Some(())
    }
}

fn manifest(build: Build) -> Manifest {
    Manifest {
        channel: "stable".into(),
        rollout: None,
        targets: BTreeMap::from([(
            TARGET.to_owned(),
            Release {
                version: build.version().to_string(),
                url: format!("https://releases.invalid/amux-{}", build.major),
                sha256: String::new(),
                size: 1,
            },
        )]),
        signature: String::new(),
    }
}

impl Machine for SupervisorUpdate {
    type State = State;
    type Action = Action;

    fn init(&self) -> State {
        let world = World {
            path: Some(Build::FIRST),
            prev: None,
            prev_linked: false,
            renaming: false,
            staged: None,
            rejected: None,
            published: None,
            daemons: BTreeMap::new(),
            before_update: None,
        };
        let (supervisor, out) = self
            .supervisor(&world, false, START_MS)
            .expect("the first binary is in place");
        let mut state = State {
            supervisor,
            image: Build::FIRST,
            stranded: false,
            world,
            timers: BTreeMap::new(),
            now_ms: START_MS,
            pending: out.into_iter().filter_map(Pending::of).collect(),
            used: Used::default(),
            seen: Seen::default(),
        };
        self.drain(&mut state).expect("the first start performs");
        state
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        if s.stranded {
            return;
        }
        if s.used.supervisor_crashes < self.supervisor_crashes {
            out.push(Action::CrashSupervisor);
        }
        if !s.pending.is_empty() {
            out.push(Action::Perform);
            return;
        }
        if s.used.releases < self.releases {
            out.push(Action::ReleaseListed { good: true });
            out.push(Action::ReleaseListed { good: false });
        }
        for (id, daemon) in &s.world.daemons {
            if daemon.build.good && !daemon.prepared && !daemon.stopping {
                out.push(Action::Prepare(*id));
            }
            if daemon.told_go && !daemon.activated {
                out.push(Action::Go(*id));
            }
            if !daemon.prepared && !daemon.stopping && s.used.start_failures < self.start_failures {
                out.push(Action::StartFailed(*id));
            }
            if daemon.stopping {
                out.push(Action::DaemonStops(*id));
            }
            if s.used.daemon_crashes < self.daemon_crashes {
                out.push(Action::CrashDaemon(*id));
            }
        }
        if s.used.waits < self.waits {
            for id in s.timers.keys() {
                if *id != TimerId::Check {
                    out.push(Action::Timer(*id));
                }
            }
        }
        if s.used.updates < self.updates {
            out.push(Action::Update);
        }
    }

    fn next(&self, s: &State, action: &Action) -> Option<State> {
        let mut state = s.clone();
        match action {
            Action::CrashSupervisor => {
                state.used.supervisor_crashes += 1;
                self.restart(&mut state)?;
            }
            Action::Perform => {
                let next = state.pending.pop_front()?;
                self.perform(&mut state, next)?;
            }
            Action::ReleaseListed { good } => {
                state.used.releases += 1;
                let major = state.world.published.map_or(2, |build| build.major + 1);
                state.world.published = Some(Build { major, good: *good });
                let at_ms = state.timers.remove(&TimerId::Check)?;
                state.now_ms = state.now_ms.max(at_ms);
                self.feed(&mut state, Input::Timer(TimerId::Check));
            }
            Action::Prepare(daemon) => {
                state.world.daemons.get_mut(daemon)?.prepared = true;
                self.feed(&mut state, Input::Prepared { daemon: *daemon });
            }
            Action::Go(daemon) => {
                state.world.daemons.get_mut(daemon)?.activated = true;
                self.feed(&mut state, Input::Activated { daemon: *daemon });
            }
            Action::StartFailed(daemon) => {
                state.used.start_failures += 1;
                state.world.daemons.remove(daemon)?;
                self.feed(
                    &mut state,
                    Input::StartFailed {
                        daemon: *daemon,
                        why: "it could not be started".into(),
                    },
                );
            }
            Action::DaemonStops(daemon) | Action::CrashDaemon(daemon) => {
                if matches!(action, Action::CrashDaemon(_)) {
                    state.used.daemon_crashes += 1;
                }
                state.world.daemons.remove(daemon)?;
                let code = if matches!(action, Action::CrashDaemon(_)) {
                    1
                } else {
                    0
                };
                self.feed(
                    &mut state,
                    Input::DaemonExited {
                        daemon: *daemon,
                        code,
                    },
                );
            }
            Action::Timer(id) => {
                state.used.waits += 1;
                let at_ms = state.timers.remove(id)?;
                state.now_ms = state.now_ms.max(at_ms);
                self.feed(&mut state, Input::Timer(*id));
            }
            Action::Update => {
                state.used.updates += 1;
                let ask = supervisor_core::AskId(u64::from(state.used.updates));
                self.feed(
                    &mut state,
                    Input::Asked {
                        ask,
                        request: Request::Check,
                    },
                );
            }
        }
        self.drain(&mut state)?;
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(
                "the supervisor core never asks for a dump",
                |_, s: &State| s.seen.dumped.is_none(),
            ),
            Property::always(
                "there is always a binary that can start: something is at the path for the service \
                 manager to start, and it is a build whose daemon starts, or prev, which is, is \
                 kept beside it; except between the two renames of a Windows install or \
                 rollback, which empty the path, so a crash or power loss there leaves nothing to \
                 start",
                |_, s: &State| {
                    let starts = |build: Option<Build>| build.is_some_and(|build| build.good);
                    (s.world.path.is_some() || s.world.renaming)
                        && (starts(s.world.path) || starts(s.world.prev))
                },
            ),
            Property::always(
                "never two daemons serving: at most one daemon told go is alive",
                |_, s: &State| s.serving() <= 1,
            ),
            Property::always(
                "never two daemons serving: no daemon is started while another lives",
                |_, s: &State| s.seen.started_beside.is_none(),
            ),
            Property::always(
                "where a running binary cannot be replaced, no binary is installed while a daemon \
                 runs: the daemon stops before the swap",
                |_, s: &State| s.seen.installed_under.is_none(),
            ),
            Property::always(
                "a rollback restores the previous binary: it records the build it rolls back as \
                 rejected and puts back the binary that was at the path before the update",
                |_, s: &State| s.seen.wrong_rollback.is_none(),
            ),
            Property::always(
                "everything before go is read-only apart from migration and generation: no daemon \
                 is told go before it has prepared",
                |_, s: &State| s.seen.go_before_prepared.is_none(),
            ),
            Property::sometimes(
                "an update sometimes completes: a daemon from the new build serves with prev gone",
                |_, s: &State| {
                    s.world.prev.is_none()
                        && s.world.path.is_some_and(|build| build.major > 1)
                        && s.world
                            .daemons
                            .values()
                            .any(|daemon| daemon.activated && Some(daemon.build) == s.world.path)
                },
            ),
            Property::sometimes(
                "where an install links prev to the binary, a supervisor sometimes starts to find \
                 prev is its own binary and removes it",
                |model: &Self, s: &State| !model.replace_while_running || s.seen.removed_own_prev,
            ),
            Property::sometimes(
                "a bad release is sometimes rolled back, and the first build serves again",
                |_, s: &State| {
                    s.seen.rolled_back
                        && s.world.path == Some(Build::FIRST)
                        && s.world.daemons.values().any(|daemon| daemon.activated)
                },
            ),
        ]
    }
}
