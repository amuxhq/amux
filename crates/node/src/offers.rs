//! What each provider offers on this host with no agent running, and
//! whether it is signed in. The daemon holds no provider code: it starts
//! the hidden helper `amux catalogue <provider>` the way it starts an agent
//! process, and the helper runs the provider just long enough to ask. One
//! copy per provider, kept in `providers/<provider>` under the profile, so
//! it outlives a restart; both Claude kinds share Claude's.
//!
//! A copy is asked again when it is old, when it says signed out and a
//! recheck is due (signing in should show soon), or when the provider's
//! `--version` prints something else than when it was asked. The version
//! is read at most once per recheck interval.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use prost::Message as _;
use sha2::{Digest as _, Sha256};
use store::StoreError;
pub use wire::Provider;
use wire::{Catalogue, CatalogueBlob, ErrorCode, ProviderOffer, ProviderOnHost};

use crate::runtime::{Launch, ProfileRuntime};

#[derive(Debug, thiserror::Error)]
pub enum CatalogueError {
    #[error("no provider numbered {0}")]
    NoProvider(i32),
    #[error("asking the provider what it offers: {0}")]
    Helper(String),
    #[error("reading the catalogue: {0}")]
    Read(io::Error),
    #[error("writing the catalogue: {0}")]
    Write(io::Error),
    #[error("the store: {0}")]
    Store(#[from] StoreError),
}

impl CatalogueError {
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NoProvider(_) => ErrorCode::InvalidArgument,
            Self::Helper(_) => ErrorCode::Unavailable,
            Self::Read(_) | Self::Write(_) | Self::Store(_) => ErrorCode::Internal,
        };
        wire::Error {
            code: code as i32,
            message: self.to_string(),
            details: Vec::new(),
        }
    }
}

/// The profile's directory of host copies, one file per provider. The
/// helper also runs in it, so a provider sees no project's own commands.
pub const PROVIDERS: &str = "providers";

/// How long `<provider> --version` may take.
const VERSION_PATIENCE: Duration = Duration::from_secs(10);

/// The providers a host can be asked about.
pub const ASKED: [Provider; 2] = [Provider::Claude, Provider::Codex];

/// A provider as `amux catalogue` takes it and its copy's file is named:
/// the enum value's own name, lowercase, without the enum's prefix.
pub(crate) fn provider_word(provider: Provider) -> String {
    provider
        .as_str_name()
        .trim_start_matches("PROVIDER_")
        .to_ascii_lowercase()
}

fn provider_command(provider: Provider, launch: &Launch) -> &str {
    match provider {
        Provider::Claude => &launch.claude_command,
        Provider::Codex => &launch.codex_command,
        // Not one ASKED holds.
        Provider::Unspecified => "",
    }
}

/// What a provider offers on this host, as last asked.
#[derive(Clone, Debug, PartialEq)]
pub struct HostCatalogue {
    /// The catalogue encoded once, and the SHA-256 of exactly those bytes:
    /// what GetCatalogue answers and the host's entry names.
    pub blob: CatalogueBlob,
    /// The same catalogue decoded, for an agent whose kind cannot ask its
    /// provider itself.
    pub catalogue: Catalogue,
    pub signed_in: bool,
    /// What `--version` printed when it was asked.
    pub provider_version: String,
}

struct Held {
    offer: HostCatalogue,
    asked_at_ms: i64,
    /// When the provider's version was last read and matched.
    version_read_at_ms: i64,
}

/// The copies, and one run at a time per provider.
pub(crate) struct Offers {
    held: Mutex<HashMap<Provider, Held>>,
    /// What each provider offered when asked from inside a folder an agent
    /// works in, kept in memory only.
    folders: Mutex<HashMap<(Provider, PathBuf), Held>>,
    runs: HashMap<Provider, tokio::sync::Mutex<()>>,
}

impl Offers {
    /// The copies the profile kept; an unreadable one is asked again.
    pub(crate) fn load(profile_dir: &Path) -> Self {
        let mut held = HashMap::new();
        for provider in ASKED {
            let Ok(bytes) = std::fs::read(copy_path(profile_dir, provider)) else {
                continue;
            };
            let Ok(offer) = ProviderOffer::decode(bytes.as_slice()) else {
                continue;
            };
            let asked_at_ms = offer.asked_at_ms;
            held.insert(
                provider,
                Held {
                    offer: host_catalogue(offer),
                    asked_at_ms,
                    version_read_at_ms: i64::MIN,
                },
            );
        }
        Self {
            held: Mutex::new(held),
            folders: Mutex::new(HashMap::new()),
            runs: ASKED
                .into_iter()
                .map(|provider| (provider, tokio::sync::Mutex::new(())))
                .collect(),
        }
    }

