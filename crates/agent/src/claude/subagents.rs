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
//! and for a completed subagent once it has read the final reply Claude may
//! still be writing, or after a bounded moment without it.
//!
//! A follower runs until Claude exits, finished subagents' included, since
//! Claude's model may continue any of them. It reads its file when the
//! file's watch reports a change (see the tail module), and at once when it
//! is asked anything: a drain, or a follow again when a SubagentStart names
//! its subagent once more. Once drained it lets its watch, its open file and
//! every timer go and waits only to be asked again: Claude fires
//! SubagentStart for a subagent its model continues before that subagent
//! writes, and anything written otherwise would land after the subagent's
//! end. A follower that ended, on a file it could no longer read, is started
//! anew by the next follow.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use interpret::{Channel, Fact};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::dir;
use crate::provider::ProviderEvent;
use crate::tail::{Tail, Watch};

/// Where each subagent's cursor is kept, under private/.
const CURSORS: &str = "subagent-cursors";
/// How long rows wait for the meta that says which call launched their
/// subagent. Claude writes the meta when it launches the subagent, so this
/// only matters for a Claude that writes none.
const META_WAIT: Duration = Duration::from_millis(500);
/// The longest a drain waits for a completed subagent's final reply, which
/// Claude may write just after it tells the agent the subagent finished;
/// the drain is reported as soon as that reply is read. The interpreter
/// gives up on its own after the same time.
const DRAIN_WAIT: Duration = Duration::from_millis(2000);

/// The followed subagent files, by Claude's id for each subagent.
#[derive(Default)]
pub struct Followers {
    followed: BTreeMap<String, Follower>,
    #[cfg(test)]
    wake: Option<std::sync::Arc<tokio::sync::Notify>>,
}

#[cfg(test)]
impl Followers {
    /// Followers whose file watches are woken by `wake` instead of the OS
    /// watcher, for paused-clock tests.
    fn with_wake(wake: std::sync::Arc<tokio::sync::Notify>) -> Self {
        Self {
            followed: BTreeMap::new(),
            wake: Some(wake),
        }
    }
}

struct Follower {
    tail: Tail,
    task: tokio::task::JoinHandle<()>,
    asks: mpsc::UnboundedSender<Ask>,
}

/// What a follower is asked while it runs.
enum Ask {
    /// Drain the file, waiting for this reply if there is one.
    Drain(Option<String>),
    /// Its subagent was named again, as it is when Claude's model
    /// continues it: read at once, whether or not a change was reported.
    Wake,
}

