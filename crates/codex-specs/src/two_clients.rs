//! Specifications for two clients of one Codex server on a socket: amux and
//! another client on the same live thread, as Codex's own app is when it
//! attaches. Each pins what amux's client is told about work the other
//! client caused: its prompt, its interrupt, its answer to an approval amux
//! was also asked, and its steer beside amux's own.
//!
//! Live capture starts `codex app-server --listen unix://…` and connects
//! both clients over its WebSocket framing. A bridge hands each client
//! plain lines and records every message either way under the client's
//! transport id, `amux` or `other`, so replay feeds each client its own
//! lines in the order the server's were seen. amux's prompts and steers
//! carry its input id in hex as Codex's client message id, as amux sends
//! them; the other client's carry ids of their own.

use std::path::Path;

use codex::{Codex, Event, ThreadEventStream, text_input};
use codex_protocol::Extra;
use codex_protocol::client::{ThreadResumeParams, TurnStartParams, TurnSteerParams};
use codex_protocol::items::ThreadItem;
use codex_protocol::server::{Decision, ServerNotification, ServerRequest};
use codex_protocol::thread::{SandboxMode, TurnStatus};

use super::{
    ScenarioReport, decision, next_event, report, started, stringify, thread_config,
    turn_completed, turn_started, wait_for_completion,
};

/// The specifications in this module.
pub const TWO_CLIENTS: &[&str] = &[
    "two_clients_prompt",
    "two_clients_approval",
    "two_clients_steer",
    "two_clients_join_fresh",
];

/// The transport ids the two clients' lines are recorded under.
pub const AMUX: &str = "amux";
pub const OTHER: &str = "other";

/// The name amux gives the thread, which lets the other client join a
/// thread that has not run a turn.
const THREAD_NAME: &str = "codex-spec-two-clients";

/// amux's client message id for an input id: the id in hex.
fn amux_message_id(input_id: &str) -> String {
    input_id.bytes().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) async fn run(
    name: &str,
    amux: &Codex,
    other: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    match name {
        "two_clients_prompt" => prompt_and_interrupt(amux, other, model, project).await,
        "two_clients_approval" => approval_answered_elsewhere(amux, other, model, project).await,
        "two_clients_steer" => steers_from_both(amux, other, model, project).await,
        "two_clients_join_fresh" => join_before_a_turn(amux, other, model, project).await,
        other => Err(format!("unknown two-client specification {other}")),
    }
}

/// amux starts and names a thread; the other client joins it.
async fn start_and_join(
    amux: &Codex,
    other: &Codex,
    model: &str,
    project: &Path,
    sandbox: SandboxMode,
) -> Result<(codex::Thread, codex::Thread), String> {
    let mut config = thread_config(model, project);
    config.sandbox = Some(sandbox.clone());
    let thread = amux
        .start_thread(config.clone())
        .await
        .map_err(|error| format!("model {model}: thread/start failed: {error}"))?;
    amux.rename_thread(thread.id(), THREAD_NAME)
        .await
        .map_err(stringify)?;
    let joined = other
        .resume_thread(ThreadResumeParams {
            thread_id: thread.id().to_owned(),
            cwd: config.cwd,
            model: config.model,
            approval_policy: config.approval_policy,
            sandbox: Some(sandbox),
            exclude_turns: None,
            extra: Extra::new(),
        })
        .await
        .map_err(|error| format!("the other client could not join: {error}"))?;
    if joined.id() != thread.id() {
        return Err("the other client joined a different thread".to_owned());
    }
    Ok((thread, joined))
}

