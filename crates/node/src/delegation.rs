//! What an agent may ask for when it starts a subagent, and how the
//! subagent's host fits what it asked onto what its provider offers.
//!
//! Three things a start can choose are kept apart because they differ in
//! kind. Authority (the permission) is capped: a subagent starts at its
//! parent's level, or a narrower one the parent asks for, never a wider
//! one. The mind (model and effort) is cost and capability, not authority,
//! so a parent picks it freely from what the subagent's host offers, and it
//! is never kept as the person's pick for later agents. Isolation (a new
//! workspace) widens nothing. Arguments for the provider itself, and the
//! settings a person picks only in the provider's own terms (a raw
//! permission value or a mode), stay the person's.
//!
//! The subagent's host settles all of it, against what the parent's own
//! host states of the parent on the call (its permission level, read from
//! the parent's own row as the call leaves, never from a copy here that
//! may lag), and its own provider's catalogue, onto which it maps the level
//! and against which it checks the model and effort.

use wire::{AgentSettings, Catalogue, CreateAgentRequest, PermissionLevel};

use crate::runtime::{ProfileRuntime, RegistryError};

/// A refusal in words the calling model can act on.
fn refused(words: impl Into<String>) -> RegistryError {
    RegistryError::Delegation(words.into())
}

fn not_yours(words: impl Into<String>) -> RegistryError {
    RegistryError::NotYours(words.into())
}

/// The level a subagent starts at: the one asked for, or its parent's.
/// Asking for a wider one than the parent's is refused, naming the
/// parent's. A parent whose own level is not known (its permission has
/// none, as an ACP agent a person added, or its row has no settings yet)
/// gets read-only, the narrowest there is, and may ask for nothing wider.
pub(crate) fn ceiling(
    asked: PermissionLevel,
    parent: PermissionLevel,
) -> Result<PermissionLevel, RegistryError> {
    let word = |level: PermissionLevel| level.word().unwrap_or("unknown");
    match (asked, parent) {
        // A parent whose own level is not known could be exceeded by any
        // level but the narrowest.
        (PermissionLevel::Unspecified, PermissionLevel::Unspecified) => {
            Ok(PermissionLevel::ReadOnly)
        }
        (PermissionLevel::Unspecified, parent) => Ok(parent),
        (asked, PermissionLevel::Unspecified) if asked == PermissionLevel::ReadOnly => Ok(asked),
        (asked, PermissionLevel::Unspecified) => Err(not_yours(format!(
            "your own permission has no level amux knows, so a subagent can be asked only for \
             read-only, which is what it gets when none is asked; {} is refused",
            word(asked)
        ))),
        (asked, parent) if asked as i32 > parent as i32 => Err(not_yours(format!(
            "permission {} is wider than yours, {}: a subagent gets at most your own; ask for \
             {} or narrower",
            word(asked),
            word(parent),
            word(parent)
        ))),
        (asked, _) => Ok(asked),
    }
}

/// Refuses what only the person chooses: the provider's own arguments, a
/// permission or mode in the provider's own terms, and keeping the model
/// or effort as the host's pick for later agents.
pub(crate) fn only_picks(request: &CreateAgentRequest) -> Result<(), RegistryError> {
    if request.remember_model || request.remember_effort {
        return Err(not_yours(
            "an agent's picks for a subagent are never kept as the person's",
        ));
    }
    let settings = request.settings();
    if !request.args.is_empty() || settings.permission.is_some() || settings.mode.is_some() {
        return Err(not_yours(
            "a subagent's permission is asked for by level (read-only, ask, auto or full); \
             provider arguments, modes and permission values are the person's to choose",
        ));
    }
    Ok(())
}

/// What a subagent's host made of what its parent asked: the settings it
/// starts on, as the start's answer reports them.
pub(crate) struct Fitted {
    pub(crate) settings: AgentSettings,
}

