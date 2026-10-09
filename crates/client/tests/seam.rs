//! Both ways of reaching the client service answer alike: the in-process
//! call and gRPC over the profile socket, against one real node.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use client::{Client, GrpcClient, InProcess, RpcError};
use futures_util::StreamExt as _;
use provider_fakes::script::Step;
use testnet::{AgentDecl, FakeKind, Net, Topology};
use wire::{
    ErrorCode, FetchRequest, GetBlobRequest, GetRequest, ListProfilesRequest, PutBlobRequest,
    SubscribeRequest, inventory_event, session_event,
};

const PATIENCE: Duration = Duration::from_secs(20);

async fn clients(net: &Net) -> [(&'static str, Arc<dyn Client>); 2] {
    let profiles = net
        .front_door("desk")
        .await
        .unwrap()
        .list_profiles(ListProfilesRequest {})
        .await
        .unwrap()
        .into_inner()
        .profiles;
    let grpc = GrpcClient::connect(profiles[0].socket_path.as_ref())
        .await
        .unwrap();
    [
        (
            "in process",
            Arc::new(InProcess::new(net.client("desk").unwrap())),
        ),
        ("grpc", Arc::new(grpc)),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn the_in_process_and_socket_clients_answer_alike() {
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
    for (name, client) in clients(&net).await {
        // The inventory lists the agent, then catches up.
        let mut inventory = client.subscribe_inventory().await.unwrap();
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
        let mut stream = client
            .subscribe(SubscribeRequest {
                agent_id: worker.clone(),
                tail: 40,
            })
            .await
            .unwrap();
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
            .get(GetRequest {
                agent_id: worker.clone(),
                key: key.clone().unwrap(),
            })
            .await
            .unwrap();
        assert!(item.text.contains("hello"), "{name}");
        let page = client
            .fetch(FetchRequest {
                agent_id: worker.clone(),
                before_order: None,
                limit: 100,
            })
            .await
            .unwrap();
        assert!(page.exhausted, "{name}");
        assert!(page.items.iter().any(|held| held.key == item.key), "{name}");

        // Blobs go in and come back out.
        let blob = client
            .put_blob(PutBlobRequest {
                agent_id: worker.clone(),
                name: "note.txt".into(),
                mime: "text/plain".into(),
                bytes: format!("from {name}").into_bytes(),
            })
            .await
            .unwrap();
        let back = client
            .get_blob(GetBlobRequest {
                agent_id: worker.clone(),
                hash: blob.hash,
            })
            .await
            .unwrap();
        assert_eq!(back.bytes, format!("from {name}").into_bytes());

        // The runtime's own errors arrive whole.
        let missing = client
            .get(GetRequest {
                agent_id: worker.clone(),
                key: "no-such-key".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(missing.code(), Some(ErrorCode::NotFound), "{name}");
        assert!(!missing.is_transport());
    }
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_socket_client_says_transport_while_its_daemon_is_down_and_redials_after() {
    let mut net = Net::start(Topology::new().host("desk")).await.unwrap();
    let [_, (_, client)] = clients(&net).await;
    let mut inventory = client.subscribe_inventory().await.unwrap();
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
    match client.subscribe_inventory().await {
        Err(RpcError::Transport(_)) => {}
        Err(other) => panic!("a down daemon is a transport failure, not {other:?}"),
        Ok(_) => panic!("nothing answers while the daemon is down"),
    }
    net.restart_daemon("desk").await.unwrap();
    patience::until("the same client to redial the restarted daemon", || async {
        client
            .subscribe_inventory()
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .await
    .unwrap();
    net.shutdown().await.unwrap();
}
