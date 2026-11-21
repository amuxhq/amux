//! An ACP agent as the agent process hosts it, against fake-acp: its
//! command over stdio, the handshake, prompts, asks, the queue, a cancel,
//! reopening its session in a later process or ending for good when it
//! cannot, sign-in, attachments within what it takes, and amux's own tools
//! allowed without asking.

use agent::ExitCause;
use provider_fakes::{AcpAgent, Call, Offer, Outcome, Step, ToolServerCall};
use support::*;
use wire::Kind;

use crate::support;

fn acp(provider: &'static str, steps: Vec<Step>) -> Setup {
    Setup {
        kind: Kind::Acp,
        provider,
        steps,
        ..Setup::sdk()
    }
}

fn said(text: &str) -> Vec<Step> {
    vec![
        Step::Text {
            chunks: vec![text.into()],
        },
        Step::TurnEnd,
    ]
}

/// The lines the agent wrote the fake that call `method`.
fn sent(agent: &Agent, method: &str) -> Vec<serde_json::Value> {
    agent
        .provider_input()
        .into_iter()
        .filter(|line| line["method"] == method)
        .collect()
}

fn input(id: &[u8], of: wire::acp_input::Of) -> wire::Input {
    wire::Input::new(id.to_vec(), wire::AcpInput { of: Some(of) })
}

/// How each turn the journal ended came out.
fn outcomes(log: &Log) -> Vec<wire::TurnOutcome> {
    log.steps
        .iter()
        .filter_map(|step| step.turn_end.as_ref())
        .map(|end| end.outcome())
        .collect()
}

fn interpreter_cause(cause: ExitCause) -> String {
    match cause {
        ExitCause::Interpreter(cause) => cause,
        other => panic!("the interpreter did not end the incarnation: {other:?}"),
    }
}

/// The agent runs the spec's command over stdio and does the handshake:
/// `initialize` offering forms and links and no files or terminals, then
/// `session/new` in the agent's folder with amux's tool server; the session
/// the agent names is kept for the next process. A prompt runs a turn. No
/// terminal or hook socket is bound for it.
#[tokio::test(flavor = "multi_thread")]
async fn an_acp_agent_handshakes_and_runs_a_turn() {
    let agent = Agent::start(acp("opencode", said("hello"))).await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "hi").await, Verdict::Accepted);
    agent
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    assert!(agent.log().has_text("hello"));
    assert_eq!(outcomes(&agent.log()), [wire::TurnOutcome::Completed]);

    let initialize = &sent(&agent, "initialize")[0];
    assert_eq!(initialize["id"], interpret::acp::INITIALIZE_REQUEST);
    let capabilities = &initialize["params"]["clientCapabilities"];
    assert_eq!(
        capabilities,
        &serde_json::json!({"elicitation": {"form": {}, "url": {}}}),
        "{initialize}"
    );
    let new = &sent(&agent, "session/new")[0];
    assert_eq!(new["params"]["cwd"], agent.work().to_str().unwrap());
    let server = &new["params"]["mcpServers"][0];
    assert_eq!(server["name"], "amux");
    assert_eq!(server["command"], install_path());
    assert_eq!(
        server["args"],
        serde_json::json!(["internal", "mcp", agent.dir.to_str().unwrap()])
    );
    let session = agent.provider_session();
    assert!(!session.is_empty());
    assert_eq!(
        agent.log().acp().session_id.as_deref(),
        Some(session.as_str())
    );
    assert!(!agent.dir.join(agent::PTY_SOCK).exists());
    assert!(
        !agent
            .dir
            .join(agent::PRIVATE)
            .join(agent::HOOKS_SOCK)
            .exists()
    );

    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
    agent.assert_released();
}

