//! The values the phone bridge carries that are not views of one model:
//! what a host asks for and what an act came to. Like the views, they cross
//! as JSON and their Swift mirrors are generated from these definitions.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;

use attachments::PLACEHOLDER;
use model::{AgentKey, Connection, InputState, Key, PhaseView, Refusal, Waiting};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui_view::{
    ChangeTotals, ComposerView, ContextView, ControlsSummary, QueuedRow, Reach, RefusedPrompt,
    SentPrompt, SignInView, ToolRows,
};
use wire::{BlobRef, Kind, RouteKind};

/// What an embedded runtime starts from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StartConfig {
    /// The installation's directory: identity, trust, the store and reports.
    pub data_dir: PathBuf,
    /// What this device calls itself to the machines it pairs with.
    pub device_name: String,
    /// The runtime's own log, which a dump includes.
    #[serde(default)]
    pub log_path: Option<PathBuf>,
    /// Discovery candidates are listed only when their scope equals this.
    #[serde(default)]
    pub discovery_scope: String,
    /// Listen for and dial direct links on the local network.
    #[serde(default = "yes")]
    pub lan: bool,
    /// Where direct links listen, instead of every interface. Only a
    /// driving build reads it, to keep a simulator run on loopback.
    #[serde(default)]
    pub lan_bind: Option<SocketAddr>,
    /// Dial the relay's TCP carrier here in plaintext, instead of where the
    /// account service names it. Only a driving build reads it, to reach a
    /// served test relay.
    #[serde(default)]
    pub relay_tcp: Option<SocketAddr>,
    /// Dial the relay's QUIC carrier here, instead of where the account
    /// service names it, trusting `relay_root`. Only a driving build reads
    /// it, to reach a served test relay the way a phone reaches the cloud.
    #[serde(default)]
    pub relay_quic: Option<SocketAddr>,
    /// The served relay's self-signed certificate, DER as hex.
    #[serde(default)]
    pub relay_root: Option<String>,
    /// How many rows a chat opens with.
    #[serde(default = "default_tail")]
    pub tail: u32,
    /// Whether a published build sends product analytics, as the person
    /// set it in the app's settings.
    #[serde(default = "yes")]
    pub telemetry: bool,
}

fn yes() -> bool {
    true
}

fn default_tail() -> u32 {
    crate::DEFAULT_TAIL
}

/// What changed in a chat since the host last took its changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatChanges {
    /// Rows to fetch again, each once.
    pub keys: Vec<Key>,
    /// Every row id may be new: read the keys again.
    pub reloaded: bool,
    /// Something beside the rows moved: the frame, the overview or the ask.
    pub session: bool,
}

/// Everything around a chat's rows that a screen draws at once.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ChatFrame {
    pub agent: AgentKey,
    pub name: String,
    pub kind: Kind,
    pub phase: PhaseView,
    pub composer: ComposerView,
    pub waiting: Option<Waiting>,
    /// The model, effort in force, and a permission or mode other than the
    /// normal one, as the composer says them.
    pub controls: ControlsSummary,
    /// The agent's branch and change totals as of its last turn end; None
    /// outside a repository.
    pub git: Option<GitView>,
    pub context: Option<ContextView>,
    /// Only a problem; it replaces the composer with a foot card.
    pub sign_in: Option<SignInView>,
    pub connection: Connection,
    /// The rows are current with the agent's host.
    pub caught_up: bool,
    /// Older rows exist below the held window.
    pub has_older: bool,
    /// Rows arrived above the window while the reader is in history: the
    /// new-activity affordance shows from this.
    pub arrivals_held: bool,
    pub queue: Vec<QueuedRow>,
    /// This client's prompts on their way: at the feed's end or in the
    /// queue, sending or perhaps never arrived.
    pub underway: Vec<SentPrompt>,
    /// This client's prompts the agent refused, until the person takes
    /// their words back into the composer.
    pub refused: Vec<RefusedPrompt>,
    /// The input answering the head ask, which a card that was not
    /// confirmed resends or discards.
    pub ask_input: Option<Vec<u8>>,
    /// The runtime no longer serves this chat: the agent is gone.
    pub ended: Option<String>,
}

/// Where an agent works in its repository, and what it has changed there:
/// the header's totals for either comparison.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct GitView {
    /// None on a detached head.
    pub branch: Option<String>,
    /// What the branch is compared against on it; None when not known.
    pub base_branch: Option<String>,
    pub uncommitted: Option<ChangeTotals>,
    pub on_branch: Option<ChangeTotals>,
}

