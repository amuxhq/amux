//! The carrier against a fake far host: a probe service on a tonic server
//! of its own, reached over in-memory pipes the test can cut, behind a
//! client entry whose own handlers are a probe too, so every answer says
//! which side gave it.

use std::collections::HashMap;
use std::str::FromStr as _;
use std::sync::Mutex;
use std::time::Duration;

use bytes::{Buf as _, BufMut as _};
use futures_util::StreamExt as _;
use hyper_util::rt::TokioIo;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::codegen::http::uri::PathAndQuery;
use tonic::transport::{Endpoint, Server};

use super::*;
use crate::api::{Entrance, Entry, Surface};
use crate::link::link_log::{CarriedCall, CarriedOutcome};

/// A message is its bytes: the probe needs no schema.
#[derive(Clone, Copy, Default)]
struct Raw;

impl Codec for Raw {
    type Encode = Vec<u8>;
    type Decode = Vec<u8>;
    type Encoder = Raw;
    type Decoder = Raw;
    fn encoder(&mut self) -> Raw {
        Raw
    }
    fn decoder(&mut self) -> Raw {
        Raw
    }
}

impl Encoder for Raw {
    type Item = Vec<u8>;
    type Error = Status;
    fn encode(&mut self, item: Vec<u8>, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        dst.put_slice(&item);
        Ok(())
    }
}

impl Decoder for Raw {
    type Item = Vec<u8>;
    type Error = Status;
    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Vec<u8>>, Status> {
        Ok(Some(src.copy_to_bytes(src.remaining()).to_vec()))
    }
}

type Messages = std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Vec<u8>, Status>> + Send>>;

/// A test-only service: `Who` says who answered, with the authority and
/// caller header it saw; `Ticks` streams until dropped; `Echo` answers
/// each message of a two-way stream as it comes; `Hang` never answers.
#[derive(Clone)]
struct Probe {
    name: &'static str,
    /// What the probe saw happen, for a test to wait on.
    events: mpsc::UnboundedSender<&'static str>,
}

impl tonic::server::NamedService for Probe {
    const NAME: &'static str = "probe.Probe";
}

impl Service<http::Request<Body>> for Probe {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<http::Response<Body>, Infallible>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let seen = format!(
            "answered-by={} authority={} caller={}",
            self.name,
            request
                .uri()
                .authority()
                .map_or("-", |authority| authority.as_str()),
            request
                .headers()
                .get(wire::CALLER_HEADER)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("-"),
        );
        let events = self.events.clone();
        let path = request.uri().path().to_owned();
        Box::pin(async move {
            let mut grpc = tonic::server::Grpc::new(Raw);
            Ok(match path.as_str() {
                "/probe.Probe/Who" => {
                    let who = tower::service_fn(move |_: tonic::Request<Vec<u8>>| {
                        let seen = seen.clone();
                        async move { Ok::<_, Status>(tonic::Response::new(seen.into_bytes())) }
                    });
                    grpc.unary(who, request).await
                }
                "/probe.Probe/Hang" => {
                    let hang = tower::service_fn(|_: tonic::Request<Vec<u8>>| {
                        std::future::pending::<Result<tonic::Response<Vec<u8>>, Status>>()
                    });
                    grpc.unary(hang, request).await
                }
                "/probe.Probe/Ticks" => {
                    let ticks = tower::service_fn(move |_: tonic::Request<Vec<u8>>| {
                        let gone = Gone(events.clone());
                        async move {
                            let ticks =
                                futures_util::stream::unfold((0u8, gone), |(n, gone)| async move {
                                    tokio::time::sleep(Duration::from_millis(10)).await;
                                    Some((Ok(vec![n]), (n.wrapping_add(1), gone)))
                                });
                            Ok::<_, Status>(tonic::Response::new(Box::pin(ticks) as Messages))
                        }
                    });
                    grpc.server_streaming(ticks, request).await
                }
                "/probe.Probe/Echo" => {
                    let echo = tower::service_fn(
                        |request: tonic::Request<tonic::Streaming<Vec<u8>>>| async move {
                            let echoed = request.into_inner().map(|message| {
                                message.map(|message| [b"echo ".as_slice(), &message].concat())
                            });
                            Ok::<_, Status>(tonic::Response::new(Box::pin(echoed) as Messages))
                        },
                    );
                    grpc.streaming(echo, request).await
                }
                _ => Status::unimplemented("").into_http(),
            })
        })
    }
}

