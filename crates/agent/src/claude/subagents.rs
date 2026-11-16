//! Terminal Claude's subagents' transcripts. Claude writes each subagent's
//! conversation to a file of its own beside the agent's transcript
//! (`<session>/subagents/agent-<id>.jsonl`), with a `agent-<id>.meta.json`
//! that names the call that launched it. The interpreter asks for a file
//! when Claude's SubagentStart hook names the subagent; one follower per
//! file then reports its meta, before any row, and its rows as they are
//! written, each file with its own cursor under private/ so a resumed
//! agent reads on from where it stopped.
//!
//! A subagent's end is a fact in the agent's own transcript, which can land
//! before the subagent's last rows: the interpreter then asks for its file
//! to be drained, and the follower reports when it has read it to its end,
//! having waited a bounded moment for a final reply Claude is still
//! writing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use interpret::{Channel, Fact};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::dir;
use crate::provider::{Cursor, ProviderEvent, TRANSCRIPT_POLL, saved_cursor};

/// Where each subagent's cursor is kept, under private/.
const CURSORS: &str = "subagent-cursors";
/// How long rows wait for the meta that says which call launched their
/// subagent. Claude writes the meta when it launches the subagent, so this
/// only matters for a Claude that writes none.
const META_WAIT: Duration = Duration::from_millis(500);
/// How long a drain waits for a completed subagent's final reply, which
/// Claude may write just after it tells the agent the subagent finished.
/// The interpreter gives up on its own after the same time.
const DRAIN_WAIT: Duration = Duration::from_millis(2000);

/// The followed subagent files, by Claude's id for each subagent.
#[derive(Default)]
pub struct Followers {
    followed: BTreeMap<String, Follower>,
}

struct Follower {
    cursor: Cursor,
    task: tokio::task::JoinHandle<()>,
    /// Asks the follower to drain its file: the reply it waits for, if any.
    drain: mpsc::UnboundedSender<Option<String>>,
}

impl Followers {
    /// Follows the subagent `agent_id`'s file beside `transcript`, the
    /// agent's own; once per subagent, as a continued one writes on in the
    /// same file.
    pub fn follow(
        &mut self,
        agent_dir: &Path,
        transcript: &Path,
        agent_id: &str,
        events: &mpsc::Sender<ProviderEvent>,
    ) {
        // Claude's ids are hex; anything else is no file of Claude's.
        if self.followed.contains_key(agent_id)
            || agent_id.is_empty()
            || !agent_id.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return;
        }
        let folder = transcript.with_extension("").join("subagents");
        let rows = folder.join(format!("agent-{agent_id}.jsonl"));
        let meta = folder.join(format!("agent-{agent_id}.meta.json"));
        let cursors = agent_dir.join(dir::PRIVATE).join(CURSORS);
        if let Err(error) = std::fs::create_dir_all(&cursors) {
            let events = events.clone();
            tokio::spawn(async move {
                let _ = events
                    .send(ProviderEvent::WriteFailed {
                        path: cursors,
                        error,
                    })
                    .await;
            });
            return;
        }
        let file = cursors.join(agent_id);
        let cursor = Cursor::new(file.clone(), saved_cursor(&file, &rows), rows);
        let (drain, drains) = mpsc::unbounded_channel();
        let task = tokio::spawn(read(
            agent_id.to_owned(),
            cursor.clone(),
            meta,
            events.clone(),
            drains,
        ));
        self.followed.insert(
            agent_id.to_owned(),
            Follower {
                cursor,
                task,
                drain,
            },
        );
    }

    /// Asks for the subagent `agent_id`'s file to be read to its end,
    /// waiting a bounded moment for `last_reply` when there is one. A file
    /// never followed reads as drained at once.
    pub fn drain(
        &mut self,
        agent_id: &str,
        last_reply: Option<String>,
        events: &mpsc::Sender<ProviderEvent>,
    ) {
        if let Some(follower) = self.followed.get(agent_id)
            && follower.drain.send(last_reply).is_ok()
        {
            return;
        }
        let events = events.clone();
        let fact = drained(agent_id);
        tokio::spawn(async move {
            let _ = events.send(ProviderEvent::Fact(fact)).await;
        });
    }

    /// Stops following and returns the rows not yet read: Claude has exited,
    /// so the files are complete.
    pub async fn finish(&mut self, events: &mpsc::Sender<ProviderEvent>) -> Vec<Fact> {
        let mut facts = Vec::new();
        for (_, follower) in std::mem::take(&mut self.followed) {
            follower.task.abort();
            let _ = follower.task.await;
            let Ok(rows) = follower.cursor.unread() else {
                continue;
            };
            for row in rows {
                if !follower.cursor.advance(events, row.len() + 1).await {
                    break;
                }
                if interpret::claude_pty::subagent_row_kept(&row) {
                    facts.push(Fact {
                        channel: Channel::SubagentTranscript,
                        payload: row,
                    });
                }
            }
        }
        facts
    }
}

impl Drop for Followers {
    fn drop(&mut self) {
        for follower in self.followed.values() {
            follower.task.abort();
        }
    }
}

fn agent_fact(payload: Vec<u8>) -> Fact {
    Fact {
        channel: Channel::Agent,
        payload,
    }
}

