//! What each provider offers on this host with no agent running, and
//! whether it is signed in. A provider is named by its id: `claude`,
//! `codex`, or an ACP agent's. The daemon holds no provider code: it starts
//! the hidden helper `amux catalogue <provider>` the way it starts an agent
//! process, and the helper runs the provider just long enough to ask. One
//! copy per provider, kept in `providers/<provider>` under the profile, so
//! it outlives a restart; both Claude kinds share Claude's.
//!
//! A copy is asked again when it is old, when it says signed out and a
//! recheck is due (signing in should show soon), or when the provider's
//! `--version` prints something else than when it was asked. The version
//! is read at most once per recheck interval.
//!
//! Claude and Codex are asked when a client first wants what they offer.
//! An ACP agent is asked when the runtime starts and whenever the agents
//! the config names change ([`ProfileRuntime::refresh_agents`]): asking one
//! is an `initialize` and nothing else, and no client could name an agent
//! a person added before its host lists it.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prost::Message as _;
use sha2::{Digest as _, Sha256};
use store::StoreError;
use wire::agents::{CLAUDE, CODEX};
use wire::{
    AgentCapabilities, Catalogue, ErrorCode, GetCatalogueResponse, ProviderDescriptor,
    ProviderOffer, ProviderOnHost,
};

use crate::runtime::{Launch, ProfileRuntime};

#[derive(Debug, thiserror::Error)]
pub enum CatalogueError {
    /// No built-in agent and no `[agents.<id>]` entry has the id.
    #[error("this device knows no agent {0:?}")]
    NoAgent(String),
    /// The request names no provider id.
    #[error("the request names no provider")]
    NoProvider,
    /// The agent's command is not found where agents are started from.
    #[error("{0} is not installed on this device")]
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
            Self::NoAgent(_) | Self::NoProvider => ErrorCode::InvalidArgument,
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

/// The providers that are not ACP agents, which the host asks with their
/// own commands: what `amux catalogue` takes and their copies are named.
pub const OWN: [&str; 2] = [CLAUDE, CODEX];

fn own_command<'a>(provider: &str, launch: &'a Launch) -> &'a str {
    if provider == CLAUDE {
        &launch.claude_command
    } else {
        &launch.codex_command
    }
}

/// What a person does to sign in to Claude or Codex, in the words amux's
/// clients have always given; an ACP agent's comes from its `initialize`.
fn own_sign_in_hint(provider: &str) -> &'static str {
    if provider == CLAUDE {
        "Run claude and sign in with /login."
    } else {
        "Run codex login."
    }
}

