//! The fleet: home's sections, each family placed by its loudest member and
//! ordered by since-when, every row with its second line; cards; and the
//! family header a chat shows.

use std::collections::{HashMap, HashSet};

use attachments::PLACEHOLDER;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
pub use ui_state::ExitCause;
use ui_state::{
    Activity, ActivityKind, AgentKey, Attention, FleetState, ItemBody, ItemClass, SessionState,
};
use wire::{Agent, Kind, Lifecycle, Presence, RouteKind, SignInState, UsageState};

use crate::ask::{AskBody, ask_card};
use crate::text::{markdown_words, within, without_direction_controls};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FleetCard {
    pub agent: AgentKey,
    pub name: String,
    pub kind: Kind,
    pub attention: Attention,
    /// How it ended, once it has.
    pub exit_cause: Option<ExitCause>,
    /// The branch of the folder the agent started in, as of its last turn
    /// end; None on a detached head or outside a repository.
    pub branch: Option<String>,
    pub cwd: String,
    pub phase_since_ms: i64,
    pub host: String,
    pub host_reach: Reach,
    /// Children in the fleet, and how loud the family is.
    pub children: u32,
    pub family_attention: Attention,
    /// The whole family below and including this agent: what a folded
    /// family stands for when a client counts its fleet.
    pub members: u32,
    /// Live agents below this one on every host its host knows of, as its
    /// row reports them: what a stop or delete would cut. It counts children
    /// on hosts this client cannot see, which `members` leaves out.
    pub descendants: u32,
    /// Jobs its provider runs in the background, as its row reports them.
    pub background_jobs: u32,
    /// This client's stop of it is held: its host could not be reached, and
    /// the stop goes when the host is back. Cancel takes it back.
    pub stop_held: bool,
}

/// Home's sections, computed once for every client. Empty sections are
/// left out.
#[derive(Clone, Debug, Default, PartialEq, Serialize, JsonSchema)]
pub struct FleetView {
    pub sections: Vec<FleetSection>,
    /// Agents across the whole fleet that need the person, and that are
    /// working or starting: counted by agent, not by family, whatever is
    /// folded or filtered out of the sections. An agent whose host is out
    /// of reach is not counted: its row says only that.
    pub need_you: u32,
    pub working: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FleetSection {
    pub kind: SectionKind,
    /// How many families the section holds, folded or not.
    pub families: u32,
    /// Whether the section starts folded: exited agents are history,
    /// rarely opened.
    pub folded_by_default: bool,
    /// Each family's head, with the members of expanded families under it.
    pub rows: Vec<FleetRow>,
}

/// Where a family sits: by its loudest member. Live holds the starting,
/// working and idle: alive, whether or not busy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub enum SectionKind {
    NeedsYou,
    Live,
    Exited,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FleetRow {
    pub card: FleetCard,
    /// Zero for a family's head.
    pub depth: u32,
    pub expanded: bool,
    pub second_line: SecondLine,
    /// On a folded family's head that does not itself need the person: the
    /// member that does, which the row speaks for.
    pub loud: Option<LoudMember>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct LoudMember {
    pub agent: AgentKey,
    pub name: String,
    pub second_line: SecondLine,
}

/// What a row says under its name, by the agent's state. What the agent is
/// working on is a fact for agents finding each other, not for this line.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub enum SecondLine {
    /// Needs you: what it asks.
    Ask(AskSummary),
    /// Working: the step it is running, as the chat's activity line has it.
    Step(ActivityLine),
    /// Idle: the first line of what it last said.
    LastSaid(String),
    /// Idle, and cannot go on until the person acts.
    Stuck(StuckReason),
    Exited(ExitCause),
    /// The agent's host is out of reach; the agent is only as it last said.
    HostAway,
    /// The agent's host no longer trusts this machine: it will not come
    /// back on its own, only by pairing again. The agent is only as it last
    /// said.
    HostRevoked,
    /// Starting, or nothing known yet.
    Blank,
}

/// The head ask, small enough for a row: what is asked, without the diff,
/// the plan or the form that the ask card carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AskSummary {
    pub subject: AskSubject,
    /// Asks open, the head included.
    pub count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum AskSubject {
    Command {
        command: String,
    },
    Edit {
        path: String,
        files: u32,
        /// As the ask's card has it: None for a write that may do either.
        created: Option<bool>,
    },
    Tool {
        server: String,
        tool: String,
    },
    /// The first question, and how many there are.
    Question {
        question: String,
        count: u32,
    },
    Plan,
    Form {
        server: String,
        message: String,
    },
    Link {
        server: String,
        message: String,
    },
    Access {
        reason: String,
    },
    Unanswerable {
        reason: String,
    },
}

/// The activity line, with the subject of the step it names when the step
/// is a held row: the command, the file, the query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ActivityLine {
    pub activity: Activity,
    pub step: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum StuckReason {
    /// The provider is signed out, its sign-in expired, or signing in failed.
    SignedOut { state: SignInState, account: String },
    /// A usage window is spent; it resets at the latest reset of the
    /// windows that are, when the provider says.
    UsageLimit { resets_at_ms: Option<i64> },
}

/// What one agent's session knows for its row. Read from the session alone,
/// so a client never holds the fleet and a session at once; [`fleet_view`]
/// picks from it by the state the inventory row says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionLine {
    pub ask: Option<AskSummary>,
    pub step: Option<ActivityLine>,
    pub last_said: Option<String>,
    pub stuck: Option<StuckReason>,
}