/// Fits a subagent's model, effort and permission level onto what its
/// provider offers on this host, `catalogue`, writing the permission the
/// level maps to into `settings`. A model or effort the catalogue does not
/// list is refused, listing what it does; one asked of a provider whose
/// catalogue this host has not read is refused too, since nothing can say
/// it exists. Left out, they are the ones this host starts a new agent on,
/// where its catalogue names them. A level maps onto the widest permission at or below it; a
/// provider with no permission that narrow is refused. A provider whose
/// permissions have no level at all (an ACP agent a person added) is
/// refused for every level but full, which its own default cannot exceed.
pub(crate) fn fit(
    provider: &str,
    settings: &mut AgentSettings,
    level: PermissionLevel,
    catalogue: Option<&Catalogue>,
    host: &str,
) -> Result<Fitted, RegistryError> {
    let provider = provider.to_owned();
    let AgentSettings {
        model,
        effort,
        permission,
        permission_level,
        ..
    } = settings;
    *permission_level = 0;
    let listed = |what: &str| {
        refused(format!(
            "{host} has not listed what {provider} offers yet, so its {what} cannot be checked; \
             leave {what} out and it starts on its default"
        ))
    };
    let entry = match model.as_deref() {
        Some(asked) => {
            let catalogue = catalogue.ok_or_else(|| listed("model"))?;
            let found = catalogue
                .models
                .iter()
                .find(|offered| offered.value == asked || offered.resolved_model == asked);
            match found {
                Some(found) => Some(found),
                None => {
                    let names: Vec<&str> = catalogue
                        .models
                        .iter()
                        .map(|offered| offered.value.as_str())
                        .collect();
                    return Err(refused(format!(
                        "{provider} on {host} offers no model {asked:?}; it offers {}",
                        list(&names)
                    )));
                }
            }
        }
        // Left out, the model is the one this host starts a new agent on,
        // as a person's client fills it in: named in the start, since the
        // provider's own default may be another.
        None => {
            let starting = catalogue.and_then(|catalogue| {
                let starting = catalogue.new_agent.as_ref()?.model.as_deref()?;
                catalogue
                    .models
                    .iter()
                    .find(|offered| offered.value == starting || offered.resolved_model == starting)
                    .map(|entry| (starting.to_owned(), entry))
            });
            starting.map(|(starting, entry)| {
                *model = Some(starting);
                entry
            })
        }
    };
    if let Some(asked) = effort.as_deref() {
        let catalogue = catalogue.ok_or_else(|| listed("effort"))?;
        let takes: Vec<&str> = match entry {
            Some(entry) => entry.efforts.iter().map(String::as_str).collect(),
            None => {
                let mut every: Vec<&str> = catalogue
                    .models
                    .iter()
                    .flat_map(|offered| offered.efforts.iter().map(String::as_str))
                    .collect();
                every.sort_unstable();
                every.dedup();
                every
            }
        };
        let whose = entry.map_or_else(|| provider.clone(), |entry| entry.value.clone());
        if takes.is_empty() {
            return Err(refused(format!(
                "{whose} on {host} takes no effort; leave effort out"
            )));
        }
        if !takes.contains(&asked) {
            return Err(refused(format!(
                "{whose} on {host} takes no effort {asked:?}; it takes {}",
                list(&takes)
            )));
        }
    }
    let offered = wire::levels::offered(&provider, catalogue);
    let mapped = match level {
        PermissionLevel::Unspecified => None,
        // A provider whose permissions have no level cannot be held below
        // anything, so only a parent at full, which nothing exceeds, may
        // start one on its own default.
        PermissionLevel::Full if offered.is_empty() => None,
        level if offered.is_empty() => {
            return Err(not_yours(format!(
                "{provider} on {host} has no permission levels amux knows, so it cannot be \
                 held to {}: only an agent with full permission starts it as a subagent",
                level.word().unwrap_or("your level")
            )));
        }
        level => Some(wire::levels::at(&offered, level).ok_or_else(|| {
            let words: Vec<&str> = wire::levels::levels(&offered)
                .into_iter()
                .filter_map(PermissionLevel::word)
                .collect();
            refused(format!(
                "{provider} on {host} offers no permission at or below {}; it offers {}",
                level.word().unwrap_or("that"),
                list(&words)
            ))
        })?),
    };
    *permission = mapped.as_ref().map(|mapped| mapped.value.clone());
    let defaults = catalogue.and_then(|catalogue| catalogue.new_agent.as_ref());
    // Left out, the effort is the new-agent one where the model takes it,
    // named for the same reason; else the provider's default for the model.
    if effort.is_none()
        && let Some(entry) = entry
        && let Some(starting) = defaults.and_then(|defaults| defaults.effort.as_ref())
        && entry.efforts.contains(starting)
    {
        *effort = Some(starting.clone());
    }
    let starting_effort = effort
        .clone()
        .or_else(|| entry.and_then(|entry| entry.default_effort.clone()));
    Ok(Fitted {
        settings: AgentSettings {
            model: model
                .clone()
                .or_else(|| defaults.and_then(|defaults| defaults.model.clone())),
            effort: starting_effort,
            permission: mapped.as_ref().map(|mapped| mapped.value.clone()),
            permission_level: mapped
                .as_ref()
                .map_or(PermissionLevel::Unspecified, |mapped| mapped.level)
                as i32,
            mode: None,
        },
    })
}

