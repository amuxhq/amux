//! What the provider offers and allows: its usage windows, the catalogue
//! an agent offers and the one the host offers with no agent, and, for
//! Codex, a permission and a mode changed from amux.
//!
//! Judged as the asks are: against the scenario's own traffic, recorded
//! through the probe and replayed through the interpreter.

use prost::Message as _;
use wire::{
    AgentInput, CodexCreateConfig, GetBlobRequest, GetCatalogueRequest, Kind, agent_input,
    inventory_event,
};

use super::asks::settled;
use super::{Install, Log, READY, SETTLE, Scenario, Verdict, expect_outcome, judge};

/// What a catalogue lists, by value, for comparing two of them.
#[derive(Debug, PartialEq)]
struct Offered {
    models: Vec<String>,
    commands: Vec<String>,
    permissions: Vec<String>,
    modes: Vec<String>,
}

impl Offered {
    fn of(catalogue: &wire::Catalogue) -> Offered {
        Offered {
            models: catalogue.models.iter().map(|m| m.value.clone()).collect(),
            commands: catalogue.commands.iter().map(|c| c.name.clone()).collect(),
            permissions: catalogue
                .permissions
                .iter()
                .map(|p| p.value.clone())
                .collect(),
            modes: catalogue.modes.iter().map(|m| m.value.clone()).collect(),
        }
    }
}

/// The windows a session's usage names, in order.
fn windows(kind: Kind, log: &Log) -> Vec<String> {
    match log.state(kind).usage {
        ui_state::Usage::Claude(usage) => usage
            .windows
            .iter()
            .map(|window| match window.limit() {
                wire::Limit::Unspecified => window.provider_name.clone(),
                limit => limit.as_str_name().to_owned(),
            })
            .collect(),
        ui_state::Usage::Codex(usage) => usage
            .windows
            .iter()
            .map(|window| {
                format!(
                    "{} {}m",
                    window.limit().as_str_name(),
                    window.window_minutes
                )
            })
            .collect(),
        ui_state::Usage::Unknown => Vec::new(),
    }
}

impl Install {
    /// Creates the scenario's agent and waits until it takes input, signed
    /// in; None with the reason when it is not.
    pub(super) async fn ready_agent(
        &self,
        scenario: Scenario,
        config: Option<CodexCreateConfig>,
    ) -> Result<Result<(Vec<u8>, super::Follow), Verdict>, String> {
        let agent = match config {
            Some(config) => self.create_codex(scenario, config).await?,
            None => self.create(scenario).await?,
        };
        let follow = self.follow(&agent);
        follow.ready().await?;
        if let Some(why) = follow.signed_out() {
            return Ok(Err(Verdict::Unavailable(why)));
        }
        Ok(Ok((agent, follow)))
    }

    /// One turn, then the provider's usage windows are read and the
    /// interpreter's replay reads the same ones.
    pub(super) async fn usage(&self) -> Result<Verdict, String> {
        let (_agent, follow) = match self.ready_agent(Scenario::Usage, None).await? {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            Scenario::Usage,
            "Reply with the single word pong and nothing else.",
        )
        .await?;
        expect_outcome(follow.turn_end(1).await?, wire::TurnOutcome::Completed)?;
        self.note_model(&follow);
        let kind = self.kind;
        follow
            .until(SETTLE * 4, "the usage windows", |log| {
                !windows(kind, log).is_empty()
            })
            .await?;
        let live = follow.snapshot();
        let captured = self.captured(Scenario::Usage)?;
        if windows(kind, &captured) != windows(kind, &live) {
            return Err(format!(
                "the replay reads windows {:?}, the session {:?}",
                windows(kind, &captured),
                windows(kind, &live)
            ));
        }
        judge(&captured.shape(kind), &live.shape(kind))?;
        Ok(Verdict::Pass)
    }

    /// What a provider offers on a host, with the hash the host names it by.
    async fn host_catalogue_of(
        &self,
        host_id: Vec<u8>,
        provider: wire::Provider,
    ) -> Result<(Vec<u8>, wire::Catalogue), String> {
        let blob = self
            .client
            .agents
            .clone()
            .get_catalogue(GetCatalogueRequest {
                host_id,
                provider: provider as i32,
            })
            .await
            .map_err(|status| format!("the host's catalogue: {status}"))?
            .into_inner();
        let catalogue = wire::Catalogue::decode(blob.bytes.as_slice())
            .map_err(|error| format!("the host's catalogue does not decode: {error}"))?;
        Ok((blob.hash, catalogue))
    }

    /// The catalogue blob an agent's snapshot names, as a client reads it.
    async fn agent_catalogue_of(
        &self,
        agent_id: Vec<u8>,
        hash: Vec<u8>,
    ) -> Result<wire::Catalogue, String> {
        let blob = self
            .client
            .agents
            .clone()
            .get_blob(GetBlobRequest { agent_id, hash })
            .await
            .map_err(|status| format!("the catalogue: {status}"))?
            .into_inner();
        wire::Catalogue::decode(blob.bytes.as_slice())
            .map_err(|error| format!("the catalogue does not decode: {error}"))
    }