pub fn session_line(state: &SessionState, now_ms: i64) -> SessionLine {
    let agent = state.agent_state();
    let stuck = match agent.sign_in.state() {
        SignInState::SignedOut | SignInState::Expired | SignInState::Failed => {
            Some(StuckReason::SignedOut {
                state: agent.sign_in.state(),
                account: agent.sign_in.account.clone(),
            })
        }
        SignInState::Unknown | SignInState::SignedIn => None,
    }
    .or_else(|| {
        (agent.usage.state() == UsageState::Blocked).then(|| StuckReason::UsageLimit {
            resets_at_ms: crate::usage_windows(&agent.usage)
                .iter()
                .filter(|window| window.state == UsageState::Blocked)
                .filter_map(|window| window.resets_at_ms)
                .max(),
        })
    });
    SessionLine {
        ask: ask_card(state).map(|card| AskSummary {
            subject: subject(card.body),
            count: card.count as u32,
        }),
        step: activity(state, now_ms).map(|activity| ActivityLine {
            step: step_subject(state, &activity),
            activity,
        }),
        last_said: last_said(state),
        stuck,
    }
}

fn subject(body: AskBody) -> AskSubject {
    match body {
        AskBody::Command { command, .. } => AskSubject::Command { command },
        AskBody::Edit {
            path,
            files,
            created,
            ..
        } => AskSubject::Edit {
            path,
            files,
            created,
        },
        AskBody::Tool { server, tool, .. } => AskSubject::Tool { server, tool },
        AskBody::Question(questions) => AskSubject::Question {
            count: questions.len() as u32,
            question: questions
                .into_iter()
                .next()
                .map(|first| first.question)
                .unwrap_or_default(),
        },
        AskBody::Plan { .. } => AskSubject::Plan,
        AskBody::Form {
            server, message, ..
        } => AskSubject::Form { server, message },
        AskBody::Link {
            server, message, ..
        } => AskSubject::Link { server, message },
        AskBody::Access { reason, .. } => AskSubject::Access { reason },
        AskBody::Unanswerable { reason } => AskSubject::Unanswerable { reason },
    }
}

/// The activity line, for the chat and the agent's row alike. While
/// nothing more specific is under way, a step still running in this turn
/// is what the agent is doing: Codex writes reasoning and notes while a
/// command runs, and the newest item alone would say only "Working".
pub(crate) fn activity(state: &SessionState, now_ms: i64) -> Option<Activity> {
    let activity = state.activity(now_ms)?;
    if activity.kind != ActivityKind::Working {
        return Some(activity);
    }
    let running = state
        .transcript()
        .iter()
        .rev()
        .take_while(|held| {
            !matches!(
                held.class,
                ItemClass::Prompt
                    | ItemClass::Steer
                    | ItemClass::AgentMessage
                    | ItemClass::Turn
                    | ItemClass::Boundary
            )
        })
        .find(|held| {
            matches!(&held.class, ItemClass::Tool(tool)
                if tool.in_flight && !tool.subagent && !tool.child && !tool.hidden)
        });
    Some(match running {
        Some(held) => Activity {
            kind: ActivityKind::Running {
                key: held.item.key.clone(),
            },
            since_ms: held.item.at_ms,
            elapsed_ms: (now_ms - held.item.at_ms).max(0),
        },
        None => activity,
    })
}

