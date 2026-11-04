//! First-run onboarding as the daemon serves it: one list of steps, the
//! same for every client, each in a state derived from what the daemon
//! already knows wherever it can be, so no client's copy can drift.
//!
//! Only what cannot be derived is kept: the steps a person confirmed or
//! skipped, whether an agent here was ever created from a paired device,
//! and whether a client has shown the analytics notice. They live in
//! `<root>/onboarding.json`, the installation's state rather than its
//! config file, since none of them is a preference. Confirmations are the
//! installation's, as the settings they confirm are; a skip of a step whose
//! state depends on the profile (pairing, signing in, the first agent) is
//! that profile's.
//!
//! A step added in a later version shows to existing installations the
//! next time a client opens, alone, and only while it is needed.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;
use wire::{OnboardingStepId as Id, StepState};

use crate::daemon::Installation;
use crate::profiles::ProfileId;
use crate::registry::{ProfileRefusal, Registry};
use crate::runtime::ProfileRuntime;

/// The file in the root.
pub const ONBOARDING: &str = "onboarding.json";

/// Every step, in the order clients show them.
pub(crate) const STEPS: [Id; 6] = [
    Id::PairPhone,
    Id::KeepReachable,
    Id::SignIn,
    Id::Projects,
    Id::Improve,
    Id::FirstAgent,
];

