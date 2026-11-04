//! Folders worth offering as projects: where Claude Code and Codex ran on
//! this machine lately, read from the session logs each keeps under the
//! person's home. Each log names the directory its session ran in; that
//! directory's Git repository is what is offered, and only while it still
//! exists outside the temporary directories.
//!
//! Claude Code keeps `~/.claude/projects/<folder>/<session>.jsonl`, one
//! folder per directory it ran in, each line carrying `cwd`. Codex keeps
//! `~/.codex/sessions/<year>/<month>/<day>/rollout-*.jsonl`, whose first
//! line describes the session with its `cwd`. A log's modification time is
//! when its session last ran.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wire::ProjectEntry;

/// How far back a session counts.
pub(crate) const WITHIN: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The most folders one answer offers.
pub(crate) const MOST: usize = 8;

/// How many lines of a Claude Code log are read looking for its directory:
/// the first lines can be summaries that carry none, and a long session's
/// log is not worth reading to its end.
const LINES_READ: usize = 64;

/// The Git repositories sessions ran in within [`WITHIN`] of `now`, newest
/// first, at most [`MOST`]. Reads the file system: call it off the async
/// workers.
pub(crate) fn suggest(home: &Path, now: SystemTime) -> Vec<ProjectEntry> {
    suggest_outside(home, now, &temporary_dirs())
}

/// [`suggest`], leaving out repositories under `temporary`.
fn suggest_outside(home: &Path, now: SystemTime, temporary: &[PathBuf]) -> Vec<ProjectEntry> {
    let since = now.checked_sub(WITHIN).unwrap_or(UNIX_EPOCH);
    let mut ran: Vec<(SystemTime, PathBuf)> = Vec::new();
    claude_sessions(&home.join(".claude").join("projects"), since, &mut ran);
    codex_sessions(&home.join(".codex").join("sessions"), since, &mut ran);
    ran.sort_by_key(|(at, _)| std::cmp::Reverse(*at));

    let home = home.canonicalize().unwrap_or_else(|_| home.to_owned());
    // A home made under a temporary directory, as a sandbox's is, keeps
    // what is under it.
    let temporary: Vec<&PathBuf> = temporary
        .iter()
        .filter(|dir| !home.starts_with(dir))
        .collect();
    let mut repositories: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    let mut offered: Vec<ProjectEntry> = Vec::new();
    for (at, cwd) in ran {
        if offered.len() == MOST {
            break;
        }
        let Some(root) = repositories
            .entry(cwd.clone())
            .or_insert_with(|| repository(&cwd))
            .clone()
        else {
            continue;
        };
        // A home that is itself a repository (its dotfiles) would claim
        // every session outside a project of its own.
        if root == home || temporary.iter().any(|dir| root.starts_with(dir)) {
            continue;
        }
        let Some(path) = root.to_str() else {
            continue;
        };
        if offered.iter().any(|entry| entry.path == path) {
            continue;
        }
        offered.push(ProjectEntry {
            path: path.to_owned(),
            name: root.file_name().map_or_else(
                || path.to_owned(),
                |name| name.to_string_lossy().into_owned(),
            ),
            last_used_ms: at
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|since| i64::try_from(since.as_millis()).ok()),
        });
    }
    offered
}

/// When a file was last written, if within the window.
fn modified_since(path: &Path, since: SystemTime) -> Option<SystemTime> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    (modified >= since).then_some(modified)
}

fn is_log(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
}

/// Each Claude Code project folder's newest session in the window, with
/// the directory it ran in: one folder is one directory, so its newest log
/// says all it can.
fn claude_sessions(projects: &Path, since: SystemTime, ran: &mut Vec<(SystemTime, PathBuf)>) {
    let Ok(folders) = std::fs::read_dir(projects) else {
        return;
    };
    for folder in folders.flatten() {
        let Ok(logs) = std::fs::read_dir(folder.path()) else {
            continue;
        };
        let mut logs: Vec<(SystemTime, PathBuf)> = logs
            .flatten()
            .map(|log| log.path())
            .filter(|path| is_log(path))
            .filter_map(|path| Some((modified_since(&path, since)?, path)))
            .collect();
        logs.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        // The newest log that names its directory.
        if let Some((at, cwd)) = logs
            .into_iter()
            .find_map(|(at, path)| Some((at, cwd_in(&path, LINES_READ, |line| line.cwd)?)))
        {
            ran.push((at, cwd));
        }
    }
}

/// Every Codex session in the window, with the directory it ran in.
fn codex_sessions(sessions: &Path, since: SystemTime, ran: &mut Vec<(SystemTime, PathBuf)>) {
    let mut dirs = vec![sessions.to_owned()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                dirs.push(path);
            } else if is_log(&path)
                && let Some(at) = modified_since(&path, since)
                && let Some(cwd) = cwd_in(&path, 1, |line| {
                    line.payload.and_then(|p| p.cwd).or(line.cwd)
                })
            {
                ran.push((at, cwd));
            }
        }
    }
}

/// What a log line says about where its session ran.
#[derive(serde::Deserialize)]
struct Line {
    cwd: Option<PathBuf>,
    payload: Option<Payload>,
}

#[derive(serde::Deserialize)]
struct Payload {
    cwd: Option<PathBuf>,
}