/// The subject of the step a running activity names: its row's, or for a
/// call announced before its row landed, the tool's name. A path in the
/// agent's folder is named from it, as on the step's row.
fn step_subject(state: &SessionState, activity: &Activity) -> Option<String> {
    let ActivityKind::Running { key } = &activity.kind else {
        return None;
    };
    let subject = match state.transcript().get(key) {
        Some(held) => within(&state.agent().cwd, &crate::rows::subject_of(held)),
        None => state
            .agent_state()
            .running_calls
            .iter()
            .find(|call| call.tool_use_id == *key)
            .map(|call| call.tool_name.clone())
            .unwrap_or_default(),
    };
    (!subject.is_empty()).then_some(subject)
}

/// The first line of the newest message the agent wrote to the person, as
/// the words the chat draws it with: its markdown read, not shown.
fn last_said(state: &SessionState) -> Option<String> {
    use wire::claude_pty_item::Kind as Pty;
    use wire::claude_sdk_item::Kind as Sdk;
    use wire::codex_item::Kind as Codex;
    state
        .transcript()
        .iter()
        .rev()
        .filter(|held| {
            matches!(
                held.body,
                ItemBody::ClaudePty(Pty::Message(_))
                    | ItemBody::ClaudeSdk(Sdk::Message(_))
                    | ItemBody::Codex(Codex::Message(_))
            )
        })
        .find_map(|held| {
            // An attachment's placeholder is not words: a reply that opens
            // with one says its first line of prose, or failing that names
            // what it attached.
            held.item
                .text
                .lines()
                .filter(|line| !line.trim_start().starts_with("```"))
                .map(|line| {
                    without_direction_controls(&markdown_words(&line.replace(PLACEHOLDER, "")))
                })
                .map(|line| line.trim().to_owned())
                .find(|line| !line.is_empty())
                .or_else(|| held.item.attachments.iter().find_map(attachment_name))
        })
}

fn attachment_name(attachment: &wire::Attachment) -> Option<String> {
    use wire::attachment::Of;
    let name = match attachment.of.as_ref()? {
        Of::Image(blob) | Of::File(blob) => &blob.name,
        Of::Text(text) => &text.name,
        Of::Review(review) => &review.diff.as_ref()?.patch.as_ref()?.name,
    };
    (!name.is_empty()).then(|| without_direction_controls(name))
}

/// The row's second line: the state comes from the inventory row, so it
/// agrees with the section; the words come from the agent's session. How an
/// agent ended comes before its host's reach: an exited agent's row is
/// history, whatever its host is doing now.
fn second_line(
    fleet: &FleetState,
    local_host: &[u8],
    agent: &Agent,
    line: Option<&SessionLine>,
) -> SecondLine {
    let attention = ui_state::attention(agent);
    if attention == Attention::Exited {
        return SecondLine::Exited(ui_state::exit_cause(agent));
    }
    match reach(fleet, local_host, &agent.host_id) {
        Reach::Online(_) => {}
        Reach::Offline(Offline::Revoked) => return SecondLine::HostRevoked,
        Reach::Away | Reach::Offline(_) => return SecondLine::HostAway,
    }
    let Some(line) = line else {
        return SecondLine::Blank;
    };
    let said = match attention {
        Attention::NeedsYou => line.ask.clone().map(SecondLine::Ask),
        Attention::Working => line.step.clone().map(SecondLine::Step),
        Attention::Idle => line
            .stuck
            .clone()
            .map(SecondLine::Stuck)
            .or_else(|| line.last_said.clone().map(SecondLine::LastSaid)),
        Attention::Starting | Attention::Exited => None,
    };
    said.unwrap_or(SecondLine::Blank)
}

