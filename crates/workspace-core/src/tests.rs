use super::*;

fn member(n: u8) -> MemberId {
    MemberId([n; 16])
}

fn key(kind: Kind) -> Key {
    Key {
        kind,
        id: "/trees/fix".to_owned(),
    }
}

fn plan() -> Plan {
    Plan {
        repository: "/source/amux".to_owned(),
        name: "fix".to_owned(),
        branch: "fix".to_owned(),
        from: None,
    }
}

/// The core, stepped one input at a time, with its invariants checked after
/// each step and the views it publishes set apart from the jobs it asks for.
struct Run {
    core: Workspaces,
    published: Vec<View>,
}

impl Run {
    fn new() -> Self {
        Self {
            core: Workspaces::new(),
            published: Vec::new(),
        }
    }

    /// The outputs of one step, publications aside.
    fn step(&mut self, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        self.core.step(input, &mut out);
        if let Err(broken) = self.core.check_invariants() {
            panic!("{broken}");
        }
        out.into_iter()
            .filter(|output| match output {
                Output::Publish { view } => {
                    self.published.push(view.clone());
                    false
                }
                _ => true,
            })
            .collect()
    }

    fn added(&mut self, n: u8, kind: Kind, made_here: bool) -> Vec<Output> {
        self.step(Input::Added {
            member: member(n),
            workspace: key(kind),
            made_here,
            running: false,
        })
    }

    fn launching(&mut self, n: u8) -> Vec<Output> {
        self.step(Input::Launching { member: member(n) })
    }

    fn slept(&mut self, n: u8) -> Vec<Output> {
        self.step(Input::Slept { member: member(n) })
    }

    fn removed(&mut self, n: u8, discard: bool) -> Vec<Output> {
        self.removed_before(n, discard, &[])
    }

    /// Member `n` removed ahead of `descendants`, which follow it.
    fn removed_before(&mut self, n: u8, discard: bool, descendants: &[u8]) -> Vec<Output> {
        self.step(Input::Removed {
            member: member(n),
            discard,
            descendants: descendants.iter().copied().map(member).collect(),
        })
    }

    fn claimed(&mut self, kind: Kind) -> Vec<Output> {
        self.step(Input::Claimed {
            workspace: key(kind),
            result: Ok(()),
        })
    }

    fn started(&mut self, result: Result<(), String>) -> Vec<Output> {
        self.step(Input::Started {
            workspace: key(Kind::Wt),
            result,
        })
    }

    fn stopped(&mut self) -> Vec<Output> {
        self.step(Input::Stopped {
            workspace: key(Kind::Wt),
            result: Ok(()),
        })
    }

    fn deleted(&mut self, kind: Kind, removal: Removal) -> Vec<Output> {
        self.step(Input::Deleted {
            workspace: key(kind),
            removal,
        })
    }

    /// A wt tree this host made, with member 1 launched in it and its start
    /// job answered.
    fn made_wt_running() -> Self {
        let mut run = Self::new();
        run.step(Input::Create {
            member: member(1),
            workspace: key(Kind::Wt),
            plan: plan(),
        });
        run.launching(1);
        run.step(Input::Made {
            workspace: key(Kind::Wt),
            result: Ok(()),
        });
        run.claimed(Kind::Wt);
        run.started(Ok(()));
        run
    }
}

fn launch(n: u8) -> Output {
    Output::Launch { member: member(n) }
}

fn take(kind: Kind) -> Output {
    Output::TakeClaim {
        workspace: key(kind),
    }
}

fn release(kind: Kind) -> Output {
    Output::ReleaseClaim {
        workspace: key(kind),
    }
}

fn start() -> Output {
    Output::Start {
        workspace: key(Kind::Wt),
    }
}

fn stop() -> Output {
    Output::StopWhenUnheld {
        workspace: key(Kind::Wt),
    }
}

fn delete(kind: Kind, discard: bool) -> Output {
    Output::Delete {
        workspace: key(kind),
        discard,
    }
}

#[test]
fn a_folder_member_launches_at_once_and_its_folder_is_never_touched() {
    let mut run = Run::new();
    assert_eq!(run.added(1, Kind::Folder, true), vec![]);
    assert_eq!(run.launching(1), vec![launch(1)]);
    assert_eq!(run.slept(1), vec![]);
    assert_eq!(run.removed(1, true), vec![]);
    assert_eq!(
        run.core.keys().count(),
        0,
        "a memberless folder is forgotten"
    );
}