/// Says the ticks stream was dropped, when the probe drops it.
struct Gone(mpsc::UnboundedSender<&'static str>);

impl Drop for Gone {
    fn drop(&mut self) {
        let _ = self.0.send("ticks dropped");
    }
}

/// The far host: a probe served on a tonic server of its own, as a link's
/// streams are served, with no router in front.
struct Far {
    id: HostId,
    connections: mpsc::Sender<tokio::io::DuplexStream>,
    /// The pumps between each connection's two pipes: aborting them is
    /// the link failing.
    pumps: Arc<Mutex<Vec<AbortHandle>>>,
    events: mpsc::UnboundedReceiver<&'static str>,
}

impl Far {
    fn new() -> Self {
        let (events_tx, events) = mpsc::unbounded_channel();
        let (connections, accepted) = mpsc::channel(8);
        let incoming = futures_util::stream::unfold(accepted, |mut accepted| async move {
            accepted
                .recv()
                .await
                .map(|io| (Ok::<_, std::io::Error>(io), accepted))
        });
        tokio::spawn(
            Server::builder()
                .add_service(Probe {
                    name: "far",
                    events: events_tx,
                })
                .serve_with_incoming(incoming),
        );
        Self {
            id: HostId::from_u128(0x0192_f0c8_7a51_7c3e_9d2f_3b1c_5e6a_7d80),
            connections,
            pumps: Arc::default(),
            events,
        }
    }

    /// A channel to the far host, built for its id as a link's is.
    fn channel(&self) -> Channel {
        let (connections, pumps) = (self.connections.clone(), self.pumps.clone());
        Endpoint::from_shared(format!("https://{}", wire::authority(&self.id)))
            .unwrap()
            .connect_with_connector_lazy(tower::service_fn(move |_: http::Uri| {
                let (connections, pumps) = (connections.clone(), pumps.clone());
                async move {
                    let (here, near) = tokio::io::duplex(1 << 16);
                    let (mut far, there) = tokio::io::duplex(1 << 16);
                    connections
                        .send(there)
                        .await
                        .map_err(|_| std::io::Error::other("the far host is gone"))?;
                    let pump = tokio::spawn(async move {
                        let mut near = near;
                        let _ = tokio::io::copy_bidirectional(&mut near, &mut far).await;
                    });
                    pumps.lock().unwrap().push(pump.abort_handle());
                    Ok::<_, std::io::Error>(TokioIo::new(here))
                }
            }))
    }

    /// The link under every connection fails.
    fn sever(&self) {
        for pump in self.pumps.lock().unwrap().drain(..) {
            pump.abort();
        }
    }

    async fn saw(&mut self, event: &str) {
        let seen = tokio::time::timeout(Duration::from_secs(10), self.events.recv())
            .await
            .expect("the far host saw it in time");
        assert_eq!(seen, Some(event));
    }
}

/// The hosts a carrier reaches, and how.
#[derive(Default)]
struct Hosts(HashMap<HostId, Result<Channel, Unreached>>);

impl Reach for Hosts {
    fn channel(&self, host: HostId) -> BoxFuture<'_, Result<Channel, Unreached>> {
        let reached = self
            .0
            .get(&host)
            .cloned()
            .unwrap_or(Err(Unreached::Unreachable));
        Box::pin(async move { reached })
    }

    fn name(&self, _: HostId) -> String {
        "desk".to_owned()
    }
}

/// A host that is never reached within any test's time.
struct Stuck;

impl Reach for Stuck {
    fn channel(&self, _: HostId) -> BoxFuture<'_, Result<Channel, Unreached>> {
        Box::pin(std::future::pending())
    }

    fn name(&self, _: HostId) -> String {
        "desk".to_owned()
    }
}

