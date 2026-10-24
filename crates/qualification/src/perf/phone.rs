//! The phone's performance workload: the served network `just ios perf`
//! measures the iPhone app against, generated here so the runner, the
//! served topology file and this description are one thing.
//!
//! Three machines on the phone's local network, forty agents across them
//! for a fleet a person could plausibly hold, and one agent whose
//! conversation is already a thousand rows long when it streams on cue.
//! The phone pairs with every machine by the link it prints; the runner
//! (`scripts/ios-perf.py`) says what is measured over each part and
//! against which budget.

use provider_fakes::script::{Outcome, Script, Step, Tool, ToolClass};
use testnet::{AgentDecl, HostDecl, Topology};

/// The machines the phone pairs with, every one with a LAN listener.
pub const HOSTS: [&str; 3] = ["desk", "laptop", "studio"];
/// The relay account every machine, and the phone away from home, signs in to.
pub const ACCOUNT: &str = "ada";
/// The agents the fleet holds, spread over the machines.
pub const FLEET_AGENTS: usize = 40;
/// The agent that streams on cue, on the first machine, into a
/// conversation already `LONG_ROWS` long: the transcript is measured with
/// history behind it, as a person's is.
pub const STREAM_AGENT: &str = "stream";
/// The rows its turn writes before the first cue.
pub const LONG_ROWS: usize = 1_000;
/// How many streamed samples a run can take: each waits for its own gate.
pub const STREAM_SAMPLES: usize = 5;
/// Rows per streamed sample, and the pause between them: 50 rows a second
/// for 20 seconds, the rate the transcript is held to.
pub const STREAM_ROWS: usize = 1_000;
pub const STREAM_PACE_MS: u64 = 20;

/// The gate the runner opens to start streamed sample `at`.
pub fn stream_gate(at: usize) -> String {
    format!("stream-{at}")
}

/// A fleet agent's name.
pub fn fleet_agent(at: usize) -> String {
    format!("agent-{at:02}")
}

/// The machine fleet agent `at` runs on: dealt round the machines.
pub fn fleet_host(at: usize) -> &'static str {
    HOSTS[at % HOSTS.len()]
}

/// A small deterministic generator, so two machines measure the same rows
/// without shipping a fixture.
struct Dealer(u64);

