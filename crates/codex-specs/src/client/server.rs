use std::sync::Arc;

use codex_protocol::client::{
    AccountReadParams, Capabilities, ClientInfo, InitializeParams, ThreadReadParams,
    ThreadResumeParams, ThreadSetNameParams, ThreadStartParams,
};
use codex_protocol::server::{
    AccountReadResponse, InitializeResponse, ThreadReadResponse, ThreadResponse,
};
use codex_protocol::{ClientNotification, ClientRequest, Extra};
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::client::config::CodexConfig;
use crate::client::dispatch::ServerInner;
use crate::client::error::Error;
use crate::client::thread::Thread;
use crate::client::transport;

// ── Codex ────────────────────────────────────────────────────────

/// A client of one codex app-server.
///
/// Created via [`connect()`](crate::client::connect) or [`Codex::from_io`]. All
/// methods take `&self`; the handle is cheap to clone.
#[derive(Clone)]
pub struct Codex {
    pub(crate) inner: Arc<ServerInner>,
}

impl Codex {
    /// Start a codex app-server on stdio and perform the initialize handshake.
    pub(crate) async fn connect(config: CodexConfig) -> Result<Self, Error> {
        let process = transport::spawn_process(transport::process_command(&config))
            .map_err(|e| Error::Process(format!("{e:#}")))?;
        Self::from_process(config, process).await
    }

    async fn from_process(config: CodexConfig, process: transport::Process) -> Result<Self, Error> {
        let transport::Process {
            child,
            stdin,
            stdout,
            stderr,
        } = process;
        let cancel = CancellationToken::new();
        let child_waiter = transport::spawn_child_waiter(child, cancel.clone());
        transport::spawn_stderr_task(stderr, "codex app-server stdio");
        Self::start(stdout, stdin, config, cancel, Some(child_waiter)).await
    }

    /// A client over already-open lines (for replay and tests). Performs the
    /// initialize handshake over them.
    pub async fn from_io(
        reader: impl AsyncBufRead + Unpin + Send + 'static,
        writer: impl AsyncWrite + Unpin + Send + 'static,
        config: CodexConfig,
    ) -> Result<Self, Error> {
        Self::start(reader, writer, config, CancellationToken::new(), None).await
    }

    async fn start(
        reader: impl AsyncBufRead + Unpin + Send + 'static,
        writer: impl AsyncWrite + Unpin + Send + 'static,
        config: CodexConfig,
        cancel: CancellationToken,
        child_waiter: Option<tokio::task::JoinHandle<()>>,
    ) -> Result<Self, Error> {
        let recorder = config
            .record_io
            .as_deref()
            .map(transport::WireRecorder::new)
            .transpose()?;
        let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>(64);
        let inner = Arc::new(ServerInner::new(stdin_tx, cancel.clone(), child_waiter));
        transport::spawn_reader_task(reader, inner.clone(), cancel.clone(), recorder.clone());
        transport::spawn_writer_task(writer, stdin_rx, cancel, recorder);

        let codex = Self { inner };
        if let Err(error) = codex.initialize(&config).await {
            codex.inner.shutdown().await;
            return Err(error);
        }
        Ok(codex)
    }

    async fn initialize(&self, config: &CodexConfig) -> Result<(), Error> {
        let init: InitializeResponse = self
            .inner
            .request(ClientRequest::Initialize(InitializeParams {
                client_info: ClientInfo {
                    name: config.client_name.clone(),
                    title: None,
                    version: config.client_version.clone(),
                    extra: Extra::new(),
                },
                capabilities: Some(Capabilities {
                    experimental_api: Some(true),
                    opt_out_notification_methods: Vec::new(),
                    extra: Extra::new(),
                }),
                extra: Extra::new(),
            }))
            .await?;
        self.inner
            .notify(ClientNotification::Initialized(()))
            .await?;
        let _ = self.inner.init_result.set(init);
        Ok(())
    }

    /// What the server answered to `initialize`.
    pub fn initialization_result(&self) -> Option<&InitializeResponse> {
        self.inner.init_result.get()
    }

    // ── Thread management ────────────────────────────────────────