    /// Per provider asked, its hash and whether it is signed in: what this
    /// host's entry in the fleet list says.
    pub(crate) fn on_host(&self) -> Vec<ProviderOnHost> {
        let held = self.held.lock().unwrap();
        ASKED
            .into_iter()
            .filter_map(|provider| {
                let held = held.get(&provider)?;
                Some(ProviderOnHost {
                    provider: provider as i32,
                    catalogue: Some(held.offer.blob.hash.clone()),
                    signed_in: held.offer.signed_in,
                })
            })
            .collect()
    }
}

fn copy_path(profile_dir: &Path, provider: Provider) -> PathBuf {
    profile_dir.join(PROVIDERS).join(provider_word(provider))
}

/// Encodes the helper's catalogue once and names it by the hash of exactly
/// those bytes, so what is served is what is named.
fn host_catalogue(offer: ProviderOffer) -> HostCatalogue {
    let catalogue = offer.catalogue.unwrap_or_default();
    let bytes = catalogue.encode_to_vec();
    HostCatalogue {
        blob: CatalogueBlob {
            hash: Sha256::digest(&bytes).to_vec(),
            bytes,
        },
        catalogue,
        signed_in: offer.signed_in,
        provider_version: offer.provider_version,
    }
}

impl ProfileRuntime {
    /// What `provider` offers on this host, from the copy while it holds,
    /// else from the helper. A failed ask answers with the copy when there
    /// is one.
    pub async fn host_catalogue(
        &self,
        provider: Provider,
    ) -> Result<HostCatalogue, CatalogueError> {
        let offers = self.offers();
        let _run = offers.runs[&provider].lock().await;
        let launch = self.launch();
        let command = provider_command(provider, &launch).to_owned();
        let now = self.clock_now();
        let held = {
            let held = offers.held.lock().unwrap();
            held.get(&provider).map(|held| {
                let age = now.saturating_sub(held.asked_at_ms);
                let current = age < launch.catalogue_max_age_ms
                    && (held.offer.signed_in || age < launch.catalogue_recheck_ms);
                let version_due =
                    now.saturating_sub(held.version_read_at_ms) >= launch.catalogue_recheck_ms;
                (held.offer.clone(), current, version_due)
            })
        };
        let version = match &held {
            Some((offer, true, false)) => return Ok(offer.clone()),
            _ => provider_version(&command, &launch).await,
        };
        if let (Some((offer, true, _)), Some(version)) = (&held, &version)
            && *version == offer.provider_version
        {
            if let Some(held) = offers.held.lock().unwrap().get_mut(&provider) {
                held.version_read_at_ms = now;
            }
            return Ok(offer.clone());
        }

        let providers = self.dir().join(PROVIDERS);
        let asked = match self
            .ask_provider(provider, &command, &launch, &providers)
            .await
        {
            Ok(asked) => asked,
            Err(error) => {
                tracing::warn!(provider = provider_word(provider), %error, "asking what the provider offers failed");
                return match held {
                    Some((offer, _, _)) => Ok(offer),
                    None => Err(error),
                };
            }
        };
        let offer = ProviderOffer {
            provider_version: version.unwrap_or_default(),
            asked_at_ms: now,
            ..asked
        };
        let path = copy_path(self.dir(), provider);
        let bytes = offer.encode_to_vec();
        tokio::task::spawn_blocking(move || write_copy(&path, &bytes))
            .await
            .map_err(|error| CatalogueError::Write(io::Error::other(error)))?
            .map_err(CatalogueError::Write)?;
        let answer = host_catalogue(offer);
        offers.held.lock().unwrap().insert(
            provider,
            Held {
                offer: answer.clone(),
                asked_at_ms: now,
                version_read_at_ms: now,
            },
        );
        self.sync_hosts().await;
        Ok(answer)
    }