impl GitView {
    pub fn of(git: &wire::Git) -> GitView {
        let totals = |totals: &wire::ChangeTotals| ChangeTotals {
            files: totals.files,
            added: totals.added,
            removed: totals.removed,
        };
        GitView {
            branch: git.branch.clone(),
            base_branch: git.base_branch.clone(),
            uncommitted: git.uncommitted.as_ref().map(totals),
            on_branch: git.on_branch.as_ref().map(totals),
        }
    }
}

/// How rows are asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RowOptions {
    pub tools: ToolRowsOption,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ToolRowsOption {
    #[default]
    ShowAll,
    Hide,
    /// Each run folds to its newest step unless one of its steps is here:
    /// hold its newest, and re-hold the newest as the run grows.
    Collapse {
        open: Vec<Key>,
    },
}

impl RowOptions {
    /// The open runs a folding view borrows, when there are any.
    pub fn open(&self) -> HashSet<Key> {
        match &self.tools {
            ToolRowsOption::Collapse { open } => open.iter().cloned().collect(),
            ToolRowsOption::ShowAll | ToolRowsOption::Hide => HashSet::new(),
        }
    }

    pub fn tool_rows<'a>(&self, open: &'a HashSet<Key>) -> ToolRows<'a> {
        match &self.tools {
            ToolRowsOption::ShowAll => ToolRows::ShowAll,
            ToolRowsOption::Hide => ToolRows::Hide,
            ToolRowsOption::Collapse { .. } => ToolRows::Collapse { open },
        }
    }
}

/// A prompt as the composer holds it: runs of words and attachments in
/// reading order.
///
/// A prompt on the wire is text with one placeholder per attachment, and
/// an attachment whose placeholder is missing never reaches the model. A
/// draft holds its attachments in place among its words instead, so it
/// cannot be built with the two out of step; the placeholders are written
/// only when it is sent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Draft {
    pub parts: Vec<DraftPart>,
}

/// One run of a draft's words, or one attachment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DraftPart {
    Text(String),
    Attachment(DraftAttachment),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DraftAttachment {
    /// Stored with put_blob first.
    Image(BlobRef),
    File(BlobRef),
    Text {
        name: String,
        text: String,
    },
    /// Comments on a frozen diff, sent with the diff that names its patch.
    Review {
        diff: wire::Diff,
        comments: Vec<wire::ReviewComment>,
    },
}

impl DraftAttachment {
    pub fn to_wire(&self) -> wire::Attachment {
        use wire::attachment::Of;
        let of = match self {
            DraftAttachment::Image(blob) => Of::Image(blob.clone()),
            DraftAttachment::File(blob) => Of::File(blob.clone()),
            DraftAttachment::Text { name, text } => Of::Text(wire::InlineText {
                name: name.clone(),
                text: text.clone(),
            }),
            DraftAttachment::Review { diff, comments } => Of::Review(wire::Review {
                diff: Some(diff.clone()),
                comments: comments.clone(),
            }),
        };
        wire::Attachment { of: Some(of) }
    }

    /// The draft's form of an attachment a prompt carried; None for one
    /// with nothing in it.
    pub fn from_wire(attachment: &wire::Attachment) -> Option<DraftAttachment> {
        use wire::attachment::Of;
        Some(match attachment.of.as_ref()? {
            Of::Image(blob) => DraftAttachment::Image(blob.clone()),
            Of::File(blob) => DraftAttachment::File(blob.clone()),
            Of::Text(inline) => DraftAttachment::Text {
                name: inline.name.clone(),
                text: inline.text.clone(),
            },
            Of::Review(review) => DraftAttachment::Review {
                diff: review.diff.clone()?,
                comments: review.comments.clone(),
            },
        })
    }
}

impl Draft {
    /// A draft of words alone.
    pub fn words(text: impl Into<String>) -> Draft {
        Draft {
            parts: vec![DraftPart::Text(text.into())],
        }
    }

    /// The prompt as it is sent: the words with a placeholder at each
    /// attachment's place, and the attachments in that order.
    pub fn to_prompt(&self) -> (String, Vec<wire::Attachment>) {
        let mut text = String::new();
        let mut attachments = Vec::new();
        for part in &self.parts {
            match part {
                DraftPart::Text(words) => text.push_str(words),
                DraftPart::Attachment(attachment) => {
                    text.push(PLACEHOLDER);
                    attachments.push(attachment.to_wire());
                }
            }
        }
        (text, attachments)
    }

