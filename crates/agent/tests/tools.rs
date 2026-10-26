//! amux's tool server against a stand-in daemon on the agent's tools.sock:
//! the seven tools as the model calls them, the retry over an update
//! window, and the whole path from a provider's call through the harness's
//! launch of `<install path> mcp <dir>` to the item the interpreter draws.

mod support;

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent::local_socket::{LocalListener, LocalStream};
use agent::{NOT_RUNNING, TOOLS_SOCK, ToolsConfig};
use prost::Message as _;
use provider_fakes::{Step, Tool, ToolClass};
use serde_json::{Value, json};
use support::*;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tonic::{Request, Response, Status};
use wire::client_service_server::ClientService;
use wire::{
    Agent as Row, AgentRef, AgentSpec, AmbiguousAgentName, CreateAgentRequest, HostEntry,
    InventoryEvent, Kind, Lifecycle, Phase, SendInputRequest, SendInputResponse,
    SendMessageRequest, SendMessageResponse, create_agent_request, inventory_event,
};

const ME: [u8; 16] = [0xa1; 16];
const REVIEWER: [u8; 16] = [0xb2; 16];
const CHILD: [u8; 16] = [0xc3; 16];
const HERE: [u8; 16] = [0x01; 16];
const STUDIO: [u8; 16] = [0x02; 16];

// --- the stand-in daemon ---------------------------------------------------

/// What the tool server asked the daemon, in order.
#[derive(Clone, Debug)]
enum Call {
    Inventory,
    Resolve(String),
    Send(SendMessageRequest),
    Create(Box<CreateAgentRequest>),
    Stop(wire::StopAgentRequest),
}

#[derive(Clone, Default)]
struct Fleet {
    hosts: Vec<HostEntry>,
    agents: Vec<Row>,
    /// Names the daemon finds several agents by.
    ambiguous: Vec<String>,
    /// The reason SendMessage refuses with, if it does.
    refuse_send: Option<Status>,
    /// The reason StopAgent refuses with, if it does.
    refuse_stop: Option<Status>,
    /// Set, a send, spawn or stop is taken and then its connection is
    /// dropped before the daemon answers.
    sever: Option<Arc<tokio::sync::Notify>>,
    /// Set, a send, spawn or stop is taken and answered as unavailable.
    unavailable: bool,
    calls: Arc<Mutex<Vec<Call>>>,
}

impl Fleet {
    fn record(&self, call: Call) {
        self.calls.lock().unwrap().push(call);
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// Having taken the call, drop every connection and never answer, or
    /// answer that the daemon is unavailable.
    async fn severed(&self) -> Result<(), Status> {
        if let Some(sever) = &self.sever {
            sever.notify_waiters();
            std::future::pending::<()>().await;
        }
        if self.unavailable {
            return Err(Status::unavailable("the host went away"));
        }
        Ok(())
    }
}

type Events = Pin<Box<dyn futures_util::Stream<Item = Result<InventoryEvent, Status>> + Send>>;
type Sessions =
    Pin<Box<dyn futures_util::Stream<Item = Result<wire::SessionEvent, Status>> + Send>>;

#[tonic::async_trait]
impl ClientService for Fleet {
    type SubscribeInventoryStream = Events;
    type SubscribeSessionStream = Sessions;

    async fn subscribe_inventory(
        &self,
        _: Request<wire::SubscribeInventoryRequest>,
    ) -> Result<Response<Events>, Status> {
        self.record(Call::Inventory);
        let mut events = self
            .hosts
            .iter()
            .cloned()
            .map(inventory_event::Of::Host)
            .chain(self.agents.iter().cloned().map(inventory_event::Of::Agent))
            .map(|of| Ok(InventoryEvent { of: Some(of) }))
            .collect::<Vec<_>>();
        events.push(Ok(InventoryEvent {
            of: Some(inventory_event::Of::CaughtUp(Default::default())),
        }));
        // A subscription stays open after CaughtUp, as the daemon's does.
        let events = futures_util::StreamExt::chain(
            futures_util::stream::iter(events),
            futures_util::stream::pending(),
        );
        Ok(Response::new(Box::pin(events)))
    }