/// A call the agent asks about opens an ask offering exactly its options;
/// the person's answer goes back by the option's id and the call runs.
#[tokio::test(flavor = "multi_thread")]
async fn a_permission_is_asked_and_answered() {
    let agent = Agent::start(acp(
        "antigravity",
        vec![
            Step::Call(Call {
                kind: "execute".into(),
                title: "./make-file.sh".into(),
                name: None,
                input: None,
                locations: Vec::new(),
                diff: None,
                ask: Some(Offer::Once),
                outcome: Outcome {
                    output: "made".into(),
                    ..Outcome::default()
                },
            }),
            Step::TurnEnd,
        ],
    ))
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "make it").await, Verdict::Accepted);
    agent
        .wait("the ask opens", |log| !log.ask_keys().is_empty())
        .await;
    let ask = agent.log().ask_keys()[0].clone();
    let answer = input(
        b"a1",
        wire::acp_input::Of::Answer(wire::AcpAnswer {
            ask_key: ask,
            of: Some(wire::acp_answer::Of::Option("allow".into())),
        }),
    );
    assert_eq!(daemon.input(answer).await, Verdict::Accepted);
    agent
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    assert!(agent.log().ask_keys().is_empty());
    assert!(agent.log().has_text("made"));
    let answered = agent
        .provider_input()
        .into_iter()
        .find(|line| line["result"]["outcome"]["outcome"] == "selected")
        .expect("the permission's answer");
    assert_eq!(answered["result"]["outcome"]["optionId"], "allow");
    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
}

/// An interrupt cancels the turn the ACP way, a `session/cancel`, and the
/// turn ends interrupted at the agent's `cancelled` answer; the agent then
/// takes the next prompt.
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupt_cancels_the_turn() {
    let agent = Agent::start(acp(
        "grok",
        vec![
            Step::Text {
                chunks: vec!["working".into()],
            },
            Step::WaitFor {
                path: "@release".into(),
            },
            Step::TurnEnd,
            Step::Text {
                chunks: vec!["again".into()],
            },
            Step::TurnEnd,
        ],
    ))
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "go").await, Verdict::Accepted);
    agent
        .wait("the turn streams", |log| log.has_text("working"))
        .await;
    let interrupt = input(b"i1", wire::acp_input::Of::Interrupt(wire::Interrupt {}));
    assert_eq!(daemon.input(interrupt).await, Verdict::Accepted);
    agent
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    assert_eq!(outcomes(&agent.log()), [wire::TurnOutcome::Interrupted]);
    assert_eq!(sent(&agent, "session/cancel").len(), 1);
    agent.release();
    assert_eq!(daemon.prompt(b"p2", "go on").await, Verdict::Accepted);
    agent
        .wait("the next turn ends", |log| log.turn_ends() == 2)
        .await;
    assert!(agent.log().has_text("again"));
    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
}

/// A prompt sent while a turn runs waits in amux's queue and goes to the
/// agent only once the running prompt is answered.
#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_sent_mid_turn_waits_for_the_answer() {
    let agent = Agent::start(acp(
        "opencode",
        vec![
            Step::WaitFor {
                path: "@release".into(),
            },
            Step::Text {
                chunks: vec!["one".into()],
            },
            Step::TurnEnd,
            Step::Text {
                chunks: vec!["two".into()],
            },
            Step::TurnEnd,
        ],
    ))
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "first").await, Verdict::Accepted);
    agent
        .until("the first prompt is sent", || {
            sent(&agent, "session/prompt").len() == 1
        })
        .await;
    assert_eq!(daemon.prompt(b"p2", "second").await, Verdict::Queued);
    assert_eq!(sent(&agent, "session/prompt").len(), 1, "sent mid-turn");
    agent.release();
    agent
        .wait("both turns end", |log| log.turn_ends() == 2)
        .await;
    let prompts = sent(&agent, "session/prompt");
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[1]["params"]["prompt"][0]["text"], "second");
    assert!(agent.log().has_text("two"));
    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
}

/// A later process resumes the saved session where the agent offers
/// `session/resume`, and the conversation goes on under the same id.
#[tokio::test(flavor = "multi_thread")]
async fn a_later_process_resumes_the_session() {
    let mut steps = said("kiwi");
    steps.extend(said("still kiwi"));
    let agent = Agent::start(acp("opencode", steps)).await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(
        daemon.prompt(b"p1", "remember kiwi").await,
        Verdict::Accepted
    );
    agent
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    let session = agent.provider_session();
    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);

    agent.resume();
    let mut daemon = agent.dial().await;
    agent
        .wait("the resumed boundary", |log| {
            log.boundaries()
                .iter()
                .any(|boundary| boundary == "RESUMED")
        })
        .await;
    let resume = sent(&agent, "session/resume");
    assert_eq!(resume.len(), 1);
    assert_eq!(resume[0]["params"]["sessionId"], session.as_str());
    assert_eq!(resume[0]["params"]["mcpServers"][0]["name"], "amux");
    assert_eq!(
        sent(&agent, "session/new").len(),
        1,
        "only the first process's"
    );
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p2", "which word?").await, Verdict::Accepted);
    agent
        .wait("the second turn ends", |log| log.turn_ends() == 2)
        .await;
    assert_eq!(agent.provider_session(), session);
    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
}

