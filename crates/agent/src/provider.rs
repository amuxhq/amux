//! The provider child: how it is started for each kind, how its output
//! becomes facts, and how the interpreter's effects reach it.
//!
//! Headless Claude is a plain child speaking stream-JSON on stdin and
//! stdout. Claude in a terminal runs on a pseudo-terminal whose raw bytes
//! go to the terminal log; its facts are hook payloads (on
//! private/hooks.sock) and its transcript rows. Codex's app server listens
//! on private/codex.sock with the agent as one of its clients. An ACP agent
//! is a plain child speaking JSON-RPC on stdin and stdout, on every
//! platform. Every child runs in its own process group so a kill reaches
//! whatever it started.
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

use base64::Engine as _;
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

use crate::tail::{Tail, Watch};
use crate::terminal::{self, KeyStep};
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
const CODEX_THREAD: &str = interpret::codex::THREAD_REQUEST;
/// The longest a starting Codex server is waited for to listen.
const CODEX_LISTEN_WAIT: Duration = Duration::from_secs(30);
/// How long the `taskkill /T` of an ACP agent behind a Windows shim has to
/// end its tree before the shim is killed regardless.
#[cfg(windows)]
const TREE_KILL_WAIT: Duration = Duration::from_secs(5);
/// How long an ACP agent has to answer each request of its handshake:
/// long enough for an agent that loads a lot at its start, or reaches a
/// slow network for its sign-in.
pub const ACP_HANDSHAKE_WAIT: Duration = Duration::from_secs(60);
/// How long a resume keeps asking for a thread another Codex server still
/// writes. A killed agent's server is told to finish by its tether, but
/// Codex can take seconds to exit and holds the thread until it does.
const CODEX_WRITER_WAIT: Duration = Duration::from_secs(30);
/// The pause before asking again for a thread another server holds.
const CODEX_WRITER_RETRY: Duration = Duration::from_millis(250);

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
    Messaging(crate::claude::hooks::MessagingCredentials),
    /// A write of the provider's own state into private/ failed. The state
    /// is what the next incarnation resumes from, so an incarnation that
    /// cannot record it ends rather than run on as if it had.
    WriteFailed {
        path: PathBuf,
        error: io::Error,
    },
    /// The provider runs but never got going, in these words: an ACP agent
    /// that did not answer its handshake in time. The host kills it and
    /// ends the incarnation as for one that could not be started.
    Unstarted(String),
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
    /// Terminal Claude is hosted on Unix only: ConPTY re-renders the child's
    /// output in its own escape sequences, and Claude's messaging socket and
    /// the fake's raw console mode are Unix-only. docs/ARCHITECTURE.md
    /// records the gap under "Windows, as a stated cost".
    #[error(
        "terminal Claude is not hosted on Windows in this build; headless Claude and Codex are"
    )]
    TerminalOnWindows,
    #[error(
        "terminal Claude needs Claude Code {} or later; this machine has {found}. Update Claude, or chat in amux",
        crate::claude::version::MIN_TERMINAL
    )]
    ClaudeTooOld { found: String },
    /// A later process of an ACP agent with a conversation whose saved id
    /// cannot be read: opening a new session would pass a fresh
    /// conversation off as the old one reopened.
    #[error(
        "the id of the agent's conversation, saved in {}, cannot be read, so the conversation cannot be reopened; amux does not start a new one in its place",
        Path::new(dir::PRIVATE).join(dir::PROVIDER_SESSION).display()
    )]
    SessionLost,
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// A plain child's stdin, shared with the handshake that writes first.
type Stdin = Arc<tokio::sync::Mutex<Option<ChildStdin>>>;
/// The agent's connection to its Codex server, shared the same way; None
/// once closed, or when the server ended before it took a client.
type CodexSender = Arc<tokio::sync::Mutex<Option<crate::codex::Sender>>>;

enum Input {
    Stdin(Stdin),
    Codex(CodexSender),
    /// Keystrokes for the typing task.
    Terminal(mpsc::UnboundedSender<Vec<KeyStep>>),
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
    messaging: Option<crate::claude::hooks::MessagingCredentials>,
    session: String,
    dir: PathBuf,
    events: mpsc::Sender<ProviderEvent>,
    followers: Vec<Follower>,
    /// The transcript followed last, beside which Claude writes its
    /// subagents' files.
    transcript: Option<PathBuf>,
    subagents: crate::claude::subagents::Followers,
    /// Every process below the provider when it was asked to finish, killed
    /// once it has exited if still running (see the tree module).
    census: Vec<tree::Descendant>,
    /// What an ACP agent's prompts may carry, as its `initialize` answer
    /// said; None for the other kinds.
    takes: Option<Arc<std::sync::Mutex<crate::acp::Takes>>>,
    /// An ACP agent's: the request that opened its session, as the
    /// handshake sent it, which the interpreter has sent again once a
    /// person may have signed in ([`Effect::AcpReopen`]).
    acp_opening: Option<Arc<std::sync::Mutex<Option<Vec<u8>>>>>,
    /// The child is a Windows batch file, as npm's `.cmd` shim is: the
    /// shell running it runs the program as its own child, so a kill ends
    /// the whole tree.
    #[cfg_attr(not(windows), allow(dead_code))]
    behind_shim: bool,
    /// The plain child's own process, for a kill that ends it at once.
    #[cfg_attr(not(windows), allow(dead_code))]
    process: Process,
}

