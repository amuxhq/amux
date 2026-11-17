//! A file read as it grows: terminal Claude's transcript, and each of its
//! subagents' files beside it.
//!
//! A [`Tail`] holds the file open once it has something to read and keeps
//! two positions: how far rows have been sent, which is kept in a cursor
//! file under private/ so a resumed agent reads on from there, and how far
//! the file has been read, which runs ahead of it by the rows not yet sent
//! and the start of a row still being written. Each look reads only the
//! bytes written since the last.
//!
//! A [`Watch`] says when to look again. It waits on the platform's report of
//! a change (kqueue on macOS, inotify on Linux), to the file itself once it
//! exists and to the nearest folder above it until then, and looks anyway
//! every [`FALLBACK_POLL`]: a report can be missed, a network filesystem
//! may send none, and a watcher can fail to start. The files themselves
//! share one watcher for the process, since an agent follows one file for
//! every subagent its session ever ran and Linux allows a user few inotify
//! instances; a wait for a file not made yet has a watcher of its own,
//! dropped once the file exists, because kqueue watches what appears in a
//! watched folder for as long as its watcher lives.

use std::fs::File;
use std::io::{self, Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use notify::Watcher as _;
use tokio::sync::{Notify, mpsc};

use crate::provider::ProviderEvent;

/// How long a [`Watch`] waits for a reported change before it looks anyway.
pub(crate) const FALLBACK_POLL: Duration = Duration::from_secs(1);

/// A followed file and how far it has been read, kept in a cursor file as
/// it moves. Clones share the position, so what one reader leaves unsent
/// another reads, as when the provider's exit drains a follower it stopped.
#[derive(Clone)]
pub(crate) struct Tail {
    path: PathBuf,
    cursor: PathBuf,
    state: Arc<Mutex<State>>,
}

struct State {
    /// How far rows have been sent: the position the cursor file keeps.
    offset: u64,
    /// The file, opened once it had something past the cursor.
    file: Option<File>,
    /// What was read past the cursor: from `sent` on, the rows not yet
    /// sent and then the start of a row still being written.
    read: Vec<u8>,
    /// How much of `read` was sent since it was last trimmed.
    sent: usize,
}

impl Tail {
    /// Follows `path` from where the cursor kept in `cursor` says an
    /// earlier follower of it stopped, or from its start.
    pub(crate) fn new(cursor: PathBuf, path: PathBuf) -> Self {
        let offset = saved_cursor(&cursor, &path);
        Self {
            path,
            cursor,
            state: Arc::new(Mutex::new(State {
                offset,
                file: None,
                read: Vec::new(),
                sent: 0,
            })),
        }
    }

    /// The followed file.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// A watch on the followed file.
    pub(crate) fn watch(&self) -> Watch {
        Watch::new(self.path.clone())
    }

    /// Closes the file until there is more to read in it.
    pub(crate) fn close(&self) {
        self.state.lock().expect("tail lock").file = None;
    }

    /// The whole rows written past the cursor, or why the file cannot be
    /// read on: an error reading it, or a file gone, shorter than what was
    /// read already or replaced by another, none of which Claude does to a
    /// transcript it writes. A file not written yet has no rows.
    ///
    /// It is looked at for as long as the agent runs, finished subagents'
    /// files included, so a file no longer than what was read is not read,
    /// nor opened if it never was.
    pub(crate) fn unread(&self) -> Result<Vec<Vec<u8>>, String> {
        let mut state = self.state.lock().expect("tail lock");
        let state = &mut *state;
        state.read.drain(..state.sent);
        state.sent = 0;
        let end = state.offset + state.read.len() as u64;
        let length = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound && end == 0 => {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error.to_string()),
        };
        if length < end {
            return Err(format!(
                "the file is {length} bytes, shorter than the {end} already read"
            ));
        }
        if length > end {
            let file = match &mut state.file {
                Some(file) => file,
                None => {
                    let mut file = File::open(&self.path).map_err(|error| error.to_string())?;
                    file.seek(io::SeekFrom::Start(end))
                        .map_err(|error| error.to_string())?;
                    state.file.insert(file)
                }
            };
            file.read_to_end(&mut state.read)
                .map_err(|error| error.to_string())?;
            // The open file ends short of the one the path names now.
            if state.offset + (state.read.len() as u64) < length {
                return Err("the file was replaced by another".to_owned());
            }
        }
        Ok(whole_rows(&state.read))
    }

    /// Moves past one row and its newline; an empty row is skipped the
    /// same way, and never sent. The position is the state a resumed
    /// session reads on from, so a position that cannot be kept ends the
    /// incarnation; false says it could not be.
    pub(crate) async fn advance(
        &self,
        events: &mpsc::Sender<ProviderEvent>,
        length: usize,
    ) -> bool {
        let saved = {
            let mut state = self.state.lock().expect("tail lock");
            debug_assert!(
                state.sent + length <= state.read.len(),
                "past what was read"
            );
            state.sent += length;
            state.offset += length as u64;
            format!("{}\n{}", state.offset, self.path.display())
        };
        if let Err(error) = std::fs::write(&self.cursor, saved) {
            let _ = events
                .send(ProviderEvent::WriteFailed {
                    path: self.cursor.clone(),
                    error,
                })
                .await;
            return false;
        }
        true
    }
}

