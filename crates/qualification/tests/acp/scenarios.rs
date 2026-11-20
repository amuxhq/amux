//! The scenarios each built-in agent runs.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::Duration;

use serde_json::{Value, json};

use super::client::{Conn, Permission, initialize_params, text};
use super::{Agent, Cx, Notes, Verdict};

/// How long `initialize` may take; a first start can download or index.
const INITIALIZE: Duration = Duration::from_secs(90);
/// How long opening a session may take.
const OPEN: Duration = Duration::from_secs(120);
/// How long one turn may take.
const TURN: Duration = Duration::from_secs(240);
/// How long a settings change may take.
const SETTING: Duration = Duration::from_secs(30);
/// The quiet that ends a settle, and the most a settle waits.
const QUIET: Duration = Duration::from_secs(2);
const SETTLE_CAP: Duration = Duration::from_secs(10);

/// A turn long enough to interrupt or talk over.
const LONG_TURN: &str = "Write a 500-word story about a lighthouse keeper. Do not use any tools.";
/// The image the attachment scenario sends.
const SQUARE_PNG: &[u8] = include_bytes!("../live/square.png");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    Prompt,
    Read,
    Edit,
    Allow,
    Deny,
    AllowAlways,
    Cancel,
    Queued,
    MidTurn,
    Image,
    Config,
    Resume,
    Load,
    Catalogue,
    SignedOut,
    Elicitation,
    Question,
    Compact,
}

impl Scenario {
    pub const ALL: &'static [Scenario] = &[
        Scenario::Prompt,
        Scenario::Read,
        Scenario::Edit,
        Scenario::Allow,
        Scenario::Deny,
        Scenario::AllowAlways,
        Scenario::Cancel,
        Scenario::Queued,
        Scenario::MidTurn,
        Scenario::Image,
        Scenario::Config,
        Scenario::Resume,
        Scenario::Load,
        Scenario::Catalogue,
        Scenario::SignedOut,
        Scenario::Elicitation,
        Scenario::Question,
        Scenario::Compact,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Scenario::Prompt => "prompt",
            Scenario::Read => "read",
            Scenario::Edit => "edit",
            Scenario::Allow => "allow",
            Scenario::Deny => "deny",
            Scenario::AllowAlways => "allow_always",
            Scenario::Cancel => "cancel",
            Scenario::Queued => "queued",
            Scenario::MidTurn => "mid_turn",
            Scenario::Image => "image",
            Scenario::Config => "config",
            Scenario::Resume => "resume",
            Scenario::Load => "load",
            Scenario::Catalogue => "catalogue",
            Scenario::SignedOut => "signed_out",
            Scenario::Elicitation => "elicitation",
            Scenario::Question => "question",
            Scenario::Compact => "compact",
        }
    }

    /// What the recording shows, for its manifest.
    pub fn describe(self) -> &'static str {
        match self {
            Scenario::Prompt => "one plain prompt and its answer",
            Scenario::Read => "a prompt that has the agent read a file in its folder",
            Scenario::Edit => {
                "a prompt that has the agent edit a file, its permission allowed once"
            }
            Scenario::Allow => "a shell command needing permission, allowed once",
            Scenario::Deny => "a shell command needing permission, rejected once",
            Scenario::AllowAlways => {
                "a narrow shell command allowed always, then the same command again"
            }
            Scenario::Cancel => {
                "a long turn cancelled with session/cancel once it streams, then one more prompt"
            }
            Scenario::Queued => "a second prompt sent as soon as the first turn's answer arrives",
            Scenario::MidTurn => "a second session/prompt sent while the first turn streams",
            Scenario::Image => "a prompt with a PNG image attached",
            Scenario::Config => {
                "thought level and mode changed by session/set_config_option and \
                 session/set_mode, then one prompt"
            }
            Scenario::Resume => {
                "a prompt, the process ended, then session/resume in a new process and a prompt"
            }
            Scenario::Load => {
                "a prompt, the process ended, then session/load in a new process and a prompt"
            }
            Scenario::Catalogue => "initialize alone, with no session opened",
            Scenario::SignedOut => "initialize and session/new under an empty home, signed out",
            Scenario::Elicitation => {
                "a tool server's form elicitation, during a tool call the prompt asks for"
            }
            Scenario::Question => {
                "a prompt asking the agent to put a question to the person with its own \
                 question tool rather than in chat"
            }
            Scenario::Compact => "one prompt, then the agent's own compact command",
        }
    }

    /// Whether the agent is started as `Agent::asking` says.
    pub fn asks(self) -> bool {
        matches!(
            self,
            Scenario::Allow | Scenario::Deny | Scenario::AllowAlways
        )
    }

    pub fn skipped_for(self, agent: &Agent) -> Option<&'static str> {
        match self {
            // Only Grok's sign-in can be kept apart from the owner's: its
            // token is a file under the home folder.
            Scenario::SignedOut if agent.name != "grok" => {
                Some("signed-out start is recorded for Grok only")
            }
            _ => None,
        }
    }

    pub async fn run(self, cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
        match self {
            Scenario::Prompt => prompt(cx, notes).await,
            Scenario::Read => read(cx, notes).await,
            Scenario::Edit => edit(cx, notes).await,
            Scenario::Allow => shell(cx, notes, Permission::AllowOnce).await,
            Scenario::Deny => shell(cx, notes, Permission::RejectOnce).await,
            Scenario::AllowAlways => allow_always(cx, notes).await,
            Scenario::Cancel => cancel(cx, notes).await,
            Scenario::Queued => queued(cx, notes).await,
            Scenario::MidTurn => mid_turn(cx, notes).await,
            Scenario::Image => image(cx, notes).await,
            Scenario::Config => config(cx, notes).await,
            Scenario::Resume => reopen(cx, notes, "session/resume").await,
            Scenario::Load => reopen(cx, notes, "session/load").await,
            Scenario::Catalogue => catalogue(cx, notes).await,
            Scenario::SignedOut => signed_out(cx, notes).await,
            Scenario::Elicitation => elicitation(cx, notes).await,
            Scenario::Question => question(cx, notes).await,
            Scenario::Compact => compact(cx, notes).await,
        }
    }
}

