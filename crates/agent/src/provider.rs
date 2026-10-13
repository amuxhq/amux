//! The provider child: how it is started for each kind, how its output
//! becomes facts, and how the interpreter's effects reach it.
//!
//! Headless Claude is a plain child speaking stream-JSON on stdin and
//! stdout. Claude in a terminal runs on a pseudo-terminal whose raw bytes
//! go to the terminal log; its facts are hook payloads (on
//! private/hooks.sock) and its transcript rows. Codex's app server listens
//! on private/codex.sock with the agent as one of its clients. Every child
//! runs in its own process group so a kill reaches whatever it started.
//!
//! The child's exit is what ends the agent, never the end of its output: a
//! straggler the provider left behind can hold a terminal or a pipe open
//! forever, so after the exit the output still buffered is collected for a
//! moment and the reader is then abandoned, not joined.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use claude_protocol::stream as claude_stream;
use codex_protocol::client::{
    Capabilities, ClientInfo, InitializeParams, InjectedContent, InjectedItem, ThreadResumeParams,
    ThreadStartParams,
};
use codex_protocol::items::{LocalImageInput, UserInput};
use codex_protocol::{ClientMessage, ClientNotification, ClientRequest, RequestId, ServerMessage};
use interpret::{Channel, Effect, Fact};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{mpsc, oneshot};
use wire::{AgentSpec, Attachment};

use crate::terminal::{self, Keys};
use crate::{dir, tree};

/// How long output still in flight is collected after the child exits.
const TRAILING_OUTPUT: Duration = Duration::from_millis(300);
/// How long terminal Claude's output must stay quiet after its input goes
/// live before it counts as ready, so a first screen that is its
/// folder-trust dialog is seen before anything is typed into it.
const FIRST_SCREEN_QUIET: Duration = Duration::from_millis(100);
/// The longest the first screen is waited for once input is live.
const FIRST_SCREEN_WAIT: Duration = Duration::from_millis(500);
/// The request ids of the agent's own Codex handshake; the interpreter's
/// requests are numbered amux-N.
const CODEX_INITIALIZE: &str = "agent-initialize";
const CODEX_THREAD: &str = "agent-thread";
/// The longest a starting Codex server is waited for to listen.
const CODEX_LISTEN_WAIT: Duration = Duration::from_secs(30);
/// How long a resume keeps asking for a thread another Codex server still
/// writes. A killed agent's server is told to finish by its tether, but
/// Codex can take seconds to exit and holds the thread until it does.
const CODEX_WRITER_WAIT: Duration = Duration::from_secs(30);
/// The pause before asking again for a thread another server holds.
const CODEX_WRITER_RETRY: Duration = Duration::from_millis(250);
/// How often a followed transcript is read for new rows.
const TRANSCRIPT_POLL: Duration = Duration::from_millis(25);
/// The hook events terminal Claude reports to the agent, when the spec's
/// hook policy names none.
const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PermissionRequest",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "SubagentStop",
    "Notification",
];

/// What the provider did.
#[derive(Debug)]
pub enum ProviderEvent {
    Fact(Fact),
    /// Raw terminal bytes, for the terminal log.
    Output(Vec<u8>),
    /// The child exited, with its code when it had one.
    Exited(Option<i32>),
    /// Terminal Claude's messaging socket and the token it takes, as its
    /// hooks report them.
    Messaging(claude::hooks::MessagingCredentials),
    /// A write of the provider's own state into private/ failed. The state
    /// is what the next incarnation resumes from, so an incarnation that
    /// cannot record it ends rather than run on as if it had.
    WriteFailed {
        path: PathBuf,
        error: io::Error,
    },
}

/// Writes the provider's state to `path` in private/, and reports the
/// failure to the host when the write fails: the host ends the incarnation
/// the same way it does for a journal write that fails. Every write of
/// state the next incarnation resumes from goes through here; a write that
/// only helps a dump does not.
async fn persist(events: &mpsc::Sender<ProviderEvent>, path: &Path, bytes: impl AsRef<[u8]>) {
    if let Err(error) = std::fs::write(path, bytes) {
        let _ = events
            .send(ProviderEvent::WriteFailed {
                path: path.to_path_buf(),
                error,
            })
            .await;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("starting {command}: {source}")]
    Spawn { command: String, source: io::Error },
    #[error("agents of kind {0} cannot be hosted yet")]
    Unhosted(i32),
    /// Terminal Claude is hosted on Unix only: ConPTY re-renders the child's
    /// output in its own escape sequences, and Claude's messaging socket and
    /// the fake's raw console mode are Unix-only. docs/ARCHITECTURE.md
    /// records the gap under "Windows, as a stated cost".
    #[error(
        "terminal Claude is not hosted on Windows in this build; headless Claude and Codex are"
    )]
    TerminalOnWindows,
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// A plain child's stdin, shared with the handshake that writes first.
type Stdin = Arc<tokio::sync::Mutex<Option<ChildStdin>>>;
/// The agent's connection to its Codex server, shared the same way; None
/// once closed, or when the server ended before it took a client.
type CodexSender = Arc<tokio::sync::Mutex<Option<codex::host::Sender>>>;

enum Input {
    Stdin(Stdin),
    Codex(CodexSender),
    /// Keystrokes for the typing task.
    Terminal(mpsc::UnboundedSender<Vec<claude::pty::keymap::KeyStep>>),
    Closed,
}

/// The running provider child.
pub struct Provider {
    input: Input,
    /// The child's pid, which is also its process group.
    #[cfg_attr(not(unix), allow(dead_code))]
    pid: Option<u32>,
    /// Kills a plain child where there are no process groups.
    #[cfg_attr(unix, allow(dead_code))]
    kill: Option<oneshot::Sender<()>>,
    terminal: Option<pty_host::PtyHandle>,
    /// Terminal Claude's keymap, when one covers its version.
    keys: Option<Keys>,
    messaging: Option<claude::hooks::MessagingCredentials>,
    session: String,
    dir: PathBuf,
    events: mpsc::Sender<ProviderEvent>,
    followers: Vec<Follower>,
    /// Every process below the provider when it was asked to finish, killed
    /// once it has exited if still running (see the tree module).
    census: Vec<tree::Descendant>,
}