/// A plain child's process, held for [`Provider::kill`]. On Windows, a
/// handle of its own to the child's process, duplicated from the child's,
/// so it stays valid after the task waiting on the child has reaped and
/// dropped it: terminating a process that has already exited through it
/// does nothing. Elsewhere nothing: a kill there signals the child's group.
#[derive(Default)]
struct Process(#[cfg(windows)] Option<std::os::windows::io::OwnedHandle>);

impl Process {
    fn of(child: &tokio::process::Child) -> Self {
        #[cfg(windows)]
        {
            use std::os::windows::io::BorrowedHandle;
            Self(child.raw_handle().and_then(|raw| {
                // SAFETY: the child is alive and owns this handle for the
                // length of the borrow, which only duplicates it.
                unsafe { BorrowedHandle::borrow_raw(raw) }
                    .try_clone_to_owned()
                    .ok()
            }))
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Self()
        }
    }

    /// Ends the process now, without waiting for it.
    #[cfg(windows)]
    fn terminate(&self) {
        use std::os::windows::io::AsRawHandle as _;
        if let Some(handle) = &self.0 {
            // SAFETY: the handle is this value's own, open until it drops.
            unsafe {
                windows_sys::Win32::System::Threading::TerminateProcess(handle.as_raw_handle(), 1);
            }
        }
    }
}

/// Asked of headless Claude so a command's row can open on what it is for
/// while the model is still writing the command: Claude streams a call's
/// input in the order the model writes its fields, and by its own tool
/// definition's order the description comes last, after commands that can
/// take the model many seconds to write.
const DESCRIPTION_FIRST: &str = "When you call the Bash tool, write its \"description\" field before its \"command\" field in the input JSON.";

/// Adds `text` to the system prompt the agent's own arguments append, so
/// neither flag can displace the other, or appends it alone.
fn append_system_prompt(args: &mut Vec<String>, text: &str) {
    const FLAG: &str = "--append-system-prompt";
    for index in 0..args.len() {
        if args[index] == FLAG
            && let Some(value) = args.get_mut(index + 1)
        {
            *value = format!("{value}\n\n{text}");
            return;
        }
        if let Some(value) = args[index]
            .strip_prefix(FLAG)
            .and_then(|rest| rest.strip_prefix('='))
        {
            args[index] = format!("{FLAG}={value}\n\n{text}");
            return;
        }
    }
    args.extend([FLAG.to_owned(), text.to_owned()]);
}

impl Provider {
    pub async fn spawn(
        spec: &AgentSpec,
        dir: &Path,
        events: mpsc::Sender<ProviderEvent>,
        clock: Arc<dyn crate::clock::Clock>,
    ) -> Result<Self, ProviderError> {
        match wire::Kind::try_from(spec.kind) {
            Ok(wire::Kind::ClaudeSdk) => Self::spawn_sdk(spec, dir, events).await,
            Ok(wire::Kind::ClaudePty) => Self::spawn_terminal(spec, dir, events).await,
            Ok(wire::Kind::Codex) => Self::spawn_codex(spec, dir, events).await,
            Ok(wire::Kind::Acp) => Self::spawn_acp(spec, dir, events, clock).await,
            Ok(wire::Kind::Unspecified) | Err(_) => {
                unreachable!("run refuses any other kind")
            }
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
        let (mut args, settings, withheld) = claude_launch(spec, dir, false)?;
        append_system_prompt(&mut args, DESCRIPTION_FIRST);
        if withheld {
            let _ = events
                .send(ProviderEvent::Fact(Fact {
                    channel: Channel::Agent,
                    payload: interpret::claude_pty::tools_withheld_fact(),
                }))
                .await;
        }
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
        let process = Process::of(&child);
        // Claude finishes its turn after its input closes: tethered, it is
        // asked to finish when the agent dies instead.
        #[cfg(unix)]
        let tether = match pty_host::Tether::new(&child) {
            Ok(tether) => tether,
            Err(source) => {
                let _ = child.start_kill();
                return Err(ProviderError::Spawn {
                    command: "Claude's tether".to_owned(),
                    source,
                });
            }
        };
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");

        let lines = events.clone();
        let cwd = PathBuf::from(&spec.cwd);
        let reader = tokio::spawn(async move {
            let mut stdout = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = stdout.next_line().await {
                let fact = Fact {
                    channel: Channel::Stream,
                    payload: crate::notebook::annotate_stream(line, &cwd).into_bytes(),
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
            #[cfg(unix)]
            tether.release().await;
            let _ = tokio::time::timeout(TRAILING_OUTPUT, reader).await;
            let code = status.ok().and_then(|status| status.code());
            let _ = exited.send(ProviderEvent::Exited(code)).await;
        });

        let mut provider = Self {
            input: Input::Stdin(Arc::new(tokio::sync::Mutex::new(Some(stdin)))),
            pid,
            kill: Some(kill),
            terminal: None,
            messaging: None,
            session,
            dir: dir.to_owned(),
            events,
            followers: Vec::new(),
            transcript: None,
            subagents: Default::default(),
            census: Vec::new(),
            takes: None,
            acp_opening: None,
            behind_shim: false,
            process,
        };
        // Claude reports nothing until it is asked; its answer is what says
        // it takes input. amux's interrupt ends the turn and leaves
        // background work running on every kind; Claude spares background
        // subagents on an interrupt, as it spares background shells, only
        // for a host that says it can stop each task on its own. The two
        // forwarding options put every subagent's whole conversation in the
        // stream, nested subagents' included; without them Claude streams
        // only direct subagents' tool calls.
        let initialize = claude_stream::ControlRequest::new(
            "agent-initialize",
            claude_stream::ControlRequestBody::Initialize(
                claude_stream::control::InitializeRequestBody {
                    per_task_stop_affordance: Some(true),
                    forward_subagent_text: Some(true),
                    forward_nested_subagent_tool_calls: Some(true),
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
        let mut child = crate::codex::spawn_server(command, &listen).map_err(|source| {
            ProviderError::Spawn {
                command: spec.provider_command.clone(),
                source,
            }
        })?;
        let pid = child.id();
        let process = Process::of(&child);
        #[cfg(unix)]
        let tether = match &listen {
            crate::codex::Listen::Unix(_) => match pty_host::Tether::new(&child) {
                Ok(tether) => Some(tether),
                Err(source) => {
                    let _ = child.start_kill();
                    return Err(ProviderError::Spawn {
                        command: "Codex's tether".to_owned(),
                        source,
                    });
                }
            },
            crate::codex::Listen::Stdio => None,
        };
        let connection =
            match crate::codex::Connection::connect(&listen, &mut child, CODEX_LISTEN_WAIT).await {
                Ok(connection) => Some(connection),
                // A server that ends at its start ends the incarnation as
                // any provider exit does, with its code.
                Err(crate::codex::ConnectError::Exited(_)) => None,
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
                    && (id == CODEX_THREAD || id == interpret::codex::RESUME_REQUEST)
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
                        if interpret::codex::starts_a_thread(&id) =>
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
        let on_socket = matches!(listen, crate::codex::Listen::Unix(_));
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
                        opt_out_notification_methods: interpret::codex::DROPPED_NOTIFICATIONS
                            .iter()
                            .map(|method| (*method).to_owned())
                            .collect(),
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
            // The resume leaves the turns out, as Codex's own app does: a
            // long thread's whole history can outgrow one message, and the
            // interpreter needs only the thread.
            let start = match thread {
                Some(thread_id) => ClientMessage::request(
                    RequestId::String(interpret::codex::RESUME_REQUEST.into()),
                    ClientRequest::ThreadResume(ThreadResumeParams {
                        thread_id,
                        cwd: Some(cwd),
                        model,
                        exclude_turns: Some(true),
                        ..Default::default()
                    }),
                ),
                None => ClientMessage::request(
                    RequestId::String(CODEX_THREAD.into()),
                    ClientRequest::ThreadStart(ThreadStartParams {
                        cwd: Some(cwd),
                        model,
                        ..Default::default()
                    }),
                ),
            };
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
            messaging: None,
            session: String::new(),
            dir: dir.to_owned(),
            events,
            followers: Vec::new(),
            transcript: None,
            subagents: Default::default(),
            census: Vec::new(),
            takes: None,
            acp_opening: None,
            behind_shim: false,
            process,
        })
    }

    /// An ACP agent: its command and arguments in the agent's folder, over
    /// stdio pipes, tethered and with the environment scrubbed as headless
    /// Claude is. The agent does the handshake, as it does Codex's:
    /// `initialize`, then the session the interpreter's [`opening`] picks
    /// from that answer and the session saved by the last incarnation. Every
    /// line the agent writes is a fact, the answers to the handshake
    /// included; the interpreter writes everything after it. What the agent
    /// writes on stderr is kept in its log and reaches the interpreter as a
    /// fact of the agent process's own; it is never taken as a failure, as
    /// Grok writes an error there for every remote tool server it cannot
    /// reach at each start.
    ///
    /// Each handshake request has [`ACP_HANDSHAKE_WAIT`] on `clock` to be
    /// answered, so a command that runs but never answers does not leave
    /// the agent starting for good. A later process whose saved session id
    /// is there but cannot be read is not started: it would open a new
    /// session in place of the old one. One whose id is gone altogether
    /// opens a new session, which the interpreter refuses where it knows
    /// of a conversation.
    ///
    /// [`opening`]: interpret::acp::opening
    async fn spawn_acp(
        spec: &AgentSpec,
        dir: &Path,
        events: mpsc::Sender<ProviderEvent>,
        clock: Arc<dyn crate::clock::Clock>,
    ) -> Result<Self, ProviderError> {
        let saved = saved_session(dir).filter(|_| spec.incarnation > 1);
        if spec.incarnation > 1
            && saved.is_none()
            && dir.join(dir::PRIVATE).join(dir::PROVIDER_SESSION).exists()
        {
            return Err(ProviderError::SessionLost);
        }
        let blobs = dir.join(dir::BLOBS);
        std::fs::create_dir_all(&blobs)?;
        let setup = crate::acp::SessionSetup {
            cwd: spec.cwd.clone(),
            // The files amux attaches live outside the working folder, in
            // the agent's blob folder, named by path in each prompt: an
            // agent that takes more folders may read them unasked, as
            // Claude is allowed to.
            folders: vec![blobs.display().to_string()],
            tool_server: tool_server(spec, dir),
        };
        let program = crate::acp::program(&spec.provider_command, &spec.provider_env);
        let behind_shim = program
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
            });
        let mut command = tokio::process::Command::new(program);
        command
            .args(&spec.provider_args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);
        environment(&mut command, spec);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|source| ProviderError::Spawn {
            command: spec.provider_command.clone(),
            source,
        })?;
        let pid = child.id();
        let process = Process::of(&child);
        // An agent may run on after its input closes: tethered, it is asked
        // to finish when the agent process dies instead.
        #[cfg(unix)]
        let tether = match pty_host::Tether::new(&child) {
            Ok(tether) => tether,
            Err(source) => {
                let _ = child.start_kill();
                return Err(ProviderError::Spawn {
                    command: "the ACP agent's tether".to_owned(),
                    source,
                });
            }
        };
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let takes = Arc::new(std::sync::Mutex::new(crate::acp::Takes::default()));

        let (initialized, on_initialized) = oneshot::channel();
        let (opened, on_opened) = oneshot::channel();
        let lines = events.clone();
        let read_takes = takes.clone();
        let reader = tokio::spawn(async move {
            let mut initialized = Some(initialized);
            let mut opened = Some(opened);
            let mut stdout = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = stdout.next_line().await {
                if opened.is_some()
                    && acp_session_answered(line.as_bytes())
                    && let Some(opened) = opened.take()
                {
                    let _ = opened.send(());
                }
                if initialized.is_some()
                    && let Some(answer) = acp_initialized(line.as_bytes())
                {
                    if let Some(init) = &answer {
                        let caps = init.agent_capabilities.as_ref();
                        *read_takes.lock().expect("never poisoned") = crate::acp::Takes::of(
                            caps.and_then(|caps| caps.prompt_capabilities.as_ref()),
                        );
                    }
                    if let Some(initialized) = initialized.take() {
                        let _ = initialized.send(answer);
                    }
                }
                let fact = Fact {
                    channel: Channel::Rpc,
                    payload: line.into_bytes(),
                };
                if lines.send(ProviderEvent::Fact(fact)).await.is_err() {
                    break;
                }
            }
        });
        let mut stderr_reader =
            tokio::spawn(acp_stderr(stderr, provider_log(dir)?, events.clone()));
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
            #[cfg(unix)]
            tether.release().await;
            let _ = tokio::time::timeout(TRAILING_OUTPUT, reader).await;
            // A straggler the agent left can hold its stderr open forever.
            if tokio::time::timeout(TRAILING_OUTPUT, &mut stderr_reader)
                .await
                .is_err()
            {
                stderr_reader.abort();
            }
            let code = status.ok().and_then(|status| status.code());
            let _ = exited.send(ProviderEvent::Exited(code)).await;
        });

        let stdin: Stdin = Arc::new(tokio::sync::Mutex::new(Some(stdin)));
        let handshake = stdin.clone();
        let opening_sent: Arc<std::sync::Mutex<Option<Vec<u8>>>> = Arc::default();
        let sent = opening_sent.clone();
        let unanswered = events.clone();
        let agent = if spec.provider.is_empty() {
            spec.provider_command.clone()
        } else {
            spec.provider.clone()
        };
        tokio::spawn(async move {
            let deadline = || {
                clock.sleep_until(
                    clock
                        .now_ms()
                        .saturating_add(ACP_HANDSHAKE_WAIT.as_millis() as i64),
                )
            };
            let late = |method: &str| {
                ProviderEvent::Unstarted(format!(
                    "{agent} did not answer {method} within {} seconds",
                    ACP_HANDSHAKE_WAIT.as_secs()
                ))
            };
            let initialize = crate::acp::initialize(interpret::acp::INITIALIZE_REQUEST);
            if write_line(&handshake, &initialize).await.is_err() {
                return;
            }
            // A refused or unreadable answer, or one in a version amux does
            // not speak, ends the incarnation in the interpreter, which
            // reads it too; so does an agent that ends first.
            let init = tokio::select! {
                answer = on_initialized => answer,
                () = deadline() => {
                    let _ = unanswered.send(late("initialize")).await;
                    return;
                }
            };
            let Ok(Some(init)) = init else {
                return;
            };
            if crate::acp::speaks("", &init).is_err() {
                return;
            }
            let opening = interpret::acp::opening(&init, saved.as_deref());
            let Some(request) =
                crate::acp::open_session(&opening, &setup, acp_takes_folders(&init))
            else {
                return;
            };
            *sent.lock().expect("never poisoned") = Some(request.clone());
            if write_line(&handshake, &request).await.is_err() {
                return;
            }
            let method = match opening {
                interpret::acp::Opening::Resume(_) => "session/resume",
                interpret::acp::Opening::Load(_) => "session/load",
                _ => "session/new",
            };
            tokio::select! {
                _ = on_opened => {}
                () = deadline() => {
                    let _ = unanswered.send(late(method)).await;
                }
            }
        });

        Ok(Self {
            input: Input::Stdin(stdin),
            pid,
            kill: Some(kill),
            terminal: None,
            messaging: None,
            session: String::new(),
            dir: dir.to_owned(),
            events,
            followers: Vec::new(),
            transcript: None,
            subagents: Default::default(),
            census: Vec::new(),
            takes: Some(takes),
            acp_opening: Some(opening_sent),
            behind_shim,
            process,
        })
    }

    /// Claude in a terminal. The agent reads its version first, refuses a
    /// Claude older than [`crate::claude::version::MIN_TERMINAL`], and reports it
    /// as the launch fact before anything Claude says. Its hooks report to private/hooks.sock; its
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
        // A version that cannot be read starts anyway: a wrapper or an odd
        // banner is likelier than an old Claude.
        match crate::claude::version::probe_version(Path::new(&spec.provider_command)).await {
            Ok(version) if version < crate::claude::version::MIN_TERMINAL => {
                return Err(ProviderError::ClaudeTooOld {
                    found: version.to_string(),
                });
            }
            Ok(_) => {}
            Err(error) => eprintln!("amux agent: reading the Claude version: {error}"),
        }
        let launch = interpret::claude_pty::launch_fact();
        let _ = events
            .send(ProviderEvent::Fact(Fact {
                channel: Channel::Agent,
                payload: launch,
            }))
            .await;
        let private = dir.join(dir::PRIVATE);
        let (mut args, settings, withheld) = claude_launch(spec, dir, true)?;
        if withheld {
            let _ = events
                .send(ProviderEvent::Fact(Fact {
                    channel: Channel::Agent,
                    payload: interpret::claude_pty::tools_withheld_fact(),
                }))
                .await;
        }
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
            crate::claude::hooks::HOOK_SOCKET_ENV.into(),
            socket_address(&private.join(dir::HOOKS_SOCK))?.into(),
        ));
        let process = pty_host::spawn(pty_host::PtySpawn {
            command: PathBuf::from(&spec.provider_command),
            args,
            cwd: PathBuf::from(&spec.cwd),
            env,
            env_remove: crate::claude::launch::CHILD_SESSION_ENV_SCRUB
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
            messaging: None,
            session,
            dir: dir.to_owned(),
            events,
            followers: Vec::new(),
            transcript: None,
            subagents: Default::default(),
            census: Vec::new(),
            takes: None,
            acp_opening: None,
            behind_shim: false,
            process: Process::default(),
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
                id,
                text,
                attachments,
                via: interpret::Carrier::Stdin,
            } => {
                let uuid = interpret::claude_sdk::client_uuid(&id);
                // Files another agent sent were brought into this agent's
                // blobs by its daemon, so their elements name paths here.
                self.user_message(&uuid, &text, &attachments).await
            }
            Effect::CodexTurnInput {
                request,
                attachments,
            } => {
                let request = codex_turn_input(&request, &attachments, &self.dir.join(dir::BLOBS))?;
                self.write_line(&request).await
            }
            Effect::AcpPrompt {
                request,
                attachments,
            } => {
                let takes = self
                    .takes
                    .as_ref()
                    .map(|takes| *takes.lock().expect("never poisoned"))
                    .unwrap_or_default();
                let request = crate::acp::prompt_with_attachments(
                    &request,
                    &attachments,
                    &self.dir.join(dir::BLOBS),
                    takes,
                )?;
                self.write_line(&request).await
            }
            Effect::ProviderSession { id } => {
                self.adopt_session(&id).await;
                Ok(())
            }
            Effect::AcpReopen => {
                let request = self
                    .acp_opening
                    .as_ref()
                    .and_then(|sent| sent.lock().expect("never poisoned").clone());
                match request {
                    Some(request) => self.write_line(&request).await,
                    // The handshake has not sent it yet; it will.
                    None => Ok(()),
                }
            }
            Effect::FollowTranscript { path } => {
                let path = PathBuf::from(path);
                // A transcript is named by its session. A new one (a
                // `/clear`, or a resume Claude ran itself) is the session
                // the next incarnation resumes.
                if let Some(session) = path.file_stem().and_then(|stem| stem.to_str()) {
                    self.adopt_session(session).await;
                }
                self.transcript = Some(path.clone());
                self.follow(path);
                Ok(())
            }
            Effect::FollowSubagent { agent_id } => {
                if let Some(transcript) = &self.transcript {
                    self.subagents
                        .follow(&self.dir, transcript, &agent_id, &self.events);
                }
                Ok(())
            }
            Effect::DrainSubagent {
                agent_id,
                last_reply,
            } => {
                self.subagents.drain(&agent_id, last_reply, &self.events);
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
                let steps = terminal::steps(&input, &text)?;
                self.type_keys(steps)
            }
            Effect::Inject {
                text,
                attachments,
                via: interpret::Carrier::MessagingSocket,
                ..
            } => {
                let text = self.with_attachments(&text, &attachments);
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

        let images = attachments
            .iter()
            .any(|attachment| !attachments::images(attachment).is_empty());
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
        let blobs = self.dir.join(dir::BLOBS);
        for piece in attachments::pieces(&positioned, &blobs) {
            match piece {
                Piece::Text(text) => prose.push_str(text),
                Piece::Element {
                    element,
                    attachment,
                    ..
                } => {
                    prose.push_str(&element);
                    for image in attachments::images(attachment) {
                        // The element names the file, so a model can still
                        // read an image whose bytes cannot be inlined, or
                        // are not an image Claude reads.
                        let path = blobs.join(interpret::to_hex(&image.hash));
                        let Ok(bytes) = std::fs::read(path) else {
                            continue;
                        };
                        let Some(media_type) = attachments::native_image(&bytes) else {
                            continue;
                        };
                        blocks.push(text_block(std::mem::take(&mut prose)));
                        blocks.push(ContentBlock::Image {
                            source: ImageSource {
                                r#type: ImageSourceType::Base64,
                                media_type: media_type.to_owned(),
                                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                                extensions: Default::default(),
                            },
                            extensions: Default::default(),
                        });
                    }
                }
            }
        }
        if !prose.is_empty() {
            blocks.push(text_block(prose));
        }
        claude_stream::MessageContent::Blocks(blocks)
    }

    fn type_keys(&self, steps: Vec<KeyStep>) -> io::Result<()> {
        match &self.input {
            Input::Terminal(keys) => keys.send(steps).map_err(|_| closed()),
            _ => Err(closed()),
        }
    }

    pub fn set_messaging(&mut self, messaging: crate::claude::hooks::MessagingCredentials) {
        self.messaging = Some(messaging);
    }

    /// An agent message for terminal Claude: onto its messaging socket,
    /// which queues it the way Claude queues a peer's message; pasted into
    /// the terminal when the socket is not known yet or refuses it.
    async fn message_terminal(&mut self, text: &str) -> io::Result<()> {
        if let Some(messaging) = &self.messaging {
            let sent = async {
                let mut socket = crate::claude::messaging::MessagingSocket::connect(
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
        let steps = terminal::prompt(&terminal::pasted_message(text))?;
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

    /// Claude's session is now `session`: the one the next incarnation
    /// resumes, saved when it differs from the saved one, and the one later
    /// user messages name.
    async fn adopt_session(&mut self, session: &str) {
        let file = self.dir.join(dir::PRIVATE).join(dir::PROVIDER_SESSION);
        if std::fs::read_to_string(&file)
            .ok()
            .as_deref()
            .map(str::trim)
            != Some(session)
        {
            persist(&self.events, &file, session).await;
        }
        session.clone_into(&mut self.session);
    }

    /// Reads a transcript's rows as they are written. A transcript an
    /// earlier incarnation followed is read on from where it stopped, so a
    /// resumed session's old rows are not read twice; any other from its
    /// start. The position is kept in private/ after every row.
    fn follow(&mut self, path: PathBuf) {
        let file = self.dir.join(dir::PRIVATE).join(dir::TRANSCRIPT_CURSOR);
        let tail = Tail::new(file, path);
        let task = tokio::spawn(follow_transcript(
            tail.clone(),
            tail.watch(),
            self.events.clone(),
        ));
        self.followers.push(Follower { tail, task });
    }

    /// Stops following and returns the rows the followers had not read,
    /// the agent's own and then its subagents': the provider has exited, so
    /// its transcripts are complete, and rows it wrote just before exiting
    /// must not be lost to the poll interval.
    pub async fn finish_transcripts(&mut self) -> Vec<Fact> {
        let mut rows = Vec::new();
        for follower in std::mem::take(&mut self.followers) {
            follower.task.abort();
            let _ = follower.task.await;
            // A file that cannot be read on has no rows to add; its
            // follower said why when it ended.
            for row in follower.tail.unread().unwrap_or_default() {
                follower.tail.advance(&self.events, row.len() + 1).await;
                if row.is_empty() {
                    continue;
                }
                rows.push(Fact {
                    channel: Channel::Transcript,
                    payload: row,
                });
            }
        }
        rows.extend(self.subagents.finish(&self.events).await);
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
            // Windows has no process groups. An ACP agent npm installed
            // runs behind a `.cmd` shim whose shell runs the agent as its
            // child, so killing the shim alone would leave the agent
            // running: that tree is ended by `taskkill /T`, started here
            // and not waited for, since the stop the daemon asked waits on
            // this agent process's exit within seconds and taskkill can take
            // that long on a loaded machine. The shim itself is killed only
            // once taskkill is done (or has had its time), since a tree
            // whose root has gone can no longer be found from it. Started,
            // taskkill runs on even if this process ends first.
            #[cfg(windows)]
            if self.behind_shim
                && let Some(pid) = self.pid
                && let Some(mut tree) = crate::acp::start_killing_tree(pid)
            {
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(TREE_KILL_WAIT, tree.wait()).await;
                    let _ = kill.send(());
                });
                return;
            }
            // Any other child ends here, before this returns: the agent
            // process exits right after a kill, which can drop the task
            // waiting on the child before that task has killed it, and the
            // child would outlive the agent, holding its folder and the
            // handles it inherited. The task then sees it exit.
            #[cfg(windows)]
            self.process.terminate();
            let _ = kill.send(());
        }
    }

    /// Bytes an attached terminal typed, in order with the keystrokes the
    /// interpreter asked for.
    pub fn type_raw(&self, bytes: Vec<u8>) {
        let _ = self.type_keys(vec![KeyStep::Write(bytes)]);
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
        input.push(UserInput::text(attachments::element_in(attachment, blobs)));
        // Codex reads only some image types and swaps any other for a note
        // the person never sees; the element names that file instead.
        for blob in attachments::images(attachment) {
            let path = blobs.join(interpret::to_hex(&blob.hash));
            if attachments::native_image_at(&path).is_some() {
                input.push(UserInput::LocalImage(LocalImageInput {
                    path: path.to_string_lossy().into_owned(),
                    extra: Default::default(),
                }));
            }
        }
    }
    Ok(codex_protocol::encode(&request))
}

/// An ACP agent's answer to the agent's `initialize`, when `line` is one:
/// what it says it offers, or None for a refusal or an answer that does
/// not read as one.
fn acp_initialized(line: &[u8]) -> Option<Option<acp_protocol::agent::InitializeResponse>> {
    let acp_protocol::AgentMessage::Response {
        id: acp_protocol::RequestId::String(id),
        result,
        ..
    } = acp_protocol::decode(line).ok()?
    else {
        return None;
    };
    (id == interpret::acp::INITIALIZE_REQUEST).then(|| {
        result
            .ok()
            .and_then(|result| acp_protocol::result(&result).ok())
    })
}

/// Whether `line` is an ACP agent's answer, of any kind, to the request
/// that opens its session.
fn acp_session_answered(line: &[u8]) -> bool {
    use interpret::acp::{LOAD_REQUEST, NEW_SESSION_REQUEST, RESUME_REQUEST};
    matches!(
        acp_protocol::decode(line),
        Ok(acp_protocol::AgentMessage::Response {
            id: acp_protocol::RequestId::String(id),
            ..
        }) if [NEW_SESSION_REQUEST, RESUME_REQUEST, LOAD_REQUEST].contains(&id.as_str())
    )
}

/// Whether an ACP agent takes more folders than its working one.
fn acp_takes_folders(init: &acp_protocol::agent::InitializeResponse) -> bool {
    init.agent_capabilities.as_ref().is_some_and(|caps| {
        // Antigravity writes each capability again in snake case.
        [&caps.session_capabilities, &caps.session_capabilities_again]
            .into_iter()
            .flatten()
            .any(|session| matches!(session.additional_directories, Some(Some(_))))
    })
}

/// The longest stderr line an ACP agent's interpreter is told of; its log
/// keeps the whole line.
const STDERR_LINE_CAP: usize = 2048;

/// Copies an ACP agent's stderr into its log, line by line, and tells the
/// interpreter of each line.
async fn acp_stderr(
    stderr: tokio::process::ChildStderr,
    log: std::fs::File,
    events: mpsc::Sender<ProviderEvent>,
) {
    let mut log = tokio::fs::File::from_std(log);
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let _ = log.write_all(line.as_bytes()).await;
        let _ = log.write_all(b"\n").await;
        let mut end = line.len().min(STDERR_LINE_CAP);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        let fact = Fact {
            channel: Channel::Agent,
            payload: interpret::acp::stderr_fact(&line[..end]),
        };
        if events.send(ProviderEvent::Fact(fact)).await.is_err() {
            break;
        }
    }
    let _ = log.flush().await;
}