/// What a person does to sign in to an ACP agent: the agent's own words
/// where they name a command to run (OpenCode's "Run `opencode auth login`
/// in the terminal"), else a built-in's hint, else the first sign-in method
/// the agent describes at all ("Log in with your Google account"); empty
/// where there is none of these.
fn agent_sign_in_hint(provider: &str, acp: Option<&wire::AcpOffer>) -> String {
    let described: Vec<&str> = acp
        .into_iter()
        .flat_map(|acp| &acp.sign_in_methods)
        .map(|method| method.description.trim())
        .filter(|description| !description.is_empty())
        .collect();
    described
        .iter()
        .find(|description| description.contains('`'))
        .copied()
        .or_else(|| wire::agents::built_in(provider).map(|agent| agent.sign_in_hint))
        .or_else(|| described.first().copied())
        .unwrap_or_default()
        .to_owned()
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

impl Held {
    fn of(asked: ProviderOffer, offer: HostCatalogue, version_read_at_ms: i64) -> Self {
        Self {
            asked_at_ms: asked.asked_at_ms,
            asked,
            offer,
            version_read_at_ms,
        }
    }
}

/// The copies, and one run at a time per provider.
pub(crate) struct Offers {
    /// Each provider's copy by its id: Claude's and Codex's, and each ACP
    /// agent's, read from its file when first asked for.
    held: Mutex<HashMap<String, Held>>,
    /// One helper run at a time per provider.
    runs: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// What each provider offered when asked from inside a folder an agent
    /// works in, kept in memory only.
    folders: Mutex<HashMap<(String, PathBuf), Held>>,
}

impl Offers {
    /// The copies the profile kept of Claude's and Codex's; an unreadable
    /// one is asked again. An ACP agent's is read when it is first asked
    /// for, which the runtime does as it starts.
    pub(crate) fn load(profile_dir: &Path, launch: &Launch) -> Self {
        let mut held = HashMap::new();
        for provider in OWN {
            let Ok(bytes) = std::fs::read(copy_path(profile_dir, provider)) else {
                continue;
            };
            let Ok(offer) = ProviderOffer::decode(bytes.as_slice()) else {
                continue;
            };
            let catalogue = host_catalogue(offer.clone(), &remembered(provider, launch));
            held.insert(provider.to_owned(), Held::of(offer, catalogue, i64::MIN));
        }
        Self {
            held: Mutex::new(held),
            runs: Mutex::new(HashMap::new()),
            folders: Mutex::new(HashMap::new()),
        }
    }

    /// The lock that keeps to one helper run at a time for `provider`.
    fn run(&self, provider: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.runs
            .lock()
            .unwrap()
            .entry(provider.to_owned())
            .or_default()
            .clone()
    }

    /// Names Claude's and Codex's copies again after the host's remembered
    /// picks changed, so each hash follows what a client would now be told.
    /// An ACP agent's is named again as it is next asked for, which the
    /// runtime does at once ([`ProfileRuntime::refresh_agents`]).
    pub(crate) fn remembered_changed(&self, launch: &Launch) {
        for (provider, held) in self.held.lock().unwrap().iter_mut() {
            if OWN.contains(&provider.as_str()) {
                held.offer = host_catalogue(held.asked.clone(), &remembered(provider, launch));
            }
        }
    }

    /// The copy of what `provider` offers, as last asked.
    pub(crate) fn offered(&self, provider: &str) -> Option<Catalogue> {
        let held = self.held.lock().unwrap();
        held.get(provider).map(|held| held.offer.catalogue.clone())
    }

    /// Per provider, how clients name it and what its agents can do, and
    /// once asked its hash and whether it is signed in: what this host's
    /// entry in the fleet list says. Claude and Codex always, so a client
    /// knows the kind a new Claude agent starts as before it asks; then the
    /// ACP agents the config names that have been asked, in its order. An
    /// agent no longer named is left out.
    pub(crate) fn on_host(&self, launch: &Launch) -> Vec<ProviderOnHost> {
        let held = self.held.lock().unwrap();
        let own = OWN.into_iter().map(|provider| {
            let held = held.get(provider);
            let kind = if provider == CLAUDE {
                launch.claude_kind
            } else {
                wire::Kind::Codex
            };
            ProviderOnHost {
                provider_id: provider.to_owned(),
                catalogue: held.map(|held| held.offer.blob.hash.clone()),
                signed_in: held.is_some_and(|held| held.offer.signed_in),
                descriptor: Some(ProviderDescriptor {
                    name: wire::agents::name_or_id(provider).to_owned(),
                    kind: kind as i32,
                    sign_in_hint: own_sign_in_hint(provider).to_owned(),
                    version: held
                        .map(|held| held.offer.provider_version.clone())
                        .unwrap_or_default(),
                }),
                capabilities: AgentCapabilities::of_kind(kind),
            }
        });
        let agents = launch.agents.iter().filter_map(|program| {
            let held = held.get(&program.id)?;
            Some(ProviderOnHost {
                provider_id: program.id.clone(),
                catalogue: Some(held.offer.blob.hash.clone()),
                signed_in: held.offer.signed_in,
                descriptor: Some(ProviderDescriptor {
                    name: program.name.clone(),
                    kind: wire::Kind::Acp as i32,
                    sign_in_hint: agent_sign_in_hint(&program.id, held.offer.acp.as_ref()),
                    version: held.offer.provider_version.clone(),
                }),
                // A copy an earlier build asked says nothing of them: it
                // reads as able to do nothing beyond what every agent can,
                // until the agent is asked again.
                capabilities: Some(
                    held.asked
                        .capabilities
                        .unwrap_or_else(|| AgentCapabilities::acp(false, false, false)),
                ),
            })
        });
        own.chain(agents).collect()
    }

    /// Whether the host's copy for `provider` says it is signed in; None
    /// before it has one.
    pub(crate) fn signed_in(&self, provider: &str) -> Option<bool> {
        let held = self.held.lock().unwrap();
        held.get(provider).map(|held| held.offer.signed_in)
    }

    /// What the host's entry lists for `provider`, if it lists it.
    fn listed(&self, provider: &str, launch: &Launch) -> Option<ProviderOnHost> {
        self.on_host(launch)
            .into_iter()
            .find(|offered| offered.provider_id == provider)
    }

    /// Forgets an ACP agent's copy, once the host no longer offers it.
    fn forget(&self, agent: &str) -> bool {
        self.held.lock().unwrap().remove(agent).is_some()
    }
}

/// What an ACP agent was asked with, as its copy keeps it: the SHA-256 of
/// its command, its arguments and its environment, each length-prefixed so
/// no two launches read alike.
fn launched(command: &str, args: &[String], env: &HashMap<String, String>) -> Vec<u8> {
    use sha2::{Digest as _, Sha256};
    let mut hash = Sha256::new();
    let mut put = |part: &str| {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    };
    put(command);
    for arg in args {
        put(arg);
    }
    let mut env: Vec<_> = env.iter().collect();
    env.sort();
    for (key, value) in env {
        put(key);
        put(value);
    }
    hash.finalize().to_vec()
}

/// A provider's copy: named by its id. An ACP agent's id is never
/// `claude` or `codex`, which the config refuses.
fn copy_path(profile_dir: &Path, provider: &str) -> PathBuf {
    profile_dir.join(PROVIDERS).join(provider)
}

/// The picks a new agent of `provider` starts with on this host. A host that
/// runs Claude in its own terminal keeps to Claude's own settings.
fn remembered(provider: &str, launch: &Launch) -> settings::NewAgentDefaults {
    if provider == CLAUDE && launch.claude_kind == wire::Kind::ClaudePty {
        return settings::NewAgentDefaults::default();
    }
    launch.new_agent.of(provider)
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
    /// When that conversation said it.
    pub sign_in_at_ms: i64,
}

/// The copy an ACP agent's helper answer and its conversations make
/// together: the helper's catalogue where it lists models without a
/// session (Grok), else the newest conversation's; and the helper's
/// sign-in state where `initialize` shows one, else the newest
/// conversation's; with the host's remembered picks for the agent laid
/// over what a new one starts with.
fn agent_catalogue(
    asked: &ProviderOffer,
    learned: Learned,
    remembered: &settings::NewAgentDefaults,
) -> HostCatalogue {
    let mut offer = asked.clone();
    let listed = offer
        .catalogue
        .as_ref()
        .is_some_and(|catalogue| !catalogue.models.is_empty());
    if !listed && let Some(catalogue) = learned.catalogue {
        offer.catalogue = Some(catalogue);
    }
    let acp = offer.acp.get_or_insert_default();
    // A conversation's word is what the agent did when asked to open a
    // session; it stands over what `initialize` let the helper guess
    // unless the helper asked later.
    if learned.sign_in != wire::SignInState::Unspecified
        && (acp.sign_in() == wire::SignInState::Unspecified
            || learned.sign_in_at_ms >= asked.asked_at_ms)
    {
        acp.set_sign_in(learned.sign_in);
        offer.signed_in = learned.sign_in != wire::SignInState::SignedOut;
    }
    host_catalogue(offer, remembered)
}

impl ProfileRuntime {
    /// What `provider` offers on this host, by its id: Claude's or Codex's
    /// copy, or an ACP agent's ([`Self::agent_catalogue`]).
    pub async fn host_catalogue(&self, provider: &str) -> Result<HostCatalogue, CatalogueError> {
        match provider {
            "" => Err(CatalogueError::NoProvider),
            CLAUDE | CODEX => self.own_catalogue(provider).await,
            agent => self.agent_catalogue(agent).await,
        }
    }

    /// What Claude or Codex offers on this host, from the copy while it
    /// holds, else from the helper. A failed ask answers with the copy when
    /// there is one.
    async fn own_catalogue(&self, provider: &str) -> Result<HostCatalogue, CatalogueError> {
        let offers = self.offers();
        let run = offers.run(provider);
        let _run = run.lock().await;
        let launch = self.launch();
        let command = own_command(provider, &launch).to_owned();
        let now = self.clock_now();
        let held = {
            let held = offers.held.lock().unwrap();
            held.get(provider).map(|held| {
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
            if let Some(held) = offers.held.lock().unwrap().get_mut(provider) {
                held.version_read_at_ms = now;
            }
            return Ok(offer.clone());
        }

        let providers = self.dir().join(PROVIDERS);
        let asked = match self
            .run_helper(
                provider,
                &command,
                &[],
                &launch.provider_env,
                &launch,
                &providers,
            )
            .await
        {
            Ok(asked) => asked,
            Err(error) => {
                tracing::warn!(provider, %error, "asking what the provider offers failed");
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
        self.keep_copy(provider, &offer).await?;
        let answer = host_catalogue(offer.clone(), &remembered(provider, &launch));
        offers
            .held
            .lock()
            .unwrap()
            .insert(provider.to_owned(), Held::of(offer, answer.clone(), now));
        self.tell_own_host();
        Ok(answer)
    }

    /// Writes `offer` as `provider`'s copy under the profile.
    async fn keep_copy(&self, provider: &str, offer: &ProviderOffer) -> Result<(), CatalogueError> {
        let path = copy_path(self.dir(), provider);
        let bytes = offer.encode_to_vec();
        tokio::task::spawn_blocking(move || crate::install::write_cache(&path, &bytes))
            .await
            .map_err(|error| CatalogueError::Write(io::Error::other(error)))?
            .map_err(CatalogueError::Write)
    }

    /// What `provider` offers an agent working in `folder`. Claude lists a
    /// project's own commands and skills only when asked from inside the
    /// project, so the host's copy, asked in the profile's directory, would
    /// miss them. Asked there and kept in memory as long as the host's copy
    /// would be; an ask that fails answers with the host's catalogue.
    pub async fn folder_catalogue(
        &self,
        provider: &'static str,
        folder: &Path,
    ) -> Result<HostCatalogue, CatalogueError> {
        let offers = self.offers();
        let launch = self.launch();
        let now = self.clock_now();
        let key = (provider.to_owned(), folder.to_owned());
        if let Some(held) = offers.folders.lock().unwrap().get(&key) {
            let age = now.saturating_sub(held.asked_at_ms);
            if age < launch.catalogue_max_age_ms
                && (held.offer.signed_in || age < launch.catalogue_recheck_ms)
            {
                return Ok(held.offer.clone());
            }
        }
        let command = own_command(provider, &launch).to_owned();
        let asked = {
            let run = offers.run(provider);
            let _run = run.lock().await;
            self.run_helper(
                provider,
                &command,
                &[],
                &launch.provider_env,
                &launch,
                folder,
            )
            .await
        };
        match asked {
            Ok(asked) => {
                let asked = ProviderOffer {
                    asked_at_ms: now,
                    ..asked
                };
                let answer = host_catalogue(asked.clone(), &Default::default());
                offers
                    .folders
                    .lock()
                    .unwrap()
                    .insert(key, Held::of(asked, answer.clone(), now));
                Ok(answer)
            }
            Err(error) => {
                tracing::warn!(provider, %error, "asking what the provider offers in a folder failed");
                self.host_catalogue(provider).await
            }
        }
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
            // Ended by `running` instead, with the provider it started.
            .kill_on_drop(false);
        #[cfg(unix)]
        helper.process_group(0);
        let child = helper
            .spawn()
            .map_err(|error| CatalogueError::Helper(error.to_string()))?;
        let mut running = Running(child.id());
        let deadline = Duration::from_millis(launch.core.start_deadline_ms);
        let output = match tokio::time::timeout(deadline, child.wait_with_output()).await {
            Ok(output) => {
                running.0 = None;
                output.map_err(|error| CatalogueError::Helper(error.to_string()))?
            }
            Err(_) => {
                drop(running);
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

/// A catalogue helper still running, by its pid: dropped before it exits,
/// as when its ask takes too long or the daemon stops mid-ask, it is
/// killed with the provider it started, which runs in its group (on
/// Windows, below it in its process tree). Killing the helper alone would
/// leave the provider running with whatever the daemon's process handed
/// down to it.
struct Running(Option<u32>);

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(pid) = self.0.take() {
            crate::runtime::kill_group(pid);
        }
    }
}

impl ProfileRuntime {
    /// The ACP agents this host offers now: the ones it knows whose
    /// command is found.
    pub fn offered_agents(&self) -> Vec<crate::agents::Offered> {
        let launch = self.launch();
        crate::agents::offered(&launch.agents, &launch.provider_env)
    }

    /// Asks every ACP agent the config names what it offers, in the
    /// background, so the host's entry lists each one it offers with its
    /// version, sign-in and what it can do; an agent whose command is no
    /// longer found leaves the entry. Run as the runtime starts and whenever
    /// the agents the config names change.
    ///
    /// Each agent is asked on its own task, all at once: asking starts the
    /// agent's program and waits for its `initialize`, which takes seconds
    /// for a real agent, so asked one after another the last agent a config
    /// names would be listed only after every one before it had started.
    /// Each agent's own lock still keeps to one ask of it at a time.
    pub(crate) fn refresh_agents(self: &Arc<Self>) {
        for program in &self.launch().agents {
            let runtime = Arc::downgrade(self);
            let id = program.id.clone();
            tokio::spawn(async move {
                let Some(runtime) = runtime.upgrade() else {
                    return;
                };
                match runtime.agent_catalogue(&id).await {
                    Ok(_) => {}
                    Err(CatalogueError::NotInstalled(_) | CatalogueError::NoAgent(_)) => {
                        if runtime.offers().forget(&id) {
                            runtime.tell_own_host();
                        }
                    }
                    Err(error) => {
                        tracing::warn!(agent = id, %error, "asking an agent what it offers failed")
                    }
                }
            });
        }
    }

    /// What the ACP agent `agent` offers on this host, and whether it is
    /// signed in: the helper's answer to `initialize`, asked again once old
    /// (or once a recheck is due while it says signed out), with what this
    /// host's own conversations with the agent showed where `initialize`
    /// alone does not tell, and the host's remembered picks for the agent
    /// laid over it. The helper opens no session, so asking leaves nothing
    /// in the agent's history; before any conversation, an agent that lists
    /// nothing without one offers no models. A failed ask answers with the
    /// copy when there is one.
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
        let env = program.environment(&launch.provider_env);
        let command = path.to_string_lossy().into_owned();
        let launched = launched(&command, &program.args, &env);
        let offers = self.offers();
        let run = offers.run(agent);
        let _run = run.lock().await;
        let now = self.clock_now();
        // What the host's entry says of the agent now, to tell paired hosts
        // and clients when any of it moves: its copy, name, version,
        // sign-in or capabilities.
        let listed = offers.listed(agent, &launch);
        let held = {
            let mut held = offers.held.lock().unwrap();
            if !held.contains_key(agent)
                && let Ok(bytes) = std::fs::read(copy_path(self.dir(), agent))
                && let Ok(asked) = ProviderOffer::decode(bytes.as_slice())
            {
                let offer = host_catalogue(asked.clone(), &Default::default());
                held.insert(agent.to_owned(), Held::of(asked, offer, i64::MIN));
            }
            // A copy asked of the program as the config named it before is
            // not this program's: it is dropped, and asked again.
            if held
                .get(agent)
                .is_some_and(|held| held.asked.launched != launched)
            {
                held.remove(agent);
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
                match self
                    .run_helper(agent, &command, &program.args, &env, &launch, &providers)
                    .await
                {
                    Ok(asked) => {
                        let asked = ProviderOffer {
                            asked_at_ms: now,
                            launched: launched.clone(),
                            ..asked
                        };
                        self.keep_copy(agent, &asked).await?;
                        asked
                    }
                    Err(error) => {
                        tracing::warn!(agent, %error, "asking what the agent offers failed");
                        match held {
                            Some((asked, _)) => asked,
                            None => {
                                if offers.listed(agent, &launch) != listed {
                                    self.tell_own_host();
                                }
                                return Err(error);
                            }
                        }
                    }
                }
            }
        };
        let learned = self.learned(agent).await?;
        let answer = agent_catalogue(&asked, learned, &remembered(agent, &launch));
        let asked_at = asked.asked_at_ms;
        offers.held.lock().unwrap().insert(
            agent.to_owned(),
            Held {
                asked,
                offer: answer.clone(),
                asked_at_ms: asked_at,
                version_read_at_ms: now,
            },
        );
        // A copy read from the profile as the host starts is newly listed
        // too, so the host's entry is published with it.
        if offers.listed(agent, &launch) != listed {
            self.tell_own_host();
        }
        Ok(answer)
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
            learned.sign_in_at_ms = conversation.at_ms;
        }
        Ok(learned)
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
            capabilities: None,
            launched: Vec::new(),
        }
    }

    /// An agent that lists nothing without a session offers what its newest
    /// conversation published; before any, nothing.
    #[test]
    fn an_agents_catalogue_is_learned_from_its_newest_conversation() {
        let helper = asked(Some(Catalogue::default()), SignInState::Unspecified);
        let before = agent_catalogue(&helper, Learned::default(), &Default::default());
        assert!(before.catalogue.models.is_empty());
        let learned = Learned {
            catalogue: Some(catalogue("gemini-4")),
            sign_in: SignInState::Unspecified,
            sign_in_at_ms: 0,
        };
        let after = agent_catalogue(&helper, learned, &Default::default());
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
            sign_in_at_ms: 0,
        };
        let offer = agent_catalogue(&helper, learned, &Default::default());
        assert_eq!(offer.catalogue.models[0].value, "grok-5");
        assert!(
            offer.signed_in,
            "initialize, asked later, showed it signed in"
        );
        assert_eq!(offer.acp.unwrap().sign_in(), SignInState::SignedIn);
    }

    /// A conversation refused as signed out after the helper last asked is
    /// the newer word, whatever `initialize` guessed.
    #[test]
    fn a_later_conversation_says_the_sign_in() {
        let helper = asked(Some(catalogue("grok-5")), SignInState::SignedIn);
        let learned = Learned {
            catalogue: None,
            sign_in: SignInState::SignedOut,
            sign_in_at_ms: 2,
        };
        let offer = agent_catalogue(&helper, learned, &Default::default());
        assert!(!offer.signed_in);
        assert_eq!(offer.acp.unwrap().sign_in(), SignInState::SignedOut);
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
                sign_in_at_ms: 0,
            },
            &Default::default(),
        );
        assert!(!out.signed_in);
        assert_eq!(out.acp.unwrap().sign_in(), SignInState::SignedOut);
        let unknown = agent_catalogue(&helper, Learned::default(), &Default::default());
        assert!(unknown.signed_in, "never called signed out on a guess");
        assert_eq!(unknown.acp.unwrap().sign_in(), SignInState::Unspecified);
    }

    /// An agent's own words for signing in are its hint where they name a
    /// command; where they only say how (Grok's "Cached token from
    /// ~/.grok/auth.json"), a built-in's hint says what to run, and an
    /// agent a person added keeps its own words.
    #[test]
    fn a_sign_in_hint_says_what_to_run() {
        let offer = |descriptions: &[&str]| AcpOffer {
            sign_in_methods: descriptions
                .iter()
                .map(|description| wire::AcpSignInMethod {
                    id: "m".into(),
                    name: "M".into(),
                    description: (*description).to_owned(),
                })
                .collect(),
            ..Default::default()
        };
        let grok = offer(&["Cached token from ~/.grok/auth.json", "Sign in with Grok"]);
        assert_eq!(agent_sign_in_hint("grok", Some(&grok)), "Run `grok login`.");
        let opencode = offer(&["Run `opencode auth login` in the terminal"]);
        assert_eq!(
            agent_sign_in_hint("opencode", Some(&opencode)),
            "Run `opencode auth login` in the terminal"
        );
        assert_eq!(
            agent_sign_in_hint("antigravity", None),
            "Run agy and sign in when it asks."
        );
        let own = offer(&["", "Log in with your account"]);
        assert_eq!(
            agent_sign_in_hint("my-agent", Some(&own)),
            "Log in with your account"
        );
        assert_eq!(agent_sign_in_hint("my-agent", None), "");
    }
}