/// An agent that offers only `session/load` replays the conversation
/// before it answers; what it replays is already in the chat and is not
/// drawn again.
#[tokio::test(flavor = "multi_thread")]
async fn a_loaded_session_replays_without_drawing_twice() {
    let agent = Agent::start(Setup {
        acp: AcpAgent {
            resume: false,
            ..AcpAgent::default()
        },
        ..acp("my-own-agent", said("kiwi"))
    })
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(
        daemon.prompt(b"p1", "remember kiwi").await,
        Verdict::Accepted
    );
    agent
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
    let drawn = |log: &Log| {
        log.full_items()
            .values()
            .filter(|item| item.text == "kiwi")
            .count()
    };
    assert_eq!(drawn(&agent.log()), 1);

    agent.resume();
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(sent(&agent, "session/load").len(), 1);
    assert!(sent(&agent, "session/resume").is_empty());
    let replayed = agent
        .facts()
        .iter()
        .any(|fact| fact.to_string().contains("user_message_chunk"));
    assert!(replayed, "the agent replayed nothing");
    assert_eq!(drawn(&agent.log()), 1, "the replay was drawn again");
    // Each process plays the script from its start, so the next turn says
    // the word again, live this time.
    assert_eq!(daemon.prompt(b"p2", "which word?").await, Verdict::Accepted);
    agent
        .wait("the second turn ends", |log| log.turn_ends() == 2)
        .await;
    assert_eq!(drawn(&agent.log()), 2);
    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
}

/// An agent that can reopen no session holds its conversation while its
/// process runs, and a later process ends for good at `initialize` rather
/// than start a new conversation in its place.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_cannot_reopen_ends_rather_than_start_afresh() {
    let agent = Agent::start(Setup {
        acp: AcpAgent {
            resume: false,
            load: false,
            ..AcpAgent::default()
        },
        ..acp("my-own-agent", said("hello"))
    })
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "hi").await, Verdict::Accepted);
    agent
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);

    agent.resume();
    let cause = interpreter_cause(agent.exit().await);
    assert_eq!(cause, interpret::acp::UNRESUMABLE);
    assert_eq!(
        sent(&agent, "session/new").len(),
        1,
        "a fresh session was opened"
    );
    assert!(sent(&agent, "session/load").is_empty());
    assert!(sent(&agent, "session/resume").is_empty());
}

/// An agent that refuses to reopen the saved session ends the incarnation
/// in its words, and nothing new is opened in its place.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_resume_ends_the_agent() {
    let agent = Agent::start(Setup {
        acp: AcpAgent {
            refuse_reopen: true,
            ..AcpAgent::default()
        },
        ..acp("grok", said("hello"))
    })
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "hi").await, Verdict::Accepted);
    agent
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);

    agent.resume();
    let cause = interpreter_cause(agent.exit().await);
    assert!(
        cause.contains("refused to reopen its conversation"),
        "{cause}"
    );
    assert_eq!(sent(&agent, "session/resume").len(), 1);
    assert_eq!(
        sent(&agent, "session/new").len(),
        1,
        "a fresh session was opened"
    );
}

/// An agent that answers `session/new` "Authentication required" ends the
/// incarnation, and its snapshot says it is signed out.
#[tokio::test(flavor = "multi_thread")]
async fn a_signed_out_agent_ends_saying_so() {
    let agent = Agent::start(Setup {
        signed_out: true,
        ..acp("grok", said("hello"))
    })
    .await;
    let cause = interpreter_cause(agent.exit().await);
    assert!(cause.contains("Authentication required"), "{cause}");
    let sign_in = agent.log().acp().sign_in.unwrap_or_default();
    assert_eq!(sign_in.state(), wire::SignInState::SignedOut);
}