    /// The agent says what it offers; the catalogue it names reads back
    /// with the permissions it may be given, and the interpreter's replay
    /// offers the same.
    pub(super) async fn catalogue(&self) -> Result<Verdict, String> {
        let (agent, follow) = match self.ready_agent(Scenario::Catalogue, None).await? {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        let kind = self.kind;
        follow
            .until(SETTLE * 4, "the agent's catalogue", |log| {
                log.state(kind).catalogue.is_some()
            })
            .await?;
        let named = follow
            .snapshot()
            .state(kind)
            .catalogue
            .ok_or("the agent names no catalogue")?;
        let catalogue = self.agent_catalogue_of(agent, named).await?;
        if catalogue.permissions.is_empty() {
            return Err("the catalogue offers no permission".into());
        }
        // Terminal Claude's models are the host's to offer, not the agent's.
        if kind != Kind::ClaudePty && catalogue.models.is_empty() {
            return Err("the catalogue offers no model".into());
        }
        if kind == Kind::Codex && catalogue.modes.is_empty() {
            return Err("Codex's catalogue offers no mode".into());
        }
        let captured = self.captured(Scenario::Catalogue)?;
        // Terminal Claude is handed the host's models and commands (the
        // permissions a running agent may take are its own); the others
        // offer what their provider told the interpreter.
        let (expected, from) = match kind {
            Kind::ClaudePty => {
                let host = self.host_entry().await?;
                let (_, copy) = self
                    .host_catalogue_of(host.host_id().to_vec(), wire::Provider::Claude)
                    .await?;
                (
                    Some(wire::Catalogue {
                        permissions: catalogue.permissions.clone(),
                        modes: catalogue.modes.clone(),
                        ..copy
                    }),
                    "the host",
                )
            }
            _ => (captured.catalogue.clone(), "the replay"),
        };
        if let Some(expected) = expected
            && Offered::of(&expected) != Offered::of(&catalogue)
        {
            return Err(format!(
                "{from} offers {:?}, the agent {:?}",
                Offered::of(&expected),
                Offered::of(&catalogue)
            ));
        }
        judge(&captured.shape(kind), &follow.snapshot().shape(kind))?;
        Ok(Verdict::Pass)
    }

    /// With no agent to ask, the host says what its provider offers and
    /// names that copy, signed in, on its own fleet entry.
    pub(super) async fn host_catalogue(&self) -> Result<Verdict, String> {
        let provider = match self.kind {
            Kind::Codex => wire::Provider::Codex,
            _ => wire::Provider::Claude,
        };
        let host = self.host_entry().await?;
        let (hash, catalogue) = self
            .host_catalogue_of(host.host_id().to_vec(), provider)
            .await?;
        if catalogue.models.is_empty() || catalogue.permissions.is_empty() {
            return Err(format!("the host offers {:?}", Offered::of(&catalogue)));
        }
        let host = self.host_entry().await?;
        let named = host
            .providers
            .iter()
            .find(|on_host| on_host.provider == provider as i32)
            .ok_or_else(|| format!("the host's entry names no {provider:?}"))?;
        if named.catalogue.as_ref() != Some(&hash) {
            return Err("the host's entry names another catalogue".into());
        }
        if !named.signed_in {
            return Ok(Verdict::Unavailable(format!(
                "the host says {provider:?} is not signed in"
            )));
        }
        Ok(Verdict::Pass)
    }

    /// This host's entry in the fleet list, as the inventory opens with it.
    async fn host_entry(&self) -> Result<wire::HostEntry, String> {
        let mut inventory = self
            .client
            .agents
            .clone()
            .subscribe_inventory(wire::Empty {})
            .await
            .map_err(|status| status.to_string())?
            .into_inner();
        let event = tokio::time::timeout(READY, inventory.message())
            .await
            .map_err(|_| "the inventory said nothing".to_owned())?
            .map_err(|status| status.to_string())?
            .ok_or("the inventory ended")?;
        match event.of {
            Some(inventory_event::Of::Host(entry)) => Ok(entry),
            other => Err(format!("the inventory opens with {other:?}, not this host")),
        }
    }

    /// A setting changed from amux, then a turn: the session reads it, and
    /// so does the replay.
    async fn change(
        &self,
        scenario: Scenario,
        of: agent_input::Of,
        read: fn(&ui_state::AgentState) -> Option<String>,
        want: &str,
    ) -> Result<Verdict, String> {
        let (agent, follow) = match self
            .ready_agent(scenario, Some(CodexCreateConfig::default()))
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        let kind = self.kind;
        self.input(&agent, AgentInput { of: Some(of) }).await?;
        self.send(
            scenario,
            "Reply with the single word pong and nothing else. Do not plan anything.",
        )
        .await?;
        // A turn in plan mode may still end with a plan; it is left be.
        self.drive(
            &agent,
            &follow,
            "the turn to end",
            |_| {
                Ok(super::asks::Answer::Given(wire::answer::Of::Plan(
                    wire::PlanAnswer {
                        choice: wire::PlanChoice::KeepPlanning as i32,
                        note: None,
                    },
                )))
            },
            |log| settled(kind, log),
        )
        .await?;
        self.note_model(&follow);
        let live = follow.snapshot();
        let now = read(&live.state(kind));
        if now.as_deref() != Some(want) {
            return Err(format!("the session reads {now:?}, not {want}"));
        }
        let captured = self.captured(scenario)?;
        let replayed = read(&captured.state(kind));
        if replayed != now {
            return Err(format!(
                "the replay reads {replayed:?}, the session {now:?}"
            ));
        }
        judge(&captured.shape(kind), &live.shape(kind))?;
        Ok(Verdict::Pass)
    }

    /// Codex made read-only from amux.
    pub(super) async fn permission(&self) -> Result<Verdict, String> {
        self.change(
            Scenario::Permission,
            agent_input::Of::Permission(wire::SetPermission {
                value: "read-only".into(),
            }),
            |state| state.permission.clone(),
            "read-only",
        )
        .await
    }

    /// Codex put in plan mode from amux.
    pub(super) async fn mode(&self) -> Result<Verdict, String> {
        self.change(
            Scenario::Mode,
            agent_input::Of::Mode(wire::SetMode {
                value: "plan".into(),
            }),
            |state| state.mode.clone(),
            "plan",
        )
        .await
    }
}