impl Followers {
    /// Follows the subagent `agent_id`'s file beside `transcript`, the
    /// agent's own, with one follower per subagent, as a continued one
    /// writes on in the same file. Following one already followed wakes its
    /// follower; one whose follower ended is followed anew, from its cursor.
    pub fn follow(
        &mut self,
        agent_dir: &Path,
        transcript: &Path,
        agent_id: &str,
        events: &mpsc::Sender<ProviderEvent>,
    ) {
        if let Some(follower) = self.followed.get(agent_id) {
            if follower.asks.send(Ask::Wake).is_ok() {
                return;
            }
            self.followed.remove(agent_id);
        }
        // Claude's ids are hex; anything else is no file of Claude's.
        if agent_id.is_empty() || !agent_id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
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
        #[cfg(test)]
        let tail = match &self.wake {
            Some(wake) => Tail::with_wake(cursors.join(agent_id), rows, wake.clone()),
            None => Tail::new(cursors.join(agent_id), rows),
        };
        #[cfg(not(test))]
        let tail = Tail::new(cursors.join(agent_id), rows);
        let (asks, asked) = mpsc::unbounded_channel();
        #[cfg(test)]
        let meta_wake = self.wake.clone();
        let task = tokio::spawn(read(
            agent_id.to_owned(),
            tail.clone(),
            meta,
            #[cfg(test)]
            meta_wake,
            events.clone(),
            asked,
        ));
        self.followed
            .insert(agent_id.to_owned(), Follower { tail, task, asks });
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
            && follower.asks.send(Ask::Drain(last_reply)).is_ok()
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
            let Ok(rows) = follower.tail.unread() else {
                continue;
            };
            for row in rows {
                if !follower.tail.advance(events, row.len() + 1).await {
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
    tail: Tail,
    meta_path: PathBuf,
    #[cfg(test)] meta_wake: Option<std::sync::Arc<tokio::sync::Notify>>,
    events: mpsc::Sender<ProviderEvent>,
    mut asked: mpsc::UnboundedReceiver<Ask>,
) {
    let send = |fact: Fact| {
        let events = events.clone();
        async move { events.send(ProviderEvent::Fact(fact)).await.is_ok() }
    };
    // The file is watched until its subagent is drained, and again once
    // it is asked about.
    let mut rows_changed = Some(tail.watch());
    // The meta is watched until it is read or no longer waited for.
    #[cfg(test)]
    let mut meta_changed = Some(match meta_wake {
        Some(wake) => Watch::manual(meta_path.clone(), wake),
        None => Watch::new(meta_path.clone()),
    });
    #[cfg(not(test))]
    let mut meta_changed = Some(Watch::new(meta_path.clone()));
    let meta_by = Instant::now() + META_WAIT;
    let mut met = false;
    let mut draining: Option<Instant> = None;
    // The final reply a drain waits for, and what the subagent last said,
    // as the interpreter matches the two.
    let mut awaited: Option<String> = None;
    let mut said = String::new();
    let mut ask: Option<Ask> = None;
    loop {
        if !met && let Some(meta) = read_meta(&meta_path) {
            met = true;
            meta_changed = None;
            let fact = agent_fact(interpret::claude_pty::subagent_meta_fact(&agent_id, meta));
            if !send(fact).await {
                return;
            }
        }
        while let Some(next) = ask.take().or_else(|| asked.try_recv().ok()) {
            if rows_changed.is_none() {
                rows_changed = Some(tail.watch());
            }
            let Ask::Drain(reply) = next else {
                continue;
            };
            let owed =
                reply.filter(|reply| !interpret::claude_pty::subagent_said_is_reply(&said, reply));
            let wait = if owed.is_some() {
                DRAIN_WAIT
            } else {
                Duration::ZERO
            };
            let by = Instant::now() + wait;
            draining = Some(draining.map_or(by, |earlier| earlier.max(by)));
            if owed.is_some() {
                awaited = owed;
            }
        }
        let awaiting_meta = !met && Instant::now() < meta_by;
        if !awaiting_meta {
            // A meta that came late is still read with the rows.
            meta_changed = None;
        }
        if !awaiting_meta || draining.is_some() {
            let rows = match tail.unread() {
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
                if let Some(words) = interpret::claude_pty::subagent_row_said(&row) {
                    said = words;
                }
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
                if !tail.advance(&events, row.len() + 1).await {
                    return;
                }
            }
            let replied = awaited
                .as_deref()
                .is_some_and(|reply| interpret::claude_pty::subagent_said_is_reply(&said, reply));
            if draining.is_some_and(|by| replied || Instant::now() >= by) {
                draining = None;
                awaited = None;
                if !send(drained(&agent_id)).await {
                    return;
                }
                // Its subagent is done with until it is asked about again:
                // nothing is watched or timed until then.
                rows_changed = None;
                meta_changed = None;
                tail.close();
                match asked.recv().await {
                    Some(next) => ask = Some(next),
                    None => return,
                }
                continue;
            }
        }
        // Read again when the file or its awaited meta changes, when asked,
        // or at the next deadline: a drain's end, or the meta's.
        let deadline = draining.or(awaiting_meta.then_some(meta_by));
        tokio::select! {
            () = changed(&mut rows_changed) => {}
            () = changed(&mut meta_changed) => {}
            next = asked.recv() => match next {
                Some(next) => ask = Some(next),
                // The followers let it go: nothing reads its facts any more.
                None => return,
            },
            () = until(deadline) => {}
        }
    }
}

/// Waits until `watch` reports a change, or forever when there is none.
async fn changed(watch: &mut Option<Watch>) {
    match watch {
        Some(watch) => watch.changed().await,
        None => std::future::pending().await,
    }
}

/// Waits until `deadline`, or forever when there is none.
async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
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

    #[tokio::test(start_paused = true)]
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

        // A drained follower looks again when asked, as a continued
        // subagent's start asks.
        std::fs::write(&rows, "{}\n").unwrap();
        followers.follow(&agent_dir, &transcript, "a1", &sender);
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

    /// A subagent's folder as Claude writes it, with a first row and a meta.
    struct Folder {
        _root: tempfile::TempDir,
        agent_dir: PathBuf,
        transcript: PathBuf,
        rows: PathBuf,
    }

    fn folder() -> Folder {
        let root = tempfile::tempdir().unwrap();
        let agent_dir = root.path().join("agent");
        std::fs::create_dir_all(agent_dir.join(dir::PRIVATE)).unwrap();
        let transcript = root.path().join("s1.jsonl");
        let folder = root.path().join("s1").join("subagents");
        std::fs::create_dir_all(&folder).unwrap();
        let rows = folder.join("agent-a1.jsonl");
        std::fs::write(&rows, row("u1")).unwrap();
        std::fs::write(
            folder.join("agent-a1.meta.json"),
            r#"{"toolUseId":"toolu_1","description":"Look"}"#,
        )
        .unwrap();
        Folder {
            _root: root,
            agent_dir,
            transcript,
            rows,
        }
    }

    fn row(uuid: &str) -> String {
        format!("{{\"type\":\"user\",\"uuid\":\"{uuid}\",\"agentId\":\"a1\"}}\n")
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }

    /// The row the next fact carries, past its meta.
    async fn next_row(events: &mut mpsc::Receiver<ProviderEvent>) -> serde_json::Value {
        loop {
            let (channel, fact) = next(events).await;
            if channel == Channel::SubagentTranscript {
                return fact;
            }
        }
    }

    /// A follower that ended on a file it could not read leaves no trace
    /// that would stop the subagent being followed again: Claude's model
    /// may continue it, and the follow its start asks for reads on from
    /// its cursor.
    #[tokio::test(start_paused = true)]
    async fn a_follower_that_ended_on_a_file_it_could_not_read_is_started_anew_on_the_next_follow()
    {
        let at = folder();
        let (sender, mut events) = mpsc::channel(16);
        let mut followers = Followers::default();
        followers.follow(&at.agent_dir, &at.transcript, "a1", &sender);
        assert_eq!(next_row(&mut events).await["uuid"], "u1");

        std::fs::write(&at.rows, "").unwrap();
        let (_, unreadable) = next(&mut events).await;
        assert_eq!(unreadable["type"], "subagent_unreadable");
        // It returns right after it reports: no time passes in between.
        while !followers.followed["a1"].task.is_finished() {
            tokio::task::yield_now().await;
        }

        std::fs::write(&at.rows, row("u1") + &row("u2")).unwrap();
        followers.follow(&at.agent_dir, &at.transcript, "a1", &sender);
        assert_eq!(next_row(&mut events).await["uuid"], "u2");
    }

    /// A file left alone is read again when it grows, at the latest by the
    /// fallback look. Once drained, a file that grows is not read until a
    /// follow is asked again, as a continued subagent's start asks, which
    /// reads it at once. The clock is tokio's paused one, which moves only
    /// when every task waits on it.
    #[tokio::test(start_paused = true)]
    async fn an_idle_file_is_read_when_it_grows_and_once_drained_only_when_followed_again() {
        let at = folder();
        let (sender, mut events) = mpsc::channel(16);
        let mut followers = Followers::with_wake(std::sync::Arc::new(tokio::sync::Notify::new()));
        followers.follow(&at.agent_dir, &at.transcript, "a1", &sender);
        assert_eq!(next_row(&mut events).await["uuid"], "u1");

        tokio::time::sleep(Duration::from_secs(10)).await;
        append(&at.rows, &row("u2"));
        let written = Instant::now();
        assert_eq!(next_row(&mut events).await["uuid"], "u2");
        let waited = written.elapsed();
        assert!(
            waited <= crate::tail::FALLBACK_POLL,
            "an idle file read {waited:?} after it grew"
        );

        followers.drain("a1", None, &sender);
        let (_, drained) = next(&mut events).await;
        assert_eq!(drained["type"], "subagent_drained");
        append(&at.rows, &row("u3"));
        // An hour of the paused clock passes at once: no task wakes in it.
        assert!(
            tokio::time::timeout(Duration::from_secs(3600), events.recv())
                .await
                .is_err(),
            "a drained file was read before it was asked about"
        );

        let asked = Instant::now();
        followers.follow(&at.agent_dir, &at.transcript, "a1", &sender);
        assert_eq!(next_row(&mut events).await["uuid"], "u3");
        assert_eq!(
            asked.elapsed(),
            Duration::ZERO,
            "a follow again waits on nothing"
        );
    }

    /// An assistant row of the subagent's that says `text`.
    fn reply_row(text: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "uuid": format!("r-{text}"),
            "agentId": "a1",
            "message": {
                "id": format!("msg-{text}"),
                "type": "message",
                "role": "assistant",
                "model": "claude-haiku",
                "content": [{"type": "text", "text": text}],
            },
        })
        .to_string()
            + "\n"
    }

