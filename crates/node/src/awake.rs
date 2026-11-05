//! The daemon's half of `keep_awake`: while one of this machine's agents is
//! working, the machine stays awake on battery too. The supervisor holds the
//! half for power (see `supervisor::keep_awake`); only the daemon knows when
//! an agent works.
//!
//! Each profile follows its own inventory for its own agents' phases, and
//! the installation holds one assertion while any profile has a working
//! agent and `keep_awake` is on. Only a supervised install takes the
//! assertion: one whose service manager runs the daemon has never kept the
//! machine awake, and a test or the phone has nothing to keep awake.

use std::collections::BTreeSet;
use std::sync::Weak;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use wire::inventory_event::Of;

use crate::profiles::ProfileId;
use crate::runtime::ProfileRuntime;
#[cfg(not(target_os = "ios"))]
use crate::supervisor::KeepAwake;

/// A phone keeps nothing awake: it runs no agents of its own, and iOS
/// decides when it sleeps.
#[cfg(target_os = "ios")]
struct KeepAwake;

#[cfg(target_os = "ios")]
impl KeepAwake {
    fn while_working() -> Self {
        KeepAwake
    }
}

/// How long a profile whose inventory stream ended waits before opening it
/// again: it ends when the profile falls behind or stops.
const REOPEN: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Wanted {
    pub(crate) keep_awake: bool,
    /// The profiles with an agent of their own working now.
    pub(crate) working: BTreeSet<ProfileId>,
}

impl Wanted {
    pub(crate) fn awake(&self) -> bool {
        self.keep_awake && !self.working.is_empty()
    }
}

pub(crate) struct Awake {
    wanted: watch::Sender<Wanted>,
}

impl Awake {
    pub(crate) fn new(keep_awake: bool) -> Awake {
        Awake {
            wanted: watch::Sender::new(Wanted {
                keep_awake,
                working: BTreeSet::new(),
            }),
        }
    }

    pub(crate) fn keep_awake(&self, on: bool) {
        self.wanted
            .send_if_modified(|wanted| std::mem::replace(&mut wanted.keep_awake, on) != on);
    }

    fn working(&self, profile: ProfileId, working: bool) {
        self.wanted.send_if_modified(|wanted| match working {
            true => wanted.working.insert(profile),
            false => wanted.working.remove(&profile),
        });
    }

    /// Whether the machine should be kept awake now.
    pub(crate) fn wanted(&self) -> bool {
        self.wanted.borrow().awake()
    }

    /// Holds the assertion while it is wanted, until the task is aborted.
    pub(crate) fn hold(&self) -> JoinHandle<()> {
        let mut wanted = self.wanted.subscribe();
        tokio::spawn(async move {
            let mut held: Option<KeepAwake> = None;
            loop {
                let awake = wanted.borrow_and_update().awake();
                if awake != held.is_some() {
                    held = awake.then(KeepAwake::while_working);
                }
                if wanted.changed().await.is_err() {
                    return;
                }
            }
        })
    }
}

/// Follows one profile's own agents and tells `awake` whether any of them
/// is working, until the task is aborted.
pub(crate) fn follow(
    runtime: Weak<ProfileRuntime>,
    profile: ProfileId,
    awake: Weak<Awake>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let Some(live) = runtime.upgrade() else {
                return;
            };
            let own = live.host().as_bytes().to_vec();
            let opened = live.subscribe_inventory().await;
            drop(live);
            if let Ok(mut inventory) = opened {
                let mut working = BTreeSet::new();
                while let Some(event) = inventory.next().await {
                    match &event.of {
                        Some(Of::Agent(agent)) if agent.host_id == own => {
                            if agent.phase() == wire::Phase::Working
                                && agent.lifecycle() != wire::Lifecycle::Exited
                            {
                                working.insert(agent.agent_id.clone());
                            } else {
                                working.remove(&agent.agent_id);
                            }
                        }
                        Some(Of::AgentRemoved(removed)) if removed.host_id == own => {
                            working.remove(&removed.agent_id);
                        }
                        _ => continue,
                    }
                    let Some(awake) = awake.upgrade() else {
                        return;
                    };
                    awake.working(profile, !working.is_empty());
                }
            }
            let Some(awake) = awake.upgrade() else {
                return;
            };
            awake.working(profile, false);
            tokio::time::sleep(REOPEN).await;
        }
    })
}
