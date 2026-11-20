//! What the terminal and phone stories share: the words they type and
//! expect, the checks on what the hosts and providers recorded, and the
//! door requests they make.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use super::{items, prompts, quoted};
use crate::door::Control;

pub(crate) fn ids(agents: &[Value]) -> BTreeSet<String> {
    agents.iter().map(|agent| text(&agent["id"])).collect()
}

pub(crate) fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

pub(crate) fn listed<'a>(agents: &'a [Value], id: &str) -> Option<&'a Value> {
    agents.iter().find(|agent| agent["id"] == id)
}

/// The one agent `host` lists under `name`.
pub(crate) fn named<'a>(agents: &'a [Value], name: &str) -> Result<&'a Value, String> {
    match agents
        .iter()
        .filter(|agent| agent["name"] == name)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [agent] => Ok(agent),
        found => Err(format!(
            "{} agents are named {name}: {agents:?}",
            found.len()
        )),
    }
}

pub(crate) const LIVE: i64 = 1;
pub(crate) const EXITED: i64 = 2;
pub(crate) const ACTIVE: i64 = 1;
pub(crate) const SETTLED: i64 = 3;

/// The host lists `id` under `name` in lifecycle `state`.
pub(crate) fn lifecycle(agents: &[Value], id: &str, name: &str, state: i64) -> Result<(), String> {
    match listed(agents, id) {
        Some(agent) if agent["name"] == name && agent["lifecycle"] == state => Ok(()),
        agent => Err(format!(
            "the host lists {id} as {agent:?}, not {name:?} in lifecycle {state}"
        )),
    }
}

pub(crate) fn keys_of(chat: &Value) -> Vec<Value> {
    items(chat).iter().map(|item| item["key"].clone()).collect()
}

pub(crate) const PROMPT: &str = "Check the deployment once.";
pub(crate) const REPLY: &str = "The deploy check passed.";
pub(crate) const HELLO: &str = "Say hello.";
pub(crate) const BACK: &str = "Welcome back.";
pub(crate) const READY: &str = "Ready when you are.";

pub(crate) const COMMENT: &str = "Why --delete here?";

pub(crate) fn pasted() -> String {
    (0..12)
        .map(|n| format!("deploy log {n}: rsync finished with status 0"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every file under `directory`, at any depth.
pub(crate) fn walk(directory: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}

pub(crate) fn never_received(chat: &Value, text: &str) -> Result<(), String> {
    if prompts(chat, text).is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the desk received {} while access was lost",
            quoted(text)
        ))
    }
}

/// What the provider was sent, one JSON line each.
fn provider_lines(lines: &[String]) -> Result<Vec<Value>, String> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).map_err(|error| format!("{line}: {error}")))
        .collect()
}

/// What headless Claude was answered, in order, from what it was sent.
pub(crate) fn control_responses(lines: &[String]) -> Result<Vec<Value>, String> {
    Ok(provider_lines(lines)?
        .into_iter()
        .filter(|line| line["type"] == "control_response")
        .map(|line| line["response"]["response"].clone())
        .collect())
}

/// What Codex was answered, in order, from what it was sent.
pub(crate) fn codex_results(lines: &[String]) -> Result<Vec<Value>, String> {
    Ok(provider_lines(lines)?
        .into_iter()
        .filter(|line| line.get("result").is_some() && (line["id"].is_i64() || line["id"].is_u64()))
        .map(|line| line["result"].clone())
        .collect())
}

pub(crate) const PLAN_PROMPT: &str = "Move the journal.";
pub(crate) const PLAN_NOTE: &str = "Check the copy before switching.";
pub(crate) const PLAN_DONE: &str = "Moving the journal as planned.";
/// The plan went back once with `note`, then was approved once.
pub(crate) fn sent_back_then_approved(decisions: &[Value], note: &str) -> Result<(), String> {
    let behaviors: Vec<&Value> = decisions
        .iter()
        .map(|decision| &decision["behavior"])
        .collect();
    let noted = decisions
        .first()
        .and_then(|decision| decision["message"].as_str())
        .is_some_and(|message| message.ends_with(note));
    if behaviors == [&json!("deny"), &json!("allow")] && noted {
        Ok(())
    } else {
        Err(format!("Claude was answered {decisions:?}"))
    }
}

