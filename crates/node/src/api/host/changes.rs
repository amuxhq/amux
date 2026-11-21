//! What changed in this host's agents' working trees: ReviewService. And
//! what deleting one would take with it, which AgentService answers
//! through [`preview_delete`].
//!
//! Both are asked of the agent's own host and read only its own rows, so
//! the rule about who may ask is checked against the authoritative row:
//! a person always, an agent only about its own children.

use std::sync::Weak;

use store::AgentRow;
use tonic::service::RoutesBuilder;
use tonic::{Request, Response, Status};
use wire::review_service_server::ReviewService;
use wire::{
    ErrorCode, GetChangesRequest, GetChangesResponse, PreviewDeleteRequest, PreviewDeleteResponse,
};

use crate::api::Entry;
use crate::api::caller::{Caller, for_a_caller};
use crate::grpc::{agent_id, status, upgrade, wire_error};
use crate::runtime::ProfileRuntime;

/// Mounts ReviewService for `entry`: every entry, since the rule about
/// the caller is the same wherever the call came in.
pub(in crate::api) fn mount(
    routes: &mut RoutesBuilder,
    runtime: &Weak<ProfileRuntime>,
    _entry: Entry,
) {
    routes.add_service(for_a_caller(wire::review_service_server(Reviews {
        runtime: runtime.clone(),
    })));
}

#[derive(Clone)]
struct Reviews {
    runtime: Weak<ProfileRuntime>,
}

#[tonic::async_trait]
impl ReviewService for Reviews {
    async fn get_changes(
        &self,
        request: Request<GetChangesRequest>,
    ) -> Result<Response<GetChangesResponse>, Status> {
        let caller = Caller::of(&request)?;
        let runtime = upgrade(&self.runtime)?;
        let request = request.into_inner();
        let row = own_agent(&runtime, &request.agent_id).await?;
        person_or_parent(
            &caller,
            &row,
            "an agent reads only its own children's changes",
        )?;
        runtime
            .changes_of(&row, request.base)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }
}

/// AgentService.PreviewDelete: what deleting the agent now would take
/// with it, for `caller`. Whoever may delete the agent may ask, by the
/// same rule as DeleteAgent's, so a warning is shown exactly to those who
/// could act on it.
pub(in crate::api) async fn preview_delete(
    runtime: &ProfileRuntime,
    request: PreviewDeleteRequest,
    caller: &Caller,
) -> Result<Response<PreviewDeleteResponse>, Status> {
    let row = own_agent(runtime, &request.agent_id).await?;
    person_or_parent(caller, &row, "an agent deletes only its own children")?;
    runtime
        .preview_delete_of(&row)
        .await
        .map(Response::new)
        .map_err(|error| status(error.to_wire()))
}

/// One of this host's own agents' rows, by the id a request names.
async fn own_agent(runtime: &ProfileRuntime, id: &[u8]) -> Result<AgentRow, Status> {
    runtime
        .own_row(agent_id(id)?)
        .await
        .map_err(|error| status(error.to_wire()))
}

