//! Calls a person on the laptop addresses to the desk, carried there by the
//! laptop's node over the link, unread. The laptop's profile socket and
//! its phone entry route a call whose authority is the desk's id to the
//! carrier, which passes it on with the caller header and turns what the
//! transport did into the same statuses for every service; the desk
//! answers it on the link as its own host services answer, for the person
//! the header names at the laptop.

use std::sync::Arc;

use analytics::{Origin, Recording};
use node::link_log::{CarriedCall, CarriedOutcome};
use node::{Daemon, Edge, LoopbackLink, Owner, ProfileRuntime, StartOptions, Telemetry};
use prost::Message as _;
use support::synthetic::*;
use support::*;
use tonic::Request;
use tonic::transport::Channel;
use wire::amux::staging::{
    self as staging, FollowAgentsRequest, PinFolderRequest, PutBlobRequest, SendInputRequest,
};
use wire::{SendInputResponse, send_input_response};

use crate::support;

const SEGMENTS: u64 = 1 << 20;

/// A desk running one synthetic agent, its analytics recorded, and a
/// laptop paired with it and linked, both serving their profile sockets.
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
    recording: Arc<Recording>,
}

impl Pair {
    async fn new() -> (Self, LoopbackLink) {
        let desk = Install::new();
        let code = desk.path("code");
        std::fs::create_dir_all(code.join("docs")).unwrap();
        let mut agent = SyntheticAgent::new(&desk, "coder", SEGMENTS);
        agent.register(&desk);
        agent.append(&snapshot(wire::Phase::Idle, &[], 1_000));
        agent.go_live();
        let recording = Recording::new();
        let launch = quiet_launch();
        let desk_daemon = node::start(
            StartOptions {
                analytics: Telemetry::Record(recording.clone()),
                ..serving(&desk, launch)
            },
            None,
        )
        .await
        .expect("the desk starts");
        let desk_runtime = runtime(&desk_daemon, &desk);
        let laptop = Install::new();
        let laptop_daemon = node::start(serving(&laptop, quiet_launch()), None)
            .await
            .expect("the laptop starts");
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
            recording,
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

    async fn link(&self) -> LoopbackLink {
        let link = self.laptop_edge.link_in_process(&self.desk_edge).unwrap();
        assert!(wait_for_route(&self.laptop_edge, self.desk_runtime.host(), PATIENCE).await);
        link
    }

    fn desk_id(&self) -> node::HostId {
        self.desk_runtime.host()
    }

    /// A channel on the laptop's profile socket for `authority`, as a
    /// client addresses one host.
    async fn laptop_socket(&self, authority: &str) -> Channel {
        socket(&self.laptop_runtime, authority).await
    }

    /// A folder on the desk that is not pinned there yet.
    fn desk_docs(&self) -> std::path::PathBuf {
        self.desk.path("code").join("docs")
    }

    /// A channel on the laptop's socket addressed to the desk.
    async fn to_desk(&self) -> Channel {
        self.laptop_socket(&wire::authority(&self.desk_id())).await
    }

    /// A person on the laptop's input for the desk's agent.
    fn send(&self, id: &[u8], text: &str) -> Request<SendInputRequest> {
        Request::new(SendInputRequest {
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

    /// The link log's lines for the calls the laptop carried.
    fn carried(&self) -> Vec<CarriedCall> {
        let log = String::from_utf8_lossy(&captured_log().lock().unwrap()).into_owned();
        log.lines()
            .filter_map(CarriedCall::parse)
            .filter(|(host, _)| *host == self.laptop_runtime.host())
            .map(|(_, call)| call)
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

/// Options for a daemon on `install` that serves its profile's socket, as
/// the installed one does.
fn serving(install: &Install, launch: node::Launch) -> StartOptions {
    StartOptions {
        front_door: Some(install.path("door.sock")),
        ..install.options("boot-1", launch)
    }
}

/// A channel on `runtime`'s profile socket whose calls name `authority`.
async fn socket(runtime: &ProfileRuntime, authority: &str) -> Channel {
    let path = runtime.dir().join(node::PROFILE_SOCKET);
    tonic::transport::Endpoint::from_shared(format!("http://{authority}"))
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                agent_dir::local_socket::connect(&path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .expect("the socket answers")
}

/// Pins `folder` on the host the call is addressed to.
fn pinning(folder: &std::path::Path) -> Request<PinFolderRequest> {
    Request::new(PinFolderRequest {
        path: folder.to_string_lossy().into_owned(),
        pinned: true,
    })
}

/// The names of the folders `runtime`'s own entry says are pinned.
fn pinned(runtime: &ProfileRuntime) -> Vec<String> {
    runtime
        .host_entry()
        .folders
        .into_iter()
        .filter(|folder| folder.kind() == wire::FolderKind::Pinned)
        .map(|folder| folder.name)
        .collect()
}

/// Waits for `runtime`'s own entry to say `names` are its pinned folders.
async fn until_pinned(runtime: &ProfileRuntime, names: &[&str]) {
    until("the entry to say the pin", || {
        let held = pinned(runtime);
        async move {
            if held == names {
                Ok(())
            } else {
                Err(format!("{held:?}"))
            }
        }
    })
    .await
    .unwrap();
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

/// The reason in a status's details, as amux's daemons write it.
fn reason(status: &tonic::Status) -> Option<wire::ErrorReason> {
    wire::Error::decode(status.details()).ok()?.reason()
}

/// A person on the laptop pins a folder on the desk through the laptop's
/// socket, and the desk makes the change for itself; the desk counts a
/// person's prompt sent the same way as coming from the laptop.
#[tokio::test(flavor = "multi_thread")]
async fn a_person_on_the_laptop_is_answered_by_the_desk_as_a_person_there() {
    captured_log();
    let (pair, _link) = Pair::new().await;
    assert!(pinned(&pair.desk_runtime).is_empty());

    staging::host_service_client(pair.to_desk().await)
        .pin_folder(pinning(&pair.desk_docs()))
        .await
        .unwrap();
    until_pinned(&pair.desk_runtime, &["docs"]).await;
    assert!(
        pinned(&pair.laptop_runtime).is_empty(),
        "the laptop pinned nothing of its own"
    );

    let answer = staging::agent_service_client(pair.to_desk().await)
        .send_input(pair.send(b"p1", "look at the logs"))
        .await
        .unwrap();
    assert_eq!(verdict(answer.get_ref()), "queued");
    let laptop = pair.laptop_runtime.host();
    let counted = pair
        .recording
        .events()
        .into_iter()
        .filter(|(_, event)| {
            matches!(
                event,
                analytics::Event::PromptSent { from: Origin::PairedHost(host), .. }
                    if *host == laptop
            )
        })
        .count();
    assert_eq!(
        counted, 1,
        "the desk counts the prompt as the laptop's person's"
    );
    pair.stop().await;
}

/// The phone calls its node in memory through the same router and carrier.
#[tokio::test(flavor = "multi_thread")]
async fn the_phones_entry_in_memory_carries_a_call_to_the_desk() {
    let (pair, _link) = Pair::new().await;
    let entrance = pair
        .laptop_daemon
        .registry()
        .routes(pair.laptop.profile)
        .unwrap();
    let origin = format!("http://{}", wire::authority(&pair.desk_id()))
        .parse()
        .unwrap();
    let mut desk = staging::host_service_client::HostServiceClient::with_origin(entrance, origin);
    desk.pin_folder(pinning(&pair.desk_docs())).await.unwrap();
    until_pinned(&pair.desk_runtime, &["docs"]).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_for_a_host_with_no_link_never_leaves_and_says_so() {
    let (pair, link) = Pair::new().await;
    link.sever();
    drop(link);
    let desk = staging::agent_service_client(pair.to_desk().await);
    until("the laptop to lose its route to the desk", || {
        let mut desk = desk.clone();
        let request = pair.send(b"p1", "look at the logs");
        async move {
            match desk.send_input(request).await {
                Err(status) if reason(&status) == Some(wire::ErrorReason::HostUnreachable) => {
                    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
                    Ok(())
                }
                other => Err(format!("{other:?}")),
            }
        }
    })
    .await
    .unwrap();
    assert!(
        pair.sent_ids().is_empty(),
        "a call that never left never reached the agent"
    );

    // A host never paired is no different.
    let stranger = node::HostId::from_u128(0x0192_f0c8_7a51_7c3e_9d2f_0000_0000_0001);
    let answer =
        staging::host_service_client(pair.laptop_socket(&wire::authority(&stranger)).await)
            .pin_folder(pinning(&pair.desk_docs()))
            .await
            .unwrap_err();
    assert_eq!(reason(&answer), Some(wire::ErrorReason::HostUnreachable));
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_for_a_host_that_no_longer_trusts_this_one_says_so() {
    let (pair, link) = Pair::new().await;
    let laptop = pair.laptop_runtime.host();
    pair.desk_edge
        .unpair(wire::HostRef::id(laptop.as_bytes().to_vec()))
        .await
        .unwrap();
    until("the laptop to read the desk as revoked", || {
        let revoked = pair
            .laptop_runtime
            .host_entry_of(pair.desk_id())
            .is_some_and(|entry| entry.revoked());
        async move {
            if revoked {
                Ok(())
            } else {
                Err("not yet".to_owned())
            }
        }
    })
    .await
    .unwrap();
    let answer = staging::host_service_client(pair.to_desk().await)
        .pin_folder(pinning(&pair.desk_docs()))
        .await
        .unwrap_err();
    assert_eq!(answer.code(), tonic::Code::Unauthenticated, "{answer:?}");
    assert_eq!(answer.message(), node::TRUST_REVOKED);
    drop(link);
    pair.stop().await;
}

/// A carried call always has the caller header, and the peer service
/// answers only a paired daemon asking for itself, so a client cannot
/// reach it through its node.
#[tokio::test(flavor = "multi_thread")]
async fn a_carried_call_to_the_peer_service_is_refused() {
    let (pair, _link) = Pair::new().await;
    let answer = staging::peer_service_client(pair.to_desk().await)
        .follow_agents(FollowAgentsRequest {})
        .await
        .unwrap_err();
    assert_eq!(answer.code(), tonic::Code::PermissionDenied, "{answer:?}");
    pair.stop().await;
}

/// The profile's administration is served on its own socket only: carried
/// to another host it finds nothing there.
#[tokio::test(flavor = "multi_thread")]
async fn administration_carried_to_another_host_finds_nothing_mounted() {
    let (pair, _link) = Pair::new().await;
    let cancel = || Request::new(wire::CancelPairingRequest::default());
    wire::profile_admin_service_client::ProfileAdminServiceClient::new(
        pair.laptop_socket("amux.local").await,
    )
    .cancel_pairing(cancel())
    .await
    .expect("the laptop's own administration answers");
    let answer =
        wire::profile_admin_service_client::ProfileAdminServiceClient::new(pair.to_desk().await)
            .cancel_pairing(cancel())
            .await
            .unwrap_err();
    assert_eq!(answer.code(), tonic::Code::Unimplemented, "{answer:?}");
    pair.stop().await;
}

/// An attachment at the stated limit crosses the carrier and the link and
/// is stored on the desk.
#[tokio::test(flavor = "multi_thread")]
async fn an_attachment_at_the_limit_carried_to_the_desk_arrives() {
    let (pair, _link) = Pair::new().await;
    let bytes: Vec<u8> = (0..wire::ATTACHMENT_SIZE_LIMIT)
        .map(|index| (index % 251) as u8)
        .collect();
    let stored = staging::agent_service_client(pair.to_desk().await)
        .put_blob(PutBlobRequest {
            agent_id: pair.agent.id.as_bytes().to_vec(),
            name: "big.bin".to_owned(),
            mime: "application/octet-stream".to_owned(),
            bytes: bytes.clone(),
        })
        .await
        .expect("the attachment arrives")
        .into_inner()
        .blob
        .expect("the desk names what it stored");
    assert_eq!(stored.size, bytes.len() as u64);
    pair.stop().await;
}

/// A send that went out and whose link failed under it may have arrived,
/// so it is aborted, never refused; the agent took it once, and nothing
/// sends it again.
#[tokio::test(flavor = "multi_thread")]
async fn a_send_cut_off_after_it_left_is_aborted_and_arrives_once() {
    let (pair, link) = Pair::new().await;
    pair.agent.freeze();
    let sending = tokio::spawn({
        let request = pair.send(b"p1", "look at the logs");
        let mut desk = staging::agent_service_client(pair.to_desk().await);
        async move { desk.send_input(request).await }
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
        Err(status) => assert_eq!(status.code(), tonic::Code::Aborted, "{status:?}"),
    }

    let _link = pair.link().await;
    pair.agent.thaw();
    let answer = staging::agent_service_client(pair.to_desk().await)
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

/// The carrier reads no body: an input's arm or field no daemon knows
/// reaches the agent exactly as its client encoded it.
#[tokio::test(flavor = "multi_thread")]
async fn an_input_no_daemon_knows_is_carried_as_it_was_sent() {
    let (pair, _link) = Pair::new().await;
    // A field 99 no version of the message has, then an arm 99 alone:
    // varint key 99 << 3, value 1.
    let unknown = [0x98, 0x06, 0x01];
    let input = wire::Input {
        input_id: b"from-the-laptop".to_vec(),
        kind: wire::Kind::ClaudeSdk as i32,
        body: unknown.to_vec(),
    };
    let answer = staging::agent_service_client(pair.to_desk().await)
        .send_input(Request::new(SendInputRequest {
            agent_id: pair.agent.id.as_bytes().to_vec(),
            input: Some(input.clone()),
        }))
        .await
        .unwrap();
    assert_eq!(verdict(answer.get_ref()), "queued");
    let handed = pair
        .agent
        .inputs()
        .into_iter()
        .filter_map(|handed| match handed {
            Handed::Input(input) => Some(input.encode_to_vec()),
            Handed::Message(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(handed, vec![input.encode_to_vec()]);
    pair.stop().await;
}

/// Each carried call, answered or not, leaves one line in the laptop's
/// link log; a call answered on the laptop leaves none.
#[tokio::test(flavor = "multi_thread")]
async fn the_link_log_has_one_line_per_carried_call() {
    captured_log();
    let (pair, link) = Pair::new().await;
    let laptop_docs = pair.laptop.path("docs");
    std::fs::create_dir_all(&laptop_docs).unwrap();
    let mut here = staging::host_service_client(pair.laptop_socket("amux.local").await);
    here.pin_folder(pinning(&laptop_docs)).await.unwrap();
    let mut desk = staging::host_service_client(pair.to_desk().await);
    desk.pin_folder(pinning(&pair.desk_docs())).await.unwrap();
    let unknown =
        wire::profile_admin_service_client::ProfileAdminServiceClient::new(pair.to_desk().await)
            .cancel_pairing(Request::new(wire::CancelPairingRequest::default()))
            .await
            .unwrap_err();
    assert_eq!(unknown.code(), tonic::Code::Unimplemented);
    link.sever();
    drop(link);
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let docs = pair.desk_docs();
    until("a call to the desk to find no link", || {
        let mut desk = desk.clone();
        let docs = docs.clone();
        attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        async move {
            match desk.pin_folder(pinning(&docs)).await {
                Err(status) if reason(&status) == Some(wire::ErrorReason::HostUnreachable) => {
                    Ok(())
                }
                other => Err(format!("{other:?}")),
            }
        }
    })
    .await
    .unwrap();

    let carried = pair.carried();
    let desk_id = pair.desk_id();
    let list = "/amux.staging.HostService/PinFolder";
    assert!(
        carried.iter().all(|call| call.peer == desk_id),
        "{carried:?}"
    );
    let answered: Vec<_> = carried
        .iter()
        .filter(|call| call.outcome == CarriedOutcome::Answered)
        .map(|call| (call.method.as_str(), call.code))
        .collect();
    assert_eq!(
        answered,
        [
            (list, tonic::Code::Ok as i32),
            (
                "/amux.v1.ProfileAdminService/CancelPairing",
                tonic::Code::Unimplemented as i32
            ),
        ],
        "one line for each call carried and answered: {carried:?}"
    );
    // Each try after the link went is logged too: unreachable once the
    // laptop knows, aborted for one that went out on the link as it went.
    let rest = &carried[answered.len()..];
    assert_eq!(rest.len(), attempts.into_inner(), "{carried:?}");
    assert!(
        rest.iter().all(|call| call.method == list
            && match call.outcome {
                CarriedOutcome::Unreachable => call.code == tonic::Code::FailedPrecondition as i32,
                CarriedOutcome::Aborted => call.code == tonic::Code::Aborted as i32,
                _ => false,
            }),
        "{rest:?}"
    );
    assert_eq!(rest.last().unwrap().outcome, CarriedOutcome::Unreachable);
    pair.stop().await;
}