impl Dealer {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// The conversation's history: a mix of prose with markdown, tool calls of
/// both classes with short and long outputs, and thinking, dealt from a
/// fixed seed. Percentages are of rows: 55 prose, 20 short tool calls, 10
/// exploring reads, 5 commands with output over two hundred lines, 5 edits,
/// 5 thinking.
pub fn long_rows(rows: usize) -> Vec<Step> {
    let mut dealer = Dealer(1);
    let mut steps = Vec::with_capacity(rows);
    for at in 0..rows {
        let draw = dealer.below(100);
        let step = if draw < 55 {
            Step::Text {
                chunks: vec![prose(at, dealer.below(4))],
            }
        } else if draw < 75 {
            Step::Tool(Tool {
                name: Some("Grep".to_owned()),
                class: ToolClass::Exploration,
                input: Some(
                    serde_json::json!({ "pattern": format!("needle_{at}"), "path": "src" }),
                ),
                outcome: Outcome {
                    output: format!("src/lib.rs:{}: let needle_{at} = {};\n", at % 900 + 1, at),
                    error: false,
                    pieces: None,
                    ms: None,
                },
                wait_for: None,
            })
        } else if draw < 85 {
            Step::Tool(Tool {
                name: Some("Read".to_owned()),
                class: ToolClass::Exploration,
                input: Some(
                    serde_json::json!({ "file_path": format!("src/module_{}.rs", at % 40) }),
                ),
                outcome: Outcome {
                    output: (1..=40)
                        .map(|line| format!("{line:>6}\tfn item_{at}_{line}() {{ /* body */ }}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    error: false,
                    pieces: None,
                    ms: None,
                },
                wait_for: None,
            })
        } else if draw < 90 {
            Step::Tool(Tool {
                name: Some("Bash".to_owned()),
                class: ToolClass::Consequential,
                input: Some(
                    serde_json::json!({ "command": format!("cargo test -p crate_{}", at % 7) }),
                ),
                outcome: Outcome {
                    output: (1..=220)
                        .map(|line| format!("test case_{at}_{line} ... ok"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    error: false,
                    pieces: None,
                    ms: None,
                },
                wait_for: None,
            })
        } else if draw < 95 {
            Step::Tool(Tool {
                name: Some("Edit".to_owned()),
                class: ToolClass::Consequential,
                input: Some(serde_json::json!({
                    "file_path": format!("src/module_{}.rs", at % 40),
                    "old_string": format!("let value = {};", at),
                    "new_string": format!("let value = {};", at + 1),
                })),
                outcome: Outcome {
                    output: "The file has been updated.".to_owned(),
                    error: false,
                    pieces: None,
                    ms: None,
                },
                wait_for: None,
            })
        } else {
            Step::Thinking {
                text: format!(
                    "Row {at}: weighing whether the change belongs here or one module over; \
                     the caller already handles the empty case, so the guard can go."
                ),
            }
        };
        steps.push(step);
    }
    steps
}

/// A prose row with the markdown a real answer carries: a heading, a list,
/// a code span or plain sentences.
fn prose(at: usize, shape: u64) -> String {
    match shape {
        0 => format!(
            "## Step {at}\n\nThe change is in `crates/node/src/runtime.rs`: the batch is committed \
             before the fan-out, so a subscriber never sees a revision the store lacks."
        ),
        1 => format!(
            "Row {at}: three things to check before this lands:\n\n- the journal ends at a whole frame\n\
             - the cursor advanced past it\n- the boundary was written once"
        ),
        2 => format!(
            "Row {at}: the ingest cost is about 23 µs a frame, so `INGEST_BATCH` holds the lock \
             for **6 ms** at most; that is the bound the number comes from, not the ring."
        ),
        _ => format!(
            "Row {at}: reading the relay logs first, then the daemon's, to see which side closed \
             the link. The timestamps will say whether the close preceded the retry."
        ),
    }
}

/// The streaming agent's one turn: `LONG_ROWS` of history, then
/// `STREAM_SAMPLES` bursts, each waiting on its gate, each `STREAM_ROWS`
/// numbered rows `STREAM_PACE_MS` apart.
pub fn stream_script() -> Script {
    let mut steps = long_rows(LONG_ROWS);
    for at in 0..STREAM_SAMPLES {
        steps.push(Step::WaitFor {
            path: stream_gate(at).into(),
        });
        steps.push(Step::Repeat {
            times: STREAM_ROWS,
            steps: vec![
                Step::Text {
                    chunks: vec![format!(
                        "sample {at} message {{pass}}: the agent reports progress on its task"
                    )],
                },
                Step::Pause { ms: STREAM_PACE_MS },
            ],
            played: 0,
        });
    }
    steps.push(Step::TurnEnd);
    Script {
        steps,
        ..Script::default()
    }
}

/// A fleet agent says one thing and rests.
fn fleet_script(at: usize) -> Script {
    Script {
        steps: vec![
            Step::Text {
                chunks: vec![format!("Agent {at} is ready and waiting for work.")],
            },
            Step::TurnEnd,
        ],
        ..Script::default()
    }
}

/// The served topology `just ios perf` drives.
pub fn topology() -> Topology {
    // Every machine signs in to one account at the served relay, so the
    // phone can reach them through it as it does away from home.
    let mut topology = Topology::new().relay(&[ACCOUNT]);
    for host in HOSTS {
        topology = topology.host_decl(HostDecl {
            name: host.to_owned(),
            lan: true,
            account: Some(ACCOUNT.to_owned()),
            ..HostDecl::default()
        });
    }
    for at in 0..FLEET_AGENTS {
        let mut agent = AgentDecl::new(&fleet_agent(at), fleet_host(at)).prompt("Get ready.");
        agent.script = Some(fleet_script(at));
        topology = topology.agent(agent);
    }
    let mut stream = AgentDecl::new(STREAM_AGENT, HOSTS[0])
        .prompt("Work through the module and report as you go.");
    stream.script = Some(stream_script());
    topology.agent(stream)
}
