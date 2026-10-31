//! `amux status`: what this machine's amux is doing, read without starting
//! it. The answer is also the exit status: 0 when the daemon answers, 3
//! when it does not.

use std::path::Path;

use anyhow::Result;
use node::supervisor::Files;
use settings::InstallationConfig;
use wire::cloud_standing::Standing;
use wire::relay_link::State;

use crate::connect;
use crate::server::running_supervisor;

pub async fn run(config_path: Option<&Path>) -> Result<bool> {
    let config = connect::load_config(config_path)?;
    let door = connect::front_door_now(&config).await;
    let daemon = match &door {
        Some(door) => connect::installation(door.clone())
            .get_installation(wire::GetInstallationRequest {})
            .await
            .ok()
            .map(|installation| installation.into_inner().version),
        None => None,
    };
    let supervisor = if config.supervisor {
        running_supervisor(&config.root)?
    } else {
        None
    };
    let this = node::version();
    println!(
        "{}",
        running_line(
            this,
            daemon.as_deref(),
            config.supervisor,
            supervisor.as_deref()
        )
    );
    if config.supervisor {
        let mut updates = format!(
            "Updates: {}, from the {} channel.",
            match config.updates()? {
                settings::Updates::Auto => "installed automatically",
                settings::Updates::Manual => "installed by `amux update`",
            },
            config.channel_name()
        );
        // Only a build newer than this one is still waiting: a later update
        // past it leaves the old record behind.
        let rejected = std::env::current_exe()
            .ok()
            .and_then(|binary| Files::beside(&binary).rejected())
            .filter(|rejected| semver::Version::parse(this).is_ok_and(|this| *rejected > this));
        if let Some(rejected) = rejected {
            updates.push_str(&format!(
                " amux {rejected} did not start here and was rolled back; \
                 `amux update --retry` tries it again."
            ));
        }
        println!("{updates}");
    }
    if let Some(door) = door
        && daemon.is_some()
    {
        let profiles = connect::installation(door)
            .list_profiles(wire::ListProfilesRequest {})
            .await
            .map_err(crate::plain)?
            .into_inner()
            .profiles;
        for profile in profiles {
            println!("Profile {}: {}", profile.label, account(&profile));
        }
    }
    println!("{}", telemetry_line(&config));
    let file = config_path
        .map(Path::to_owned)
        .unwrap_or_else(InstallationConfig::default_path);
    println!("Config: {}", file.display());
    Ok(daemon.is_some())
}

fn running_line(
    this: &str,
    daemon: Option<&str>,
    supervised: bool,
    supervisor: Option<&str>,
) -> String {
    let under = match (supervised, supervisor) {
        (true, Some(pid)) => format!(" under the supervisor (pid {pid})"),
        (true, None) => String::new(),
        (false, _) => " without a supervisor".to_owned(),
    };
    match daemon {
        Some(version) if version == this => format!("amux {version} is running{under}."),
        Some(version) => format!(
            "amux {version} is running{under}; this command is amux {this}, and the daemon \
             takes it when it next starts."
        ),
        None if supervisor.is_some() => format!(
            "amux {this} is starting: the supervisor is up{under}, its daemon is not answering \
             yet."
        ),
        None if supervised => {
            format!("amux {this} is not running; any amux command starts it.")
        }
        None => format!(
            "amux {this} is not running; this host's service manager starts it \
             (`amux server start` starts one by hand)."
        ),
    }
}

/// A profile's account as a sentence's object.
fn account(profile: &wire::Profile) -> String {
    let who = if profile.email.is_empty() {
        profile.account_name.clone()
    } else {
        profile.email.clone()
    };
    let standing = profile
        .cloud
        .as_ref()
        .and_then(|cloud| cloud.standing.as_ref());
    match standing {
        None | Some(Standing::Unbound(_)) => {
            "not signed in; `amux login` reaches your hosts through the relay".to_owned()
        }
        Some(Standing::SignedOut(_)) => format!("signed out of {who}"),
        Some(Standing::SignedIn(link)) => {
            let relay = match link.state.as_ref() {
                Some(State::Connected(_)) => "relay connected",
                Some(State::Connecting(_)) | None => "relay connecting",
                Some(State::Retrying(_)) => "relay unreachable, retrying",
                Some(State::SignInAgain(_)) => "the account needs `amux login` again",
                Some(State::VersionMismatch(_)) => "the relay refuses this version; `amux update`",
            };
            format!("signed in as {who}, {relay}")
        }
    }
}

/// Whether amux sends product analytics and where.
fn telemetry_line(config: &InstallationConfig) -> String {
    let destination = analytics::Endpoint::resolve(node::release::is_published())
        .map(|endpoint| endpoint.describe(settings::DEFAULT_CLOUD_URL));
    match (config.telemetry, destination) {
        (false, _) => "Telemetry: off. amux sends no product analytics.".to_owned(),
        (true, _) if analytics::do_not_track() => {
            // This command sees its own shell's environment, not the
            // daemon's: one started at login never sees a variable set in a
            // shell profile, so only the setting is sure to reach it.
            "Telemetry: on, but DO_NOT_TRACK is set in this shell, so a daemon started from \
             it sends nothing. A daemon started at login does not see it; \
             `amux config set telemetry false` turns telemetry off for every daemon."
                .to_owned()
        }
        (true, None) => {
            "Telemetry: on, but this is a development build, which sends nothing.".to_owned()
        }
        (true, Some(destination)) => format!(
            "Telemetry: on. Usage events go to {destination}, then to PostHog in the EU. \
             `amux config set telemetry false` stops them."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_running_line_says_who_runs_amux_and_whether_versions_differ() {
        assert_eq!(
            running_line("1.0.0", Some("1.0.0"), true, Some("42")),
            "amux 1.0.0 is running under the supervisor (pid 42)."
        );
        assert!(
            running_line("1.1.0", Some("1.0.0"), true, Some("42"))
                .contains("this command is amux 1.1.0")
        );
        assert_eq!(
            running_line("1.0.0", None, true, None),
            "amux 1.0.0 is not running; any amux command starts it."
        );
        assert!(running_line("1.0.0", None, false, None).contains("service manager starts it"));
    }
}
