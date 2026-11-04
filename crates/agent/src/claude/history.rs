//! Finding the transcript file Claude Code keeps for a session.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve a session transcript under the current project, a sibling Git
/// worktree, or an unambiguous project-directory fallback.
fn find_session_file(config_root: &Path, working_dir: &Path, session_id: &str) -> Option<PathBuf> {
    if !valid_session_id(session_id) {
        return None;
    }
    let worktrees = worktrees(working_dir)?;
    let projects = config_root.join("projects");
    if let Some(path) = worktrees.into_iter().find_map(|worktree| {
        let path = projects
            .join(project_slug(&worktree))
            .join(format!("{session_id}.jsonl"));
        path.is_file().then_some(path)
    }) {
        return Some(path);
    }

    let mut found = None;
    for entry in std::fs::read_dir(projects).ok()?.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let path = entry.path().join(format!("{session_id}.jsonl"));
        if path.is_file() {
            if found.is_some() {
                return None;
            }
            found = Some(path);
        }
    }
    found
}

/// The working folder and its sibling git worktrees, whose projects
/// `--resume` searches first.
fn worktrees(working_dir: &Path) -> Option<Vec<PathBuf>> {
    let mut worktrees = vec![absolute_path(working_dir).ok()?];
    if let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(working_dir)
        .args(["worktree", "list", "--porcelain"])
        .output()
        && output.status.success()
    {
        worktrees.extend(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.strip_prefix("worktree "))
                .map(PathBuf::from),
        );
    }
    worktrees.sort();
    worktrees.dedup();
    Some(worktrees)
}

/// Where Claude keeps CLAUDE_CONFIG_DIR's contents: as the agent's
/// environment or this process sets it, else `~/.claude`.
pub fn config_root(provider_env: &std::collections::HashMap<String, String>) -> Option<PathBuf> {
    const CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";
    provider_env
        .get(CONFIG_DIR)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os(CONFIG_DIR).map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
}

/// The project folders the transcripts of a working folder's sessions are
/// in: its own, and its sibling worktrees', which a resume also reaches.
#[derive(Debug)]
pub struct ProjectFolders {
    /// `<config>/projects`, as given and as the file system resolves it.
    projects: Vec<PathBuf>,
    slugs: Vec<String>,
}

/// The longest project folder name Claude writes whole; a longer one is cut
/// to this and given a hash of the path after a hyphen.
const SLUG_MOST: usize = 200;

impl ProjectFolders {
    pub fn of(config_root: &Path, working_dir: &Path) -> Self {
        let projects = config_root.join("projects");
        let mut resolved = vec![projects.clone()];
        resolved.extend(std::fs::canonicalize(&projects).ok());
        resolved.dedup();
        Self {
            projects: resolved,
            slugs: worktrees(working_dir)
                .unwrap_or_default()
                .iter()
                .map(|worktree| project_slug(worktree))
                .collect(),
        }
    }

    /// Whether `transcript` is the file Claude keeps session `session` in,
    /// in one of these folders.
    pub fn hold(&self, transcript: &Path, session: &str) -> bool {
        use std::path::Component;
        if !transcript.is_absolute()
            || transcript
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
            || transcript.file_name() != Some(std::ffi::OsStr::new(&format!("{session}.jsonl")))
        {
            return false;
        }
        let Some(folder) = transcript.parent() else {
            return false;
        };
        let (Some(name), Some(projects)) = (folder.file_name(), folder.parent()) else {
            return false;
        };
        let name = name.to_string_lossy();
        self.projects.iter().any(|known| known == projects)
            && self.slugs.iter().any(|slug| {
                *slug == name
                    || (slug.len() > SLUG_MOST
                        && name
                            .strip_prefix(&slug[..SLUG_MOST])
                            .is_some_and(|hash| hash.starts_with('-')))
            })
    }
}

/// What Claude would make of a session id at launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionFile {
    /// A transcript holding a conversation: `--resume` takes it.
    Conversation,
    /// A transcript in this folder's project with no conversation (a Claude
    /// stopped before it saved its first prompt leaves only metadata):
    /// `--resume` finds nothing to resume, and `--session-id` refuses an id
    /// it already has.
    Empty,
    /// No transcript where `--session-id` looks: Claude starts the session.
    Missing,
}

/// Looks where Claude does: `--resume` across every project
/// [`find_session_file`] reaches, `--session-id` in this folder's project
/// alone.
pub fn session_file(config_root: &Path, working_dir: &Path, session_id: &str) -> SessionFile {
    if find_session_file(config_root, working_dir, session_id)
        .is_some_and(|path| has_conversation(&path))
    {
        return SessionFile::Conversation;
    }
    let here = absolute_path(working_dir).map(|dir| {
        config_root
            .join("projects")
            .join(project_slug(&dir))
            .join(format!("{session_id}.jsonl"))
    });
    if here.is_ok_and(|path| path.is_file()) {
        SessionFile::Empty
    } else {
        SessionFile::Missing
    }
}