#[test]
fn a_new_worktree_is_made_before_its_member_launches_and_never_started_or_stopped() {
    let mut run = Run::new();
    let created = run.step(Input::Create {
        member: member(1),
        workspace: key(Kind::Git),
        plan: plan(),
    });
    assert_eq!(
        created,
        vec![Output::Make {
            workspace: key(Kind::Git),
            plan: plan()
        }]
    );
    assert!(run.core.view_of(member(1)).unwrap().creating);
    assert_eq!(run.launching(1), vec![], "it waits for the worktree");
    let made = run.step(Input::Made {
        workspace: key(Kind::Git),
        result: Ok(()),
    });
    assert_eq!(made, vec![launch(1)], "git has no claim and no start hook");
    assert!(!run.published.last().unwrap().creating);
    assert_eq!(run.slept(1), vec![], "nor a stop");
    assert_eq!(run.removed(1, false), vec![delete(Kind::Git, false)]);
}

#[test]
fn a_new_wt_tree_launches_its_member_once_claimed_and_starts() {
    let mut run = Run::new();
    run.step(Input::Create {
        member: member(1),
        workspace: key(Kind::Wt),
        plan: plan(),
    });
    assert_eq!(run.launching(1), vec![]);
    let made = run.step(Input::Made {
        workspace: key(Kind::Wt),
        result: Ok(()),
    });
    assert_eq!(made, vec![take(Kind::Wt)], "no launch before the claim");
    assert_eq!(run.claimed(Kind::Wt), vec![launch(1), start()]);
}

#[test]
fn a_hand_made_tree_is_claimed_while_a_member_is_awake_but_never_started_or_stopped() {
    let mut run = Run::new();
    run.added(1, Kind::Wt, false);
    assert_eq!(run.launching(1), vec![take(Kind::Wt)]);
    assert_eq!(run.claimed(Kind::Wt), vec![launch(1)]);
    assert_eq!(run.slept(1), vec![release(Kind::Wt)]);
    assert_eq!(run.removed(1, true), vec![], "never removed");
    assert_eq!(run.core.keys().count(), 0);
}

#[test]
fn the_last_awake_member_going_releases_the_claim_and_asks_for_one_stop() {
    let mut run = Run::made_wt_running();
    run.added(2, Kind::Wt, true);
    assert_eq!(
        run.launching(2),
        vec![launch(2)],
        "the claim is held already"
    );
    assert_eq!(run.slept(1), vec![], "another member is awake");
    assert_eq!(run.slept(2), vec![release(Kind::Wt), stop()]);
    // Waking and sleeping again while the call waits asks for no second one:
    // the one outstanding cannot complete while the claim is held, and
    // completes once it is released.
    assert_eq!(run.launching(1), vec![take(Kind::Wt)]);
    assert_eq!(
        run.claimed(Kind::Wt),
        vec![launch(1)],
        "no start while the stop is outstanding"
    );
    assert_eq!(run.slept(1), vec![release(Kind::Wt)]);
    assert_eq!(run.stopped(), vec![]);
}

#[test]
fn a_stop_waits_for_this_hosts_start_job_and_is_then_reconsidered() {
    let mut run = Run::new();
    run.step(Input::Create {
        member: member(1),
        workspace: key(Kind::Wt),
        plan: plan(),
    });
    run.launching(1);
    run.step(Input::Made {
        workspace: key(Kind::Wt),
        result: Ok(()),
    });
    assert_eq!(run.claimed(Kind::Wt), vec![launch(1), start()]);
    assert_eq!(
        run.slept(1),
        vec![release(Kind::Wt)],
        "the start still runs"
    );
    assert_eq!(run.started(Ok(())), vec![stop()]);
}

#[test]
fn a_member_waking_before_the_start_finishes_takes_back_the_owed_stop() {
    let mut run = Run::new();
    run.step(Input::Create {
        member: member(1),
        workspace: key(Kind::Wt),
        plan: plan(),
    });
    run.launching(1);
    run.step(Input::Made {
        workspace: key(Kind::Wt),
        result: Ok(()),
    });
    run.claimed(Kind::Wt);
    run.slept(1);
    assert_eq!(run.launching(1), vec![take(Kind::Wt)]);
    assert_eq!(
        run.claimed(Kind::Wt),
        vec![launch(1)],
        "nothing stopped since the start still outstanding"
    );
    assert_eq!(run.started(Ok(())), vec![], "no stop is owed");
}