/// amux starts and names a thread and runs no turn. The other client
/// resumes it as Codex's own app does, without the turns, and is refused:
/// the app pages them from the history on disk, which a thread has only
/// after its first turn. amux reads the thread with its turns, which writes
/// it, and the same resume then joins. No turn runs.
async fn join_before_a_turn(
    amux: &Codex,
    other: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let config = thread_config(model, project);
    let thread = amux
        .start_thread(config)
        .await
        .map_err(|error| format!("model {model}: thread/start failed: {error}"))?;
    amux.rename_thread(thread.id(), THREAD_NAME)
        .await
        .map_err(stringify)?;
    let paged = || ThreadResumeParams {
        thread_id: thread.id().to_owned(),
        exclude_turns: Some(true),
        ..ThreadResumeParams::default()
    };
    match other.resume_thread(paged()).await {
        Ok(_) => return Err("the app's resume joined a thread not yet on disk".to_owned()),
        Err(error) if error.to_string().contains("missing source rollout") => {}
        Err(error) => return Err(format!("the app's resume failed otherwise: {error}")),
    }
    amux.read_thread(thread.id(), true)
        .await
        .map_err(stringify)?;
    let joined = other
        .resume_thread(paged())
        .await
        .map_err(|error| format!("the app could not join once the thread was read: {error}"))?;
    if joined.id() != thread.id() {
        return Err("the other client joined a different thread".to_owned());
    }
    Ok(report(&thread))
}

fn turn(text: &str, client_message_id: String) -> TurnStartParams {
    TurnStartParams {
        input: vec![text_input(text)],
        client_user_message_id: Some(client_message_id),
        ..TurnStartParams::default()
    }
}

/// The other client prompts a fresh thread, then starts a long turn and
/// interrupts it; amux only watches.
async fn prompt_and_interrupt(
    amux: &Codex,
    other: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, joined) =
        start_and_join(amux, other, model, project, SandboxMode::WorkspaceWrite).await?;
    let mut events = thread.events().await.map_err(stringify)?;
    let mut theirs = joined.events().await.map_err(stringify)?;

    joined
        .start_turn(turn(
            "Reply with exactly CODEX_SPEC_OTHER and nothing else.",
            "other-prompt-1".to_owned(),
        ))
        .await
        .map_err(stringify)?;
    let messages = wait_for_completion(&mut events).await?;
    if !messages
        .iter()
        .any(|(text, _)| text.contains("CODEX_SPEC_OTHER"))
    {
        return Err("amux did not see the other client's turn answered".to_owned());
    }
    wait_for_completion(&mut theirs).await?;

    let long = joined
        .start_turn(turn(
            "Count slowly from one to one hundred, one number per line.",
            "other-prompt-2".to_owned(),
        ))
        .await
        .map_err(stringify)?;
    while turn_started(&next_event(&mut theirs, "the long turn's start").await?).is_none() {}
    joined.interrupt(&long.id).await.map_err(stringify)?;
    let status = completion_status(&mut events).await?;
    if status != TurnStatus::Interrupted {
        return Err(format!("amux saw the interrupted turn end as {status:?}"));
    }
    wait_for_completion(&mut theirs).await?;
    Ok(report(&thread))
}

/// amux prompts a command that needs approval; both clients are asked and
/// the other client allows it.
async fn approval_answered_elsewhere(
    amux: &Codex,
    other: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, joined) =
        start_and_join(amux, other, model, project, SandboxMode::ReadOnly).await?;
    let mut events = thread.events().await.map_err(stringify)?;
    let mut theirs = joined.events().await.map_err(stringify)?;
    let file = project.join("approval-elsewhere.txt");
    let command = if project == Path::new("<MACHINE_PATH>") {
        // The sanitizer replaces the whole path token with its trailing
        // punctuation, so replay sends the recorded sentence.
        "Run this exact shell command and no substitute: /usr/bin/touch <MACHINE_PATH> Then say DONE."
            .to_owned()
    } else {
        format!(
            "Run this exact shell command and no substitute: /usr/bin/touch {}. Then say DONE.",
            file.display()
        )
    };
    thread
        .start_turn(turn(&command, amux_message_id("p1")))
        .await
        .map_err(stringify)?;

    loop {
        let event = next_event(&mut theirs, "the approval request").await?;
        if let Event::Request {
            id,
            request: ServerRequest::CommandApproval(_) | ServerRequest::FileChangeApproval(_),
        } = event.event
        {
            joined
                .respond(id, decision(Decision::Accept))
                .await
                .map_err(stringify)?;
            break;
        }
        if turn_completed(&event).is_some() {
            return Err("the turn ended before asking for approval".to_owned());
        }
    }

    let (mut asked, mut resolved) = (false, false);
    loop {
        let event = next_event(&mut events, "the turn's end").await?;
        match &event.event {
            Event::Request {
                request: ServerRequest::CommandApproval(_) | ServerRequest::FileChangeApproval(_),
                ..
            } => asked = true,
            Event::Notification(ServerNotification::ServerRequestResolved(_)) => resolved = true,
            _ => {}
        }
        if let Some(turn) = turn_completed(&event) {
            if turn.status != TurnStatus::Completed {
                return Err(format!("the turn ended {:?}", turn.status));
            }
            break;
        }
    }
    if !asked || !resolved {
        return Err(format!(
            "amux was asked: {asked}; told it was resolved: {resolved}"
        ));
    }
    wait_for_completion(&mut theirs).await?;
    if project.is_absolute() && !file.exists() {
        return Err(format!("{} was not made", file.display()));
    }
    Ok(report(&thread))
}

