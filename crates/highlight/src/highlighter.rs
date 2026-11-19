//! The highlighter a process shares: grammars built on first use away from
//! the caller's thread, finished code kept by content, and blocks still
//! streaming re-highlighted in the background at a bounded rate.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::grammar::{Configured, GRAMMARS, GrammarId};
use crate::{Highlighted, Language, Line, render};

/// How often a block still streaming in is highlighted again. Each pass
/// parses the block from the start, so passing on every line that arrives
/// costs time quadratic in the block's length; at this rate a long block
/// costs a bounded share of a core, and new lines still colour within a
/// glance.
pub const STREAM_INTERVAL: Duration = Duration::from_millis(100);

/// Finished blocks kept before the cache starts again.
const FINISHED: usize = 512;

/// Blocks streaming at once that each keep their last pass. One message
/// streams at a time and only its last block grows, so a few is plenty.
const STREAMS: usize = 8;

/// Work the highlighter hands off its caller's thread.
pub type Job = Box<dyn FnOnce() + Send + 'static>;

type Spawn = dyn Fn(Job) + Send + Sync;

/// Highlights code. Cheap to clone; clones share their grammars and
/// caches. Most callers use [`Highlighter::shared`].
#[derive(Clone)]
pub struct Highlighter {
    inner: Arc<Inner>,
}

struct Inner {
    spawn: Box<Spawn>,
    slots: Vec<Slot>,
    /// Work handed off and not yet finished.
    pending: AtomicUsize,
    finished: Mutex<HashMap<Key, Arc<[Line]>>>,
    streams: Mutex<Streams>,
}

/// A grammar's configuration, built once. `None` inside means its queries
/// failed to compile, so its code is drawn plain for good.
#[derive(Default)]
struct Slot {
    started: AtomicBool,
    built: OnceLock<Option<Configured>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    grammar: usize,
    hash: u64,
    len: usize,
}

impl Key {
    fn of(grammar: GrammarId, code: &str) -> Key {
        let mut hasher = DefaultHasher::new();
        code.hash(&mut hasher);
        Key {
            grammar: grammar.index(),
            hash: hasher.finish(),
            len: code.len(),
        }
    }
}

#[derive(Default)]
struct Streams {
    next: u64,
    /// Least recently asked for first.
    blocks: Vec<Stream>,
}

/// A block still streaming in: the completed lines its latest pass was
/// asked to highlight, and the last pass that came back.
struct Stream {
    id: u64,
    grammar: GrammarId,
    /// The completed lines of the latest pass started. A block keeps this
    /// stream while its code starts with them.
    asked: String,
    /// The completed lines of the last pass that finished, and their lines.
    done: Option<(String, Arc<[Line]>)>,
    running: bool,
    started_at: Option<Instant>,
}

impl Default for Highlighter {
    fn default() -> Self {
        Highlighter::new()
    }
}

impl Highlighter {
    /// A highlighter that does its background work on threads of its own.
    pub fn new() -> Highlighter {
        Highlighter::with_spawner(|job| {
            // Without a thread to spare, the work is done here: late, but
            // never lost.
            let job = Arc::new(Mutex::new(Some(job)));
            let spare = job.clone();
            let spawned = std::thread::Builder::new()
                .name("highlight".to_owned())
                .spawn(move || run_once(&spare));
            if spawned.is_err() {
                run_once(&job);
            }
        })
    }

    /// A highlighter that hands its background work to `spawn`, which
    /// must run each job once, on any thread. Tests run the jobs when they
    /// choose to, so what a caller sees before and after is fixed.
    pub fn with_spawner(spawn: impl Fn(Job) + Send + Sync + 'static) -> Highlighter {
        Highlighter {
            inner: Arc::new(Inner {
                spawn: Box::new(spawn),
                slots: GRAMMARS.iter().map(|_| Slot::default()).collect(),
                pending: AtomicUsize::new(0),
                finished: Mutex::new(HashMap::new()),
                streams: Mutex::new(Streams::default()),
            }),
        }
    }