const OWN: HostId = HostId::from_u128(0x0192_f0c8_7a51_7c3e_9d2f_aaaa_aaaa_aaaa);

/// The phone's entry, with a probe for its own handlers and a carrier
/// reaching `reach`.
fn near(reach: impl Reach) -> Entrance {
    let (events, _) = mpsc::unbounded_channel();
    let handlers = tonic::service::Routes::new(Probe {
        name: "near",
        events,
    });
    crate::api::client_entrance(
        &Weak::new(),
        Entry::Phone,
        OWN,
        handlers,
        Carrier::new(OWN, Arc::new(reach)),
    )
}

fn reaching(far: &Far) -> Hosts {
    Hosts(HashMap::from([(far.id, Ok(far.channel()))]))
}

/// A client of `entrance` addressing `authority`, as the phone's stubs
/// call it in memory: no authority is a call to its own node.
async fn client(entrance: &Entrance, authority: Option<String>) -> tonic::client::Grpc<Entrance> {
    let entrance = entrance.clone();
    let mut grpc = match authority {
        Some(authority) => tonic::client::Grpc::with_origin(
            entrance,
            format!("http://{authority}").parse().unwrap(),
        ),
        None => tonic::client::Grpc::new(entrance),
    };
    grpc.ready().await.unwrap();
    grpc
}

async fn who(entrance: &Entrance, authority: Option<String>) -> Result<String, Status> {
    let mut grpc = client(entrance, authority).await;
    let mut request = tonic::Request::new(Vec::new());
    // A client claiming to be someone: the entry's word replaces it.
    request.metadata_mut().insert_bin(
        wire::CALLER_HEADER,
        tonic::metadata::MetadataValue::from_bytes(b"an agent, honestly"),
    );
    grpc.unary(request, PathAndQuery::from_static("/probe.Probe/Who"), Raw)
        .await
        .map(|answer| String::from_utf8(answer.into_inner()).unwrap())
}

#[tokio::test]
async fn a_call_for_another_host_is_carried_there_as_the_entrys_caller() {
    let far = Far::new();
    let entrance = near(reaching(&far));
    let desk = wire::authority(&far.id);
    let person = Caller::person_here(Surface::Phone).header(OWN).unwrap();
    assert_eq!(
        who(&entrance, Some(desk.clone())).await.unwrap(),
        format!("answered-by=far authority={desk} caller={person}")
    );
    for here in [
        None,
        Some("amux.local".to_owned()),
        Some(wire::authority(&OWN)),
        Some(desk.to_uppercase()),
    ] {
        let answer = who(&entrance, here.clone()).await.unwrap();
        assert!(
            answer.starts_with("answered-by=near ") && answer.ends_with(" caller=-"),
            "{here:?}: {answer}"
        );
    }
}

#[tokio::test]
async fn a_carried_stream_runs_until_the_caller_drops_it_which_cancels_it_at_the_owner() {
    let mut far = Far::new();
    let entrance = near(reaching(&far));
    let mut ticks = client(&entrance, Some(wire::authority(&far.id)))
        .await
        .server_streaming(
            tonic::Request::new(Vec::new()),
            PathAndQuery::from_static("/probe.Probe/Ticks"),
            Raw,
        )
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ticks.next().await.unwrap().unwrap(), vec![0]);
    assert_eq!(ticks.next().await.unwrap().unwrap(), vec![1]);
    drop(ticks);
    far.saw("ticks dropped").await;
}

#[tokio::test]
async fn a_two_way_stream_is_carried_message_by_message() {
    let far = Far::new();
    let entrance = near(reaching(&far));
    let (up, outbound) = mpsc::channel::<Vec<u8>>(1);
    let outbound = tokio_stream_of(outbound);
    let mut down = client(&entrance, Some(wire::authority(&far.id)))
        .await
        .streaming(
            tonic::Request::new(outbound),
            PathAndQuery::from_static("/probe.Probe/Echo"),
            Raw,
        )
        .await
        .unwrap()
        .into_inner();
    for word in ["one", "two", "three"] {
        up.send(word.as_bytes().to_vec()).await.unwrap();
        // Each answer arrives before the next message is sent.
        let back = down.next().await.unwrap().unwrap();
        assert_eq!(back, format!("echo {word}").into_bytes());
    }
    drop(up);
    assert!(down.next().await.is_none(), "a half-close ends the stream");
}