/// amux's turn runs a slow command; the other client steers it, then amux
/// does.
async fn steers_from_both(
    amux: &Codex,
    other: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, joined) =
        start_and_join(amux, other, model, project, SandboxMode::WorkspaceWrite).await?;
    let mut events = thread.events().await.map_err(stringify)?;
    let mut theirs = joined.events().await.map_err(stringify)?;
    let running = thread
        .start_turn(turn(
            "Run the shell command `sleep 8` and wait for it to finish. Then reply with \
             CODEX_SPEC_DONE followed by anything else you were asked to say.",
            amux_message_id("p1"),
        ))
        .await
        .map_err(stringify)?;
    loop {
        let event = next_event(&mut events, "the slow command").await?;
        if let Some(ThreadItem::CommandExecution(_)) = started(&event) {
            break;
        }
        if turn_completed(&event).is_some() {
            return Err("the turn ended before running the command".to_owned());
        }
    }
    let their_turn = loop {
        if let Some(turn) = turn_started(&next_event(&mut theirs, "the turn's start").await?) {
            break turn.id.clone();
        }
    };
    if their_turn != running.id {
        return Err("the clients saw different turns".to_owned());
    }
    joined
        .steer(TurnSteerParams {
            expected_turn_id: their_turn,
            input: vec![text_input("Also say CODEX_SPEC_OTHER_STEER.")],
            client_user_message_id: Some("other-steer-1".to_owned()),
            ..TurnSteerParams::default()
        })
        .await
        .map_err(|error| format!("the other client's steer: {error}"))?;
    thread
        .steer(TurnSteerParams {
            expected_turn_id: running.id.clone(),
            input: vec![text_input("Also say CODEX_SPEC_AMUX_STEER.")],
            client_user_message_id: Some(amux_message_id("s1")),
            ..TurnSteerParams::default()
        })
        .await
        .map_err(|error| format!("amux's steer: {error}"))?;
    wait_for_completion(&mut events).await?;
    wait_for_completion(&mut theirs).await?;
    Ok(report(&thread))
}

/// Reads to the end of the running turn and returns how it ended.
async fn completion_status(events: &mut ThreadEventStream) -> Result<TurnStatus, String> {
    loop {
        if let Some(turn) = turn_completed(&next_event(events, "the turn's end").await?) {
            return Ok(turn.status.clone());
        }
    }
}

/// The socket server and both clients' recorded bridges, for live capture.
#[cfg(unix)]
pub(super) mod live {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use codex::host::{Connection, Listen};
    use codex::{Codex, CodexConfig};
    use pty_host::Tether;
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::task::JoinHandle;

    use super::{AMUX, OTHER};

    const LISTEN_WAIT: Duration = Duration::from_secs(60);

    /// Appends each message either client exchanged to `io.jsonl`, in the
    /// order the bridges saw them.
    struct Recorder {
        file: Mutex<std::fs::File>,
        start: Instant,
    }

    impl Recorder {
        fn line(&self, transport: &str, dir: &str, line: &str) {
            use std::io::Write as _;
            let row = serde_json::json!({
                "us": self.start.elapsed().as_micros() as u64,
                "dir": dir,
                "line": line,
                "transport_id": transport,
            });
            let mut file = self.file.lock().expect("the recorder's lock");
            let _ = writeln!(file, "{row}");
        }
    }

    pub struct Server {
        child: tokio::process::Child,
        tether: Tether,
        socket: PathBuf,
        bridges: Vec<JoinHandle<()>>,
    }