#[test]
fn a_member_woken_during_a_stop_gets_a_start_once_the_stop_completes() {
    let mut run = Run::made_wt_running();
    assert_eq!(run.slept(1), vec![release(Kind::Wt), stop()]);
    assert_eq!(run.launching(1), vec![take(Kind::Wt)]);
    // The stop hook already ran, so the claim was held only after it.
    assert_eq!(run.claimed(Kind::Wt), vec![launch(1)]);
    assert_eq!(run.stopped(), vec![start()]);
    // In the other order the start follows the claim.
    let mut run = Run::made_wt_running();
    run.slept(1);
    run.launching(1);
    assert_eq!(run.stopped(), vec![]);
    assert_eq!(run.claimed(Kind::Wt), vec![launch(1), start()]);
}

#[test]
fn consent_covers_only_the_members_its_delete_takes_with_it() {
    // A parent deleted with consent, its child in the tree following it by
    // the family rule without: the child's delete removes the tree and the
    // changes the parent agreed to lose.
    let mut run = Run::made_wt_running();
    run.added(2, Kind::Wt, true);
    run.slept(1);
    run.stopped();
    assert_eq!(
        run.removed_before(1, true, &[2]),
        vec![],
        "its child is left"
    );
    assert_eq!(run.removed(2, false), vec![delete(Kind::Wt, true)]);

    // Another agent working in the tree, not its descendant: the consent
    // was not the first delete's to give, and the other's own plain delete
    // keeps the changes.
    let mut run = Run::made_wt_running();
    run.added(2, Kind::Wt, true);
    run.slept(1);
    run.stopped();
    assert_eq!(run.removed(1, true), vec![]);
    assert_eq!(
        run.removed(2, false),
        vec![delete(Kind::Wt, false)],
        "another member's work was lost to a consent that never covered it"
    );

    // A member that joins after the consent was given is not covered by it.
    let mut run = Run::made_wt_running();
    run.added(2, Kind::Wt, true);
    run.slept(1);
    run.stopped();
    run.removed_before(1, true, &[2]);
    run.added(3, Kind::Wt, true);
    run.removed(2, false);
    let last = run.removed(3, false);
    assert_eq!(
        last,
        vec![delete(Kind::Wt, false)],
        "the new member's changes were never agreed to"
    );
}

#[test]
fn a_delete_waits_for_an_outstanding_stop_whose_hook_would_refuse_it() {
    // `amux rm` of an awake agent: its process ends, which asks for the
    // stop, then its row goes while the stop hook may be running and holding
    // the tree, so the removal waits for the stop's answer.
    let mut run = Run::made_wt_running();
    assert_eq!(run.slept(1), vec![release(Kind::Wt), stop()]);
    assert_eq!(run.removed(1, true), vec![], "the stop is outstanding");
    assert_eq!(run.stopped(), vec![delete(Kind::Wt, true)]);
    assert_eq!(run.deleted(Kind::Wt, Removal::Removed), vec![]);
    assert_eq!(run.core.keys().count(), 0);

    // A stop that fails is answered too, and the delete follows it.
    let mut run = Run::made_wt_running();
    run.slept(1);
    run.removed(1, false);
    assert_eq!(
        run.step(Input::Stopped {
            workspace: key(Kind::Wt),
            result: Err("the stop hook timed out".to_owned()),
        }),
        vec![delete(Kind::Wt, false)]
    );
}

#[test]
fn the_last_member_deleted_while_awake_releases_and_deletes_and_a_kept_tree_is_stopped() {
    let mut run = Run::made_wt_running();
    assert_eq!(
        run.removed(1, false),
        vec![release(Kind::Wt), delete(Kind::Wt, false)],
        "the removal stops it, so no stop call goes first"
    );
    assert_eq!(run.removed(1, false), vec![], "deleted once");
    assert_eq!(run.deleted(Kind::Wt, Removal::Kept), vec![stop()]);
    assert_eq!(run.stopped(), vec![]);
    assert_eq!(run.core.keys().count(), 0);

    let mut run = Run::made_wt_running();
    run.removed(1, false);
    assert_eq!(run.deleted(Kind::Wt, Removal::Removed), vec![]);
    assert_eq!(run.core.keys().count(), 0);
}

#[test]
fn a_kept_tree_is_no_longer_this_hosts_and_is_never_deleted_again() {
    let mut run = Run::made_wt_running();
    run.slept(1);
    run.stopped();
    run.removed(1, false);
    assert_eq!(run.deleted(Kind::Wt, Removal::Kept), vec![]);
    // An agent started in it later finds a tree like one made by hand.
    run.added(2, Kind::Wt, false);
    assert!(!run.core.view_of(member(2)).unwrap().made_here);
    assert_eq!(run.launching(2), vec![take(Kind::Wt)]);
    assert_eq!(run.claimed(Kind::Wt), vec![launch(2)], "never started");
    assert_eq!(run.slept(2), vec![release(Kind::Wt)], "never stopped");
    assert_eq!(run.removed(2, true), vec![], "never removed");
}