impl Provider {
    pub async fn spawn(
        spec: &AgentSpec,
        dir: &Path,
        events: mpsc::Sender<ProviderEvent>,
    ) -> Result<Self, ProviderError> {
        match wire::Kind::try_from(spec.kind) {
            Ok(wire::Kind::ClaudeSdk) => Self::spawn_sdk(spec, dir, events).await,
            Ok(wire::Kind::ClaudePty) => Self::spawn_terminal(spec, dir, events).await,
            Ok(wire::Kind::Codex) => Self::spawn_codex(spec, dir, events).await,
            Ok(wire::Kind::Unspecified) | Err(_) => Err(ProviderError::Unhosted(spec.kind)),
        }
    }

    /// Headless Claude: `claude -p` over stream-JSON, told to echo each
    /// user message so its reflection carries the uuid it was sent with,
    /// and to send its reply as it is written rather than whole.
    async fn spawn_sdk(
        spec: &AgentSpec,
        dir: &Path,
        events: mpsc::Sender<ProviderEvent>,
    ) -> Result<Self, ProviderError> {
        let (session, resume) = claude_session(spec, dir)?;
        let (args, settings) = claude_launch(spec, dir, false)?;
        let mut command = tokio::process::Command::new(&spec.provider_command);
        command
            .args(&args)
            .args([
                "--print",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--permission-prompt-tool",
                "stdio",
                "--replay-user-messages",
                if resume { "--resume" } else { "--session-id" },
                &session,
                "--settings",
                &settings,
            ])
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(provider_log(dir)?)
            .kill_on_drop(false);
        environment(&mut command, spec);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|source| ProviderError::Spawn {
            command: spec.provider_command.clone(),
            source,
        })?;
        let pid = child.id();
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");