/// The rows of `unread` its last newline ends; what follows it is still
/// being written.
fn whole_rows(unread: &[u8]) -> Vec<Vec<u8>> {
    let Some(end) = unread.iter().rposition(|byte| *byte == b'\n') else {
        return Vec::new();
    };
    unread[..end]
        .split(|byte| *byte == b'\n')
        .map(<[u8]>::to_vec)
        .collect()
}

/// Where an earlier follower of `path` stopped; the start for any other
/// file, or one that is shorter now.
fn saved_cursor(cursor: &Path, path: &Path) -> u64 {
    let Ok(saved) = std::fs::read_to_string(cursor) else {
        return 0;
    };
    let Some((offset, saved_path)) = saved.split_once('\n') else {
        return 0;
    };
    let length = std::fs::metadata(path).map_or(0, |metadata| metadata.len());
    match offset.parse::<u64>() {
        Ok(offset) if Path::new(saved_path) == path && offset <= length => offset,
        _ => 0,
    }
}

/// Says when a followed file may have changed: on a change the platform
/// reports, and at the latest after [`FALLBACK_POLL`].
pub(crate) struct Watch {
    path: PathBuf,
    wake: Arc<Notify>,
    fallback: Duration,
    /// What is watched: the file, or the nearest folder above it that
    /// exists; None when nothing has been looked for yet, or nothing above
    /// the file exists.
    watched: Option<PathBuf>,
    /// Holds the watch on `watched`; ending it ends the watch.
    _held: Held,
}

enum Held {
    /// Nothing reports changes; the fallback alone looks again.
    Nothing,
    /// The file, on the process's watcher.
    File(#[allow(dead_code)] Subscription),
    /// A folder, on a watcher of its own.
    Folder(#[allow(dead_code)] notify::RecommendedWatcher),
}

impl Watch {
    /// Watches `path`, reporting from now on what changes it.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self::with_fallback(path, FALLBACK_POLL)
    }

    /// A watch that waits up to `fallback` for a report.
    pub(crate) fn with_fallback(path: PathBuf, fallback: Duration) -> Self {
        let mut watch = Self {
            path,
            wake: Arc::new(Notify::new()),
            fallback,
            watched: None,
            _held: Held::Nothing,
        };
        watch.settle();
        watch
    }

    /// Waits until the file may have changed. A watch that moved, to the
    /// file once it exists or to a folder nearer it, returns at once, since
    /// a change may have come while it moved. Safe to drop unfinished, as
    /// in a `select!`: the change it waited for wakes the next wait.
    pub(crate) async fn changed(&mut self) {
        if self.settle() {
            return;
        }
        let _ = tokio::time::timeout(self.fallback, self.wake.notified()).await;
    }

    /// Watches what the file calls for now; true when that moved.
    fn settle(&mut self) -> bool {
        let target = if self.path.exists() {
            Some(self.path.clone())
        } else {
            self.path
                .ancestors()
                .skip(1)
                .find(|folder| folder.is_dir())
                .map(Path::to_path_buf)
        };
        if target == self.watched {
            return false;
        }
        self._held = Held::Nothing;
        self._held = match &target {
            Some(file) if *file == self.path => SHARED
                .as_ref()
                .and_then(|shared| shared.subscribe(file, &self.wake))
                .map_or(Held::Nothing, Held::File),
            Some(folder) => watch_folder(folder, &self.wake).map_or(Held::Nothing, Held::Folder),
            None => Held::Nothing,
        };
        self.watched = target;
        true
    }
}

/// A report of a read wakes nobody: following a file reads it.
fn reports_a_change(event: &notify::Event) -> bool {
    !matches!(event.kind, notify::EventKind::Access(_))
}

/// A watcher of its own on `folder`, waking `wake` on whatever changes in
/// it.
fn watch_folder(folder: &Path, wake: &Arc<Notify>) -> Option<notify::RecommendedWatcher> {
    let woken = Arc::clone(wake);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.as_ref().map_or(true, reports_a_change) {
            woken.notify_one();
        }
    })
    .ok()?;
    watcher
        .watch(folder, notify::RecursiveMode::NonRecursive)
        .ok()?;
    Some(watcher)
}

