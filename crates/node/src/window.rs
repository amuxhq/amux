//! What a driver keeps beside a core that has no checkpoint of its own, so
//! a dump the core asks for leaves something on disk that replays: the
//! core's state when the window began, every input stepped since and the
//! state now, written as one JSON file. `just repro` steps the core from
//! the start through the inputs and names the first invariant broken.

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// The inputs a window holds at least, once that many were stepped: a
/// window keeps two halves of this many and starts a new half from the
/// state as it stands when the newer fills, so it costs one clone of the
/// core's state per this many inputs and always reaches back at least this
/// far. Enough for the run-up to a broken invariant on a busy relay, which
/// steps a few inputs per stream.
pub(crate) const WINDOW_INPUTS: usize = 256;

#[derive(Clone)]
struct Half<S, I> {
    start: S,
    inputs: Vec<I>,
}

/// A core's recent history: the state its older half began from and every
/// input since. A clone is what a driver holding its core under a lock
/// takes, to render after letting go.
#[derive(Clone)]
pub(crate) struct Window<S, I> {
    older: Option<Half<S, I>>,
    newer: Option<Half<S, I>>,
}

impl<S, I> Default for Window<S, I> {
    fn default() -> Self {
        Self {
            older: None,
            newer: None,
        }
    }
}

impl<S: Clone + Serialize, I: Clone + Serialize> Window<S, I> {
    /// Records `input` as it is about to be stepped onto `now`.
    pub(crate) fn record(&mut self, now: &S, input: &I) {
        match &mut self.newer {
            Some(half) if half.inputs.len() < WINDOW_INPUTS => half.inputs.push(input.clone()),
            _ => {
                self.older = self.newer.take();
                self.newer = Some(Half {
                    start: now.clone(),
                    inputs: vec![input.clone()],
                });
            }
        }
    }

    /// The file: the core (`relay` or `supervisor`), this build, the
    /// reason, the state the window began from (null before any input),
    /// each input since and the state now.
    pub(crate) fn render(&self, core: &str, reason: &str, now: &S) -> String {
        let inputs: Vec<&I> = self
            .older
            .iter()
            .chain(self.newer.iter())
            .flat_map(|half| half.inputs.iter())
            .collect();
        let start = self
            .older
            .as_ref()
            .or(self.newer.as_ref())
            .map(|half| &half.start);
        let window = serde_json::json!({
            "core": core,
            "amux": crate::version(),
            "reason": reason,
            "start": start,
            "inputs": inputs,
            "now": now,
        });
        serde_json::to_string_pretty(&window).expect("a core's state writes as JSON")
    }
}

/// Writes `text` to `<dir>/<stem>-<unix ms>-<id>.json` and returns the path.
pub(crate) fn write(dir: &Path, stem: &str, text: &str) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    let id = uuid::Uuid::new_v4().simple().to_string();
    let path = dir.join(format!("{stem}-{now_ms}-{}.json", &id[..8]));
    std::fs::write(&path, text)?;
    if let Err(error) = crate::install::prune_reports(dir) {
        tracing::warn!(%error, "cannot prune the reports");
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_reaches_back_at_least_its_inputs_and_starts_where_they_did() {
        let mut window: Window<u64, u64> = Window::default();
        let mut state = 0;
        for input in 1..=(2 * WINDOW_INPUTS as u64 + 10) {
            window.record(&state, &input);
            state += input;
        }
        let text = window.render("test", "a reason", &state);
        let written: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(written["core"], "test");
        assert_eq!(written["reason"], "a reason");
        // The older half began at input 257 of 522: the state then was the
        // sum of the 256 before it.
        let start = (1..=WINDOW_INPUTS as u64).sum::<u64>();
        assert_eq!(written["start"], start);
        let inputs: Vec<u64> = serde_json::from_value(written["inputs"].clone()).unwrap();
        assert_eq!(inputs.len(), 266);
        assert_eq!(inputs[0], 257);
        assert_eq!(written["now"], state);
        let replayed =
            start + (WINDOW_INPUTS as u64 + 1..=2 * WINDOW_INPUTS as u64 + 10).sum::<u64>();
        assert_eq!(
            replayed, state,
            "the start plus the inputs is the state now"
        );
    }
}