fn tokio_stream_of(
    mut receiver: mpsc::Receiver<Vec<u8>>,
) -> impl futures_util::Stream<Item = Vec<u8>> + Send + 'static {
    futures_util::stream::poll_fn(move |cx| receiver.poll_recv(cx))
}

#[tokio::test(start_paused = true)]
async fn an_answer_that_does_not_begin_within_its_limit_is_aborted() {
    let far = Far::new();
    let entrance = near(reaching(&far));
    let started = tokio::time::Instant::now();
    let answer = client(&entrance, Some(wire::authority(&far.id)))
        .await
        .unary(
            tonic::Request::new(Vec::new()),
            PathAndQuery::from_static("/probe.Probe/Hang"),
            Raw,
        )
        .await
        .unwrap_err();
    assert_eq!(answer.code(), tonic::Code::Aborted, "{answer:?}");
    assert_eq!(
        answer.message(),
        "desk stopped answering; the call may or may not have arrived"
    );
    assert_eq!(started.elapsed(), ANSWER_PATIENCE);
}

#[tokio::test]
async fn a_link_failing_under_a_carried_stream_aborts_it() {
    let far = Far::new();
    let entrance = near(reaching(&far));
    let mut ticks = client(&entrance, Some(wire::authority(&far.id)))
        .await
        .server_streaming(
            tonic::Request::new(Vec::new()),
            PathAndQuery::from_static("/probe.Probe/Ticks"),
            Raw,
        )
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ticks.next().await.unwrap().unwrap(), vec![0]);
    far.sever();
    let ended = loop {
        match ticks.next().await {
            // Ticks already on their way may still arrive.
            Some(Ok(_)) => {}
            Some(Err(status)) => break status,
            None => panic!("a stream cut under it ended as though it finished"),
        }
    };
    assert_eq!(ended.code(), tonic::Code::Aborted, "{ended:?}");
}

/// What the owner does not have it answers for itself, and the answer
/// passes through as it gave it: "unimplemented" with no words, which the
/// client library reads as "that host needs updating to do this".
#[tokio::test]
async fn a_method_the_owner_does_not_have_passes_through_as_unimplemented() {
    let far = Far::new();
    let entrance = near(reaching(&far));
    for path in ["/amux.experimental.Nothing1/Open", "/probe.Probe/Nothing"] {
        let answer = client(&entrance, Some(wire::authority(&far.id)))
            .await
            .unary(
                tonic::Request::new(Vec::new()),
                PathAndQuery::from_str(path).unwrap(),
                Raw,
            )
            .await
            .unwrap_err();
        assert_eq!(answer.code(), tonic::Code::Unimplemented, "{path}");
        assert_eq!(answer.message(), "", "{path}");
        assert!(crate::net_error::from_status(&answer).is_none(), "{path}");
    }
}

#[tokio::test]
async fn a_call_that_never_left_says_why() {
    let desk = HostId::from_u128(7);
    let cases = [
        (
            Unreached::Unreachable,
            tonic::Code::FailedPrecondition,
            Some(wire::ErrorReason::HostUnreachable),
        ),
        (
            Unreached::Lapsed,
            tonic::Code::PermissionDenied,
            Some(wire::ErrorReason::PaymentRequired),
        ),
        (Unreached::Revoked, tonic::Code::Unauthenticated, None),
    ];
    for (unreached, code, reason) in cases {
        let entrance = near(Hosts(HashMap::from([(desk, Err(unreached))])));
        let answer = who(&entrance, Some(wire::authority(&desk)))
            .await
            .unwrap_err();
        assert_eq!(answer.code(), code, "{unreached:?}");
        assert_eq!(
            crate::net_error::from_status(&answer).and_then(|error| error.reason()),
            reason,
            "{unreached:?}"
        );
        if unreached == Unreached::Revoked {
            assert!(crate::link::channels::says_revoked(&answer));
        }
    }
}

