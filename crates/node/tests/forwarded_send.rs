//! A send to an agent on another host, forwarded to its host's daemon. A
//! send that never left this host is a definite rejection: the host cannot
//! be reached. One whose link fails after it went out may have arrived, so
//! the sender is told it was aborted and settles it at the next catch-up;
//! nothing sends it again.

use std::sync::Arc;

use node::{Daemon, Edge, Owner, ProfileRuntime};
use support::synthetic::*;
use support::*;
use wire::session_service_server::SessionService as _;
use wire::{SendInputRequest, SendInputResponse, send_input_response};

use crate::support;

const SEGMENTS: u64 = 1 << 20;

/// A desk running one synthetic agent and a laptop paired with it, linked.
struct Pair {
    desk: Install,
    laptop: Install,
    desk_daemon: Daemon,
    laptop_daemon: Daemon,
    desk_runtime: Arc<ProfileRuntime>,
    laptop_runtime: Arc<ProfileRuntime>,
    desk_edge: Arc<Edge>,
    laptop_edge: Arc<Edge>,
    agent: SyntheticAgent,
}

impl Pair {
    async fn new() -> (Self, node::LoopbackLink) {
        let desk = Install::new();
        let mut agent = SyntheticAgent::new(&desk, "coder", SEGMENTS);
        agent.register(&desk);
        agent.append(&snapshot(wire::Phase::Idle, &[], 1_000));
        agent.go_live();
        let desk_daemon = desk.start("boot-1", quiet_launch()).await;
        let desk_runtime = runtime(&desk_daemon, &desk);
        let laptop = Install::new();
        let laptop_daemon = laptop.start("boot-1", quiet_launch()).await;
        let laptop_runtime = runtime(&laptop_daemon, &laptop);
        let (desk_edge, laptop_edge) =
            (desk_runtime.edge().unwrap(), laptop_runtime.edge().unwrap());
        desk_edge.trust(&laptop_edge).await.unwrap();
        laptop_edge.trust(&desk_edge).await.unwrap();
        let pair = Self {
            desk,
            laptop,
            desk_daemon,
            laptop_daemon,
            desk_runtime,
            laptop_runtime,
            desk_edge,
            laptop_edge,
            agent,
        };
        let link = pair.link().await;
        until("the laptop to learn the desk's agent", || async {
            match pair.laptop_runtime.owner(pair.agent.id.as_bytes()).await {
                Ok(Owner::Peer(_)) => Ok(()),
                Ok(owner) => Err(format!("{owner:?}")),
                Err(error) => Err(error.to_string()),
            }
        })
        .await
        .unwrap();
        (pair, link)
    }

    async fn link(&self) -> node::LoopbackLink {
        let link = self.laptop_edge.link_in_process(&self.desk_edge).unwrap();
        assert!(
            self.laptop_edge
                .wait_for_route(self.desk_runtime.host(), PATIENCE)
                .await
        );
        link
    }

    /// What a person on the laptop sends the desk's agent.
    fn send(&self, id: &[u8], text: &str) -> tonic::Request<SendInputRequest> {
        tonic::Request::new(SendInputRequest {
            agent_id: self.agent.id.as_bytes().to_vec(),
            input: Some(sdk_prompt(id, text)),
        })
    }

    fn sent_ids(&self) -> Vec<Vec<u8>> {
        self.agent
            .inputs()
            .into_iter()
            .map(|handed| handed.id().to_vec())
            .collect()
    }

    async fn stop(mut self) {
        self.agent.die().await;
        let Self {
            desk_daemon,
            laptop_daemon,
            desk_runtime,
            laptop_runtime,
            desk_edge,
            laptop_edge,
            desk,
            laptop,
            ..
        } = self;
        drop((desk_runtime, laptop_runtime, desk_edge, laptop_edge));
        desk_daemon.shutdown().await.unwrap();
        laptop_daemon.shutdown().await.unwrap();
        drop((desk, laptop));
    }
}

