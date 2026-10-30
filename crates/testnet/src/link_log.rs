//! The link log as the net reads it. An in-process host writes its lines
//! into this process's trace, which the net captures here; a process host
//! writes them to its daemon's log, which the net reads. Either way a line
//! is read back by `node::link_log::LinkEvent::parse`, or `Rebind::parse`
//! for a network change, as a person reading a log would read it.
//!
//! The steps of an in-process host's clean stop are captured beside it,
//! so a stop that hangs is reported at the step it is stuck in.

use std::collections::HashMap;
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

/// The step each in-process host's clean stop last began, by the host the
/// stop's span names.
static STOP_STEPS: Mutex<Option<HashMap<HostId, String>>> = Mutex::new(None);

/// The span a host's clean stop runs in, which ties the steps it logs to
/// the host.
const STOPPING: &str = "testnet_stopping";

/// Captures the link log from this process's trace from now on, unless
/// something else already owns the process's subscriber; that one should
/// carry [`layer`]. The rest of the in-process hosts' trace is printed to
/// stderr when RUST_LOG asks, which the test runner keeps for a test that
/// failed.
pub(crate) fn install() {
    INSTALL.call_once(|| {
        let printed = std::env::var_os("RUST_LOG").is_some().then(|| {
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(tracing_subscriber::EnvFilter::from_default_env())
        });
        let _ = tracing_subscriber::registry()
            .with(printed)
            .with(layer())
            .try_init();
    });
}

/// The layer that captures the link log, for a process that sets up its
/// own subscriber, such as the net's door.
pub fn layer<S>() -> impl Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    Capture
        .with_filter(Targets::new().with_target(TARGET, LevelFilter::INFO))
        .and_then(
            StopSteps.with_filter(Targets::new().with_target(node::STOP_TARGET, LevelFilter::INFO)),
        )
}

/// The span to run `host`'s clean stop in, so [`stop_step`] can say where
/// it got to.
pub(crate) fn stopping(host: HostId) -> tracing::Span {
    tracing::info_span!(target: node::STOP_TARGET, STOPPING, host = %host)
}

/// The step `host`'s clean stop last began, if it began one.
pub(crate) fn stop_step(host: HostId) -> Option<String> {
    STOP_STEPS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .as_ref()?
        .get(&host)
        .cloned()
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

struct StopSteps;

/// The host a [`STOPPING`] span names.
struct Stopping(HostId);

impl<S> Layer<S> for StopSteps
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        if attrs.metadata().name() != STOPPING {
            return;
        }
        let mut line = Line::default();
        attrs.record(&mut line);
        let host = line
            .fields
            .trim()
            .strip_prefix("host=")
            .and_then(|host| host.parse().ok());
        if let (Some(host), Some(span)) = (host, ctx.span(id)) {
            span.extensions_mut().insert(Stopping(host));
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        let Some(host) = ctx.event_scope(event).and_then(|mut scope| {
            scope.find_map(|span| span.extensions().get::<Stopping>().map(|it| it.0))
        }) else {
            return;
        };
        let mut line = Line::default();
        event.record(&mut line);
        STOP_STEPS
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get_or_insert_default()
            .insert(host, line.message);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_is_read_at_the_step_it_last_began() {
        let host = HostId::new_v4();
        let other = HostId::new_v4();
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            stopping(host).in_scope(|| {
                tracing::info!(target: node::STOP_TARGET, "closing direct links");
                tracing::info!(target: node::STOP_TARGET, "stopping the relay link");
            });
            tracing::info!(target: node::STOP_TARGET, "said outside any stop");
        });
        assert_eq!(stop_step(host).as_deref(), Some("stopping the relay link"));
        assert_eq!(stop_step(other), None);
    }
}