    /// A drain waiting for a completed subagent's final reply is reported
    /// as soon as the row saying it is read, written after the drain was
    /// asked, rather than at the end of the wait. The clock is tokio's
    /// paused one, which moves only when every task waits on it.
    #[tokio::test(start_paused = true)]
    async fn a_drain_is_reported_once_the_final_reply_it_waits_for_is_read() {
        let at = folder();
        let (sender, mut events) = mpsc::channel(16);
        let wake = std::sync::Arc::new(tokio::sync::Notify::new());
        let mut followers = Followers::with_wake(wake.clone());
        followers.follow(&at.agent_dir, &at.transcript, "a1", &sender);
        assert_eq!(next_row(&mut events).await["uuid"], "u1");

        let asked = Instant::now();
        followers.drain("a1", Some(" DONE\n".to_owned()), &sender);
        // The follower takes the ask and finds nothing to read yet.
        tokio::time::sleep(Duration::from_millis(100)).await;
        append(&at.rows, &reply_row("DONE"));
        wake.notify_one();
        assert_eq!(next_row(&mut events).await["uuid"], "r-DONE");
        let (_, drained) = next(&mut events).await;
        assert_eq!(drained["type"], "subagent_drained");
        assert_eq!(
            asked.elapsed(),
            Duration::from_millis(100),
            "drained without waiting for the drain deadline"
        );
    }