/// An open session and what the agent said about it.
struct Session {
    id: String,
    capabilities: Value,
    /// The newest config options the agent reported.
    options: Vec<Value>,
    /// `session/new`'s answer, whole.
    opened: Value,
}

impl Session {
    fn option(&self, category: &str) -> Option<&Value> {
        self.options
            .iter()
            .find(|option| option["category"] == category)
    }

    fn advertises_close(&self) -> bool {
        self.capabilities
            .pointer("/sessionCapabilities/close")
            .is_some()
    }
}

/// Starts the agent and answers `initialize`.
async fn start(
    cx: &Cx,
    notes: &mut Notes,
    env: &[(&str, OsString)],
) -> Result<(Conn, Value), String> {
    start_as(cx, notes, cx.agent.command, env).await
}

async fn start_as(
    cx: &Cx,
    notes: &mut Notes,
    command: &[&str],
    env: &[(&str, OsString)],
) -> Result<(Conn, Value), String> {
    let mut conn = Conn::start(&cx.probe, command, &cx.project, &cx.capture, env)?;
    let answer = conn
        .call("initialize", initialize_params(), INITIALIZE)
        .await?;
    notes.add(format!(
        "initialize answered in {} ms",
        conn.started.elapsed().as_millis()
    ));
    let result = answer
        .get("result")
        .cloned()
        .ok_or_else(|| format!("initialize failed: {}", answer["error"]))?;
    Ok((conn, result["agentCapabilities"].clone()))
}

/// Starts the agent, opens a session in the project folder with
/// `servers`, and switches it to the cheapest model and thought level.
async fn open(cx: &Cx, notes: &mut Notes, servers: Value) -> Result<(Conn, Session), String> {
    open_as(cx, notes, servers, cx.agent.command, &[], Value::Null).await
}

/// Opens a session with the agent started so that it asks before shell
/// commands (see `Agent::asking`).
async fn open_asking(cx: &Cx, notes: &mut Notes) -> Result<(Conn, Session), String> {
    match cx.agent.asking {
        Some(asking) => {
            let env: Vec<(&str, OsString)> = asking
                .env
                .iter()
                .map(|(name, value)| (*name, OsString::from(value)))
                .collect();
            let meta: Value = serde_json::from_str(asking.session_meta).unwrap_or_default();
            open_as(cx, notes, json!([]), asking.command, &env, meta).await
        }
        None => open(cx, notes, json!([])).await,
    }
}