    /// A prompt read back into a draft. Each placeholder is the next
    /// attachment, as a row draws it: one with no attachment left is
    /// dropped, and attachments with no placeholder follow the words.
    fn of(text: &str, attachments: &[wire::Attachment]) -> Draft {
        let mut parts = Vec::new();
        let mut attachments = attachments.iter().filter_map(DraftAttachment::from_wire);
        for (index, words) in text.split(PLACEHOLDER).enumerate() {
            if index > 0
                && let Some(attachment) = attachments.next()
            {
                parts.push(DraftPart::Attachment(attachment));
            }
            if !words.is_empty() {
                parts.push(DraftPart::Text(words.to_owned()));
            }
        }
        parts.extend(attachments.map(DraftPart::Attachment));
        Draft { parts }
    }

    /// A queued prompt as a draft: its words and every attachment.
    pub fn from_queued(entry: &wire::QueuedInput) -> Draft {
        Draft::of(&entry.text, &entry.attachments)
    }

    /// A prompt this client sent, as a draft.
    pub fn from_sent(sent: &ui_state::SentInput) -> Option<Draft> {
        match &sent.what {
            ui_state::InputWhat::Prompt { text, attachments } => Some(Draft::of(text, attachments)),
            _ => None,
        }
    }
}

/// An agent's working-tree diff as its host froze it for a review page, and
/// the patch it names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FrozenReview {
    pub diff: wire::Diff,
    pub patch: String,
}

impl FrozenReview {
    /// The page's document, with `comments` placed on their lines.
    pub fn doc(&self, comments: &[wire::ReviewComment]) -> ui_view::ReviewDoc {
        ui_view::review_doc(&self.diff, &self.patch, comments)
    }
}

/// What became of a prompt: its id, and its state as the chat shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SendOutcome {
    pub input_id: Vec<u8>,
    pub state: InputState,
}

/// What an act on a chat came to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ActOutcome {
    Done,
    /// The agent or its host refused it.
    Rejected(Refusal),
    /// The connection dropped before the agent answered.
    NotConfirmed,
    /// The call did not reach the agent.
    Failed(String),
}

/// What asking for older rows came to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum PageOutcome {
    Arrived(u32),
    /// Older history is held only by the agent's host, which cannot be
    /// reached; never an empty page.
    OriginUnreachable,
    Failed(String),
}

/// A host in the fleet as a screen lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct HostView {
    pub host_id: Vec<u8>,
    pub name: String,
    /// This device.
    pub local: bool,
    pub trusted: bool,
    /// Found nearby and not paired.
    pub candidate: bool,
    /// How this device reaches it now, and over which route.
    pub reach: Reach,
    pub platform: Option<String>,
    pub version: Option<String>,
    pub last_dial_error: Option<String>,
    /// Where discovery found it; what pairing dials.
    pub addrs: Vec<String>,
    /// For this device: whether it is signed in to the account its profile
    /// is bound to; None while it was never bound.
    pub signed_in: Option<bool>,
    /// This device's copy of the host's agents has caught up with the host
    /// on a live stream: what the fleet shows for it is what it lists now.
    /// Always true for this device.
    pub current: bool,
    /// The providers the host has said it can run, and whether each is
    /// signed in there.
    pub providers: Vec<ProviderSignIn>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ProviderSignIn {
    pub provider: wire::Provider,
    pub signed_in: bool,
}

/// What changed in the fleet since the host last took its changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FleetChanges {
    /// Agents whose card or family attention may differ.
    pub agents: Vec<AgentKey>,
    /// The host list moved.
    pub hosts: bool,
}

/// Pairing with a machine: the PIN its person reads out, or the link its
/// QR code carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum PairRequest {
    Pin {
        /// Empty when pairing by address alone.
        #[serde(default)]
        host_id: Vec<u8>,
        pin: String,
        /// Where to dial it.
        addrs: Vec<String>,
    },
    Link(String),
}

/// An agent to start on a host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewAgent {
    pub host_id: Vec<u8>,
    pub kind: Kind,
    pub cwd: String,
    pub name: String,
    /// The provider's model, effort, permission and mode, by their values
    /// in the host's catalogue; the host's default for each one absent. Only
    /// Codex takes a mode.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    /// Start it in a worktree of its own, made from the folder's repository.
    #[serde(default)]
    pub new_worktree: bool,
}

/// A directory a host offers to start an agent in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Directory {
    pub path: String,
    pub name: String,
    pub last_used_ms: Option<i64>,
}

/// What a host offers to start an agent in: where agents ran lately and the
/// repositories under its roots.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Directories {
    pub recent: Vec<Directory>,
    pub repositories: Vec<Directory>,
    pub roots: Vec<String>,
}