fn verdict(answer: &SendInputResponse) -> String {
    match &answer.of {
        Some(send_input_response::Of::Accepted(accepted)) if accepted.queued => "queued".into(),
        Some(send_input_response::Of::Accepted(_)) => "accepted".into(),
        Some(send_input_response::Of::Rejected(rejected)) => {
            format!("rejected {:?}", rejected.refusal())
        }
        None => "empty".into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_cut_off_after_it_left_is_uncertain_and_arrives_once() {
    let (pair, link) = Pair::new().await;
    let laptop = node::ClientApi::new(&pair.laptop_runtime, None);

    // The desk's agent takes the prompt in and has not answered when the
    // link drops under the call.
    pair.agent.freeze();
    let sending = tokio::spawn({
        let request = pair.send(b"p1", "look at the logs");
        let laptop = laptop.clone();
        async move { laptop.send_input(request).await }
    });
    until("the prompt to reach the desk's agent", || async {
        match pair.sent_ids().len() {
            1 => Ok(()),
            n => Err(format!("{n} inputs")),
        }
    })
    .await
    .unwrap();
    link.sever();
    drop(link);
    let answer = tokio::time::timeout(PATIENCE, sending)
        .await
        .expect("the cut call answers")
        .unwrap();
    match answer {
        Ok(answer) => panic!(
            "a send that went out came back {}, as if it never arrived",
            verdict(answer.get_ref())
        ),
        Err(status) => assert_eq!(
            status.code(),
            tonic::Code::Aborted,
            "a send that may have arrived is uncertain: {status:?}"
        ),
    }

    // The link comes back and the agent answers what it took in. Nothing
    // sends the first prompt again; the next one goes through.
    let _link = pair.link().await;
    pair.agent.thaw();
    let answer = laptop
        .send_input(pair.send(b"p2", "and the config"))
        .await
        .unwrap();
    assert_eq!(verdict(answer.get_ref()), "queued");
    assert_eq!(
        pair.sent_ids(),
        vec![b"p1".to_vec(), b"p2".to_vec()],
        "the cut prompt arrived once"
    );
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_with_no_link_never_leaves_and_is_rejected() {
    let (pair, link) = Pair::new().await;
    let laptop = node::ClientApi::new(&pair.laptop_runtime, None);

    link.sever();
    drop(link);
    until("the laptop to lose its route to the desk", || async {
        let answer = laptop
            .send_input(pair.send(b"p1", "look at the logs"))
            .await
            .map_err(|status| format!("{status:?}"))?;
        match verdict(answer.get_ref()).as_str() {
            "rejected HostUnreachable" => Ok(()),
            other => Err(other.to_owned()),
        }
    })
    .await
    .unwrap();
    assert!(
        pair.sent_ids().is_empty(),
        "a rejected send never reached the agent"
    );
    pair.stop().await;
}

/// An input crosses daemons as bytes: an arm or field the daemons do not
/// know reaches a newer agent exactly as its client encoded it, whether
/// the person is on the agent's own host or on a paired one.
#[tokio::test(flavor = "multi_thread")]
async fn unknown_input_arm_survives_forwarding() {
    use prost::Message as _;
    let (pair, _link) = Pair::new().await;
    // A prompt with a field 99 no version of the message has, then an arm
    // 99 alone: varint key 99 << 3, value 1.
    let unknown = [0x98, 0x06, 0x01];
    let prompt = wire::ClaudeSdkInput {
        of: Some(wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
            text: "with something new".into(),
            attachments: Vec::new(),
        })),
    }
    .encode_to_vec();
    let inputs = [
        wire::Input {
            input_id: b"on-the-desk".to_vec(),
            kind: wire::Kind::ClaudeSdk as i32,
            body: [prompt.as_slice(), &unknown].concat(),
        },
        wire::Input {
            input_id: b"from-the-laptop".to_vec(),
            kind: wire::Kind::ClaudeSdk as i32,
            body: unknown.to_vec(),
        },
    ];
    let desk = node::ClientApi::new(&pair.desk_runtime, None);
    let laptop = node::ClientApi::new(&pair.laptop_runtime, None);
    for (api, input) in [(&desk, &inputs[0]), (&laptop, &inputs[1])] {
        let answer = api
            .send_input(tonic::Request::new(SendInputRequest {
                agent_id: pair.agent.id.as_bytes().to_vec(),
                input: Some(input.clone()),
            }))
            .await
            .unwrap();
        assert_eq!(verdict(answer.get_ref()), "queued");
    }
    let handed = pair
        .agent
        .inputs()
        .into_iter()
        .filter_map(|handed| match handed {
            Handed::Input(input) => Some(input.encode_to_vec()),
            Handed::Message(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        handed,
        inputs
            .iter()
            .map(|input| input.encode_to_vec())
            .collect::<Vec<_>>(),
        "the agent got the bytes its clients sent"
    );
    pair.stop().await;
}
