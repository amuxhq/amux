//! The fleet list's entries for other hosts: what a host's handshake said
//! about itself reaches the entry another host lists it under.

use support::synthetic::*;
use support::*;

use crate::support;

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
    assert!(wait_for_route(&laptop_edge, desk_host, PATIENCE).await);

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
                .and_then(|entry| entry.host)
                .map(|host| (host.version, host.capabilities));
            let expected = (node::version().to_owned(), Some(expected.clone()));
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

/// A link and the channels over it reach the profile's core as decoded
/// inputs, never as streams: the link coming up, what the peer's handshake
/// says it reaches, each channel the core said to open with the key the
/// peer was paired under, and the link going.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_and_its_channels_reach_the_core_as_inputs() {
    use daemon_core::Input;
    use daemon_core::payload::{CarrierKind, ChannelClass};

    let desk = Install::new();
    let desk_daemon = desk.start("boot-1", quiet_launch()).await;
    let desk_runtime = runtime(&desk_daemon, &desk);
    let laptop = Install::new();
    let laptop_daemon = laptop.start("boot-1", quiet_launch()).await;
    let laptop_runtime = runtime(&laptop_daemon, &laptop);
    let (desk_edge, laptop_edge) = (desk_runtime.edge().unwrap(), laptop_runtime.edge().unwrap());
    let desk_host = daemon_core::HostId(*desk_runtime.host().as_bytes());
    desk_edge.trust(&laptop_edge).await.unwrap();
    laptop_edge.trust(&desk_edge).await.unwrap();
    let link = laptop_edge.link_in_process(&desk_edge).unwrap();
    assert!(wait_for_route(&laptop_edge, desk_runtime.host(), PATIENCE).await);

    let ring = laptop.profile_dir().join(node::CORE_RING);
    let inputs = || {
        core_ring::saved::<daemon_core::Recorded>(&ring)
            .ok()
            .flatten()
            .map(|saved| saved.inputs)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|line| match line {
                daemon_core::Recorded::Input(input) => Some(*input),
                daemon_core::Recorded::Read { .. } => None,
            })
            .collect::<Vec<_>>()
    };
    until("the laptop's core to hear a channel to the desk opened", || {
        let inputs = inputs();
        async move {
            match inputs
                .iter()
                .any(|input| matches!(input, Input::ChannelOpened { peer, .. } if *peer == desk_host))
            {
                true => Ok(()),
                false => Err(format!("{} inputs", inputs.len())),
            }
        }
    })
    .await
    .unwrap();
    let seen = inputs();
    let Some(up) = seen.iter().find_map(|input| match input {
        Input::LinkUp {
            link, peer, kind, ..
        } if *peer == desk_host => Some((*link, *kind)),
        _ => None,
    }) else {
        panic!("the link coming up reached the core: {seen:?}");
    };
    assert_eq!(up.1, CarrierKind::Direct);
    assert!(
        seen.iter().any(|input| matches!(
            input,
            Input::LinkControl { link, frame }
                if *link == up.0
                    && matches!(frame.body, Some(wire::message::Body::HelloAck(_)))
        )),
        "the desk's answer to the laptop's hello reached the core"
    );
    let Some((wanted, pin)) = seen.iter().find_map(|input| match input {
        Input::WantChannel {
            link,
            peer,
            class: ChannelClass::Calls,
            pin,
        } if *link == up.0 && *peer == desk_host => Some((link, pin.clone())),
        _ => None,
    }) else {
        panic!("a calls channel was asked of the core: {seen:?}");
    };
    assert_eq!(*wanted, up.0);
    assert!(
        pin.is_some_and(|pin| !pin.0.is_empty()),
        "the channel was asked with the key the desk was paired under"
    );

    link.sever();
    until("the laptop's core to hear the link went", || {
        let gone = inputs()
            .iter()
            .any(|input| matches!(input, Input::LinkDown { link, .. } if *link == up.0));
        async move {
            if gone {
                Ok(())
            } else {
                Err("still up".to_owned())
            }
        }
    })
    .await
    .unwrap();

    drop((link, desk_edge, laptop_edge, desk_runtime, laptop_runtime));
    desk_daemon.shutdown().await.unwrap();
    laptop_daemon.shutdown().await.unwrap();
}