/// The model an ACP agent's session takes for `asked`, a model named at
/// its start, and the effort the name carries, if it carries one. An ACP
/// agent drops a model its session does not offer and runs on its own
/// default, saying nothing, so a name is matched onto what the host's copy
/// lists before the agent starts: the value itself; else, ignoring case,
/// a value or display name; else a value or display name with an effort
/// the model takes, as Antigravity's own `agy models` spells its models
/// (`gemini-3.8-flash-low`, "Gemini 3.8 Flash (Low)") where its sessions
/// take the model `gemini-3.8-flash` and the effort `low` apart. Err with
/// the values it takes where no model matches, or more than one does.
pub(crate) fn acp_model(
    catalogue: &Catalogue,
    asked: &str,
) -> Result<(String, Option<String>), Vec<String>> {
    let models = &catalogue.models;
    if let Some(exact) = models.iter().find(|offered| offered.value == asked) {
        return Ok((exact.value.clone(), None));
    }
    let same = |name: &str| !name.is_empty() && name.eq_ignore_ascii_case(asked);
    let mut found: Vec<(String, Option<String>)> = models
        .iter()
        .filter(|offered| same(&offered.value) || same(&offered.display_name))
        .map(|offered| (offered.value.clone(), None))
        .collect();
    if found.is_empty() {
        found = models
            .iter()
            .flat_map(|offered| {
                offered.efforts.iter().filter_map(move |effort| {
                    let spelled = [
                        format!("{}-{effort}", offered.value),
                        format!("{} ({effort})", offered.display_name),
                    ];
                    spelled
                        .iter()
                        .any(|spelled| same(spelled))
                        .then(|| (offered.value.clone(), Some(effort.clone())))
                })
            })
            .collect();
    }
    found.dedup();
    match <[_; 1]>::try_from(found) {
        Ok([one]) => Ok(one),
        Err(_) => Err(models.iter().map(|offered| offered.value.clone()).collect()),
    }
}