/// Whether a transcript has a user or assistant row. Claude writes the
/// file only at its first such message, so this is its own test in
/// practice; reading stops at the first.
fn has_conversation(path: &Path) -> bool {
    #[derive(serde::Deserialize)]
    struct Row {
        #[serde(rename = "type")]
        kind: String,
    }
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    std::io::BufRead::lines(std::io::BufReader::new(file))
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Row>(&line).ok())
        .any(|row| row.kind == "user" || row.kind == "assistant")
}

pub fn valid_session_id(session_id: &str) -> bool {
    uuid::Uuid::parse_str(session_id).is_ok()
}

fn absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn project_slug(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn session_file_uses_the_configured_project_slug() {
        let root = TempDir::new().unwrap();
        let project = Path::new("/Users/example/project.with_under score");
        let config = root.path().join("claude");
        let session_id = "11111111-1111-1111-1111-111111111111";
        let transcript = config
            .join("projects")
            .join("-Users-example-project-with-under-score")
            .join(format!("{session_id}.jsonl"));
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, b"{}\n").unwrap();

        assert_eq!(
            find_session_file(&config, project, session_id),
            Some(transcript)
        );
    }

    /// A transcript resumes only with a conversation in it; one in this
    /// folder's project without one blocks its id; none here is unused.
    #[test]
    fn a_session_file_is_judged_as_claude_judges_it() {
        let root = TempDir::new().unwrap();
        let config = root.path().join("claude");
        let work = root.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let session = "33333333-3333-3333-3333-333333333333";
        let transcript = config
            .join("projects")
            .join(project_slug(&work))
            .join(format!("{session}.jsonl"));
        assert_eq!(session_file(&config, &work, session), SessionFile::Missing);
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(
            &transcript,
            "{\"type\":\"last-prompt\"}\n{\"type\":\"mode\"}\n",
        )
        .unwrap();
        assert_eq!(session_file(&config, &work, session), SessionFile::Empty);
        std::fs::write(
            &transcript,
            "{\"type\":\"mode\"}\nnot json\n{\"type\":\"user\"}\n",
        )
        .unwrap();
        assert_eq!(
            session_file(&config, &work, session),
            SessionFile::Conversation
        );
    }

    /// A hook's transcript is accepted only as this session's file in the
    /// working folder's own project, under any name a long path is cut to.
    #[test]
    fn project_folders_hold_only_this_folders_transcripts() {
        let root = TempDir::new().unwrap();
        let config = root.path().join("claude");
        let work = root.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let folders = ProjectFolders::of(&config, &work);
        let session = "44444444-4444-4444-4444-444444444444";
        let project = config.join("projects").join(project_slug(&work));
        let ours = project.join(format!("{session}.jsonl"));
        assert!(folders.hold(&ours, session));
        assert!(!folders.hold(&ours, "55555555-5555-5555-5555-555555555555"));
        assert!(!folders.hold(&project.join("other.jsonl"), session));
        assert!(
            !folders.hold(
                &config
                    .join("projects/elsewhere")
                    .join(format!("{session}.jsonl")),
                session
            )
        );
        assert!(!folders.hold(
            Path::new(&format!("/home/me/.ssh/{session}.jsonl")),
            session
        ));
        assert!(
            !folders.hold(
                &project
                    .join("..")
                    .join("x")
                    .join(format!("{session}.jsonl")),
                session
            )
        );

        let long = root.path().join("d".repeat(240));
        std::fs::create_dir_all(&long).unwrap();
        let folders = ProjectFolders::of(&config, &long);
        let slug = project_slug(&long);
        let cut = config
            .join("projects")
            .join(format!("{}-1x2y3z", &slug[..SLUG_MOST]))
            .join(format!("{session}.jsonl"));
        assert!(folders.hold(&cut, session));
    }

    #[test]
    fn session_file_scans_for_one_unambiguous_fallback() {
        let root = TempDir::new().unwrap();
        let config = root.path().join("claude");
        let session_id = "22222222-2222-2222-2222-222222222222";
        let transcript = config
            .join("projects")
            .join("recorded-elsewhere")
            .join(format!("{session_id}.jsonl"));
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, b"{}\n").unwrap();

        assert_eq!(
            find_session_file(
                &config,
                Path::new("/working/directory/whose-slug-is-absent"),
                session_id,
            ),
            Some(transcript.clone())
        );

        let duplicate = config
            .join("projects")
            .join("duplicate")
            .join(format!("{session_id}.jsonl"));
        std::fs::create_dir_all(duplicate.parent().unwrap()).unwrap();
        std::fs::write(duplicate, b"{}\n").unwrap();
        assert_eq!(
            find_session_file(
                &config,
                Path::new("/working/directory/whose-slug-is-absent"),
                session_id,
            ),
            None
        );
    }
}
