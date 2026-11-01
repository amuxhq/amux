//! The flood workload's smoke: three agents and short phases, run in the
//! ordinary test lane so the workload keeps working between perf runs. It
//! checks the workload measures everything it reports, not the budgets:
//! those hold only for release builds on an enrolled machine.

#![cfg(unix)]

use qualification::perf::flood::{self, FloodOptions};

#[tokio::test(flavor = "multi_thread")]
async fn flood_smoke_with_three_agents_measures_every_metric() {
    let runs = flood::run(&FloodOptions::smoke()).await.unwrap();
    let names: Vec<&str> = runs.iter().map(|run| run.metric.name).collect();
    assert_eq!(
        names,
        [
            "flood fleet caught up",
            "flood chat caught up",
            "flood ingest lag",
            "flood agent process memory",
            "flood catch-up under K",
            "flood catch-up over K",
            "flood backlog growth with the daemon killed",
            "flood backlog drain after restart",
            "flood ingest cost per frame",
        ]
    );
    for run in &runs {
        assert!(
            !run.samples.is_empty(),
            "{} has no samples",
            run.metric.name
        );
        for sample in &run.samples {
            assert!(
                sample.value.is_finite() && sample.value >= 0.0,
                "{}: {}",
                run.metric.name,
                sample.value
            );
        }
        println!(
            "{}: {:?}",
            run.metric.name,
            run.samples
                .iter()
                .map(|sample| sample.value)
                .collect::<Vec<_>>()
        );
    }
}