    /// What `provider` offers an agent working in `folder`. Claude lists a
    /// project's own commands and skills only when asked from inside the
    /// project, so the host's copy, asked in the profile's directory, would
    /// miss them. Asked there and kept in memory as long as the host's copy
    /// would be; an ask that fails answers with the host's catalogue.
    pub async fn folder_catalogue(
        &self,
        provider: Provider,
        folder: &Path,
    ) -> Result<HostCatalogue, CatalogueError> {
        let offers = self.offers();
        let launch = self.launch();
        let now = self.clock_now();
        let key = (provider, folder.to_owned());
        if let Some(held) = offers.folders.lock().unwrap().get(&key) {
            let age = now.saturating_sub(held.asked_at_ms);
            if age < launch.catalogue_max_age_ms
                && (held.offer.signed_in || age < launch.catalogue_recheck_ms)
            {
                return Ok(held.offer.clone());
            }
        }
        let command = provider_command(provider, &launch).to_owned();
        let asked = {
            let _run = offers.runs[&provider].lock().await;
            self.ask_provider(provider, &command, &launch, folder).await
        };
        match asked {
            Ok(asked) => {
                let answer = host_catalogue(ProviderOffer {
                    asked_at_ms: now,
                    ..asked
                });
                offers.folders.lock().unwrap().insert(
                    key,
                    Held {
                        offer: answer.clone(),
                        asked_at_ms: now,
                        version_read_at_ms: now,
                    },
                );
                Ok(answer)
            }
            Err(error) => {
                tracing::warn!(provider = provider_word(provider), folder = %folder.display(), %error, "asking what the provider offers in a folder failed");
                self.host_catalogue(provider).await
            }
        }
    }

    /// The copy for `provider` as held, asking nothing.
    pub fn held_host_catalogue(&self, provider: Provider) -> Option<HostCatalogue> {
        let held = self.offers().held.lock().unwrap();
        held.get(&provider).map(|held| held.offer.clone())
    }

    /// Runs `amux catalogue <provider>` in `folder`, in its own process
    /// group, and reads what it wrote to the profile's providers directory.
    /// The caller holds the provider's run lock.
    async fn ask_provider(
        &self,
        provider: Provider,
        command: &str,
        launch: &Launch,
        folder: &Path,
    ) -> Result<ProviderOffer, CatalogueError> {
        let dir = self.dir().join(PROVIDERS);
        std::fs::create_dir_all(&dir).map_err(CatalogueError::Write)?;
        let out = dir.join(format!("{}.asked", provider_word(provider)));
        let _ = std::fs::remove_file(&out);
        let mut helper = tokio::process::Command::new(&launch.install_path);
        helper
            .arg("catalogue")
            .arg(provider_word(provider))
            .arg("--command")
            .arg(command)
            .arg("--out")
            .arg(&out)
            .envs(&launch.provider_env)
            .current_dir(folder)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        helper.process_group(0);
        let child = helper
            .spawn()
            .map_err(|error| CatalogueError::Helper(error.to_string()))?;
        let pid = child.id();
        let deadline = Duration::from_millis(u64::try_from(launch.start_deadline_ms).unwrap_or(0));
        let output = match tokio::time::timeout(deadline, child.wait_with_output()).await {
            Ok(output) => output.map_err(|error| CatalogueError::Helper(error.to_string()))?,
            Err(_) => {
                // The provider runs in the helper's group: both go.
                if let Some(pid) = pid {
                    crate::runtime::kill_group(pid);
                }
                return Err(CatalogueError::Helper(format!(
                    "asking {} took longer than {}ms",
                    provider_word(provider),
                    launch.start_deadline_ms
                )));
            }
        };
        if !output.status.success() {
            return Err(CatalogueError::Helper(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let bytes = std::fs::read(&out).map_err(CatalogueError::Read)?;
        let _ = std::fs::remove_file(&out);
        ProviderOffer::decode(bytes.as_slice())
            .map_err(|_| CatalogueError::Helper(format!("{} does not decode", out.display())))
    }
}

/// What `<command> --version` prints, as printed; None when it cannot be
/// read.
async fn provider_version(command: &str, launch: &Launch) -> Option<String> {
    let mut probe = tokio::process::Command::new(command);
    probe
        .arg("--version")
        .envs(&launch.provider_env)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(VERSION_PATIENCE, probe.output())
        .await
        .ok()?
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Through a temporary file, so a reader never sees half a copy.
fn write_copy(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
}