    async fn resolve_agent(
        &self,
        request: Request<wire::ResolveAgentRequest>,
    ) -> Result<Response<Row>, Status> {
        let name = request.into_inner().name;
        self.record(Call::Resolve(name.clone()));
        if self.ambiguous.contains(&name) {
            let detail = AmbiguousAgentName {
                name: name.clone(),
                candidates: vec![row(&REVIEWER, &HERE, &name), row(&CHILD, &STUDIO, &name)],
            };
            let message = format!(
                "{name} names more than one agent; pick one with {name}@here or {name}@studio"
            );
            let error = wire::Error::new(wire::ErrorCode::FailedPrecondition, message.clone())
                .with_detail(&detail);
            return Err(Status::with_details(
                tonic::Code::FailedPrecondition,
                message,
                error.encode_to_vec().into(),
            ));
        }
        self.agents
            .iter()
            .find(|agent| agent.name == name)
            .cloned()
            .map(Response::new)
            .ok_or_else(|| Status::not_found(format!("no agent named {name}")))
    }

    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        let envelope = request.into_inner();
        self.record(Call::Send(envelope.clone()));
        self.severed().await?;
        if let Some(status) = &self.refuse_send {
            return Err(status.clone());
        }
        Ok(Response::new(SendMessageResponse {
            envelope_id: envelope.id,
        }))
    }

    async fn create_agent(
        &self,
        request: Request<CreateAgentRequest>,
    ) -> Result<Response<Row>, Status> {
        let request = request.into_inner();
        self.record(Call::Create(Box::new(request.clone())));
        self.severed().await?;
        // The daemon resolves a host name against the hosts it trusts.
        let host = match request.host.as_ref().and_then(|host| host.of.as_ref()) {
            Some(wire::host_ref::Of::Name(name)) if name.eq_ignore_ascii_case("studio") => {
                STUDIO.to_vec()
            }
            Some(wire::host_ref::Of::Name(name)) => {
                return Err(Status::not_found(format!(
                    "no trusted host is named {name}; trusted hosts: laptop, Studio"
                )));
            }
            Some(wire::host_ref::Of::HostId(host)) => host.clone(),
            None => HERE.to_vec(),
        };
        let name = request.name.clone().unwrap_or_else(|| "quiet-otter".into());
        Ok(Response::new(row(&request.agent_id, &host, &name)))
    }

    async fn send_input(
        &self,
        _: Request<SendInputRequest>,
    ) -> Result<Response<SendInputResponse>, Status> {
        // No tool sends a person's input: the stop tool stops.
        Err(Status::unimplemented("send_input"))
    }

    async fn subscribe_session(
        &self,
        _: Request<wire::SubscribeSessionRequest>,
    ) -> Result<Response<Sessions>, Status> {
        Err(Status::unimplemented("subscribe"))
    }
    async fn list_items(
        &self,
        _: Request<wire::ListItemsRequest>,
    ) -> Result<Response<wire::ListItemsResponse>, Status> {
        Err(Status::unimplemented("fetch"))
    }
    async fn get_item(
        &self,
        _: Request<wire::GetItemRequest>,
    ) -> Result<Response<wire::Item>, Status> {
        Err(Status::unimplemented("get"))
    }
    async fn rename_agent(
        &self,
        _: Request<wire::RenameAgentRequest>,
    ) -> Result<Response<Row>, Status> {
        Err(Status::unimplemented("rename"))
    }
    async fn stop_agent(
        &self,
        request: Request<wire::StopAgentRequest>,
    ) -> Result<Response<wire::StopAgentResponse>, Status> {
        self.record(Call::Stop(request.into_inner()));
        self.severed().await?;
        match &self.refuse_stop {
            Some(status) => Err(status.clone()),
            None => Ok(Response::new(wire::StopAgentResponse {})),
        }
    }
    async fn resume_agent(
        &self,
        _: Request<wire::ResumeAgentRequest>,
    ) -> Result<Response<Row>, Status> {
        Err(Status::unimplemented("resume"))
    }
    async fn delete_agent(
        &self,
        _: Request<wire::DeleteAgentRequest>,
    ) -> Result<Response<wire::Empty>, Status> {
        Err(Status::unimplemented("delete"))
    }
    async fn put_blob(
        &self,
        _: Request<wire::PutBlobRequest>,
    ) -> Result<Response<wire::BlobRef>, Status> {
        Err(Status::unimplemented("put_blob"))
    }
    async fn get_blob(
        &self,
        _: Request<wire::GetBlobRequest>,
    ) -> Result<Response<wire::GetBlobResponse>, Status> {
        Err(Status::unimplemented("get_blob"))
    }
    async fn get_diff(
        &self,
        _: Request<wire::GetDiffRequest>,
    ) -> Result<Response<wire::Diff>, Status> {
        Err(Status::unimplemented("diff"))
    }
    async fn get_catalogue(
        &self,
        _: Request<wire::GetCatalogueRequest>,
    ) -> Result<Response<wire::GetCatalogueResponse>, Status> {
        Err(Status::unimplemented("get_catalogue"))
    }
    async fn list_repositories(
        &self,
        _: Request<wire::ListRepositoriesRequest>,
    ) -> Result<Response<wire::ListRepositoriesResponse>, Status> {
        Err(Status::unimplemented("list_repositories"))
    }
    async fn dump(
        &self,
        _: Request<wire::DumpRequest>,
    ) -> Result<Response<wire::DumpResponse>, Status> {
        Err(Status::unimplemented("dump"))
    }
    async fn update_draft(
        &self,
        _: Request<wire::UpdateDraftRequest>,
    ) -> Result<Response<wire::Draft>, Status> {
        Err(Status::unimplemented("save_draft"))
    }
    async fn get_draft(
        &self,
        _: Request<wire::GetDraftRequest>,
    ) -> Result<Response<wire::Draft>, Status> {
        Err(Status::unimplemented("get_draft"))
    }
}

