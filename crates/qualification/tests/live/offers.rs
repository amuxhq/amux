//! What the provider offers and allows: its usage windows, the catalogue
//! an agent offers and the one the host offers with no agent, and, for
//! Codex, a permission and a mode changed from amux.
//!
//! Judged as the asks are: against the scenario's own traffic, recorded
//! through the probe and replayed through the interpreter.

use wire::{
    CodexCreateConfig, CodexInput, Empty, GetCatalogueRequest, HostProvider, Kind, codex_input,
    get_catalogue_request, inventory_event,
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
                wire::ClaudeLimit::Unspecified => window.provider_name.clone(),
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
    async fn ready_agent(
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

    async fn catalogue_of(&self, of: get_catalogue_request::Of) -> Result<wire::Catalogue, String> {
        self.client
            .clone()
            .get_catalogue(GetCatalogueRequest { of: Some(of) })
            .await
            .map(tonic::Response::into_inner)
            .map_err(|status| format!("the catalogue: {status}"))
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
        let named = follow.snapshot().state(kind).catalogue;
        let catalogue = self
            .catalogue_of(get_catalogue_request::Of::AgentId(agent))
            .await?;
        if Some(&catalogue.hash) != named.as_ref() {
            return Err("the catalogue read back is not the one the agent names".into());
        }
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
                let copy = self
                    .catalogue_of(get_catalogue_request::Of::Host(HostProvider {
                        host_id: host.host_id,
                        provider: "claude".into(),
                    }))
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
            Kind::Codex => "codex",
            _ => "claude",
        };
        let host = self.host_entry().await?;
        let catalogue = self
            .catalogue_of(get_catalogue_request::Of::Host(HostProvider {
                host_id: host.host_id.clone(),
                provider: provider.to_owned(),
            }))
            .await?;
        if catalogue.models.is_empty() || catalogue.permissions.is_empty() {
            return Err(format!("the host offers {:?}", Offered::of(&catalogue)));
        }
        let host = self.host_entry().await?;
        let named = host
            .providers
            .iter()
            .find(|on_host| on_host.provider == provider)
            .ok_or_else(|| format!("the host's entry names no {provider}"))?;
        if named.catalogue.as_ref() != Some(&catalogue.hash) {
            return Err("the host's entry names another catalogue".into());
        }
        if !named.signed_in {
            return Ok(Verdict::Unavailable(format!(
                "the host says {provider} is not signed in"
            )));
        }
        Ok(Verdict::Pass)
    }

    /// This host's entry in the fleet list, as the inventory opens with it.
    async fn host_entry(&self) -> Result<wire::HostEntry, String> {
        let mut inventory = self
            .client
            .clone()
            .subscribe_inventory(Empty {})
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
        of: codex_input::Of,
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
        self.input(&agent, wire::KindInput::Codex(CodexInput { of: Some(of) }))
            .await?;
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
                Ok(super::asks::Answer::Codex(wire::codex_answer::Of::Plan(
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
            codex_input::Of::Permission(wire::SetPermission {
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
            codex_input::Of::Mode(wire::SetMode {
                value: "plan".into(),
            }),
            |state| state.mode.clone(),
            "plan",
        )
        .await
    }
}
