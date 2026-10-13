use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;

fn advert(name: &str) -> Advertisement {
    Advertisement {
        host_id: HostId::new_v4(),
        name: name.to_string(),
        version: 7,
        addrs: vec![SocketAddr::from((Ipv4Addr::new(192, 0, 2, 8), 4819))],
        scope: String::new(),
    }
}

#[test]
#[cfg(not(target_os = "ios"))]
fn txt_encoding_carries_version_host_id_name_and_scope() {
    let advert = Advertisement {
        scope: "worktree-a".to_owned(),
        ..advert("Studio")
    };
    let service = mdns::service_info_for_test(&advert).unwrap();
    let properties = service.get_properties();

    assert_eq!(properties.len(), 4);
    assert_eq!(service.get_property_val_str("name"), Some("Studio"));
    assert_eq!(service.get_property_val_str("scope"), Some("worktree-a"));
    assert_eq!(service.get_property_val_str("v"), Some("7"));
    assert_eq!(
        service.get_property_val_str("hid"),
        Some(advert.host_id.hyphenated().to_string().as_str())
    );
    assert_eq!(service.get_port(), 4819);
    assert!(service.get_fullname().ends_with(SERVICE_TYPE));
}

/// The TXT record a Mac advertises, pinned in a file the iPhone app's
/// discovery test reads too. The app parses the record with Foundation rather
/// than this crate, so a spelling only this crate accepts would pass every
/// Rust round trip and still hide the Mac from every phone.
#[test]
#[cfg(not(target_os = "ios"))]
fn txt_encoding_matches_the_record_the_phone_reads() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/discovery/advertisement.json"
    ))
    .unwrap();
    let advert = Advertisement {
        host_id: fixture["host_id"].as_str().unwrap().parse().unwrap(),
        name: "Studio".to_string(),
        version: fixture["version"].as_u64().unwrap() as u32,
        addrs: vec![SocketAddr::from((Ipv4Addr::new(192, 0, 2, 8), 4819))],
        scope: fixture["scope"].as_str().unwrap().to_owned(),
    };
    let service = mdns::service_info_for_test(&advert).unwrap();

    let txt = fixture["txt"].as_object().unwrap();
    assert_eq!(service.get_properties().len(), txt.len());
    for (key, value) in txt {
        assert_eq!(
            service.get_property_val_str(key),
            value.as_str(),
            "TXT property {key}"
        );
    }
}

#[test]
#[cfg(not(target_os = "ios"))]
fn txt_decoding_builds_an_advertisement() {
    let expected = Advertisement {
        scope: "run-7".to_owned(),
        ..advert("Studio.local")
    };
    let resolved = mdns::service_info_for_test(&expected)
        .unwrap()
        .as_resolved_service();

    let decoded = mdns::advertisement_from_service_for_test(&resolved).unwrap();

    assert_eq!(decoded, expected);
}

/// A responder that finds a second "Studio" on the network renames this
/// one's instance "Studio (2)". The machine is still called Studio, and its
/// own record says so.
#[test]
#[cfg(not(target_os = "ios"))]
fn a_renamed_instance_is_known_by_the_name_its_record_carries() {
    let advert = advert("Studio");
    let properties = txt_properties(&advert);
    let renamed = mdns_sd::ServiceInfo::new(
        SERVICE_TYPE,
        "Studio (2)",
        "amux-renamed.local.",
        &[IpAddr::V4(Ipv4Addr::new(192, 0, 2, 8))][..],
        4819,
        &properties[..],
    )
    .unwrap()
    .as_resolved_service();

    let decoded = mdns::advertisement_from_service_for_test(&renamed).unwrap();

    assert_eq!(decoded.name, "Studio");
}

#[tokio::test]
async fn scripted_announce_reaches_every_browser() {
    let discovery = ScriptedDiscovery::new();
    let peer = discovery.clone();
    let mut first = discovery.browse();
    let mut second = peer.browse();
    let advert = advert("Studio");

    discovery.announce(advert.clone());

    assert_eq!(
        first.recv().await.unwrap(),
        DiscoveryEvent::Found(advert.clone())
    );
    assert_eq!(second.recv().await.unwrap(), DiscoveryEvent::Found(advert));
}

#[tokio::test]
async fn scripted_withdraw_reaches_every_browser() {
    let discovery = ScriptedDiscovery::new();
    let mut first = discovery.browse();
    let mut second = discovery.browse();
    let host_id = HostId::new_v4();

    discovery.withdraw_host(host_id);

    assert_eq!(
        first.recv().await.unwrap(),
        DiscoveryEvent::Lost { host_id }
    );
    assert_eq!(
        second.recv().await.unwrap(),
        DiscoveryEvent::Lost { host_id }
    );
}

#[test]
fn found_hosts_returns_the_latest_addresses() {
    let hosts = FoundHosts::default();
    let host = HostId::new_v4();
    let first = SocketAddr::from((Ipv4Addr::LOCALHOST, 1001));
    let second = SocketAddr::from((Ipv4Addr::LOCALHOST, 1002));

    hosts.found(Advertisement {
        host_id: host,
        name: "Studio".to_string(),
        version: 1,
        addrs: vec![first],
        scope: String::new(),
    });
    hosts.found(Advertisement {
        host_id: host,
        name: "Studio".to_string(),
        version: 1,
        addrs: vec![second],
        scope: String::new(),
    });

    assert_eq!(hosts.addrs_for(host), vec![second]);
    assert!(hosts.addrs_for(HostId::new_v4()).is_empty());
    hosts.lost(host);
    assert!(hosts.addrs_for(host).is_empty());
}

struct MockClock {
    now: Mutex<Instant>,
}

impl MockClock {
    fn advance(&self, duration: Duration) {
        let mut now = self.now.lock().unwrap();
        *now += duration;
    }
}

impl MonotonicClock for MockClock {
    fn now(&self) -> Instant {
        *self.now.lock().unwrap()
    }
}

#[test]
fn wake_gap_requeries_after_a_monotonic_pause() {
    let clock = Arc::new(MockClock {
        now: Mutex::new(Instant::now()),
    });
    let monitor = WakeMonitor::new(clock.clone());
    let requeries = AtomicUsize::new(0);

    clock.advance(WAKE_GAP);
    monitor.tick(|| {
        requeries.fetch_add(1, Ordering::Relaxed);
    });
    assert_eq!(requeries.load(Ordering::Relaxed), 0);

    clock.advance(WAKE_GAP + Duration::from_millis(1));
    monitor.tick(|| {
        requeries.fetch_add(1, Ordering::Relaxed);
    });
    assert_eq!(requeries.load(Ordering::Relaxed), 1);
}