async fn open_as(
    cx: &Cx,
    notes: &mut Notes,
    servers: Value,
    command: &[&str],
    env: &[(&str, OsString)],
    meta: Value,
) -> Result<(Conn, Session), String> {
    let (mut conn, capabilities) = start_as(cx, notes, command, env).await?;
    let mut params = json!({"cwd": cx.project, "mcpServers": servers});
    if !meta.is_null() {
        params["_meta"] = meta;
    }
    let answer = conn.call("session/new", params, OPEN).await?;
    notes.add(format!(
        "session/new answered {} ms after start",
        conn.started.elapsed().as_millis()
    ));
    let opened = answer
        .get("result")
        .cloned()
        .ok_or_else(|| format!("session/new failed: {}", answer["error"]))?;
    let mut session = Session {
        id: opened["sessionId"]
            .as_str()
            .ok_or("session/new gave no sessionId")?
            .to_owned(),
        capabilities,
        options: opened["configOptions"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
        opened,
    };
    cheapen(&mut conn, &mut session, &cx.agent, notes).await?;
    Ok((conn, session))
}

/// Switches the session to the agent's cheapest model and lowest thought
/// level where it is not on them already.
async fn cheapen(
    conn: &mut Conn,
    session: &mut Session,
    agent: &Agent,
    notes: &mut Notes,
) -> Result<(), String> {
    let wanted = [
        Some(("model", agent.model)),
        agent.thought_level.map(|level| ("thought_level", level)),
    ];
    for (category, value) in wanted.into_iter().flatten() {
        let Some(option) = session.option(category) else {
            notes.add(format!("no {category} config option to set"));
            continue;
        };
        if option["currentValue"] == value {
            continue;
        }
        let id = option["id"].clone();
        set_option(conn, session, &id, value, notes).await?;
    }
    Ok(())
}

async fn set_option(
    conn: &mut Conn,
    session: &mut Session,
    id: &Value,
    value: &str,
    notes: &mut Notes,
) -> Result<Value, String> {
    let answer = conn
        .call(
            "session/set_config_option",
            json!({"sessionId": session.id, "configId": id, "value": value}),
            SETTING,
        )
        .await?;
    match answer.get("result") {
        Some(result) => {
            if let Some(options) = result["configOptions"].as_array() {
                session.options = options.clone();
            }
            notes.add(format!("set_config_option {id}={value}: ok"));
        }
        None => notes.add(format!(
            "set_config_option {id}={value}: error {}",
            answer["error"]
        )),
    }
    Ok(answer)
}

/// Sends a prompt and waits for its answer, returning it whole.
async fn turn(conn: &mut Conn, session: &Session, blocks: Vec<Value>) -> Result<Value, String> {
    let id = conn
        .request(
            "session/prompt",
            json!({"sessionId": session.id, "prompt": blocks}),
        )
        .await?;
    conn.response(id, TURN).await
}

/// Closes the session where the agent offers that, then ends the process.
async fn close(mut conn: Conn, session: &Session, notes: &mut Notes) {
    conn.settle(QUIET, SETTLE_CAP).await;
    if session.advertises_close() {
        match conn
            .call("session/close", json!({"sessionId": session.id}), SETTING)
            .await
        {
            Ok(answer) if answer.get("error").is_some() => {
                notes.add(format!("session/close: error {}", answer["error"]))
            }
            Ok(_) => {}
            Err(error) => notes.add(format!("session/close: {error}")),
        }
    }
    conn.end().await;
}

/// A prompt's answer, in a word: its stop reason or its error.
fn outcome(answer: &Value) -> String {
    match answer.get("result") {
        Some(result) => format!("stopReason {}", result["stopReason"]),
        None => format!("error {}", answer["error"]),
    }
}

fn stop_reason(answer: &Value) -> Option<&str> {
    answer.pointer("/result/stopReason").and_then(Value::as_str)
}

/// The session updates and agent requests since message `from`, counted
/// by kind, with the options each permission request offered.
fn traffic(conn: &Conn, from: usize) -> String {
    let mut kinds = BTreeMap::<String, usize>::new();
    let mut asks = Vec::new();
    for message in &conn.seen[from..] {
        let method = message["method"].as_str().unwrap_or_default();
        let kind = match method {
            "session/update" => message
                .pointer("/params/update/sessionUpdate")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned(),
            "session/request_permission" => {
                let options: Vec<String> = message["params"]["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|option| option["kind"].as_str().unwrap_or("?").to_owned())
                    .collect();
                asks.push(format!("permission[{}]", options.join(",")));
                continue;
            }
            "" => continue,
            other => other.to_owned(),
        };
        *kinds.entry(kind).or_default() += 1;
    }
    let mut parts: Vec<String> = kinds
        .into_iter()
        .map(|(kind, count)| format!("{kind}×{count}"))
        .collect();
    parts.extend(asks);
    parts.join(" ")
}

fn permissions_since(conn: &Conn, from: usize) -> usize {
    conn.seen[from..]
        .iter()
        .filter(|message| message["method"] == "session/request_permission")
        .count()
}

/// The agent's message text since message `from`.
fn said(conn: &Conn, from: usize) -> String {
    conn.seen[from..]
        .iter()
        .filter(|message| {
            message.pointer("/params/update/sessionUpdate") == Some(&json!("agent_message_chunk"))
        })
        .filter_map(|message| message.pointer("/params/update/content/text")?.as_str())
        .collect()
}

fn streams(message: &Value) -> bool {
    message["method"] == "session/update"
        && matches!(
            message
                .pointer("/params/update/sessionUpdate")
                .and_then(Value::as_str),
            Some("agent_message_chunk" | "agent_thought_chunk" | "tool_call")
        )
}

fn snippet(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 80 {
        format!("{}…", flat.chars().take(80).collect::<String>())
    } else {
        flat
    }
}

/// One turn, reported: its outcome, its traffic and what the agent said.
async fn reported_turn(
    conn: &mut Conn,
    session: &Session,
    blocks: Vec<Value>,
    label: &str,
    notes: &mut Notes,
) -> Result<(Value, usize), String> {
    let from = conn.seen.len();
    let answer = turn(conn, session, blocks).await?;
    notes.add(format!(
        "{label}: {} | {} | said {:?}",
        outcome(&answer),
        traffic(conn, from),
        snippet(&said(conn, from))
    ));
    Ok((answer, from))
}

async fn prompt(cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
    let (mut conn, session) = open(cx, notes, json!([])).await?;
    let (answer, _) = reported_turn(
        &mut conn,
        &session,
        vec![text("Reply with exactly the word: pong")],
        "turn",
        notes,
    )
    .await?;
    close(conn, &session, notes).await;
    Ok(if stop_reason(&answer) == Some("end_turn") {
        Verdict::Pass
    } else {
        Verdict::Finding
    })
}

async fn read(cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
    std::fs::write(
        cx.project.join("notes.txt"),
        "The secret word is lighthouse.\n",
    )
    .map_err(|error| error.to_string())?;
    let (mut conn, session) = open(cx, notes, json!([])).await?;
    let (_, from) = reported_turn(
        &mut conn,
        &session,
        vec![text(
            "Read the file notes.txt in the current folder and reply with only the secret word \
             it contains.",
        )],
        "turn",
        notes,
    )
    .await?;
    let found = said(&conn, from).to_lowercase().contains("lighthouse");
    close(conn, &session, notes).await;
    Ok(if found {
        Verdict::Pass
    } else {
        Verdict::Finding
    })
}

async fn edit(cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
    let file = cx.project.join("greeting.txt");
    std::fs::write(&file, "hello world\n").map_err(|error| error.to_string())?;
    let (mut conn, session) = open(cx, notes, json!([])).await?;
    reported_turn(
        &mut conn,
        &session,
        vec![text(
            "In greeting.txt in the current folder, replace the word hello with goodbye. \
             Change nothing else.",
        )],
        "turn",
        notes,
    )
    .await?;
    close(conn, &session, notes).await;
    let after = std::fs::read_to_string(&file).unwrap_or_default();
    notes.add(format!("greeting.txt is now {after:?}"));
    Ok(if after.contains("goodbye") {
        Verdict::Pass
    } else {
        Verdict::Finding
    })
}

/// A script in the project folder that writes the file it is given. Every
/// agent asks before running a program it does not know, where it would
/// run `touch` or `echo` unasked as read-only or safe, and a rule an agent
/// keeps for "always" names only this script.
fn write_script(cx: &Cx, name: &str) -> Result<(), String> {
    let path = cx.project.join(name);
    std::fs::write(&path, "#!/bin/sh\necho made > \"$1\"\n").map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// A shell command that needs permission, answered by `answer`.
async fn shell(cx: &Cx, notes: &mut Notes, answer: Permission) -> Result<Verdict, String> {
    let made = match answer {
        Permission::RejectOnce => "amux-shell-denied.txt",
        _ => "amux-shell-allowed.txt",
    };
    write_script(cx, "amux-make-file.sh")?;
    let (mut conn, session) = open_asking(cx, notes).await?;
    conn.permission = answer;
    let (_, from) = reported_turn(
        &mut conn,
        &session,
        vec![text(&format!(
            "Run this exact shell command in the current folder with your shell tool: \
             `./amux-make-file.sh {made}`. Then reply with the word done. If you are not \
             allowed to run it, do not try another way; just say so."
        ))],
        "turn",
        notes,
    )
    .await?;
    let asked = permissions_since(&conn, from);
    close(conn, &session, notes).await;
    let exists = cx.project.join(made).exists();
    notes.add(format!("{made} exists afterwards: {exists}"));
    let as_asked = match answer {
        Permission::RejectOnce => !exists,
        _ => exists,
    };
    Ok(if asked > 0 && as_asked {
        Verdict::Pass
    } else {
        notes.add(format!("permission requests: {asked}"));
        Verdict::Finding
    })
}

/// The command is a script of the lane's own, harmless and named for it,
/// so whatever rule the agent keeps for "always" allows only it.
async fn allow_always(cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
    let command = "./amux-acp-allow-always.sh amux-acp-allow-always.txt";
    write_script(cx, "amux-acp-allow-always.sh")?;
    let (mut conn, session) = open_asking(cx, notes).await?;
    conn.permission = Permission::AllowAlways;
    let (_, first) = reported_turn(
        &mut conn,
        &session,
        vec![text(&format!(
            "Run this exact shell command in the current folder with your shell tool: \
             `{command}`. Then reply with the word done."
        ))],
        "first",
        notes,
    )
    .await?;
    let asked_first = permissions_since(&conn, first);
    // Anything asked again is answered once, to see whether it was.
    conn.permission = Permission::AllowOnce;
    let (_, second) = reported_turn(
        &mut conn,
        &session,
        vec![text(&format!(
            "Run exactly the same shell command again: `{command}`. Then reply with the word \
             done."
        ))],
        "again",
        notes,
    )
    .await?;
    let asked_again = permissions_since(&conn, second);
    close(conn, &session, notes).await;
    notes.add(format!(
        "permission requests: first {asked_first}, again {asked_again}"
    ));
    Ok(if asked_first > 0 && asked_again == 0 {
        Verdict::Pass
    } else {
        Verdict::Finding
    })
}

async fn cancel(cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
    let (mut conn, session) = open(cx, notes, json!([])).await?;
    let from = conn.seen.len();
    let id = conn
        .request(
            "session/prompt",
            json!({"sessionId": session.id, "prompt": [text(LONG_TURN)]}),
        )
        .await?;
    conn.until(TURN, streams).await?;
    conn.notify("session/cancel", json!({"sessionId": session.id}))
        .await?;
    let cancelled_at = conn.seen.len();
    let answer = conn.response(id, Duration::from_secs(90)).await?;
    let late = conn.seen.len() - cancelled_at;
    notes.add(format!(
        "cancelled turn: {} | {} | {late} messages between cancel and answer",
        outcome(&answer),
        traffic(&conn, from)
    ));
    let answered = conn.seen.len();
    conn.settle(QUIET, SETTLE_CAP).await;
    notes.add(format!("after the answer: {}", traffic(&conn, answered)));
    reported_turn(
        &mut conn,
        &session,
        vec![text("Reply with exactly the word: after")],
        "next turn",
        notes,
    )
    .await?;
    close(conn, &session, notes).await;
    Ok(if stop_reason(&answer) == Some("cancelled") {
        Verdict::Pass
    } else {
        Verdict::Finding
    })
}

async fn queued(cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
    let (mut conn, session) = open(cx, notes, json!([])).await?;
    let (first, _) = reported_turn(
        &mut conn,
        &session,
        vec![text("Reply with exactly the word: one")],
        "first",
        notes,
    )
    .await?;
    // Sent the moment the first answer arrives, as amux sends a held prompt.
    let (second, _) = reported_turn(
        &mut conn,
        &session,
        vec![text("Reply with exactly the word: two")],
        "second",
        notes,
    )
    .await?;
    close(conn, &session, notes).await;
    Ok(
        if stop_reason(&first) == Some("end_turn") && stop_reason(&second) == Some("end_turn") {
            Verdict::Pass
        } else {
            Verdict::Finding
        },
    )
}

async fn mid_turn(cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
    let (mut conn, session) = open(cx, notes, json!([])).await?;
    let from = conn.seen.len();
    let first = conn
        .request(
            "session/prompt",
            json!({"sessionId": session.id, "prompt": [text(LONG_TURN)]}),
        )
        .await?;
    conn.until(TURN, streams).await?;
    let sent_at = conn.seen.len();
    let second = conn
        .request(
            "session/prompt",
            json!({"sessionId": session.id, "prompt": [
                text("Stop the story now and reply with exactly the word: steered"),
            ]}),
        )
        .await?;
    // Whichever answer comes first is kept until it is asked for.
    let first_answer = conn.response(first, TURN).await?;
    let first_done = conn.seen.len();
    let second_answer = conn.response(second, TURN).await?;
    let order = conn
        .seen
        .iter()
        .filter_map(|message| match message.get("id").and_then(Value::as_u64) {
            Some(id) if message.get("method").is_none() && (id == first || id == second) => {
                Some(if id == first { "first" } else { "second" })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    notes.add(format!(
        "first prompt: {} | second prompt: {} | answers in order {order:?}",
        outcome(&first_answer),
        outcome(&second_answer)
    ));
    notes.add(format!(
        "before the second prompt: {} | after it until the first answer: {} | after: {}",
        traffic(&conn, from).chars().take(200).collect::<String>(),
        traffic_between(&conn, sent_at, first_done),
        traffic(&conn, first_done)
    ));
    notes.add(format!(
        "said after the second prompt: {:?}",
        snippet(&said(&conn, sent_at))
    ));
    close(conn, &session, notes).await;
    Ok(Verdict::Pass)
}

fn traffic_between(conn: &Conn, from: usize, to: usize) -> String {
    let kinds: Vec<String> = conn.seen[from..to]
        .iter()
        .filter_map(|message| {
            message
                .pointer("/params/update/sessionUpdate")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    let mut counted = BTreeMap::<String, usize>::new();
    for kind in kinds {
        *counted.entry(kind).or_default() += 1;
    }
    counted
        .into_iter()
        .map(|(kind, count)| format!("{kind}×{count}"))
        .collect::<Vec<_>>()
        .join(" ")
}

async fn image(cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
    let (mut conn, session) = open(cx, notes, json!([])).await?;
    notes.add(format!(
        "promptCapabilities.image = {}",
        session
            .capabilities
            .pointer("/promptCapabilities/image")
            .unwrap_or(&Value::Null)
    ));
    let (answer, _) = reported_turn(
        &mut conn,
        &session,
        vec![
            text("What is the main colour of the attached image? Reply with one word."),
            json!({"type": "image", "mimeType": "image/png", "data": base64(SQUARE_PNG)}),
        ],
        "turn",
        notes,
    )
    .await?;
    close(conn, &session, notes).await;
    Ok(if stop_reason(&answer) == Some("end_turn") {
        Verdict::Pass
    } else {
        Verdict::Finding
    })
}

async fn config(cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
    let (mut conn, mut session) = open(cx, notes, json!([])).await?;
    notes.add(format!(
        "config options: {}",
        session
            .options
            .iter()
            .map(|option| format!(
                "{}({})={}",
                option["id"].as_str().unwrap_or("?"),
                option["category"].as_str().unwrap_or("?"),
                option["currentValue"]
            ))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    let modes = session.opened.pointer("/modes/availableModes").cloned();
    notes.add(format!(
        "modes field: current {} of {}",
        session
            .opened
            .pointer("/modes/currentModeId")
            .unwrap_or(&Value::Null),
        modes
            .as_ref()
            .and_then(Value::as_array)
            .map(|modes| modes
                .iter()
                .map(|mode| mode["id"].to_string())
                .collect::<Vec<_>>()
                .join(","))
            .unwrap_or_else(|| "none".into())
    ));
    let mut failures = 0;
    // A different thought level than the cheap one the session is on.
    if let Some(option) = session.option("thought_level").cloned() {
        let other = values(&option)
            .into_iter()
            .find(|value| Some(value.as_str()) == Some("medium"))
            .or_else(|| {
                values(&option)
                    .into_iter()
                    .find(|value| option["currentValue"] != *value)
            });
        if let Some(other) = other {
            let from = conn.seen.len();
            let answer = set_option(&mut conn, &mut session, &option["id"], &other, notes).await?;
            failures += usize::from(answer.get("error").is_some());
            conn.settle(Duration::from_millis(800), SETTLE_CAP).await;
            notes.add(format!("after thought level: {}", traffic(&conn, from)));
        }
    }
    // Mode through the config option, then back through session/set_mode.
    let mode_option = session.option("mode").cloned();
    if let Some(option) = &mode_option {
        let other = values(option)
            .into_iter()
            .find(|value| value == "plan")
            .or_else(|| {
                values(option)
                    .into_iter()
                    .find(|value| option["currentValue"] != *value)
            });
        if let Some(other) = other {
            let from = conn.seen.len();
            let answer = set_option(&mut conn, &mut session, &option["id"], &other, notes).await?;
            failures += usize::from(answer.get("error").is_some());
            conn.settle(Duration::from_millis(800), SETTLE_CAP).await;
            notes.add(format!("after mode option: {}", traffic(&conn, from)));
        }
    }
    let back = mode_option
        .as_ref()
        .and_then(|option| option["currentValue"].as_str().map(str::to_owned))
        .or_else(|| {
            modes.as_ref()?.as_array()?.first()?["id"]
                .as_str()
                .map(str::to_owned)
        });
    // The original mode, from what the session opened with.
    let original = session
        .opened
        .pointer("/modes/currentModeId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(back)
        .unwrap_or_else(|| "default".into());
    let from = conn.seen.len();
    let answer = conn
        .call(
            "session/set_mode",
            json!({"sessionId": session.id, "modeId": original}),
            SETTING,
        )
        .await?;
    conn.settle(Duration::from_millis(800), SETTLE_CAP).await;
    notes.add(format!(
        "session/set_mode {original}: {} | {}",
        match answer.get("result") {
            Some(result) => format!("ok {result}"),
            None => format!("error {}", answer["error"]),
        },
        traffic(&conn, from)
    ));
    reported_turn(
        &mut conn,
        &session,
        vec![text("Reply with exactly the word: ok")],
        "turn",
        notes,
    )
    .await?;
    close(conn, &session, notes).await;
    Ok(if failures == 0 && answer.get("result").is_some() {
        Verdict::Pass
    } else {
        Verdict::Finding
    })
}

fn values(option: &Value) -> Vec<String> {
    option["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|choice| choice["value"].as_str().map(str::to_owned))
        .collect()
}

/// A conversation reopened in a new process by `method` (`session/resume`
/// or `session/load`), as amux continues one after the agent's process,
/// the daemon or the machine restarts.
async fn reopen(cx: &Cx, notes: &mut Notes, method: &str) -> Result<Verdict, String> {
    let (mut conn, session) = open(cx, notes, json!([])).await?;
    reported_turn(
        &mut conn,
        &session,
        vec![text(
            "Remember the word kiwi. Reply with exactly the word: ok",
        )],
        "first process",
        notes,
    )
    .await?;
    conn.settle(QUIET, SETTLE_CAP).await;
    // Ended as a restart ends it, without closing the session.
    conn.end().await;

    let (mut conn, capabilities) = start(cx, notes, &[]).await?;
    let from = conn.seen.len();
    let answer = conn
        .call(
            method,
            json!({"sessionId": session.id, "cwd": cx.project, "mcpServers": []}),
            OPEN,
        )
        .await?;
    let answered = conn.seen.len();
    conn.settle(QUIET, SETTLE_CAP).await;
    notes.add(format!(
        "{method}: {} | before its answer: {} | after: {}",
        match answer.get("result") {
            Some(result) => format!(
                "ok, answer keys {:?}",
                result
                    .as_object()
                    .map(|o| o.keys().cloned().collect::<Vec<_>>())
            ),
            None => format!("error {}", answer["error"]),
        },
        traffic_between(&conn, from, answered),
        traffic(&conn, answered)
    ));
    let Some(result) = answer.get("result") else {
        conn.end().await;
        return Ok(Verdict::Finding);
    };
    let mut reopened = Session {
        id: session.id.clone(),
        capabilities,
        options: result["configOptions"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
        opened: result.clone(),
    };
    if reopened.options.is_empty() {
        notes.add(format!("{method} answer carries no config options"));
    } else {
        cheapen(&mut conn, &mut reopened, &cx.agent, notes).await?;
    }
    let (_, from) = reported_turn(
        &mut conn,
        &reopened,
        vec![text(
            "Which word did I ask you to remember? Reply with that word only.",
        )],
        "second process",
        notes,
    )
    .await?;
    let remembered = said(&conn, from).to_lowercase().contains("kiwi");
    close(conn, &reopened, notes).await;
    Ok(if remembered {
        Verdict::Pass
    } else {
        Verdict::Finding
    })
}

/// What an agent tells a client that opens no session: amux's host learns
/// an agent's catalogue this way without leaving a conversation behind.
async fn catalogue(cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
    let (mut conn, capabilities) = start(cx, notes, &[]).await?;
    let initialize = conn
        .seen
        .iter()
        .find(|message| message.get("id") == Some(&json!(1)))
        .cloned()
        .unwrap_or_default();
    let result = &initialize["result"];
    notes.add(format!(
        "authMethods: {}",
        result["authMethods"]
            .as_array()
            .map(|methods| methods
                .iter()
                .map(|m| m["id"].to_string())
                .collect::<Vec<_>>()
                .join(","))
            .unwrap_or_else(|| "none".into())
    ));
    let models = result
        .pointer("/_meta/modelState/availableModels")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .map(|model| model["modelId"].as_str().unwrap_or("?").to_owned())
                .collect::<Vec<_>>()
                .join(",")
        });
    notes.add(format!(
        "models in initialize: {}",
        models.unwrap_or_else(|| "none".into())
    ));
    // The session-less catalogue methods, called only where advertised.
    for (method, advertised) in [
        (
            "session/options",
            capabilities.pointer("/sessionCapabilities/options"),
        ),
        ("auth/status", capabilities.pointer("/auth/status")),
    ] {
        if advertised.is_some() {
            let answer = conn.call(method, json!({}), SETTING).await?;
            notes.add(format!("{method}: {answer}"));
        } else {
            notes.add(format!("{method} not advertised"));
        }
    }
    conn.settle(QUIET, SETTLE_CAP).await;
    let unprompted = traffic(&conn, 1);
    notes.add(format!("unprompted after initialize: {unprompted}"));
    conn.end().await;
    Ok(Verdict::Pass)
}

/// Grok under a home folder of its own, empty, so it finds no sign-in;
/// the owner's is never touched (no `authenticate`, no `logout`).
async fn signed_out(cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
    let home = cx.scratch.join("home");
    std::fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    let env: Vec<(&str, OsString)> = vec![
        ("HOME", home.clone().into()),
        ("XDG_CONFIG_HOME", home.join(".config").into()),
        ("XDG_DATA_HOME", home.join(".local/share").into()),
        ("XDG_STATE_HOME", home.join(".local/state").into()),
        ("XDG_CACHE_HOME", home.join(".cache").into()),
    ];
    if std::env::var_os("XAI_API_KEY").is_some() || std::env::var_os("GROK_API_KEY").is_some() {
        return Ok(Verdict::NotRun(
            "an API key in the environment would sign Grok in".into(),
        ));
    }
    let (mut conn, _) = start(cx, notes, &env).await?;
    let answer = conn
        .call(
            "session/new",
            json!({"cwd": cx.project, "mcpServers": []}),
            OPEN,
        )
        .await?;
    notes.add(format!(
        "session/new after {} ms: {}",
        conn.started.elapsed().as_millis(),
        match answer.get("result") {
            Some(result) => format!("opened {}", result["sessionId"]),
            None => format!("error {}", answer["error"]),
        }
    ));
    conn.settle(QUIET, SETTLE_CAP).await;
    conn.end().await;
    let wrote: Vec<String> = walk(&home)
        .into_iter()
        .map(|path| {
            path.strip_prefix(&home)
                .unwrap_or(&path)
                .display()
                .to_string()
        })
        .collect();
    notes.add(format!("files Grok wrote under the empty home: {wrote:?}"));
    Ok(if answer.get("error").is_some() {
        Verdict::Pass
    } else {
        Verdict::Finding
    })
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(walk(&path));
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// A form the agent passes on from a tool server: the probe's own
/// one-tool server asks to confirm a word through an MCP elicitation.
async fn elicitation(cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
    let servers = json!([{
        "name": "spec",
        "command": cx.probe,
        "args": ["__tool-server"],
        "env": [],
    }]);
    let (mut conn, session) = open(cx, notes, servers).await?;
    let (_, from) = reported_turn(
        &mut conn,
        &session,
        vec![text(
            "Call the ask_the_operator tool from the spec tool server with the word kiwi, then \
             reply with exactly what it returned.",
        )],
        "turn",
        notes,
    )
    .await?;
    let elicited = conn.seen[from..]
        .iter()
        .filter(|message| message["method"] == "elicitation/create")
        .count();
    notes.add(format!("elicitation/create requests: {elicited}"));
    close(conn, &session, notes).await;
    Ok(if elicited > 0 {
        Verdict::Pass
    } else {
        Verdict::Finding
    })
}

/// An agent's own way of asking the person something, where it has one:
/// whatever it sends for it (an elicitation, a permission request, an
/// extension request) is what amux must show as an ask.
async fn question(cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
    let (mut conn, session) = open(cx, notes, json!([])).await?;
    let (_, from) = reported_turn(
        &mut conn,
        &session,
        vec![text(
            "Use your built-in tool for asking the user a question (not plain chat) to ask me \
             whether I prefer tea or coffee, offering those two choices. Then reply with my \
             answer in one word.",
        )],
        "turn",
        notes,
    )
    .await?;
    let requests: Vec<String> = conn.seen[from..]
        .iter()
        .filter(|message| message.get("id").is_some())
        .filter_map(|message| message["method"].as_str().map(str::to_owned))
        .collect();
    notes.add(format!("requests from the agent: {requests:?}"));
    close(conn, &session, notes).await;
    Ok(
        if requests.iter().any(|method| method == "elicitation/create") {
            Verdict::Pass
        } else {
            Verdict::Finding
        },
    )
}

async fn compact(cx: &Cx, notes: &mut Notes) -> Result<Verdict, String> {
    let (mut conn, session) = open(cx, notes, json!([])).await?;
    reported_turn(
        &mut conn,
        &session,
        vec![text("Reply with exactly the word: one")],
        "first",
        notes,
    )
    .await?;
    let commands: Vec<String> = conn
        .seen
        .iter()
        .rev()
        .find_map(|message| {
            (message.pointer("/params/update/sessionUpdate")
                == Some(&json!("available_commands_update")))
            .then(|| {
                message
                    .pointer("/params/update/availableCommands")?
                    .as_array()
                    .cloned()
            })
            .flatten()
        })
        .unwrap_or_default()
        .iter()
        .filter_map(|command| command["name"].as_str().map(str::to_owned))
        .collect();
    let Some(command) = commands
        .iter()
        .find(|name| matches!(name.as_str(), "compact" | "compress" | "summarize"))
        .cloned()
    else {
        notes.add(format!("no compact command among {commands:?}"));
        close(conn, &session, notes).await;
        return Ok(Verdict::Finding);
    };
    reported_turn(
        &mut conn,
        &session,
        vec![text(&format!("/{command}"))],
        &format!("/{command}"),
        notes,
    )
    .await?;
    close(conn, &session, notes).await;
    Ok(Verdict::Pass)
}

/// Standard base64 with padding, for the image block.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for at in 0..4 {
            if at <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * at) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