/// An agent's attention as home places and counts it: one whose host is
/// out of reach is only as it last said, as its row says, so it neither
/// works nor waits on the person from here. The header's counts and the
/// sections both read this, so they agree.
fn placed(fleet: &FleetState, local_host: &[u8], agent: &Agent) -> Attention {
    let attention = ui_state::attention(agent);
    if attention > Attention::Idle && !reach(fleet, local_host, &agent.host_id).online() {
        Attention::Idle
    } else {
        attention
    }
}

fn section_of(attention: Attention) -> SectionKind {
    match attention {
        Attention::NeedsYou => SectionKind::NeedsYou,
        Attention::Exited => SectionKind::Exited,
        Attention::Idle | Attention::Starting | Attention::Working => SectionKind::Live,
    }
}

fn card(fleet: &FleetState, local_host: &[u8], agent: &Agent) -> FleetCard {
    let at = ui_state::agent_key(agent);
    let attention = ui_state::attention(agent);
    FleetCard {
        name: without_direction_controls(&agent.name),
        kind: agent.kind(),
        attention,
        exit_cause: (attention == Attention::Exited).then(|| ui_state::exit_cause(agent)),
        branch: agent.git.as_ref().and_then(|git| git.branch.clone()),
        cwd: agent.cwd.clone(),
        phase_since_ms: agent.phase_since_ms,
        host: fleet
            .host(&agent.host_id)
            .map(|host| host.name.clone())
            .unwrap_or_default(),
        host_reach: reach(fleet, local_host, &agent.host_id),
        children: fleet.families().children(&at).count() as u32,
        family_attention: fleet.family_attention(&at).unwrap_or(Attention::Exited),
        members: fleet.family(&at).len() as u32,
        descendants: agent.descendants,
        background_jobs: agent.background_jobs,
        stop_held: fleet.stop_held(&at),
        agent: at,
    }
}

/// Which agents home lists: those whose row says the words, narrowed to
/// one state or one host. The default lists every agent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FleetFilter {
    /// Matched without regard to case against what a row says.
    pub words: String,
    pub only: Option<FleetOnly>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum FleetOnly {
    NeedsYou,
    /// Working or starting.
    Working,
    Host {
        host: Vec<u8>,
    },
}

impl FleetFilter {
    /// Whether `agent` is one this filter lists. A family stays when any of
    /// its members is, so a filter finds folded members too.
    pub fn keeps(&self, fleet: &FleetState, local_host: &[u8], agent: &Agent) -> bool {
        let needle = self.words.trim().to_lowercase();
        let said = needle.is_empty() || matches(fleet, agent, &needle);
        let placed = || placed(fleet, local_host, agent);
        let narrowed = match &self.only {
            None => true,
            Some(FleetOnly::NeedsYou) => placed() == Attention::NeedsYou,
            Some(FleetOnly::Working) => {
                matches!(placed(), Attention::Working | Attention::Starting)
            }
            Some(FleetOnly::Host { host }) => agent.host_id == *host,
        };
        said && narrowed
    }
}

/// Whether a lowercase `needle` is in what the agent's row says: its name,
/// branch, folder, host, how it ended and which agent it is.
pub fn matches(fleet: &FleetState, agent: &Agent, needle: &str) -> bool {
    let host = fleet.host(&agent.host_id).map(|host| host.name.as_str());
    let branch = agent.git.as_ref().and_then(|git| git.branch.as_deref());
    // The cause as the row words it, so the filter finds what it shows.
    // Only an exited row shows one: a live row can briefly hold the cause
    // its journal ended with, until the daemon marks it exited.
    let cause = (agent.lifecycle() == Lifecycle::Exited)
        .then(|| ui_state::exit_words(agent.exit_cause, agent.exit_detail.as_deref()));
    [
        Some(agent.name.as_str()),
        branch,
        Some(agent.cwd.as_str()),
        host,
        cause.as_deref(),
        Some(kind_name(agent.kind())),
    ]
    .into_iter()
    .flatten()
    .any(|field| field.to_lowercase().contains(needle))
}

/// The agent as a row names it: Claude chatted with here, Claude in its
/// own terminal, or Codex.
pub fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::ClaudeSdk => "Claude",
        Kind::ClaudePty => "Claude (terminal)",
        Kind::Codex => "Codex",
        Kind::Unspecified => "",
    }
}