        let lines = events.clone();
        let reader = tokio::spawn(async move {
            let mut stdout = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = stdout.next_line().await {
                let fact = Fact {
                    channel: Channel::Stream,
                    payload: line.into_bytes(),
                };
                if lines.send(ProviderEvent::Fact(fact)).await.is_err() {
                    break;
                }
            }
        });
        let (kill, killed) = oneshot::channel();
        let exited = events.clone();
        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status,
                _ = killed => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            let _ = tokio::time::timeout(TRAILING_OUTPUT, reader).await;
            let code = status.ok().and_then(|status| status.code());
            let _ = exited.send(ProviderEvent::Exited(code)).await;
        });

        let mut provider = Self {
            input: Input::Stdin(Arc::new(tokio::sync::Mutex::new(Some(stdin)))),
            pid,
            kill: Some(kill),
            terminal: None,
            keys: None,
            messaging: None,
            session,
            dir: dir.to_owned(),
            events,
            followers: Vec::new(),
            census: Vec::new(),
        };
        // Claude reports nothing until it is asked; its answer is what says
        // it takes input. amux's interrupt ends the turn and leaves
        // background work running on every kind; Claude spares background
        // subagents on an interrupt, as it spares background shells, only
        // for a host that says it can stop each task on its own.
        let initialize = claude_stream::ControlRequest::new(
            "agent-initialize",
            claude_stream::ControlRequestBody::Initialize(
                claude_stream::control::InitializeRequestBody {
                    per_task_stop_affordance: Some(true),
                    ..Default::default()
                },
            ),
        );
        provider
            .write_line(&claude_stream::encode(
                &claude_stream::Input::ControlRequest(initialize),
            ))
            .await?;
        Ok(provider)
    }

    /// Codex: one app server per agent, listening on a socket in the
    /// agent's private folder (over stdio on Windows), with the agent as one
    /// client; Codex's own app can join the same thread on that socket. The
    /// agent does the handshake (initialize, initialized, then thread/start
    /// or, for a later incarnation, thread/resume); every message the server
    /// sends the agent is a fact, and the interpreter writes everything after
    /// the handshake.
    async fn spawn_codex(
        spec: &AgentSpec,
        dir: &Path,
        events: mpsc::Sender<ProviderEvent>,
    ) -> Result<Self, ProviderError> {
        let session_path = dir.join(dir::PRIVATE).join(dir::PROVIDER_SESSION);
        let thread = std::fs::read_to_string(&session_path)
            .ok()
            .map(|thread| thread.trim().to_owned())
            .filter(|thread| !thread.is_empty() && spec.incarnation > 1);
        let listen = codex_listen(dir)?;
        let mut command = tokio::process::Command::new(&spec.provider_command);
        command
            .args(codex_args(spec, dir))
            .current_dir(&spec.cwd)
            .stderr(provider_log(dir)?)
            .kill_on_drop(false);
        environment(&mut command, spec);
        #[cfg(unix)]
        command.process_group(0);
        let mut child =
            codex::host::spawn_server(command, &listen).map_err(|source| ProviderError::Spawn {
                command: spec.provider_command.clone(),
                source,
            })?;
        let pid = child.id();
        #[cfg(unix)]
        let tether = match &listen {
            codex::host::Listen::Unix(_) => match codex::host::Tether::new(&child) {
                Ok(tether) => Some(tether),
                Err(source) => {
                    let _ = child.start_kill();
                    return Err(ProviderError::Spawn {
                        command: "Codex's tether".to_owned(),
                        source,
                    });
                }
            },
            codex::host::Listen::Stdio => None,
        };
        let connection =
            match codex::host::Connection::connect(&listen, &mut child, CODEX_LISTEN_WAIT).await {
                Ok(connection) => Some(connection),
                // A server that ends at its start ends the incarnation as
                // any provider exit does, with its code.
                Err(codex::host::ConnectError::Exited(_)) => None,
                Err(error) => {
                    let _ = child.start_kill();
                    return Err(ProviderError::Spawn {
                        command: spec.provider_command.clone(),
                        source: io::Error::other(error),
                    });
                }
            };
        let (sender, mut receiver) = match connection {
            Some(connection) => {
                let (sender, receiver) = connection.into_split();
                (Some(sender), Some(receiver))
            }
            None => (None, None),
        };
        let sender: CodexSender = Arc::new(tokio::sync::Mutex::new(sender));

        let (initialized, on_initialized) = oneshot::channel();
        let (ended, on_ended) = oneshot::channel::<()>();
        let (held, mut on_held) = mpsc::unbounded_channel::<()>();
        let lines = events.clone();
        let writer_wait = tokio::time::Instant::now() + CODEX_WRITER_WAIT;
        let reader = tokio::spawn(async move {
            let mut initialized = Some(initialized);
            while let Some(Some(Ok(line))) = match &mut receiver {
                Some(receiver) => Some(receiver.next().await),
                None => None,
            } {
                let decoded = codex_protocol::decode(&line);
                // The previous incarnation's server still writes the thread:
                // ask again until it lets go, rather than hand the refusal
                // on, which ends the incarnation.
                if let Ok(ServerMessage::Response {
                    id: RequestId::String(id),
                    result: Err(error),
                    ..
                }) = &decoded
                    && id == CODEX_THREAD
                    && error.message.contains("active writer")
                    && tokio::time::Instant::now() < writer_wait
                    && held.send(()).is_ok()
                {
                    continue;
                }
                let answer = match decoded {
                    Ok(ServerMessage::Response { id, result, .. }) => Some((id, result.ok())),
                    Ok(ServerMessage::Unknown(unknown)) => unknown.id.map(|id| (id, None)),
                    _ => None,
                };
                match answer {
                    Some((RequestId::String(id), _)) if id == CODEX_INITIALIZE => {
                        if let Some(initialized) = initialized.take() {
                            let _ = initialized.send(());
                        }
                    }
                    // The thread the server made or resumed, or the one a
                    // `/clear` started, is the session the next incarnation
                    // resumes.
                    Some((RequestId::String(id), Some(result)))
                        if id == CODEX_THREAD
                            || id.starts_with(interpret::codex::CLEAR_REQUEST) =>
                    {
                        if let Ok(answer) = codex_protocol::result::<
                            codex_protocol::server::ThreadResponse,
                        >(&result)
                        {
                            persist(&lines, &session_path, &answer.thread.id).await;
                        }
                    }
                    _ => {}
                }
                let fact = Fact {
                    channel: Channel::Rpc,
                    payload: line,
                };
                if lines.send(ProviderEvent::Fact(fact)).await.is_err() {
                    break;
                }
            }
            let _ = ended.send(());
        });
        let (kill, killed) = oneshot::channel();
        let exited = events.clone();
        let on_socket = matches!(listen, codex::host::Listen::Unix(_));
        tokio::spawn(async move {
            let mut killed = killed;
            let mut on_ended = Some(on_ended);
            let status = loop {
                tokio::select! {
                    status = child.wait() => break status,
                    _ = &mut killed => {
                        let _ = child.start_kill();
                        break child.wait().await;
                    }
                    // On a socket the server outlives its clients, so the
                    // end of the agent's connection, whether the agent
                    // closed it or the server dropped it, is when the server
                    // is asked to finish. It finishes what is running and
                    // exits.
                    _ = async { on_ended.as_mut().expect("guarded").await },
                        if on_socket && on_ended.is_some() =>
                    {
                        on_ended = None;
                        terminate(&child);
                    }
                }
            };
            #[cfg(unix)]
            if let Some(tether) = tether {
                tether.release().await;
            }
            let _ = tokio::time::timeout(TRAILING_OUTPUT, reader).await;
            let code = status.ok().and_then(|status| status.code());
            let _ = exited.send(ProviderEvent::Exited(code)).await;
        });

        let handshake = sender.clone();
        let cwd = spec.cwd.clone();
        let model = spec.config.as_ref().and_then(|config| config.model.clone());
        tokio::spawn(async move {
            let initialize = ClientMessage::request(
                RequestId::String(CODEX_INITIALIZE.into()),
                ClientRequest::Initialize(InitializeParams {
                    client_info: ClientInfo {
                        name: "amux".into(),
                        title: None,
                        version: crate::VERSION.into(),
                        extra: Default::default(),
                    },
                    capabilities: Some(Capabilities {
                        experimental_api: Some(true),
                        extra: Default::default(),
                    }),
                    extra: Default::default(),
                }),
            );
            if send_codex(&handshake, &codex_protocol::encode(&initialize))
                .await
                .is_err()
                || on_initialized.await.is_err()
            {
                return;
            }
            let start = match thread {
                Some(thread_id) => ClientRequest::ThreadResume(ThreadResumeParams {
                    thread_id,
                    cwd: Some(cwd),
                    model,
                    ..Default::default()
                }),
                None => ClientRequest::ThreadStart(ThreadStartParams {
                    cwd: Some(cwd),
                    model,
                    ..Default::default()
                }),
            };
            let start = ClientMessage::request(RequestId::String(CODEX_THREAD.into()), start);
            let initialized = ClientMessage::notification(ClientNotification::Initialized(()));
            let _ = send_codex(&handshake, &codex_protocol::encode(&initialized)).await;
            let start = codex_protocol::encode(&start);
            let _ = send_codex(&handshake, &start).await;
            while on_held.recv().await.is_some() {
                tokio::time::sleep(CODEX_WRITER_RETRY).await;
                if send_codex(&handshake, &start).await.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            input: Input::Codex(sender),
            pid,
            kill: Some(kill),
            terminal: None,
            keys: None,
            messaging: None,
            session: String::new(),
            dir: dir.to_owned(),
            events,
            followers: Vec::new(),
            census: Vec::new(),
        })
    }

    /// Claude in a terminal. The agent reads its version and resolves the
    /// keymap for it first, and reports both as the launch fact before
    /// anything Claude says. Its hooks report to private/hooks.sock; its
    /// messaging socket is private/messaging.sock.
    async fn spawn_terminal(
        spec: &AgentSpec,
        dir: &Path,
        events: mpsc::Sender<ProviderEvent>,
    ) -> Result<Self, ProviderError> {
        if cfg!(windows) {
            return Err(ProviderError::TerminalOnWindows);
        }
        let (session, resume) = claude_session(spec, dir)?;
        let keys = Keys::resolve(Path::new(&spec.provider_command)).await;
        let launch = interpret::claude_pty::launch_fact(
            keys.as_ref().map_or("", |keys| keys.version.as_str()),
            keys.as_ref().map_or("", Keys::name),
            &keys
                .as_ref()
                .map(Keys::permission_menus)
                .unwrap_or_default(),
            keys.as_ref().and_then(Keys::send_now_refused).as_deref(),
            true,
        );
        let _ = events
            .send(ProviderEvent::Fact(Fact {
                channel: Channel::Agent,
                payload: launch,
            }))
            .await;
        // What the host's Claude offers is what the daemon gathered in its
        // own folder; asked here, Claude adds the project's own commands.
        let offered = events.clone();
        let asking = spec.clone();
        tokio::spawn(async move {
            let catalogue = crate::catalogue::claude_in(&asking).await.ok();
            let _ = offered
                .send(ProviderEvent::Fact(Fact {
                    channel: Channel::Agent,
                    payload: interpret::claude_pty::offered_fact(catalogue),
                }))
                .await;
        });
        let private = dir.join(dir::PRIVATE);
        let (mut args, settings) = claude_launch(spec, dir, true)?;
        args.extend([
            if resume { "--resume" } else { "--session-id" }.to_owned(),
            session.clone(),
            "--settings".to_owned(),
            settings,
            "--messaging-socket-path".to_owned(),
            messaging_address(&private.join(dir::MESSAGING_SOCK))?
                .display()
                .to_string(),
        ]);
        let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = spec
            .provider_env
            .iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
        env.push((
            claude::hooks::HOOK_SOCKET_ENV.into(),
            socket_address(&private.join(dir::HOOKS_SOCK))?.into(),
        ));
        let process = pty_host::spawn(pty_host::PtySpawn {
            command: PathBuf::from(&spec.provider_command),
            args,
            cwd: PathBuf::from(&spec.cwd),
            env,
            env_remove: claude::launch::CHILD_SESSION_ENV_SCRUB
                .iter()
                .map(Into::into)
                .collect(),
            size: pty_host::PtySize::default(),
        })
        .map_err(|error| ProviderError::Spawn {
            command: spec.provider_command.clone(),
            source: io::Error::other(error),
        })?;
        let handle = process.handle.clone();
        let mut output = handle.output();
        let mut exit = process.exit;
        let forward = events.clone();
        tokio::spawn(async move {
            let agent_fact = |payload| {
                ProviderEvent::Fact(Fact {
                    channel: Channel::Agent,
                    payload,
                })
            };
            let mut ready = crate::ready::InputLive::default();
            // Only a first screen is the trust dialog; later output that
            // happens to read like it is not.
            let mut trust = Some(crate::ready::TrustDialog::default());
            // Input is live: when the first screen has settled (quiet, or
            // waited for long enough), Claude is ready.
            let mut settling: Option<(tokio::time::Instant, tokio::time::Instant)> = None;
            let status = loop {
                let settled = settling.map(|(quiet, cap)| quiet.min(cap));
                tokio::select! {
                    bytes = output.recv() => match bytes {
                        Some(bytes) => {
                            let _ = forward.send(ProviderEvent::Output(bytes.to_vec())).await;
                            if trust.as_mut().is_some_and(|trust| trust.watch(&bytes)) {
                                trust = None;
                                let payload = interpret::claude_pty::trust_dialog_fact();
                                let _ = forward.send(agent_fact(payload)).await;
                            }
                            let now = tokio::time::Instant::now();
                            if ready.watch(&bytes) {
                                settling = Some((now + FIRST_SCREEN_QUIET, now + FIRST_SCREEN_WAIT));
                            } else if let Some((quiet, _)) = &mut settling {
                                *quiet = now + FIRST_SCREEN_QUIET;
                            }
                        }
                        None => break exit.wait().await,
                    },
                    () = tokio::time::sleep_until(settled.unwrap_or_else(tokio::time::Instant::now)),
                        if settled.is_some() =>
                    {
                        settling = None;
                        trust = None;
                        let payload = interpret::claude_pty::ready_fact();
                        let _ = forward.send(agent_fact(payload)).await;
                    }
                    status = exit.wait() => break status,
                }
            };
            // The reader ends only when every holder of the terminal has
            // closed it; take what is already there and leave it.
            while let Ok(Some(bytes)) = tokio::time::timeout(TRAILING_OUTPUT, output.recv()).await {
                let _ = forward.send(ProviderEvent::Output(bytes.to_vec())).await;
            }
            let code = if status.signal().is_some() {
                None
            } else {
                Some(status.exit_code() as i32)
            };
            let _ = forward.send(ProviderEvent::Exited(code)).await;
        });
        Ok(Self {
            input: Input::Terminal(terminal::typist(handle.clone())),
            pid: Some(handle.pid()),
            kill: None,
            terminal: Some(handle),
            keys,
            messaging: None,
            session,
            dir: dir.to_owned(),
            events,
            followers: Vec::new(),
            census: Vec::new(),
        })
    }

    /// Carries out an effect aimed at the provider.
    pub async fn perform(&mut self, effect: Effect) -> io::Result<()> {
        match effect {
            Effect::ProviderWrite(bytes) => self.write_line(&bytes).await,
            Effect::UserMessage {
                uuid,
                text,
                attachments,
            } => self.user_message(&uuid, &text, &attachments).await,
            Effect::Inject {
                envelope,
                via: interpret::Carrier::Stdin,
            } => {
                let uuid = interpret::claude_sdk::client_uuid(&envelope.id);
                self.user_message(&uuid, &envelope.text, &envelope.attachments)
                    .await
            }
            Effect::CodexTurnInput {
                request,
                attachments,
            } => {
                let request = codex_turn_input(&request, &attachments, &self.dir.join(dir::BLOBS))?;
                self.write_line(&request).await
            }
            Effect::FollowTranscript { path } => {
                self.follow(PathBuf::from(path));
                Ok(())
            }
            Effect::Terminal(input) => {
                let text = match &input {
                    interpret::claude_pty::TerminalInput::Prompt { text, attachments }
                    | interpret::claude_pty::TerminalInput::SendNow { text, attachments } => {
                        self.with_attachments(text, attachments)
                    }
                    _ => String::new(),
                };
                let keys = self.keys.as_ref().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "no keymap covers this Claude, so nothing can be typed",
                    )
                })?;
                let steps = keys.steps(&input, &text)?;
                self.type_keys(steps)
            }
            Effect::Inject {
                envelope,
                via: interpret::Carrier::MessagingSocket,
            } => {
                let text = self.with_attachments(&envelope.text, &envelope.attachments);
                self.message_terminal(&text).await
            }
            other => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("this agent cannot carry out {other:?} yet"),
            )),
        }
    }

    /// The text a provider reads for a prompt: its attachments written in
    /// as elements naming their blobs.
    fn with_attachments(&self, text: &str, attachments: &[Attachment]) -> String {
        if attachments.is_empty() {
            return text.to_owned();
        }
        attachments::format(
            &attachments::Positioned {
                text: text.to_owned(),
                attachments: attachments.to_vec(),
            },
            &self.dir.join(dir::BLOBS),
        )
    }

    /// A headless user message's content: the text with its elements, or,
    /// when a person attached images, content blocks with each image's bytes
    /// as a native image block right after its element.
    fn content(&self, text: &str, attachments: &[Attachment]) -> claude_stream::MessageContent {
        use attachments::Piece;
        use claude_stream::{ContentBlock, ImageSource, ImageSourceType};
        use wire::attachment::Of;

        let images = attachments
            .iter()
            .any(|attachment| matches!(attachment.of, Some(Of::Image(_))));
        if !images {
            return claude_stream::MessageContent::Text(self.with_attachments(text, attachments));
        }
        let text_block = |text: String| ContentBlock::Text {
            text,
            extensions: Default::default(),
        };
        let positioned = attachments::Positioned {
            text: text.to_owned(),
            attachments: attachments.to_vec(),
        };
        let mut blocks = Vec::new();
        let mut prose = String::new();
        for piece in attachments::pieces(&positioned, &self.dir.join(dir::BLOBS)) {
            match piece {
                Piece::Text(text) => prose.push_str(text),
                Piece::Element {
                    element,
                    attachment,
                    path,
                } => {
                    prose.push_str(&element);
                    let Some(Of::Image(blob)) = &attachment.of else {
                        continue;
                    };
                    // The element names the file, so a model can still
                    // read an image whose bytes cannot be inlined.
                    let Some(bytes) = path.and_then(|path| std::fs::read(path).ok()) else {
                        continue;
                    };
                    blocks.push(text_block(std::mem::take(&mut prose)));
                    blocks.push(ContentBlock::Image {
                        source: ImageSource {
                            r#type: ImageSourceType::Base64,
                            media_type: blob.mime.clone(),
                            data: base64(&bytes),
                            extensions: Default::default(),
                        },
                        extensions: Default::default(),
                    });
                }
            }
        }
        if !prose.is_empty() {
            blocks.push(text_block(prose));
        }
        claude_stream::MessageContent::Blocks(blocks)
    }

    fn type_keys(&self, steps: Vec<claude::pty::keymap::KeyStep>) -> io::Result<()> {
        match &self.input {
            Input::Terminal(keys) => keys.send(steps).map_err(|_| closed()),
            _ => Err(closed()),
        }
    }

    pub fn set_messaging(&mut self, messaging: claude::hooks::MessagingCredentials) {
        self.messaging = Some(messaging);
    }

    /// An agent message for terminal Claude: onto its messaging socket,
    /// which queues it the way Claude queues a peer's message; pasted into
    /// the terminal when the socket is not known yet or refuses it.
    async fn message_terminal(&mut self, text: &str) -> io::Result<()> {
        if let Some(messaging) = &self.messaging {
            let sent = async {
                let mut socket = claude::messaging::MessagingSocket::connect(
                    &messaging.socket_path,
                    &messaging.token,
                )
                .await?;
                socket.send(text).await
            }
            .await;
            match sent {
                Ok(_) => return Ok(()),
                Err(error) => {
                    eprintln!("amux agent: the messaging socket failed ({error}); pasting")
                }
            }
        }
        let keys = self.keys.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "no messaging socket and no keymap to paste an agent message with",
            )
        })?;
        let steps = keys.prompt(&terminal::pasted_message(text))?;
        self.type_keys(steps)
    }

    async fn user_message(
        &mut self,
        uuid: &str,
        text: &str,
        attachments: &[Attachment],
    ) -> io::Result<()> {
        let line = claude_stream::Input::User(claude_stream::UserInput {
            message: claude_stream::MessageParam {
                role: claude_stream::Role::User,
                content: self.content(text, attachments),
                extensions: Default::default(),
            },
            parent_tool_use_id: None,
            session_id: self.session.clone(),
            uuid: Some(uuid.to_owned()),
            priority: None,
            extensions: Default::default(),
        });
        self.write_line(&claude_stream::encode(&line)).await
    }

    async fn write_line(&mut self, bytes: &[u8]) -> io::Result<()> {
        match &mut self.input {
            Input::Stdin(stdin) => write_line(stdin, bytes).await,
            Input::Codex(sender) => send_codex(sender, bytes).await,
            Input::Terminal(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "terminal Claude takes keystrokes, not lines",
            )),
            Input::Closed => Err(closed()),
        }
    }

    /// Reads a transcript's rows as they are written. A transcript an
    /// earlier incarnation followed is read on from where it stopped, so a
    /// resumed session's old rows are not read twice; any other from its
    /// start. The position is kept in private/ after every row.
    fn follow(&mut self, path: PathBuf) {
        let cursor = Cursor {
            file: self.dir.join(dir::PRIVATE).join(dir::TRANSCRIPT_CURSOR),
            offset: Arc::new(std::sync::Mutex::new(saved_cursor(
                &self.dir.join(dir::PRIVATE).join(dir::TRANSCRIPT_CURSOR),
                &path,
            ))),
            path,
        };
        let events = self.events.clone();
        let reading = cursor.clone();
        let task = tokio::spawn(async move {
            loop {
                for row in reading.new_rows() {
                    let length = row.len() + 1;
                    if row.is_empty() {
                        if !reading.advance(&events, length).await {
                            return;
                        }
                        continue;
                    }
                    let fact = Fact {
                        channel: Channel::Transcript,
                        payload: row,
                    };
                    // Sending is where an abort lands; the row counts as read
                    // only once it is sent.
                    if events.send(ProviderEvent::Fact(fact)).await.is_err() {
                        return;
                    }
                    if !reading.advance(&events, length).await {
                        return;
                    }
                }
                tokio::time::sleep(TRANSCRIPT_POLL).await;
            }
        });
        self.followers.push(Follower { cursor, task });
    }

    /// Stops following and returns the rows the followers had not read:
    /// the provider has exited, so its transcripts are complete, and rows
    /// it wrote just before exiting must not be lost to the poll interval.
    pub async fn finish_transcripts(&mut self) -> Vec<Fact> {
        let mut rows = Vec::new();
        for follower in std::mem::take(&mut self.followers) {
            follower.task.abort();
            let _ = follower.task.await;
            for row in follower.cursor.new_rows() {
                follower.cursor.advance(&self.events, row.len() + 1).await;
                if row.is_empty() {
                    continue;
                }
                rows.push(Fact {
                    channel: Channel::Transcript,
                    payload: row,
                });
            }
        }
        rows
    }

    /// Asks the provider to finish: end of input for a plain child, a
    /// terminate signal for a terminal one. Codex on a socket is sent its
    /// signal once its server has closed the agent's connection in turn.
    /// Asks the provider to finish: the end of its input, and SIGTERM, on
    /// which every provider cleans up the commands it runs. What runs below
    /// it is counted first, for `finish` to end what it leaves.
    pub fn close(&mut self) {
        if let Some(pid) = self.pid
            && self.census.is_empty()
        {
            self.census = tree::descendants(pid);
        }
        match std::mem::replace(&mut self.input, Input::Closed) {
            Input::Stdin(stdin) => {
                tokio::spawn(async move { stdin.lock().await.take() });
                // Headless Claude does not exit on the end of its input
                // while a background shell runs.
                #[cfg(unix)]
                if let Some(pid) = self.pid {
                    // SAFETY: kill only sends a signal.
                    unsafe {
                        libc::kill(pid as libc::pid_t, libc::SIGTERM);
                    }
                }
            }
            Input::Codex(sender) => {
                tokio::spawn(async move {
                    if let Some(mut sender) = sender.lock().await.take() {
                        let _ = sender.close().await;
                    }
                });
            }
            Input::Terminal(_) => {
                if let Some(terminal) = &self.terminal {
                    let _ = terminal.signal_process_group(pty_host::ProcessGroupSignal::Terminate);
                }
            }
            Input::Closed => {}
        }
    }

    /// The provider has exited after `close`: whatever it left running
    /// below it is killed.
    pub fn finish(&mut self) {
        tree::kill(&std::mem::take(&mut self.census));
    }

    /// Everything below the provider, then its process group, now.
    pub fn kill(&mut self) {
        if let Some(pid) = self.pid {
            self.census.extend(tree::descendants(pid));
        }
        self.finish();
        if let Some(terminal) = &self.terminal {
            let _ = terminal.signal_process_group(pty_host::ProcessGroupSignal::Kill);
            return;
        }
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // SAFETY: killpg only sends a signal.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        if let Some(kill) = self.kill.take() {
            let _ = kill.send(());
        }
    }

    /// Bytes an attached terminal typed, in order with the keystrokes the
    /// interpreter asked for.
    pub fn type_raw(&self, bytes: Vec<u8>) {
        let _ = self.type_keys(vec![claude::pty::keymap::KeyStep::Write(bytes)]);
    }

    /// Resizes the terminal, for a provider that has one.
    pub fn resize(&self, rows: u16, cols: u16) {
        if let Some(terminal) = &self.terminal {
            let _ = terminal.resize(pty_host::PtySize { rows, cols });
        }
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        for follower in &self.followers {
            follower.task.abort();
        }
    }
}