    /// Start a new thread.
    pub async fn start_thread(&self, params: ThreadStartParams) -> Result<Thread, Error> {
        let session: ThreadResponse = self
            .inner
            .request(ClientRequest::ThreadStart(params))
            .await?;
        let registration = self.inner.register_thread(&session.thread.id).await;
        Ok(Thread::new(self.inner.clone(), session, registration))
    }

    /// Resume an existing thread.
    pub async fn resume_thread(&self, params: ThreadResumeParams) -> Result<Thread, Error> {
        // Install a fresh registration before the request: the app-server
        // replays the thread's history before it answers.
        let registration = self
            .inner
            .reregister_thread_for_resume(&params.thread_id)
            .await;
        let session: ThreadResponse = self
            .inner
            .request(ClientRequest::ThreadResume(params))
            .await?;
        registration.finish_staging().await;
        Ok(Thread::new(self.inner.clone(), session, registration))
    }

    pub async fn rename_thread(&self, thread_id: &str, name: &str) -> Result<(), Error> {
        self.inner
            .request_value(ClientRequest::ThreadSetName(ThreadSetNameParams {
                thread_id: thread_id.to_owned(),
                name: name.to_owned(),
                extra: Extra::new(),
            }))
            .await
            .map(drop)
    }

    /// Reads a thread; with its turns, a loaded thread that has run none is
    /// written to disk, so a client paging its history can resume it.
    pub async fn read_thread(
        &self,
        thread_id: &str,
        include_turns: bool,
    ) -> Result<ThreadReadResponse, Error> {
        self.inner
            .request(ClientRequest::ThreadRead(ThreadReadParams {
                thread_id: thread_id.to_owned(),
                include_turns: Some(include_turns),
                extra: Extra::new(),
            }))
            .await
    }

    // ── Non-thread operations ────────────────────────────────────

    /// Read the signed-in account, optionally refreshing its token first.
    pub async fn read_account(
        &self,
        params: AccountReadParams,
    ) -> Result<AccountReadResponse, Error> {
        self.inner.request(ClientRequest::AccountRead(params)).await
    }

    // ── Shutdown ─────────────────────────────────────────────────

    /// Shut down the codex app-server subprocess.
    pub async fn close(self) {
        self.inner.shutdown().await;
    }
}

#[cfg(test)]
mod remediation_tests {
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

    use super::*;

    fn test_io() -> (
        BufReader<DuplexStream>,
        DuplexStream,
        BufReader<DuplexStream>,
        DuplexStream,
    ) {
        let (client_reader, server_writer) = tokio::io::duplex(4096);
        let (client_writer, server_reader) = tokio::io::duplex(4096);
        (
            BufReader::new(client_reader),
            client_writer,
            BufReader::new(server_reader),
            server_writer,
        )
    }