/// Home: every family in the section of its loudest member, newest
/// since-when first, with the members of expanded families (by root agent
/// id in `expand`) under their parents, newest first. A family stays when
/// `keep` holds for any member, so a filter finds folded members too.
/// `lines` holds what each session knows; an agent without one says only
/// what its inventory row does.
///
/// Since-when moves only when an agent's state does, so streaming never
/// reorders rows.
pub fn fleet_view(
    fleet: &FleetState,
    local_host: &[u8],
    lines: &HashMap<AgentKey, SessionLine>,
    expand: &HashSet<Vec<u8>>,
    keep: &dyn Fn(&Agent) -> bool,
) -> FleetView {
    let mut families: Vec<(SectionKind, i64, &Agent)> = fleet
        .roots()
        .filter_map(|root| {
            let members = fleet.family(&ui_state::agent_key(root));
            if !members.iter().any(|member| keep(member)) {
                return None;
            }
            let section = section_of(
                members
                    .iter()
                    .map(|member| placed(fleet, local_host, member))
                    .max()?,
            );
            // Ordered by when the members that place it there last changed.
            let since = members
                .iter()
                .filter(|member| section_of(placed(fleet, local_host, member)) == section)
                .map(|member| member.phase_since_ms)
                .max()?;
            Some((section, since, root))
        })
        .collect();
    families.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| ui_state::agent_key(a.2).cmp(&ui_state::agent_key(b.2)))
    });
    let sections = [
        SectionKind::NeedsYou,
        SectionKind::Live,
        SectionKind::Exited,
    ]
    .into_iter()
    .filter_map(|kind| {
        let heads: Vec<&Agent> = families
            .iter()
            .filter(|family| family.0 == kind)
            .map(|family| family.2)
            .collect();
        if heads.is_empty() {
            return None;
        }
        let mut rows = Vec::new();
        for head in &heads {
            push(fleet, local_host, lines, head, 0, expand, &mut rows);
        }
        Some(FleetSection {
            kind,
            families: heads.len() as u32,
            folded_by_default: kind == SectionKind::Exited,
            rows,
        })
    })
    .collect();
    // An agent whose host is out of reach is only as it last said, as its
    // row says: it is not counted as working or needing the person.
    let counted = |wanted: &[Attention]| {
        fleet
            .agents()
            .filter(|agent| wanted.contains(&placed(fleet, local_host, agent)))
            .count() as u32
    };
    FleetView {
        sections,
        need_you: counted(&[Attention::NeedsYou]),
        working: counted(&[Attention::Working, Attention::Starting]),
    }
}

fn push(
    fleet: &FleetState,
    local_host: &[u8],
    lines: &HashMap<AgentKey, SessionLine>,
    agent: &Agent,
    depth: u32,
    expand: &HashSet<Vec<u8>>,
    rows: &mut Vec<FleetRow>,
) {
    let card_ = card(fleet, local_host, agent);
    let mut children: Vec<&Agent> = fleet
        .families()
        .children(&card_.agent)
        .filter_map(|child| fleet.agent(child))
        .collect();
    children.sort_by(|a, b| {
        b.phase_since_ms
            .cmp(&a.phase_since_ms)
            .then_with(|| ui_state::agent_key(a).cmp(&ui_state::agent_key(b)))
    });
    let expanded = !children.is_empty() && expand.contains(&card_.agent.agent);
    let loud = (!children.is_empty() && !expanded && card_.attention != Attention::NeedsYou)
        .then(|| {
            fleet
                .family(&card_.agent)
                .into_iter()
                .filter(|member| placed(fleet, local_host, member) == Attention::NeedsYou)
                .max_by(|a, b| {
                    a.phase_since_ms
                        .cmp(&b.phase_since_ms)
                        .then_with(|| ui_state::agent_key(b).cmp(&ui_state::agent_key(a)))
                })
                .map(|member| {
                    let key = ui_state::agent_key(member);
                    LoudMember {
                        name: without_direction_controls(&member.name),
                        second_line: second_line(fleet, local_host, member, lines.get(&key)),
                        agent: key,
                    }
                })
        })
        .flatten();
    rows.push(FleetRow {
        second_line: second_line(fleet, local_host, agent, lines.get(&card_.agent)),
        card: card_,
        depth,
        expanded,
        loud,
    });
    if expanded {
        for child in children {
            push(fleet, local_host, lines, child, depth + 1, expand, rows);
        }
    }
}

