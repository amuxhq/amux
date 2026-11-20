//! The reviews experiment's service, Reviews1: a person's review of one of
//! this host's agents' changes.
//!
//! Mounted only while this host runs the experiment, so a host without it
//! answers every call "unimplemented", as it answers any service it does
//! not serve, and a client reads that as the review not being offered
//! there. Never mounted on an agent's tools socket, and an agent's call
//! that arrives another way (carried from a paired host) is refused:
//! reviews are written by people, and an agent reads what it is sent.
//! Answered only for this host's own agents, whose working trees and
//! snapshots are here: a copy of another host's agent is not found,
//! naming the host that runs it.

use std::sync::{Arc, Weak};

use store::AgentRow;
use tonic::service::RoutesBuilder;
use tonic::{Request, Response, Status};
use wire::ErrorCode;
use wire::experimental::reviews1_server::Reviews1;
use wire::experimental::{
    ReviewEditRequest, ReviewGetFilesRequest, ReviewGetFilesResponse, ReviewOpenRequest,
    ReviewSummary,
};

use crate::api::Entry;
use crate::api::caller::{Caller, for_a_caller};
use crate::grpc::{agent_id, status, upgrade, wire_error};
use crate::runtime::ProfileRuntime;

/// Mounts Reviews1 for `entry`: a person's entries and links, while this
/// host runs the experiment. Whether it does is fixed when the daemon
/// starts, before any entry is served.
pub(in crate::api) fn mount(
    routes: &mut RoutesBuilder,
    runtime: &Weak<ProfileRuntime>,
    entry: Entry,
) {
    match entry {
        Entry::Profile | Entry::Phone | Entry::Link => {}
        Entry::Tools(_) => return,
    }
    let on = runtime.upgrade().is_some_and(|runtime| {
        runtime
            .launch()
            .experiments
            .is_on(&settings::experiments::REVIEWS)
    });
    if on {
        routes.add_service(for_a_caller(wire::experimental::reviews1_server(Reviews {
            runtime: runtime.clone(),
        })));
    }
}

#[derive(Clone)]
struct Reviews {
    runtime: Weak<ProfileRuntime>,
}

/// What a call is answered with: the runtime, the own agent's row the
/// call names, the request, and the person calling.
type Asked<T> = (Arc<ProfileRuntime>, AgentRow, T, Caller);

impl Reviews {
    /// `request` as a person asks it of one of this host's own agents,
    /// named by `agent`; refused for an agent, and for an agent this host
    /// only holds a copy of.
    async fn asked<T>(
        &self,
        request: Request<T>,
        agent: impl FnOnce(&T) -> &[u8],
    ) -> Result<Asked<T>, Status> {
        let caller = Caller::of(&request)?;
        if let Caller::Agent { .. } = caller {
            return Err(status(wire_error(
                ErrorCode::PermissionDenied,
                "reviews are written by people",
            )));
        }
        let runtime = upgrade(&self.runtime)?;
        let request = request.into_inner();
        let row = runtime
            .own_row(agent_id(agent(&request))?)
            .await
            .map_err(|error| status(error.to_wire()))?;
        Ok((runtime, row, request, caller))
    }
}

#[tonic::async_trait]
impl Reviews1 for Reviews {
    async fn open(
        &self,
        request: Request<ReviewOpenRequest>,
    ) -> Result<Response<ReviewSummary>, Status> {
        let (runtime, row, request, caller) = self
            .asked(request, |request| request.agent_id.as_slice())
            .await?;
        crate::reviews::open(&runtime, row, request, &caller)
            .await
            .map(Response::new)
    }

    async fn get_files(
        &self,
        request: Request<ReviewGetFilesRequest>,
    ) -> Result<Response<ReviewGetFilesResponse>, Status> {
        let (runtime, row, request, caller) = self
            .asked(request, |request| request.agent_id.as_slice())
            .await?;
        crate::reviews::files::get_files(&runtime, row, request, &caller)
            .await
            .map(Response::new)
    }

