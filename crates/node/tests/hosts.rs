//! The fleet list's entries for other hosts: what a host's handshake said
//! about itself reaches the entry another host lists it under.

mod support;

use support::synthetic::*;
use support::*;

/// After a handshake, the laptop lists the desk with the version and the
/// capabilities the desk's handshake gave, the kinds it runs among them.
#[tokio::test(flavor = "multi_thread")]
async fn remote_entry_carries_version_and_capabilities() {
    let desk = Install::new();
    let mut options = desk.options("boot-1", quiet_launch());
    options.edge.kinds = vec![wire::Kind::ClaudeSdk, wire::Kind::Codex];
    let desk_daemon = node::start(options, None).await.unwrap();
    let desk_runtime = runtime(&desk_daemon, &desk);
    let laptop = Install::new();
    let laptop_daemon = laptop.start("boot-1", quiet_launch()).await;
    let laptop_runtime = runtime(&laptop_daemon, &laptop);
    let (desk_edge, laptop_edge) = (desk_runtime.edge().unwrap(), laptop_runtime.edge().unwrap());
    let desk_host = desk_runtime.host();
    desk_edge.trust(&laptop_edge).await.unwrap();
    laptop_edge.trust(&desk_edge).await.unwrap();
    let _link = laptop_edge.link_in_process(&desk_edge).unwrap();
    assert!(laptop_edge.wait_for_route(desk_host, PATIENCE).await);

    let expected = wire::Capabilities {
        cloud_relay: false,
        kinds: vec![wire::Kind::ClaudeSdk as i32, wire::Kind::Codex as i32],
    };
    assert_eq!(desk_edge.capabilities(), expected, "the desk's own word");
    until(
        "the laptop to list the desk's version and capabilities",
        || {
            let listed = laptop_runtime
                .host_entry_of(desk_host)
                .map(|entry| (entry.version, entry.capabilities));
            let expected = (Some(node::version().to_owned()), Some(expected.clone()));
            async move {
                match listed {
                    Some(listed) if listed == expected => Ok(()),
                    other => Err(format!("{other:?}")),
                }
            }
        },
    )
    .await
    .unwrap();

    drop((desk_edge, laptop_edge, desk_runtime, laptop_runtime));
    desk_daemon.shutdown().await.unwrap();
    laptop_daemon.shutdown().await.unwrap();
}

/// A host-set change made while an inventory stream is opening, after the
/// opening has read the host set, reaches that stream: the core answers the
/// opening before it steps the change, so the change arrives live.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_published_while_an_inventory_opens_reaches_it() {
    let install = Install::new();
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);
    let newcomer = uuid::Uuid::new_v4().as_bytes().to_vec();
    let entry = wire::HostEntry {
        host_id: newcomer.clone(),
        name: "newcomer".to_owned(),
        trust: wire::Trust::Candidate as i32,
        presence: wire::Presence::Online as i32,
        ..wire::HostEntry::default()
    };
    let target = std::sync::Arc::downgrade(&runtime);
    let hook: node::JoinHook = std::sync::Arc::new(move || {
        let (target, entry) = (target.clone(), entry.clone());
        Box::pin(async move {
            if let Some(runtime) = target.upgrade() {
                runtime.publish_host(entry);
            }
        })
    });
    runtime.set_inventory_opening_hook(Some(hook));
    let mut inventory = runtime.subscribe_inventory().await.unwrap();
    runtime.set_inventory_opening_hook(None);

    let listed = tokio::time::timeout(PATIENCE, async {
        while let Some(event) = inventory.next().await {
            if let Some(wire::inventory_event::Of::Host(host)) = &event.of
                && host.host_id == newcomer
            {
                return true;
            }
        }
        false
    })
    .await;
    assert!(
        matches!(listed, Ok(true)),
        "the host published while the stream opened never reached it"
    );

    drop((inventory, runtime));
    daemon.shutdown().await.unwrap();
}