/// The router asks the carrier to be ready before every call it routes,
/// local ones included; the carrier waits for a host inside the call, so a
/// host that takes its time holds up nothing answered here.
#[tokio::test]
async fn a_host_being_reached_holds_up_no_call_answered_here() {
    let entrance = near(Stuck);
    let waiting = tokio::spawn({
        let mut grpc = client(&entrance, Some(wire::authority(&HostId::from_u128(7)))).await;
        async move {
            grpc.unary(
                tonic::Request::new(Vec::new()),
                PathAndQuery::from_static("/probe.Probe/Who"),
                Raw,
            )
            .await
        }
    });
    let answer = tokio::time::timeout(Duration::from_secs(10), who(&entrance, None))
        .await
        .expect("a local call is answered while another waits for its host")
        .unwrap();
    assert!(answer.starts_with("answered-by=near "), "{answer}");
    assert!(!waiting.is_finished());
    waiting.abort();
}

/// A link's entry has no router: a call that arrives from a paired host
/// addressed to a third host is answered here, never passed on, even with
/// a carrier able to reach that host.
#[tokio::test]
async fn a_call_arriving_on_a_link_for_a_third_host_is_answered_here() {
    let far = Far::new();
    let (events, _) = mpsc::unbounded_channel();
    let handlers = tonic::service::Routes::new(Probe {
        name: "near",
        events,
    });
    let link = crate::api::entrance(&Weak::new(), Entry::Link, handlers);
    let mut grpc = client(&link, Some(wire::authority(&far.id))).await;
    let mut request = tonic::Request::new(Vec::new());
    request
        .extensions_mut()
        .insert(crate::transport::BoxedGrpcConnectInfo {
            auth: crate::transport::BoxedGrpcAuth::Trusted {
                peer: HostId::from_u128(9),
            },
        });
    let answer = grpc
        .unary(request, PathAndQuery::from_static("/probe.Probe/Who"), Raw)
        .await
        .unwrap();
    let answer = String::from_utf8(answer.into_inner()).unwrap();
    assert!(answer.starts_with("answered-by=near "), "{answer}");
}

/// What this thread logs, for a test on a current-thread runtime.
fn logged() -> (Arc<Mutex<Vec<u8>>>, tracing::subscriber::DefaultGuard) {
    #[derive(Clone)]
    struct Into(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Into {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let log = Arc::new(Mutex::new(Vec::new()));
    let writer = Into(log.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    (log, tracing::subscriber::set_default(subscriber))
}

fn carried_lines(log: &Mutex<Vec<u8>>) -> Vec<CarriedCall> {
    String::from_utf8_lossy(&log.lock().unwrap())
        .lines()
        .filter_map(CarriedCall::parse)
        .map(|(host, call)| {
            assert_eq!(host, OWN);
            call
        })
        .collect()
}

/// Every carried call leaves one line when it ends, a call its caller let
/// go of before any answer began included.
#[tokio::test]
async fn every_carried_call_leaves_one_line_when_it_ends() {
    let (log, _logging) = logged();
    let far = Far::new();
    let entrance = near(reaching(&far));
    let desk = wire::authority(&far.id);
    who(&entrance, Some(desk.clone())).await.unwrap();
    who(&entrance, None).await.unwrap();
    let mut grpc = client(&entrance, Some(desk)).await;
    let hanging = grpc.unary(
        tonic::Request::new(Vec::new()),
        PathAndQuery::from_static("/probe.Probe/Hang"),
        Raw,
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), hanging)
            .await
            .is_err()
    );
    let lines = carried_lines(&log);
    let ended: Vec<_> = lines
        .iter()
        .map(|call| (call.method.as_str(), call.outcome, call.code))
        .collect();
    assert_eq!(
        ended,
        [
            ("/probe.Probe/Who", CarriedOutcome::Answered, 0),
            (
                "/probe.Probe/Hang",
                CarriedOutcome::Dropped,
                tonic::Code::Cancelled as i32
            ),
        ]
    );
    assert!(lines.iter().all(|call| call.peer == far.id));
}