/// A connection the stand-in daemon accepted, which fails its reads and
/// writes once the daemon severs its connections.
struct Conn(LocalStream, Pin<Box<tokio::sync::futures::OwnedNotified>>);

impl Conn {
    fn severed(&mut self, cx: &mut std::task::Context<'_>) -> bool {
        self.1.as_mut().poll(cx).is_ready()
    }
}

fn reset() -> std::io::Error {
    std::io::Error::from(std::io::ErrorKind::ConnectionReset)
}

impl tonic::transport::server::Connected for Conn {
    type ConnectInfo = ();
    fn connect_info(&self) {}
}

impl AsyncRead for Conn {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.severed(cx) {
            return std::task::Poll::Ready(Err(reset()));
        }
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Conn {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.severed(cx) {
            return std::task::Poll::Ready(Err(reset()));
        }
        Pin::new(&mut self.0).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// The daemon listening on `<dir>/tools.sock` until dropped.
struct Daemon {
    task: tokio::task::JoinHandle<()>,
}

impl Daemon {
    fn listen(dir: &Path, fleet: Fleet) -> Self {
        let listener = LocalListener::bind(&dir.join(TOOLS_SOCK)).expect("tools.sock binds");
        let sever = fleet.sever.clone().unwrap_or_default();
        let incoming = futures_util::stream::unfold(listener, move |mut listener| {
            let sever = sever.clone();
            async move {
                let accepted = listener
                    .accept()
                    .await
                    .map(|stream| Conn(stream, Box::pin(sever.notified_owned())));
                Some((accepted, listener))
            }
        });
        let task = tokio::spawn(async move {
            let _ = tonic::transport::Server::builder()
                .add_service(wire::client_service_server(fleet))
                .serve_with_incoming(incoming)
                .await;
        });
        Self { task }
    }

    /// Stops listening and waits until the socket is gone. Aborting alone
    /// drops the listener later on a worker thread, and its unbinding would
    /// then remove a successor's socket bound at the same path.
    async fn stop(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn row(id: &[u8], host: &[u8], name: &str) -> Row {
    Row {
        agent_id: id.to_vec(),
        host_id: host.to_vec(),
        kind: Kind::ClaudeSdk as i32,
        name: name.to_owned(),
        lifecycle: Lifecycle::Live as i32,
        phase: Phase::Idle as i32,
        ..Default::default()
    }
}

fn host(id: &[u8], name: &str, reach: wire::trusted::Reach) -> HostEntry {
    HostEntry::trusted_host(id, name, reach)
}

/// Me on this host with a child, a reviewer on the studio, and a candidate
/// host nobody paired.
fn fleet() -> Fleet {
    let mut me = row(&ME, &HERE, "lead");
    me.kind = Kind::ClaudePty as i32;
    me.phase = Phase::Working as i32;
    me.working_on = Some("Splitting the parser".into());
    let mut reviewer = row(&REVIEWER, &STUDIO, "reviewer");
    reviewer.kind = Kind::Codex as i32;
    let mut child = row(&CHILD, &HERE, "helper");
    child.parent = Some(AgentRef {
        host_id: HERE.to_vec(),
        agent_id: ME.to_vec(),
    });
    child.lifecycle = Lifecycle::Exited as i32;
    child.exit_cause = wire::ExitCause::Finished as i32;
    Fleet {
        hosts: vec![
            host(
                &HERE,
                "laptop",
                wire::trusted::Reach::Online(wire::RouteKind::Direct as i32),
            ),
            host(
                &STUDIO,
                "Studio",
                wire::trusted::Reach::Away(wire::Empty {}),
            ),
            HostEntry::candidate_host([0x03; 16], "stranger", Vec::new()),
        ],
        agents: vec![me, reviewer, child],
        ..Default::default()
    }
}

// --- the tool server, driven as a harness drives it ------------------------

/// An agent directory with a spec, and the tool server serving it.
struct Mcp {
    _root: tempfile::TempDir,
    dir: PathBuf,
    cwd: PathBuf,
    to_server: tokio::io::DuplexStream,
    from_server: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    next_id: u64,
}

impl Mcp {
    async fn start(retry_window: Duration) -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("agents").join(interpret::to_hex(&ME));
        let cwd = root.path().join("work");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let spec = AgentSpec {
            agent_id: ME.to_vec(),
            kind: Kind::ClaudePty as i32,
            cwd: cwd.display().to_string(),
            incarnation: 1,
            ..Default::default()
        };
        std::fs::write(dir.join("spec.1"), spec.encode_to_vec()).unwrap();
        let (to_server, server_in) = tokio::io::duplex(1 << 20);
        let (server_out, from_server) = tokio::io::duplex(1 << 20);
        tokio::spawn(agent::serve_tools_on(
            dir.clone(),
            server_in,
            server_out,
            ToolsConfig { retry_window },
        ));
        let mut mcp = Self {
            _root: root,
            dir,
            cwd,
            to_server,
            from_server: BufReader::new(from_server).lines(),
            next_id: 0,
        };
        let init = mcp
            .request(
                "initialize",
                json!({ "protocolVersion": "2025-06-18", "capabilities": {},
                        "clientInfo": { "name": "test", "version": "1" } }),
            )
            .await;
        assert_eq!(init["result"]["serverInfo"]["name"], "amux");
        mcp.write(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        mcp
    }

    async fn write(&mut self, message: Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.to_server.write_all(line.as_bytes()).await.unwrap();
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        let line = tokio::time::timeout(Duration::from_secs(20), self.from_server.next_line())
            .await
            .expect("the tool server answers")
            .unwrap()
            .expect("the tool server is still serving");
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], json!(id), "responses come in order");
        response
    }

    /// A tool call's text, and whether the server marked it an error.
    async fn call(&mut self, tool: &str, arguments: Value) -> (String, bool) {
        let response = self
            .request(
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            )
            .await;
        let result = &response["result"];
        (
            result["content"][0]["text"].as_str().unwrap().to_owned(),
            result["isError"] == json!(true),
        )
    }

    async fn ok(&mut self, tool: &str, arguments: Value) -> Value {
        let (text, error) = self.call(tool, arguments).await;
        assert!(!error, "{tool} failed: {text}");
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    }

    async fn refused(&mut self, tool: &str, arguments: Value) -> String {
        let (text, error) = self.call(tool, arguments).await;
        assert!(error, "{tool} succeeded: {text}");
        text
    }
}

const WINDOW: Duration = Duration::from_secs(3);

#[tokio::test(flavor = "multi_thread")]
async fn the_server_lists_the_seven_tools() {
    let mut mcp = Mcp::start(WINDOW).await;
    let listed = mcp.request("tools/list", json!({})).await;
    let names = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "agents", "hosts", "send", "spawn", "stop", "status", "attach"
        ]
    );
    let unknown = mcp
        .request("tools/call", json!({ "name": "delete", "arguments": {} }))
        .await;
    assert!(
        unknown["error"]["message"]
            .as_str()
            .unwrap()
            .contains("delete")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_lists_the_fleet_read_to_caught_up_and_marks_you() {
    let mut mcp = Mcp::start(WINDOW).await;
    let mut fleet = fleet();
    // A live row can already hold the cause its journal ended with, until
    // its daemon marks it exited; the tool says none for a live agent.
    let reviewer = &mut fleet.agents[1];
    assert_eq!(reviewer.name, "reviewer");
    reviewer.exit_cause = wire::ExitCause::ProviderExited as i32;
    reviewer.exit_detail = Some("exit code 1".into());
    let _daemon = Daemon::listen(&mcp.dir, fleet.clone());
    let listed = mcp.ok("agents", json!({})).await;
    assert_eq!(
        listed,
        json!({ "agents": [
            { "name": "lead", "id": interpret::to_hex(&ME), "kind": "claude", "host": "laptop",
              "live": true, "phase": "working", "working_on": "Splitting the parser", "you": true },
            { "name": "reviewer", "id": interpret::to_hex(&REVIEWER), "kind": "codex",
              "host": "Studio", "live": true, "phase": "idle" },
            { "name": "helper", "id": interpret::to_hex(&CHILD), "kind": "claude",
              "host": "laptop", "live": false, "phase": "idle", "exit_cause": "finished",
              "parent": "lead" },
        ]})
    );
    assert!(matches!(fleet.calls().as_slice(), [Call::Inventory]));
}

#[tokio::test(flavor = "multi_thread")]
async fn hosts_lists_trusted_hosts_and_this_one() {
    let mut mcp = Mcp::start(WINDOW).await;
    let _daemon = Daemon::listen(&mcp.dir, fleet());
    let listed = mcp.ok("hosts", json!({})).await;
    assert_eq!(
        listed,
        json!({ "hosts": [
            { "name": "laptop", "presence": "online", "this_host": true },
            { "name": "Studio", "presence": "away" },
        ]})
    );
}

/// The envelope carries no sender: the daemon fills it from the socket the
/// call arrived on. The answer is the id the interpreter reads.
#[tokio::test(flavor = "multi_thread")]
async fn send_resolves_the_recipient_and_answers_the_envelope_id() {
    let mut mcp = Mcp::start(WINDOW).await;
    let fleet = fleet();
    let _daemon = Daemon::listen(&mcp.dir, fleet.clone());
    let sent = mcp
        .ok(
            "send",
            json!({ "to": "reviewer", "text": "Please review.", "context": "parser" }),
        )
        .await;
    let calls = fleet.calls();
    let [Call::Resolve(name), Call::Send(request)] = calls.as_slice() else {
        panic!("resolve then send, got {calls:?}");
    };
    assert_eq!(name, "reviewer");
    assert_eq!(sent, json!({ "id": interpret::to_hex(&request.id) }));
    assert_eq!(
        request.body.as_ref().map(|body| body.text.as_str()),
        Some("Please review.")
    );
    assert_eq!(request.context.as_deref(), Some(b"parser".as_slice()));
    assert_eq!(
        request.to,
        Some(AgentRef {
            host_id: STUDIO.to_vec(),
            agent_id: REVIEWER.to_vec(),
        })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn send_says_plainly_when_the_name_or_the_delivery_fails() {
    let mut mcp = Mcp::start(WINDOW).await;
    let mut fleet = fleet();
    fleet.ambiguous = vec!["twin".into()];
    fleet.refuse_send = Some(Status::failed_precondition("reviewer has exited"));
    let _daemon = Daemon::listen(&mcp.dir, fleet.clone());
    assert_eq!(
        mcp.refused("send", json!({ "to": "ghost", "text": "hi" }))
            .await,
        "no agent named ghost"
    );
    assert_eq!(
        mcp.refused("send", json!({ "to": "twin", "text": "hi" }))
            .await,
        "twin names more than one agent; pick one with twin@here or twin@studio",
        "the daemon's refusal, offering the name@host forms, reaches the model as it is"
    );
    assert_eq!(
        mcp.refused("send", json!({ "to": "reviewer", "text": "hi" }))
            .await,
        "reviewer has exited"
    );
    assert_eq!(
        mcp.refused("send", json!({ "to": "reviewer" })).await,
        "text is required"
    );
}

/// With no host named the child is created on the daemon the call reached,
/// which is the parent's own, in the parent's directory; a named host is
/// looked up among the trusted ones and the directory is left to it.
#[tokio::test(flavor = "multi_thread")]
async fn spawn_places_the_child_by_host_name_defaulting_to_the_parents_host() {
    let mut mcp = Mcp::start(WINDOW).await;
    let fleet = fleet();
    let _daemon = Daemon::listen(&mcp.dir, fleet.clone());

    let spawned = mcp
        .ok(
            "spawn",
            json!({ "kind": "codex", "prompt": "Write the tests.", "name": "tester" }),
        )
        .await;
    let calls = fleet.calls();
    let [Call::Create(here)] = calls.as_slice() else {
        panic!("one create, got {calls:?}");
    };
    assert_eq!(
        spawned,
        json!({ "name": "tester", "id": interpret::to_hex(&here.agent_id) })
    );
    assert_eq!(here.host, None, "the daemon that took the call places it");
    assert_eq!(
        here.parent, None,
        "the daemon sets the parent from the caller"
    );
    assert_eq!(here.cwd, mcp.cwd.display().to_string());
    assert_eq!(
        here.agent,
        Some(create_agent_request::Agent::Codex(
            wire::CodexCreateConfig::default()
        )),
        "Codex with its default settings"
    );
    let prompt = here.initial_prompt.clone().unwrap();
    assert_eq!(prompt.prompt.unwrap().text, "Write the tests.");
    assert!(!prompt.input_id.is_empty());

    mcp.ok(
        "spawn",
        json!({ "kind": "claude", "prompt": "Review it.", "host": "studio" }),
    )
    .await;
    let calls = fleet.calls();
    let [_, Call::Create(there)] = calls.as_slice() else {
        panic!("a second create, got {calls:?}");
    };
    assert_eq!(
        there.host,
        Some(wire::HostRef::named("studio")),
        "the daemon resolves the name"
    );
    assert_eq!(there.cwd, "", "a path here means nothing there");
    assert_eq!(
        there.agent.as_ref().map(create_agent_request::Agent::kind),
        Some(Kind::ClaudeSdk)
    );

    assert_eq!(
        mcp.refused(
            "spawn",
            json!({ "kind": "codex", "prompt": "x", "host": "stranger" })
        )
        .await,
        "no trusted host is named stranger; trusted hosts: laptop, Studio"
    );
    assert_eq!(
        mcp.refused("spawn", json!({ "kind": "gemini", "prompt": "x" }))
            .await,
        "no agent kind \"gemini\"; use claude or codex"
    );
    assert_eq!(
        fleet.calls().len(),
        3,
        "the daemon refused the stranger and the unknown kind never reached it"
    );
}

/// The stop tool stops, as a person's stop does: the daemon ends the child
/// now and its descendants with it. It is not an interrupt.
#[tokio::test(flavor = "multi_thread")]
async fn stop_stops_the_named_child_through_the_daemon() {
    let mut mcp = Mcp::start(WINDOW).await;
    let fleet = fleet();
    let _daemon = Daemon::listen(&mcp.dir, fleet.clone());
    assert_eq!(mcp.ok("stop", json!({ "name": "helper" })).await, json!({}));
    let calls = fleet.calls();
    let [Call::Resolve(_), Call::Stop(request)] = calls.as_slice() else {
        panic!("resolve then stop, got {calls:?}");
    };
    assert_eq!(request.agent_id, CHILD.to_vec());

    let mut fleet = fleet.clone();
    fleet.refuse_stop = Some(Status::permission_denied(
        "an agent stops only its own children",
    ));
    fleet.calls = Default::default();
    let mcp_dir = mcp.dir.clone();
    _daemon.stop().await;
    let _daemon = Daemon::listen(&mcp_dir, fleet);
    assert_eq!(
        mcp.refused("stop", json!({ "name": "reviewer" })).await,
        "reviewer did not stop: an agent stops only its own children"
    );
}

/// status and attach never dial: here no daemon exists at all.
#[tokio::test(flavor = "multi_thread")]
async fn status_answers_ok_without_the_daemon() {
    let mut mcp = Mcp::start(WINDOW).await;
    let started = tokio::time::Instant::now();
    assert_eq!(
        mcp.ok("status", json!({ "working_on": "Splitting the parser" }))
            .await,
        json!("ok")
    );
    assert_eq!(
        mcp.ok("status", json!({ "working_on": null })).await,
        json!("ok")
    );
    assert!(started.elapsed() < WINDOW, "no daemon was waited for");
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_stores_the_file_by_its_hash_and_answers_the_element() {
    use sha2::Digest as _;

    let mut mcp = Mcp::start(WINDOW).await;
    let bytes = b"\x89PNG\r\n\x1a\nnot really a picture";
    std::fs::write(mcp.cwd.join("chart.png"), bytes).unwrap();
    let hash = sha2::Sha256::digest(bytes).to_vec();
    let started = tokio::time::Instant::now();

    let (element, error) = mcp.call("attach", json!({ "path": "chart.png" })).await;
    assert!(!error, "{element}");
    assert!(started.elapsed() < WINDOW, "no daemon was waited for");
    let stored = mcp.dir.join(agent::BLOBS).join(interpret::to_hex(&hash));
    assert_eq!(std::fs::read(&stored).unwrap(), bytes);
    let expected = wire::Attachment {
        of: Some(wire::attachment::Of::Image(wire::BlobRef {
            hash: hash.clone(),
            name: "chart.png".into(),
            mime: "image/png".into(),
            size: bytes.len() as u64,
        })),
    };
    assert_eq!(element, attachments::element(&expected, Some(&stored)));
    let parsed = attachments::parse(&element);
    assert_eq!(parsed.positioned.attachments, vec![expected]);

    let (named, _) = mcp
        .call(
            "attach",
            json!({ "path": mcp.cwd.join("chart.png"), "name": "Q3 chart" }),
        )
        .await;
    assert!(named.contains("name=\"Q3 chart\""), "{named}");
    let missing = mcp.refused("attach", json!({ "path": "nope.txt" })).await;
    assert!(missing.starts_with("reading "), "{missing}");
}

/// A file over the attachment limit is refused in words naming the limit,
/// before its bytes are read or stored: the recipient could not be sent it.
#[tokio::test(flavor = "multi_thread")]
async fn attach_refuses_a_file_over_the_attachment_limit() {
    let mut mcp = Mcp::start(WINDOW).await;
    let huge = std::fs::File::create(mcp.cwd.join("huge.bin")).unwrap();
    huge.set_len(wire::ATTACHMENT_SIZE_LIMIT as u64 + 1)
        .unwrap();
    let refused = mcp.refused("attach", json!({ "path": "huge.bin" })).await;
    assert_eq!(
        refused,
        "huge.bin is too large to attach: files can be at most 64 MB"
    );
    let stored = std::fs::read_dir(mcp.dir.join(agent::BLOBS))
        .map(|blobs| blobs.count())
        .unwrap_or(0);
    assert_eq!(stored, 0, "nothing was stored");
}

/// A send's attachment is measured as stored, not as its element says:
/// one over the limit is refused naming it, and nothing is sent.
#[tokio::test(flavor = "multi_thread")]
async fn send_refuses_an_attachment_stored_over_the_limit() {
    let mut mcp = Mcp::start(WINDOW).await;
    let fleet = fleet();
    let _daemon = Daemon::listen(&mcp.dir, fleet.clone());
    let hash = vec![7u8; 32];
    let blobs = mcp.dir.join(agent::BLOBS);
    std::fs::create_dir_all(&blobs).unwrap();
    let stored = blobs.join(interpret::to_hex(&hash));
    std::fs::File::create(&stored)
        .unwrap()
        .set_len(wire::ATTACHMENT_SIZE_LIMIT as u64 + 1)
        .unwrap();
    let element = attachments::element(
        &wire::Attachment {
            of: Some(wire::attachment::Of::File(wire::BlobRef {
                hash,
                name: "dump.bin".into(),
                mime: "application/octet-stream".into(),
                size: 10,
            })),
        },
        Some(&stored),
    );
    let refused = mcp
        .refused(
            "send",
            json!({ "to": "reviewer", "text": format!("Here: {element}") }),
        )
        .await;
    assert_eq!(
        refused,
        "dump.bin is too large to attach: files can be at most 64 MB"
    );
    assert!(
        !fleet
            .calls()
            .iter()
            .any(|call| matches!(call, Call::Send(_))),
        "nothing was sent"
    );
}

/// A daemon that comes up inside the window is reached; with none the call
/// says so once the window has passed.
#[tokio::test(flavor = "multi_thread")]
async fn a_fleet_call_retries_over_an_update_and_reports_a_missing_daemon() {
    let mut mcp = Mcp::start(WINDOW).await;
    let dir = mcp.dir.clone();
    let fleet = fleet();
    let restarted = tokio::spawn({
        let fleet = fleet.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(700)).await;
            Daemon::listen(&dir, fleet)
        }
    });
    let listed = mcp.ok("hosts", json!({})).await;
    assert_eq!(listed["hosts"].as_array().unwrap().len(), 2);
    drop(restarted.await.unwrap());

    let started = tokio::time::Instant::now();
    assert_eq!(
        mcp.refused("agents", json!({})).await,
        NOT_RUNNING,
        "no daemon listens any more"
    );
    let waited = started.elapsed();
    assert!(
        waited >= WINDOW - Duration::from_millis(600) && waited < WINDOW * 2,
        "retried for the window: {waited:?}"
    );
}

/// A send, spawn or stop is at most once: the daemon may have acted on a
/// call whose connection dropped before it answered, so the call is never
/// made again on the model's behalf and the model is told plainly.
#[tokio::test(flavor = "multi_thread")]
async fn a_call_the_daemon_may_have_taken_is_never_made_again() {
    let dropped = Fleet {
        sever: Some(Arc::default()),
        ..fleet()
    };
    let unavailable = Fleet {
        unavailable: true,
        ..fleet()
    };
    for fleet in [dropped, unavailable] {
        let mut mcp = Mcp::start(WINDOW).await;
        let _daemon = Daemon::listen(&mcp.dir, fleet.clone());
        let calls = [
            ("send", json!({ "to": "reviewer", "text": "Look at this" })),
            (
                "spawn",
                json!({ "kind": "codex", "prompt": "Write the tests" }),
            ),
            ("stop", json!({ "name": "helper" })),
        ];
        for (tool, arguments) in calls {
            let refusal = mcp.refused(tool, arguments).await;
            assert!(
                refusal.contains("may have taken it") && refusal.contains("not made again"),
                "{tool}: {refusal}"
            );
        }
        let taken: Vec<&str> = fleet
            .calls()
            .iter()
            .filter_map(|call| match call {
                Call::Send(_) => Some("send"),
                Call::Create(_) => Some("create"),
                Call::Stop(_) => Some("stop"),
                _ => None,
            })
            .collect();
        assert_eq!(taken, ["send", "create", "stop"], "each call went out once");
    }
}

// --- the whole path, per kind ----------------------------------------------

/// The provider calls status and send through the server its launch names
/// at the install path; the server dials this agent's tools.sock; the
/// interpreter draws the send as the message it sent, with the envelope id
/// the daemon took, and the status as working_on, and neither as a tool row.
async fn the_harness_runs_the_tool_server_from_the_install_path(kind: Kind) {
    let tool = |name: &str, input: Value| {
        Step::Tool(Tool {
            name: Some(format!("mcp__amux__{name}")),
            class: ToolClass::Consequential,
            input: Some(input),
            outcome: Default::default(),
            wait_for: None,
        })
    };
    let agent = Agent::start(Setup {
        kind,
        steps: vec![
            tool("status", json!({ "working_on": "Reviewing the parser" })),
            tool(
                "send",
                json!({ "to": "reviewer", "text": "Ready for review." }),
            ),
            tool("send", json!({ "to": "ghost", "text": "Anyone?" })),
            Step::TurnEnd,
        ],
        ..Setup::sdk()
    })
    .await;
    let fleet = fleet();
    let _tools = Daemon::listen(&agent.dir, fleet.clone());
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "go").await, Verdict::Accepted);
    agent
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;

    let log = agent.log();
    assert_eq!(log.working_on().as_deref(), Some("Reviewing the parser"));
    let sent = fleet
        .calls()
        .into_iter()
        .find_map(|call| match call {
            Call::Send(envelope) => Some(envelope),
            _ => None,
        })
        .expect("the daemon took a message");
    assert_eq!(
        sent.body.as_ref().map(|body| body.text.as_str()),
        Some("Ready for review.")
    );
    let messages = log
        .full_items()
        .into_values()
        .filter_map(|item| drawn(kind, &item).map(|drawn| (item, drawn)))
        .collect::<Vec<_>>();
    let summary = messages
        .iter()
        .map(|(item, drawn)| match drawn {
            Drawn::Message(message) => format!(
                "message to {} {:?} {} {} {}",
                message.to,
                message.send_state(),
                item.text,
                if message.envelope_id == sent.id {
                    "with the envelope id"
                } else {
                    "without it"
                },
                message.rejection
            ),
            Drawn::Tool(name) => format!("tool row {name}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            "message to reviewer Sent Ready for review. with the envelope id ",
            "message to ghost Rejected Anyone? without it no agent named ghost",
        ],
        "{kind:?}"
    );
    daemon.stop().await;
    agent.exit().await;
}

enum Drawn {
    Message(wire::AgentMessage),
    Tool(String),
}

/// An item that is a sent agent message or any tool row.
fn drawn(kind: Kind, item: &wire::Item) -> Option<Drawn> {
    use wire::{claude_pty_item, claude_sdk_item, codex_item, work};
    let body = item.body.as_slice();
    match kind {
        Kind::ClaudePty => match wire::ClaudePtyItem::decode(body).ok()?.kind? {
            claude_pty_item::Kind::AgentMessage(message) => Some(Drawn::Message(message)),
            claude_pty_item::Kind::Tool(tool) => Some(Drawn::Tool(tool.name)),
            _ => None,
        },
        Kind::ClaudeSdk => match wire::ClaudeSdkItem::decode(body).ok()?.kind? {
            claude_sdk_item::Kind::AgentMessage(message) => Some(Drawn::Message(message)),
            claude_sdk_item::Kind::Tool(tool) => Some(Drawn::Tool(tool.name)),
            _ => None,
        },
        _ => match wire::CodexItem::decode(body).ok()?.kind? {
            codex_item::Kind::AgentMessage(message) => Some(Drawn::Message(message)),
            codex_item::Kind::Work(work) => match work.of? {
                work::Of::Mcp(call) => Some(Drawn::Tool(call.tool)),
                _ => None,
            },
            _ => None,
        },
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn headless_claude_runs_amuxs_tools_through_the_install_path() {
    the_harness_runs_the_tool_server_from_the_install_path(Kind::ClaudeSdk).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_runs_amuxs_tools_through_the_install_path() {
    the_harness_runs_the_tool_server_from_the_install_path(Kind::Codex).await;
}

// Unix only: Windows does not host terminal Claude (ConPTY re-renders its output; see
// docs/ARCHITECTURE.md, "Windows, as a stated cost").
#[cfg(unix)]
#[test]
fn terminal_claude_runs_amuxs_tools_through_the_install_path() {
    terminal_test(the_harness_runs_the_tool_server_from_the_install_path(
        Kind::ClaudePty,
    ));
}