/// An attachment's bytes, in the agent's blob folder as its daemon puts
/// them there, and the attachment naming them.
fn attached(agent: &Agent, hash: u8, bytes: &[u8], image: bool, name: &str) -> wire::Attachment {
    let blobs = agent.dir.join(agent::BLOBS);
    std::fs::create_dir_all(&blobs).unwrap();
    std::fs::write(blobs.join(interpret::to_hex(&[hash; 32])), bytes).unwrap();
    let blob = wire::BlobRef {
        hash: vec![hash; 32],
        name: name.into(),
        mime: if image { "image/png" } else { "text/plain" }.into(),
        size: bytes.len() as u64,
    };
    wire::Attachment {
        of: Some(if image {
            wire::attachment::Of::Image(blob)
        } else {
            wire::attachment::Of::File(blob)
        }),
    }
}

fn prompt_with(id: &[u8], text: &str, attachments: Vec<wire::Attachment>) -> wire::Input {
    input(
        id,
        wire::acp_input::Of::Prompt(wire::PromptInput {
            text: text.into(),
            attachments,
        }),
    )
}

/// An image goes to an agent that takes images as an image block after
/// its element; an agent that takes none refuses one at once, and a file
/// reaches it as the element naming where the file is.
#[tokio::test(flavor = "multi_thread")]
async fn attachments_go_within_what_the_agent_takes() {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nthe rest";
    let mut steps = said("red");
    steps.extend(said("read it"));

    let takes = Agent::start(acp("antigravity", steps.clone())).await;
    let mut daemon = takes.dial().await;
    takes.ready().await;
    let image = attached(&takes, 0xab, PNG, true, "red.png");
    let text = format!("what colour? {}", attachments::PLACEHOLDER);
    assert_eq!(
        daemon.input(prompt_with(b"p1", &text, vec![image])).await,
        Verdict::Accepted
    );
    takes
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    let blocks = sent(&takes, "session/prompt")[0]["params"]["prompt"].clone();
    assert_eq!(blocks[1]["type"], "image", "{blocks}");
    assert_eq!(blocks[1]["mimeType"], "image/png");
    daemon.stop().await;
    assert_eq!(takes.exit().await, ExitCause::Stopped);

    let refuses = Agent::start(Setup {
        acp: AcpAgent {
            image: false,
            embedded_context: false,
            ..AcpAgent::default()
        },
        ..acp("grok", steps)
    })
    .await;
    let mut daemon = refuses.dial().await;
    refuses.ready().await;
    let image = attached(&refuses, 0xab, PNG, true, "red.png");
    assert_eq!(
        daemon.input(prompt_with(b"p1", &text, vec![image])).await,
        Verdict::Rejected(wire::Refusal::Unsupported)
    );
    assert!(sent(&refuses, "session/prompt").is_empty());
    let file = attached(&refuses, 0xcd, b"kiwi\n", false, "notes.txt");
    let text = format!("read {}", attachments::PLACEHOLDER);
    assert_eq!(
        daemon.input(prompt_with(b"p2", &text, vec![file])).await,
        Verdict::Accepted
    );
    refuses
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    let blocks = sent(&refuses, "session/prompt")[0]["params"]["prompt"].clone();
    assert_eq!(blocks.as_array().unwrap().len(), 1, "{blocks}");
    let path = refuses
        .dir
        .join(agent::BLOBS)
        .join(interpret::to_hex(&[0xcd; 32]));
    assert!(
        blocks[0]["text"]
            .as_str()
            .unwrap()
            .contains(path.to_str().unwrap()),
        "{blocks}"
    );
    daemon.stop().await;
    assert_eq!(refuses.exit().await, ExitCause::Stopped);
}