    /// A drain whose final reply never comes is reported when the wait is
    /// over, and one whose reply was read before it was asked at once.
    #[tokio::test(start_paused = true)]
    async fn a_drain_gives_up_on_a_reply_that_never_comes_and_waits_for_none_already_read() {
        let at = folder();
        let (sender, mut events) = mpsc::channel(16);
        let mut followers = Followers::default();
        followers.follow(&at.agent_dir, &at.transcript, "a1", &sender);
        assert_eq!(next_row(&mut events).await["uuid"], "u1");

        let asked = Instant::now();
        followers.drain("a1", Some("NEVER".to_owned()), &sender);
        let (_, drained) = next(&mut events).await;
        assert_eq!(drained["type"], "subagent_drained");
        assert_eq!(asked.elapsed(), DRAIN_WAIT);

        append(&at.rows, &reply_row("SAID"));
        followers.follow(&at.agent_dir, &at.transcript, "a1", &sender);
        assert_eq!(next_row(&mut events).await["uuid"], "r-SAID");
        let asked = Instant::now();
        followers.drain("a1", Some("SAID".to_owned()), &sender);
        let (_, drained) = next(&mut events).await;
        assert_eq!(drained["type"], "subagent_drained");
        assert_eq!(asked.elapsed(), Duration::ZERO);
    }

    /// A subagent followed before Claude made its folder, its file or its
    /// meta is read once they appear, meta first.
    #[tokio::test]
    async fn a_subagent_followed_before_its_files_exist_is_read_once_they_do() {
        let root = tempfile::tempdir().unwrap();
        let agent_dir = root.path().join("agent");
        std::fs::create_dir_all(agent_dir.join(dir::PRIVATE)).unwrap();
        let transcript = root.path().join("s1.jsonl");
        let (sender, mut events) = mpsc::channel(16);
        let mut followers = Followers::default();
        followers.follow(&agent_dir, &transcript, "a1", &sender);

        let folder = root.path().join("s1").join("subagents");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("agent-a1.meta.json"),
            r#"{"toolUseId":"toolu_1","description":"Look"}"#,
        )
        .unwrap();
        append(&folder.join("agent-a1.jsonl"), &row("u1"));
        let (channel, meta) = next(&mut events).await;
        assert_eq!(channel, Channel::Agent);
        assert_eq!(meta["meta"]["toolUseId"], "toolu_1");
        assert_eq!(next_row(&mut events).await["uuid"], "u1");
    }
}