#[test]
fn a_delete_waits_for_a_claim_being_taken_so_the_backend_does_not_refuse_it() {
    let mut run = Run::new();
    run.added(1, Kind::Wt, true);
    assert_eq!(run.launching(1), vec![take(Kind::Wt)]);
    assert_eq!(run.removed(1, false), vec![]);
    assert_eq!(
        run.claimed(Kind::Wt),
        vec![release(Kind::Wt), delete(Kind::Wt, false)]
    );
}

#[test]
fn a_workspace_that_could_not_be_made_refuses_launches_and_goes_with_its_member() {
    let mut run = Run::new();
    run.step(Input::Create {
        member: member(1),
        workspace: key(Kind::Wt),
        plan: plan(),
    });
    run.launching(1);
    let failed = run.step(Input::Made {
        workspace: key(Kind::Wt),
        result: Err("the branch is taken on origin".to_owned()),
    });
    assert_eq!(
        failed,
        vec![Output::NotMade {
            member: member(1),
            detail: "the branch is taken on origin".to_owned()
        }]
    );
    assert!(matches!(
        run.launching(1).as_slice(),
        [Output::NotMade { .. }]
    ));
    // Whatever it left is this host's to remove.
    assert_eq!(run.removed(1, false), vec![delete(Kind::Wt, false)]);
}

#[test]
fn a_delete_during_making_applies_once_it_is_made() {
    let mut run = Run::new();
    run.step(Input::Create {
        member: member(1),
        workspace: key(Kind::Wt),
        plan: plan(),
    });
    assert_eq!(run.removed(1, true), vec![]);
    let made = run.step(Input::Made {
        workspace: key(Kind::Wt),
        result: Ok(()),
    });
    assert_eq!(made, vec![delete(Kind::Wt, true)]);
}

#[test]
fn a_member_joining_a_workspace_being_deleted_claims_it_and_launches_once_it_is_kept() {
    let mut run = Run::made_wt_running();
    run.slept(1);
    run.stopped();
    assert_eq!(run.removed(1, false), vec![delete(Kind::Wt, false)]);
    run.added(2, Kind::Wt, true);
    // The claim makes the backend refuse the removal, and keeps a stop call
    // issued before from beginning under the member.
    assert_eq!(run.launching(2), vec![take(Kind::Wt)]);
    assert_eq!(
        run.claimed(Kind::Wt),
        vec![],
        "no launch before the delete answers"
    );
    assert_eq!(
        run.deleted(Kind::Wt, Removal::Kept),
        vec![launch(2)],
        "disowned: no start"
    );
    assert!(!run.core.view_of(member(2)).unwrap().made_here);

    // Joining while the delete still waits for the stop: nothing was
    // deleted, so the member launches and the tree stays this host's.
    let mut run = Run::made_wt_running();
    run.slept(1);
    run.removed(1, false);
    run.added(2, Kind::Wt, true);
    assert_eq!(run.launching(2), vec![take(Kind::Wt)]);
    assert_eq!(run.claimed(Kind::Wt), vec![launch(2)]);
    assert_eq!(
        run.stopped(),
        vec![start()],
        "the stop ran before the claim was held"
    );
    assert!(run.core.view_of(member(2)).unwrap().made_here);
}

#[test]
fn a_member_asleep_in_a_workspace_that_was_removed_cannot_launch() {
    let mut run = Run::made_wt_running();
    run.slept(1);
    run.stopped();
    run.removed(1, false);
    run.added(2, Kind::Wt, true);
    assert_eq!(run.deleted(Kind::Wt, Removal::Removed), vec![]);
    assert!(matches!(
        run.launching(2).as_slice(),
        [Output::NotMade { member: m, .. }] if *m == member(2)
    ));
}

#[test]
fn a_restarted_daemon_retakes_claims_for_running_members_and_sweeps_memberless_workspaces() {
    let mut run = Run::new();
    let adopted = run.step(Input::Added {
        member: member(1),
        workspace: key(Kind::Wt),
        made_here: true,
        running: true,
    });
    assert_eq!(adopted, vec![take(Kind::Wt)]);
    // Already running, so no launch; but the daemon before may have lost
    // its start job, and the hook is safe to repeat.
    assert_eq!(run.claimed(Kind::Wt), vec![start()]);
    let other = Key {
        kind: Kind::Git,
        id: "/trees/old".to_owned(),
    };
    let swept = run.step(Input::Swept {
        owned: vec![key(Kind::Wt), other.clone()],
    });
    assert_eq!(
        swept,
        vec![Output::Delete {
            workspace: other,
            discard: false
        }],
        "only the one without members"
    );
    assert_eq!(run.started(Ok(())), vec![]);
    assert_eq!(run.slept(1), vec![release(Kind::Wt), stop()]);
}

