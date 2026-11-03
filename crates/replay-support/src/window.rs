//! Replaying the report a relay or a supervisor writes when its core asks
//! for one: the core's state when its driver's window began, every input
//! stepped since and the state now. The core is restored at the start and
//! stepped through the inputs; the first input after which one of its own
//! invariants broke is the step that broke, and a replay that holds must
//! end in the state the report ends with.

use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::replay::{Failing, Stage, StageVerdict, Verdict};

/// What a replay that ends elsewhere than the report breaks.
pub const ENDS_AS_WRITTEN: &str = "the window's start and every input since step to the state the \
     report ends with";

/// Whether `path` is a core's window report: JSON naming its core.
pub fn is_window(path: &Path) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|written| stage(&written).is_some())
}

fn stage(written: &Value) -> Option<Stage> {
    match written["core"].as_str()? {
        "relay" => Some(Stage::Relay),
        "supervisor" => Some(Stage::Supervisor),
        _ => None,
    }
}

/// Replays the window report at `path` through the core it names.
pub fn replay(path: &Path) -> Result<StageVerdict, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let written: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{}: not a core's report: {error}", path.display()))?;
    let Some(stage) = stage(&written) else {
        return Err(format!(
            "{}: names no core this build replays: {}",
            path.display(),
            written["core"]
        ));
    };
    let verdict = match stage {
        Stage::Relay => window(&written, |relay: &mut relay_core::Relay, input| {
            let mut out = Vec::new();
            relay.step(input, &mut out);
            out.into_iter().find_map(|output| match output {
                relay_core::Output::Dump {
                    reason: daemon_core::DumpReason::Violation(violation),
                } => Some(violation),
                _ => None,
            })
        }),
        _ => window(
            &written,
            |supervisor: &mut supervisor_core::Supervisor, input| {
                let mut out = Vec::new();
                supervisor.step(input, &mut out);
                out.into_iter().find_map(|output| match output {
                    supervisor_core::Output::Dump {
                        reason: supervisor_core::DumpReason::Violation(violation),
                    } => Some(violation),
                    _ => None,
                })
            },
        ),
    };
    Ok(StageVerdict {
        stage,
        verdict: verdict.unwrap_or_else(Verdict::Unreadable),
    })
}

/// Steps a core of state `S` restored from the window's start through its
/// inputs of type `I`, judging the core's own invariants after each. A step
/// over its budget is the replaying machine's timing, not the core's, so
/// only invariants count.
fn window<S, I>(
    written: &Value,
    step: impl Fn(&mut S, I) -> Option<core_ring::Violation>,
) -> Result<Verdict, String>
where
    S: DeserializeOwned + PartialEq,
    I: DeserializeOwned + std::fmt::Debug,
{
    let read = |field: &str| -> Result<Value, String> {
        written
            .get(field)
            .cloned()
            .ok_or_else(|| format!("the report has no {field}"))
    };
    let start = read("start")?;
    if start.is_null() {
        return Ok(Verdict::Absent(
            "the core had stepped no input when it asked".to_owned(),
        ));
    }
    let mut state: S =
        serde_json::from_value(start).map_err(|error| format!("its start: {error}"))?;
    let inputs: Vec<I> =
        serde_json::from_value(read("inputs")?).map_err(|error| format!("its inputs: {error}"))?;
    let now: S =
        serde_json::from_value(read("now")?).map_err(|error| format!("its state now: {error}"))?;
    let count = inputs.len();
    for (at, input) in inputs.into_iter().enumerate() {
        let what = format!("{input:?}");
        if let Some(violation) = step(&mut state, input) {
            return Ok(Verdict::Fails(Failing {
                step: at + 1,
                what,
                property: violation.invariant.sentence,
                detail: violation.detail,
            }));
        }
    }
    if state != now {
        return Ok(Verdict::Fails(Failing {
            step: count,
            what: "the window's last input".to_owned(),
            property: ENDS_AS_WRITTEN,
            detail: "the replay ends in another state".to_owned(),
        }));
    }
    Ok(Verdict::Holds(format!(
        "{count} inputs from the window's start, each step judged, ending as the report does"
    )))
}