/// A Codex turn request with its attachments appended to `params.input`:
/// an image as a local image by its blob's path, anything else as the
/// element text the model reads, naming the file's path so the model can
/// open it.
fn codex_turn_input(
    request: &[u8],
    attachments: &[Attachment],
    blobs: &Path,
) -> io::Result<Vec<u8>> {
    let mut request = codex_protocol::decode_client(request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if let ClientMessage::Request {
        request: ClientRequest::ThreadInjectItems(params),
        ..
    } = &mut request
    {
        // An injected message takes text only: each attachment's element
        // goes at its place, naming the file by its path here.
        for item in &mut params.items {
            let InjectedItem::Message(message) = item else {
                continue;
            };
            for content in &mut message.content {
                let InjectedContent::InputText(text) = content else {
                    continue;
                };
                text.text = attachments::format(
                    &attachments::Positioned {
                        text: std::mem::take(&mut text.text),
                        attachments: attachments.to_vec(),
                    },
                    blobs,
                );
            }
        }
        return Ok(codex_protocol::encode(&request));
    }
    let input = match &mut request {
        ClientMessage::Request {
            request: ClientRequest::TurnStart(params),
            ..
        } => &mut params.input,
        ClientMessage::Request {
            request: ClientRequest::TurnSteer(params),
            ..
        } => &mut params.input,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a Codex turn request that is neither a turn start nor a steer",
            ));
        }
    };
    for attachment in attachments {
        // Every attachment is introduced by its element, which names it: a
        // bare local image reaches the model as "Image #1", and it cannot
        // tell which attached file is which.
        input.push(UserInput::text(attachments::element(
            attachment,
            attachments::blob_path(attachment, blobs).as_deref(),
        )));
        if let Some(wire::attachment::Of::Image(blob)) = &attachment.of {
            input.push(UserInput::LocalImage(LocalImageInput {
                path: blobs
                    .join(interpret::to_hex(&blob.hash))
                    .to_string_lossy()
                    .into_owned(),
                extra: Default::default(),
            }));
        }
    }
    Ok(codex_protocol::encode(&request))
}

