//! The installation's front door and a profile's administration, called in
//! process as their sockets serve them: how long a pairing window stays
//! open, what a pairing call must name, and what a rename and a delete
//! change.

use std::sync::Arc;

use support::synthetic::*;
use support::*;
use tonic::Request;
use wire::installation_service_server::InstallationService as _;
use wire::profile_admin_service_server::ProfileAdminService as _;
use wire::{
    CreateProfileRequest, DeleteProfileRequest, Empty, RenameProfileRequest, StartPairingRequest,
    start_pairing_request,
};

use crate::support;

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
    let admin = daemon.profile_admin(install.profile);
    let start = |method: start_pairing_request::Method, ttl_seconds: Option<u64>| {
        let admin = admin.clone();
        async move {
            admin
                .start_pairing(Request::new(StartPairingRequest {
                    method: Some(method),
                    ttl_seconds,
                }))
                .await
                .map(tonic::Response::into_inner)
        }
    };
    let status = || {
        let admin = admin.clone();
        async move {
            admin
                .get_pairing_status(Request::new(wire::GetPairingStatusRequest {}))
                .await
                .unwrap()
                .into_inner()
                .active
        }
    };
    let cancel = || {
        let admin = admin.clone();
        async move {
            admin
                .cancel_pairing(Request::new(wire::CancelPairingRequest {}))
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

    let missing = admin
        .start_pairing(Request::new(StartPairingRequest {
            method: None,
            ttl_seconds: Some(60),
        }))
        .await
        .expect_err("a window with no method");
    assert_eq!(missing.code(), tonic::Code::InvalidArgument);

    drop(admin);
    daemon.shutdown().await.unwrap();
}

/// A pending pairing is named by the token BeginPair answered; a confirm or
/// an abandon that names none is a malformed call, not a wrong secret.
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_pairing_token_is_invalid() {
    let install = Install::new();
    let daemon = install.start("boot-1", quiet_launch()).await;
    let admin = daemon.profile_admin(install.profile);
    let confirmed = admin
        .confirm_pair(Request::new(wire::ConfirmPairRequest { token: Vec::new() }))
        .await
        .expect_err("a confirm naming no pairing");
    assert_eq!(confirmed.code(), tonic::Code::InvalidArgument);
    let abandoned = admin
        .abandon_pair(Request::new(wire::AbandonPairRequest { token: Vec::new() }))
        .await
        .expect_err("an abandon naming no pairing");
    assert_eq!(abandoned.code(), tonic::Code::InvalidArgument);
    // A token that names no pending pairing reads as a wrong secret, as it
    // always has, so a guesser learns nothing.
    let unknown = admin
        .confirm_pair(Request::new(wire::ConfirmPairRequest {
            token: vec![7; 16],
        }))
        .await
        .expect_err("a token nothing began");
    assert_eq!(unknown.code(), tonic::Code::PermissionDenied);

    // A profile id is its sixteen bytes; text is the CLI's to parse.
    let named_as_text = daemon
        .front_door()
        .delete_profile(Request::new(DeleteProfileRequest {
            profile_id: install.profile.to_string().into_bytes(),
        }))
        .await
        .expect_err("a profile id in its text form");
    assert_eq!(named_as_text.code(), tonic::Code::InvalidArgument);

    drop(admin);
    daemon.shutdown().await.unwrap();
}

/// A rename changes the label the profile is listed under, and a delete
/// takes it out of the list.
#[tokio::test(flavor = "multi_thread")]
async fn rename_and_delete_change_the_listed_profile() {
    let install = Install::new();
    let daemon = install.start("boot-1", quiet_launch()).await;
    let door = daemon.front_door();
    let created = door
        .create_profile(Request::new(CreateProfileRequest {
            label: Some("spare".to_owned()),
        }))
        .await
        .unwrap()
        .into_inner();
    let listed = || {
        let door = door.clone();
        let id = created.id.clone();
        async move {
            door.list_profiles(Request::new(wire::ListProfilesRequest {}))
                .await
                .unwrap()
                .into_inner()
                .profiles
                .into_iter()
                .find(|profile| profile.id == id)
        }
    };

    let renamed = door
        .rename_profile(Request::new(RenameProfileRequest {
            profile_id: created.id.clone(),
            override_name: Some("kept".to_owned()),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(renamed.label, "kept");
    let after = listed().await.expect("the profile is still listed");
    assert_eq!(after.label, "kept", "the list shows the new label");

    door.delete_profile(Request::new(DeleteProfileRequest {
        profile_id: created.id.clone(),
    }))
    .await
    .unwrap();
    assert!(listed().await.is_none(), "the profile is gone");

    drop(door);
    daemon.shutdown().await.unwrap();
}

/// Removing a profile ends the dumps its core asked for, as a shutdown
/// does: one still waiting for the core is let go within its time and
/// does not hold the profile's runtime, and its store, past the removal.
#[tokio::test(flavor = "multi_thread")]
async fn removing_a_profile_ends_a_dump_still_in_flight() {
    let install = Install::new();
    // Every step is over its budget, and every one asks for a dump.
    let launch = node::Launch {
        step_budget: daemon_core::StepBudget { micros: 0 },
        dump_interval_ms: 0,
        ..quiet_launch()
    };
    let daemon = install.start("boot-1", launch).await;
    let door = daemon.front_door();
    let created = door
        .create_profile(Request::new(CreateProfileRequest {
            label: Some("spare".to_owned()),
        }))
        .await
        .unwrap()
        .into_inner();
    let id: node::ProfileId = uuid::Uuid::from_slice(&created.id).unwrap();
    let runtime = daemon.profile(id).expect("the new profile runs").clone();

    // A step between two holds of the core: the dumps it asks for wait for
    // the core's part behind the second.
    let (release_first, first) = std::sync::mpsc::channel::<()>();
    let (release_second, second) = std::sync::mpsc::channel::<()>();
    let mut held = Box::pin(runtime.read_store(move |_| first.recv().is_ok()));
    assert!(poll_once(&mut held).await.is_pending());
    runtime.mark_exiting(uuid::Uuid::new_v4());
    let mut holding = Box::pin(runtime.read_store(move |_| second.recv().is_ok()));
    assert!(poll_once(&mut holding).await.is_pending());
    release_first.send(()).unwrap();
    assert!(held.await.unwrap());
    let reports = install.data_dir.join(node::REPORTS);
    until("a dump under way", || {
        let partial = std::fs::read_dir(&reports)
            .into_iter()
            .flatten()
            .any(|entry| {
                entry.is_ok_and(|entry| entry.file_name().to_string_lossy().starts_with(".dump-"))
            });
        async move { partial.then_some(()).ok_or("no dump started yet") }
    })
    .await
    .unwrap();

    // The removal lets the dump go, then waits for the store to close
    // before it removes the files, which this test's handle and hold
    // still keep open.
    let gone = Arc::downgrade(&runtime);
    let removing = daemon.front_door();
    let profile_id = created.id.clone();
    let removal = tokio::spawn(async move {
        removing
            .delete_profile(Request::new(DeleteProfileRequest { profile_id }))
            .await
    });
    // The removal holds nothing of the runtime, and no dump does: this
    // test's handle is the last.
    until("the dump let go of the runtime", || {
        let count = Arc::strong_count(&runtime);
        async move { (count == 1).then_some(()).ok_or(count) }
    })
    .await
    .expect("a dump in flight holds the removed profile's runtime");
    release_second.send(()).unwrap();
    assert!(holding.await.unwrap());
    drop(runtime);
    removal.await.unwrap().unwrap();
    assert!(gone.upgrade().is_none(), "the runtime is gone");
    assert!(
        !install
            .data_dir
            .join("profiles")
            .join(id.to_string())
            .exists(),
        "the profile's files are gone"
    );
    drop(door);
    daemon.shutdown().await.unwrap();
}

/// Polls a future once, as far as it gets before it waits.
async fn poll_once<F: std::future::Future + Unpin>(future: &mut F) -> std::task::Poll<F::Output> {
    std::future::poll_fn(|cx| std::task::Poll::Ready(std::pin::Pin::new(&mut *future).poll(cx)))
        .await
}
