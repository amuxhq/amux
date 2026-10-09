//! The driver trace: what a driver saw, in the order it saw it, with the
//! model as it stood when the trace began, so a dump replays the
//! transitions instead of only showing where they ended. That order is the
//! one thing the runtime's rows cannot reproduce. Memory only and bounded.

/// Events a trace keeps: between half this and this many.
pub const TRACE_EVENTS: usize = 400;

/// One traced event.
#[derive(Clone, Debug, PartialEq)]
pub struct Traced<M> {
    /// Position in everything this driver saw since it opened.
    pub seq: u64,
    /// When the driver saw it, on the driver's clock.
    pub at_ms: i64,
    pub event: TraceEvent<M>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TraceEvent<M> {
    /// A message the driver applied to its model.
    Msg(M),
    /// Something the driver did that is not a model message.
    Driver(DriverEvent),
}

/// The driver's own acts, recorded between the messages they caused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriverEvent {
    Subscribed {
        tail: u32,
    },
    SubscribeFailed {
        error: String,
    },
    /// The stream ended; `error` when the transport failed rather than the
    /// runtime closing it.
    StreamEnded {
        error: Option<String>,
    },
    /// The runtime closed the stream after Lagged: reopened at once (a
    /// session with a tail).
    Retail,
    /// The reader returned after the head moved on: reopened with a tail
    /// that builds the window apart.
    Reload,
    Backoff {
        until_ms: i64,
    },
    Get {
        key: String,
    },
    GetFailed {
        key: String,
        error: String,
    },
    Page {
        before: Option<u64>,
        limit: u32,
    },
    PageFailed {
        error: String,
    },
    Blob {
        hash: Vec<u8>,
    },
    /// The chat on screen names a catalogue this client does not hold.
    Catalogue {
        hash: Vec<u8>,
    },
    CatalogueFailed {
        hash: Vec<u8>,
        error: String,
    },
    /// The window changed: widened for a chat on screen, or narrowed to
    /// the few rows the fleet holds off screen.
    Window {
        tail: u32,
        cap: u32,
    },
    /// The phone left the foreground: the stream was dropped.
    Suspended,
    /// Back in the foreground: reopened with a tail.
    Resumed,
    /// The runtime refused to serve the stream again; the driver stopped.
    Ended {
        error: String,
    },
}

/// A trace as a dump carries it: the model at the start of the oldest
/// segment kept, and every event since.
#[derive(Clone, Debug, PartialEq)]
pub struct DriverTrace<S, M> {
    pub start: S,
    pub events: Vec<Traced<M>>,
}

impl<S, M> DriverTrace<S, M> {
    /// The messages in order, for replaying onto `start`.
    pub fn msgs(&self) -> impl Iterator<Item = &M> {
        self.events.iter().filter_map(|traced| match &traced.event {
            TraceEvent::Msg(msg) => Some(msg),
            TraceEvent::Driver(_) => None,
        })
    }
}

/// A model or message as a dump may carry it: its structure, with no
/// content a person or an agent wrote.
pub trait Structure {
    fn structure(&self) -> String;
}

impl<S: Structure, M: Structure> DriverTrace<S, M> {
    /// The trace as a dump carries it: the starting model's structure, then
    /// one event per line.
    pub fn render(&self) -> String {
        let mut out = format!("start:\n{}\nevents:\n", self.start.structure());
        for traced in &self.events {
            let event = match &traced.event {
                TraceEvent::Msg(msg) => msg.structure(),
                TraceEvent::Driver(driver) => format!("{driver:?}"),
            };
            out.push_str(&format!("{} @{} {event}\n", traced.seq, traced.at_ms));
        }
        out
    }
}

struct Segment<S, M> {
    start: S,
    events: Vec<Traced<M>>,
}

/// Two segments of half the bound each: when the newer fills, it becomes
/// the older and a new one starts from the model as it stands, so the
/// trace always holds at least half the bound of recent history.
pub(crate) struct Ring<S, M> {
    half: usize,
    seq: u64,
    older: Option<Segment<S, M>>,
    newer: Segment<S, M>,
}

impl<S: Clone, M: Clone> Ring<S, M> {
    pub(crate) fn new(start: S) -> Ring<S, M> {
        Ring {
            half: TRACE_EVENTS / 2,
            seq: 0,
            older: None,
            newer: Segment {
                start,
                events: Vec::new(),
            },
        }
    }

    /// Records an event before it is applied to `now`, the model as it
    /// stands.
    pub(crate) fn record(&mut self, now: &S, at_ms: i64, event: TraceEvent<M>) {
        if self.newer.events.len() >= self.half {
            let fresh = Segment {
                start: now.clone(),
                events: Vec::with_capacity(self.half),
            };
            self.older = Some(std::mem::replace(&mut self.newer, fresh));
        }
        self.newer.events.push(Traced {
            seq: self.seq,
            at_ms,
            event,
        });
        self.seq += 1;
    }

    pub(crate) fn trace(&self) -> DriverTrace<S, M> {
        let (start, mut events) = match &self.older {
            Some(older) => (older.start.clone(), older.events.clone()),
            None => (self.newer.start.clone(), Vec::new()),
        };
        events.extend(self.newer.events.iter().cloned());
        DriverTrace { start, events }
    }
}
