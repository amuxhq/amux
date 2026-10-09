//! Typing into Claude's terminal: the keymap resolved for the Claude this
//! incarnation launched turns the interpreter's semantic inputs into bytes,
//! and one task types them in order, with the pauses the keymap asks for.
//!
//! The keymap is resolved at every start against the version Claude
//! reports, so an updated Claude gets the right map without a new spec and
//! an edited keymap takes effect at the next start. The choice is recorded
//! on the boundary item through the launch fact.

use std::io;
use std::path::Path;

use claude::pty::keymap::{self, Environment, KeyStep, Keymap, ProgramName, Resolved};
use claude::pty::{
    AskAnswer, AskKind, PermissionAnswer, PlanAnswer, QuestionAnswer, QuestionFact,
    QuestionResponse,
};
use interpret::claude_pty::{PermissionChoice, PermissionMenus, PlanChoice, TerminalInput};
use tokio::sync::mpsc;

/// The keymap for the Claude this incarnation runs, and the version it was
/// resolved for.
pub struct Keys {
    pub version: String,
    resolved: Resolved,
    keymap: Keymap,
}

impl Keys {
    /// Resolves the keymap for the version `command --version` reports.
    /// None when Claude reports no version this build reads or no keymap
    /// covers it; the agent then runs without one and cannot type.
    pub async fn resolve(command: &Path) -> Option<Self> {
        let version = match claude::version::probe_version(command).await {
            Ok(version) => version,
            Err(error) => {
                eprintln!("amux agent: reading the Claude version: {error}");
                return None;
            }
        };
        match keymap::resolve_session(&keymap::KeymapSources::default(), &version) {
            Ok((resolved, keymap)) => Some(Self {
                version: version.to_string(),
                resolved,
                keymap,
            }),
            Err(error) => {
                eprintln!("amux agent: no keymap for Claude {version}: {error}");
                None
            }
        }
    }

    /// The name the boundary item records.
    pub fn name(&self) -> &str {
        &self.resolved.keymap.name
    }

    /// The permission menus this keymap can type an allowance on.
    pub fn permission_menus(&self) -> PermissionMenus {
        let shapes = self
            .keymap
            .verified_shapes
            .get(&ProgramName::PermissionMenu)
            .cloned()
            .unwrap_or_default();
        let counts = |counts: Vec<usize>| counts.into_iter().map(|count| count as u32).collect();
        PermissionMenus {
            per_suggestion: counts(shapes.permission_suggestions),
            folded: counts(shapes.permission_folded_suggestions),
        }
    }

    /// Why this keymap cannot type send now for this Claude, when it
    /// cannot.
    pub fn send_now_refused(&self) -> Option<String> {
        match self.resolved.stability_limits.get(&ProgramName::SendNow) {
            Some(keymap::Extrapolation::Refused { reason }) => Some(reason.clone()),
            Some(keymap::Extrapolation::Allowed) => None,
            None => Some("this Claude's keymap has no send now".to_owned()),
        }
    }

    /// The keystrokes for a semantic input.
    pub fn steps(&self, input: &TerminalInput, prompt_text: &str) -> io::Result<Vec<KeyStep>> {
        match input {
            TerminalInput::Prompt { .. } => self.prompt(prompt_text),
            TerminalInput::SendNow { .. } => keymap::encode(
                &self.keymap,
                &self.resolved,
                ProgramName::SendNow,
                &Environment {
                    ask: None,
                    answer: None,
                    prompt: Some(prompt_text),
                },
            )
            .map_err(io::Error::other),
            TerminalInput::Clear => self.prompt("/clear"),
            TerminalInput::Interrupt => self.encode(ProgramName::Interrupt, None, None),
            TerminalInput::CyclePermissionMode => self.encode(ProgramName::ModeCycle, None, None),
            TerminalInput::Permission {
                suggestions,
                choice,
            } => {
                let ask = AskKind::Permission {
                    tool_name: String::new(),
                    suggestions: *suggestions as usize,
                    is_plan: false,
                };
                let answer = AskAnswer::Permission(match choice {
                    PermissionChoice::AllowOnce => PermissionAnswer::AllowOnce,
                    PermissionChoice::AllowScoped { suggestion } => PermissionAnswer::AllowScoped {
                        suggestion: *suggestion as usize,
                    },
                    PermissionChoice::Deny { note } => PermissionAnswer::Deny {
                        feedback: (!note.is_empty()).then(|| note.clone()),
                    },
                });
                self.encode(ProgramName::PermissionMenu, Some(&ask), Some(&answer))
            }
            TerminalInput::Plan(choice) => {
                let ask = AskKind::Permission {
                    tool_name: "ExitPlanMode".to_owned(),
                    suggestions: 0,
                    is_plan: true,
                };
                let answer = AskAnswer::Plan(match choice {
                    PlanChoice::ApproveAutoAcceptEdits => PlanAnswer::ApproveAuto,
                    PlanChoice::Approve => PlanAnswer::ApproveManual,
                    PlanChoice::SendBack { note } => PlanAnswer::RequestChanges {
                        feedback: note.clone(),
                    },
                });
                self.encode(ProgramName::PlanMenu, Some(&ask), Some(&answer))
            }
            // The trust dialog preselects No: Enter exits, Down and Enter
            // trusts the folder.
            TerminalInput::Trust { trust } => {
                let key = |name| {
                    self.keymap
                        .keys
                        .get(&name)
                        .cloned()
                        .ok_or_else(|| unsupported(&format!("the keymap has no {name:?} key")))
                };
                let mut steps = Vec::new();
                if *trust {
                    let pause = self
                        .keymap
                        .delays
                        .get(&keymap::DelayName::AfterMove)
                        .copied()
                        .unwrap_or_default();
                    steps.push(KeyStep::Write(key(keymap::KeyName::Down)?));
                    steps.push(KeyStep::Delay(std::time::Duration::from_millis(u64::from(
                        pause,
                    ))));
                }
                steps.push(KeyStep::Write(key(keymap::KeyName::Enter)?));
                Ok(steps)
            }
            TerminalInput::Question { questions, answers } => {
                let ask = AskKind::Question {
                    questions: questions
                        .iter()
                        .map(|shape| QuestionFact {
                            options: shape.options as usize,
                            multi_select: shape.multi_select,
                            previews: shape.previews,
                        })
                        .collect(),
                };
                let answer = AskAnswer::Question(QuestionResponse {
                    answers: answers
                        .iter()
                        .map(|choice| QuestionAnswer {
                            selected: choice.selected.iter().map(|at| *at as usize).collect(),
                            other: choice.other.clone(),
                        })
                        .collect(),
                });
                self.encode(ProgramName::QuestionForm, Some(&ask), Some(&answer))
            }
        }
    }

