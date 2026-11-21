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
use wire::{Catalogue, ErrorCode, GetCatalogueResponse, ProviderOffer, ProviderOnHost};

use crate::runtime::{Launch, ProfileRuntime};

#[derive(Debug, thiserror::Error)]
pub enum CatalogueError {
    #[error("no provider numbered {0}")]
    NoProvider(i32),
    /// No built-in agent and no `[agents.<id>]` entry has the id.
    #[error("this computer knows no agent {0:?}")]
    NoAgent(String),
    /// The agent's command is not found where agents are started from.
    #[error("{0} is not installed on this computer")]
    NotInstalled(String),
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
            Self::NoProvider(_) | Self::NoAgent(_) => ErrorCode::InvalidArgument,
            Self::NotInstalled(_) => ErrorCode::FailedPrecondition,
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
    pub blob: GetCatalogueResponse,
    /// The same catalogue decoded, for an agent whose kind cannot ask its
    /// provider itself.
    pub catalogue: Catalogue,
    pub signed_in: bool,
    /// What `--version` printed when it was asked; for an ACP agent, the
    /// version its `initialize` answer names.
    pub provider_version: String,
    /// What an ACP agent's `initialize` said beyond its catalogue: whether
    /// it reopens a conversation, what a prompt may carry, how it is signed
    /// in to. Its sign-in state is the best this host knows. None for Claude
    /// and Codex.
    pub acp: Option<wire::AcpOffer>,
}

struct Held {
    /// What the provider said, before the host's remembered picks.
    asked: ProviderOffer,
    offer: HostCatalogue,
    asked_at_ms: i64,
    /// When the provider's version was last read and matched.
    version_read_at_ms: i64,
}

/// The copies, and one run at a time per provider.
pub(crate) struct Offers {
    held: Mutex<HashMap<Provider, Held>>,
    /// Each ACP agent's copy, by its id, read from its file when first
    /// asked for. Kept apart from Claude's and Codex's because the wire's
    /// providers name those two alone; an agent's id is the key it will
    /// have once the wire names every provider by its id.
    agents: Mutex<HashMap<String, Held>>,
    /// One helper run at a time per ACP agent.
    agent_runs: Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    /// What each provider offered when asked from inside a folder an agent
    /// works in, kept in memory only.
    folders: Mutex<HashMap<(Provider, PathBuf), Held>>,
    runs: HashMap<Provider, tokio::sync::Mutex<()>>,
}