    async fn edit(
        &self,
        request: Request<ReviewEditRequest>,
    ) -> Result<Response<ReviewSummary>, Status> {
        let (runtime, row, request, caller) = self
            .asked(request, |request| request.agent_id.as_slice())
            .await?;
        crate::reviews::edit(&runtime, row, request, &caller)
            .await
            .map(Response::new)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use store::AgentKey;
    use wire::experimental::reviews1_client;

    use super::*;
    use crate::api::testing::{Calling, Served, served_as};
    use crate::{AgentId, HostId};

    /// A host with one agent of its own, `own`, and a copy of `away`, which
    /// the paired host `paired` runs; both exited.
    struct Host {
        _root: tempfile::TempDir,
        daemon: Option<crate::Daemon>,
        runtime: Arc<ProfileRuntime>,
        here: HostId,
        paired: HostId,
        own: AgentId,
        away: AgentId,
    }

    impl Host {
        /// The host, running the reviews experiment when `reviews`.
        async fn start(reviews: bool) -> Self {
            let root = tempfile::tempdir().unwrap();
            let work = root.path().join("work");
            std::fs::create_dir_all(&work).unwrap();
            let data_dir = root.path().join("data");
            let here = crate::create_profile(&data_dir).unwrap();
            let profile = crate::profile_dir(&data_dir, here);
            let paired = HostId::new_v4();
            let [own, away] = std::array::from_fn(|_| AgentId::new_v4());
            let mut store = store::Sqlite::open(
                &profile.join(crate::install::STORE),
                here.as_bytes().to_vec(),
            )
            .unwrap();
            for (host, agent, name) in [(here, own, "own"), (paired, away, "away")] {
                let mut row = AgentRow::new(
                    AgentKey::new(host.as_bytes().to_vec(), agent.as_bytes().to_vec()),
                    wire::Kind::ClaudeSdk,
                    work.to_string_lossy(),
                );
                row.name = name.to_owned();
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
            // Paired, so its copy is kept: never reached in these tests.
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

            let mut launch = crate::Launch {
                install_path: "/nonexistent/amux".into(),
                step_budget: daemon_core::StepBudget::UNLIMITED,
                agent_step_budget_micros: daemon_core::StepBudget::UNLIMITED.micros,
                ..crate::Launch::default()
            };
            if reviews {
                launch.experiments =
                    settings::experiments::Enabled::of(&[&settings::experiments::REVIEWS]);
            }
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
                own,
                away,
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

        /// Each of the service's calls about `agent`, made on `served` as
        /// `calling`, in the order open, get files, edit.
        async fn every_call(
            &self,
            served: Served,
            calling: Calling,
            agent: AgentId,
        ) -> [Result<(), Status>; 3] {
            let mut client = reviews1_client(served_as(&self.runtime, served, calling));
            let agent_id = agent.as_bytes().to_vec();
            let open = client
                .open(ReviewOpenRequest {
                    agent_id: agent_id.clone(),
                    ..Default::default()
                })
                .await
                .map(drop);
            let files = client
                .get_files(ReviewGetFilesRequest {
                    agent_id: agent_id.clone(),
                    ..Default::default()
                })
                .await
                .map(drop);
            let edit = client
                .edit(ReviewEditRequest {
                    agent_id,
                    ..Default::default()
                })
                .await
                .map(drop);
            [open, files, edit]
        }

        /// Open about `agent`, made on `served` as `calling`.
        async fn open(
            &self,
            served: Served,
            calling: Calling,
            agent: AgentId,
        ) -> Result<(), Status> {
            let [open, ..] = self.every_call(served, calling, agent).await;
            open
        }

        /// Every entry a person comes in on, with the person on it.
        fn people(&self) -> [(Served, Calling); 3] {
            [
                (
                    Served::Profile,
                    Calling::PersonHere(analytics::Client::Terminal),
                ),
                (Served::Phone, Calling::PersonHere(analytics::Client::Phone)),
                (
                    Served::Link,
                    Calling::PersonAt(self.paired, analytics::Client::Mac),
                ),
            ]
        }

        /// What an entry answers a call to a service it does not serve: the
        /// peer service, which only a link serves, asked on the profile's
        /// socket.
        async fn unserved(&self) -> (tonic::Code, String) {
            let calling = Calling::PersonHere(analytics::Client::Terminal);
            let mut peer =
                wire::peer_service_client(served_as(&self.runtime, Served::Profile, calling));
            let refused = peer
                .get_blob(wire::PeerServiceGetBlobRequest::default())
                .await
                .unwrap_err();
            (refused.code(), refused.message().to_owned())
        }

        /// The agent `own`, calling on its tools socket.
        fn own_agent(&self) -> Calling {
            Calling::Agent {
                host: self.here,
                agent: self.own,
                name: "own".to_owned(),
            }
        }
    }

    fn code(answer: &Result<(), Status>) -> Option<tonic::Code> {
        answer.as_ref().err().map(Status::code)
    }

    /// The code and words `answer` was refused with.
    fn refusal(answer: &Result<(), Status>) -> Option<(tonic::Code, String)> {
        answer
            .as_ref()
            .err()
            .map(|refused| (refused.code(), refused.message().to_owned()))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn with_the_experiment_off_every_call_is_unimplemented() {
        let host = Host::start(false).await;
        let unserved = host.unserved().await;
        assert_eq!(unserved.0, tonic::Code::Unimplemented);
        for (served, calling) in host.people() {
            for answer in host.every_call(served, calling.clone(), host.own).await {
                assert_eq!(
                    refusal(&answer).as_ref(),
                    Some(&unserved),
                    "{served:?} {calling:?}"
                );
            }
        }
        host.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn on_an_agents_tools_socket_every_call_is_unimplemented() {
        let host = Host::start(true).await;
        let unserved = host.unserved().await;
        let tools = Served::Tools(host.own);
        for answer in host.every_call(tools, host.own_agent(), host.own).await {
            assert_eq!(refusal(&answer).as_ref(), Some(&unserved));
        }
        host.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_is_refused_wherever_its_call_arrives() {
        let host = Host::start(true).await;
        let afar = Calling::Agent {
            host: host.paired,
            agent: AgentId::new_v4(),
            name: "remote".to_owned(),
        };
        for (served, calling) in [
            (Served::Link, afar),
            (Served::Profile, host.own_agent()),
            (Served::Phone, host.own_agent()),
        ] {
            for answer in host.every_call(served, calling.clone(), host.own).await {
                let refused = answer.unwrap_err();
                assert_eq!(
                    refused.code(),
                    tonic::Code::PermissionDenied,
                    "{served:?} {calling:?}: {refused:?}"
                );
                assert!(
                    refused.message().contains("people"),
                    "{}",
                    refused.message()
                );
            }
        }
        host.stop().await;
    }

    /// A person on every entry gets past the service's own refusals to the
    /// review itself, which answers otherwise than a host that does not
    /// serve the service.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_person_reaches_the_review_on_every_entry() {
        let host = Host::start(true).await;
        let unserved = host.unserved().await;
        for (served, calling) in host.people() {
            let answer = host.open(served, calling.clone(), host.own).await;
            assert!(
                !matches!(
                    code(&answer),
                    Some(tonic::Code::PermissionDenied | tonic::Code::NotFound)
                ),
                "{served:?} {calling:?}: {answer:?}"
            );
            assert_ne!(
                refusal(&answer).as_ref(),
                Some(&unserved),
                "{served:?} {calling:?} is answered as if nothing served reviews"
            );
        }
        host.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_copy_is_not_found_and_names_the_host_that_runs_it() {
        let host = Host::start(true).await;
        for (served, calling) in host.people() {
            for answer in host.every_call(served, calling.clone(), host.away).await {
                let refused = answer.unwrap_err();
                assert_eq!(refused.code(), tonic::Code::NotFound, "{refused:?}");
                assert!(
                    refused.message().contains("runs on desk")
                        && refused.message().contains(&host.paired.to_string()),
                    "{}",
                    refused.message()
                );
            }
            let unknown = host.open(served, calling, AgentId::new_v4()).await;
            assert_eq!(code(&unknown), Some(tonic::Code::NotFound));
        }
        host.stop().await;
    }
}
