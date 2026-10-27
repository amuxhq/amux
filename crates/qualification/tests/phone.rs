//! The phone's performance topology, as `testnet journey perf` serves it.

use qualification::perf::phone;

#[test]
fn the_topology_loads_as_served_and_holds_the_workload() {
    let generated = phone::topology();
    let text = serde_json::to_string(&generated).unwrap();
    let served: testnet::Topology = serde_json::from_str(&text).unwrap();
    assert_eq!(served, generated);
    assert_eq!(served.hosts.len(), phone::HOSTS.len());
    assert_eq!(served.agents.len(), phone::FLEET_AGENTS + 1);
    assert_eq!(phone::long_rows(phone::LONG_ROWS).len(), phone::LONG_ROWS);
    let stream = phone::stream_script();
    assert_eq!(
        stream.steps.len(),
        phone::LONG_ROWS + 2 * phone::STREAM_SAMPLES + 1
    );
    let gates = stream
        .steps
        .iter()
        .filter(|step| matches!(step, provider_fakes::script::Step::WaitFor { .. }))
        .count();
    assert_eq!(gates, phone::STREAM_SAMPLES);
}
