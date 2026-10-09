//! The installation's front door, called in process as the phone calls it:
//! how long a pairing window stays open, and what a pairing call must name.

mod support;

use support::synthetic::*;
use support::*;
use tonic::Request;
use wire::profile_service_server::ProfileService as _;
use wire::{Empty, PendingPairRequest, ProfileRequest, StartPairingRequest, start_pairing_request};

/// The five minutes a window lasts when the request names no length.
const DEFAULT_TTL_SECONDS: u64 = 5 * 60;
/// The longest any window may last: a demo window is a standing secret.
const MAX_TTL_SECONDS: u64 = 90 * 86_400;

fn methods() -> [(&'static str, start_pairing_request::Method); 3] {
    [
        ("pin", start_pairing_request::Method::Pin(Empty {})),
        ("qr", start_pairing_request::Method::Qr(Empty {})),
        (
            "demo",
            start_pairing_request::Method::DemoPin("246810".to_owned()),
        ),
    ]
}

/// Every method reads its window's length from the one `ttl_seconds`: a
/// length it names is the window's, an absent one is the default, and a
/// length outside one second to ninety days is refused. A window lasts
/// what it was given, whatever its method.
#[tokio::test(flavor = "multi_thread")]
async fn ttl_applies_to_each_method() {
    let install = Install::new();
    let daemon = install.start("boot-1", quiet_launch()).await;
    let door = daemon.front_door();
    let profile = install.profile.as_bytes().to_vec();
    let start = |method: start_pairing_request::Method, ttl_seconds: Option<u64>| {
        let door = door.clone();
        let profile = profile.clone();
        async move {
            door.start_pairing(Request::new(StartPairingRequest {
                profile_id: profile,
                method: Some(method),
                ttl_seconds,
            }))
            .await
            .map(tonic::Response::into_inner)
        }
    };
    let status = || {
        let door = door.clone();
        let profile = profile.clone();
        async move {
            door.get_pairing_status(Request::new(ProfileRequest {
                profile_id: profile,
            }))
            .await
            .unwrap()
            .into_inner()
            .active
        }
    };
    let cancel = || {
        let door = door.clone();
        let profile = profile.clone();
        async move {
            door.cancel_pairing(Request::new(ProfileRequest {
                profile_id: profile,
            }))
            .await
            .unwrap();
        }
    };

    for (name, method) in methods() {
        for (asked, answered) in [
            (Some(120), 120),
            (None, DEFAULT_TTL_SECONDS),
            (Some(MAX_TTL_SECONDS), MAX_TTL_SECONDS),
        ] {
            let window = start(method.clone(), asked)
                .await
                .unwrap_or_else(|status| panic!("{name} for {asked:?}: {status:?}"));
            assert_eq!(window.ttl_seconds, answered, "{name} for {asked:?}");
            assert!(status().await, "{name} for {asked:?} is open");
            cancel().await;
        }
        for refused in [0, MAX_TTL_SECONDS + 1] {
            let status = start(method.clone(), Some(refused))
                .await
                .expect_err("a window outside its bounds");
            assert_eq!(
                status.code(),
                tonic::Code::InvalidArgument,
                "{name} for {refused}s: {status:?}"
            );
        }
        assert!(!status().await, "a refused {name} window opens nothing");

        // The length is the window's, not only its answer: one second
        // later, it is closed.
        start(method, Some(1)).await.unwrap();
        until(&format!("the one-second {name} window to close"), || {
            let open = status();
            async move {
                match open.await {
                    false => Ok(()),
                    true => Err("still open"),
                }
            }
        })
        .await
        .unwrap();
        println!(
            "{name}: 120s, default {DEFAULT_TTL_SECONDS}s and 90 days kept; 0 and past 90 days refused; 1s closes"
        );
    }

    let missing = door
        .start_pairing(Request::new(StartPairingRequest {
            profile_id: profile.clone(),
            method: None,
            ttl_seconds: Some(60),
        }))
        .await
        .expect_err("a window with no method");
    assert_eq!(missing.code(), tonic::Code::InvalidArgument);

    drop(door);
    daemon.shutdown().await.unwrap();
}

/// A pending pairing is named by the token BeginPair answered; a confirm or
/// an abandon that names none is a malformed call, not a wrong secret.
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_pairing_token_is_invalid() {
    let install = Install::new();
    let daemon = install.start("boot-1", quiet_launch()).await;
    let door = daemon.front_door();
    let request = || {
        Request::new(PendingPairRequest {
            profile_id: install.profile.as_bytes().to_vec(),
            token: Vec::new(),
        })
    };
    let confirmed = door
        .confirm_pair(request())
        .await
        .expect_err("a confirm naming no pairing");
    assert_eq!(confirmed.code(), tonic::Code::InvalidArgument);
    let abandoned = door
        .abandon_pair(request())
        .await
        .expect_err("an abandon naming no pairing");
    assert_eq!(abandoned.code(), tonic::Code::InvalidArgument);
    // A token that names no pending pairing reads as a wrong secret, as it
    // always has, so a guesser learns nothing.
    let unknown = door
        .confirm_pair(Request::new(PendingPairRequest {
            profile_id: install.profile.as_bytes().to_vec(),
            token: vec![7; 16],
        }))
        .await
        .expect_err("a token nothing began");
    assert_eq!(unknown.code(), tonic::Code::PermissionDenied);

    // A profile id is its sixteen bytes; text is the CLI's to parse.
    let named_as_text = door
        .get_pairing_status(Request::new(ProfileRequest {
            profile_id: install.profile.to_string().into_bytes(),
        }))
        .await
        .expect_err("a profile id in its text form");
    assert_eq!(named_as_text.code(), tonic::Code::InvalidArgument);

    drop(door);
    daemon.shutdown().await.unwrap();
}
