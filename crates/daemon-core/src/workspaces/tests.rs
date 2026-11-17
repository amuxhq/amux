//! The workspaces experiment composed into the daemon core: a create in a
//! new workspace, a launch waiting for its claim, the jobs its lifecycle
//! asks for, and a restarted daemon resolving its agents again.

use std::collections::BTreeSet;

use store::{AgentKey, AgentRow, Sqlite};
use wire::{AgentSpec, Envelope, ExitCause, Lifecycle};

use super::*;
use crate::families::{Create, Ended};
use crate::ids::CallId;
use crate::payload::{CallError, ClientRequest, MessageFrom, Response, Status};
use crate::{Input, StartOptions, TimerId};

const OWN: [u8; 16] = *b"own-host-0000000";
const LEAD: AgentId = AgentId(*b"lead-00000000000");
const MATE: AgentId = AgentId(*b"mate-00000000000");
const IDLE: AgentId = AgentId(*b"idle-00000000000");

struct Rig {
    daemon: Daemon,
    next_call: u64,
    _dir: tempfile::TempDir,
}

fn store() -> Sqlite {
    let mut store = Sqlite::open_in_memory(OWN).expect("a store opens");
    store
        .settle_experiments(&[VERSION])
        .expect("the experiment's tables");
    store
}

fn daemon(store: Sqlite, on: bool, dir: &tempfile::TempDir) -> Daemon {
    Daemon::new(
        store,
        StartOptions {
            generation: 1,
            agents: dir.path().to_path_buf(),
            workspaces: on,
            ..StartOptions::default()
        },
    )
}

/// A daemon running the experiment over `store`, its start done with no
/// workspace this host made left over.
fn rig_over(store: Sqlite) -> Rig {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut rig = Rig {
        daemon: daemon(store, true, &dir),
        next_call: 0,
        _dir: dir,
    };
    let out = rig.step(Input::Swept);
    let (owned, _) = rig.job(&out, |job| matches!(job, WorkspaceJob::Owned));
    rig.done(owned, WorkspaceDone::Owned(Vec::new()));
    rig
}

fn rig() -> Rig {
    rig_over(store())
}

fn wt_tree(path: &str) -> Key {
    Key {
        kind: Kind::Wt,
        id: path.to_owned(),
    }
}

fn planned(path: &str, name: &str) -> Planned {
    Planned {
        workspace: wt_tree(path),
        plan: Plan {
            repository: "/src/shop".into(),
            name: name.into(),
            branch: name.into(),
            from: None,
        },
        base_branch: Some("main".into()),
    }
}

fn create(agent: AgentId, name: Option<&str>, new_workspace: bool, cwd: &str) -> ClientRequest {
    ClientRequest::CreateAgent(Box::new(Create {
        agent,
        cwd: cwd.into(),
        given: name.map(str::to_owned),
        branches: BTreeSet::from(["main".to_owned()]),
        new_workspace,
        workspace: None,
        recent: None,
        spec: AgentSpec {
            agent_id: agent.0.to_vec(),
            kind: wire::Kind::Codex as i32,
            ..AgentSpec::default()
        },
    }))
}

impl Rig {
    fn step(&mut self, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        self.daemon.step(input, &mut out);
        let dumps: Vec<_> = out
            .iter()
            .filter(|output| matches!(output, Output::Dump { .. }))
            .collect();
        assert!(dumps.is_empty(), "no check fails: {dumps:?}");
        out
    }

    fn call(&mut self, request: ClientRequest) -> (CallId, Vec<Output>) {
        self.next_call += 1;
        let call = CallId(self.next_call);
        (call, self.step(Input::ClientCall { call, request }))
    }

    fn done(&mut self, job: JobId, done: WorkspaceDone) -> Vec<Output> {
        self.step(Input::JobDone {
            job,
            result: JobResult::Workspace(done),
        })
    }

