//! The embedded runtime against served hosts: it pairs with a desk by the
//! PIN the desk shows, holds the desk's agents as replica rows a chat reads
//! in process, switches its source policy, and signs in to an account whose
//! relay links it to the account's hosts. Every account is a profile of the
//! one installation, which the daemon's registry creates, binds, pauses and
//! deletes.

#![cfg(unix)]

use std::sync::{Arc, Condvar, Mutex};

use app_embedded::{Cores, EdgeOverrides, EmbeddedRuntime, PairRequest, ProfileId, StartConfig};
use app_runtime::values::{
    AccountBinding, BillingInterval, Draft, PaywallFrom, RelayLink, UsageEvent,
};
use app_runtime::{AppRuntime, Wake};
use client::SystemClock;
use model::{AgentKey, Connection};
use node::SourcePolicy;
use patience::{PATIENCE, until};
use provider_fakes::script::Step;
use testnet::{AgentDecl, FakeKind, HostDecl, Net, Relay, Topology};
use ui_view::FleetCard;
use wire::{Empty, StartPairingRequest, start_pairing_request};

fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

fn worker() -> AgentDecl {
    AgentDecl::new("worker", "desk")
        .kind(FakeKind::ClaudeSdk)
        .steps(vec![
            text("turn one"),
            Step::TurnEnd,
            text("answered from the phone"),
            Step::TurnEnd,
            text("answered after the phone came back"),
            Step::TurnEnd,
        ])
        .prompt("go")
}

fn config(dir: &std::path::Path) -> StartConfig {
    StartConfig {
        data_dir: dir.join("data"),
        device_name: "phone".into(),
        log_path: None,
        discovery_scope: String::new(),
        lan: true,
        lan_bind: None,
        relay_tcp: None,
        relay_quic: None,
        relay_root: None,
        tail: 50,
        telemetry: true,
        auto_reports: false,
    }
}

/// The app's queue, as a thread of the test's: the cores step there,
/// whenever they ring, and nowhere else.
fn queue() -> Cores {
    let rang = Arc::new((Mutex::new(false), Condvar::new()));
    let ringing = rang.clone();
    let cores = Cores::new(move || {
        *ringing.0.lock().unwrap() = true;
        ringing.1.notify_all();
    });
    let stepping = cores.clone();
    std::thread::Builder::new()
        .name("app-queue".to_owned())
        .spawn(move || {
            loop {
                let mut rung = rang
                    .1
                    .wait_while(rang.0.lock().unwrap(), |rung| !*rung)
                    .unwrap();
                *rung = false;
                drop(rung);
                stepping.step();
            }
        })
        .unwrap();
    cores
}

fn loopback() -> EdgeOverrides {
    EdgeOverrides {
        lan_bind: ([127, 0, 0, 1], 0).into(),
        ..EdgeOverrides::default()
    }
}

async fn app(embedded: &EmbeddedRuntime, profile: ProfileId) -> AppRuntime {
    let wake = Arc::new(|_: Wake| {});
    AppRuntime::open(
        embedded.client(profile).unwrap(),
        Arc::new(SystemClock),
        embedded.host_id(profile).unwrap(),
        wake,
    )
    .await
    .unwrap()
}

/// The one profile a fresh installation has.
async fn only_profile(embedded: &EmbeddedRuntime) -> ProfileId {
    let profiles = embedded.profiles().unwrap();
    assert_eq!(profiles.len(), 1, "{profiles:?}");
    profiles[0].id.parse().unwrap()
}

/// Waits until `check` holds of the app's state; the failure names the
/// hosts the app lists then.
async fn eventually(app: &AppRuntime, what: &str, mut check: impl FnMut() -> bool) {
    until(what, || {
        std::future::ready(
            check()
                .then_some(())
                .ok_or_else(|| format!("hosts: {:?}", app.hosts())),
        )
    })
    .await
    .unwrap();
}

fn worker_key(net: &Net) -> AgentKey {
    let agent = net.agent("worker").unwrap();
    AgentKey {
        host: agent.host_id.as_bytes().to_vec(),
        agent: agent.id.as_bytes().to_vec(),
    }
}