/// A change to one agent from outside its chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AgentAct {
    Rename(String),
    Stop,
    Delete,
}

/// What only the app sees, which the runtime records for product
/// analytics on a profile's behalf.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum UsageEvent {
    /// The subscription page opened, from one of the app's tabs.
    PaywallViewed { from: PaywallFrom },
    /// A purchase began with the App Store.
    PurchaseStarted { interval: BillingInterval },
}

/// The tab the subscription page was opened from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaywallFrom {
    Agents,
    Hosts,
    You,
}

/// How often a subscription bills.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BillingInterval {
    Monthly,
    Yearly,
}

/// A machine a pairing has reached and authenticated, before this device
/// trusts it: what the person compares and then accepts or turns away.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PendingPair {
    /// Names this attempt to confirm or abandon it.
    pub token: Vec<u8>,
    pub host_id: Vec<u8>,
    pub name: String,
    /// The machine's key, as hex of its SHA-256.
    pub fingerprint: String,
    /// When the machine stops holding this attempt open.
    pub expires_at_ms: i64,
    pub via: RouteKind,
}

/// This device as the machines it pairs with know it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Identity {
    pub host_id: Vec<u8>,
    pub name: String,
    pub fingerprint: String,
}

/// A machine this device trusts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairedPeer {
    pub host_id: Vec<u8>,
    pub name: String,
    pub fingerprint: String,
}

/// This device and the machines it trusts, by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Roster {
    pub identity: Identity,
    pub peers: Vec<PairedPeer>,
}

/// A machine the phone's own browser found on the local network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Found {
    pub host_id: Vec<u8>,
    pub name: String,
    pub version: u32,
    /// Resolved addresses as `ip:port`, IPv6 in brackets.
    pub addrs: Vec<String>,
    pub scope: String,
}

/// One of this device's profiles, one per account it signed in to, and the
/// one nobody has signed in on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProfileView {
    pub id: String,
    pub label: String,
    /// The bound account's subject at its service, kept after a sign-out;
    /// empty for a profile nobody has signed in on.
    pub subject: String,
    pub account: AccountView,
}

/// The account this device's profile is bound to, as the account screens
/// show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccountView {
    pub binding: AccountBinding,
    pub email: String,
    pub name: String,
    /// Whether the account buys the relay, as the relay link last heard.
    pub pro: Option<bool>,
    pub relay: RelayLink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AccountBinding {
    /// Never signed in: this device works on its own network.
    Unbound,
    SignedIn,
    SignedOut,
    /// Signed in, with the relay link turned off.
    Paused,
}

/// The relay link, as far as a person needs to know it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RelayLink {
    Off,
    Connecting,
    Connected,
    Retrying,
    /// The account service wants the person to sign in again.
    SignInAgain,
    /// The account service refuses this build as too old.
    VersionMismatch,
}

/// A bearer for the account service, borrowed from the profile, which alone
/// spends the refresh token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Bearer {
    pub bearer: String,
    pub expires_at_ms: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(name: &str) -> BlobRef {
        BlobRef {
            hash: name.as_bytes().to_vec(),
            name: name.to_owned(),
            mime: "application/octet-stream".to_owned(),
            size: 3,
        }
    }

    /// A prompt's attachments come back into a draft whole, as they were
    /// sent.
    #[test]
    fn every_attachment_comes_back_from_the_wire_as_it_was_sent() {
        let sent = vec![
            DraftAttachment::Image(blob("photo")),
            DraftAttachment::File(blob("notes")),
            DraftAttachment::Text {
                name: "Pasted text".into(),
                text: "one\ntwo\nthree".into(),
            },
            DraftAttachment::Review {
                diff: wire::Diff {
                    head: "abc".into(),
                    patch: Some(blob("patch")),
                    ..Default::default()
                },
                comments: vec![wire::ReviewComment {
                    path: "src/lib.rs".into(),
                    line: 4,
                    old_line: 0,
                    text: "Name this.".into(),
                }],
            },
        ];
        let wire: Vec<wire::Attachment> = sent.iter().map(DraftAttachment::to_wire).collect();
        let back = Draft::of("look", &wire);
        let mut parts = vec![DraftPart::Text("look".into())];
        parts.extend(sent.iter().cloned().map(DraftPart::Attachment));
        assert_eq!(
            back.parts, parts,
            "attachments with no placeholder follow the words"
        );
        assert_eq!(
            DraftAttachment::from_wire(&wire::Attachment { of: None }),
            None
        );
    }
}
