//! `testnet serve <topology.json>`: starts a topology on wall time, prints
//! one readiness JSON line once it is ready, and serves its door until a
//! driver sends Shutdown or the process is interrupted or terminated.
//!
//! `testnet ctl …`: drives a served net from a shell, one command at a time,
//! for a person or an agent exploring by hand.
//!
//! `testnet journey …`: runs the shared journey stories on the real clients
//! against served topologies.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use testnet::Topology;

#[derive(Parser)]
#[command(name = "testnet", about = "The many-daemons harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start a topology and serve its door.
    Serve {
        topology: PathBuf,
        /// Where the control socket listens.
        #[arg(long, default_value = "127.0.0.1:0")]
        control: SocketAddr,
        /// Put the net's root at DIR/net instead of a random temporary
        /// directory, so every path a client draws is the same each run.
        #[arg(long, value_name = "DIR")]
        root_in: Option<PathBuf>,
        /// Route the LAN through gates the net holds, so any pair of hosts
        /// can be cut however they came to trust each other.
        #[arg(long)]
        gated_lan: bool,
        /// Each agent's facts ring, in MiB, where it should differ from the
        /// product's.
        #[arg(long, value_name = "MIB")]
        facts_ring_mib: Option<u64>,
        /// Where real providers' traffic is recorded.
        #[arg(long, value_name = "DIR")]
        recordings: Option<PathBuf>,
    },
    /// Drive a served net from the shell (`testnet ctl --help`).
    #[command(disable_help_flag = true)]
    Ctl {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
    /// Run the journey stories on the real clients (`testnet journey --help`).
    #[command(disable_help_flag = true)]
    Journey {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
}

fn main() -> ExitCode {
    // ctl and journey parse their own arguments and never need a runtime of
    // their own; they are dispatched before clap sees the rest so their help
    // reads as their own.
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("ctl") => return testnet::ctl::main(arguments[1..].to_vec()),
        #[cfg(unix)]
        Some("journey") => return testnet::journey::main(arguments[1..].to_vec()),
        _ => {}
    }
    match serve() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:?}");
            ExitCode::FAILURE
        }
    }
}

#[tokio::main]
async fn serve() -> Result<()> {
    {
        use tracing_subscriber::Layer as _;
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::util::SubscriberInitExt as _;
        // The link log is captured whatever RUST_LOG says, so the door can
        // answer for it; the rest is printed only when RUST_LOG asks.
        let printed = std::env::var_os("RUST_LOG").is_some().then(|| {
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(tracing_subscriber::EnvFilter::from_default_env())
        });
        tracing_subscriber::registry()
            .with(printed)
            .with(testnet::link_log::layer())
            .init();
    }
    let Command::Serve {
        topology,
        control,
        root_in,
        gated_lan,
        facts_ring_mib,
        recordings,
    } = Cli::parse().command
    else {
        unreachable!("ctl and journey are dispatched before parsing");
    };
    let topology = Topology::load(&topology)?;
    let options = testnet::NetOptions {
        clock: testnet::ClockMode::Wall,
        root_in,
        gated_lan,
        facts_ring_mib,
        recordings,
        ..testnet::NetOptions::default()
    };
    let mut served = testnet::door::serve_with(topology, control, options)
        .await
        .context("serving the topology")?;
    println!("{}", serde_json::to_string(&served.readiness)?);
    tokio::select! {
        () = served.closed() => {}
        _ = tokio::signal::ctrl_c() => {}
        () = terminated() => {}
    }
    served.shutdown().await?;
    Ok(())
}

/// Resolves on SIGTERM, which a supervisor sends to stop the net politely;
/// its default action would end the process without the shutdown that
/// stops the hosts and withdraws their records.
async fn terminated() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                terminate.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    }
    #[cfg(not(unix))]
    std::future::pending::<()>().await;
}