    /// The workspace job a step asked for that `matches`.
    fn job(
        &self,
        out: &[Output],
        matches: impl Fn(&WorkspaceJob) -> bool,
    ) -> (JobId, WorkspaceJob) {
        out.iter()
            .find_map(|output| match output {
                Output::Job {
                    id,
                    op: Job::Workspace(job),
                } if matches(job) => Some((*id, job.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no such workspace job: {out:#?}"))
    }

    fn row(&self, agent: AgentId) -> Option<AgentRow> {
        self.daemon
            .store
            .agent(&AgentKey::new(OWN, agent.0))
            .unwrap()
    }

    fn entry(&self, agent: AgentId) -> wire::experimental::AgentWorkspace {
        entry_of(&self.row(agent).expect("its row")).expect("its workspace on the row")
    }

    /// Creates `agent` in a new wt tree at `path`, made and claimed, its
    /// process started.
    fn launched_in_new_tree(&mut self, agent: AgentId, name: &str, path: &str) {
        let (_, out) = self.call(create(agent, Some(name), true, "/src/shop"));
        let (plan, _) = self.job(&out, |job| matches!(job, WorkspaceJob::Plan { .. }));
        let out = self.done(plan, WorkspaceDone::Planned(Ok(planned(path, name))));
        let (make, _) = self.job(&out, |job| matches!(job, WorkspaceJob::Make { .. }));
        let out = self.done(
            make,
            WorkspaceDone::Made {
                workspace: wt_tree(path),
                result: Ok(()),
            },
        );
        let (claim, _) = self.job(&out, |job| matches!(job, WorkspaceJob::TakeClaim { .. }));
        let out = self.done(
            claim,
            WorkspaceDone::Claimed {
                workspace: wt_tree(path),
                result: Ok(()),
            },
        );
        assert!(spawned(&out, agent).is_some(), "{out:#?}");
    }
}

fn spawned(out: &[Output], agent: AgentId) -> Option<(AgentSpec, Option<Key>)> {
    out.iter().find_map(|output| match output {
        Output::SpawnAgent {
            spec, workspace, ..
        } if spec.agent_id == agent.0 => Some((*spec.clone(), workspace.clone())),
        _ => None,
    })
}

fn answer_of(out: &[Output], to: CallId) -> Option<wire::Agent> {
    out.iter().find_map(|output| match output {
        Output::Answer {
            call,
            response: Response::Agent(agent),
        } if *call == to => Some(*agent.clone()),
        _ => None,
    })
}

fn workspace_jobs(out: &[Output]) -> Vec<WorkspaceJob> {
    out.iter()
        .filter_map(|output| match output {
            Output::Job {
                op: Job::Workspace(job),
                ..
            } => Some(job.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_create_in_a_new_workspace_answers_with_its_row_and_starts_once_made_and_claimed() {
    let mut rig = rig();
    let (call, out) = rig.call(create(LEAD, Some("fix"), true, "/src/shop"));
    assert!(rig.row(LEAD).is_none(), "no row before the plan");
    assert!(answer_of(&out, call).is_none());
    let (plan, job) = rig.job(&out, |job| matches!(job, WorkspaceJob::Plan { .. }));
    assert_eq!(
        job,
        WorkspaceJob::Plan {
            folder: "/src/shop".into(),
            name: "fix".into()
        }
    );

    // The name's folder or branch is taken: the next free one is planned.
    let out = rig.done(
        plan,
        WorkspaceDone::Planned(Err(PlanRefusal::Taken("there".into()))),
    );
    let (plan, job) = rig.job(&out, |job| matches!(job, WorkspaceJob::Plan { .. }));
    assert!(matches!(job, WorkspaceJob::Plan { name, .. } if name == "fix-2"));
    assert!(rig.row(LEAD).is_none());

    let path = "/trees/shop/fix-2";
    let out = rig.done(plan, WorkspaceDone::Planned(Ok(planned(path, "fix-2"))));
    let answered = answer_of(&out, call).expect("answered once the row is written");
    assert_eq!(answered.cwd, path, "its final folder");
    assert_eq!(answered.name, "fix-2");
    let entry = wire::experimental::WORKSPACE
        .get(&answered.experimental)
        .expect("the answer carries the workspace");
    assert!(entry.creating && entry.made_by_amux, "{entry:?}");
    assert_eq!(entry.id, path);
    assert!(
        spawned(&out, LEAD).is_none(),
        "nothing starts before it is made"
    );
    let (make, job) = rig.job(&out, |job| matches!(job, WorkspaceJob::Make { .. }));
    assert_eq!(
        job,
        WorkspaceJob::Make {
            workspace: wt_tree(path),
            plan: planned(path, "fix-2").plan
        }
    );

    // A message meanwhile waits for the process, with no patience of its
    // own.
    rig.next_call += 1;
    let message = CallId(rig.next_call);
    let out = rig.step(Input::ClientCall {
        call: message,
        request: ClientRequest::SendMessage {
            envelope: Box::new(Envelope {
                id: b"envelope".to_vec(),
                to: Some(wire::AgentRef {
                    host_id: OWN.to_vec(),
                    agent_id: LEAD.0.to_vec(),
                }),
                ..Envelope::default()
            }),
            from: MessageFrom::Person,
        },
    });
    assert!(
        !out.iter().any(|output| matches!(
            output,
            Output::ArmTimer { id: TimerId::Lane(agent), .. } if *agent == LEAD
        )),
        "{out:#?}"
    );

    let out = rig.done(
        make,
        WorkspaceDone::Made {
            workspace: wt_tree(path),
            result: Ok(()),
        },
    );
    assert!(!rig.entry(LEAD).creating);
    assert!(spawned(&out, LEAD).is_none(), "it waits for its claim");
    let (claim, _) = rig.job(&out, |job| matches!(job, WorkspaceJob::TakeClaim { .. }));

    let out = rig.done(
        claim,
        WorkspaceDone::Claimed {
            workspace: wt_tree(path),
            result: Ok(()),
        },
    );
    let (spec, workspace) = spawned(&out, LEAD).expect("it starts once claimed");
    assert_eq!(spec.cwd, path);
    assert_eq!(spec.base_branch.as_deref(), Some("main"));
    assert_eq!(workspace, Some(wt_tree(path)));
    let (start, _) = rig.job(&out, |job| {
        *job == WorkspaceJob::Start {
            workspace: wt_tree(path),
        }
    });

    // The message goes to the process once it takes input.
    let out = rig.step(Input::AgentHello { agent: LEAD });
    assert!(
        out.iter().any(|output| matches!(
            output,
            Output::WriteControl { agent, frame } if *agent == LEAD
                && matches!(frame.of, Some(wire::ctl_frame::Of::AgentMessage(_)))
        )),
        "{out:#?}"
    );

    // Its last awake member gone, the claim goes; the stop waits for the
    // start it follows, then is asked for once nothing else holds the tree.
    let out = rig.step(Input::ControlClosed {
        agent: LEAD,
        ended: Ended::Exited,
    });
    assert_eq!(
        workspace_jobs(&out),
        vec![WorkspaceJob::ReleaseClaim {
            workspace: wt_tree(path)
        }]
    );
    let out = rig.done(
        start,
        WorkspaceDone::Started {
            workspace: wt_tree(path),
            result: Ok(()),
        },
    );
    assert_eq!(
        workspace_jobs(&out),
        vec![WorkspaceJob::StopWhenUnheld {
            workspace: wt_tree(path)
        }]
    );
}

#[test]
fn a_workspace_that_cannot_be_made_ends_its_agent_and_a_stop_meanwhile_waits_for_it() {
    let mut rig = rig();
    let (call, out) = rig.call(create(LEAD, Some("fix"), true, "/src/shop"));
    let (plan, _) = rig.job(&out, |job| matches!(job, WorkspaceJob::Plan { .. }));
    let path = "/trees/shop/fix";
    let out = rig.done(plan, WorkspaceDone::Planned(Ok(planned(path, "fix"))));
    assert!(answer_of(&out, call).is_some());
    let (make, _) = rig.job(&out, |job| matches!(job, WorkspaceJob::Make { .. }));

    let (stop, out) = rig.call(ClientRequest::KillAgent { agent: LEAD });
    assert!(
        !out.iter()
            .any(|output| matches!(output, Output::EndCall { call, .. } if *call == stop)),
        "the kill waits in the agent's lane: {out:#?}"
    );

    let out = rig.done(
        make,
        WorkspaceDone::Made {
            workspace: wt_tree(path),
            result: Err("making the wt tree: branch `fix` already exists on origin".into()),
        },
    );
    let row = rig.row(LEAD).unwrap();
    assert_eq!(row.lifecycle, Lifecycle::Exited as i32);
    assert_eq!(row.exit_cause, ExitCause::CouldNotStart as i32);
    assert!(
        row.exit_detail
            .as_deref()
            .is_some_and(|detail| detail.contains("already exists on origin")),
        "{row:?}"
    );
    assert!(spawned(&out, LEAD).is_none());
    let stopped = answer_of(&out, stop).expect("the kill is answered after the making");
    assert_eq!(stopped.lifecycle, Lifecycle::Exited as i32);
}

#[test]
fn a_plan_that_is_refused_fails_the_create_with_no_agent_made() {
    let mut rig = rig();
    let (call, out) = rig.call(create(LEAD, None, true, "/loose"));
    let (plan, _) = rig.job(&out, |job| matches!(job, WorkspaceJob::Plan { .. }));
    let refused = wire::Error {
        code: wire::ErrorCode::FailedPrecondition as i32,
        message: "starting in a new workspace: /loose is not in a git repository".into(),
        details: Vec::new(),
    };
    let out = rig.done(
        plan,
        WorkspaceDone::Planned(Err(PlanRefusal::Refused(refused.clone()))),
    );
    assert!(
        out.contains(&Output::EndCall {
            call,
            status: Status::Failed(CallError::Registry(Refusal::Worktree(refused))),
        }),
        "{out:#?}"
    );
    assert!(rig.row(LEAD).is_none());
}

#[test]
fn an_agent_in_an_existing_folder_launches_at_once_and_deleting_the_last_member_removes_what_was_made()
 {
    let mut rig = rig();
    let git = Key {
        kind: Kind::Git,
        id: "/worktrees/shop/fix".into(),
    };
    // A worktree this host made, and an agent created in it, resolved by
    // the shell as it created the agent.
    let mut request = create(MATE, Some("mate"), false, &git.id);
    if let ClientRequest::CreateAgent(create) = &mut request {
        create.workspace = Some(Resolved {
            workspace: git.clone(),
            made_here: true,
        });
    }
    let (_, out) = rig.call(request);
    let (_, workspace) = spawned(&out, MATE).expect("a worktree needs no claim");
    assert_eq!(workspace, Some(git.clone()));
    assert_eq!(
        rig.entry(MATE).kind,
        wire::experimental::WorkspaceKind::Git as i32
    );
    assert!(
        workspace_jobs(&out).is_empty(),
        "a worktree has no hooks: {out:#?}"
    );
    rig.step(Input::AgentHello { agent: MATE });
    rig.step(Input::ControlClosed {
        agent: MATE,
        ended: Ended::Exited,
    });

    let (_, out) = rig.call(ClientRequest::DeleteAgent {
        agent: MATE,
        discard: false,
    });
    assert!(
        workspace_jobs(&out).contains(&WorkspaceJob::Delete {
            workspace: git.clone(),
            discard: false
        }),
        "{out:#?}"
    );
    assert!(rig.row(MATE).is_none());
}

/// Two agents created in a worktree this host made, both exited.
fn two_members(rig: &mut Rig) -> Key {
    let git = Key {
        kind: Kind::Git,
        id: "/worktrees/shop/pair".into(),
    };
    for (agent, name) in [(LEAD, "lead"), (MATE, "mate")] {
        let mut request = create(agent, Some(name), false, &git.id);
        if let ClientRequest::CreateAgent(create) = &mut request {
            create.workspace = Some(Resolved {
                workspace: git.clone(),
                made_here: true,
            });
        }
        rig.call(request);
        rig.step(Input::AgentHello { agent });
        rig.step(Input::ControlClosed {
            agent,
            ended: Ended::Exited,
        });
    }
    git
}

#[test]
fn consent_to_discard_covers_a_child_that_follows_its_parent() {
    let mut rig = rig();
    let git = two_members(&mut rig);
    let mut mate = rig.row(MATE).expect("mate's row");
    mate.parent = Some(AgentKey::new(OWN, LEAD.0));
    rig.daemon
        .store
        .put_agent(&mate)
        .expect("mate is lead's child");

    // The parent's delete agrees to discard; its child, in the same
    // workspace, follows it without saying so, as the family rule deletes
    // it: the consent covers the child's changes too.
    let mut jobs = Vec::new();
    let (_, out) = rig.call(ClientRequest::DeleteAgent {
        agent: LEAD,
        discard: true,
    });
    jobs.extend(workspace_jobs(&out));
    if rig.row(MATE).is_some() {
        let (_, out) = rig.call(ClientRequest::DeleteAgent {
            agent: MATE,
            discard: false,
        });
        jobs.extend(workspace_jobs(&out));
    }
    assert!(
        jobs.contains(&WorkspaceJob::Delete {
            workspace: git,
            discard: true
        }),
        "{jobs:#?}"
    );
}

#[test]
fn consent_to_discard_does_not_cover_another_agent_working_in_the_workspace() {
    let mut rig = rig();
    let git = two_members(&mut rig);

    // The first delete agrees to discard, but another agent, not its
    // descendant, still works in the workspace: nothing is removed, and the
    // consent was not this delete's to give.
    let (_, out) = rig.call(ClientRequest::DeleteAgent {
        agent: MATE,
        discard: true,
    });
    assert!(workspace_jobs(&out).is_empty(), "{out:#?}");
    let (_, out) = rig.call(ClientRequest::DeleteAgent {
        agent: LEAD,
        discard: false,
    });
    assert!(
        workspace_jobs(&out).contains(&WorkspaceJob::Delete {
            workspace: git,
            discard: false
        }),
        "the other agent's work was lost to a consent that never covered it: {out:#?}"
    );
}

#[test]
fn without_consent_the_last_members_delete_keeps_the_workspaces_changes() {
    let mut rig = rig();
    let git = two_members(&mut rig);
    let mut jobs = Vec::new();
    for agent in [MATE, LEAD] {
        let (_, out) = rig.call(ClientRequest::DeleteAgent {
            agent,
            discard: false,
        });
        jobs.extend(workspace_jobs(&out));
    }
    assert!(
        jobs.contains(&WorkspaceJob::Delete {
            workspace: git,
            discard: false
        }),
        "{jobs:#?}"
    );
}

#[test]
fn a_restarted_daemon_resolves_its_agents_running_first_then_sweeps_what_it_made() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store();
    let tree = wt_tree("/trees/shop/lead");
    let orphan = Key {
        kind: Kind::Git,
        id: "/worktrees/shop/gone".into(),
    };
    for (agent, cwd, live) in [
        (IDLE, "/src/notes", false),
        (LEAD, tree.id.as_str(), true),
        (MATE, tree.id.as_str(), false),
    ] {
        let mut row = AgentRow::new(AgentKey::new(OWN, agent.0), wire::Kind::Codex, cwd);
        if !live {
            row.lifecycle = Lifecycle::Exited as i32;
        }
        store.put_agent(&row).unwrap();
    }
    let mut rig = Rig {
        daemon: daemon(store, true, &dir),
        next_call: 0,
        _dir: dir,
    };
    // The sweep found the lead's process running.
    rig.step(Input::Adopted { agent: LEAD });
    let out = rig.step(Input::Swept);
    let jobs = workspace_jobs(&out);
    assert_eq!(
        jobs,
        vec![
            WorkspaceJob::Resolve {
                folder: tree.id.clone()
            },
            WorkspaceJob::Resolve {
                folder: "/src/notes".into()
            },
            WorkspaceJob::Owned,
        ],
        "one job per folder, the running agent's first"
    );
    let ids: Vec<JobId> = out
        .iter()
        .filter_map(|output| match output {
            Output::Job {
                id,
                op: Job::Workspace(_),
            } => Some(*id),
            _ => None,
        })
        .collect();

    // A resume asked for before its agent is resolved waits for it.
    let out = rig.done(
        ids[0],
        WorkspaceDone::Resolved {
            folder: tree.id.clone(),
            resolved: Resolved {
                workspace: tree.clone(),
                made_here: true,
            },
        },
    );
    // The running member retakes the claim, and the tree is started again:
    // nothing says whether the daemon before finished starting it.
    let (claim, _) = rig.job(&out, |job| matches!(job, WorkspaceJob::TakeClaim { .. }));
    let out = rig.done(
        claim,
        WorkspaceDone::Claimed {
            workspace: tree.clone(),
            result: Ok(()),
        },
    );
    assert!(workspace_jobs(&out).contains(&WorkspaceJob::Start {
        workspace: tree.clone()
    }));
    assert_eq!(rig.entry(MATE).id, tree.id);

    rig.done(
        ids[1],
        WorkspaceDone::Resolved {
            folder: "/src/notes".into(),
            resolved: Resolved {
                workspace: Key {
                    kind: Kind::Folder,
                    id: "/src/notes".into(),
                },
                made_here: false,
            },
        },
    );
    // What this host made that has no member left goes, keeping changes.
    let out = rig.done(
        ids[2],
        WorkspaceDone::Owned(vec![tree.clone(), orphan.clone()]),
    );
    assert_eq!(
        workspace_jobs(&out),
        vec![WorkspaceJob::Delete {
            workspace: orphan,
            discard: false
        }]
    );
    assert_eq!(
        rig.entry(IDLE).kind,
        wire::experimental::WorkspaceKind::Folder as i32
    );
}

#[test]
fn a_daemon_restarted_while_it_made_a_workspace_ends_that_agent_as_could_not_start() {
    let mut store = store();
    let key = AgentKey::new(OWN, LEAD.0);
    store
        .put_agent(&AgentRow::new(
            key.clone(),
            wire::Kind::Codex,
            "/trees/shop/fix",
        ))
        .unwrap();
    let creating = to_entry(&View {
        workspace: wt_tree("/trees/shop/fix"),
        made_here: true,
        creating: true,
        problem: None,
    });
    store
        .put_experimental(&key, VERSION, Some(&creating.encode_to_vec()))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut rig = Rig {
        daemon: daemon(store, true, &dir),
        next_call: 0,
        _dir: dir,
    };
    rig.step(Input::ControlClosed {
        agent: LEAD,
        ended: Ended::NoDirectory,
    });
    let row = rig.row(LEAD).unwrap();
    assert_eq!(row.exit_cause, ExitCause::CouldNotStart as i32, "{row:?}");
    assert!(row.exit_detail.unwrap().contains("restarted"));
}

#[test]
fn a_hook_that_fails_or_an_environment_that_does_not_load_is_shown_on_the_agent() {
    let mut rig = rig();
    let path = "/trees/shop/lead";
    rig.launched_in_new_tree(LEAD, "lead", path);
    let out = rig.step(Input::WorkspaceEnvironment {
        agent: LEAD,
        problem: Some("its environment did not load: .wt.toml: bad".into()),
    });
    assert!(
        rig.entry(LEAD)
            .problem
            .is_some_and(|problem| problem.contains("did not load")),
        "{out:#?}"
    );
    rig.step(Input::WorkspaceEnvironment {
        agent: LEAD,
        problem: None,
    });
    assert_eq!(rig.entry(LEAD).problem, None, "a later load clears it");
}

#[test]
fn without_the_experiment_a_new_worktree_is_made_before_the_row_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let mut rig = Rig {
        daemon: daemon(store(), false, &dir),
        next_call: 0,
        _dir: dir,
    };
    rig.step(Input::Swept);
    let (_, out) = rig.call(create(LEAD, Some("fix"), true, "/src/shop"));
    assert!(
        out.iter().any(|output| matches!(
            output,
            Output::Job {
                op: Job::MakeWorktree { .. },
                ..
            }
        )),
        "{out:#?}"
    );
    assert!(workspace_jobs(&out).is_empty());
    assert!(rig.row(LEAD).is_none());
    // An agent in an existing folder starts at once, in no workspace.
    let (_, out) = rig.call(create(MATE, Some("mate"), false, "/src/shop"));
    assert_eq!(spawned(&out, MATE).unwrap().1, None);
    assert!(rig.row(MATE).unwrap().experimental.is_empty());
}

#[test]
fn a_replica_row_carries_its_origins_entry_only_where_the_experiment_runs() {
    let entry = to_entry(&View {
        workspace: wt_tree("/trees/shop/far"),
        made_here: true,
        creating: false,
        problem: None,
    })
    .encode_to_vec();
    for on in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon(store(), on, &dir);
        let agent = wire::Agent {
            host_id: b"far-host-0000000".to_vec(),
            agent_id: b"theirs".to_vec(),
            name: "far".into(),
            lifecycle: Lifecycle::Live as i32,
            experimental: [(VERSION.to_owned(), entry.clone())].into(),
            ..wire::Agent::default()
        };
        let mut out = Vec::new();
        daemon.put_replica_row(&agent, &mut out).unwrap();
        let held = daemon
            .store
            .agent(&AgentKey::new(
                agent.host_id.clone(),
                agent.agent_id.clone(),
            ))
            .unwrap()
            .unwrap();
        let served = crate::serve::to_wire(&held);
        if on {
            assert_eq!(served.experimental[VERSION], entry);
        } else {
            assert!(served.experimental.is_empty());
        }
    }
}

#[test]
fn a_checkpoint_carries_the_workspaces_and_restores_them() {
    let mut rig = rig();
    rig.launched_in_new_tree(LEAD, "lead", "/trees/shop/lead");
    let checkpoint = rig.daemon.checkpoint();
    let dir = tempfile::tempdir().unwrap();
    let restored = Daemon::restore(&checkpoint, store(), dir.path().to_path_buf()).unwrap();
    assert_eq!(restored.workspaces(), rig.daemon.workspaces());
    assert!(
        restored
            .workspaces()
            .core()
            .workspace_of(MemberId(LEAD.0))
            .is_some()
    );
}