    /// The process's highlighter, whose grammars and caches every caller
    /// shares.
    pub fn shared() -> &'static Highlighter {
        static SHARED: OnceLock<Highlighter> = OnceLock::new();
        SHARED.get_or_init(Highlighter::new)
    }

    /// Highlights finished code. Kept by content, so drawing the same block
    /// again costs a lookup. Until the language's grammar is built, the
    /// code comes back plain and not settled, and the grammar is being
    /// built in the background.
    pub fn highlight(&self, code: &str, language: Option<Language>) -> Highlighted {
        let Some(language) = language else {
            return Highlighted::plain(code);
        };
        let Some(grammar) = GrammarId::of(language) else {
            return Highlighted::settled(render::diff(code));
        };
        if !self.ensure(grammar) {
            return Highlighted::unsettled(render::plain(code));
        }
        let key = Key::of(grammar, code);
        if let Some(lines) = self.finished().get(&key) {
            return Highlighted::settled_shared(lines.clone());
        }
        // A block that has just finished streaming may have been passed
        // whole already; its streams are done with either way.
        let streamed = {
            let mut streams = self.streams();
            let whole = streams.blocks.iter().find_map(|stream| match &stream.done {
                Some((text, lines)) if stream.grammar == grammar && text == code => {
                    Some(lines.clone())
                }
                _ => None,
            });
            streams
                .blocks
                .retain(|stream| stream.grammar != grammar || !code.starts_with(&stream.asked));
            whole
        };
        let lines = streamed.unwrap_or_else(|| self.render(code, grammar).into());
        let mut finished = self.finished();
        if finished.len() >= FINISHED {
            finished.clear();
        }
        finished.insert(key, lines.clone());
        Highlighted::settled_shared(lines)
    }

    /// Highlights a block still streaming in, without waiting on anything.
    /// The block's completed lines are highlighted in the background, a
    /// pass at most every [`STREAM_INTERVAL`] by `now`; what comes back is
    /// the last pass's lines and the rest of the code plain. It is settled
    /// once nothing in flight or due would colour more of it, which leaves
    /// a last line without its newline plain until the block grows.
    ///
    /// A block is known by its content: a call whose code starts with the
    /// lines an earlier call's pass was asked for carries that block on, so
    /// a caller holds nothing per block. Call [`Highlighter::highlight`]
    /// once the block is finished.
    pub fn highlight_streaming(
        &self,
        code: &str,
        language: Option<Language>,
        now: Instant,
    ) -> Highlighted {
        let Some(language) = language else {
            return Highlighted::plain(code);
        };
        let Some(grammar) = GrammarId::of(language) else {
            return Highlighted::settled(render::diff(code));
        };
        if !self.ensure(grammar) {
            return Highlighted::unsettled(render::plain(code));
        }
        if let Some(lines) = self.finished().get(&Key::of(grammar, code)) {
            return Highlighted::settled_shared(lines.clone());
        }
        let completed = &code[..code.rfind('\n').map_or(0, |at| at + 1)];
        let mut guard = self.streams();
        let streams = &mut *guard;
        let at = match streams
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, stream)| stream.grammar == grammar && code.starts_with(&stream.asked))
            .max_by_key(|(_, stream)| stream.asked.len())
            .map(|(at, _)| at)
        {
            Some(at) => at,
            None => {
                if streams.blocks.len() >= STREAMS {
                    streams.blocks.remove(0);
                }
                streams.next += 1;
                streams.blocks.push(Stream {
                    id: streams.next,
                    grammar,
                    asked: String::new(),
                    done: None,
                    running: false,
                    started_at: None,
                });
                streams.blocks.len() - 1
            }
        };
        // Most recently asked for last, so the oldest goes first.
        let stream = streams.blocks.remove(at);
        streams.blocks.push(stream);
        let stream = streams.blocks.last_mut().expect("just pushed");

        let behind = stream.asked.len() < completed.len();
        let due = stream
            .started_at
            .is_none_or(|started| now.saturating_duration_since(started) >= STREAM_INTERVAL);
        let start = behind && !stream.running && due;
        if start {
            stream.asked = completed.to_owned();
            stream.running = true;
            stream.started_at = Some(now);
        }

        let mut lines: Vec<Line> = Vec::new();
        let mut rest = code;
        if let Some((text, done)) = &stream.done {
            lines.extend(done.iter().cloned());
            rest = &code[text.len()..];
        }
        lines.extend(render::plain(rest));
        let caught_up = stream.done.as_ref().map_or(0, |(text, _)| text.len()) == completed.len();
        let highlighted = Highlighted {
            lines: lines.into(),
            settled: caught_up && !stream.running,
        };
        let id = stream.id;
        // Started with the streams unlocked, since the pass locks them to
        // hand its result back and may run before the spawner returns.
        drop(guard);
        if start {
            self.pass(id, grammar, completed.to_owned());
        }
        highlighted
    }

    /// Builds the language's grammar on this thread, waiting for it if a
    /// background build is under way, so the next call highlights. For a
    /// caller with no frame to keep moving, such as a host highlighting a
    /// file for a diff.
    pub fn prepare(&self, language: Language) {
        if let Some(grammar) = GrammarId::of(language) {
            for grammar in needs(grammar) {
                let slot = &self.inner.slots[grammar.index()];
                slot.started.store(true, Ordering::Relaxed);
                let _ = slot.built.get_or_init(|| build(grammar));
            }
        }
    }

    /// Builds every grammar on this thread, for a caller that wants its
    /// first frame highlighted and settled and can wait for it. Returns how
    /// long it took.
    pub fn preload(&self) -> Duration {
        let started = Instant::now();
        for language in Language::ALL {
            self.prepare(language);
        }
        started.elapsed()
    }

    /// Whether code in the language highlights now, rather than coming
    /// back plain while its grammar is built.
    pub fn is_ready(&self, language: Language) -> bool {
        GrammarId::of(language).is_none_or(|grammar| {
            needs(grammar).all(|grammar| self.inner.slots[grammar.index()].built.get().is_some())
        })
    }

    /// Whether background work is under way: a frame drawn now may show
    /// code plain that a later frame colours, so it should be drawn again
    /// soon.
    pub fn busy(&self) -> bool {
        self.inner.pending.load(Ordering::Acquire) > 0
    }

    /// Whether the grammar and each it injects is built, starting a
    /// background build of any that has not been asked for.
    fn ensure(&self, grammar: GrammarId) -> bool {
        let mut ready = true;
        for grammar in needs(grammar) {
            let slot = &self.inner.slots[grammar.index()];
            if slot.built.get().is_some() {
                continue;
            }
            ready = false;
            if !slot.started.swap(true, Ordering::AcqRel) {
                let inner = self.inner.clone();
                self.spawn(move || {
                    let _ = inner.slots[grammar.index()]
                        .built
                        .get_or_init(|| build(grammar));
                });
            }
        }
        ready
    }

    /// Highlights a streaming block's completed lines in the background and
    /// hands the result to its stream, if the block still has one.
    fn pass(&self, id: u64, grammar: GrammarId, text: String) {
        let highlighter = self.clone();
        self.spawn(move || {
            let lines: Arc<[Line]> = highlighter.render(&text, grammar).into();
            let mut streams = highlighter.streams();
            if let Some(stream) = streams.blocks.iter_mut().find(|stream| stream.id == id) {
                stream.done = Some((text, lines));
                stream.running = false;
            }
        });
    }

    pub(crate) fn render(&self, code: &str, grammar: GrammarId) -> Vec<Line> {
        render::highlighted(code, grammar, |grammar| {
            self.inner.slots[grammar.index()].built.get()?.as_ref()
        })
    }

    fn spawn(&self, job: impl FnOnce() + Send + 'static) {
        self.inner.pending.fetch_add(1, Ordering::AcqRel);
        let inner = self.inner.clone();
        (self.inner.spawn)(Box::new(move || {
            // Counted down however the job ends: where a panic unwinds (tests
            // and development builds), one must not leave the highlighter
            // busy for good.
            struct Done(Arc<Inner>);
            impl Drop for Done {
                fn drop(&mut self) {
                    self.0.pending.fetch_sub(1, Ordering::AcqRel);
                }
            }
            let _done = Done(inner);
            job();
        }));
    }

    fn finished(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Arc<[Line]>>> {
        self.inner
            .finished
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn streams(&self) -> std::sync::MutexGuard<'_, Streams> {
        self.inner
            .streams
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The grammar and those it injects: all must be built before its code is
/// highlighted.
fn needs(grammar: GrammarId) -> impl Iterator<Item = GrammarId> {
    std::iter::once(grammar).chain(grammar.grammar().injects.iter().copied())
}

fn build(grammar: GrammarId) -> Option<Configured> {
    grammar.grammar().configure().ok()
}

fn run_once(job: &Mutex<Option<Job>>) {
    let job = job.lock().ok().and_then(|mut job| job.take());
    if let Some(job) = job {
        job();
    }
}

/// Background work held until whoever holds it runs it. A highlighter
/// built on it does nothing away from its caller, so a test fixes what a
/// caller sees before and after each job, whatever the machine's speed.
#[derive(Clone, Default)]
pub struct HeldJobs(Arc<Mutex<Vec<Job>>>);

impl HeldJobs {
    /// A highlighter whose background work waits here.
    pub fn highlighter(&self) -> Highlighter {
        let held = self.clone();
        Highlighter::with_spawner(move |job| held.jobs().push(job))
    }

    /// The jobs waiting.
    pub fn len(&self) -> usize {
        self.jobs().len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs().is_empty()
    }

    /// Runs every job waiting, and any those hand off in turn.
    pub fn run(&self) {
        loop {
            let jobs = std::mem::take(&mut *self.jobs());
            if jobs.is_empty() {
                return;
            }
            for job in jobs {
                job();
            }
        }
    }

    fn jobs(&self) -> std::sync::MutexGuard<'_, Vec<Job>> {
        // A job that panicked leaves the list as it was.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TokenKind;

    fn kinds(block: &Highlighted) -> Vec<Vec<TokenKind>> {
        block
            .lines
            .iter()
            .map(|line| line.iter().map(|run| run.kind).collect())
            .collect()
    }

    fn is_plain(block: &Highlighted) -> bool {
        block
            .lines
            .iter()
            .flatten()
            .all(|run| run.kind == TokenKind::Plain)
    }

    #[test]
    fn a_grammar_not_built_yet_draws_plain_until_its_build_has_run() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        let code = "fn main() {}\n";

        let first = highlighter.highlight(code, Some(Language::Rust));
        assert!(!first.settled);
        assert!(is_plain(&first));
        assert_eq!(first.lines.len(), 1);
        assert!(!highlighter.is_ready(Language::Rust));
        assert!(highlighter.busy());
        assert_eq!(queue.len(), 1, "the first ask starts one build");

        let again = highlighter.highlight(code, Some(Language::Rust));
        assert!(!again.settled);
        assert_eq!(queue.len(), 1, "asking again does not start another");

        queue.run();
        assert!(highlighter.is_ready(Language::Rust));
        assert!(!highlighter.busy());
        let ready = highlighter.highlight(code, Some(Language::Rust));
        assert!(ready.settled);
        assert_eq!(ready.lines[0][0].text, "fn");
        assert_eq!(ready.lines[0][0].kind, TokenKind::Keyword);
        // Only the grammar asked for was built.
        assert!(!highlighter.is_ready(Language::Swift));
    }

    #[test]
    fn a_grammar_waits_for_the_grammars_it_injects() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        let code = "<script>const x = 1;</script>\n";

        let first = highlighter.highlight(code, Some(Language::Html));
        assert!(!first.settled);
        assert_eq!(
            queue.len(),
            3,
            "HTML, and the JavaScript and CSS it hands its script and style to"
        );
        queue.run();
        let ready = highlighter.highlight(code, Some(Language::Html));
        assert!(ready.settled);
        assert!(
            ready.lines[0]
                .iter()
                .any(|run| run.text == "const" && run.kind == TokenKind::Keyword),
            "{:?}",
            ready.lines
        );
        // JavaScript was built along the way, so it is ready on its own.
        assert!(highlighter.is_ready(Language::JavaScript));
    }

    #[test]
    fn plain_code_and_diffs_need_no_grammar() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        let plain = highlighter.highlight("a\nb\n", None);
        assert!(plain.settled);
        assert_eq!(plain, Highlighted::plain("a\nb\n"));
        let diff = highlighter.highlight("-a\n+b\n", Some(Language::Diff));
        assert!(diff.settled);
        assert_eq!(
            kinds(&diff),
            vec![vec![TokenKind::Deleted], vec![TokenKind::Inserted]]
        );
        let streamed =
            highlighter.highlight_streaming("-a\n+b", Some(Language::Diff), Instant::now());
        assert!(streamed.settled);
        assert_eq!(
            kinds(&streamed),
            vec![vec![TokenKind::Deleted], vec![TokenKind::Inserted]]
        );
        assert!(highlighter.is_ready(Language::Diff));
        assert_eq!(queue.len(), 0);
        assert!(!highlighter.busy());
    }

    #[test]
    fn finished_code_is_kept_by_content() {
        let highlighter = Highlighter::with_spawner(|job| job());
        highlighter.prepare(Language::Rust);
        let first = highlighter.highlight("let a = 1;\n", Some(Language::Rust));
        let again = highlighter.highlight("let a = 1;\n", Some(Language::Rust));
        assert!(
            Arc::ptr_eq(&first.lines, &again.lines),
            "the second draw is the kept result"
        );
        let other = highlighter.highlight("let b = 2;\n", Some(Language::Rust));
        assert!(!Arc::ptr_eq(&first.lines, &other.lines));
        // The same code in another language is another result.
        highlighter.prepare(Language::Python);
        let python = highlighter.highlight("let a = 1;\n", Some(Language::Python));
        assert!(!Arc::ptr_eq(&first.lines, &python.lines));
    }

    #[test]
    fn preload_builds_every_grammar() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        highlighter.preload();
        for language in Language::ALL {
            assert!(highlighter.is_ready(language), "{language:?}");
        }
        assert_eq!(
            queue.len(),
            0,
            "preloading waits for nothing in the background"
        );
    }

    #[test]
    fn prepare_waits_for_a_build_already_under_way() {
        // Real threads: the background build may or may not have finished
        // when prepare is called, and either way prepare returns ready.
        let highlighter = Highlighter::new();
        assert!(
            !highlighter
                .highlight("let x = 1\n", Some(Language::Swift))
                .settled
        );
        highlighter.prepare(Language::Swift);
        assert!(highlighter.is_ready(Language::Swift));
        assert!(
            highlighter
                .highlight("let x = 1\n", Some(Language::Swift))
                .settled
        );
    }

    #[test]
    fn a_streaming_block_is_passed_over_at_most_once_an_interval() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        highlighter.prepare(Language::Rust);
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let rust = Some(Language::Rust);

        // The first ask starts a pass over the completed line at once.
        let first = highlighter.highlight_streaming("fn a() {}\nlet", rust, at(0));
        assert!(!first.settled);
        assert!(is_plain(&first));
        assert_eq!(first.lines.len(), 2);
        assert_eq!(queue.len(), 1);

        // While it runs, more lines start nothing more.
        let grown = highlighter.highlight_streaming("fn a() {}\nlet x = 1;\n", rust, at(10));
        assert!(!grown.settled);
        assert!(is_plain(&grown));
        assert_eq!(queue.len(), 1);

        // Its result covers the line it was asked for; the rest is plain,
        // and the next pass waits for the interval.
        queue.run();
        let partly = highlighter.highlight_streaming("fn a() {}\nlet x = 1;\n", rust, at(20));
        assert!(!partly.settled, "the second line is due a pass");
        assert_eq!(partly.lines[0][0].kind, TokenKind::Keyword);
        assert!(
            partly.lines[1]
                .iter()
                .all(|run| run.kind == TokenKind::Plain)
        );
        assert_eq!(queue.len(), 0, "too soon for another pass");
        let still = highlighter.highlight_streaming("fn a() {}\nlet x = 1;\n", rust, at(99));
        assert_eq!(still, partly);
        assert_eq!(queue.len(), 0);

        // Once the interval has passed, the next pass starts.
        let due = highlighter.highlight_streaming("fn a() {}\nlet x = 1;\n", rust, at(100));
        assert!(!due.settled);
        assert_eq!(queue.len(), 1);
        queue.run();
        let caught_up = highlighter.highlight_streaming("fn a() {}\nlet x = 1;\n", rust, at(110));
        assert!(caught_up.settled);
        assert_eq!(caught_up.lines[1][0].text, "let");
        assert_eq!(caught_up.lines[1][0].kind, TokenKind::Keyword);

        // A last line without its newline waits for the block to grow: the
        // block is settled with it plain, and no pass starts.
        let tail = highlighter.highlight_streaming("fn a() {}\nlet x = 1;\nfn", rust, at(500));
        assert!(tail.settled);
        assert_eq!(tail.lines.len(), 3);
        assert!(tail.lines[2].iter().all(|run| run.kind == TokenKind::Plain));
        assert_eq!(queue.len(), 0);
        assert!(!highlighter.busy());
    }

    #[test]
    fn a_finished_block_takes_over_from_its_stream() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        highlighter.prepare(Language::Rust);
        let now = Instant::now();
        let code = "let a = 1;\n";
        highlighter.highlight_streaming(code, Some(Language::Rust), now);
        queue.run();
        let streamed = highlighter.highlight_streaming(code, Some(Language::Rust), now);
        assert!(streamed.settled);

        let finished = highlighter.highlight(code, Some(Language::Rust));
        assert!(finished.settled);
        assert_eq!(
            finished, streamed,
            "the pass over the whole block is the finished result"
        );
        assert!(
            highlighter.streams().blocks.is_empty(),
            "the block's stream is done with"
        );
        // Asking for the finished block as streaming finds the kept result.
        let again = highlighter.highlight_streaming(code, Some(Language::Rust), now);
        assert!(Arc::ptr_eq(&again.lines, &finished.lines));
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn two_blocks_streaming_keep_their_own_passes() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        highlighter.prepare(Language::Rust);
        let now = Instant::now();
        let rust = Some(Language::Rust);
        highlighter.highlight_streaming("fn a() {}\n", rust, now);
        highlighter.highlight_streaming("struct B;\n", rust, now);
        assert_eq!(queue.len(), 2, "each block starts its own first pass");
        queue.run();
        let a = highlighter.highlight_streaming("fn a() {}\nfn", rust, now);
        let b = highlighter.highlight_streaming("struct B;\nst", rust, now);
        assert_eq!(a.lines[0][0].text, "fn");
        assert_eq!(b.lines[0][0].text, "struct");
        assert!(a.settled && b.settled);
        assert_eq!(highlighter.streams().blocks.len(), 2);
    }

    #[test]
    fn a_stream_not_ready_draws_plain_and_builds_its_grammar() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        let block =
            highlighter.highlight_streaming("fn a() {}\n", Some(Language::Rust), Instant::now());
        assert!(!block.settled);
        assert!(is_plain(&block));
        assert_eq!(queue.len(), 1, "the build, and no pass before it");
        assert!(highlighter.streams().blocks.is_empty());
    }

    #[test]
    fn streams_are_bounded() {
        let highlighter = Highlighter::with_spawner(|job| job());
        highlighter.prepare(Language::Rust);
        let now = Instant::now();
        for n in 0..STREAMS + 3 {
            highlighter.highlight_streaming(
                &format!("const N{n}: u8 = {n};\n"),
                Some(Language::Rust),
                now,
            );
        }
        assert_eq!(highlighter.streams().blocks.len(), STREAMS);
    }
}