/// A followed transcript and the task reading it.
struct Follower {
    tail: Tail,
    task: tokio::task::JoinHandle<()>,
}

/// Sends a transcript's rows as `watch` says it grew, until the agent
/// stops reading or the file cannot be read on.
async fn follow_transcript(tail: Tail, mut watch: Watch, events: mpsc::Sender<ProviderEvent>) {
    loop {
        let rows = match tail.unread() {
            Ok(rows) => rows,
            Err(error) => {
                eprintln!(
                    "amux agent: following {} no further: {error}",
                    tail.path().display()
                );
                return;
            }
        };
        for row in rows {
            let length = row.len() + 1;
            if !row.is_empty() {
                let fact = Fact {
                    channel: Channel::Transcript,
                    payload: row,
                };
                // Sending is where an abort lands; the row counts as read
                // only once it is sent.
                if events.send(ProviderEvent::Fact(fact)).await.is_err() {
                    return;
                }
            }
            if !tail.advance(&events, length).await {
                return;
            }
        }
        watch.changed().await;
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the provider's input is closed")
}

/// Claude's launch arguments from the spec, less any `--settings`, plus
/// amux's tool server, and the one settings value the agent passes
/// instead: the user's settings with the agent's merged over them; and
/// whether amux's tool server was left out because the organization
/// manages Claude's MCP servers, so Claude would refuse to start with it. Every
/// Claude accepts messages from other sessions at once, so a
/// bypass-permissions agent does not hold an agent message behind an
/// approval nobody is there to give, and runs amux's own tools, all but
/// `attach`, without asking; terminal Claude also reports its hook events to the agent
/// through the hook binary at the install path.
fn claude_launch(
    spec: &AgentSpec,
    dir: &Path,
    hooks: bool,
) -> io::Result<(Vec<String>, String, bool)> {
    let mut args = spec.provider_args.clone();
    let managed_folder = spec
        .provider_env
        .get(crate::claude::launch::MANAGED_SETTINGS_PATH_ENV)
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(crate::claude::launch::MANAGED_SETTINGS_PATH_ENV).map(PathBuf::from)
        });
    let mut tools = tool_server(spec, dir);
    let withheld = tools.is_some() && crate::claude::launch::managed_mcp(managed_folder.as_deref());
    if withheld {
        tools = None;
    }
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
    let sources = crate::claude::launch::take_settings_args(&mut args).map_err(io::Error::other)?;
    let user = crate::claude::launch::load_user_settings(Path::new(&spec.cwd), &sources)
        .map_err(io::Error::other)?;
    let config = spec.config.clone().unwrap_or_default();
    let managed = crate::claude::launch::ManagedSettings {
        hook_command: if hooks && !config.install_path.is_empty() {
            vec![
                config.install_path,
                "internal".into(),
                "hook".into(),
                "claude".into(),
            ]
        } else {
            Vec::new()
        },
        accept_cross_session: true,
        // amux's tools by name, so one that asks first is left out. The
        // files amux attaches live in the agent's blob folder, outside the
        // working folder: without the read rule Claude asks to read a file
        // the person just attached, naming only its hash path. `//` makes
        // the rule's path absolute.
        permissions_allow: tools
            .iter()
            .flat_map(|_| crate::tools::unasked())
            .map(|tool| format!("mcp__{}__{tool}", interpret::AMUX_TOOL_SERVER))
            .chain([format!("Read(/{}/**)", rule_path(&dir.join(dir::BLOBS)))])
            .collect(),
    };
    let settings = crate::claude::launch::merged_settings(user, &managed);
    Ok((args, settings.to_string(), withheld))
}