impl Offers {
    /// The copies the profile kept; an unreadable one is asked again.
    pub(crate) fn load(profile_dir: &Path, launch: &Launch) -> Self {
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
                    offer: host_catalogue(offer.clone(), &remembered(provider, launch)),
                    asked: offer,
                    asked_at_ms,
                    version_read_at_ms: i64::MIN,
                },
            );
        }
        Self {
            held: Mutex::new(held),
            agents: Mutex::new(HashMap::new()),
            agent_runs: Mutex::new(HashMap::new()),
            folders: Mutex::new(HashMap::new()),
            runs: ASKED
                .into_iter()
                .map(|provider| (provider, tokio::sync::Mutex::new(())))
                .collect(),
        }
    }

    /// Names the copies again after the host's remembered picks changed, so
    /// each hash follows what a client would now be told.
    pub(crate) fn remembered_changed(&self, launch: &Launch) {
        for (provider, held) in self.held.lock().unwrap().iter_mut() {
            held.offer = host_catalogue(held.asked.clone(), &remembered(*provider, launch));
        }
    }

    /// The copy of what `provider` offers, as last asked.
    pub(crate) fn offered(&self, provider: Provider) -> Option<Catalogue> {
        let held = self.held.lock().unwrap();
        held.get(&provider).map(|held| held.offer.catalogue.clone())
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

/// An ACP agent's copy: named by its id, which is never `claude` or `codex`.
fn agent_copy_path(profile_dir: &Path, agent: &str) -> PathBuf {
    profile_dir.join(PROVIDERS).join(agent)
}

/// The picks a new agent of `provider` starts with on this host. A host that
/// runs Claude in its own terminal keeps to Claude's own settings.
fn remembered(provider: Provider, launch: &Launch) -> settings::NewAgentDefaults {
    match provider {
        Provider::Claude if launch.claude_kind == wire::Kind::ClaudePty => {
            settings::NewAgentDefaults::default()
        }
        Provider::Claude => launch.new_agent.claude.clone(),
        Provider::Codex => launch.new_agent.codex.clone(),
        // Not one ASKED holds.
        Provider::Unspecified => settings::NewAgentDefaults::default(),
    }
}

/// Encodes the helper's catalogue once, with the host's remembered picks
/// over what the provider would start a new agent with, and names it by the
/// hash of exactly those bytes, so what is served is what is named.
fn host_catalogue(offer: ProviderOffer, remembered: &settings::NewAgentDefaults) -> HostCatalogue {
    let mut catalogue = offer.catalogue.unwrap_or_default();
    if remembered.model.is_some() || remembered.effort.is_some() {
        let starting = catalogue.new_agent.get_or_insert_default();
        if remembered.model.is_some() {
            starting.model.clone_from(&remembered.model);
        }
        if remembered.effort.is_some() {
            starting.effort.clone_from(&remembered.effort);
        }
    }
    let bytes = catalogue.encode_to_vec();
    HostCatalogue {
        blob: GetCatalogueResponse {
            hash: Sha256::digest(&bytes).to_vec(),
            bytes,
        },
        catalogue,
        signed_in: offer.signed_in,
        provider_version: offer.provider_version,
        acp: offer.acp,
    }
}

/// What this host's own conversations with an ACP agent have shown, for
/// what the agent tells only a session: its models, modes and commands,
/// and whether it is signed in.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Learned {
    /// The catalogue the newest conversation that published one published.
    pub catalogue: Option<Catalogue>,
    /// The sign-in state the newest conversation that knew one was in: a
    /// session refused as signed out says so.
    pub sign_in: wire::SignInState,
}