    impl Server {
        /// Ends the server once both clients have closed, and waits for
        /// the bridges to record the last of what they carried.
        pub async fn stop(mut self) {
            for bridge in self.bridges.drain(..) {
                let _ = tokio::time::timeout(Duration::from_secs(10), bridge).await;
            }
            if let Some(pid) = self.child.id() {
                // The server leads its own group; TERM lets it finish as the
                // end of its input would.
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", &format!("-{pid}")])
                    .status();
            }
            let _ = tokio::time::timeout(Duration::from_secs(30), self.child.wait()).await;
            let _ = self.child.start_kill();
            self.tether.release().await;
            let _ = std::fs::remove_file(&self.socket);
        }
    }

    /// Starts Codex's server on a socket and connects amux, then the other
    /// client, recording both into `io_path`.
    pub async fn open(
        codex_home: &Path,
        model: &str,
        project: &Path,
        io_path: &Path,
    ) -> Result<(Codex, Codex, Server), String> {
        let recorder = Arc::new(Recorder {
            file: Mutex::new(std::fs::File::create(io_path).map_err(|error| error.to_string())?),
            start: Instant::now(),
        });
        // A socket path must stay short; the scratch home's is not.
        let socket = PathBuf::from(format!(
            "/tmp/codex-spec-{}.sock",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        ));
        let listen = Listen::Unix(socket.clone());
        let mut command = tokio::process::Command::new("codex");
        command
            .args(["--model", model])
            .current_dir(project)
            .env("CODEX_HOME", codex_home)
            .stderr(std::process::Stdio::null());
        let mut child =
            codex::host::spawn_server(command, &listen).map_err(|error| error.to_string())?;
        let tether = Tether::new(&child).map_err(|error| error.to_string())?;
        let mut bridges = Vec::new();
        let mut clients = Vec::new();
        for (transport, name) in [(AMUX, "amux-codex-spec"), (OTHER, "codex-spec-other")] {
            let connection = Connection::connect(&listen, &mut child, LISTEN_WAIT)
                .await
                .map_err(|error| error.to_string())?;
            let (reader, writer, mut tasks) = bridge(connection, transport, recorder.clone());
            bridges.append(&mut tasks);
            let codex = Codex::from_io(
                reader,
                writer,
                CodexConfig {
                    model: Some(model.to_owned()),
                    client_name: name.to_owned(),
                    ..CodexConfig::default()
                },
            )
            .await
            .map_err(|error| format!("{transport} client: {error}"))?;
            clients.push(codex);
        }
        let other = clients.pop().expect("two clients");
        let amux = clients.pop().expect("two clients");
        Ok((
            amux,
            other,
            Server {
                child,
                tether,
                socket,
                bridges,
            },
        ))
    }

    /// Plain lines for a client over one socket connection, each recorded
    /// on its way through.
    fn bridge(
        connection: Connection,
        transport: &'static str,
        recorder: Arc<Recorder>,
    ) -> (
        BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
        Vec<JoinHandle<()>>,
    ) {
        let (client, ours) = tokio::io::duplex(1 << 20);
        let (from_client, mut to_client) = tokio::io::split(ours);
        let (mut sender, mut receiver) = connection.into_split();
        let outbound = {
            let recorder = recorder.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(from_client).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    recorder.line(transport, "stdin", &line);
                    if sender.send(line.as_bytes()).await.is_err() {
                        break;
                    }
                }
                let _ = sender.close().await;
            })
        };
        let inbound = tokio::spawn(async move {
            while let Some(Ok(message)) = receiver.next().await {
                let line = String::from_utf8_lossy(&message).into_owned();
                // Recorded before the client sees it, so whatever the client
                // does about it is recorded after.
                recorder.line(transport, "stdout", &line);
                let delivered = async {
                    to_client.write_all(line.as_bytes()).await?;
                    to_client.write_all(b"\n").await?;
                    to_client.flush().await
                }
                .await;
                if delivered.is_err() {
                    break;
                }
            }
            let _ = to_client.shutdown().await;
        });
        let (reader, writer) = tokio::io::split(client);
        (BufReader::new(reader), writer, vec![outbound, inbound])
    }
}