impl ProfileRuntime {
    /// Names an ACP agent's model as its session takes it ([`acp_model`]),
    /// for a person's start and an agent's alike, setting the effort the
    /// name carries where none was asked. A model the host's copy does not
    /// list is refused with what it lists; one named with an effort that
    /// another asked effort contradicts is refused too. Where the copy
    /// lists no models (an agent a person added, before a conversation),
    /// the name passes through as given.
    pub(crate) async fn name_acp_model(
        &self,
        kind: wire::Kind,
        request: &mut CreateAgentRequest,
    ) -> Result<(), RegistryError> {
        if kind != wire::Kind::Acp {
            return Ok(());
        }
        let agent = request.provider.clone();
        let acp = request.settings.get_or_insert_with(AgentSettings::default);
        let Some(asked) = acp.model.clone() else {
            return Ok(());
        };
        let catalogue = match self.offers().offered(&agent) {
            Some(held) => Some(held),
            None => self
                .host_catalogue(&agent)
                .await
                .ok()
                .map(|offered| offered.catalogue),
        };
        let Some(catalogue) = catalogue.filter(|catalogue| !catalogue.models.is_empty()) else {
            return Ok(());
        };
        let here = self
            .edge()
            .map_or_else(|| "this device".to_owned(), |edge| edge.host_name());
        let (model, effort) = acp_model(&catalogue, &asked).map_err(|offered| {
            let offered: Vec<&str> = offered.iter().map(String::as_str).collect();
            RegistryError::UnknownModel(format!(
                "{agent} on {here} offers no model {asked:?}; it offers {}",
                list(&offered)
            ))
        })?;
        if let Some(effort) = effort {
            match acp.effort.as_deref() {
                None => acp.effort = Some(effort),
                Some(asked_effort) if asked_effort == effort => {}
                Some(asked_effort) => {
                    return Err(RegistryError::UnknownModel(format!(
                        "{asked:?} is {model} at effort {effort}, but effort {asked_effort} \
                         was asked; name the model {model} with the effort you want"
                    )));
                }
            }
        }
        acp.model = Some(model);
        Ok(())
    }
}

