//! What an agent's working tree changed, and what deleting the agent
//! would take with it, as this host answers them.
//!
//! Both are answered only for this host's own agents: only the owner has
//! the working tree, and only the owner removes a workspace. A copy of
//! another host's agent is not found here, and the refusal names that
//! host, since that is where the call goes.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use git_facts::{Base, GitError};
use highlight::Language;
use store::{AgentKey, AgentRow, StoreError};
use wire::ErrorCode;
use wire::amux::staging::{
    ChangedFile, FileFlag, FileStatus, GetChangesResponse, PreviewDeleteResponse,
    get_changes_request,
};
use wire::experimental::WorkspaceKind;

use crate::runtime::ProfileRuntime;
use crate::{AgentId, HostId, spec};

/// Why a call about one of this host's agents found none.
#[derive(Debug, thiserror::Error)]
pub enum NotHere {
    #[error("no agent with that id")]
    NoAgent,
    /// This host holds a copy of the agent, which another host runs: a
    /// client that sent the call here meant to send it there.
    #[error(
        "that agent runs on {name}, not on this host, which holds only a copy of it; ask {name} (host {host})"
    )]
    Elsewhere { host: HostId, name: String },
    #[error("the store: {0}")]
    Store(#[from] StoreError),
}

impl NotHere {
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NoAgent | Self::Elsewhere { .. } => ErrorCode::NotFound,
            Self::Store(_) => ErrorCode::Internal,
        };
        wire_error(code, self.to_string())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ChangesError {
    #[error(transparent)]
    NotHere(#[from] NotHere),
    #[error("a branch to compare with has a name")]
    NoBranchName,
    #[error(transparent)]
    Git(#[from] GitError),
    #[error("reading the agent's spec: {0}")]
    Spec(io::Error),
    #[error("the store: {0}")]
    Store(#[from] StoreError),
}

impl ChangesError {
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NotHere(not_here) => return not_here.to_wire(),
            Self::NoBranchName => ErrorCode::InvalidArgument,
            // The folder is not a repository, or the branch named shares
            // no history with it: the agent's state, not this host's.
            Self::Git(_) => ErrorCode::FailedPrecondition,
            Self::Spec(_) | Self::Store(_) => ErrorCode::Internal,
        };
        wire_error(code, self.to_string())
    }
}

fn wire_error(code: ErrorCode, message: String) -> wire::Error {
    wire::Error {
        code: code as i32,
        message,
        details: Vec::new(),
    }
}

impl ProfileRuntime {
    /// One of this host's own agents' rows, by its id.
    pub(crate) async fn own_row(&self, agent: AgentId) -> Result<AgentRow, NotHere> {
        let row = self
            .row_by_id(agent.as_bytes())
            .await?
            .ok_or(NotHere::NoAgent)?;
        if row.agent.host == self.host().as_bytes() {
            return Ok(row);
        }
        let host = HostId::from_slice(&row.agent.host).map_err(|_| NotHere::NoAgent)?;
        Err(NotHere::Elsewhere {
            host,
            name: self.host_name(host),
        })
    }

    /// What changed in the working tree of one of this host's agents
    /// against `base`: HEAD, a branch, or with none the default rule, from
    /// the base branch the agent's spec recorded.
    pub async fn changes(
        &self,
        agent: AgentId,
        base: Option<get_changes_request::Base>,
    ) -> Result<GetChangesResponse, ChangesError> {
        let row = self.own_row(agent).await?;
        self.changes_of(&row, base).await
    }

    /// [`ProfileRuntime::changes`] for the row already found.
    pub(crate) async fn changes_of(
        &self,
        row: &AgentRow,
        base: Option<get_changes_request::Base>,
    ) -> Result<GetChangesResponse, ChangesError> {
        let base = match base {
            Some(get_changes_request::Base::Head(_)) => Base::Head,
            Some(get_changes_request::Base::Branch(branch)) if branch.is_empty() => {
                return Err(ChangesError::NoBranchName);
            }
            Some(get_changes_request::Base::Branch(branch)) => Base::Branch(branch),
            None => Base::Default {
                recorded_branch: self.recorded_base_branch(row)?,
            },
        };
        let cwd = PathBuf::from(&row.cwd);
        let changes = git_facts::changes(&cwd, &base).await?;
        let files = tokio::task::spawn_blocking(move || {
            changes
                .files
                .into_iter()
                .map(|file| changed_file(&cwd, file))
                .collect()
        })
        .await
        .expect("naming the files' languages does not panic");
        let used = changes.base;
        Ok(GetChangesResponse {
            // On the base branch itself the comparison ran from where the
            // branch left its upstream, so the upstream is the branch it
            // was made against: the commit is always the merge base with
            // the branch named here, or HEAD when none is.
            base_branch: used.upstream.or(used.branch).unwrap_or_default(),
            base_commit: used.commit.unwrap_or_default(),
            files,
        })
    }

    /// The branch the agent's worktree was made from, as its newest spec
    /// records it: none for an agent that started in a folder of its own.
    fn recorded_base_branch(&self, row: &AgentRow) -> Result<Option<String>, ChangesError> {
        let Ok(id) = AgentId::from_slice(&row.agent.agent) else {
            return Ok(None);
        };
        match spec::newest(&self.agent_dir(id)) {
            Ok(spec) => Ok(spec.and_then(|spec| spec.base_branch)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(ChangesError::Spec(error)),
        }
    }

    /// What deleting one of this host's agents now would take with it.
    /// Changes nothing.
    pub async fn preview_delete(
        &self,
        agent: AgentId,
    ) -> Result<PreviewDeleteResponse, ChangesError> {
        let row = self.own_row(agent).await?;
        self.preview_delete_of(&row).await
    }

    /// [`ProfileRuntime::preview_delete`] for the row already found.
    pub(crate) async fn preview_delete_of(
        &self,
        row: &AgentRow,
    ) -> Result<PreviewDeleteResponse, ChangesError> {
        // Without the experiment this host removes no workspace with its
        // agents, whatever entries rows written while it ran still carry.
        let removes_workspace = self.workspaces_on() && {
            let rows = self.read_rows(|store| store.agents()).await?;
            removes_workspace(&rows, &row.agent)
        };
        let cwd = Path::new(&row.cwd);
        // A folder that is gone has nothing left to lose.
        let facts = match cwd.is_dir() {
            true => git_facts::delete_facts(cwd).await?,
            false => None,
        };
        let (branch, own_commits, uncommitted) = match facts {
            Some(facts) => (facts.branch, facts.own_commits, facts.uncommitted),
            None => (None, None, git_facts::ChangeTotals::default()),
        };
        Ok(PreviewDeleteResponse {
            removes_workspace,
            branch,
            own_commits,
            uncommitted: Some(wire::ChangeTotals {
                files: uncommitted.files,
                added: uncommitted.added,
                removed: uncommitted.removed,
            }),
        })
    }
}

/// Whether deleting `agent` removes its workspace, from the rows this host
/// holds: the workspace goes with its last member when this host made it.
/// The agent's descendants go with it, so a member among them does not
/// keep the workspace; any other agent of this host working in it does.
/// The host checks again when it removes the workspace.
fn removes_workspace(rows: &[AgentRow], agent: &AgentKey) -> bool {
    let Some(entry) = rows
        .iter()
        .find(|row| row.agent == *agent)
        .and_then(daemon_core::workspaces::entry_of)
    else {
        return false;
    };
    if !entry.made_by_amux || entry.kind() == WorkspaceKind::Folder {
        return false;
    }
    let going = family(rows, agent);
    !rows.iter().any(|other| {
        other.agent.host == agent.host
            && !going.contains(&other.agent)
            && daemon_core::workspaces::entry_of(other)
                .is_some_and(|theirs| theirs.kind == entry.kind && theirs.id == entry.id)
    })
}

/// The agent and every descendant among `rows`, on any host: a child on a
/// paired host goes with its parent too, and so does a child it made here.
fn family(rows: &[AgentRow], agent: &AgentKey) -> BTreeSet<AgentKey> {
    let mut children: BTreeMap<&AgentKey, Vec<&AgentKey>> = BTreeMap::new();
    for row in rows {
        if let Some(parent) = &row.parent {
            children.entry(parent).or_default().push(&row.agent);
        }
    }
    let mut going = BTreeSet::new();
    let mut stack = vec![agent];
    while let Some(at) = stack.pop() {
        if going.insert(at.clone()) {
            stack.extend(children.get(at).into_iter().flatten().copied());
        }
    }
    going
}

/// One file as the wire says it, its language picked by the one rule every
/// client colours code by.
fn changed_file(cwd: &Path, file: git_facts::ChangedFile) -> ChangedFile {
    let status = match file.status {
        git_facts::Status::Changed => FileStatus::Changed,
        git_facts::Status::Created => FileStatus::Created,
        git_facts::Status::Deleted => FileStatus::Deleted,
        git_facts::Status::Renamed => FileStatus::Renamed,
    };
    let readable = !file.binary && !file.flags.large && file.status != git_facts::Status::Deleted;
    let language = Language::from_path(&file.path)
        .or_else(|| {
            readable
                .then(|| first_line(&cwd.join(&file.path)))
                .flatten()
                .and_then(|line| Language::from_file(&file.path, &line))
        })
        .map_or_else(String::new, |language| language.name().to_owned());
    let flags = [
        (file.flags.generated, FileFlag::Generated),
        (file.flags.lockfile, FileFlag::Lockfile),
        (file.flags.vendored, FileFlag::Vendored),
        (file.flags.large, FileFlag::Large),
    ]
    .into_iter()
    .filter_map(|(set, flag)| set.then_some(flag as i32))
    .collect();
    ChangedFile {
        path: file.path,
        old_path: file.old_path,
        status: status as i32,
        added: file.added,
        removed: file.removed,
        binary: file.binary,
        language,
        flags,
    }
}

/// How much of a file is read for the `#!` line a script without an
/// extension names its language by.
const SHEBANG_BYTES: u64 = 256;

/// The file's first line, for its `#!`; none when it cannot be read.
fn first_line(path: &Path) -> Option<String> {
    let mut head = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(SHEBANG_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    let text = String::from_utf8_lossy(&head);
    Some(text.lines().next().unwrap_or_default().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(host: u8, agent: u8) -> AgentKey {
        AgentKey::new(vec![host; 16], vec![agent; 16])
    }

    /// A row of `host`'s in a workspace made by `host`, or by hand.
    fn member(agent: AgentKey, parent: Option<AgentKey>, workspace: &str, made: bool) -> AgentRow {
        let mut row = AgentRow::new(agent, wire::Kind::ClaudeSdk, workspace);
        row.parent = parent;
        let entry = wire::experimental::AgentWorkspace {
            kind: WorkspaceKind::Git as i32,
            id: workspace.to_owned(),
            made_by_amux: made,
            ..Default::default()
        };
        wire::experimental::WORKSPACE.put(&mut row.experimental, &entry);
        row
    }

    #[test]
    fn a_workspace_goes_with_its_last_member_and_its_family() {
        let (parent, child, grandchild) = (key(1, 1), key(1, 2), key(2, 3));
        let rows = vec![
            member(parent.clone(), None, "/w/fix", true),
            member(child.clone(), Some(parent.clone()), "/w/fix", true),
            // A child the child made on a paired host, and its own child
            // back here: both go with the family.
            member(grandchild.clone(), Some(child.clone()), "/p/fix", true),
            member(key(1, 4), Some(grandchild), "/w/fix", true),
        ];
        assert!(removes_workspace(&rows, &parent));
        assert!(
            !removes_workspace(&rows, &child),
            "its parent outside the child's family still works there"
        );
    }

    #[test]
    fn a_workspace_another_agent_works_in_stays() {
        let (agent, other) = (key(1, 1), key(1, 2));
        let shared = vec![
            member(agent.clone(), None, "/w/fix", true),
            member(other, None, "/w/fix", true),
        ];
        assert!(!removes_workspace(&shared, &agent));

        // The same folder on another host is another workspace.
        let elsewhere = vec![
            member(agent.clone(), None, "/w/fix", true),
            member(key(2, 2), None, "/w/fix", true),
        ];
        assert!(removes_workspace(&elsewhere, &agent));
    }

    #[test]
    fn a_workspace_this_host_did_not_make_or_a_plain_folder_stays() {
        let agent = key(1, 1);
        assert!(!removes_workspace(
            &[member(agent.clone(), None, "/w/fix", false)],
            &agent
        ));
        let mut folder = member(agent.clone(), None, "/w/fix", true);
        let mut entry = daemon_core::workspaces::entry_of(&folder).unwrap();
        entry.kind = WorkspaceKind::Folder as i32;
        wire::experimental::WORKSPACE.put(&mut folder.experimental, &entry);
        assert!(!removes_workspace(&[folder], &agent));
        let mut bare = AgentRow::new(agent.clone(), wire::Kind::ClaudeSdk, "/w/fix");
        bare.parent = None;
        assert!(
            !removes_workspace(&[bare], &agent),
            "no entry, no workspace"
        );
        assert!(!removes_workspace(&[], &agent));
    }

    #[test]
    fn a_files_language_is_picked_by_its_path_then_its_first_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("deploy"), "#!/usr/bin/env bash\necho\n").unwrap();
        std::fs::write(dir.path().join("notes"), "plain words\n").unwrap();
        let file = |path: &str, status| git_facts::ChangedFile {
            path: path.to_owned(),
            old_path: None,
            status,
            added: 1,
            removed: 0,
            binary: false,
            flags: git_facts::Flags::default(),
        };
        let language = |path: &str, status| changed_file(dir.path(), file(path, status)).language;
        assert_eq!(language("src/main.rs", git_facts::Status::Changed), "rust");
        assert_eq!(language("deploy", git_facts::Status::Created), "bash");
        assert_eq!(language("notes", git_facts::Status::Changed), "");
        assert_eq!(
            language("gone", git_facts::Status::Deleted),
            "",
            "a deleted file is not read"
        );
    }
}