/// The copy an ACP agent's helper answer and its conversations make
/// together: the helper's catalogue where it lists models without a
/// session (Grok), else the newest conversation's; and the helper's
/// sign-in state where `initialize` shows one, else the newest
/// conversation's. A new agent starts on the agent's own defaults, so no
/// remembered pick is laid over it.
fn agent_catalogue(asked: &ProviderOffer, learned: Learned) -> HostCatalogue {
    let mut offer = asked.clone();
    let listed = offer
        .catalogue
        .as_ref()
        .is_some_and(|catalogue| !catalogue.models.is_empty());
    if !listed && let Some(catalogue) = learned.catalogue {
        offer.catalogue = Some(catalogue);
    }
    let acp = offer.acp.get_or_insert_default();
    if acp.sign_in() == wire::SignInState::Unspecified
        && learned.sign_in != wire::SignInState::Unspecified
    {
        acp.set_sign_in(learned.sign_in);
        offer.signed_in = learned.sign_in != wire::SignInState::SignedOut;
    }
    host_catalogue(offer, &settings::NewAgentDefaults::default())
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
        tokio::task::spawn_blocking(move || crate::install::write_cache(&path, &bytes))
            .await
            .map_err(|error| CatalogueError::Write(io::Error::other(error)))?
            .map_err(CatalogueError::Write)?;
        let answer = host_catalogue(offer.clone(), &remembered(provider, &launch));
        offers.held.lock().unwrap().insert(
            provider,
            Held {
                asked: offer,
                offer: answer.clone(),
                asked_at_ms: now,
                version_read_at_ms: now,
            },
        );
        self.tell_own_host();
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
                let asked = ProviderOffer {
                    asked_at_ms: now,
                    ..asked
                };
                let answer = host_catalogue(asked.clone(), &Default::default());
                offers.folders.lock().unwrap().insert(
                    key,
                    Held {
                        asked,
                        offer: answer.clone(),
                        asked_at_ms: now,
                        version_read_at_ms: now,
                    },
                );
                Ok(answer)
            }
            Err(error) => {
                tracing::warn!(provider = provider_word(provider), %error, "asking what the provider offers in a folder failed");
                self.host_catalogue(provider).await
            }
        }
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
        let word = provider_word(provider);
        self.run_helper(&word, command, &[], &launch.provider_env, launch, folder)
            .await
    }

    /// Runs `amux internal catalogue <word> --command <command> [--arg
    /// <arg>]...` in `folder`, with `env` added to the daemon's
    /// environment. The caller holds the provider's or agent's run lock.
    async fn run_helper(
        &self,
        word: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        launch: &Launch,
        folder: &Path,
    ) -> Result<ProviderOffer, CatalogueError> {
        let dir = self.dir().join(PROVIDERS);
        std::fs::create_dir_all(&dir).map_err(CatalogueError::Write)?;
        let out = dir.join(format!("{word}.asked"));
        let _ = std::fs::remove_file(&out);
        let mut helper = tokio::process::Command::new(&launch.install_path);
        helper
            .args(["internal", "catalogue", word])
            .arg("--command")
            .arg(command);
        for arg in args {
            // Joined, so an argument starting with a dash is never read as
            // one of the helper's own options.
            helper.arg(format!("--arg={arg}"));
        }
        helper
            .arg("--out")
            .arg(&out)
            .envs(env)
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
        let deadline = Duration::from_millis(launch.core.start_deadline_ms);
        let output = match tokio::time::timeout(deadline, child.wait_with_output()).await {
            Ok(output) => output.map_err(|error| CatalogueError::Helper(error.to_string()))?,
            Err(_) => {
                // The provider runs in the helper's group: both go.
                if let Some(pid) = pid {
                    crate::runtime::kill_group(pid);
                }
                return Err(CatalogueError::Helper(format!(
                    "asking {word} took longer than {}ms",
                    launch.core.start_deadline_ms
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

impl ProfileRuntime {
    /// The ACP agents this host offers now: the ones it knows whose
    /// command is found.
    pub fn offered_agents(&self) -> Vec<crate::agents::Offered> {
        let launch = self.launch();
        crate::agents::offered(&launch.agents, &launch.provider_env)
    }

    /// What the ACP agent `agent` offers on this host, and whether it is
    /// signed in: the helper's answer to `initialize`, asked again once old
    /// (or once a recheck is due while it says signed out), with what this
    /// host's own conversations with the agent showed where `initialize`
    /// alone does not tell. The helper opens no session, so asking leaves
    /// nothing in the agent's history; before any conversation, an agent
    /// that lists nothing without one offers no models. A failed ask
    /// answers with the copy when there is one.
    pub async fn agent_catalogue(&self, agent: &str) -> Result<HostCatalogue, CatalogueError> {
        let launch = self.launch();
        let program = launch
            .agents
            .iter()
            .find(|program| program.id == agent)
            .ok_or_else(|| CatalogueError::NoAgent(agent.to_owned()))?;
        let path = program
            .find(&launch.provider_env)
            .ok_or_else(|| CatalogueError::NotInstalled(program.name.clone()))?;
        let offers = self.offers();
        let run = offers
            .agent_runs
            .lock()
            .unwrap()
            .entry(agent.to_owned())
            .or_default()
            .clone();
        let _run = run.lock().await;
        let now = self.clock_now();
        let held = {
            let mut held = offers.agents.lock().unwrap();
            if !held.contains_key(agent)
                && let Ok(bytes) = std::fs::read(agent_copy_path(self.dir(), agent))
                && let Ok(asked) = ProviderOffer::decode(bytes.as_slice())
            {
                held.insert(
                    agent.to_owned(),
                    Held {
                        offer: host_catalogue(asked.clone(), &Default::default()),
                        asked_at_ms: asked.asked_at_ms,
                        asked,
                        version_read_at_ms: i64::MIN,
                    },
                );
            }
            held.get(agent).map(|held| {
                let age = now.saturating_sub(held.asked_at_ms);
                let current = age < launch.catalogue_max_age_ms
                    && (held.offer.signed_in || age < launch.catalogue_recheck_ms);
                (held.asked.clone(), current)
            })
        };
        let asked = match held {
            Some((asked, true)) => asked,
            held => {
                let providers = self.dir().join(PROVIDERS);
                let env = program.environment(&launch.provider_env);
                let command = path.to_string_lossy();
                match self
                    .run_helper(agent, &command, &program.args, &env, &launch, &providers)
                    .await
                {
                    Ok(asked) => {
                        let asked = ProviderOffer {
                            asked_at_ms: now,
                            ..asked
                        };
                        let copy = agent_copy_path(self.dir(), agent);
                        let bytes = asked.encode_to_vec();
                        tokio::task::spawn_blocking(move || {
                            crate::install::write_cache(&copy, &bytes)
                        })
                        .await
                        .map_err(|error| CatalogueError::Write(io::Error::other(error)))?
                        .map_err(CatalogueError::Write)?;
                        offers.agents.lock().unwrap().insert(
                            agent.to_owned(),
                            Held {
                                offer: host_catalogue(asked.clone(), &Default::default()),
                                asked: asked.clone(),
                                asked_at_ms: now,
                                version_read_at_ms: now,
                            },
                        );
                        asked
                    }
                    Err(error) => {
                        tracing::warn!(agent, %error, "asking what the agent offers failed");
                        match held {
                            Some((asked, _)) => asked,
                            None => return Err(error),
                        }
                    }
                }
            }
        };
        let learned = self.learned(agent).await?;
        Ok(agent_catalogue(&asked, learned))
    }

    /// What this host's own conversations with the ACP agent `agent` have
    /// shown: each running agent's snapshot names its agent's id and the
    /// catalogue blob it wrote to its own directory, and says whether its
    /// session was refused as signed out.
    pub(crate) async fn learned(&self, agent: &str) -> Result<Learned, CatalogueError> {
        use daemon_core::blobs::{BLOBS, hex};
        let own = self.host().as_bytes().to_vec();
        let wanted = agent.to_owned();
        /// One conversation with the agent, as its newest snapshot says.
        struct Conversation {
            agent: Vec<u8>,
            at_ms: i64,
            catalogue: Option<Vec<u8>>,
            sign_in: wire::SignInState,
        }
        let conversations = self
            .read_rows(move |store| {
                Ok(store
                    .agents()?
                    .into_iter()
                    .filter(|row| row.agent.host == own && row.kind == wire::Kind::Acp as i32)
                    .filter_map(|row| {
                        let snapshot = row.snapshot?;
                        let body = wire::AcpSnapshot::decode(snapshot.body.as_slice()).ok()?;
                        (body.agent.as_deref() == Some(wanted.as_str())).then(|| Conversation {
                            agent: row.agent.agent,
                            at_ms: snapshot.at_ms,
                            catalogue: snapshot.catalogue,
                            sign_in: body
                                .sign_in
                                .map(|sign_in| sign_in.state())
                                .unwrap_or_default(),
                        })
                    })
                    .collect::<Vec<_>>())
            })
            .await?;
        let mut learned = Learned::default();
        let published = conversations
            .iter()
            .filter_map(|conversation| Some((conversation, conversation.catalogue.as_ref()?)))
            .max_by_key(|(conversation, _)| conversation.at_ms);
        if let Some((conversation, hash)) = published
            && let Ok(id) = uuid::Uuid::from_slice(&conversation.agent)
        {
            let path = self.agent_dir(id).join(BLOBS).join(hex(hash));
            learned.catalogue = std::fs::read(path)
                .ok()
                .and_then(|bytes| Catalogue::decode(bytes.as_slice()).ok());
        }
        if let Some(conversation) = conversations
            .iter()
            .filter(|conversation| conversation.sign_in != wire::SignInState::Unspecified)
            .max_by_key(|conversation| conversation.at_ms)
        {
            learned.sign_in = conversation.sign_in;
        }
        Ok(learned)
    }
}

/// How the host's copy says the ACP agent `agent` reopens a conversation:
/// the copy held in memory, else the one on file; None when the host has
/// never asked it.
fn reopen_of(profile_dir: &Path, offers: &Offers, agent: &str) -> Option<wire::AcpReopen> {
    let held = offers
        .agents
        .lock()
        .unwrap()
        .get(agent)
        .map(|held| held.asked.clone());
    let asked = held.or_else(|| {
        let bytes = std::fs::read(agent_copy_path(profile_dir, agent)).ok()?;
        ProviderOffer::decode(bytes.as_slice()).ok()
    })?;
    Some(asked.acp?.reopen())
}

/// Whether a new process can carry on the conversation an ACP agent of
/// `agent`'s program had, as its newest snapshot `body` says it: one with
/// no conversation yet starts its first, and one with a conversation needs
/// a program that resumes or loads a session. A program the host never
/// asked is taken to reopen it, and its agent process ends the agent for
/// good if it cannot, rather than start a fresh conversation in its place.
pub(crate) fn reopens(
    profile_dir: &Path,
    offers: &Offers,
    agent: &str,
    body: Option<&[u8]>,
) -> bool {
    let conversation = body
        .and_then(|body| wire::AcpSnapshot::decode(body).ok())
        .is_some_and(|snapshot| snapshot.session_id.is_some());
    !conversation || reopen_of(profile_dir, offers, agent) != Some(wire::AcpReopen::None)
}

/// This host's live ACP agents whose conversation no new process can
/// reopen, by the host's copy of each program's `initialize` answer: what
/// a start after a restart of the machine exits rather than suspends or
/// continues.
pub(crate) fn unresumable(
    profile_dir: &Path,
    store: &store::Sqlite,
) -> std::collections::BTreeSet<daemon_core::AgentId> {
    let own = store.own_host();
    let offers = Offers {
        held: Mutex::new(HashMap::new()),
        agents: Mutex::new(HashMap::new()),
        agent_runs: Mutex::new(HashMap::new()),
        folders: Mutex::new(HashMap::new()),
        runs: HashMap::new(),
    };
    store
        .agents()
        .unwrap_or_default()
        .into_iter()
        .filter(|row| {
            row.agent.host == own
                && row.kind == wire::Kind::Acp as i32
                && row.lifecycle == wire::Lifecycle::Live as i32
        })
        .filter_map(|row| {
            let body = row.snapshot?.body;
            let agent = wire::AcpSnapshot::decode(body.as_slice()).ok()?.agent?;
            (!reopens(profile_dir, &offers, &agent, Some(&body)))
                .then(|| daemon_core::AgentId::from_slice(&row.agent.agent))
                .flatten()
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use wire::{AcpOffer, AcpReopen, OfferedModel, SignInState};

    use super::*;

    fn catalogue(model: &str) -> Catalogue {
        Catalogue {
            models: vec![OfferedModel {
                value: model.to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn asked(listed: Option<Catalogue>, sign_in: SignInState) -> ProviderOffer {
        ProviderOffer {
            catalogue: listed,
            signed_in: sign_in != SignInState::SignedOut,
            provider_version: "1.0".into(),
            asked_at_ms: 1,
            acp: Some(AcpOffer {
                reopen: AcpReopen::Resume as i32,
                sign_in: sign_in as i32,
                ..Default::default()
            }),
        }
    }

    /// An agent that lists nothing without a session offers what its newest
    /// conversation published; before any, nothing.
    #[test]
    fn an_agents_catalogue_is_learned_from_its_newest_conversation() {
        let helper = asked(Some(Catalogue::default()), SignInState::Unspecified);
        let before = agent_catalogue(&helper, Learned::default());
        assert!(before.catalogue.models.is_empty());
        let learned = Learned {
            catalogue: Some(catalogue("gemini-4")),
            sign_in: SignInState::Unspecified,
        };
        let after = agent_catalogue(&helper, learned);
        assert_eq!(after.catalogue.models[0].value, "gemini-4");
        assert_eq!(after.catalogue.new_agent, None, "the agent's own defaults");
        assert_eq!(after.provider_version, "1.0");
    }

    /// Grok lists its models without a session: what it lists stands over
    /// what a conversation published.
    #[test]
    fn a_catalogue_listed_without_a_session_stands() {
        let helper = asked(Some(catalogue("grok-5")), SignInState::SignedIn);
        let learned = Learned {
            catalogue: Some(catalogue("grok-4")),
            sign_in: SignInState::SignedOut,
        };
        let offer = agent_catalogue(&helper, learned);
        assert_eq!(offer.catalogue.models[0].value, "grok-5");
        assert!(offer.signed_in, "initialize showed it signed in");
        assert_eq!(offer.acp.unwrap().sign_in(), SignInState::SignedIn);
    }

    /// Where `initialize` cannot tell, a conversation refused as signed out
    /// says the agent is, and one that ran says it is signed in.
    #[test]
    fn sign_in_is_learned_where_initialize_cannot_tell() {
        let helper = asked(None, SignInState::Unspecified);
        let out = agent_catalogue(
            &helper,
            Learned {
                catalogue: None,
                sign_in: SignInState::SignedOut,
            },
        );
        assert!(!out.signed_in);
        assert_eq!(out.acp.unwrap().sign_in(), SignInState::SignedOut);
        let unknown = agent_catalogue(&helper, Learned::default());
        assert!(unknown.signed_in, "never called signed out on a guess");
        assert_eq!(unknown.acp.unwrap().sign_in(), SignInState::Unspecified);
    }

    fn offers() -> Offers {
        Offers {
            held: Mutex::new(HashMap::new()),
            agents: Mutex::new(HashMap::new()),
            agent_runs: Mutex::new(HashMap::new()),
            folders: Mutex::new(HashMap::new()),
            runs: HashMap::new(),
        }
    }

    fn with_session(session: Option<&str>) -> Vec<u8> {
        wire::AcpSnapshot {
            agent: Some("mine".into()),
            session_id: session.map(str::to_owned),
            ..Default::default()
        }
        .encode_to_vec()
    }

    /// A conversation is carried on only by a program that resumes or
    /// loads a session, as the host's copy of its `initialize` answer says;
    /// with no conversation yet, or no copy, nothing stops a start.
    #[test]
    fn a_conversation_is_reopened_only_by_a_program_that_can() {
        let dir = tempfile::tempdir().unwrap();
        let offers = offers();
        let session = with_session(Some("s-1"));
        assert!(
            reopens(dir.path(), &offers, "mine", Some(&session)),
            "never asked"
        );
        let copy = |reopen: AcpReopen| {
            let offer = ProviderOffer {
                acp: Some(AcpOffer {
                    reopen: reopen as i32,
                    ..Default::default()
                }),
                ..Default::default()
            };
            std::fs::create_dir_all(dir.path().join(PROVIDERS)).unwrap();
            std::fs::write(agent_copy_path(dir.path(), "mine"), offer.encode_to_vec()).unwrap();
        };
        copy(AcpReopen::None);
        assert!(!reopens(dir.path(), &offers, "mine", Some(&session)));
        assert!(
            reopens(dir.path(), &offers, "mine", Some(&with_session(None))),
            "no conversation to lose"
        );
        assert!(reopens(dir.path(), &offers, "mine", None));
        for reopen in [AcpReopen::Resume, AcpReopen::Load] {
            copy(reopen);
            assert!(
                reopens(dir.path(), &offers, "mine", Some(&session)),
                "{reopen:?}"
            );
        }
    }
}