/// A call to one of amux's own tools is allowed at once, without an ask;
/// another server's tool is asked about as usual.
#[tokio::test(flavor = "multi_thread")]
async fn amuxs_own_tools_are_allowed_without_asking() {
    let agent = Agent::start(acp(
        "antigravity",
        vec![
            Step::ToolServer(ToolServerCall {
                server: "amux".into(),
                tool: "status".into(),
                input: Some(serde_json::json!({"working_on": "testing"})),
                ask: Some(Offer::Once),
                outcome: Outcome {
                    output: "ok".into(),
                    ..Outcome::default()
                },
            }),
            Step::ToolServer(ToolServerCall {
                server: "spec".into(),
                tool: "ask_the_operator".into(),
                input: None,
                ask: Some(Offer::Once),
                outcome: Outcome::default(),
            }),
            Step::TurnEnd,
        ],
    ))
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(
        daemon.prompt(b"p1", "set your status").await,
        Verdict::Accepted
    );
    agent
        .wait("the other server's tool asks", |log| {
            !log.ask_keys().is_empty()
        })
        .await;
    let answers: Vec<serde_json::Value> = agent
        .provider_input()
        .into_iter()
        .filter(|line| line.get("result").is_some())
        .collect();
    assert_eq!(answers.len(), 1, "{answers:?}");
    assert_eq!(answers[0]["result"]["outcome"]["optionId"], "allow");
    assert_eq!(agent.log().ask_keys().len(), 1, "only the other server's");
    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
}

/// Runs one turn and stops, leaving the session id saved for a later
/// process.
async fn had_a_conversation(agent: &Agent) {
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "hi").await, Verdict::Accepted);
    agent
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
}

/// A later process whose saved session id is there but unreadable starts
/// no agent, and one whose id is gone has its new session refused by the
/// interpreter: either way no fresh conversation stands for the old one.
#[tokio::test(flavor = "multi_thread")]
async fn a_lost_session_id_ends_the_agent_rather_than_start_afresh() {
    let session = |agent: &Agent| agent.dir.join(agent::PRIVATE).join("provider-session");

    let agent = Agent::start(acp("opencode", said("hello"))).await;
    had_a_conversation(&agent).await;
    std::fs::write(session(&agent), "  \n").unwrap();
    agent.resume();
    match agent.exit().await {
        ExitCause::Unstarted(why) => assert!(why.contains("cannot be read"), "{why}"),
        other => panic!("the agent started: {other:?}"),
    }
    assert_eq!(sent(&agent, "session/new").len(), 1);
    assert!(sent(&agent, "session/resume").is_empty());

    let agent = Agent::start(acp("opencode", said("hello"))).await;
    had_a_conversation(&agent).await;
    std::fs::remove_file(session(&agent)).unwrap();
    agent.resume();
    let cause = interpreter_cause(agent.exit().await);
    assert!(cause.contains("conversation was lost"), "{cause}");
    assert!(sent(&agent, "session/resume").is_empty());
    assert!(sent(&agent, "session/load").is_empty());
}

/// A command that runs but never answers a request of the handshake ends
/// the agent once the request's wait is over, naming the request.
#[tokio::test(flavor = "multi_thread")]
async fn a_handshake_that_is_never_answered_ends_the_agent() {
    let wait = i64::try_from(agent::ACP_HANDSHAKE_WAIT.as_millis()).unwrap();
    for method in ["initialize", "session/new"] {
        let agent = Agent::start(Setup {
            acp: AcpAgent {
                unanswered: Some(method.into()),
                ..AcpAgent::default()
            },
            ..acp("opencode", said("hello"))
        })
        .await;
        let _daemon = agent.dial().await;
        until(&format!("{method} to be sent"), || async {
            if sent(&agent, method).is_empty() {
                Err("not yet")
            } else {
                Ok(())
            }
        })
        .await
        .unwrap();
        agent.clock.armed(T0 + wait).await;
        agent.clock.set(T0 + wait);
        match agent.exit().await {
            ExitCause::Unstarted(why) => assert_eq!(
                why,
                format!("opencode did not answer {method} within 60 seconds")
            ),
            other => panic!("{method}: {other:?}"),
        }
    }
}

/// An agent that answers `initialize` in another version of ACP than
/// amux's ends saying so, and no session is opened.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_in_another_protocol_version_ends() {
    let agent = Agent::start(Setup {
        acp: AcpAgent {
            protocol_version: 2,
            ..AcpAgent::default()
        },
        ..acp("opencode", said("hello"))
    })
    .await;
    let _daemon = agent.dial().await;
    let cause = interpreter_cause(agent.exit().await);
    assert!(
        cause.contains("speaks version 2 of the Agent Client Protocol"),
        "{cause}"
    );
    assert!(sent(&agent, "session/new").is_empty());
}