/// The process's one watcher of followed files, or None when it could not
/// start.
static SHARED: LazyLock<Option<Shared>> = LazyLock::new(Shared::start);

struct Shared {
    /// The watcher, and how many watches each file has on it. Never held
    /// while `wakes` is: a watch waits on the watcher's thread, which takes
    /// `wakes` to report.
    watcher: Mutex<(notify::RecommendedWatcher, Vec<(PathBuf, usize)>)>,
    wakes: Arc<Mutex<Vec<Subscriber>>>,
    next: AtomicU64,
}

struct Subscriber {
    id: u64,
    path: PathBuf,
    wake: Arc<Notify>,
}

/// A file's watch on the shared watcher, ended when dropped.
struct Subscription {
    id: u64,
    path: PathBuf,
}

impl Shared {
    fn start() -> Option<Self> {
        let wakes: Arc<Mutex<Vec<Subscriber>>> = Arc::default();
        let reported = Arc::clone(&wakes);
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let wakes = reported.lock().expect("wakes lock");
            // A report that names no file may stand for any: an error, or
            // a queue that overflowed and lost what it held.
            let any = match &event {
                Ok(event) if !reports_a_change(event) => return,
                Ok(event) => event.paths.is_empty() || event.need_rescan(),
                Err(_) => true,
            };
            for subscriber in wakes.iter() {
                if any
                    || event
                        .as_ref()
                        .is_ok_and(|event| event.paths.contains(&subscriber.path))
                {
                    subscriber.wake.notify_one();
                }
            }
        })
        .ok()?;
        Some(Self {
            watcher: Mutex::new((watcher, Vec::new())),
            wakes,
            next: AtomicU64::new(0),
        })
    }

    /// Wakes `wake` on changes to `file`; None when it cannot be watched.
    fn subscribe(&self, file: &Path, wake: &Arc<Notify>) -> Option<Subscription> {
        {
            let mut held = self.watcher.lock().expect("watcher lock");
            let (watcher, counts) = &mut *held;
            if let Some((_, count)) = counts.iter_mut().find(|(path, _)| path == file) {
                *count += 1;
            } else {
                watcher
                    .watch(file, notify::RecursiveMode::NonRecursive)
                    .ok()?;
                counts.push((file.to_path_buf(), 1));
            }
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.wakes.lock().expect("wakes lock").push(Subscriber {
            id,
            path: file.to_path_buf(),
            wake: Arc::clone(wake),
        });
        Some(Subscription {
            id,
            path: file.to_path_buf(),
        })
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let Some(shared) = SHARED.as_ref() else {
            return;
        };
        shared
            .wakes
            .lock()
            .expect("wakes lock")
            .retain(|subscriber| subscriber.id != self.id);
        let mut held = shared.watcher.lock().expect("watcher lock");
        let (watcher, counts) = &mut *held;
        if let Some(at) = counts.iter().position(|(path, _)| *path == self.path) {
            counts[at].1 -= 1;
            if counts[at].1 == 0 {
                counts.swap_remove(at);
                let _ = watcher.unwatch(&self.path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Reads every whole row and moves past it, as a follower does.
    async fn take(tail: &Tail, events: &mpsc::Sender<ProviderEvent>) -> Vec<String> {
        let rows = tail.unread().unwrap();
        for row in &rows {
            assert!(tail.advance(events, row.len() + 1).await);
        }
        rows.into_iter()
            .map(|row| String::from_utf8(row).unwrap())
            .collect()
    }

    /// A row arrives whole however its bytes are split across writes, each
    /// once and in order, and only whole rows move the kept position, so a
    /// tail that starts again from it reads on where the last left off.
    #[tokio::test]
    async fn rows_arrive_whole_and_in_order_however_their_writes_split_them() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("s1.jsonl");
        let cursor = root.path().join("cursor");
        let (events, _kept) = mpsc::channel(4);
        let tail = Tail::new(cursor.clone(), path.clone());
        assert_eq!(
            take(&tail, &events).await,
            Vec::<String>::new(),
            "no file yet"
        );

        append(&path, "{\"a\"");
        assert_eq!(take(&tail, &events).await, Vec::<String>::new());
        append(&path, ":1}\n{\"b\":");
        assert_eq!(take(&tail, &events).await, ["{\"a\":1}"]);
        append(&path, "2}\n\n{\"c\":3}\n{\"d");
        assert_eq!(take(&tail, &events).await, ["{\"b\":2}", "", "{\"c\":3}"]);
        assert_eq!(
            std::fs::read_to_string(&cursor).unwrap(),
            format!("25\n{}", path.display()),
            "the position counts whole rows only"
        );

        let again = Tail::new(cursor, path.clone());
        append(&path, "\":4}\n");
        assert_eq!(take(&again, &events).await, ["{\"d\":4}"]);
    }

    /// A row read but not yet moved past is read again, by the tail or a
    /// clone of it, as the provider's exit reads what an aborted send left.
    #[tokio::test]
    async fn a_row_not_moved_past_is_read_again() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("s1.jsonl");
        append(&path, "one\ntwo\n");
        let (events, _kept) = mpsc::channel(4);
        let tail = Tail::new(root.path().join("cursor"), path);
        assert_eq!(tail.unread().unwrap().len(), 2);
        assert!(tail.advance(&events, 4).await);
        assert_eq!(take(&tail.clone(), &events).await, ["two"]);
    }

    /// A file is looked at for as long as the agent runs: one read to its
    /// end is not opened again until it grows, and then read only past what
    /// was read. A file this process cannot open shows it: unchanged, it has
    /// no rows rather than an error. A file shorter than what was read, or
    /// one put in its place, cannot be read on.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_tail_reads_only_what_was_written_since_and_stops_at_a_file_shortened_or_replaced() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("agent-a1.jsonl");
        let cursor = root.path().join("cursor");
        std::fs::write(&path, "one\n").unwrap();
        std::fs::write(&cursor, format!("4\n{}", path.display())).unwrap();
        let tail = Tail::new(cursor.clone(), path.clone());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(tail.unread(), Ok(Vec::new()), "the file was read again");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, "one\ntwo\nthr").unwrap();
        assert_eq!(tail.unread(), Ok(vec![b"two".to_vec()]));
        std::fs::write(&path, "on").unwrap();
        assert!(tail.unread().unwrap_err().contains("shorter"));

        let (events, _kept) = mpsc::channel(4);
        std::fs::write(&path, "one\n").unwrap();
        let tail = Tail::new(root.path().join("other"), path.clone());
        assert_eq!(take(&tail, &events).await, ["one"]);
        let other = root.path().join("new.jsonl");
        std::fs::write(&other, "one\ntwo\n").unwrap();
        std::fs::rename(&other, &path).unwrap();
        assert!(tail.unread().unwrap_err().contains("replaced"));
    }

    /// With nothing to watch, as for a file no folder above exists for, or
    /// with a watcher that could not start, a watch looks again after the
    /// fallback. The clock is tokio's paused one, which moves only when
    /// every task waits on it.
    #[tokio::test(start_paused = true)]
    async fn with_no_report_a_watch_looks_again_after_the_fallback() {
        let mut watch = Watch::new(PathBuf::from("no-such-folder").join("s1.jsonl"));
        let started = tokio::time::Instant::now();
        watch.changed().await;
        assert_eq!(started.elapsed(), FALLBACK_POLL);
    }
}