#[test]
fn a_member_waking_as_its_tree_is_removed_is_told_it_cannot_launch() {
    let mut run = Run::made_wt_running();
    run.slept(1);
    run.stopped();
    run.removed(1, false);
    run.added(2, Kind::Wt, true);
    // It wakes after the backend removed the tree, before the answer came.
    assert_eq!(run.launching(2), vec![take(Kind::Wt)]);
    let removed = run.deleted(Kind::Wt, Removal::Removed);
    assert!(
        matches!(removed.as_slice(), [Output::NotMade { member: m, .. }] if *m == member(2)),
        "{removed:?}"
    );
    assert_eq!(
        run.claimed(Kind::Wt),
        vec![release(Kind::Wt)],
        "the claim asked for meanwhile goes as its answer comes"
    );
    assert!(matches!(
        run.launching(2).as_slice(),
        [Output::NotMade { .. }]
    ));
}

#[test]
fn a_dormant_workspace_after_a_restart_gets_nothing_until_a_member_wakes_and_sleeps() {
    let mut run = Run::new();
    assert_eq!(run.added(1, Kind::Wt, true), vec![]);
    assert_eq!(
        run.step(Input::Swept {
            owned: vec![key(Kind::Wt)]
        }),
        vec![]
    );
    assert_eq!(run.launching(1), vec![take(Kind::Wt)]);
    assert_eq!(run.claimed(Kind::Wt), vec![launch(1), start()]);
}

#[test]
fn a_failed_hook_is_the_workspaces_problem_until_the_same_hook_succeeds() {
    let mut run = Run::new();
    run.step(Input::Create {
        member: member(1),
        workspace: key(Kind::Wt),
        plan: plan(),
    });
    run.launching(1);
    run.step(Input::Made {
        workspace: key(Kind::Wt),
        result: Ok(()),
    });
    run.claimed(Kind::Wt);
    run.published.clear();
    assert_eq!(run.started(Err("start hook exited 1".to_owned())), vec![]);
    assert_eq!(
        run.published.last().unwrap().problem.as_deref(),
        Some("start hook exited 1")
    );
    // It counts as started: the stop still comes.
    assert_eq!(run.slept(1), vec![release(Kind::Wt), stop()]);
    run.step(Input::Environment {
        workspace: key(Kind::Wt),
        result: Ok(()),
    });
    assert!(
        run.core.view_of(member(1)).unwrap().problem.is_some(),
        "another source"
    );
    run.stopped();
    run.launching(1);
    run.claimed(Kind::Wt);
    run.started(Ok(()));
    assert_eq!(run.core.view_of(member(1)).unwrap().problem, None);
}

#[test]
fn a_claim_that_fails_still_lets_the_member_launch_and_says_why() {
    let mut run = Run::new();
    run.added(1, Kind::Wt, false);
    run.launching(1);
    let claimed = run.step(Input::Claimed {
        workspace: key(Kind::Wt),
        result: Err("wt is not installed".to_owned()),
    });
    assert_eq!(claimed, vec![launch(1)]);
    assert_eq!(
        run.core.view_of(member(1)).unwrap().problem.as_deref(),
        Some("wt is not installed")
    );
}

#[test]
fn a_member_stopped_before_its_claim_is_held_never_launches() {
    let mut run = Run::new();
    run.added(1, Kind::Wt, false);
    run.launching(1);
    assert_eq!(run.slept(1), vec![]);
    assert_eq!(run.claimed(Kind::Wt), vec![release(Kind::Wt)]);
    assert_eq!(run.core.keys().count(), 1, "the member is still in it");
}

#[test]
fn a_member_resolved_into_another_workspace_leaves_the_one_it_was_in() {
    let mut run = Run::new();
    run.added(1, Kind::Folder, false);
    run.added(1, Kind::Wt, false);
    assert_eq!(run.core.workspace_of(member(1)), Some(&key(Kind::Wt)));
    assert_eq!(run.core.keys().count(), 1);
}

#[test]
fn every_check_has_a_distinct_token() {
    for (index, check) in CHECKS.iter().enumerate() {
        assert!(check.id.starts_with("workspace."));
        assert!(CHECKS[..index].iter().all(|earlier| earlier.id != check.id));
    }
}