fn drained(agent_id: &str) -> Fact {
    agent_fact(interpret::claude_pty::subagent_drained_fact(agent_id))
}

/// The meta Claude wrote for a subagent, once it is whole.
fn read_meta(path: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .filter(serde_json::Value::is_object)
}

/// One subagent file's follower: its meta, then its rows as they are
/// written, and a drained fact for each drain asked of it. A file it can no
/// longer read is reported once, and followed no further.
async fn read(
    agent_id: String,
    cursor: Cursor,
    meta_path: PathBuf,
    events: mpsc::Sender<ProviderEvent>,
    mut drains: mpsc::UnboundedReceiver<Option<String>>,
) {
    let send = |fact: Fact| {
        let events = events.clone();
        async move { events.send(ProviderEvent::Fact(fact)).await.is_ok() }
    };
    let begun = Instant::now();
    let mut met = false;
    let mut draining: Option<Instant> = None;
    loop {
        if !met && let Some(meta) = read_meta(&meta_path) {
            met = true;
            let fact = agent_fact(interpret::claude_pty::subagent_meta_fact(&agent_id, meta));
            if !send(fact).await {
                return;
            }
        }
        while let Ok(reply) = drains.try_recv() {
            let wait = if reply.is_some() {
                DRAIN_WAIT
            } else {
                Duration::ZERO
            };
            let by = Instant::now() + wait;
            draining = Some(draining.map_or(by, |earlier| earlier.max(by)));
        }
        if met || draining.is_some() || begun.elapsed() >= META_WAIT {
            let rows = match cursor.unread() {
                Ok(rows) => rows,
                Err(error) => {
                    let fact = agent_fact(interpret::claude_pty::subagent_unreadable_fact(
                        &agent_id, &error,
                    ));
                    send(fact).await;
                    if draining.is_some() {
                        send(drained(&agent_id)).await;
                    }
                    return;
                }
            };
            for row in rows {
                if interpret::claude_pty::subagent_row_kept(&row) {
                    let fact = Fact {
                        channel: Channel::SubagentTranscript,
                        payload: row.clone(),
                    };
                    // Sending is where an abort lands; the row counts as
                    // read only once it is sent.
                    if !send(fact).await {
                        return;
                    }
                }
                if !cursor.advance(&events, row.len() + 1).await {
                    return;
                }
            }
            if draining.is_some_and(|by| Instant::now() >= by) {
                draining = None;
                if !send(drained(&agent_id)).await {
                    return;
                }
            }
        }
        tokio::time::sleep(TRANSCRIPT_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The next fact the followers sent, as its channel and its JSON.
    async fn next(events: &mut mpsc::Receiver<ProviderEvent>) -> (Channel, serde_json::Value) {
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("a fact in time")
            .expect("the channel open");
        match event {
            ProviderEvent::Fact(fact) => (
                fact.channel,
                serde_json::from_slice(&fact.payload).expect("a JSON fact"),
            ),
            other => panic!("not a fact: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_subagents_file_is_followed_meta_first_drained_and_reported_once_it_shrinks() {
        let root = tempfile::tempdir().unwrap();
        let agent_dir = root.path().join("agent");
        std::fs::create_dir_all(agent_dir.join(dir::PRIVATE)).unwrap();
        let transcript = root.path().join("s1.jsonl");
        let folder = root.path().join("s1").join("subagents");
        std::fs::create_dir_all(&folder).unwrap();
        let rows = folder.join("agent-a1.jsonl");
        std::fs::write(
            &rows,
            concat!(
                r#"{"type":"user","uuid":"u1","agentId":"a1"}"#,
                "\n",
                r#"{"type":"attachment","uuid":"x1","agentId":"a1"}"#,
                "\n",
            ),
        )
        .unwrap();
        std::fs::write(
            folder.join("agent-a1.meta.json"),
            r#"{"toolUseId":"toolu_1","description":"Look"}"#,
        )
        .unwrap();
        let (sender, mut events) = mpsc::channel(16);
        let mut followers = Followers::default();
        followers.follow(&agent_dir, &transcript, "a1", &sender);

        let (channel, meta) = next(&mut events).await;
        assert_eq!(channel, Channel::Agent);
        assert_eq!(meta["type"], "subagent_meta");
        assert_eq!(meta["meta"]["toolUseId"], "toolu_1");
        // The attachment row is Claude's own and never sent.
        let (channel, row) = next(&mut events).await;
        assert_eq!(channel, Channel::SubagentTranscript);
        assert_eq!(row["uuid"], "u1");

        followers.drain("a1", None, &sender);
        let (_, drained) = next(&mut events).await;
        assert_eq!(drained["type"], "subagent_drained");

        std::fs::write(&rows, "{}\n").unwrap();
        let (_, unreadable) = next(&mut events).await;
        assert_eq!(unreadable["type"], "subagent_unreadable");
        assert!(
            unreadable["error"].as_str().unwrap().contains("shorter"),
            "{unreadable}"
        );
        assert!(
            std::fs::read_to_string(agent_dir.join(dir::PRIVATE).join(CURSORS).join("a1"))
                .unwrap()
                .ends_with("agent-a1.jsonl"),
            "the cursor is kept per subagent"
        );
    }
}
