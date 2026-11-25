//! Both ways of reaching a node answer alike: gRPC over the profile socket
//! the desktop clients dial, and the node's entry called in memory as the
//! phone calls it, against real nodes. A call addressed to another host's
//! agent is carried there by the node the client is attached to, and its
//! failures read the same on both.

#![cfg(unix)]

use std::time::Duration;

use client::{Client, Installation, NotSent, Surface};
use futures_util::StreamExt as _;
use provider_fakes::script::Step;
use testnet::{AgentDecl, FakeKind, Net, Topology};
use wire::{ErrorCode, inventory_event, session_event};

const PATIENCE: Duration = Duration::from_secs(20);

/// The profile socket of `host`'s first profile.
async fn socket(net: &Net, host: &str) -> std::path::PathBuf {
    let profiles = net
        .front_door(host)
        .await
        .unwrap()
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .unwrap()
        .into_inner()
        .profiles;
    profiles[0].socket_path.clone().into()
}

/// A client of `host` over each transport.
async fn clients(net: &Net, host: &str) -> [(&'static str, Client); 2] {
    let socket = Client::connect(&socket(net, host).await, Surface::Terminal)
        .await
        .unwrap();
    let memory = Client::in_memory(net.entrance(host).unwrap(), Surface::Ios);
    [("socket", socket), ("in memory", memory)]
}

/// The agent's row as `client`'s node holds it, once it lists it.
async fn row(client: &Client, agent: &[u8]) -> wire::Agent {
    let mut inventory = client.inventory().subscribe().await.unwrap();
    tokio::time::timeout(PATIENCE, async {
        loop {
            let event = inventory.next().await.unwrap().unwrap();
            if let Some(inventory_event::Of::Agent(row)) = event.of
                && row.agent_id == agent
            {
                return row;
            }
        }
    })
    .await
    .expect("the node lists the agent")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_socket_and_in_memory_clients_answer_alike() {
    let net = Net::start(
        Topology::new().host("desk").agent(
            AgentDecl::new("worker", "desk")
                .kind(FakeKind::Codex)
                .steps(vec![
                    Step::Text {
                        chunks: vec!["hello".into()],
                    },
                    Step::TurnEnd,
                ])
                .prompt("go"),
        ),
    )
    .await
    .unwrap();
    let worker = net.agent("worker").unwrap().id.as_bytes().to_vec();
    let desk = net.host("desk").unwrap().host_id.as_bytes().to_vec();
    for (name, client) in clients(&net, "desk").await {
        // The inventory lists the agent, then catches up.
        let mut inventory = client.inventory().subscribe().await.unwrap();
        let mut listed = false;
        loop {
            let event = tokio::time::timeout(PATIENCE, inventory.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            match event.of {
                Some(inventory_event::Of::Agent(agent)) => listed |= agent.agent_id == worker,
                Some(inventory_event::Of::CaughtUp(_)) => break,
                _ => {}
            }
        }
        assert!(listed, "{name}");

        // A subscription opens with its generation and snapshot and
        // reaches the reply.
        let mut stream = client.sessions().subscribe(&worker, 40).await.unwrap();
        let first = stream.next().await.unwrap().unwrap();
        assert!(
            matches!(first.of, Some(session_event::Of::Opening(_))),
            "{name}: {first:?}"
        );
        let second = stream.next().await.unwrap().unwrap();
        assert!(
            matches!(second.of, Some(session_event::Of::Snapshot(_))),
            "{name}: {second:?}"
        );
        let mut key = None;
        while key.is_none() {
            let event = tokio::time::timeout(PATIENCE, stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let Some(session_event::Of::Item(item)) = event.of
                && item.text.contains("hello")
            {
                key = Some(item.key);
            }
        }
        let item = client
            .sessions()
            .get_item(&worker, &key.unwrap())
            .await
            .unwrap();
        assert!(item.text.contains("hello"), "{name}");
        let page = client
            .sessions()
            .list_items(&worker, None, 100)
            .await
            .unwrap();
        assert!(page.exhausted, "{name}");
        assert!(page.items.iter().any(|held| held.key == item.key), "{name}");

        // A file goes in at the agent's host and comes back out of this
        // node's copy.
        let row = row(&client, &worker).await;
        let agent = client.agent(&row);
        assert_eq!(agent.id(), worker.as_slice(), "{name}");
        let blob = agent
            .put_blob(
                "note.txt",
                "text/plain",
                format!("from {name}").into_bytes(),
            )
            .await
            .unwrap();
        let back = client
            .sessions()
            .get_blob(&worker, &blob.hash)
            .await
            .unwrap();
        assert_eq!(back, format!("from {name}").into_bytes());

        // A draft is kept by the node, and the node says who it is.
        let draft = wire::Draft {
            text: format!("half a thought, {name}"),
            attachments: Vec::new(),
        };
        client
            .drafts()
            .update(&worker, draft.clone())
            .await
            .unwrap();
        assert_eq!(client.drafts().get(&worker).await.unwrap(), draft, "{name}");
        let identity = client.admin().get_device_identity().await.unwrap();
        assert_eq!(identity.host_id, desk, "{name}");

        // The node's own id is an address like any other host's: unpinning
        // a folder never pinned is answered, and changes nothing.
        client
            .host(&desk)
            .pin_folder("/nowhere/never-pinned", false)
            .await
            .unwrap_or_else(|error| panic!("{name}: {error}"));

        // The runtime's own errors arrive whole.
        let missing = client
            .sessions()
            .get_item(&worker, "no-such-key")
            .await
            .unwrap_err();
        assert_eq!(missing.code(), Some(ErrorCode::NotFound), "{name}");
        assert!(!missing.is_transport());

        // A row whose host is no id names nowhere to send its call.
        let nowhere = wire::Agent {
            host_id: b"desk".to_vec(),
            ..row
        };
        let refused = client.agent(&nowhere).rename("x").await.unwrap_err();
        assert_eq!(refused.code(), Some(ErrorCode::InvalidArgument), "{name}");
    }
    net.shutdown().await.unwrap();
}

/// A client attached to the laptop acts on the desk's agent from the
/// agent's row alone: the row names the desk, so the call is addressed
/// there and carried by the laptop's node, and it names the agent by the
/// id held with that host. Made on the laptop's own handlers, the same
/// call would find no such agent there.
#[tokio::test(flavor = "multi_thread")]
async fn an_agents_row_addresses_its_host_from_a_node_that_holds_a_copy() {
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("laptop")
            .link("desk", "laptop")
            .agent(
                AgentDecl::new("far", "desk")
                    .kind(FakeKind::ClaudeSdk)
                    .steps(vec![
                        Step::Text {
                            chunks: vec!["Ready.".into()],
                        },
                        Step::TurnEnd,
                    ])
                    .prompt("go"),
            ),
    )
    .await
    .unwrap();
    net.settle("far").await.unwrap();
    let far = net.agent("far").unwrap().id.as_bytes().to_vec();
    let desk = net.host("desk").unwrap().host_id.as_bytes().to_vec();
    let laptop = net.host("laptop").unwrap().host_id.as_bytes().to_vec();
    for (name, client) in clients(&net, "laptop").await {
        let row = row(&client, &far).await;
        assert_eq!(row.host_id, desk, "{name}: the laptop holds the desk's row");
        let renamed = client
            .agent(&row)
            .rename(&format!("far-{}", name.replace(' ', "-")))
            .await
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(renamed.agent_id, far, "{name}");
        assert_eq!(renamed.host_id, desk, "{name}: renamed on the desk");

        // The same id addressed to the laptop is answered there, which
        // holds only a copy.
        let here = wire::Agent {
            host_id: laptop.clone(),
            ..row.clone()
        };
        let refused = client.agent(&here).rename("nowhere").await.unwrap_err();
        assert_eq!(
            refused.code(),
            Some(ErrorCode::NotFound),
            "{name}: {refused}"
        );
    }

    // With the link down the call never leaves the laptop, and every
    // client reads why the same way.
    net.sever_link("desk", "laptop").unwrap();
    for (name, client) in clients(&net, "laptop").await {
        let row = row(&client, &far).await;
        let unreached = patience::until("the laptop to find the desk unreachable", || async {
            match client.agent(&row).rename("unreached").await {
                Err(error) if error.not_sent().is_some() => Ok(error),
                other => Err(format!("{other:?}")),
            }
        })
        .await
        .unwrap();
        assert_eq!(unreached.not_sent(), Some(NotSent::Unreachable), "{name}");
        assert!(!unreached.uncertain(), "{name}");
        assert!(
            unreached.to_string().contains("desk"),
            "{name}: the refusal names the host: {unreached}"
        );
    }
    net.shutdown().await.unwrap();
}

/// An agent's review is a host service like any other: its calls are
/// addressed to the agent's host by the host's id and carried there by the
/// node the client is attached to. A host that does not run reviews
/// answers that it does not serve them; with the link down the call never
/// leaves, and the client is told the host could not be reached.
#[tokio::test(flavor = "multi_thread")]
async fn an_agents_reviews_reach_its_host() {
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("laptop")
            .link("desk", "laptop")
            .agent(
                AgentDecl::new("far", "desk")
                    .kind(FakeKind::ClaudeSdk)
                    .steps(vec![Step::TurnEnd])
                    .prompt("go"),
            ),
    )
    .await
    .unwrap();
    net.settle("far").await.unwrap();
    let far = net.agent("far").unwrap().id.as_bytes().to_vec();
    for (name, client) in clients(&net, "laptop").await {
        let row = row(&client, &far).await;
        let refused = client
            .agent(&row)
            .reviews()
            .open(None, false)
            .await
            .unwrap_err();
        assert_eq!(
            refused.code(),
            Some(ErrorCode::Unimplemented),
            "{name}: {refused}"
        );
        assert_eq!(refused.not_sent(), None, "{name}: the desk answered");
    }

    net.sever_link("desk", "laptop").unwrap();
    for (name, client) in clients(&net, "laptop").await {
        let reviews = client.agent(&row(&client, &far).await).reviews();
        let unreached = patience::until("the laptop to find the desk unreachable", || async {
            match reviews.open(None, false).await {
                Err(error) if error.not_sent().is_some() => Ok(error),
                other => Err(format!("{other:?}")),
            }
        })
        .await
        .unwrap();
        assert_eq!(unreached.not_sent(), Some(NotSent::Unreachable), "{name}");
        assert!(
            unreached.to_string().contains("desk"),
            "{name}: the refusal names the host: {unreached}"
        );
    }
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_socket_client_says_transport_while_its_daemon_is_down_and_redials_after() {
    let mut net = Net::start(Topology::new().host("desk")).await.unwrap();
    let [(_, client), _] = clients(&net, "desk").await;
    let mut inventory = client.inventory().subscribe().await.unwrap();
    net.kill_daemon("desk").await.unwrap();
    // The open stream ends, one way or the other.
    loop {
        match tokio::time::timeout(PATIENCE, inventory.next())
            .await
            .unwrap()
        {
            Some(Ok(_)) => continue,
            Some(Err(error)) => {
                assert!(error.is_transport(), "{error:?}");
                break;
            }
            None => break,
        }
    }
    match client.inventory().subscribe().await {
        Err(error) if error.is_transport() => {}
        Err(other) => panic!("a down daemon is a transport failure, not {other:?}"),
        Ok(_) => panic!("nothing answers while the daemon is down"),
    }
    net.restart_daemon("desk").await.unwrap();
    patience::until("the same client to redial the restarted daemon", || async {
        client
            .inventory()
            .subscribe()
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .await
    .unwrap();
    net.shutdown().await.unwrap();
}

/// The front door lists the installation's profiles, and opening one gives
/// a client of its node.
#[tokio::test(flavor = "multi_thread")]
async fn a_profile_opened_from_the_front_door_answers() {
    let net = Net::start(Topology::new().host("desk")).await.unwrap();
    let door = net.host("desk").unwrap().front_door.clone();
    let installation = Installation::connect(&door, Surface::Terminal)
        .await
        .unwrap();
    let profiles = installation.list_profiles().await.unwrap();
    let client = installation.open_profile(&profiles[0]).await.unwrap();
    let identity = client.admin().get_device_identity().await.unwrap();
    assert_eq!(
        identity.host_id,
        net.host("desk").unwrap().host_id.as_bytes().to_vec()
    );
    net.shutdown().await.unwrap();
}

/// Each call a socket client makes, as it reaches the node: its method and
/// the authority it names.
type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

/// A socket at `path` in front of the node's socket at `node`, passing
/// every call on to it and writing down each one.
async fn recorded(node: &std::path::Path, path: &std::path::Path) -> Seen {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use agent_dir::local_socket::LocalStream;
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tonic::codegen::http;

    /// A connection the recording socket accepted, as tonic serves one.
    struct Accepted(LocalStream);

    impl tonic::transport::server::Connected for Accepted {
        type ConnectInfo = ();

        fn connect_info(&self) -> Self::ConnectInfo {}
    }

    impl AsyncRead for Accepted {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for Accepted {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.0).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }

    #[derive(Clone)]
    struct Recording {
        node: tonic::transport::Channel,
        seen: Seen,
    }

    impl tower::Service<http::Request<tonic::body::Body>> for Recording {
        type Response = http::Response<tonic::body::Body>;
        type Error = tonic::transport::Error;
        type Future = futures_util::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
            let authority = request
                .uri()
                .authority()
                .map(|authority| authority.to_string())
                .unwrap_or_default();
            let method = request
                .uri()
                .path()
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_owned();
            self.seen.lock().unwrap().push((method, authority));
            let node = self.node.clone();
            Box::pin(tower::ServiceExt::oneshot(node, request))
        }
    }

    let node = client::local_channel(node).await.unwrap();
    let seen = Seen::default();
    let recording = Recording {
        node,
        seen: seen.clone(),
    };
    let listener = agent_dir::local_socket::LocalListener::bind(path).unwrap();
    let incoming = futures_util::stream::unfold(listener, |mut listener| async move {
        let accepted = listener.accept().await.map(Accepted);
        Some((accepted, listener))
    });
    tokio::spawn(
        tonic::transport::Server::builder()
            .layer(tower::layer::layer_fn(move |_| recording.clone()))
            .add_routes(tonic::service::Routes::default())
            .serve_with_incoming(incoming),
    );
    seen
}

/// A socket client asks its node who it is only when a call is addressed
/// to a host, and only once, so a client that reads the node's own
/// services pays no round trip for it. A call addressed to the node's own
/// id then rides the node's channel.
#[tokio::test(flavor = "multi_thread")]
async fn a_socket_client_asks_its_nodes_id_only_once_a_call_is_addressed_to_a_host() {
    let net = Net::start(
        Topology::new().host("desk").agent(
            AgentDecl::new("worker", "desk")
                .kind(FakeKind::Codex)
                .steps(vec![Step::TurnEnd])
                .prompt("go"),
        ),
    )
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let front = dir.path().join("front.sock");
    let seen = recorded(&socket(&net, "desk").await, &front).await;
    let asked = |seen: &Seen| {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == "GetDeviceIdentity")
            .count()
    };

    let client = Client::connect(&front, Surface::Terminal).await.unwrap();
    let worker = net.agent("worker").unwrap().id.as_bytes().to_vec();
    let row = row(&client, &worker).await;
    assert_eq!(asked(&seen), 0, "{:?}", seen.lock().unwrap());

    for name in ["first", "second"] {
        client.agent(&row).rename(name).await.unwrap();
    }
    assert_eq!(asked(&seen), 1, "{:?}", seen.lock().unwrap());
    let renames: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, _)| method == "RenameAgent")
        .map(|(_, authority)| authority.clone())
        .collect();
    assert_eq!(
        renames,
        ["amux.local", "amux.local"],
        "a call to the node's own id rides the node's channel"
    );
    net.shutdown().await.unwrap();
}