/// A followed transcript and the task reading it.
struct Follower {
    cursor: Cursor,
    task: tokio::task::JoinHandle<()>,
}

/// How far a transcript has been read, kept in private/ as it moves.
#[derive(Clone)]
struct Cursor {
    path: PathBuf,
    file: PathBuf,
    offset: Arc<std::sync::Mutex<usize>>,
}

impl Cursor {
    /// The whole rows written past the cursor; a row still being written
    /// waits for the next read.
    fn new_rows(&self) -> Vec<Vec<u8>> {
        let offset = *self.offset.lock().expect("cursor lock");
        let Ok(bytes) = std::fs::read(&self.path) else {
            return Vec::new();
        };
        let Some(unread) = bytes.get(offset..) else {
            return Vec::new();
        };
        let Some(end) = unread.iter().rposition(|byte| *byte == b'\n') else {
            return Vec::new();
        };
        unread[..end]
            .split(|byte| *byte == b'\n')
            .map(<[u8]>::to_vec)
            .collect()
    }

    /// Moves past one row and its newline; an empty row is skipped the
    /// same way, and never sent. The position is the state a resumed
    /// session reads on from, so a position that cannot be kept ends the
    /// incarnation; false says it could not be.
    async fn advance(&self, events: &mpsc::Sender<ProviderEvent>, length: usize) -> bool {
        let saved = {
            let mut offset = self.offset.lock().expect("cursor lock");
            *offset += length;
            format!("{offset}\n{}", self.path.display())
        };
        if let Err(error) = std::fs::write(&self.file, saved) {
            let _ = events
                .send(ProviderEvent::WriteFailed {
                    path: self.file.clone(),
                    error,
                })
                .await;
            return false;
        }
        true
    }
}

