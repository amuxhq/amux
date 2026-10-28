//! The overview: what is still in flight around a chat. Its task list,
//! the background jobs still running, the tool servers that failed, the
//! usage limits it is near, and the changed files by folder. The
//! composer's edge reads it folded; the overview reads it whole.

use schemars::JsonSchema;
use serde::Serialize;
use ui_state::{Key, SessionState, Usage};
use wire::{
    BackgroundKind, Diff, DiffBase, DiffFileChange, Kind, Limit, TaskListStatus, UsageMeter,
    UsageState, diff_base,
};

use crate::review::FileStatus;

/// Everything the overview lists; each part is empty or None when there is
/// nothing to show.
#[derive(Clone, Debug, Default, PartialEq, Serialize, JsonSchema)]
pub struct Overview {
    pub tasks: Option<TasksView>,
    /// Running background jobs, in the agent's order.
    pub jobs: Vec<JobRow>,
    /// Only tool servers that failed or need signing in.
    pub failed_servers: Vec<ServerView>,
    /// Only near or at a limit.
    pub usage_near_limit: Option<UsageView>,
    /// None until the changed files are fetched.
    pub changes: Option<Changes>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TasksView {
    pub done: u32,
    pub total: u32,
    /// The task in progress, in its active form.
    pub current: String,
    /// Every task in the agent's order.
    pub entries: Vec<TaskLine>,
}

/// One task of the list, by its subject.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TaskLine {
    pub subject: String,
    pub mark: TaskMark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum TaskMark {
    Done,
    Current,
    Todo,
}

/// A background job the agent started that is still running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct JobRow {
    /// The step that started it, when the provider said which.
    pub step: Option<Key>,
    pub command: String,
    pub started_at_ms: i64,
    /// What the job is, so a client can tell a command from the provider's
    /// own subagent or monitor, whose `command` is a description.
    pub kind: BackgroundKind,
    /// Whether the person can stop this one job, and how that stands.
    pub stop: JobStop,
}

/// Stopping one background job, which leaves the agent running. Only a kind
/// whose provider stops one job on request offers it: terminal Claude
/// cannot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum JobStop {
    /// This job cannot be stopped on its own.
    Unavailable,
    /// The person can stop it; the key names it in the stop input.
    Offered { key: String },
    /// A stop was sent and the provider has not yet said the job ended.
    Stopping,
    /// The provider refused the last stop, in its words; the person can
    /// try again.
    Refused { key: String, error: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ServerView {
    pub name: String,
    pub error: String,
    pub needs_auth: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct UsageView {
    pub blocked: bool,
    pub windows: Vec<UsageWindowView>,
    pub credits: Option<String>,
}

/// One rate-limit window: which limit it is, how much of it is used (none
/// when the provider has not said), when it resets, and its own state.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct UsageWindowView {
    pub label: UsageLabel,
    pub used_percent: Option<f64>,
    pub resets_at_ms: Option<i64>,
    pub state: UsageState,
}

/// Which limit a usage window is, for a client to word.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum UsageLabel {
    FiveHour,
    /// The weekly limit, or one model's weekly limit.
    Weekly {
        model: Option<String>,
    },
    /// A window known only by its length, as Codex gives it.
    Minutes(u32),
    /// A window known only by the provider's own name for it.
    Named(String),
}

/// The changed files of one comparison, by folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Changes {
    pub totals: ChangeTotals,
    /// Files at the root first, under the folder "", then each folder by
    /// path.
    pub folders: Vec<Folder>,
}

pub use wire::ChangeTotals;

/// One folder's changed files, sorted by name. Shortening a long path is
/// the client's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Folder {
    /// With its trailing slash; empty for the root.
    pub path: String,
    pub files: Vec<ChangedFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ChangedFile {
    /// From the repository's root.
    pub path: String,
    /// Inside its folder.
    pub name: String,
    /// Lines; none for a binary file.
    pub added: u32,
    pub removed: u32,
    pub status: FileStatus,
    pub binary: bool,
}

/// What the changed files are counted against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize, JsonSchema)]
pub enum Comparison {
    /// The working tree against its last commit.
    #[default]
    Uncommitted,
    /// Everything since the branch left its base, uncommitted work included.
    OnBranch,
}

/// The Diff base for a comparison; None on the branch when the agent's row
/// names no base branch.
pub fn diff_base(state: &SessionState, comparison: Comparison) -> Option<DiffBase> {
    let base = match comparison {
        Comparison::Uncommitted => diff_base::Base::Head(wire::Empty {}),
        Comparison::OnBranch => diff_base::Base::Branch(
            state
                .agent()
                .git
                .as_ref()
                .and_then(|git| git.base_branch.clone())?,
        ),
    };
    Some(DiffBase { base: Some(base) })
}