/// An absolute path as Claude's permission rules name it: Claude matches
/// rules against paths in POSIX form, so on Windows `C:\Users\alice` is
/// `/c/Users/alice`. Written with the native separator, a Windows rule
/// matched no file and Claude asked before reading every attachment.
fn rule_path(path: &Path) -> String {
    use std::path::{Component, Prefix};
    let mut rule = String::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                    rule.push('/');
                    rule.push(char::from(drive).to_ascii_lowercase());
                }
                _ => rule.push_str(&prefix.as_os_str().to_string_lossy().replace('\\', "/")),
            },
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => rule.push_str("/.."),
            Component::Normal(name) => {
                rule.push('/');
                rule.push_str(&name.to_string_lossy());
            }
        }
    }
    rule
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
    let args = crate::codex::attach_args(thread, &codex_listen(dir).ok()?)?;
    Some(pty_host::PtySpawn {
        command: PathBuf::from(&spec.provider_command),
        args,
        cwd: PathBuf::from(&spec.cwd),
        env: spec
            .provider_env
            .iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
        env_remove: crate::claude::launch::CHILD_SESSION_ENV_SCRUB
            .iter()
            .map(Into::into)
            .collect(),
        size,
    })
}

/// The Codex thread this agent runs, once its app server has made or
/// resumed one.
pub fn codex_thread(dir: &Path) -> Option<String> {
    saved_session(dir)
}

