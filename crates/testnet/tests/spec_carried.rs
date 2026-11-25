//! A call a person addresses to another host, carried there by the node
//! the person is attached to, when only the relay reaches that host and
//! the account's plan lapsed to free: it never leaves, and says why.

use node::harness::LiveRoute;
use prost::Message as _;
use support::*;
use testnet::{Net, Topology, until};
use wire::PinFolderRequest;

use crate::support;

/// A host on the relay alone: on a network of its own behind a strict
/// router, with no listener and no discovery.
fn relay_host(name: &str, account: &str) -> testnet::HostDecl {
    testnet::HostDecl {
        name: name.to_owned(),
        account: Some(account.to_owned()),
        router: Some(testnet::RouterKind::Strict),
        ..testnet::HostDecl::default()
    }
}

/// A channel on `host`'s profile socket whose calls name `authority`.
async fn socket(net: &Net, host: &str, authority: &str) -> tonic::transport::Channel {
    let path = net.runtime(host).unwrap().dir().join(node::PROFILE_SOCKET);
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

#[tokio::test(flavor = "multi_thread")]
async fn a_call_carried_to_a_host_only_a_lapsed_plan_reached_names_the_lapse() {
    let topology = Topology::new()
        .relay(&["ada"])
        .host_decl(relay_host("desk", "ada"))
        .host_decl(relay_host("laptop", "ada"));
    let mut net = Net::start(topology).await.unwrap();
    net.trust("laptop", "desk").await.unwrap();
    net.trust("desk", "laptop").await.unwrap();
    until_via(&net, "desk", "laptop", LiveRoute::Relay).await;
    let laptop = wire::authority(&host_id(&net, "laptop"));
    let mut to_laptop = wire::host_service_client(socket(&net, "desk", &laptop).await);
    // Unpinning a folder never pinned: answered, and it changes nothing.
    // Absolute on every OS, as a repository's path must be.
    let never_pinned = std::env::temp_dir().join("nowhere").join("never-pinned");
    let unpinning = || {
        tonic::Request::new(PinFolderRequest {
            path: never_pinned.to_string_lossy().into_owned(),
            pinned: false,
        })
    };
    to_laptop
        .pin_folder(unpinning())
        .await
        .expect("carried to the laptop through the relay on a paid plan");

    net.set_tier("ada", testnet::TierDecl::Free).await.unwrap();
    until("the carried call to name the lapse", || {
        let mut to_laptop = to_laptop.clone();
        async move {
            let refused = match to_laptop.pin_folder(unpinning()).await {
                Err(status) => status,
                Ok(_) => return Err("still carried".to_owned()),
            };
            let error = wire::Error::decode(refused.details()).ok();
            match error.as_ref().and_then(wire::Error::reason) {
                Some(wire::ErrorReason::PaymentRequired) => {
                    assert_eq!(refused.code(), tonic::Code::PermissionDenied);
                    assert!(
                        refused.message().contains("free plan"),
                        "the refusal names the lapse: {refused:?}"
                    );
                    Ok(())
                }
                _ => Err(format!("{refused:?}")),
            }
        }
    })
    .await
    .unwrap();
    net.shutdown().await.unwrap();
}