/// Words listed as a person would say them: `a, b or c`, or `nothing`.
fn list(words: &[&str]) -> String {
    match words {
        [] => "nothing".to_owned(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

impl ProfileRuntime {
    /// Settles what an agent, `parent`, asked for its subagent on this
    /// host: only the picks an agent may make; the permission level capped
    /// at `parent_level`, the parent's own as its host stated it on the
    /// call; the model, effort and level fitted to what the provider offers
    /// here; and a fork only of the parent's own conversation, on this
    /// host, by the same provider. Answers the settings the subagent starts
    /// on and the conversation it forks.
    pub(crate) async fn delegate(
        &self,
        request: &mut CreateAgentRequest,
        parent: crate::api::CallingAgent,
        parent_level: PermissionLevel,
    ) -> Result<(Fitted, Option<wire::ForkFrom>), RegistryError> {
        only_picks(request)?;
        let level = ceiling(request.settings().permission_level(), parent_level)?;
        let here = self
            .edge()
            .map_or_else(|| "this host".to_owned(), |edge| edge.host_name());
        let provider = request.provider.clone();
        let settings = request.settings.get_or_insert_with(AgentSettings::default);
        // What the provider offers here, asked of it now when this host
        // holds no copy, but only when the start chose something to check
        // against it.
        let asks = level != PermissionLevel::Unspecified
            || settings.model.is_some()
            || settings.effort.is_some();
        let catalogue = match self.offers().offered(&provider) {
            Some(held) => Some(held),
            None if asks => self
                .host_catalogue(&provider)
                .await
                .ok()
                .map(|offered| offered.catalogue),
            None => None,
        };
        let fitted = fit(&provider, settings, level, catalogue.as_ref(), &here)?;
        let fork = match request.fork {
            true => {
                if parent.host != self.host() {
                    return Err(refused(
                        "fork copies your conversation, which lives on your own host: start \
                         the subagent there, or leave fork out",
                    ));
                }
                let parent_row = self
                    .row_by_id(parent.agent.as_bytes())
                    .await?
                    .ok_or(RegistryError::NotFound(parent.agent))?;
                Some(self.fork_of(&parent_row, &provider)?)
            }
            false => None,
        };
        Ok((fitted, fork))
    }

    /// The conversation of `parent`, one of this host's agents, that a
    /// fork by `provider` copies: the provider session its newest snapshot
    /// names, and the folder it runs in. Refused for another provider's
    /// conversation, by a provider that cannot fork, and before the parent
    /// has a conversation.
    fn fork_of(
        &self,
        parent: &store::AgentRow,
        provider: &str,
    ) -> Result<wire::ForkFrom, RegistryError> {
        let theirs = parent.provider.as_str();
        if provider != theirs {
            return Err(refused(format!(
                "fork copies your conversation, which only {} can read: start a {theirs} \
                 subagent, or leave fork out",
                wire::agents::name_or_id(theirs),
            )));
        }
        let session = parent
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.conversation.clone())
            .filter(|session| !session.is_empty())
            .ok_or_else(|| {
                refused(
                    "your conversation has not started yet, so there is nothing to fork; leave \
                     fork out",
                )
            })?;
        let forks = parent
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.capabilities.as_ref())
            .is_some_and(|capabilities| capabilities.forks);
        if !forks {
            return Err(refused(format!(
                "{} cannot start a conversation from a copy of another; leave fork out",
                wire::agents::name_or_id(provider)
            )));
        }
        Ok(wire::ForkFrom {
            session,
            cwd: parent.cwd.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use wire::{OfferedModel, PermissionLevel as Level};

    use super::*;

    /// An ACP agent's model named as Antigravity's own CLI prints it is the
    /// model and effort its session takes apart; a name in another case is
    /// the value it means; a value itself stands; anything else is refused
    /// with the values the session takes, never passed on to be dropped.
    #[test]
    fn an_acp_model_is_named_as_its_session_takes_it() {
        let model = |value: &str, name: &str, efforts: &[&str]| OfferedModel {
            value: value.into(),
            display_name: name.into(),
            efforts: efforts.iter().map(|effort| (*effort).to_owned()).collect(),
            ..Default::default()
        };
        let efforts = ["low", "medium", "high", "xhigh"];
        let catalogue = Catalogue {
            models: vec![
                model("gemini-3.8-flash", "Gemini 3.8 Flash", &efforts),
                model("gemini-4-neon", "Gemini 4 Neon", &efforts),
                model("gpt-oss-120b-medium", "GPT-OSS 120B (Medium)", &efforts),
            ],
            ..Default::default()
        };
        let named = |asked: &str| acp_model(&catalogue, asked);
        assert_eq!(named("gemini-4-neon"), Ok(("gemini-4-neon".into(), None)));
        assert_eq!(
            named("gemini-3.8-flash-low"),
            Ok(("gemini-3.8-flash".into(), Some("low".into())))
        );
        assert_eq!(
            named("Gemini 4 Neon (XHigh)"),
            Ok(("gemini-4-neon".into(), Some("xhigh".into())))
        );
        assert_eq!(named("GEMINI-4-NEON"), Ok(("gemini-4-neon".into(), None)));
        assert_eq!(
            named("Gemini 3.8 Flash"),
            Ok(("gemini-3.8-flash".into(), None))
        );
        // A value that looks like another's spelling is itself.
        assert_eq!(
            named("gpt-oss-120b-medium"),
            Ok(("gpt-oss-120b-medium".into(), None))
        );
        let refused = named("gemini-9-ultra").unwrap_err();
        assert_eq!(
            refused,
            ["gemini-3.8-flash", "gemini-4-neon", "gpt-oss-120b-medium"]
        );
        // An effort the model does not take is not its spelling.
        assert!(named("gemini-4-neon-max").is_err());
    }

    /// A subagent gets its parent's level, or a narrower one asked for;
    /// a wider one is refused naming the parent's. A parent whose level is
    /// not known gets read-only, and can ask for nothing wider.
    #[test]
    fn the_parents_level_is_the_default_and_the_ceiling() {
        assert_eq!(
            ceiling(Level::Unspecified, Level::Auto).unwrap(),
            Level::Auto
        );
        assert_eq!(
            ceiling(Level::ReadOnly, Level::Auto).unwrap(),
            Level::ReadOnly
        );
        assert_eq!(ceiling(Level::Auto, Level::Auto).unwrap(), Level::Auto);
        let wider = ceiling(Level::Full, Level::Ask).unwrap_err().to_string();
        assert!(wider.contains("full is wider than yours, ask"), "{wider}");
        assert_eq!(
            ceiling(Level::ReadOnly, Level::Unspecified).unwrap(),
            Level::ReadOnly
        );
        let unknown = ceiling(Level::Ask, Level::Unspecified).unwrap_err();
        assert!(matches!(unknown, RegistryError::NotYours(_)), "{unknown}");
        assert_eq!(
            ceiling(Level::Unspecified, Level::Unspecified).unwrap(),
            Level::ReadOnly,
            "an unknown level never leaves the child on its provider's default"
        );
    }

    /// Provider arguments, a raw permission or mode, and keeping a pick are
    /// the person's.
    /// Settings naming a model and effort.
    fn mind(model: Option<&str>, effort: Option<&str>) -> AgentSettings {
        AgentSettings {
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
            ..AgentSettings::default()
        }
    }

    #[test]
    fn only_an_agents_own_picks_pass() {
        let codex =
            |settings: AgentSettings| CreateAgentRequest::of("codex").with_settings(settings);
        assert!(only_picks(&codex(mind(Some("m"), Some("low")))).is_ok());
        assert!(
            only_picks(&codex(AgentSettings {
                permission: Some("full-access".into()),
                ..Default::default()
            }))
            .is_err()
        );
        assert!(
            only_picks(&codex(AgentSettings {
                mode: Some("plan".into()),
                ..Default::default()
            }))
            .is_err()
        );
        let mut remembered = codex(AgentSettings::default());
        remembered.remember_model = true;
        assert!(only_picks(&remembered).is_err());
        let claude = CreateAgentRequest {
            args: vec!["--dangerously-skip-permissions".into()],
            ..CreateAgentRequest::of("claude")
        };
        assert!(only_picks(&claude).is_err());
    }

    fn codex_catalogue() -> Catalogue {
        Catalogue {
            models: vec![
                OfferedModel {
                    value: "gpt-6-luna".into(),
                    efforts: vec!["low".into(), "high".into()],
                    default_effort: Some("low".into()),
                    ..Default::default()
                },
                OfferedModel {
                    value: "gpt-6-sol".into(),
                    efforts: vec!["medium".into()],
                    ..Default::default()
                },
            ],
            permissions: wire::codex::PERMISSIONS
                .iter()
                .map(|named| wire::OfferedPermission {
                    value: named.value.into(),
                    settable: true,
                    normal: named.normal,
                    level: wire::levels::codex(named.value) as i32,
                    ..Default::default()
                })
                .collect(),
            new_agent: Some(mind(Some("gpt-6-sol"), Some("medium"))),
            ..Default::default()
        }
    }

    /// The model and effort are checked against the catalogue, the level
    /// mapped onto the provider's permission, and the settings answered
    /// are what the subagent starts on.
    #[test]
    fn a_subagent_is_fitted_to_what_its_host_offers() {
        let catalogue = codex_catalogue();
        let mut config = mind(Some("gpt-6-luna"), Some("high"));
        let fitted = fit(
            "codex",
            &mut config,
            Level::ReadOnly,
            Some(&catalogue),
            "laptop",
        )
        .unwrap();
        assert_eq!(config.permission.as_deref(), Some("read-only"));
        assert_eq!(
            fitted.settings,
            AgentSettings {
                model: Some("gpt-6-luna".into()),
                effort: Some("high".into()),
                permission: Some("read-only".into()),
                permission_level: Level::ReadOnly as i32,
                mode: None,
            }
        );

        // Left out, the model and effort are the host's new-agent ones,
        // named in the start rather than left to the provider.
        let mut config = AgentSettings::default();
        let fitted = fit(
            "codex",
            &mut config,
            Level::Auto,
            Some(&catalogue),
            "laptop",
        )
        .unwrap();
        assert_eq!(
            (config.model.as_deref(), config.effort.as_deref()),
            (Some("gpt-6-sol"), Some("medium"))
        );
        assert_eq!(fitted.settings.model.as_deref(), Some("gpt-6-sol"));

        let mut unknown = mind(Some("gpt-9"), None);
        let refused = fit(
            "codex",
            &mut unknown,
            Level::Ask,
            Some(&catalogue),
            "laptop",
        )
        .err()
        .unwrap()
        .to_string();
        assert!(
            refused.contains("offers no model \"gpt-9\"; it offers gpt-6-luna or gpt-6-sol"),
            "{refused}"
        );

        // The default model takes only medium.
        let mut effort = mind(None, Some("high"));
        let refused = fit("codex", &mut effort, Level::Ask, Some(&catalogue), "laptop")
            .err()
            .unwrap()
            .to_string();
        assert!(
            refused.contains("gpt-6-sol on laptop takes no effort \"high\""),
            "{refused}"
        );

        // Unchosen, the host's defaults are what the answer reports.
        let mut plain = AgentSettings::default();
        let fitted = fit("codex", &mut plain, Level::Auto, Some(&catalogue), "laptop").unwrap();
        assert_eq!(fitted.settings.model.as_deref(), Some("gpt-6-sol"));
        assert_eq!(fitted.settings.permission.as_deref(), Some("auto"));

        // Without a catalogue a model cannot be checked, and is refused.
        let mut blind = mind(Some("gpt-6-luna"), None);
        assert!(fit("codex", &mut blind, Level::Ask, None, "laptop").is_err());
    }

    /// An agent with nothing as narrow as the level is refused; one whose
    /// permissions have no level is refused below full.
    #[test]
    fn a_level_nothing_offered_fits_is_refused_and_no_levels_only_under_full() {
        let mut config = AgentSettings::default();
        let fitted = fit("opencode", &mut config, Level::Auto, None, "laptop").unwrap();
        assert_eq!(
            config.permission.as_deref(),
            Some("plan"),
            "auto fits only plan"
        );
        assert_eq!(fitted.settings.permission_level(), Level::ReadOnly);

        let claude_catalogue = Catalogue {
            permissions: vec![wire::OfferedPermission {
                value: "bypassPermissions".into(),
                settable: true,
                level: Level::Full as i32,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut claude = AgentSettings::default();
        let refused = fit(
            "claude",
            &mut claude,
            Level::Ask,
            Some(&claude_catalogue),
            "laptop",
        )
        .err()
        .unwrap()
        .to_string();
        assert!(
            refused.contains("no permission at or below ask; it offers full"),
            "{refused}"
        );

        // An agent a person added has no levels: nothing could hold it to
        // any level below full, so only a parent at full starts it, on its
        // own default.
        for level in [Level::ReadOnly, Level::Ask, Level::Auto] {
            let refused = fit(
                "my-agent",
                &mut AgentSettings::default(),
                level,
                None,
                "laptop",
            )
            .err()
            .unwrap();
            assert!(matches!(refused, RegistryError::NotYours(_)), "{level:?}");
            assert!(
                refused
                    .to_string()
                    .contains("my-agent on laptop has no permission levels"),
                "{refused}"
            );
        }
        let mut full = AgentSettings::default();
        let fitted = fit("my-agent", &mut full, Level::Full, None, "laptop").unwrap();
        assert_eq!((full.permission, full.mode), (None, None));
        assert_eq!(fitted.settings.permission_level(), Level::Unspecified);
        // Grok, a built-in whose modes are not permissions, likewise.
        let mut grok = AgentSettings::default();
        assert!(fit("grok", &mut grok, Level::ReadOnly, None, "laptop").is_err());
    }
}