    /// Pastes `text` and submits it.
    pub fn prompt(&self, text: &str) -> io::Result<Vec<KeyStep>> {
        keymap::encode(
            &self.keymap,
            &self.resolved,
            ProgramName::Prompt,
            &Environment {
                ask: None,
                answer: None,
                prompt: Some(text),
            },
        )
        .map_err(io::Error::other)
    }

    fn encode(
        &self,
        program: ProgramName,
        ask: Option<&AskKind>,
        answer: Option<&AskAnswer>,
    ) -> io::Result<Vec<KeyStep>> {
        keymap::encode(
            &self.keymap,
            &self.resolved,
            program,
            &Environment {
                ask,
                answer,
                prompt: None,
            },
        )
        .map_err(io::Error::other)
    }
}

fn unsupported(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, what.to_owned())
}

/// Starts the task that types into the terminal. Keystrokes go in the
/// order they were handed over; a pause holds everything after it without
/// holding the agent.
pub fn typist(terminal: pty_host::PtyHandle) -> mpsc::UnboundedSender<Vec<KeyStep>> {
    let (keys, mut typed) = mpsc::unbounded_channel::<Vec<KeyStep>>();
    tokio::spawn(async move {
        while let Some(steps) = typed.recv().await {
            for step in steps {
                match step {
                    KeyStep::Write(bytes) => {
                        if terminal.write(&bytes).await.is_err() {
                            return;
                        }
                    }
                    KeyStep::Delay(pause) => tokio::time::sleep(pause).await,
                }
            }
        }
    });
    keys
}

/// A message pasted into the terminal when the messaging socket cannot
/// take it, wrapped as Claude wraps what arrives on the socket, so the
/// interpreter reads its reflection the same way.
pub fn pasted_message(text: &str) -> String {
    format!("<cross-session-message from=\"amux\">\n{text}\n</cross-session-message>")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use interpret::claude_pty::{QuestionChoice, QuestionShape};

    use super::*;

    fn keys() -> Keys {
        let version = "2.1.283".parse().expect("a version");
        let (resolved, keymap) =
            keymap::resolve_session(&keymap::KeymapSources::default(), &version).expect("a keymap");
        Keys {
            version: version.to_string(),
            resolved,
            keymap,
        }
    }

    /// Claude draws a question with previews side by side, where a digit
    /// only moves the cursor: the pick is confirmed with Enter.
    #[test]
    fn a_pick_on_a_question_with_previews_is_confirmed_with_enter() {
        let input = TerminalInput::Question {
            questions: vec![QuestionShape {
                options: 4,
                multi_select: false,
                previews: true,
            }],
            answers: vec![QuestionChoice {
                selected: vec![2],
                other: None,
            }],
        };
        assert_eq!(
            keys().steps(&input, "").expect("keys"),
            [
                KeyStep::Write(b"3".to_vec()),
                KeyStep::Delay(Duration::from_millis(300)),
                KeyStep::Write(b"\r".to_vec()),
            ]
        );
    }
}
