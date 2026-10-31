//! Writes a performance workload's topology for `testnet serve`: `phone`,
//! the one `just ios perf` measures the phone on, or `flood`, the one the
//! flood workload measures. The files are generated at run time rather than
//! committed: they are what the workloads build, and the phone's long
//! conversation carries a thousand rows of prose and tool output, which is
//! nothing to review by eye.

use qualification::perf::{flood, phone};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(workload), Some(path)) = (args.next(), args.next()) else {
        return Err("usage: perf-topology phone|flood PATH".into());
    };
    let topology = match workload.as_str() {
        "phone" => phone::topology(),
        "flood" => flood::topology(&flood::FloodOptions::full()),
        other => return Err(format!("perf-topology: unknown workload {other}").into()),
    };
    let mut text = serde_json::to_string_pretty(&topology)?;
    text.push('\n');
    std::fs::write(&path, text)?;
    println!("wrote {path}");
    Ok(())
}