/// Whether a step is done when the person says so, rather than when the
/// daemon sees it happen.
pub(crate) fn confirmed_by_person(step: Id) -> bool {
    matches!(step, Id::KeepReachable | Id::Projects | Id::Improve)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Step {
    PairPhone,
    KeepReachable,
    SignIn,
    Projects,
    Improve,
    FirstAgent,
}

impl Step {
    fn of(id: Id) -> Option<Step> {
        Some(match id {
            Id::PairPhone => Step::PairPhone,
            Id::KeepReachable => Step::KeepReachable,
            Id::SignIn => Step::SignIn,
            Id::Projects => Step::Projects,
            Id::Improve => Step::Improve,
            Id::FirstAgent => Step::FirstAgent,
            Id::Unspecified => return None,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Stored {
    notice_shown: bool,
    confirmed: BTreeSet<Step>,
    skipped: BTreeSet<Step>,
    profiles: BTreeMap<Uuid, OfProfile>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct OfProfile {
    skipped: BTreeSet<Step>,
    /// An agent on this host was created on a paired device's request.
    agent_from_device: bool,
}

/// What the daemon knows that a step's state is derived from.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Facts {
    /// The install runs a supervisor, which a login item could start.
    pub(crate) supervised: bool,
    pub(crate) signed_in: bool,
    /// The profile trusts a phone.
    pub(crate) paired_phone: bool,
}

/// The installation's onboarding state.
pub(crate) struct Onboarding {
    path: PathBuf,
    stored: Mutex<Stored>,
    /// Rings whenever what is kept changes.
    changed: watch::Sender<()>,
    /// How many folders the last suggestion offered, which the projects
    /// step's usage event counts what was kept against.
    suggested: AtomicU32,
}

impl Onboarding {
    /// What `<root>/onboarding.json` holds; a file that does not read is
    /// logged and taken as nothing kept.
    pub(crate) fn open(root: &Path) -> Onboarding {
        let path = root.join(ONBOARDING);
        Onboarding {
            stored: Mutex::new(read(&path)),
            path,
            changed: watch::Sender::new(()),
            suggested: AtomicU32::new(0),
        }
    }

    pub(crate) fn suggested(&self, count: usize) {
        self.suggested
            .store(u32::try_from(count).unwrap_or(u32::MAX), Ordering::Relaxed);
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<()> {
        self.changed.subscribe()
    }

    pub(crate) fn notice_shown(&self) -> bool {
        self.stored.lock().unwrap().notice_shown
    }

    /// The steps for `profile` as they stand.
    pub(crate) fn steps(&self, profile: ProfileId, facts: Facts) -> Vec<wire::OnboardingStep> {
        let stored = self.stored.lock().unwrap();
        let of_profile = stored.profiles.get(&profile).cloned().unwrap_or_default();
        STEPS
            .iter()
            .map(|&id| {
                let step = Step::of(id).expect("a named step");
                let skipped = if confirmed_by_person(id) {
                    stored.skipped.contains(&step)
                } else {
                    of_profile.skipped.contains(&step)
                };
                let done = match id {
                    Id::PairPhone => facts.paired_phone,
                    Id::SignIn => facts.signed_in,
                    Id::FirstAgent => of_profile.agent_from_device,
                    _ => stored.confirmed.contains(&step),
                };
                let state = if id == Id::KeepReachable && !facts.supervised {
                    StepState::NotNeeded
                } else if done {
                    StepState::Done
                } else if skipped {
                    StepState::Skipped
                } else {
                    StepState::Needed
                };
                wire::OnboardingStep {
                    id: id as i32,
                    state: state as i32,
                }
            })
            .collect()
    }

    /// The person confirmed a step that is done when they say so.
    pub(crate) fn confirm(&self, step: Id) -> io::Result<()> {
        let Some(step) = Step::of(step) else {
            return Ok(());
        };
        self.change(|stored| {
            stored.skipped.remove(&step);
            stored.confirmed.insert(step)
        })
    }

    /// The person chose "Later" on a step.
    pub(crate) fn skip(&self, profile: ProfileId, step: Id) -> io::Result<()> {
        let Some(named) = Step::of(step) else {
            return Ok(());
        };
        self.change(|stored| match confirmed_by_person(step) {
            true => stored.skipped.insert(named),
            false => stored
                .profiles
                .entry(profile)
                .or_default()
                .skipped
                .insert(named),
        })
    }

    pub(crate) fn mark_notice_shown(&self) -> io::Result<()> {
        self.change(|stored| !std::mem::replace(&mut stored.notice_shown, true))
    }

    /// An agent on this host was created on a paired device's request.
    pub(crate) fn agent_from_device(&self, profile: ProfileId) -> io::Result<()> {
        self.change(|stored| {
            let of = stored.profiles.entry(profile).or_default();
            !std::mem::replace(&mut of.agent_from_device, true)
        })
    }

    /// Changes what is kept, writing it and ringing only when `change` says
    /// it moved anything.
    fn change(&self, change: impl FnOnce(&mut Stored) -> bool) -> io::Result<()> {
        let mut stored = self.stored.lock().unwrap();
        let mut next = stored.clone();
        if !change(&mut next) {
            return Ok(());
        }
        write(&self.path, &next)?;
        *stored = next;
        drop(stored);
        self.changed.send_replace(());
        Ok(())
    }
}

/// Whether a client has shown the analytics notice on the installation at
/// `root`, read from its file: for a command that runs while the daemon
/// does not.
pub fn notice_shown_in(root: &Path) -> bool {
    read(&root.join(ONBOARDING)).notice_shown
}

/// Records that the notice was shown, in the file: only while no daemon
/// runs on `root`, since a running one keeps the file and would write over
/// it.
pub fn mark_notice_shown_in(root: &Path) -> io::Result<()> {
    let path = root.join(ONBOARDING);
    let mut stored = read(&path);
    if stored.notice_shown {
        return Ok(());
    }
    stored.notice_shown = true;
    write(&path, &stored)
}

fn read(path: &Path) -> Stored {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            tracing::warn!(%error, path = %path.display(), "the onboarding state does not read");
            Stored::default()
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Stored::default(),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "reading the onboarding state");
            Stored::default()
        }
    }
}

fn write(path: &Path, stored: &Stored) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(stored).map_err(io::Error::other)?;
    crate::install::write_durably(path, &bytes)
}

/// What a client answers that the daemon cannot take.
fn bad_answer(why: &str) -> ProfileRefusal {
    ProfileRefusal::BadAnswer(why.to_owned())
}

fn usage_step(step: Id) -> Option<analytics::Step> {
    Some(match step {
        Id::PairPhone => analytics::Step::PairPhone,
        Id::KeepReachable => analytics::Step::KeepReachable,
        Id::SignIn => analytics::Step::SignIn,
        Id::Projects => analytics::Step::Projects,
        Id::Improve => analytics::Step::Improve,
        Id::FirstAgent => analytics::Step::FirstAgent,
        Id::Unspecified => return None,
    })
}

fn usage_surface(surface: wire::Surface) -> Option<analytics::Client> {
    Some(match surface {
        wire::Surface::Terminal => analytics::Client::Terminal,
        wire::Surface::Phone => analytics::Client::Phone,
        wire::Surface::Mac => analytics::Client::Mac,
        wire::Surface::Unspecified => return None,
    })
}

/// A profile named by its id's bytes, or none when they are empty.
fn named(bytes: &[u8]) -> Result<Option<ProfileId>, ProfileRefusal> {
    if bytes.is_empty() {
        return Ok(None);
    }
    Uuid::from_slice(bytes)
        .map(Some)
        .map_err(|_| bad_answer("a profile id is 16 bytes"))
}

impl Registry {
    /// The profile `wanted` names, or the oldest, which a bare `amux`
    /// opens.
    fn onboarding_runtime(
        installation: &Installation,
        wanted: Option<ProfileId>,
    ) -> Result<Arc<ProfileRuntime>, ProfileRefusal> {
        let hosted = installation.hosted.lock().unwrap();
        match wanted {
            Some(id) => hosted.get(&id).ok_or(ProfileRefusal::NoProfile(id)),
            None => hosted
                .values()
                .min_by_key(|hosted| (hosted.entry.created_ms, hosted.entry.id))
                .ok_or(ProfileRefusal::Stopping),
        }
        .map(|hosted| hosted.runtime.clone())
    }

    fn onboarding_of(installation: &Installation, runtime: &ProfileRuntime) -> wire::Onboarding {
        let facts = Facts {
            supervised: installation.settings.supervised(),
            signed_in: runtime
                .edge()
                .is_some_and(|edge| edge.cloud().signed_in() == Some(true)),
            paired_phone: runtime.trusts_a_phone(),
        };
        let onboarding = &installation.onboarding;
        wire::Onboarding {
            profile_id: runtime.profile().as_bytes().to_vec(),
            steps: onboarding.steps(runtime.profile(), facts),
            notice_shown: onboarding.notice_shown(),
        }
    }

    /// A profile's onboarding steps as they stand: the oldest profile's when
    /// it names none.
    pub fn onboarding(
        &self,
        profile: Option<ProfileId>,
    ) -> Result<wire::Onboarding, ProfileRefusal> {
        let installation = self.installation()?;
        let runtime = Self::onboarding_runtime(&installation, profile)?;
        Ok(Self::onboarding_of(&installation, &runtime))
    }

    /// A profile's onboarding steps now and each time they change, until
    /// the receiver is dropped or the profile goes.
    pub fn subscribe_onboarding(
        &self,
        profile: Option<ProfileId>,
    ) -> Result<mpsc::Receiver<wire::Onboarding>, ProfileRefusal> {
        let installation = self.installation()?;
        let runtime = Self::onboarding_runtime(&installation, profile)?;
        let profile = runtime.profile();
        // What the steps derive from: the answers kept, the profile list
        // (which moves on every sign-in and sign-out) and the hosts the
        // profile trusts.
        let mut answered = installation.onboarding.subscribe();
        let mut moved = installation.moved.subscribe();
        let mut hosts = runtime.subscribe_hosts();
        let installation = Arc::downgrade(&installation);
        let runtime = Arc::downgrade(&runtime);
        let (sent, received) = mpsc::channel(4);
        tokio::spawn(async move {
            let mut last = None;
            loop {
                answered.borrow_and_update();
                moved.borrow_and_update();
                hosts.borrow_and_update();
                let (Some(installation), Some(runtime)) =
                    (installation.upgrade(), runtime.upgrade())
                else {
                    return;
                };
                if !installation.hosted.lock().unwrap().contains_key(&profile) {
                    return;
                }
                let now = Self::onboarding_of(&installation, &runtime);
                drop((installation, runtime));
                if last.as_ref() != Some(&now) {
                    if sent.send(now.clone()).await.is_err() {
                        return;
                    }
                    last = Some(now);
                }
                let changed = tokio::select! {
                    changed = answered.changed() => changed.is_ok(),
                    changed = moved.changed() => changed.is_ok(),
                    changed = hosts.changed() => changed.is_ok(),
                    () = sent.closed() => false,
                };
                if !changed {
                    return;
                }
            }
        });
        Ok(received)
    }

    /// Takes a client's answer to a step and says where the steps stand
    /// after it. A step the daemon sees happen (pairing a phone, signing in,
    /// the first agent) is done only when it happens; a client may skip it.
    pub fn answer_onboarding(
        &self,
        request: &wire::AnswerOnboardingRequest,
    ) -> Result<wire::Onboarding, ProfileRefusal> {
        use wire::answer_onboarding_request::Answer;
        let installation = self.installation()?;
        let runtime = Self::onboarding_runtime(&installation, named(&request.profile_id)?)?;
        let profile = runtime.profile();
        let onboarding = &installation.onboarding;
        let surface = usage_surface(request.surface());
        let record = |step: Id, outcome: analytics::StepOutcome| {
            if let (Some(step), Some(surface)) = (usage_step(step), surface) {
                runtime
                    .analytics()
                    .record(analytics::Event::OnboardingStep {
                        step,
                        outcome,
                        surface,
                    });
            }
        };
        let step = |raw: i32| match Id::try_from(raw) {
            Ok(Id::Unspecified) | Err(_) => Err(bad_answer("the answer names no step")),
            Ok(step) => Ok(step),
        };
        let kept = |result: io::Result<()>| result.map_err(ProfileRefusal::failed);
        match request.answer {
            Some(Answer::Done(raw)) => {
                let done = step(raw)?;
                if !confirmed_by_person(done) {
                    return Err(bad_answer(
                        "that step is done when it happens, not when a client says so",
                    ));
                }
                kept(onboarding.confirm(done))?;
                record(done, analytics::StepOutcome::Done);
                if done == Id::Projects && surface.is_some() {
                    runtime
                        .analytics()
                        .record(analytics::Event::ProjectsChosen {
                            kept: u32::try_from(runtime.launch().projects.len())
                                .unwrap_or(u32::MAX),
                            suggested: onboarding.suggested.load(Ordering::Relaxed),
                        });
                }
            }
            Some(Answer::Skip(raw)) => {
                let skipped = step(raw)?;
                kept(onboarding.skip(profile, skipped))?;
                record(skipped, analytics::StepOutcome::Skipped);
            }
            Some(Answer::Shown(raw)) => record(step(raw)?, analytics::StepOutcome::Shown),
            Some(Answer::NoticeShown(_)) => kept(onboarding.mark_notice_shown())?,
            Some(Answer::Dismiss(_)) => {
                let now = Self::onboarding_of(&installation, &runtime);
                for needed in now
                    .steps
                    .iter()
                    .filter(|step| step.state() == StepState::Needed)
                {
                    kept(onboarding.skip(profile, needed.id()))?;
                    record(needed.id(), analytics::StepOutcome::Skipped);
                }
            }
            None => return Err(bad_answer("the request carries no answer")),
        }
        Ok(Self::onboarding_of(&installation, &runtime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(onboarding: &Onboarding, profile: ProfileId, facts: Facts) -> Vec<StepState> {
        onboarding
            .steps(profile, facts)
            .iter()
            .map(|step| step.state())
            .collect()
    }

    #[test]
    fn steps_are_derived_and_only_answers_are_kept() {
        use StepState::{Done, Needed, NotNeeded, Skipped};
        let dir = tempfile::tempdir().unwrap();
        let onboarding = Onboarding::open(dir.path());
        let (desk, work) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let supervised = Facts {
            supervised: true,
            ..Facts::default()
        };
        assert_eq!(states(&onboarding, desk, supervised), [Needed; 6]);
        assert_eq!(
            states(&onboarding, desk, Facts::default())[1],
            NotNeeded,
            "nothing to start at login without a supervisor"
        );

        onboarding.skip(desk, Id::PairPhone).unwrap();
        onboarding.skip(desk, Id::Projects).unwrap();
        onboarding.confirm(Id::Improve).unwrap();
        onboarding.agent_from_device(desk).unwrap();
        let facts = Facts {
            signed_in: true,
            ..supervised
        };
        assert_eq!(
            states(&onboarding, desk, facts),
            [Skipped, Needed, Done, Skipped, Done, Done]
        );
        // A confirmation is the installation's; a skip of a profile's own
        // step is that profile's.
        assert_eq!(
            states(&onboarding, work, supervised),
            [Needed, Needed, Needed, Skipped, Done, Needed]
        );
        // Pairing a phone after skipping it is done.
        let paired = Facts {
            paired_phone: true,
            ..facts
        };
        assert_eq!(states(&onboarding, desk, paired)[0], Done);

        // All of it survives a restart, the notice too.
        assert!(!onboarding.notice_shown() && !notice_shown_in(dir.path()));
        onboarding.mark_notice_shown().unwrap();
        let again = Onboarding::open(dir.path());
        assert!(again.notice_shown() && notice_shown_in(dir.path()));
        assert_eq!(
            states(&again, desk, facts),
            [Skipped, Needed, Done, Skipped, Done, Done]
        );
        // Confirming a skipped step makes it done.
        again.confirm(Id::Projects).unwrap();
        assert_eq!(states(&again, desk, facts)[3], Done);
    }

    #[test]
    fn a_command_marks_the_notice_in_the_file_while_no_daemon_runs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!notice_shown_in(dir.path()));
        mark_notice_shown_in(dir.path()).unwrap();
        assert!(notice_shown_in(dir.path()));
        assert!(Onboarding::open(dir.path()).notice_shown());
        std::fs::write(dir.path().join(ONBOARDING), "not json").unwrap();
        assert!(!notice_shown_in(dir.path()), "a file that does not read");
    }
}