/// Reads a desk agent's chat from this device's replica rows and sends it a
/// prompt the desk's agent answers.
async fn converse(app: &AppRuntime, net: &Net) {
    let agent = worker_key(net);
    eventually(app, "the desk's agent in the fleet", || {
        app.fleet_card(&agent).is_some()
    })
    .await;
    let chat = app.open_chat(&agent, 50).await.unwrap();
    let says = |wanted: &str| {
        let keys = chat.keys();
        chat.rows_for(&keys)
            .iter()
            .any(|row| format!("{:?}", row.kind).contains(wanted))
    };
    eventually(app, "the first turn from the desk", || {
        chat.frame().caught_up && says("turn one")
    })
    .await;
    chat.send(&Draft::words("hello from the phone")).await;
    eventually(app, "the desk's answer", || says("answered from the phone")).await;
}

/// Opens pairing mode on the desk: the PIN it shows and where it listens.
async fn pairing_pin(net: &Net) -> (String, Vec<String>) {
    let started = net
        .admin("desk")
        .await
        .unwrap()
        .start_pairing(StartPairingRequest {
            method: Some(start_pairing_request::Method::Pin(Empty {})),
            ttl_seconds: None,
        })
        .await
        .unwrap()
        .into_inner();
    let Some(wire::pairing_window::Secret::Pin(pin)) = started.secret else {
        panic!("the desk shows a PIN: {started:?}");
    };
    (pin, started.addrs)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_phone_pairs_by_pin_and_reads_the_desks_agents_from_its_own_rows() {
    let net = Net::start(
        Topology::new()
            .host_decl(HostDecl {
                name: "desk".into(),
                lan: true,
                ..HostDecl::default()
            })
            .agent(worker()),
    )
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let embedded = EmbeddedRuntime::start_with(
        &config(dir.path()),
        loopback(),
        Arc::new(SystemClock),
        queue(),
    )
    .await
    .unwrap();
    let phone = only_profile(&embedded).await;

    let desk = net.host("desk").unwrap();
    let (pin, addrs) = pairing_pin(&net).await;
    let wrong = embedded
        .pair(
            phone,
            &PairRequest::Pin {
                host_id: desk.host_id.as_bytes().to_vec(),
                pin: "000000".into(),
                addrs: addrs.clone(),
            },
        )
        .await;
    assert!(
        wrong.is_err() || pin == "000000",
        "a wrong PIN pairs: {wrong:?}"
    );
    let peer = embedded
        .pair(
            phone,
            &PairRequest::Pin {
                host_id: desk.host_id.as_bytes().to_vec(),
                pin: pin.clone(),
                addrs: addrs.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(peer.name, "desk");
    // The profile that paired the desk is the one that trusts it; a
    // profile made since trusts nothing.
    let other: ProfileId = embedded
        .create_profile(None)
        .await
        .unwrap()
        .id
        .parse()
        .unwrap();
    assert_eq!(
        embedded.trusting(desk.host_id.as_bytes()).unwrap(),
        vec![phone]
    );
    assert!(embedded.trusting(&[7; 16]).unwrap().is_empty());
    assert_ne!(
        embedded.host_id(other).unwrap(),
        embedded.host_id(phone).unwrap()
    );
    assert!(embedded.roster(other).unwrap().peers.is_empty());
    // Two profiles of one installation pair like any two hosts; each
    // trusts the other and nobody else.
    let link = embedded.offer_pairing(other).await.unwrap();
    embedded
        .pair(phone, &PairRequest::Link(link))
        .await
        .unwrap();
    let other_host = embedded.host_id(other).unwrap();
    assert_eq!(embedded.trusting(&other_host).unwrap(), vec![phone]);
    assert_eq!(
        embedded
            .trusting(&embedded.host_id(phone).unwrap())
            .unwrap(),
        vec![other]
    );
    embedded.delete_profile(other).await.unwrap();
    assert_eq!(embedded.profiles().unwrap().len(), 1);

    let app = app(&embedded, phone).await;
    converse(&app, &net).await;
    let hosts = app.hosts();
    assert!(
        hosts.iter().any(|host| host.name == "desk" && host.trusted),
        "{hosts:?}"
    );
    assert!(hosts.iter().any(|host| host.local && host.name == "phone"));

    // Leaving the foreground closes every session, the open chat's
    // included; returning reopens each, and the chat carries on.
    let agent = worker_key(&net);
    let chat = app.open_chat(&agent, 50).await.unwrap();
    let session = app.fleet().session(&agent).expect("the worker's session");
    app.set_foreground(false);
    assert!(session.suspended());
    assert_eq!(chat.frame().connection, Connection::Reconnecting);
    app.set_foreground(true);
    eventually(&app, "the chat back and current", || {
        let frame = chat.frame();
        frame.connection == Connection::Live && frame.caught_up
    })
    .await;
    chat.send(&Draft::words("still there?")).await;
    eventually(&app, "the desk's answer after the return", || {
        let keys = chat.keys();
        chat.rows_for(&keys)
            .iter()
            .any(|row| format!("{:?}", row.kind).contains("answered after the phone came back"))
    })
    .await;
    drop((chat, session));

    // In the background only the chats asked for keep a source.
    assert_eq!(embedded.source_policy(phone).unwrap(), SourcePolicy::Listed);
    embedded.list_sources(None);
    assert_eq!(
        embedded.source_policy(phone).unwrap(),
        SourcePolicy::OnDemand
    );
    embedded.list_sources(Some(phone));
    assert_eq!(embedded.source_policy(phone).unwrap(), SourcePolicy::Listed);

    embedded
        .unpair(phone, desk.host_id.as_bytes())
        .await
        .unwrap();
    eventually(&app, "the desk to leave the trusted hosts", || {
        !app.hosts()
            .iter()
            .any(|host| host.name == "desk" && host.trusted)
    })
    .await;

    // The installation keeps at least one profile.
    assert!(embedded.delete_profile(phone).await.is_err());

    drop(app);
    embedded.shutdown().await.unwrap();
    net.shutdown().await.unwrap();
}

/// The desk as the phone's browser resolves it.
fn found(desk: node::HostId, addrs: Vec<String>) -> app_runtime::values::Found {
    app_runtime::values::Found {
        host_id: desk.as_bytes().to_vec(),
        name: "desk".into(),
        version: node::PROTOCOL_VERSION,
        addrs,
        scope: String::new(),
    }
}

/// Whether the phone reaches the desk right now.
fn desk_online(app: &AppRuntime) -> bool {
    app.hosts()
        .iter()
        .any(|host| host.name == "desk" && host.reach.online())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_phone_links_to_its_desk_again_once_its_browser_finds_the_desk_back() {
    let mut net = Net::start(
        Topology::new()
            .host_decl(HostDecl {
                name: "desk".into(),
                lan: true,
                ..HostDecl::default()
            })
            .agent(worker()),
    )
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let embedded = EmbeddedRuntime::start_with(
        &config(dir.path()),
        loopback(),
        Arc::new(SystemClock),
        queue(),
    )
    .await
    .unwrap();
    let phone = only_profile(&embedded).await;
    let desk = net.host("desk").unwrap().host_id;
    let (pin, addrs) = pairing_pin(&net).await;
    embedded
        .pair(
            phone,
            &PairRequest::Pin {
                host_id: desk.as_bytes().to_vec(),
                pin,
                addrs,
            },
        )
        .await
        .unwrap();
    let app = app(&embedded, phone).await;
    eventually(&app, "the desk online", || desk_online(&app)).await;

    net.stop_daemon("desk").await.unwrap();
    eventually(&app, "the desk out of reach", || !desk_online(&app)).await;
    net.restart_daemon("desk").await.unwrap();
    // The phone's own browser sees the desk advertise again and hands it
    // over; nothing else tells the phone the desk is back.
    let (_, addrs) = pairing_pin(&net).await;
    embedded.discovered(vec![found(desk, addrs.clone())]);
    eventually(&app, "the desk online again", || desk_online(&app)).await;

    // Handed over while the desk is still down, the dial fails; the phone
    // tries again while its browser lists the desk, and links once the
    // desk answers.
    net.stop_daemon("desk").await.unwrap();
    eventually(&app, "the desk out of reach again", || !desk_online(&app)).await;
    embedded.discovered(vec![found(desk, addrs.clone())]);
    // The dial's outcome: its error on the desk's row, which the route
    // coming up earlier had cleared.
    eventually(&app, "the dial to the stopped desk to fail", || {
        app.hosts()
            .iter()
            .any(|host| host.name == "desk" && host.last_dial_error.is_some())
    })
    .await;
    net.restart_daemon("desk").await.unwrap();
    eventually(&app, "the desk online after a failed dial", || {
        desk_online(&app)
    })
    .await;

    // The desk loses power and comes back where it listened before: the
    // address the browser handed over still reaches it.
    net.checkpoint_host("desk").await.unwrap();
    net.rewind_host("desk", &[]).await.unwrap();
    eventually(&app, "the desk online after losing power", || {
        desk_online(&app)
    })
    .await;

    drop(app);
    embedded.shutdown().await.unwrap();
    net.shutdown().await.unwrap();
}

/// Waits until a profile's relay link says `wanted`.
async fn relay_link(embedded: &EmbeddedRuntime, profile: ProfileId, wanted: RelayLink) {
    until(&format!("the relay link to say {wanted:?}"), || async {
        let relay = embedded.account(profile).unwrap().relay;
        (relay == wanted)
            .then_some(())
            .ok_or_else(|| format!("relay link {relay:?}"))
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn each_account_is_a_profile_and_only_the_resumed_one_holds_a_relay_link() {
    let net = Net::start(Topology::new().relay(&["ada", "bob"]).host_decl(HostDecl {
        name: "desk".into(),
        account: Some("ada".into()),
        ..HostDecl::default()
    }))
    .await
    .unwrap();
    let relay = net.relay().unwrap();
    let gate = relay.gate().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path());
    config.lan = false;
    let embedded = EmbeddedRuntime::start_with(
        &config,
        EdgeOverrides {
            cloud: relay.cloud_options(&gate),
            ..loopback()
        },
        Arc::new(SystemClock),
        queue(),
    )
    .await
    .unwrap();
    let mut events = embedded.subscribe_profiles().unwrap();
    let ada = only_profile(&embedded).await;
    embedded
        .bind(Some(ada), relay.url(), "mobile", &Relay::login("ada"))
        .await
        .unwrap();
    relay_link(&embedded, ada, RelayLink::Connected).await;

    // A second account signs in: the registry makes it a profile of its
    // own, and the watch announces it.
    let bob = embedded
        .bind(None, relay.url(), "mobile", &Relay::login("bob"))
        .await
        .unwrap();
    assert_eq!(bob.subject, "bob");
    let bob: ProfileId = bob.id.parse().unwrap();
    assert_ne!(bob, ada);
    tokio::time::timeout(PATIENCE, async {
        loop {
            match events.next().await {
                Some(app_embedded::ProfileEvent::Upserted(view)) if view.subject == "bob" => break,
                Some(_) => {}
                None => panic!("the watch ended"),
            }
        }
    })
    .await
    .expect("the watch announced bob's profile");
    relay_link(&embedded, bob, RelayLink::Connected).await;

    // Both accounts' relay links stay up together.
    assert_eq!(embedded.account(ada).unwrap().relay, RelayLink::Connected);

    // Signing out keeps the profile tied to its account, and signing back
    // in finds it again.
    let out = embedded.sign_out(bob).await.unwrap();
    assert_eq!(out.account.binding, AccountBinding::SignedOut);
    assert_eq!(out.subject, "bob");
    assert_eq!(out.account.email, "bob@example.com");
    // A signed-out profile has no relay link.
    relay_link(&embedded, bob, RelayLink::Off).await;
    let again = embedded
        .bind(None, relay.url(), "mobile", &Relay::login("bob"))
        .await
        .unwrap();
    assert_eq!(again.id, bob.to_string());
    assert_eq!(embedded.profiles().unwrap().len(), 2);

    // Removing an account deletes its profile.
    embedded.delete_profile(bob).await.unwrap();
    let left = embedded.profiles().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].subject, "ada");

    embedded.shutdown().await.unwrap();
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_phone_signed_in_to_the_account_reaches_its_hosts_over_the_relay() {
    let net = Net::start(
        Topology::new()
            .relay(&["ada"])
            .host_decl(HostDecl {
                name: "desk".into(),
                account: Some("ada".into()),
                ..HostDecl::default()
            })
            .agent(worker()),
    )
    .await
    .unwrap();
    let relay = net.relay().unwrap();
    let gate = relay.gate().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path());
    config.lan = false;
    let embedded = EmbeddedRuntime::start_with(
        &config,
        EdgeOverrides {
            cloud: relay.cloud_options(&gate),
            ..loopback()
        },
        Arc::new(SystemClock),
        queue(),
    )
    .await
    .unwrap();
    let phone = only_profile(&embedded).await;
    let before = relay.refreshed_as().len();
    // The first sign-in names the profile the phone had before any account.
    let bound = embedded
        .bind(Some(phone), relay.url(), "mobile", &Relay::login("ada"))
        .await
        .unwrap();
    assert_eq!(bound.id, phone.to_string());
    assert_eq!(bound.subject, "ada");
    assert_eq!(
        bound.account.name.is_empty(),
        bound.account.email.is_empty()
    );
    // The token refreshes as the client that obtained it.
    assert_eq!(relay.refreshed_as()[before..], ["mobile"]);
    let account = embedded.account(phone).unwrap();
    assert_eq!(account.binding, AccountBinding::SignedIn);
    assert_eq!(account.email, "ada@example.com");
    assert_eq!(
        embedded.access_token(phone).await.unwrap().bearer,
        "access-ada"
    );
    relay_link(&embedded, phone, RelayLink::Connected).await;
    assert_eq!(embedded.account(phone).unwrap().pro, Some(true));
    // What the account buys changes (a purchase, or here a lapse) and the
    // phone asks at once rather than waiting for the link's next refresh.
    relay.set_tier("ada", node::harness::Tier::Free);
    embedded.refresh_entitlement(phone).await.unwrap();
    assert_eq!(embedded.account(phone).unwrap().pro, Some(false));
    relay.set_tier("ada", node::harness::Tier::Pro);
    embedded.refresh_entitlement(phone).await.unwrap();
    assert_eq!(embedded.account(phone).unwrap().pro, Some(true));
    // The account lists its hosts; pairing is what makes one trusted. The
    // desk listens on no local network: the pairing goes over the relay.
    let (pin, _) = pairing_pin(&net).await;
    let desk = net.host("desk").unwrap();
    embedded
        .pair(
            phone,
            &PairRequest::Pin {
                host_id: desk.host_id.as_bytes().to_vec(),
                pin,
                addrs: Vec::new(),
            },
        )
        .await
        .unwrap();

    let app = app(&embedded, phone).await;
    converse(&app, &net).await;

    drop(app);
    embedded.shutdown().await.unwrap();
    net.shutdown().await.unwrap();
}

/// The app's own events land on the profile they name, or on every profile
/// when they name none; coming to the front counts once an hour.
#[tokio::test(flavor = "multi_thread")]
async fn the_apps_own_events_are_recorded_on_its_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let recording = analytics::Recording::new();
    let embedded = EmbeddedRuntime::start_with(
        &config(dir.path()),
        EdgeOverrides {
            recording: Some(recording.clone()),
            ..loopback()
        },
        Arc::new(SystemClock),
        queue(),
    )
    .await
    .unwrap();
    let profile = only_profile(&embedded).await;
    let host = uuid::Uuid::from_slice(&embedded.host_id(profile).unwrap()).unwrap();

    embedded.client_opened();
    embedded.client_opened();
    embedded.record(
        None,
        UsageEvent::PaywallViewed {
            from: PaywallFrom::Hosts,
        },
    );
    embedded.record(
        Some(profile),
        UsageEvent::PurchaseStarted {
            interval: BillingInterval::Yearly,
        },
    );
    embedded.flush_analytics().await;

    let events: Vec<_> = recording
        .events()
        .into_iter()
        .filter(|(_, event)| event.name() != "installed")
        .collect();
    assert_eq!(
        events,
        [
            (
                host,
                analytics::Event::ClientOpened {
                    client: analytics::Client::Phone
                }
            ),
            (
                host,
                analytics::Event::PaywallViewed {
                    from: analytics::PaywallFrom::Hosts
                }
            ),
            (
                host,
                analytics::Event::PurchaseStarted {
                    interval: analytics::Interval::Yearly
                }
            ),
        ]
    );
    embedded.shutdown().await.unwrap();
}

fn app_card_idle(app: &AppRuntime, agent: &AgentKey) -> bool {
    app.fleet_card(agent)
        .is_some_and(|card| card.attention == model::Attention::Idle)
}

/// What the phone shows of the desk's agent: its fleet card, less how
/// reachable the desk is now, and every row of its chat.
async fn shown(app: &AppRuntime, agent: &AgentKey) -> (FleetCard, Vec<String>) {
    let card = app.fleet_card(agent).expect("the worker's fleet card");
    let chat = app.open_chat(agent, 50).await.unwrap();
    let rows = || {
        let keys = chat.keys();
        chat.rows_for(&keys)
            .iter()
            .map(|row| format!("{:?}", row.kind))
            .collect::<Vec<_>>()
    };
    // The chat's rows stream apart from the card: the answering turn's
    // end can follow the card going idle.
    eventually(app, "the chat's rows from the store", || {
        let rows = rows();
        rows.iter()
            .position(|row| row.contains("answered from the phone"))
            .is_some_and(|answer| rows[answer..].iter().any(|row| row.starts_with("TurnEnd")))
    })
    .await;
    (card, rows())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_phone_relaunched_after_a_suspend_shows_the_fleet_and_chat_it_left() {
    let net = Net::start(
        Topology::new()
            .host_decl(HostDecl {
                name: "desk".into(),
                lan: true,
                ..HostDecl::default()
            })
            .agent(worker()),
    )
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let embedded = EmbeddedRuntime::start_with(
        &config(dir.path()),
        loopback(),
        Arc::new(SystemClock),
        queue(),
    )
    .await
    .unwrap();
    let phone = only_profile(&embedded).await;
    let desk = net.host("desk").unwrap();
    let (pin, addrs) = pairing_pin(&net).await;
    embedded
        .pair(
            phone,
            &PairRequest::Pin {
                host_id: desk.host_id.as_bytes().to_vec(),
                pin,
                addrs,
            },
        )
        .await
        .unwrap();
    let first = app(&embedded, phone).await;
    converse(&first, &net).await;
    let agent = worker_key(&net);
    // The answer's text arrives before its turn ends; the turn's end can
    // reach the store after the card is read, and the relaunch shows it.
    eventually(&first, "the answering turn ended", || {
        app_card_idle(&first, &agent)
    })
    .await;
    let before = shown(&first, &agent).await;

    // The app is suspended, then the system ends it: the store's main
    // file is at a checkpoint, the ring starts at it, and nothing shuts
    // the runtime down.
    embedded.checkpoint().await.unwrap();
    let ring = dir
        .path()
        .join("data")
        .join(node::PROFILES)
        .join(phone.to_string())
        .join(node::CORE_RING);
    let checkpoints = std::fs::read_dir(&ring)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".checkpoint"))
                .count()
        })
        .unwrap_or(0);
    assert!(
        checkpoints > 0,
        "no checkpoint heads a segment in {}",
        ring.display()
    );
    drop(first);
    drop(embedded);
    // With the desk gone too, all the relaunch can show is what the
    // phone's store holds; the checkpoint left beside it is not read.
    net.shutdown().await.unwrap();

    let embedded = EmbeddedRuntime::start_with(
        &config(dir.path()),
        loopback(),
        Arc::new(SystemClock),
        queue(),
    )
    .await
    .unwrap();
    assert_eq!(only_profile(&embedded).await, phone);
    let app = app(&embedded, phone).await;
    eventually(&app, "the desk's agent in the fleet again", || {
        app.fleet_card(&agent).is_some()
    })
    .await;
    let (card, rows) = shown(&app, &agent).await;
    assert_eq!(
        FleetCard {
            host_reach: before.0.host_reach,
            ..card
        },
        before.0
    );
    assert_eq!(rows, before.1);

    drop(app);
    embedded.shutdown().await.unwrap();
}