/// The desk holds the two plans as its only tool items: the plan file
/// Claude wrote before each is no step of the chat.
pub(crate) fn plans_only(chat: &Value) -> Result<(), String> {
    let calls: Vec<&Value> = items(chat)
        .iter()
        .filter(|item| {
            item["key"]
                .as_str()
                .is_some_and(|key| key.starts_with("toolu_"))
        })
        .collect();
    let plans = calls.iter().all(|item| {
        item["text"]
            .as_str()
            .is_some_and(|said| said.starts_with("# Move the journal"))
    });
    if calls.len() == 2 && plans {
        Ok(())
    } else {
        let heads: Vec<String> = calls
            .iter()
            .map(|item| text(&item["text"]).chars().take(40).collect())
            .collect();
        Err(format!("the desk holds {heads:?} as tool items"))
    }
}

pub(crate) const ROTATE_PROMPT: &str = "Rotate the logs.";
pub(crate) const ROTATE_NOTE: &str = "Keep a month of logs, compressed.";
pub(crate) const IMPLEMENT: &str = "Implement the plan.";

/// Each turn amux started on Codex, from what it was sent: the
/// collaboration mode it ran in and its words. A turn naming no mode keeps
/// the one before, as Codex does.
pub(crate) fn codex_turns(lines: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut mode = "default".to_owned();
    let mut turns = Vec::new();
    for line in provider_lines(lines)? {
        if line["method"] != "turn/start" {
            continue;
        }
        let params = &line["params"];
        if let Some(named) = params["collaborationMode"]["mode"].as_str() {
            mode = named.to_owned();
        }
        let words: String = params["input"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect();
        turns.push((mode.clone(), words));
    }
    Ok(turns)
}

pub(crate) fn codex_plan_turns(
    turns: &[(String, String)],
    expected: &[(&str, &str)],
) -> Result<(), String> {
    let same = turns.len() == expected.len()
        && turns
            .iter()
            .zip(expected)
            .all(|((mode, words), (want_mode, want_words))| {
                mode == want_mode && words == want_words
            });
    if same {
        Ok(())
    } else {
        Err(format!("Codex was started on {turns:?}"))
    }
}

pub(crate) const OTHER: &str = "From the help panel";
pub(crate) const SKIPPED: &str = "Which section should come first?";
pub(crate) const PALETTE: &str = "Pick the palette too.";
pub(crate) const QUESTION_REPLY: &str = "Let me see both palettes on the desk first.";

pub(crate) fn answers() -> Value {
    json!({
        "How should the settings screen be laid out?": "Stacked",
        "Where should the screen open from?": OTHER,
    })
}

/// The first ask was answered once with exactly `answers`: the skipped
/// question left out.
pub(crate) fn answered(decisions: &[Value], answers: &Value) -> Result<(), String> {
    match decisions.first() {
        Some(decision) if decision["updatedInput"]["answers"] == *answers => Ok(()),
        _ => Err(format!("Claude was answered {decisions:?}")),
    }
}

/// The second ask was refused with the person's own words.
pub(crate) fn replied(decisions: &[Value], words: &str) -> Result<(), String> {
    if decisions.len() == 2
        && decisions[1]["behavior"] == "deny"
        && decisions[1]["message"] == words
    {
        Ok(())
    } else {
        Err(format!("Claude was answered {decisions:?}"))
    }
}

pub(crate) const FORM_TITLE: &str = "Reconnect flake in e2e";

pub(crate) fn form() -> Value {
    json!({"title": FORM_TITLE, "team": "FOX", "estimate": 3})
}

pub(crate) fn form_sent(decisions: &[Value], content: &Value) -> Result<(), String> {
    if decisions.len() == 1 && decisions[0] == json!({"action": "accept", "content": content}) {
        Ok(())
    } else {
        Err(format!("Claude was answered {decisions:?}"))
    }
}

pub(crate) const RUN: &str = "Run the soak.";
pub(crate) const STEER: &str = "Also log the peak.";
pub(crate) const WITHDRAWN: &str = "And email me.";

pub(crate) const BACKUP: &str = "Check last night's backup.";
pub(crate) const BACKUP_DONE: &str = "Last night's backup finished with no errors.";

pub(crate) fn read_json(path: &Path) -> Result<Value, String> {
    let read =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_str(&read).map_err(|error| format!("{}: {error}", path.display()))
}

pub(crate) fn sever(a: &str, b: &str) -> Control {
    Control::Sever {
        a: a.to_owned(),
        b: b.to_owned(),
    }
}

pub(crate) fn restore(a: &str, b: &str) -> Control {
    Control::Restore {
        a: a.to_owned(),
        b: b.to_owned(),
    }
}

pub(crate) fn trust(a: &str, b: &str) -> Control {
    Control::Trust {
        a: a.to_owned(),
        b: b.to_owned(),
    }
}

pub(crate) fn open_gate(name: &str) -> Control {
    Control::OpenGate {
        name: name.to_owned(),
    }
}

pub(crate) fn lines(said: &[&str]) -> Vec<String> {
    said.iter().map(|line| (*line).to_owned()).collect()
}

/// The reviewer's working tree on the desk: one commit of `deploy.sh`,
/// then an edit to it.
pub(crate) fn review_repository(work: &Path) -> Result<(), String> {
    let git = |args: &[&str]| -> Result<(), String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(work)
            .args([
                "-c",
                "user.name=Journey",
                "-c",
                "user.email=journey@example.invalid",
            ])
            .args(args)
            // A pinned author and date keep the commit id the review page
            // shows.
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
            .output()
            .map_err(|error| format!("git: {error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    };
    let script = work.join("deploy.sh");
    let write = |body: &str| std::fs::write(&script, body).map_err(|error| error.to_string());
    git(&["init", "-q", "-b", "main"])?;
    write("#!/bin/sh\necho deploying\nrsync build/ prod:/srv\n")?;
    git(&["add", "deploy.sh"])?;
    git(&["commit", "-qm", "Deploy by rsync"])?;
    write("#!/bin/sh\necho deploying\nrsync --delete build/ prod:/srv\necho done\n")
}

/// The sent review the desk received, as `attachment`: text named
/// `review` with the review's entry, closing round 1 with one comment; the
/// text says where the comment sits (deploy.sh line 3), quotes the changed
/// line it is on as diff, then the person's `comment`, and names the
/// snapshot the line numbers refer to. Answers the snapshot.
pub(crate) fn sent_review(attachment: &Value, comment: &str) -> Result<String, String> {
    let entry = &attachment["review"];
    let snapshot = text(&entry["snapshot"]);
    if attachment["kind"] != "text" || attachment["name"] != "review" || snapshot.is_empty() {
        return Err(format!("the review arrived as {attachment}"));
    }
    if entry["round"] != 1 || entry["comments"].as_array().map_or(0, Vec::len) != 1 {
        return Err(format!("the review's entry is {entry}"));
    }
    let read = text(&attachment["text"]);
    let said = [
        "1 comment from a review of your changes.".to_owned(),
        "## deploy.sh, line 3\n".to_owned(),
        "```diff\n".to_owned(),
        "+rsync --delete build/ prod:/srv\n".to_owned(),
        format!("```\n\n{comment}\n"),
        format!("Line numbers are those of snapshot {snapshot}, "),
    ];
    let mut from = 0;
    for words in &said {
        match read[from..].find(words.as_str()) {
            Some(at) => from += at + words.len(),
            None => {
                return Err(format!(
                    "the review's text does not say {words:?} where it should:\n{read}"
                ));
            }
        }
    }
    Ok(snapshot)
}

/// The snapshot a review's `text` names is a commit kept outside the
/// repository at `work`, which holds no such object: the command the text
/// gives the agent reads it from any folder and finds the working tree as
/// it was snapshotted, its listing of files made since runs, and the
/// snapshot holds the edit.
pub(crate) fn snapshot_outside(work: &Path, read: &str, snapshot: &str) -> Result<(), String> {
    let given = read
        .lines()
        .find(|line| line.starts_with("GIT_INDEX_FILE="))
        .ok_or_else(|| format!("the review gives no command:\n{read}"))?;
    // The diff, then the listing of files made since the snapshot.
    let (command, listing) = given
        .split_once(" && ")
        .ok_or_else(|| format!("the review's command lists nothing made since: {given}"))?;
    let reading = command
        .rsplit_once(" diff ")
        .map(|(reading, _)| reading)
        .ok_or_else(|| format!("the review's command is no diff: {command}"))?;
    let shell = |script: &str| {
        Command::new("sh")
            .args(["-c", script])
            .current_dir(std::env::temp_dir())
            .output()
            .map_err(|error| format!("sh: {error}"))
    };
    if !shell(&format!("{command} --exit-code --quiet"))?
        .status
        .success()
    {
        return Err(format!(
            "the review's command finds the tree moved from the snapshot: {command}"
        ));
    }
    let listed = shell(listing)?;
    if !listed.status.success() {
        return Err(format!(
            "the review's listing of files made since fails: {}",
            String::from_utf8_lossy(&listed.stderr)
        ));
    }
    let shown = shell(&format!("{reading} show {snapshot}:deploy.sh"))?;
    if !String::from_utf8_lossy(&shown.stdout).contains("rsync --delete build/ prod:/srv") {
        return Err(format!(
            "the snapshot does not hold the edit: {}",
            String::from_utf8_lossy(&shown.stderr)
        ));
    }
    let inside = Command::new("git")
        .arg("-C")
        .arg(work)
        .args(["cat-file", "-e", snapshot])
        .output()
        .map_err(|error| format!("git: {error}"))?;
    if inside.status.success() {
        return Err(format!("the repository holds the snapshot {snapshot}"));
    }
    Ok(())
}

/// Waits for the agent's review, as `observe` reads it from the desk, to
/// have sent `comment` in round 1 on `snapshot`, as [`sent_on_desk`] says.
/// The desk closes the round once the agent has taken the prompt, which
/// the agent's reply can beat.
pub(crate) fn wait_sent_on_desk(
    observe: impl FnMut() -> Result<Value, String>,
    comment: &str,
    snapshot: &str,
) -> Result<Value, String> {
    super::poll(
        std::time::Duration::from_secs(30),
        std::time::Duration::from_millis(200),
        observe,
        |review| sent_on_desk(review, comment, snapshot).is_ok(),
    )
    .map_err(|(last, why)| match last {
        Some(review) => sent_on_desk(&review, comment, snapshot)
            .err()
            .unwrap_or(why),
        None => why,
    })
}

/// The agent's review as the desk holds it, `review`, sent `comment` in
/// round 1 on `snapshot`, deploy.sh line 3, and holds no other comment.
pub(crate) fn sent_on_desk(review: &Value, comment: &str, snapshot: &str) -> Result<(), String> {
    let rounds = &review["rounds"];
    let comments = &review["comments"];
    let one = |list: &Value| list.as_array().map_or(0, Vec::len) == 1;
    let sent = one(rounds)
        && rounds[0]["n"] == 1
        && rounds[0]["snapshot"] == snapshot
        && one(comments)
        && comments[0]["text"] == comment
        && comments[0]["round"] == 1
        && comments[0]["path"] == "deploy.sh"
        && comments[0]["commit"] == snapshot
        && comments[0]["lines"] == json!([3, 3]);
    if sent {
        Ok(())
    } else {
        Err(format!("the desk's review is {review}"))
    }
}