/// The overview of a session, with the changed files of `diff` when they
/// were fetched.
pub fn overview(state: &SessionState, diff: Option<&Diff>) -> Overview {
    let agent = state.agent_state();
    let tasks = agent.tasks.known.then(|| {
        let total = agent.tasks.entries.len() as u32;
        let done = agent
            .tasks
            .entries
            .iter()
            .filter(|task| task.status() == TaskListStatus::Completed)
            .count() as u32;
        let current = agent
            .tasks
            .entries
            .iter()
            .find(|task| task.status() == TaskListStatus::InProgress)
            .map(|task| {
                if task.active_form.is_empty() {
                    task.subject.clone()
                } else {
                    task.active_form.clone()
                }
            })
            .unwrap_or_default();
        let entries = agent
            .tasks
            .entries
            .iter()
            .map(|task| TaskLine {
                subject: task.subject.clone(),
                mark: match task.status() {
                    TaskListStatus::Completed => TaskMark::Done,
                    TaskListStatus::InProgress => TaskMark::Current,
                    _ => TaskMark::Todo,
                },
            })
            .collect();
        TasksView {
            done,
            total,
            current,
            entries,
        }
    });
    // Headless Claude and Codex stop one job on request; terminal Claude
    // has no way to.
    let stops_one = matches!(state.kind(), Kind::ClaudeSdk | Kind::Codex);
    let jobs = if agent.background.known {
        agent
            .background
            .jobs
            .iter()
            .map(|job| JobRow {
                step: (!job.item_key.is_empty()).then(|| job.item_key.clone()),
                command: job.command.clone(),
                started_at_ms: job.started_at_ms,
                kind: job.kind(),
                stop: match () {
                    () if !stops_one || job.key.is_empty() => JobStop::Unavailable,
                    () if job.stopping => JobStop::Stopping,
                    () if !job.stop_error.is_empty() => JobStop::Refused {
                        key: job.key.clone(),
                        error: job.stop_error.clone(),
                    },
                    () => JobStop::Offered {
                        key: job.key.clone(),
                    },
                },
            })
            .collect()
    } else {
        Vec::new()
    };
    let failed_servers = agent
        .servers
        .servers
        .iter()
        .filter(|server| {
            matches!(
                server.status(),
                wire::ToolServerStatus::Failed | wire::ToolServerStatus::NeedsAuth
            )
        })
        .map(|server| ServerView {
            name: server.name.clone(),
            error: server.error.clone(),
            needs_auth: server.status() == wire::ToolServerStatus::NeedsAuth,
        })
        .collect();
    let usage_near_limit = match agent.usage.state() {
        UsageState::NearLimit | UsageState::Blocked => Some(UsageView {
            blocked: agent.usage.state() == UsageState::Blocked,
            windows: usage_windows(&agent.usage),
            credits: match &agent.usage {
                Usage::Codex(usage) => usage.credits.clone(),
                Usage::Claude(_) | Usage::Unknown => None,
            },
        }),
        UsageState::Unknown | UsageState::Ok => None,
    };
    Overview {
        tasks: tasks.filter(|tasks| tasks.total > 0),
        jobs,
        failed_servers,
        usage_near_limit,
        changes: diff.map(changes),
    }
}

/// A Diff's files by folder: files at the root first, then each folder by
/// path, each folder's files by name.
pub fn changes(diff: &Diff) -> Changes {
    let mut files: Vec<ChangedFile> = diff
        .files
        .iter()
        .map(|file| {
            let cut = file.path.rfind('/').map_or(0, |at| at + 1);
            ChangedFile {
                path: file.path.clone(),
                name: file.path[cut..].to_owned(),
                added: file.added,
                removed: file.removed,
                status: match file.change() {
                    DiffFileChange::Changed | DiffFileChange::Unspecified => FileStatus::Modified,
                    DiffFileChange::Created => FileStatus::Added,
                    DiffFileChange::Deleted => FileStatus::Deleted,
                },
                binary: file.binary,
            }
        })
        .collect();
    let folder = |file: &ChangedFile| file.path[..file.path.len() - file.name.len()].to_owned();
    files.sort_by(|a, b| {
        let (a_dir, b_dir) = (folder(a), folder(b));
        (!a_dir.is_empty(), a_dir, &a.name).cmp(&(!b_dir.is_empty(), b_dir, &b.name))
    });
    let totals = ChangeTotals {
        files: files.len() as u32,
        added: files.iter().map(|file| file.added).sum(),
        removed: files.iter().map(|file| file.removed).sum(),
    };
    let mut folders: Vec<Folder> = Vec::new();
    for file in files {
        let path = folder(&file);
        match folders.last_mut() {
            Some(last) if last.path == path => last.files.push(file),
            _ => folders.push(Folder {
                path,
                files: vec![file],
            }),
        }
    }
    Changes { totals, folders }
}

/// Every usage window the provider reported, in its order.
pub fn usage_windows(usage: &Usage) -> Vec<UsageWindowView> {
    let view = |label, meter: Option<UsageMeter>| {
        let meter = meter.unwrap_or_default();
        UsageWindowView {
            label,
            used_percent: (!meter.used_unknown).then_some(meter.used_percent),
            resets_at_ms: meter.resets_at_ms,
            state: meter.state(),
        }
    };
    match usage {
        Usage::Unknown => Vec::new(),
        Usage::Claude(usage) => usage
            .windows
            .iter()
            .map(|window| {
                let label = window_label(window.limit(), window.model.clone(), || {
                    UsageLabel::Named(window.provider_name.clone())
                });
                view(label, window.meter)
            })
            .collect(),
        Usage::Codex(usage) => usage
            .windows
            .iter()
            .map(|window| {
                let label = window_label(window.limit(), None, || {
                    UsageLabel::Minutes(window.window_minutes)
                });
                view(label, window.meter)
            })
            .collect(),
    }
}

/// A window's label from its length. `model` is the one model a weekly
/// window belongs to, when it belongs to one; `unnamed` labels a length
/// this build has no word for, from what the provider said of it.
fn window_label(
    limit: Limit,
    model: Option<String>,
    unnamed: impl FnOnce() -> UsageLabel,
) -> UsageLabel {
    match limit {
        Limit::FiveHour => UsageLabel::FiveHour,
        Limit::Weekly => UsageLabel::Weekly { model },
        Limit::Unspecified => unnamed(),
    }
}