/// How this machine reaches a host now: one answer every client words, for
/// the host itself and for every agent on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum Reach {
    /// A channel to it opened, over this route; this machine's own host
    /// has none.
    Online(RouteKind),
    /// Seen, and only the relay reaches it, which carries no connection
    /// between hosts on the account's free plan: upgrading reaches it.
    Away,
    /// Out of reach, for a reason this machine can name or for none.
    Offline(Offline),
}

impl Reach {
    pub fn online(self) -> bool {
        matches!(self, Reach::Online(_))
    }
}

/// Why a host is offline from here. A powered-off host and a signed-out
/// machine look the same from here, so the cause is only ever a fact about
/// this machine, never a claim about the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum Offline {
    /// No channel to it opens, or nothing reaches it at all.
    Plain,
    /// This machine is signed out of its account, so the relay carries
    /// nothing for it; a host only the relay reaches is offline until it
    /// signs in again.
    SignedOut,
    /// The host said it no longer trusts this machine, as it closed its
    /// link or refused a stream; only pairing again brings it back.
    Revoked,
}

/// Whether this machine is signed out of the account its profile is bound
/// to. A profile that was never bound is not signed out.
fn signed_out(fleet: &FleetState, local_host: &[u8]) -> bool {
    fleet
        .host(local_host)
        .is_some_and(|entry| entry.account_signed_in == Some(false))
}

/// How this machine reaches `host`. Its own host is always online. What the
/// host itself said comes first: a host that no longer trusts this machine
/// is offline for that reason whatever route still reaches it. A host this
/// machine knows nothing about, or whose presence is unknown, is offline.
pub fn reach(fleet: &FleetState, local_host: &[u8], host: &[u8]) -> Reach {
    let entry = fleet.host(host);
    if host == local_host {
        return Reach::Online(entry.map_or(RouteKind::Unspecified, |entry| entry.via()));
    }
    let Some(entry) = entry else {
        return Reach::Offline(Offline::Plain);
    };
    if entry.revoked == Some(true) {
        return Reach::Offline(Offline::Revoked);
    }
    match entry.presence() {
        Presence::Online => Reach::Online(entry.via()),
        Presence::Away => Reach::Away,
        Presence::Offline | Presence::Unspecified if signed_out(fleet, local_host) => {
            Reach::Offline(Offline::SignedOut)
        }
        Presence::Offline | Presence::Unspecified => Reach::Offline(Offline::Plain),
    }
}

pub fn fleet_card(fleet: &FleetState, local_host: &[u8], agent_id: &[u8]) -> Option<FleetCard> {
    fleet
        .find(agent_id)
        .map(|agent| card(fleet, local_host, agent))
}

/// A chat's family: its parent, its children, how loud the whole family
/// is, and how many of the family other than the chat's own agent need the
/// person, the parent's side and grandchildren included.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FamilyHeader {
    pub parent: Option<FleetCard>,
    pub children: Vec<FleetCard>,
    pub attention: Attention,
    pub need_you: u32,
}

/// None for an agent with no parent and no children.
pub fn family_header(
    fleet: &FleetState,
    local_host: &[u8],
    agent_id: &[u8],
) -> Option<FamilyHeader> {
    let agent = fleet.find(agent_id)?;
    let at = ui_state::agent_key(agent);
    let parent = fleet
        .parent(&at)
        .map(|parent| card(fleet, local_host, parent));
    let children: Vec<FleetCard> = fleet
        .families()
        .children(&at)
        .filter_map(|child| fleet.agent(child))
        .map(|child| card(fleet, local_host, child))
        .collect();
    if parent.is_none() && children.is_empty() {
        return None;
    }
    let root = fleet.root(&at);
    Some(FamilyHeader {
        parent,
        children,
        attention: fleet.family_attention(&root).unwrap_or(Attention::Exited),
        need_you: fleet
            .family(&root)
            .into_iter()
            .filter(|member| {
                ui_state::agent_key(member) != at
                    && ui_state::attention(member) == Attention::NeedsYou
            })
            .count() as u32,
    })
}