/// Where an earlier follower of `path` stopped; the start for any other
/// file, or one that is shorter now.
fn saved_cursor(cursor: &Path, path: &Path) -> usize {
    let Ok(saved) = std::fs::read_to_string(cursor) else {
        return 0;
    };
    let Some((offset, saved_path)) = saved.split_once('\n') else {
        return 0;
    };
    let length = std::fs::metadata(path).map_or(0, |metadata| metadata.len() as usize);
    match offset.parse::<usize>() {
        Ok(offset) if Path::new(saved_path) == path && offset <= length => offset,
        _ => 0,
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the provider's input is closed")
}

/// Claude's launch arguments from the spec, less any `--settings`, plus
/// amux's tool server, and the one settings value the agent passes
/// instead: the user's settings with the agent's merged over them. Every
/// Claude accepts messages from other sessions at once, so a
/// bypass-permissions agent does not hold an agent message behind an
/// approval nobody is there to give, and runs amux's own tools without
/// asking; terminal Claude also reports its hook events to the agent
/// through the hook binary at the install path.
fn claude_launch(spec: &AgentSpec, dir: &Path, hooks: bool) -> io::Result<(Vec<String>, String)> {
    let mut args = spec.provider_args.clone();
    let tools = tool_server(spec, dir);
    if let Some((command, server_args)) = &tools {
        args.extend([
            "--mcp-config".to_owned(),
            serde_json::json!({
                "mcpServers": {
                    interpret::AMUX_TOOL_SERVER: { "command": command, "args": server_args },
                },
            })
            .to_string(),
        ]);
    }
    let sources = claude::launch::take_settings_args(&mut args).map_err(io::Error::other)?;
    let user = claude::launch::load_user_settings(Path::new(&spec.cwd), &sources)
        .map_err(io::Error::other)?;
    let config = spec.config.clone().unwrap_or_default();
    let managed = claude::launch::ManagedSettings {
        hook_command: if hooks && !config.install_path.is_empty() {
            vec![config.install_path, "hooks".into(), "claude".into()]
        } else {
            Vec::new()
        },
        hook_events: HOOK_EVENTS
            .iter()
            .map(|event| (*event).to_owned())
            .collect(),
        accept_cross_session: true,
        // The files amux attaches live in the agent's blob folder, outside
        // the working folder: without this Claude asks to read a file the
        // person just attached, naming only its hash path. `//` makes the
        // rule's path absolute.
        permissions_allow: tools
            .iter()
            .map(|_| format!("mcp__{}__*", interpret::AMUX_TOOL_SERVER))
            .chain([format!("Read(/{}/**)", dir.join(dir::BLOBS).display())])
            .collect(),
        ..Default::default()
    };
    let settings = claude::launch::merged_settings(user, &managed).into_value();
    Ok((args, settings.to_string()))
}

/// Codex's global arguments: the spec's, then amux's tool server as
/// configuration overrides in Codex's TOML (a JSON string or array of
/// strings is the same TOML value).
fn codex_args(spec: &AgentSpec, dir: &Path) -> Vec<String> {
    let mut args = spec.provider_args.clone();
    if let Some((server, server_args)) = tool_server(spec, dir) {
        let key = format!("mcp_servers.{}", interpret::AMUX_TOOL_SERVER);
        args.extend([
            "--config".to_owned(),
            format!("{key}.command={}", serde_json::json!(server)),
            "--config".to_owned(),
            format!("{key}.args={}", serde_json::json!(server_args)),
        ]);
    }
    args
}

/// Codex's own terminal app on the agent's live thread, on a terminal of
/// its own: one more client of the agent's app server, so what it does
/// reaches amux as what the server reports. It takes no configuration of
/// its own; the server has the agent's. None where the server has no
/// socket (Windows), which leaves no room for a second client.
pub fn codex_view(
    spec: &AgentSpec,
    dir: &Path,
    thread: &str,
    size: pty_host::PtySize,
) -> Option<pty_host::PtySpawn> {
    let args = codex::host::attach_args(thread, &codex_listen(dir).ok()?)?;
    Some(pty_host::PtySpawn {
        command: PathBuf::from(&spec.provider_command),
        args,
        cwd: PathBuf::from(&spec.cwd),
        env: spec
            .provider_env
            .iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
        env_remove: claude::launch::CHILD_SESSION_ENV_SCRUB
            .iter()
            .map(Into::into)
            .collect(),
        size,
    })
}

/// The Codex thread this agent runs, once its app server has made or
/// resumed one.
pub fn codex_thread(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(dir::PRIVATE).join(dir::PROVIDER_SESSION))
        .ok()
        .map(|thread| thread.trim().to_owned())
        .filter(|thread| !thread.is_empty())
}