/// Lets a person through, and an agent only when the row is one of its
/// own direct children: an agent acts on the agents it started and
/// reaches any other only by messaging it. The parent edge names the
/// parent's host, so an agent of a paired host passes only for a child it
/// made here. `refusal` says what was refused.
pub(in crate::api) fn person_or_parent(
    caller: &Caller,
    row: &AgentRow,
    refusal: &str,
) -> Result<(), Status> {
    match caller {
        Caller::Person { .. } => Ok(()),
        Caller::Agent { agent, .. }
            if row.parent.as_ref().is_some_and(|parent| {
                parent.host == agent.host.as_bytes() && parent.agent == agent.agent.as_bytes()
            }) =>
        {
            Ok(())
        }
        Caller::Agent { .. } => Err(status(wire_error(ErrorCode::PermissionDenied, refusal))),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    use store::AgentKey;
    use wire::get_changes_request::Base;

    use super::*;
    use crate::api::caller::{At, Surface};
    use crate::{AgentId, HostId};

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=amux",
                "-c",
                "user.email=amux@example.invalid",
            ])
            .args(["-c", "commit.gpgsign=false", "-c", "core.fsmonitor=false"])
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }

    /// A host whose rows hold its agent `lead`, lead's child `coder`, an
    /// agent `other` of no one's, `helper`, a child an agent of the paired
    /// host `paired` made here, and a copy of `away`, which the paired
    /// host runs. Every one of them exited, in one repository's folder.
    struct Host {
        _root: tempfile::TempDir,
        daemon: Option<crate::Daemon>,
        runtime: Arc<ProfileRuntime>,
        here: HostId,
        paired: HostId,
        lead: AgentId,
        coder: AgentId,
        other: AgentId,
        helper: AgentId,
        away: AgentId,
        remote: AgentId,
    }

    impl Host {
        async fn start() -> Self {
            let root = tempfile::tempdir().unwrap();
            let work = root.path().join("work");
            std::fs::create_dir_all(&work).unwrap();
            git(&work, &["init", "-q", "-b", "main"]);
            std::fs::write(work.join("kept.txt"), "one\n").unwrap();
            git(&work, &["add", "."]);
            git(&work, &["commit", "-q", "-m", "first"]);
            std::fs::write(work.join("kept.txt"), "one\ntwo\n").unwrap();

            let data_dir = root.path().join("data");
            let here = crate::create_profile(&data_dir).unwrap();
            let profile = crate::profile_dir(&data_dir, here);
            let paired = HostId::new_v4();
            let [lead, coder, other, helper, away, remote] =
                std::array::from_fn(|_| AgentId::new_v4());
            let key = |host: HostId, agent: AgentId| {
                AgentKey::new(host.as_bytes().to_vec(), agent.as_bytes().to_vec())
            };
            let mut store = store::Sqlite::open(
                &profile.join(crate::install::STORE),
                here.as_bytes().to_vec(),
            )
            .unwrap();
            for (host, agent, name, parent) in [
                (here, lead, "lead", None),
                (here, coder, "coder", Some(key(here, lead))),
                (here, other, "other", None),
                (here, helper, "helper", Some(key(paired, remote))),
                (paired, away, "away", None),
            ] {
                let mut row = AgentRow::new(
                    key(host, agent),
                    wire::Kind::ClaudeSdk,
                    work.to_string_lossy(),
                );
                row.name = name.to_owned();
                row.parent = parent;
                row.lifecycle = wire::Lifecycle::Exited as i32;
                row.exit_cause = wire::ExitCause::Finished as i32;
                row.incarnation = 1;
                row.created_at_ms = 1;
                store.put_agent(&row).unwrap();
                if host == here {
                    let dir = profile.join(crate::install::AGENTS).join(agent.to_string());
                    std::fs::create_dir_all(&dir).unwrap();
                    journal::synthetic::SyntheticWriter::open(
                        dir.join(agent_dir::JOURNAL),
                        1 << 20,
                    )
                    .unwrap();
                }
            }
            // Paired, so its copies are kept: never reached in these tests.
            store
                .put_trusted_host(
                    &store::TrustedHost {
                        host_id: paired.as_bytes().to_vec(),
                        pubkey: vec![7; 32],
                        name: "desk".to_owned(),
                        on_account: false,
                        signed_in: None,
                        platform: 0,
                        ssh: None,
                        direct: "[]".to_owned(),
                        listed: "[]".to_owned(),
                        push_key: None,
                    },
                    store::Durability::Normal,
                )
                .unwrap();
            drop(store);

            let launch = crate::Launch {
                install_path: "/nonexistent/amux".into(),
                step_budget: daemon_core::StepBudget::UNLIMITED,
                agent_step_budget_micros: daemon_core::StepBudget::UNLIMITED.micros,
                ..crate::Launch::default()
            };
            let daemon = crate::start(
                crate::StartOptions {
                    settings: Default::default(),
                    data_dir,
                    boot_id: Some("boot-1".to_owned()),
                    launch,
                    clock: Arc::new(agent_dir::SystemClock),
                    push: Arc::new(crate::NoopSender),
                    daemon_log: None,
                    supervisor_log: None,
                    front_door: None,
                    edge: crate::EdgeOptions::default(),
                    analytics: crate::Telemetry::Off,
                    auto_reports: crate::AutoReports::off(),
                },
                None,
            )
            .await
            .unwrap();
            let runtime = daemon.profile(here).unwrap();
            Self {
                _root: root,
                daemon: Some(daemon),
                runtime,
                here,
                paired,
                lead,
                coder,
                other,
                helper,
                away,
                remote,
            }
        }

        async fn stop(self) {
            let Self {
                _root: root,
                daemon,
                runtime,
                ..
            } = self;
            drop(runtime);
            daemon.unwrap().shutdown().await.unwrap();
            drop(root);
        }

        /// GetChanges on `agent`, as `caller` asks it.
        async fn changes(
            &self,
            agent: AgentId,
            caller: &Caller,
        ) -> Result<GetChangesResponse, Status> {
            let mut request = Request::new(GetChangesRequest {
                agent_id: agent.as_bytes().to_vec(),
                base: Some(Base::Head(wire::Empty {})),
            });
            request.extensions_mut().insert(caller.clone());
            let reviews = Reviews {
                runtime: Arc::downgrade(&self.runtime),
            };
            reviews.get_changes(request).await.map(Response::into_inner)
        }

        /// PreviewDelete on `agent`, as `caller` asks it.
        async fn preview(
            &self,
            agent: AgentId,
            caller: &Caller,
        ) -> Result<PreviewDeleteResponse, Status> {
            let request = PreviewDeleteRequest {
                agent_id: agent.as_bytes().to_vec(),
            };
            preview_delete(&self.runtime, request, caller)
                .await
                .map(Response::into_inner)
        }

        /// Whether `caller` is answered about `agent` by both calls, which
        /// must agree; the refusal's code when it is not.
        async fn answers(&self, agent: AgentId, caller: &Caller) -> Result<(), tonic::Code> {
            let changes = self.changes(agent, caller).await.map(drop);
            let preview = self.preview(agent, caller).await.map(drop);
            assert_eq!(
                changes.as_ref().map_err(Status::code),
                preview.as_ref().map_err(Status::code),
                "GetChanges {changes:?} and PreviewDelete {preview:?} answer {caller:?} alike"
            );
            changes.map_err(|refused| refused.code())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_person_anywhere_is_answered_about_any_of_this_hosts_agents() {
        let host = Host::start().await;
        for caller in [
            Caller::person_here(Surface::Terminal),
            Caller::Person {
                at: At::Paired(host.paired),
                surface: Surface::Ios,
            },
        ] {
            for agent in [host.lead, host.coder, host.other, host.helper] {
                assert_eq!(host.answers(agent, &caller).await, Ok(()), "{caller:?}");
            }
        }
        let changes = host
            .changes(host.coder, &Caller::person_here(Surface::Mac))
            .await
            .unwrap();
        assert_eq!(changes.files.len(), 1, "{changes:?}");
        assert_eq!(changes.files[0].language, "");
        host.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_is_answered_only_about_its_own_children() {
        let host = Host::start().await;
        let lead = Caller::agent_of(host.here, host.lead, "lead");
        assert_eq!(host.answers(host.coder, &lead).await, Ok(()));
        for (agent, why) in [
            (host.other, "no one's child"),
            (host.lead, "itself"),
            (host.helper, "another agent's child"),
        ] {
            assert_eq!(
                host.answers(agent, &lead).await,
                Err(tonic::Code::PermissionDenied),
                "{why}"
            );
        }
        let coder = Caller::agent_of(host.here, host.coder, "coder");
        assert_eq!(
            host.answers(host.lead, &coder).await,
            Err(tonic::Code::PermissionDenied),
            "a child is not answered about its parent"
        );
        let refused = host.changes(host.other, &lead).await.unwrap_err();
        assert!(
            refused.message().contains("children"),
            "{}",
            refused.message()
        );

        // An agent of the paired host is answered about the child it made
        // here, and about no other; the same id on the paired host is
        // another agent than this host's lead.
        let remote = Caller::agent_of(host.paired, host.remote, "remote");
        assert_eq!(host.answers(host.helper, &remote).await, Ok(()));
        assert_eq!(
            host.answers(host.coder, &remote).await,
            Err(tonic::Code::PermissionDenied)
        );
        let lead_elsewhere = Caller::agent_of(host.paired, host.lead, "lead");
        assert_eq!(
            host.answers(host.coder, &lead_elsewhere).await,
            Err(tonic::Code::PermissionDenied)
        );
        host.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_copy_is_not_found_and_names_the_host_that_runs_it() {
        let host = Host::start().await;
        for caller in [
            Caller::person_here(Surface::Terminal),
            Caller::agent_of(host.here, host.lead, "lead"),
        ] {
            let changes = host.changes(host.away, &caller).await.unwrap_err();
            let preview = host.preview(host.away, &caller).await.unwrap_err();
            for refused in [&changes, &preview] {
                assert_eq!(refused.code(), tonic::Code::NotFound, "{refused:?}");
                // By the name it was paired under, and the id a call to
                // it is addressed by.
                assert!(
                    refused.message().contains("runs on desk")
                        && refused.message().contains(&host.paired.to_string()),
                    "{}",
                    refused.message()
                );
            }
            assert_eq!(
                host.answers(AgentId::new_v4(), &caller).await,
                Err(tonic::Code::NotFound)
            );
        }
        let short = PreviewDeleteRequest {
            agent_id: vec![1; 4],
        };
        let refused = preview_delete(
            &host.runtime,
            short,
            &Caller::person_here(Surface::Terminal),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.code(), tonic::Code::InvalidArgument);
        host.stop().await;
    }
}