/// The first absolute directory one of the log's first `lines` names.
fn cwd_in(path: &Path, lines: usize, cwd: impl Fn(Line) -> Option<PathBuf>) -> Option<PathBuf> {
    let file = std::fs::File::open(path).ok()?;
    BufReader::new(file)
        .lines()
        .take(lines)
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Line>(&line).ok())
        .find_map(|line| cwd(line).filter(|dir| dir.is_absolute()))
}

/// The repository a directory is in, as it is now: the nearest directory
/// up from it holding `.git`, and for a linked worktree the repository it
/// was made from, which is the project a person means.
fn repository(dir: &Path) -> Option<PathBuf> {
    let dir = dir.canonicalize().ok()?;
    let root = dir.ancestors().find(|dir| dir.join(".git").exists())?;
    let dot_git = root.join(".git");
    if dot_git.is_file()
        && let Ok(text) = std::fs::read_to_string(&dot_git)
        && let Some(gitdir) = text.trim().strip_prefix("gitdir:")
    {
        // `<repository>/.git/worktrees/<name>`; a submodule's points under
        // `.git/modules` and is a repository of its own.
        let gitdir = root.join(gitdir.trim());
        if let Some(worktrees) = gitdir.parent()
            && worktrees
                .file_name()
                .is_some_and(|name| name == "worktrees")
            && let Some(main) = worktrees.parent().and_then(Path::parent)
            && let Ok(main) = main.canonicalize()
            && main.join(".git").is_dir()
        {
            return Some(main);
        }
    }
    Some(root.to_owned())
}

/// The directories whose contents are made to be thrown away.
fn temporary_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![std::env::temp_dir()];
    if cfg!(unix) {
        dirs.extend(["/tmp", "/var/tmp", "/var/folders"].map(PathBuf::from));
    }
    let canonical: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|dir| dir.canonicalize().ok())
        .collect();
    dirs.extend(canonical);
    dirs
}

#[cfg(test)]
mod tests {
    use std::fs::{File, FileTimes};

    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    fn log(path: &Path, text: &str, at: SystemTime) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(at))
            .unwrap();
    }

    fn repo(path: &Path) -> PathBuf {
        std::fs::create_dir_all(path.join(".git")).unwrap();
        path.canonicalize().unwrap()
    }

    fn json(value: serde_json::Value) -> String {
        format!("{value}\n")
    }

    #[test]
    fn suggestions_are_recent_repositories_that_still_exist_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = repo(&dir.path().join("scratch/run"));
        let home = dir.path().join("home");
        let code = dir.path().join("code");
        let (app, lib, old, worktree) = (
            repo(&code.join("app")),
            repo(&code.join("lib")),
            repo(&code.join("old")),
            code.join("app-feature"),
        );
        std::fs::create_dir_all(app.join(".git/worktrees/feature")).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", app.join(".git/worktrees/feature").display()),
        )
        .unwrap();
        let plain = code.join("not-a-repo");
        std::fs::create_dir_all(&plain).unwrap();
        let now = SystemTime::now();

        let claude = home.join(".claude/projects");
        // A summary line first, then the session's directory, deep inside
        // the repository.
        let nested = lib.join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();
        log(
            &claude.join("-lib/one.jsonl"),
            &(json(serde_json::json!({"type": "summary"}))
                + &json(serde_json::json!({"cwd": nested}))),
            now - 2 * DAY,
        );
        log(
            &claude.join("-old/one.jsonl"),
            &json(serde_json::json!({"cwd": old})),
            now - 40 * DAY,
        );
        log(
            &claude.join("-gone/one.jsonl"),
            &json(serde_json::json!({"cwd": code.join("gone")})),
            now - DAY,
        );
        log(
            &claude.join("-plain/one.jsonl"),
            &json(serde_json::json!({"cwd": plain})),
            now - DAY,
        );
        let codex = home.join(".codex/sessions/2026/11/03");
        log(
            &codex.join("rollout-a.jsonl"),
            &json(serde_json::json!({"type": "session_meta", "payload": {"cwd": worktree}})),
            now - 3 * DAY,
        );
        log(
            &codex.join("rollout-b.jsonl"),
            &json(serde_json::json!({"type": "session_meta", "payload": {"cwd": lib}})),
            now - DAY,
        );
        log(
            &codex.join("rollout-c.jsonl"),
            &json(serde_json::json!({"payload": {"cwd": scratch}})),
            now,
        );

        let temporary = [dir.path().join("scratch").canonicalize().unwrap()];
        let offered = suggest_outside(&home, now, &temporary);
        let paths: Vec<&str> = offered.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(
            paths,
            [lib.to_str().unwrap(), app.to_str().unwrap()],
            "the newest session of each repository, a worktree as the repository it came from"
        );
        assert_eq!(offered[0].name, "lib");
        assert!(offered[0].last_used_ms > offered[1].last_used_ms);
    }

    #[test]
    fn no_more_than_the_most_are_offered() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let now = SystemTime::now();
        for n in 0..MOST + 3 {
            let repo = repo(&dir.path().join(format!("repo{n}")));
            log(
                &home.join(format!(".claude/projects/-repo{n}/s.jsonl")),
                &json(serde_json::json!({"cwd": repo})),
                now - Duration::from_secs(n as u64 * 60),
            );
        }
        let offered = suggest_outside(&home, now, &[]);
        assert_eq!(offered.len(), MOST);
        assert_eq!(offered[0].name, "repo0");
        assert!(suggest_outside(&dir.path().join("nobody"), now, &[]).is_empty());
    }
}
