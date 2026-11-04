//! A whole amux client in one file: it makes sure amux is running, opens
//! the installation's first profile and prints its fleet, again each time
//! the fleet changes. docs/WRITING_A_CLIENT.md walks through it.
//!
//! cargo run -p ui-runtime --example fleet

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use client::{Clock, GrpcInstallation, Installation, SystemClock};
use ui_runtime::Fleet;
use ui_view::{FleetView, SecondLine};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Find amux, and install or start it. An installation named by
    // AMUX_CONFIG is a development or test one: attach to it, never start it.
    let config = std::env::var_os("AMUX_CONFIG").map(PathBuf::from);
    let front_door = installer::front_door(config.as_deref())?;
    if config.is_none() {
        // A client with a person in front of it asks them before adding a
        // login item; the default adds one.
        let options = installer::Options::default();
        installer::ensure(&options, |step| eprintln!("{step:?}")).await?;
    } else if !installer::answering(&front_door).await {
        return Err(format!("amux is not running at {}", front_door.display()).into());
    }

    // 2. Pick a profile and open its services.
    let installation = GrpcInstallation::connect(&front_door).await?;
    let profiles = installation.list_profiles().await?;
    let profile = profiles.first().ok_or("the installation has no profiles")?;
    let client = installation.open_profile(profile).await?;
    // Which host is this machine: the views say whether each agent's host
    // can be reached from here.
    let local_host = client.get_device_identity().await?.host_id;

    // 3. Follow the fleet. The driver keeps the stream open, reconnects
    // when amux restarts and starts over when it falls behind.
    let fleet = Fleet::connect(client, SystemClock).await?;
    let mut changed = fleet.changed();
    let mut shown = None;
    loop {
        // A change the list does not show (an agent's progress within a
        // step) wakes this too; print only what looks different.
        let now = view(&fleet, &local_host);
        if shown.as_ref() != Some(&now) {
            print(&now);
            shown = Some(now);
        }
        if let Some(error) = fleet.ended() {
            return Err(error.into());
        }
        changed.changed().await?;
    }
}

/// The fleet as amux's own clients list it: sections of families, each row
/// with the line under its name.
fn view(fleet: &Fleet, local_host: &[u8]) -> FleetView {
    let now_ms = SystemClock.now_ms();
    // Read each session before the fleet, never holding both at once.
    let lines: HashMap<_, _> = fleet
        .sessions()
        .into_iter()
        .map(|session| {
            let state = session.state();
            (
                ui_state::agent_key(state.agent()),
                ui_view::session_line(&state, now_ms),
            )
        })
        .collect();
    let state = fleet.state();
    let expanded = HashSet::new();
    ui_view::fleet_view(&state, local_host, &lines, &expanded, &|_| true)
}

fn print(view: &FleetView) {
    println!("\n{} need you, {} working", view.need_you, view.working);
    for section in &view.sections {
        println!("{:?}", section.kind);
        for row in &section.rows {
            let indent = 2 + 2 * row.depth as usize;
            let card = &row.card;
            let line = second_line(&row.second_line);
            println!("{:indent$}{} on {}: {line}", "", card.name, card.host);
        }
    }
}

fn second_line(line: &SecondLine) -> String {
    match line {
        SecondLine::Ask(ask) => format!("asks {:?}", ask.subject),
        SecondLine::Step(step) => step.step.clone().unwrap_or_else(|| "working".into()),
        SecondLine::LastSaid(said) => said.clone(),
        SecondLine::Stuck(reason) => format!("stuck: {reason:?}"),
        SecondLine::Exited(cause) => format!("exited: {cause:?}"),
        SecondLine::HostRevoked => "its host no longer trusts this machine".into(),
        SecondLine::Blank => String::new(),
    }
}
