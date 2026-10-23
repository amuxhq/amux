//! The link log as the net reads it. An in-process host writes its lines
//! into this process's trace, which the net captures here; a process host
//! writes them to its daemon's log, which the net reads. Either way a line
//! is read back by `node::link_log::LinkEvent::parse`, or `Rebind::parse`
//! for a network change, as a person reading a log would read it.

use std::fmt::Write as _;
use std::sync::{Mutex, Once};

use node::HostId;
use node::link_log::{LinkEvent, Rebind, TARGET};
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

/// Every link log line this process's in-process hosts wrote, spelled as
/// a daemon's log spells it. Each names the host that wrote it, and hosts'
/// ids are unique, so nets sharing the process never read each other's.
static CAPTURED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static INSTALL: Once = Once::new();

/// Captures the link log from this process's trace from now on, unless
/// something else already owns the process's subscriber; that one should
/// carry [`layer`].
pub(crate) fn install() {
    INSTALL.call_once(|| {
        let _ = tracing_subscriber::registry().with(layer()).try_init();
    });
}

/// The layer that captures the link log, for a process that sets up its
/// own subscriber, such as the net's door.
pub fn layer<S>() -> impl Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    Capture.with_filter(Targets::new().with_target(TARGET, LevelFilter::INFO))
}

/// What `host`, an in-process host or a phone's profile in this process,
/// wrote to the link log about its links.
pub fn captured(host: HostId) -> Vec<LinkEvent> {
    read(&lines(), host)
}

/// The rebind lines `host`, a host or a phone's profile in this process,
/// wrote when its network changed.
pub fn rebinds(host: HostId) -> Vec<Rebind> {
    lines()
        .lines()
        .filter_map(Rebind::parse)
        .filter(|(writer, _)| *writer == host)
        .map(|(_, rebind)| rebind)
        .collect()
}

fn lines() -> String {
    CAPTURED
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .join("\n")
}

/// What a daemon's log holds of `host`'s link log.
pub(crate) fn read(log: &str, host: HostId) -> Vec<LinkEvent> {
    log.lines()
        .filter_map(LinkEvent::parse)
        .filter(|(writer, _)| *writer == host)
        .map(|(_, event)| event)
        .collect()
}

struct Capture;

impl<S: tracing::Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        let mut line = Line::default();
        event.record(&mut line);
        CAPTURED
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(format!("{TARGET}: {} {}", line.message, line.fields));
    }
}

/// An event's fields spelled as the daemon's log spells them.
#[derive(Default)]
struct Line {
    message: String,
    fields: String,
}

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}
