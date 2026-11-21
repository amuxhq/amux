//! Acts a paired host carries to this host's AgentService over a real
//! link, with the caller header its node writes: the host answering reads
//! who is calling from the link's handshake and the header, and judges a
//! calling agent's parent edge against the copy it keeps of that host.

use std::sync::Arc;

use node::{Daemon, Edge, LoopbackLink, ProfileRuntime};
use support::synthetic::*;
use support::*;
use tonic::Code;
use tonic::codegen::http;
use tonic::metadata::MetadataMap;
use tonic::transport::Channel;

use crate::host_acts::{SEGMENTS, create};
use crate::support;

/// A desk whose agents the calls act on, and a laptop paired with it and
/// linked, running two agents of its own: one live, one that has exited.
struct Pair {
    desk: Install,
    laptop: Install,
    desk_daemon: Daemon,
    laptop_daemon: Daemon,
    desk_runtime: Arc<ProfileRuntime>,
    laptop_runtime: Arc<ProfileRuntime>,
    desk_edge: Arc<Edge>,
    laptop_edge: Arc<Edge>,
    live: SyntheticAgent,
    ended: SyntheticAgent,
    _link: LoopbackLink,
}

impl Pair {
    async fn new() -> Self {
        let desk = Install::new();
        let desk_daemon = desk.start("boot-1", desk.launch("idle", Vec::new())).await;
        let desk_runtime = runtime(&desk_daemon, &desk);
        let laptop = Install::new();
        let mut live = SyntheticAgent::new(&laptop, "live", SEGMENTS);
        live.register(&laptop);
        live.go_live();
        let ended = SyntheticAgent::new(&laptop, "ended", SEGMENTS);
        ended.register(&laptop);
        let laptop_daemon = laptop.start("boot-1", quiet_launch()).await;
        let laptop_runtime = runtime(&laptop_daemon, &laptop);
        let (desk_edge, laptop_edge) =
            (desk_runtime.edge().unwrap(), laptop_runtime.edge().unwrap());
        desk_edge.trust(&laptop_edge).await.unwrap();
        laptop_edge.trust(&desk_edge).await.unwrap();
        let link = laptop_edge.link_in_process(&desk_edge).unwrap();
        assert!(wait_for_route(&laptop_edge, desk_runtime.host(), PATIENCE).await);
        let pair = Self {
            desk,
            laptop,
            desk_daemon,
            laptop_daemon,
            desk_runtime,
            laptop_runtime,
            desk_edge,
            laptop_edge,
            live,
            ended,
            _link: link,
        };
        // The desk's copy of the laptop says the one agent has ended.
        until("the desk to hold the laptop's agents", || async {
            let ended = pair.ended.id.as_bytes().to_vec();
            let row = pair
                .desk_runtime
                .read_store(move |store| store.agent_by_id(&ended).unwrap())
                .await
                .unwrap();
            match row {
                Some(row) if row.lifecycle == wire::Lifecycle::Exited as i32 => Ok(()),
                other => Err(format!("{other:?}")),
            }
        })
        .await
        .unwrap();
        pair
    }

    /// The link's channel from the laptop to the desk.
    async fn to_desk(&self) -> Channel {
        self.laptop_edge
            .channel(self.desk_runtime.host())
            .await
            .unwrap()
    }

    async fn stop(self) {
        kill_all(&self.desk_runtime).await;
        let Self {
            desk,
            laptop,
            desk_daemon,
            laptop_daemon,
            desk_runtime,
            laptop_runtime,
            desk_edge,
            laptop_edge,
            live,
            ended,
            _link,
        } = self;
        drop((desk_runtime, laptop_runtime, desk_edge, laptop_edge, _link));
        desk_daemon.shutdown().await.unwrap();
        laptop_daemon.shutdown().await.unwrap();
        drop((live, ended, desk, laptop));
    }
}

/// `message` as the laptop's node carries it for `caller`, the header it
/// writes beside it.
fn carried<T>(message: T, caller: &wire::Caller) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    let mut headers = http::HeaderMap::new();
    headers.insert(wire::CALLER_HEADER, caller.to_header().parse().unwrap());
    *request.metadata_mut() = MetadataMap::from_headers(headers);
    request
}

/// An agent of the laptop is the parent of what it starts on the desk,
/// named by the laptop's id, which only the link's handshake can say; it
/// acts on that child and on no other agent there. Once the desk has
/// caught up with the laptop and sees an agent ended, a create for it is
/// refused, as one for an ended agent of the desk's own is. A person at
/// the laptop acts on any of the desk's agents.
#[tokio::test(flavor = "multi_thread")]
async fn a_paired_hosts_agent_acts_over_the_link_only_on_its_own_children() {
    let pair = Pair::new().await;
    let laptop = pair.laptop_runtime.host();
    let mut desk = wire::agent_service_client(pair.to_desk().await);
    let live = wire::Caller::agent(pair.live.id.as_bytes().to_vec(), "live");
    let ended = wire::Caller::agent(pair.ended.id.as_bytes().to_vec(), "ended");
    let person = wire::Caller::person("phone");
    let work = pair.desk.work.clone();

    let child = desk
        .create_agent(carried(create(&work, "laptops-child", None), &live))
        .await
        .unwrap()
        .into_inner()
        .agent
        .unwrap();
    let parent = child.parent.clone().unwrap();
    assert_eq!(
        parent.host_id,
        laptop.as_bytes(),
        "named by the link's host"
    );
    assert_eq!(parent.agent_id, pair.live.id.as_bytes());
    let stranger = desk
        .create_agent(carried(create(&work, "the-persons", None), &person))
        .await
        .unwrap()
        .into_inner()
        .agent
        .unwrap();
    assert_eq!(stranger.parent, None, "a person's agent has no parent");

    let rename = |agent: &wire::Agent, name: &str| wire::RenameAgentRequest {
        agent_id: agent.agent_id.clone(),
        name: name.to_owned(),
    };
    let refused = desk
        .rename_agent(carried(rename(&stranger, "taken-over"), &live))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), Code::PermissionDenied, "{refused:?}");
    desk.rename_agent(carried(rename(&child, "renamed-by-parent"), &live))
        .await
        .unwrap();
    desk.rename_agent(carried(rename(&stranger, "renamed-by-person"), &person))
        .await
        .unwrap();

    // The desk judges an ended parent only from a caught-up copy, so a
    // create may still be made while the copy catches up; each one made is
    // stopped by the family rule.
    let ended_id = pair.ended.id;
    let refused = until("a create for the ended agent to be refused", || {
        let mut desk = desk.clone();
        let name = format!(
            "too-late-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let request = carried(create(&work, &name, None), &ended);
        async move {
            match desk.create_agent(request).await {
                Err(refused) => Ok(refused),
                Ok(made) => Err(format!("made {:?}", made.into_inner().agent)),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
    assert_eq!(
        refused.message(),
        format!("agent {ended_id} has ended and starts no more agents")
    );
    pair.stop().await;
}
