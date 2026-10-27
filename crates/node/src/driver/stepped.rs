//! Cores stepped by their caller.
//!
//! The phone runs its cores on a queue its app owns. Nothing here ever
//! calls into the app: an input arriving for any core rings the bell the
//! app gave, and the app calls [`Cores::step`] on its queue when it hears
//! it. The tokio runtime the shell runs on stays where the app started it;
//! a step enters it only so the timers and jobs the core asks for are
//! spawned there. A daemon with no caller of its own gives each profile a
//! [`Stepper`]: a thread that waits on the bell and steps, the same loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::ThreadId;

use tokio::runtime::Handle;

use super::CoreThread;
use super::feed::Bell;

/// The most inputs one core steps in one call before the others get
/// theirs; what is left rings the bell again.
const BATCH: usize = 256;

/// Every core an installation's profiles run, for the caller to step.
#[derive(Clone)]
pub struct Cores(Arc<Shared>);

struct Shared {
    bell: Bell,
    slots: Mutex<Vec<Arc<Slot>>>,
}

/// One profile's core, held for whoever steps it.
pub(crate) struct Slot {
    core: Mutex<Option<CoreThread>>,
    /// The thread stepping the core now, if any.
    stepping: Mutex<Option<ThreadId>>,
    /// The core has ended. Read without the core's lock, which a thread
    /// stepping it holds.
    ended: AtomicBool,
    handle: Handle,
}

impl std::fmt::Debug for Cores {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cores")
    }
}

impl Cores {
    /// Cores whose waiting inputs ring `bell`, from any thread.
    pub fn new(bell: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(Shared {
            bell: Arc::new(bell),
            slots: Mutex::new(Vec::new()),
        }))
    }

    /// Steps the inputs waiting for every core on the calling thread, and
    /// answers how many it stepped. A core with more than a batch waiting
    /// steps a batch and rings the bell again for the rest.
    pub fn step(&self) -> usize {
        // A step may start a profile, which holds a core of its own here:
        // the list is not held while stepping.
        let slots = self.0.slots.lock().unwrap().clone();
        let mut stepped = 0;
        let mut more = false;
        let mut ended = false;
        for slot in &slots {
            let turn = slot.step(BATCH);
            stepped += turn.stepped;
            more |= turn.more;
            ended |= turn.ended;
        }
        if ended {
            self.0
                .slots
                .lock()
                .unwrap()
                .retain(|slot| !slot.ended.load(Ordering::SeqCst));
        }
        if more {
            (self.0.bell)();
        }
        stepped
    }

    /// How many cores are still held.
    pub fn len(&self) -> usize {
        self.0.slots.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn bell(&self) -> Bell {
        self.0.bell.clone()
    }

    /// Holds `core` for stepping on the runtime it is called from, and
    /// rings for the inputs it starts with.
    pub(crate) fn hold(&self, core: CoreThread) -> Arc<Slot> {
        let slot = Arc::new(Slot {
            core: Mutex::new(Some(core)),
            stepping: Mutex::new(None),
            ended: AtomicBool::new(false),
            handle: Handle::current(),
        });
        self.0.slots.lock().unwrap().push(slot.clone());
        (self.0.bell)();
        slot
    }
}

/// A thread that steps one profile's cores whenever the bell rings, for a
/// daemon whose profiles have no caller stepping them.
pub(crate) struct Stepper {
    thread: Option<std::thread::JoinHandle<()>>,
    rung: Arc<Rung>,
}

/// The bell as a thread waits on it: how many times it rang.
struct Rung {
    count: Mutex<u64>,
    rang: Condvar,
    stopping: AtomicBool,
}

impl Rung {
    fn ring(&self) {
        *self.count.lock().unwrap() += 1;
        self.rang.notify_one();
    }

    /// Waits until the bell rang since `seen`, and answers the count.
    fn wait_past(&self, seen: u64) -> u64 {
        let mut count = self.count.lock().unwrap();
        while *count == seen {
            count = self.rang.wait(count).unwrap();
        }
        *count
    }
}

impl Stepper {
    /// Cores with a thread of their own stepping them.
    pub(crate) fn start() -> (Cores, Stepper) {
        let rung = Arc::new(Rung {
            count: Mutex::new(0),
            rang: Condvar::new(),
            stopping: AtomicBool::new(false),
        });
        let cores = Cores::new({
            let rung = rung.clone();
            move || rung.ring()
        });
        let thread = std::thread::Builder::new()
            .name("amux-core".to_owned())
            .spawn({
                let cores = cores.clone();
                let rung = rung.clone();
                move || {
                    let mut seen = 0;
                    loop {
                        cores.step();
                        if rung.stopping.load(Ordering::SeqCst) && cores.is_empty() {
                            return;
                        }
                        seen = rung.wait_past(seen);
                    }
                }
            })
            .expect("the core thread starts");
        (
            cores,
            Stepper {
                thread: Some(thread),
                rung,
            },
        )
    }

    /// Lets the thread end once its cores have, and waits for it unless
    /// it is the calling thread, which ends on its own.
    pub(crate) fn stop(mut self) {
        self.rung.stopping.store(true, Ordering::SeqCst);
        self.rung.ring();
        if let Some(thread) = self.thread.take()
            && thread.thread().id() != std::thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

/// What one core's turn at stepping did.
pub(crate) struct Turn {
    pub(crate) stepped: usize,
    /// Inputs are still waiting.
    pub(crate) more: bool,
    /// The core has ended and its store is closed.
    pub(crate) ended: bool,
}

impl Slot {
    fn step(&self, most: usize) -> Turn {
        let mut core = self.core.lock().unwrap();
        let Some(thread) = core.as_mut() else {
            return Turn {
                stepped: 0,
                more: false,
                ended: true,
            };
        };
        *self.stepping.lock().unwrap() = Some(std::thread::current().id());
        let turn = {
            let _entered = self.handle.enter();
            thread.step_waiting(most)
        };
        *self.stepping.lock().unwrap() = None;
        if turn.ended {
            *core = None;
            self.ended.store(true, Ordering::SeqCst);
        }
        turn
    }

    /// Ends the core now, closing its store, unless the calling thread is
    /// the one stepping it, which ends it once its turn is over.
    pub(crate) fn end(&self) {
        if *self.stepping.lock().unwrap() == Some(std::thread::current().id()) {
            return;
        }
        let mut core = self.core.lock().unwrap();
        self.ended.store(true, Ordering::SeqCst);
        drop(core.take());
    }
}
