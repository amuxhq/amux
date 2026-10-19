//! A simulated agent's provider: the authored script the real fakes play,
//! spoken as the test kind's facts.
//!
//! It keeps the real fakes' protocol: idle, it waits for a prompt or an
//! injected message, reflects it, and plays steps until a turn end; a
//! wait holds the turn until its gate opens, an ask until it is answered.
//! It says one fact at a time, so what it says interleaves with everything
//! else the net delivers. Steps the test kind has no fact for (thinking,
//! pauses, detached commands, usage, refused credentials) pass without a
//! word.
//!
//! It is a value: the script it plays is the net's, flattened once (a
//! repeat laid out pass by pass), and the provider keeps only its place in
//! it and what it has said and not yet been heard.

use std::collections::{BTreeSet, VecDeque};

use interpret::{Channel, Effect, Fact};
use provider_fakes::script::{Ask, Step, ToolClass};
use serde_json::{Value, json};

/// What the provider has said that the agent has not yet read: a fact, as
/// its encoded bytes, or its exit.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Said {
    Fact(Vec<u8>),
    Exit(Option<i32>),
}

/// What holds the turn.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Holding {
    /// A wait, until its gate opens; then `then` is said.
    Gate { gate: String, then: Vec<Vec<u8>> },
    /// An ask, until the host answers it.
    Answer { ask: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Provider {
    /// The next step of the script to play.
    at: usize,
    said: VecDeque<Said>,
    holding: Option<Holding>,
    /// Prompts handed while a turn ran, reflected once it ends.
    prompts: VecDeque<String>,
    turn: bool,
    alive: bool,
    /// Ids for what it says, so each item has its own key.
    next: u64,
}

fn fact(value: Value) -> Vec<u8> {
    serde_json::to_vec(&value).expect("a fact encodes")
}

impl Provider {
    /// A provider that has just started and says it is ready.
    pub(crate) fn start() -> Self {
        Self {
            at: 0,
            said: VecDeque::from([Said::Fact(fact(json!({"type": "ready"})))]),
            holding: None,
            prompts: VecDeque::new(),
            turn: false,
            alive: true,
            next: 0,
        }
    }

    /// Whether it has something to say now, with `gates` open.
    pub(crate) fn can_say(&self, gates: &BTreeSet<String>) -> bool {
        self.alive
            && (!self.said.is_empty()
                || matches!(&self.holding, Some(Holding::Gate { gate, .. }) if gates.contains(gate)))
    }

    /// The next thing it says, as the agent's driver feeds it.
    pub(crate) fn say(
        &mut self,
        script: &[Step],
        gates: &BTreeSet<String>,
    ) -> Option<agent_core::Input> {
        if !self.alive {
            return None;
        }
        if self.said.is_empty()
            && let Some(Holding::Gate { gate, .. }) = &self.holding
            && gates.contains(gate)
        {
            let Some(Holding::Gate { then, .. }) = self.holding.take() else {
                unreachable!("held at a gate")
            };
            self.said.extend(then.into_iter().map(Said::Fact));
            self.play(script);
        }
        match self.said.pop_front()? {
            Said::Fact(payload) => Some(agent_core::Input::Provider(Fact {
                channel: Channel::Stream,
                payload,
            })),
            Said::Exit(code) => {
                self.alive = false;
                self.said.clear();
                Some(agent_core::Input::ProviderExited { code })
            }
        }
    }

    /// What the agent hands it.
    pub(crate) fn take(&mut self, script: &[Step], effect: Effect) {
        if !self.alive {
            return;
        }
        match effect {
            Effect::ProviderWrite(bytes) => {
                let Ok(written) = serde_json::from_slice::<Value>(&bytes) else {
                    return;
                };
                match written["type"].as_str() {
                    Some("user") => {
                        let text = written["text"].as_str().unwrap_or_default().to_owned();
                        if self.turn {
                            self.prompts.push_back(text);
                        } else {
                            self.prompt(script, text);
                        }
                    }
                    Some("answer") => {
                        let ask = written["ask"].as_str().unwrap_or_default();
                        if matches!(&self.holding, Some(Holding::Answer { ask: held }) if held == ask)
                        {
                            self.holding = None;
                            self.said.push_back(Said::Fact(fact(
                                json!({"type": "ask_closed", "id": ask}),
                            )));
                            self.play(script);
                        }
                    }
                    Some("interrupt") if self.turn => self.end_turn_early(script),
                    _ => {}
                }
            }
            Effect::Inject { id, .. } => {
                let envelope = String::from_utf8_lossy(&id).into_owned();
                self.said.push_back(Said::Fact(fact(
                    json!({"type": "user", "envelope": envelope}),
                )));
                if !self.turn {
                    self.said
                        .push_back(Said::Fact(fact(json!({"type": "turn_start"}))));
                    self.turn = true;
                    self.play(script);
                }
            }
            _ => {}
        }
    }

    /// Asked to finish: what it already said stands, and it exits.
    pub(crate) fn close(&mut self) {
        if self.alive {
            self.holding = None;
            self.said.push_back(Said::Exit(Some(0)));
        }
    }

    /// Its process group is killed: it says nothing more.
    pub(crate) fn kill(&mut self) {
        self.alive = false;
        self.said.clear();
    }

    fn id(&mut self, what: &str) -> String {
        self.next += 1;
        format!("{what}:{}", self.next)
    }

    /// Reflects a prompt and runs a turn on it.
    fn prompt(&mut self, script: &[Step], text: String) {
        let id = self.id("user");
        self.said.push_back(Said::Fact(fact(
            json!({"type": "user", "id": id, "text": text}),
        )));
        self.turn = true;
        self.play(script);
    }

    /// An interrupt ends the turn where it stands; the rest of its steps
    /// are dropped.
    fn end_turn_early(&mut self, script: &[Step]) {
        self.holding = None;
        while let Some(step) = script.get(self.at) {
            self.at += 1;
            if *step == Step::TurnEnd {
                break;
            }
        }
        self.said
            .push_back(Said::Fact(fact(json!({"type": "turn_end"}))));
        self.turn_over(script);
    }

    fn turn_over(&mut self, script: &[Step]) {
        self.turn = false;
        if let Some(text) = self.prompts.pop_front() {
            self.prompt(script, text);
        }
    }

    /// Plays steps until the turn ends, a step holds it, or the steps run
    /// out, which leaves the turn open as a model still at work.
    fn play(&mut self, script: &[Step]) {
        while self.turn && self.holding.is_none() && self.alive {
            let Some(step) = script.get(self.at) else {
                return;
            };
            self.at += 1;
            match step {
                Step::Text { chunks } => {
                    let id = self.id("text");
                    for chunk in chunks {
                        self.said.push_back(Said::Fact(fact(
                            json!({"type": "text", "id": id, "delta": chunk}),
                        )));
                    }
                    self.said
                        .push_back(Said::Fact(fact(json!({"type": "text_done", "id": id}))));
                }
                Step::Tool(tool) => {
                    let id = self.id("tool");
                    let name = tool.name.clone().unwrap_or_else(|| match tool.class {
                        ToolClass::Exploration => "Read".to_owned(),
                        ToolClass::Consequential => "Bash".to_owned(),
                    });
                    let input = tool.input.clone().unwrap_or(Value::Null);
                    self.said.push_back(Said::Fact(fact(
                        json!({"type": "tool", "id": id, "name": name, "input": input}),
                    )));
                    let result = fact(
                        json!({"type": "tool_result", "id": id, "output": tool.outcome.text()}),
                    );
                    match &tool.wait_for {
                        Some(gate) => {
                            self.holding = Some(Holding::Gate {
                                gate: gate.to_string_lossy().into_owned(),
                                then: vec![result],
                            });
                        }
                        None => self.said.push_back(Said::Fact(result)),
                    }
                }
                Step::Ask(ask) => {
                    let id = self.id("ask");
                    let (tool, tool_name) = match ask {
                        Ask::Permission(tool) => (
                            id.clone(),
                            tool.name.clone().unwrap_or_else(|| "Bash".to_owned()),
                        ),
                        _ => (String::new(), String::new()),
                    };
                    self.said.push_back(Said::Fact(fact(
                        json!({"type": "ask", "id": id, "tool": tool, "tool_name": tool_name}),
                    )));
                    self.holding = Some(Holding::Answer { ask: id });
                }
                Step::WaitFor { path } => {
                    self.holding = Some(Holding::Gate {
                        gate: path.to_string_lossy().into_owned(),
                        then: Vec::new(),
                    });
                }
                Step::TurnEnd => {
                    self.said
                        .push_back(Said::Fact(fact(json!({"type": "turn_end"}))));
                    self.turn_over(script);
                }
                Step::Exit { code } => {
                    self.said.push_back(Said::Exit(Some(*code)));
                    self.turn = false;
                }
                // Laid out pass by pass when the script was flattened.
                Step::Repeat { .. } => {}
                Step::Thinking { .. }
                | Step::Pause { .. }
                | Step::Spawn { .. }
                | Step::Usage(_)
                | Step::AuthFailed { .. } => {}
            }
        }
    }
}

/// The script as the provider plays it: every repeat laid out pass by pass,
/// `{pass}` in a repeated text made the pass's number.
pub(crate) fn flatten(steps: &[Step]) -> Vec<Step> {
    let mut out = Vec::new();
    for step in steps {
        match step {
            Step::Repeat { times, steps, .. } => {
                for pass in 1..=*times {
                    for step in flatten_pass(steps, pass) {
                        out.push(step);
                    }
                }
            }
            other => out.push(other.clone()),
        }
    }
    out
}

/// One pass of a repeat: its steps numbered, a repeat inside it laid out
/// with its own passes.
fn flatten_pass(steps: &[Step], pass: usize) -> Vec<Step> {
    let mut out = Vec::new();
    for step in steps {
        match step {
            Step::Repeat { .. } => out.extend(flatten(std::slice::from_ref(step))),
            Step::Text { chunks } => out.push(Step::Text {
                chunks: chunks
                    .iter()
                    .map(|chunk| chunk.replace("{pass}", &pass.to_string()))
                    .collect(),
            }),
            other => out.push(other.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(provider: &mut Provider, script: &[Step], gates: &BTreeSet<String>) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(input) = provider.say(script, gates) {
            match input {
                agent_core::Input::Provider(fact) => {
                    let value: Value = serde_json::from_slice(&fact.payload).unwrap();
                    out.push(value["type"].as_str().unwrap().to_owned());
                }
                other => out.push(format!("{other:?}")),
            }
        }
        out
    }

    #[test]
    fn a_prompt_plays_its_turn_and_a_wait_holds_it_until_the_gate_opens() {
        let script = flatten(&[
            Step::Text {
                chunks: vec!["one".into()],
            },
            Step::WaitFor { path: "go".into() },
            Step::TurnEnd,
        ]);
        let mut provider = Provider::start();
        let mut gates = BTreeSet::new();
        assert_eq!(facts(&mut provider, &script, &gates), ["ready"]);
        provider.take(
            &script,
            Effect::ProviderWrite(br#"{"type":"user","text":"hi"}"#.to_vec()),
        );
        assert_eq!(
            facts(&mut provider, &script, &gates),
            ["user", "text", "text_done"]
        );
        assert!(!provider.can_say(&gates), "held at the gate");
        gates.insert("go".into());
        assert!(provider.can_say(&gates));
        assert_eq!(facts(&mut provider, &script, &gates), ["turn_end"]);
    }

    #[test]
    fn a_repeat_is_laid_out_pass_by_pass() {
        let script = flatten(&[Step::Repeat {
            times: 2,
            steps: vec![
                Step::Text {
                    chunks: vec!["pass {pass}".into()],
                },
                Step::TurnEnd,
            ],
            played: 0,
        }]);
        assert_eq!(
            script,
            [
                Step::Text {
                    chunks: vec!["pass 1".into()]
                },
                Step::TurnEnd,
                Step::Text {
                    chunks: vec!["pass 2".into()]
                },
                Step::TurnEnd,
            ]
        );
    }
}