/// amux's tool server as the harness launches it: the install path's
/// `mcp` subcommand on this agent's directory, which is how it finds the
/// socket that identifies it. None without an install path.
fn tool_server(spec: &AgentSpec, dir: &Path) -> Option<(String, Vec<String>)> {
    let install_path = spec.config.as_ref()?.install_path.clone();
    (!install_path.is_empty()).then(|| {
        (
            install_path,
            vec!["mcp".to_owned(), dir.display().to_string()],
        )
    })
}

/// The path a Unix socket in the agent directory is bound and dialled at,
/// for a child that dials it with no knowledge of the short links long
/// paths need.
fn socket_address(path: &Path) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        crate::local_socket::unix_address(path)
    }
    #[cfg(not(unix))]
    {
        Ok(path.to_owned())
    }
}

/// Where terminal Claude binds its messaging socket. Claude binds it itself
/// and refuses a directory reached through a symbolic link, so a path too
/// long for a socket moves to a real private directory rather than behind
/// the link the agent's own sockets use.
fn messaging_address(path: &Path) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        crate::local_socket::unix_private_address(path)
    }
    #[cfg(not(unix))]
    {
        Ok(path.to_owned())
    }
}

async fn send_codex(sender: &CodexSender, bytes: &[u8]) -> io::Result<()> {
    match sender.lock().await.as_mut() {
        Some(sender) => sender.send(bytes).await,
        None => Err(closed()),
    }
}

/// Asks a Codex server to finish, while it is still running.
fn terminate(child: &tokio::process::Child) {
    // A reaped child has no id, so this never signals a reused pid.
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: kill only sends a signal.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    let _ = child;
}

/// Where this agent's Codex server listens: a socket in its private
/// folder, which Codex's own app joins to attach; stdio on Windows.
pub fn codex_listen(dir: &Path) -> io::Result<codex::host::Listen> {
    #[cfg(unix)]
    {
        Ok(codex::host::Listen::Unix(socket_address(
            &dir.join(dir::PRIVATE).join(dir::CODEX_SOCK),
        )?))
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(codex::host::Listen::Stdio)
    }
}

async fn write_line(stdin: &Stdin, bytes: &[u8]) -> io::Result<()> {
    let mut stdin = stdin.lock().await;
    let Some(stdin) = stdin.as_mut() else {
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "the provider's input is closed",
        ));
    };
    stdin.write_all(bytes).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await
}

