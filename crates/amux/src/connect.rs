//! Finding the daemon: the installation config, the front door, and the
//! selected profile's client socket.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use settings::InstallationConfig;
use tonic::transport::{Channel, Endpoint};
use wire::client_service_client::ClientServiceClient;
use wire::installation_service_client::InstallationServiceClient;
use wire::profile_service_client::ProfileServiceClient;

/// How long a client waits for a daemon it did not find.
const WAIT_FOR_DAEMON: Duration = Duration::from_secs(30);
const RETRY: Duration = Duration::from_millis(200);

/// The installation config: the file named by `--config` or `AMUX_CONFIG`,
/// else the default file when it exists, else the defaults.
pub fn load_config(path: Option<&Path>) -> Result<InstallationConfig> {
    let path = match path {
        Some(path) => path.to_owned(),
        None => {
            let default = InstallationConfig::default_path();
            if !default.exists() {
                return Ok(InstallationConfig::default());
            }
            default
        }
    };
    InstallationConfig::from_file(&path).with_context(|| format!("reading {}", path.display()))
}

/// A gRPC channel over the local socket named by `path`.
pub async fn channel(path: &Path) -> std::io::Result<Channel> {
    let path = path.to_owned();
    Endpoint::from_static("http://amux.local")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                agent_dir::local_socket::connect(&path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .map_err(std::io::Error::other)
}

/// The front door, if a daemon answers on it now.
pub async fn front_door_now(config: &InstallationConfig) -> Option<Channel> {
    channel(&config.front_door_socket).await.ok()
}

/// The front door. A client that finds no daemon starts nothing: without a
/// supervisor the daemon belongs to whatever service manager runs it, so
/// the client says so and waits for it to come up.
pub async fn front_door(config: &InstallationConfig) -> Result<Channel> {
    if let Some(channel) = front_door_now(config).await {
        return Ok(channel);
    }
    let path = config.front_door_socket.display();
    match config.supervisor {
        false => eprintln!(
            "No amux daemon is answering on {path}. This install has no supervisor, so the \
             daemon belongs to the service manager that runs it; waiting for it to start \
             (`amux server start` starts one by hand)."
        ),
        // The install has a supervisor: start it when none runs, and it
        // starts the daemon. Never the daemon itself, which would run
        // beside the supervisor.
        true => {
            if crate::server::running_supervisor(&config.root)?.is_none() {
                crate::server::spawn_supervisor(config, None)?;
                eprintln!("Starting amux supervise; waiting for its daemon on {path}.");
            } else {
                eprintln!(
                    "No amux daemon is answering on {path}; waiting for amux supervise to start it."
                );
            }
        }
    }
    let deadline = Instant::now() + WAIT_FOR_DAEMON;
    loop {
        tokio::time::sleep(RETRY).await;
        if let Some(channel) = front_door_now(config).await {
            return Ok(channel);
        }
        if Instant::now() >= deadline {
            bail!(
                "no amux daemon answered on {path} within {}s",
                WAIT_FOR_DAEMON.as_secs()
            );
        }
    }
}

pub fn profiles(channel: Channel) -> ProfileServiceClient<Channel> {
    ProfileServiceClient::new(channel)
}

pub fn installation(channel: Channel) -> InstallationServiceClient<Channel> {
    InstallationServiceClient::new(channel)
}

/// The profile a reference names — its id or its label — or, with none
/// given, the installation's first profile.
pub async fn select(
    profiles: &mut ProfileServiceClient<Channel>,
    wanted: Option<&str>,
) -> Result<wire::Profile> {
    let listed = profiles
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .map_err(crate::plain)?
        .into_inner()
        .profiles;
    let Some(wanted) = wanted else {
        return listed
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("the installation has no profile"));
    };
    let wanted_id = uuid::Uuid::parse_str(wanted).ok();
    listed
        .iter()
        .find(|profile| wanted_id.is_some_and(|id| profile.id == id.as_bytes()))
        .or_else(|| listed.iter().find(|profile| profile.label == wanted))
        .cloned()
        .ok_or_else(|| {
            let known = listed
                .iter()
                .map(|profile| profile.label.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow!("no profile {wanted}; profiles: {known}")
        })
}

/// A profile's id as a person reads and types it.
pub fn profile_id(profile: &wire::Profile) -> String {
    uuid::Uuid::from_slice(&profile.id).map_or_else(|_| "unknown".to_owned(), |id| id.to_string())
}

/// The selected profile, as the front door describes it.
pub async fn profile(config: &InstallationConfig, profile: Option<&str>) -> Result<wire::Profile> {
    let door = front_door(config).await?;
    select(&mut profiles(door), profile).await
}

/// The socket of a profile the front door described.
async fn profile_channel(profile: &wire::Profile) -> Result<Channel> {
    let socket = Path::new(&profile.socket_path);
    channel(socket)
        .await
        .with_context(|| format!("connecting to {}", socket.display()))
}

/// The client service of a profile the front door described.
pub async fn client_of(profile: &wire::Profile) -> Result<ClientServiceClient<Channel>> {
    Ok(wire::client_service_client(profile_channel(profile).await?))
}

/// The administration of a profile the front door described: its pairing,
/// peers and sign-out.
pub async fn admin_of(profile: &wire::Profile) -> Result<crate::pairing::Admin> {
    Ok(crate::pairing::Admin::new(profile_channel(profile).await?))
}

/// The client service of the selected profile.
pub async fn client(
    config: &InstallationConfig,
    profile: Option<&str>,
) -> Result<ClientServiceClient<Channel>> {
    client_of(&self::profile(config, profile).await?).await
}
