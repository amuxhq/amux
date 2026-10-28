//! Reports leaving a host: a person's report is written beside the dump
//! taken when their screen froze and sent to the account the profile is
//! signed in to, or kept with the reason it was not; a dump a check asks
//! for is sent on its own when the installation sends automatic reports,
//! and counted as a usage event either way.

#![cfg(unix)]

mod support;

use analytics::{Event, Recording};
use prost::Message as _;
use provider_fakes::script::Step;
use support::*;
use testnet::{AgentDecl, Net, NetOptions, Relay, Topology, until};
use wire::{BindProfileRequest, DumpFile, DumpRequest, DumpScope, SubmitReportRequest};

const SCOPE: &str = "reports-test";

/// The manifest of the dump a report carried, unpacked from its archive.
fn carried_manifest(archive: &[u8]) -> serde_json::Value {
    let packed = zstd::stream::decode_all(archive).expect("a zstd archive");
    let part = wire::DumpPart::decode(packed.as_slice()).expect("a packed dump");
    let manifest = part
        .files
        .iter()
        .find(|file| file.name == "manifest.json")
        .expect("the dump's manifest");
    serde_json::from_slice(&manifest.contents).unwrap()
}

fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).expect("JSON")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_report_is_sent_once_signed_in_and_a_checks_dump_goes_on_its_own() {
    let recording = Recording::new();
    let net = Net::start_with(
        Topology::new()
            .relay(&["ada"])
            .host_decl(lan_host("desk", SCOPE))
            .agent(
                AgentDecl::new("worker", "desk")
                    .steps(vec![
                        Step::Text {
                            chunks: vec!["done".into()],
                        },
                        Step::TurnEnd,
                    ])
                    .prompt("start"),
            ),
        NetOptions {
            analytics: Some(node::Telemetry::Record(recording.clone())),
            auto_reports: true,
            ..NetOptions::default()
        },
    )
    .await
    .unwrap();
    let relay = net.relay().unwrap();
    let runtime = net.runtime("desk").unwrap();
    let worker = net.agent("worker").unwrap().clone();

    // Signed out: the report is written and kept, and says why it stayed.
    let dump = runtime
        .dump(DumpRequest {
            reason: "report a problem".to_owned(),
            scope: DumpScope::Report.into(),
            ..DumpRequest::default()
        })
        .await
        .unwrap();
    let files = vec![
        DumpFile {
            name: "report.json".to_owned(),
            contents: br#"{"note":"the row is wrong","parts":{"frame":"present","trace":{"absent":{"reason":"none"}}}}"#.to_vec(),
        },
        DumpFile {
            name: "frame.txt".to_owned(),
            contents: b"the frozen screen\n".to_vec(),
        },
    ];
    let kept = runtime
        .submit_report(SubmitReportRequest {
            dump_path: dump.to_string_lossy().into_owned(),
            files: files.clone(),
        })
        .await
        .unwrap();
    assert!(kept.id.is_empty(), "{kept:?}");
    assert!(kept.not_sent.contains("not signed in"), "{kept:?}");
    let report = std::path::PathBuf::from(&kept.report_path);
    assert!(report.join("dump/manifest.json").is_file());
    assert!(report.join("frame.txt").is_file());
    assert!(relay.reports().is_empty());

    // Signed in, sending it again from where it was kept goes through.
    door(&net, "desk")
        .await
        .bind_profile(BindProfileRequest {
            profile_id: Some(profile(&net, "desk")),
            cloud_url: relay.url().to_owned(),
            staged_refresh_token: Relay::login("ada"),
            // The desk already runs the worker.
            adopt_non_pristine: true,
            ..BindProfileRequest::default()
        })
        .await
        .expect("the profile binds to the account");
    let sent = runtime
        .submit_report(SubmitReportRequest {
            dump_path: kept.report_path.clone(),
            files,
        })
        .await
        .unwrap();
    assert_eq!(sent.not_sent, "", "{sent:?}");
    assert_eq!(sent.id, "report-1");
    assert_eq!(sent.report_path, kept.report_path, "the same report");
    let [received] = relay.reports().try_into().expect("one report");
    assert_eq!(received.account, "ada");
    let names: Vec<&str> = received
        .files
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(names, ["report.json", "frame.txt", "dump.pb.zst"]);
    let header = json(received.file("report.json").unwrap());
    assert_eq!(header["kind"], "report");
    assert_eq!(header["schema_version"], node::REPORT_SCHEMA);
    assert_eq!(header["note"], "the row is wrong");
    assert_eq!(header["parts"]["dump"], "present");
    let manifest = carried_manifest(received.file("dump.pb.zst").unwrap());
    assert_eq!(manifest["scope"], "report");

    // A check that broke: its dump goes on its own, and is counted.
    runtime
        .dump(DumpRequest {
            agent_ids: vec![worker.id.as_bytes().to_vec()],
            reason: "the client's transcript: a test".to_owned(),
            automatic: true,
            scope: DumpScope::Transcript.into(),
            check: ui_runtime::TRANSCRIPT_CHECK.to_owned(),
            ..DumpRequest::default()
        })
        .await
        .unwrap();
    let automatic = until("the check's dump to arrive", || async {
        let reports = relay.reports();
        reports
            .get(1)
            .cloned()
            .ok_or_else(|| format!("{} reports", reports.len()))
    })
    .await
    .unwrap();
    let header = json(automatic.file("report.json").unwrap());
    assert_eq!(header["kind"], "automatic");
    assert_eq!(header["core"], "transcript");
    assert_eq!(header["check"], ui_runtime::TRANSCRIPT_CHECK);
    assert!(automatic.file("frame.png").is_none() && automatic.file("frame.txt").is_none());
    let manifest = carried_manifest(automatic.file("dump.pb.zst").unwrap());
    assert_eq!(manifest["scope"], "transcript");
    assert_eq!(manifest["check"], ui_runtime::TRANSCRIPT_CHECK);
    let counted: Vec<_> = recording
        .events()
        .into_iter()
        .filter_map(|(_, event)| match event {
            Event::CheckFailed { check, .. } => Some(check.as_str().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(counted, [ui_runtime::TRANSCRIPT_CHECK]);

    drop(runtime);
    net.shutdown().await.unwrap();
}