    async fn read_json_line(reader: &mut BufReader<DuplexStream>) -> serde_json::Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn write_json_line(writer: &mut DuplexStream, value: serde_json::Value) {
        writer
            .write_all(value.to_string().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.flush().await.unwrap();
    }

    fn thread_session(thread_id: &str) -> serde_json::Value {
        serde_json::json!({
            "thread": {
                "id": thread_id,
                "cwd": "/tmp",
                "status": {"type": "idle"},
                "path": null,
                "agentNickname": null,
                "agentRole": null,
                "gitInfo": null,
                "name": null
            },
            "model": "test",
            "modelProvider": "openai",
            "serviceTier": null,
            "cwd": "/tmp",
            "approvalPolicy": "on-request",
            "approvalsReviewer": "user",
            "sandbox": {"type": "readOnly", "networkAccess": false},
            "reasoningEffort": null
        })
    }

    fn warning_message(event: &crate::client::ThreadEvent) -> &str {
        match event.notification() {
            Some(codex_protocol::ServerNotification::Warning(warning)) => &warning.message,
            other => panic!("expected a warning, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn initialize_error_cancels_io_tasks() {
        let (client_reader, client_writer, mut server_reader, mut server_writer) = test_io();
        let connect = tokio::spawn(Codex::from_io(
            client_reader,
            client_writer,
            CodexConfig::default(),
        ));

        let initialize = read_json_line(&mut server_reader).await;
        write_json_line(
            &mut server_writer,
            serde_json::json!({
                "id": initialize["id"],
                "error": {"code": -32602, "message": "bad initialize", "data": null}
            }),
        )
        .await;

        assert!(matches!(
            connect.await.unwrap(),
            Err(Error::Rpc(codex_protocol::RpcError { code: -32602, .. }))
        ));
        let mut trailing = String::new();
        let bytes = tokio::time::timeout(
            Duration::from_secs(1),
            server_reader.read_line(&mut trailing),
        )
        .await
        .expect("writer task remained alive after initialize failed")
        .unwrap();
        assert_eq!(bytes, 0);
    }

    #[tokio::test]
    async fn resume_stages_more_than_channel_capacity_without_overflow_or_retry_loop() {
        const REPLAY_EVENTS: usize = 300;

        let (client_reader, client_writer, mut server_reader, mut server_writer) = test_io();
        let connect = tokio::spawn(Codex::from_io(
            client_reader,
            client_writer,
            CodexConfig::default(),
        ));

        let initialize = read_json_line(&mut server_reader).await;
        write_json_line(
            &mut server_writer,
            serde_json::json!({
                "id": initialize["id"],
                "result": {
                    "userAgent": "test",
                    "codexHome": "/tmp",
                    "platformFamily": "unix",
                    "platformOs": "test"
                }
            }),
        )
        .await;
        let codex = connect.await.unwrap().unwrap();
        assert_eq!(
            read_json_line(&mut server_reader).await["method"],
            "initialized"
        );

        let resume_client = codex.clone();
        let resume = tokio::spawn(async move {
            resume_client
                .resume_thread(ThreadResumeParams {
                    thread_id: "thread-long".into(),
                    ..ThreadResumeParams::default()
                })
                .await
        });
        let request = read_json_line(&mut server_reader).await;
        assert_eq!(request["method"], "thread/resume");

        // Replay is deliberately sent before the response, when resume_thread's
        // caller cannot yet take and drain the returned event stream.
        for index in 0..REPLAY_EVENTS {
            write_json_line(
                &mut server_writer,
                serde_json::json!({
                    "method": "warning",
                    "params": {
                        "threadId": "thread-long",
                        "turnId": "turn-old",
                        "message": index.to_string()
                    }
                }),
            )
            .await;
        }
        write_json_line(
            &mut server_writer,
            serde_json::json!({"id": request["id"], "result": thread_session("thread-long")}),
        )
        .await;

        let thread = tokio::time::timeout(Duration::from_secs(2), resume)
            .await
            .expect("resume deadlocked while replaying history")
            .unwrap()
            .unwrap();
        let mut events = thread.events().await.unwrap();
        for index in 0..REPLAY_EVENTS {
            let event = events.next().await.unwrap().expect("staged replay event");
            assert_eq!(warning_message(&event), index.to_string());
            assert!(
                event.replayed,
                "history staged before the resume answered is a replay"
            );
        }
        assert_eq!(
            thread.inner.registration.state(),
            crate::client::dispatch::ThreadChannelState::Open,
            "the single resume must not poison its registration and trigger another resume"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(25), events.next())
                .await
                .is_err(),
            "the replay ended cleanly without an immediate overflow error"
        );
        // What the thread says once the resume has been answered is live.
        write_json_line(
            &mut server_writer,
            serde_json::json!({
                "method": "warning",
                "params": {"threadId": "thread-long", "turnId": "turn-new", "message": "live"}
            }),
        )
        .await;
        let live = events.next().await.unwrap().expect("live event");
        assert_eq!(warning_message(&live), "live");
        assert!(
            !live.replayed,
            "an event after the resume answered is not a replay"
        );

        codex.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn connect_reports_a_child_that_exits_before_initialize() {
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            Codex::connect(CodexConfig {
                codex_path: Some("false".into()),
                ..CodexConfig::default()
            }),
        )
        .await
        .expect("startup did not report the child's exit");
        assert!(matches!(result, Err(Error::TransportClosed)));
    }

    #[cfg(unix)]
    async fn assert_subprocess_waiter_awaited(initialize_error: bool) {
        let temp = tempfile::tempdir().unwrap();
        let marker_path = temp.path().join("stopped");
        let release_path = temp.path().join("release");
        let response = if initialize_error {
            serde_json::json!({
                "id": 1,
                "error": {"code": -32602, "message": "bad initialize", "data": null}
            })
        } else {
            serde_json::json!({
                "id": 1,
                "result": {
                    "userAgent": "fake",
                    "codexHome": "/tmp",
                    "platformFamily": "unix",
                    "platformOs": "test"
                }
            })
        };
        let script = r#"
trap '
  printf stopping > "$MARKER"
  while [ ! -e "$RELEASE" ]; do sleep 0.01; done
  printf stopped > "$MARKER"
  exit 0
' TERM
printf 'ready\n'
IFS= read -r request
printf '%s\n' "$RESPONSE"
# Some shells defer traps while an external command is running.
while :; do sleep 0.01; done
"#;
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", script])
            .env("RESPONSE", response.to_string())
            .env("MARKER", &marker_path)
            .env("RELEASE", &release_path)
            .kill_on_drop(true);
        let mut process = transport::spawn_process(command).unwrap();
        let pid = nix::unistd::Pid::from_raw(process.child.id().unwrap() as i32);
        let mut ready = String::new();
        tokio::time::timeout(Duration::from_secs(5), process.stdout.read_line(&mut ready))
            .await
            .expect("shell fixture did not start within 5 seconds")
            .unwrap();
        assert_eq!(ready, "ready\n");

        let shutdown = tokio::spawn(async move {
            let result = Codex::from_process(CodexConfig::default(), process).await;
            if initialize_error {
                assert!(matches!(
                    result,
                    Err(Error::Rpc(codex_protocol::RpcError { code: -32602, .. }))
                ));
            } else {
                result.unwrap().close().await;
            }
        });
        // Observe and release the child before the production two-second
        // TERM grace expires, so a stalled fixture fails at this boundary.
        tokio::time::timeout(Duration::from_secs(1), async {
            while std::fs::read_to_string(&marker_path).unwrap_or_default() != "stopping" {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            // The child is held inside its TERM handler until the test releases
            // it. A close that only sends a signal must not count as completion.
            assert!(
                !shutdown.is_finished(),
                "shutdown returned before the child exited"
            );
            std::fs::write(release_path, b"exit").unwrap();
        })
        .await
        .expect("fixture did not observe TERM and release the child within its one-second budget");
        tokio::time::timeout(Duration::from_secs(5), shutdown)
            .await
            .expect("shutdown did not reap the released child")
            .unwrap();
        assert_eq!(std::fs::read_to_string(marker_path).unwrap(), "stopped");
        assert_eq!(
            nix::sys::signal::kill(pid, None),
            Err(nix::errno::Errno::ESRCH)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn initialize_error_awaits_subprocess_waiter() {
        assert_subprocess_waiter_awaited(true).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn close_awaits_subprocess_waiter() {
        assert_subprocess_waiter_awaited(false).await;
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split};

    use super::*;

    #[tokio::test]
    async fn record_io_tees_both_wire_directions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("io.jsonl");
        let (client_side, server_side) = duplex(4096);
        let (client_read, client_write) = split(client_side);
        let (server_read, mut server_write) = split(server_side);
        let server_task = tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            server_write
                .write_all(br#"{"id":1,"result":{"userAgent":"ua","codexHome":"/tmp/codex","platformFamily":"unix","platformOs":"macos"}}
"#)
                .await
                .unwrap();
            server_write.flush().await.unwrap();
            line.clear();
            reader.read_line(&mut line).await.unwrap();
        });
        let codex = Codex::from_io(
            BufReader::new(client_read),
            client_write,
            CodexConfig {
                record_io: Some(path.clone()),
                ..CodexConfig::default()
            },
        )
        .await
        .unwrap();
        server_task.await.unwrap();
        codex.close().await;

        let rows: Vec<serde_json::Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["dir"], "stdin");
        assert_eq!(rows[1]["dir"], "stdout");
        assert_eq!(rows[2]["dir"], "stdin");
        assert!(rows.iter().all(|row| row["line"].is_string()));
    }
}