/// The provider's own session id for this agent, and whether this
/// incarnation continues it. The first incarnation makes one up; later ones
/// resume it.
fn provider_session(spec: &AgentSpec, dir: &Path) -> io::Result<(String, bool)> {
    let path = dir.join(dir::PRIVATE).join(dir::PROVIDER_SESSION);
    match std::fs::read_to_string(&path) {
        Ok(session) if !session.trim().is_empty() => {
            Ok((session.trim().to_owned(), spec.incarnation > 1))
        }
        Ok(_) => new_session(&path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => new_session(&path),
        Err(error) => Err(error),
    }
}

/// Claude's session for this agent, and whether this incarnation resumes
/// it. Claude writes a session's transcript once the session begins; one
/// that ended before (at its folder-trust dialog, or stopped before its
/// first prompt) has none, and Claude refuses to resume it. It starts under
/// the same id instead.
fn claude_session(spec: &AgentSpec, dir: &Path) -> io::Result<(String, bool)> {
    let (session, resume) = provider_session(spec, dir)?;
    if resume && !session_began(spec, &session) {
        eprintln!(
            "amux agent: Claude's session {session} never began; starting it rather than resuming"
        );
        return Ok((session, false));
    }
    Ok((session, resume))
}

/// Whether Claude has a transcript for this session: resuming needs one.
/// Claude keeps them under CLAUDE_CONFIG_DIR, as the spec or this process
/// sets it, else ~/.claude.
fn session_began(spec: &AgentSpec, session: &str) -> bool {
    const CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";
    let configured = spec
        .provider_env
        .get(CONFIG_DIR)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os(CONFIG_DIR).map(PathBuf::from));
    let Some(config) = configured
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
    else {
        return true;
    };
    claude::history::find_session_file(&config, Path::new(&spec.cwd), session).is_some()
}

fn new_session(path: &Path) -> io::Result<(String, bool)> {
    let session = uuid::Uuid::new_v4().to_string();
    std::fs::write(path, &session)?;
    Ok((session, false))
}

fn provider_log(dir: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(dir::PRIVATE).join(dir::PROVIDER_LOG))
}

/// The child's environment: ours without the variables a parent Claude
/// session sets, plus the spec's additions.
fn environment(command: &mut tokio::process::Command, spec: &AgentSpec) {
    for key in claude::launch::CHILD_SESSION_ENV_SCRUB {
        command.env_remove(key);
    }
    command.envs(&spec.provider_env);
}

/// Standard base64 with padding, as Claude's image blocks carry bytes.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = chunk.iter().enumerate().fold(0u32, |word, (index, byte)| {
            word | u32::from(*byte) << (16 - 8 * index)
        });
        for index in 0..4 {
            if index <= chunk.len() {
                encoded.push(ALPHABET[(word >> (18 - 6 * index) & 63) as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// A file reaches Codex as its element naming where the file is, so the
    /// model can open it; an image goes as its element and then as a local
    /// image by the same path.
    #[test]
    fn codex_attachments_name_their_blob_paths() {
        let blob = |hash: u8, name: &str, mime: &str| wire::BlobRef {
            hash: vec![hash; 32],
            name: name.into(),
            mime: mime.into(),
            size: 3,
        };
        let attachments = [
            wire::Attachment {
                of: Some(wire::attachment::Of::File(blob(
                    0xcd,
                    "report.pdf",
                    "application/pdf",
                ))),
            },
            wire::Attachment {
                of: Some(wire::attachment::Of::Image(blob(
                    0xab,
                    "chart.png",
                    "image/png",
                ))),
            },
        ];
        let blobs = Path::new("/agents/a/blobs");
        let request = br#"{"id":"amux-1","method":"turn/start","params":{"threadId":"t1","input":[{"type":"text","text":"read these","text_elements":[]}]}}"#;
        let written = super::codex_turn_input(request, &attachments, blobs).unwrap();
        let written: serde_json::Value = serde_json::from_slice(&written).unwrap();
        let input = written["params"]["input"].as_array().unwrap();
        let file = blobs.join("cd".repeat(32));
        assert_eq!(
            input[1]["text"],
            attachments::element(&attachments[0], Some(&file))
        );
        assert!(
            input[1]["text"]
                .as_str()
                .unwrap()
                .contains(&format!("path=\"{}\"", file.display()))
        );
        assert_eq!(
            input[2]["text"],
            attachments::element(&attachments[1], Some(&blobs.join("ab".repeat(32))))
        );
        assert_eq!(input[3]["type"], "localImage");
        assert_eq!(
            input[3]["path"],
            blobs.join("ab".repeat(32)).display().to_string()
        );
    }

    /// Codex is told each attached image's name, as it is told each file's:
    /// a bare local image by its hash path leaves the model calling them
    /// "Image #1", "Image #2", so it cannot answer which file is which.
    #[test]
    fn codex_is_told_each_attached_images_name() {
        let image = wire::Attachment {
            of: Some(wire::attachment::Of::Image(wire::BlobRef {
                hash: vec![0xab; 32],
                name: "red-copy.png".into(),
                mime: "image/png".into(),
                size: 3,
            })),
        };
        let request = br#"{"id":"amux-1","method":"turn/start","params":{"threadId":"t1","input":[{"type":"text","text":"which is which?","text_elements":[]}]}}"#;
        let written =
            super::codex_turn_input(request, &[image], Path::new("/agents/a/blobs")).unwrap();
        let written: serde_json::Value = serde_json::from_slice(&written).unwrap();
        let texts: Vec<&str> = written["params"]["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|input| input["text"].as_str())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("red-copy.png")),
            "nothing Codex reads names the image: {texts:?}"
        );
    }

    /// Claude may read the files amux attaches without asking: the agent's
    /// blob folder lies outside the working folder, so without a grant
    /// headless Claude asks permission to read a file the person just
    /// attached, naming only its hash path.
    #[test]
    fn claude_may_read_the_attachments_amux_gives_it() {
        let dir = std::env::temp_dir().join(format!("amux-triage-blobs-{}", std::process::id()));
        let spec = wire::AgentSpec {
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            ..Default::default()
        };
        let (args, settings) = super::claude_launch(&spec, &dir, false).unwrap();
        let blobs = dir.join("blobs").to_string_lossy().into_owned();
        assert!(
            args.iter().any(|arg| arg.contains(&blobs)) || settings.contains(&blobs),
            "neither the arguments nor the settings let Claude read {blobs}: {args:?} {settings}"
        );
    }

    #[test]
    fn base64_pads_as_the_standard_does() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"foob"), "Zm9vYg==");
    }
}