/// The provider's own id for this agent's conversation, as last saved.
fn saved_session(dir: &Path) -> Option<String> {
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
            vec![
                "internal".to_owned(),
                "mcp".to_owned(),
                dir.display().to_string(),
            ],
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
pub fn codex_listen(dir: &Path) -> io::Result<crate::codex::Listen> {
    #[cfg(unix)]
    {
        Ok(crate::codex::Listen::Unix(socket_address(
            &dir.join(dir::PRIVATE).join(dir::CODEX_SOCK),
        )?))
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(crate::codex::Listen::Stdio)
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
/// it, in both modes. Claude writes a session's transcript at its first
/// message; one that ended before (at its folder-trust dialog, or stopped
/// before its first prompt) has none, and starts under the same id. One
/// stopped as it wrote its first message can hold only metadata, which
/// Claude neither resumes nor starts again under its id: a new id replaces
/// it.
fn claude_session(spec: &AgentSpec, dir: &Path) -> io::Result<(String, bool)> {
    let (session, resume) = provider_session(spec, dir)?;
    if !resume {
        return Ok((session, false));
    }
    match session_file(spec, &session) {
        crate::claude::history::SessionFile::Conversation => Ok((session, true)),
        crate::claude::history::SessionFile::Missing => {
            eprintln!(
                "amux agent: Claude's session {session} never began; starting it rather than resuming"
            );
            Ok((session, false))
        }
        crate::claude::history::SessionFile::Empty => {
            let (new, resume) = new_session(&dir.join(dir::PRIVATE).join(dir::PROVIDER_SESSION))?;
            eprintln!(
                "amux agent: Claude's session {session} holds no conversation; starting session {new} instead"
            );
            Ok((new, resume))
        }
    }
}

/// What Claude has for this session. Claude keeps its transcripts under
/// CLAUDE_CONFIG_DIR, as the spec or this process sets it, else ~/.claude;
/// with neither known, the session is taken to have one.
fn session_file(spec: &AgentSpec, session: &str) -> crate::claude::history::SessionFile {
    let Some(config) = crate::claude::history::config_root(&spec.provider_env) else {
        return crate::claude::history::SessionFile::Conversation;
    };
    crate::claude::history::session_file(&config, Path::new(&spec.cwd), session)
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
    for key in crate::claude::launch::CHILD_SESSION_ENV_SCRUB {
        command.env_remove(key);
    }
    command.envs(&spec.provider_env);
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use interpret::Channel;
    use tokio::sync::mpsc;

    use crate::provider::ProviderEvent;
    use crate::tail::{Tail, Watch};

    /// A transcript made after following began, in a folder that did not
    /// exist either, is read as it appears and as it grows, each row once,
    /// whole and in order however its writes split it, on the watcher's
    /// reports alone: the fallback is an hour.
    #[tokio::test]
    async fn a_transcript_made_after_following_began_is_read_as_it_is_written() {
        use std::io::Write as _;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("project").join("s1.jsonl");
        let tail = Tail::new(root.path().join("cursor"), path.clone());
        let watch = Watch::with_fallback(path.clone(), Duration::from_secs(3600));
        let (events, mut sent) = mpsc::channel(4);
        let follower = tokio::spawn(super::follow_transcript(tail, watch, events));
        let append = |text: &str| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        };
        let mut next = async || match tokio::time::timeout(patience::PATIENCE, sent.recv())
            .await
            .expect("a row in time")
            .expect("the follower running")
        {
            ProviderEvent::Fact(fact) => {
                assert_eq!(fact.channel, Channel::Transcript);
                String::from_utf8(fact.payload).unwrap()
            }
            other => panic!("not a row: {other:?}"),
        };

        std::fs::create_dir(root.path().join("project")).unwrap();
        append("{\"a\":");
        append("1}\n");
        assert_eq!(next().await, "{\"a\":1}");
        append("{\"b\":2}\n\n{\"c\"");
        assert_eq!(next().await, "{\"b\":2}", "an empty row is not sent");
        append(":3}\n");
        assert_eq!(next().await, "{\"c\":3}");
        follower.abort();
    }

    /// amux's line joins a prompt the agent's own arguments append, in
    /// either spelling of the flag, and stands alone otherwise.
    #[test]
    fn the_description_first_line_joins_an_appended_prompt() {
        let strings = |args: &[&str]| args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let mut args = strings(&["--model", "opus"]);
        super::append_system_prompt(&mut args, "B");
        assert_eq!(
            args,
            strings(&["--model", "opus", "--append-system-prompt", "B"])
        );
        let mut args = strings(&["--append-system-prompt", "A", "--model", "opus"]);
        super::append_system_prompt(&mut args, "B");
        assert_eq!(
            args,
            strings(&["--append-system-prompt", "A\n\nB", "--model", "opus"])
        );
        let mut args = strings(&["--append-system-prompt=A"]);
        super::append_system_prompt(&mut args, "B");
        assert_eq!(args, strings(&["--append-system-prompt=A\n\nB"]));
    }

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
                ..Default::default()
            },
            wire::Attachment {
                of: Some(wire::attachment::Of::Image(blob(
                    0xab,
                    "chart.png",
                    "image/png",
                ))),
                ..Default::default()
            },
        ];
        let dir = tempfile::tempdir().unwrap();
        let blobs = dir.path();
        std::fs::write(blobs.join("ab".repeat(32)), b"\x89PNG\r\n\x1a\n").unwrap();
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

    /// A page review's screenshots go after its element as local images,
    /// and the element names each one's path.
    #[test]
    fn codex_sees_a_page_reviews_screenshots() {
        let blobs = tempfile::tempdir().unwrap();
        std::fs::write(blobs.path().join("ab".repeat(32)), b"\x89PNG\r\n\x1a\n").unwrap();
        let comment = |text: &str, image: Option<wire::BlobRef>| wire::PageComment {
            text: text.into(),
            element: Some(wire::PageElement {
                selector: "main".into(),
                ..Default::default()
            }),
            quote: String::new(),
            image,
        };
        let page = wire::Attachment {
            of: Some(wire::attachment::Of::Page(wire::PageReview {
                url: "http://localhost:5173/".into(),
                title: "Home".into(),
                comments: vec![
                    comment("words only", None),
                    comment(
                        "this gap",
                        Some(wire::BlobRef {
                            hash: vec![0xab; 32],
                            name: "region-2.png".into(),
                            mime: "image/png".into(),
                            size: 8,
                        }),
                    ),
                ],
            })),
            ..Default::default()
        };
        let request = br#"{"id":"amux-1","method":"turn/start","params":{"threadId":"t1","input":[{"type":"text","text":"fix","text_elements":[]}]}}"#;
        let written =
            super::codex_turn_input(request, std::slice::from_ref(&page), blobs.path()).unwrap();
        let written: serde_json::Value = serde_json::from_slice(&written).unwrap();
        let input = written["params"]["input"].as_array().unwrap();
        let path = blobs.path().join("ab".repeat(32)).display().to_string();
        assert_eq!(
            input[1]["text"],
            attachments::element_in(&page, blobs.path())
        );
        assert!(
            input[1]["text"]
                .as_str()
                .unwrap()
                .contains(&format!("image-path: {path}"))
        );
        assert_eq!(input[2]["type"], "localImage");
        assert_eq!(input[2]["path"], path);
        assert_eq!(input.len(), 3);
    }

    /// Codex gets a local image only for bytes it reads natively: an SVG
    /// labelled an image goes as the file its element names.
    #[test]
    fn codex_gets_a_local_image_only_for_bytes_it_reads() {
        let blobs = tempfile::tempdir().unwrap();
        let image = |hash: u8, bytes: &[u8], name: &str, mime: &str| {
            std::fs::write(blobs.path().join(interpret::to_hex(&[hash; 32])), bytes).unwrap();
            wire::Attachment {
                of: Some(wire::attachment::Of::Image(wire::BlobRef {
                    hash: vec![hash; 32],
                    name: name.into(),
                    mime: mime.into(),
                    size: bytes.len() as u64,
                })),
                ..Default::default()
            }
        };
        let attachments = [
            image(0xab, b"GIF89a a frame", "wave.gif", "image/gif"),
            image(0xcd, b"<svg/>", "logo.svg", "image/svg+xml"),
        ];
        let request = br#"{"id":"amux-1","method":"turn/start","params":{"threadId":"t1","input":[{"type":"text","text":"look","text_elements":[]}]}}"#;
        let written = super::codex_turn_input(request, &attachments, blobs.path()).unwrap();
        let written: serde_json::Value = serde_json::from_slice(&written).unwrap();
        let images: Vec<&str> = written["params"]["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|input| input["type"] == "localImage")
            .map(|input| input["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            images,
            [blobs.path().join("ab".repeat(32)).display().to_string()]
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
            ..Default::default()
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

    /// A file another agent sent reaches Codex as its element at its place
    /// in the injected message, naming the copy in this agent's blobs: the
    /// sender's path is on another host.
    #[test]
    fn codex_reads_an_agents_file_at_its_path_here() {
        let file = wire::Attachment {
            of: Some(wire::attachment::Of::File(wire::BlobRef {
                hash: vec![0xcd; 32],
                name: "notes.txt".into(),
                mime: "text/plain".into(),
                size: 4,
            })),
            ..Default::default()
        };
        let request = serde_json::json!({
            "id": "amux-2",
            "method": "thread/inject_items",
            "params": {
                "threadId": "t1",
                "items": [{
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": format!(
                            "[message from agent lead]\nPlease read {}",
                            attachments::PLACEHOLDER
                        ),
                    }],
                }],
            },
        });
        let blobs = Path::new("/agents/helper/blobs");
        let written =
            super::codex_turn_input(request.to_string().as_bytes(), &[file], blobs).unwrap();
        let written: serde_json::Value = serde_json::from_slice(&written).unwrap();
        let text = written["params"]["items"][0]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(
            text.starts_with("[message from agent lead]\nPlease read <amux-attachment")
                && text.contains(&blobs.join("cd".repeat(32)).display().to_string()),
            "the injected message does not name the file here: {text}"
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
        let (_, settings, _) = super::claude_launch(&spec, &dir, false).unwrap();
        let settings: serde_json::Value = serde_json::from_str(&settings).unwrap();
        let rule = format!("Read(/{}/**)", super::rule_path(&dir.join("blobs")));
        assert!(
            settings["permissions"]["allow"]
                .as_array()
                .is_some_and(|allow| allow.iter().any(|allowed| *allowed == rule)),
            "the settings do not let Claude read the blobs with {rule}: {settings}"
        );
    }

    /// amux's tools are allowed by name, all but `attach`: it reads any file
    /// the agent names into the blob folder Claude reads unasked, so it goes
    /// through Claude's own permission check like any other read.
    #[test]
    fn claude_asks_before_attaching_and_runs_the_other_tools_unasked() {
        let dir = std::env::temp_dir().join(format!("amux-allow-{}", std::process::id()));
        let spec = wire::AgentSpec {
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            provider_env: [(
                crate::claude::launch::MANAGED_SETTINGS_PATH_ENV.to_owned(),
                dir.join("no-managed-settings").display().to_string(),
            )]
            .into(),
            config: Some(wire::EffectiveConfig {
                install_path: "/opt/amux".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (_, settings, withheld) = super::claude_launch(&spec, &dir, false).unwrap();
        assert!(!withheld);
        let settings: serde_json::Value = serde_json::from_str(&settings).unwrap();
        let allow: Vec<&str> = settings["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|rule| rule.as_str())
            .filter(|rule| rule.starts_with("mcp__"))
            .collect();
        assert_eq!(
            allow,
            ["agents", "hosts", "send", "spawn", "stop", "status"]
                .map(|tool| format!("mcp__amux__{tool}"))
        );
    }

    /// Claude matches permission rules against POSIX paths, so a Windows
    /// path's drive becomes its first folder and its separators slashes.
    #[test]
    fn a_rule_names_a_path_as_claude_matches_it() {
        #[cfg(unix)]
        assert_eq!(
            super::rule_path(Path::new("/agents/a/blobs")),
            "/agents/a/blobs"
        );
        #[cfg(windows)]
        {
            assert_eq!(
                super::rule_path(Path::new(r"C:\Users\RUNNER~1\AppData\blobs")),
                "/c/Users/RUNNER~1/AppData/blobs"
            );
            assert_eq!(
                super::rule_path(Path::new(r"\\?\D:\agents\a\blobs")),
                "/d/agents/a/blobs"
            );
        }
    }
}
