//! amux's tool server against a stand-in daemon on the agent's tools.sock:
//! the seven tools as the model calls them, a spawn's way through the agent
//! process's own socket, the retry over an update window, and the whole
//! path from a provider's call through the harness's launch of
//! `<install path> mcp <dir>` to the item the interpreter draws.
//!
//! The stand-in serves what the daemon serves an agent: the inventory,
//! answered by the daemon itself, and the agent service, answered for the
//! host each call is addressed to. It records each call's address, and
//! answers for every host as the daemon and the host it passes a call to
//! would, between them.

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent::local_socket::{LocalListener, LocalStream};
use agent::{ExitCause, NOT_RUNNING, NOT_TAKING_SPAWNS, RETRY_WINDOW, Spawn, TOOLS_SOCK};
use prost::Message as _;
use provider_fakes::{Step, Tool, ToolClass};
use serde_json::{Value, json};
use support::*;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tonic::codegen::http;
use tonic::{Request, Response, Status};
use wire::amux::staging::agent_service_server::AgentService;
use wire::amux::staging::inventory_service_server::InventoryService;
use wire::amux::staging::{
    self as staging, CreateAgentRequest, SendMessageRequest, SetFilingRequest, create_agent_request,
};
use wire::{Agent as Row, AgentRef, AgentSpec, HostEntry, InventoryEvent, Kind, Lifecycle, Phase};

use crate::support;

const ME: [u8; 16] = [0xa1; 16];
const REVIEWER: [u8; 16] = [0xb2; 16];
const CHILD: [u8; 16] = [0xc3; 16];
const HERE: [u8; 16] = [0x01; 16];
const STUDIO: [u8; 16] = [0x02; 16];

/// The authority a call to `host` is made with.
fn to(host: [u8; 16]) -> String {
    wire::authority(&uuid::Uuid::from_bytes(host))
}

// --- the stand-in daemon ---------------------------------------------------

/// The authority a call arrived with, which the stand-in's layer puts in
/// each request.
#[derive(Clone, Debug)]
struct Authority(String);

/// What the tool server asked the daemon, in order, with the authority
/// each act was addressed to.
#[derive(Clone, Debug)]
enum Call {
    Inventory,
    Resolve(String),
    ResolveHost(String),
    Send(String, SendMessageRequest),
    Create(String, Box<CreateAgentRequest>),
    Stop(String, SetFilingRequest),
}

#[derive(Clone, Default)]
struct Fleet {
    hosts: Vec<HostEntry>,
    agents: Vec<Row>,
    /// Names the daemon finds several agents by.
    ambiguous: Vec<String>,
    /// The reason SendMessage refuses with, if it does.
    refuse_send: Option<Status>,
    /// The reason SetFiling refuses with, if it does.
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

/// The authority `request` arrived with.
fn authority<T>(request: &Request<T>) -> String {
    request
        .extensions()
        .get::<Authority>()
        .map(|authority| authority.0.clone())
        .unwrap_or_default()
}

/// The host a call is answered for: the one its authority names, else the
/// daemon's own.
fn answering(authority: &str) -> Vec<u8> {
    wire::host_of_authority(authority)
        .map_or_else(|| HERE.to_vec(), |host| host.as_bytes().to_vec())
}

type Events = Pin<
    Box<
        dyn futures_util::Stream<Item = Result<staging::SubscribeInventoryResponse, Status>> + Send,
    >,
>;

#[tonic::async_trait]
impl InventoryService for Fleet {
    type SubscribeInventoryStream = Events;

    async fn subscribe_inventory(
        &self,
        _: Request<staging::SubscribeInventoryRequest>,
    ) -> Result<Response<Events>, Status> {
        self.record(Call::Inventory);
        let events = self
            .hosts
            .iter()
            .cloned()
            .map(wire::inventory_event::Of::Host)
            .chain(
                self.agents
                    .iter()
                    .cloned()
                    .map(wire::inventory_event::Of::Agent),
            )
            .chain([wire::inventory_event::Of::CaughtUp(Default::default())])
            .map(|of| {
                Ok(staging::SubscribeInventoryResponse {
                    event: Some(InventoryEvent { of: Some(of) }),
                })
            })
            .collect::<Vec<_>>();
        // A subscription stays open after CaughtUp, as the daemon's does.
        let events = futures_util::StreamExt::chain(
            futures_util::stream::iter(events),
            futures_util::stream::pending(),
        );
        Ok(Response::new(Box::pin(events)))
    }

    async fn resolve_agent(
        &self,
        request: Request<staging::ResolveAgentRequest>,
    ) -> Result<Response<staging::ResolveAgentResponse>, Status> {
        let name = request.into_inner().name;
        self.record(Call::Resolve(name.clone()));
        if self.ambiguous.contains(&name) {
            let message = format!(
                "{name} names more than one agent; pick one with {name}@here or {name}@studio"
            );
            let error = wire::Error::new(wire::ErrorCode::FailedPrecondition, message.clone());
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
            .map(|agent| Response::new(staging::ResolveAgentResponse { agent: Some(agent) }))
            .ok_or_else(|| Status::not_found(format!("no agent named {name}")))
    }

    /// Among this host and the trusted ones, an exact match first, then
    /// one ignoring case, as the daemon resolves a host.
    async fn resolve_host(
        &self,
        request: Request<staging::ResolveHostRequest>,
    ) -> Result<Response<staging::ResolveHostResponse>, Status> {
        let name = request.into_inner().name;
        self.record(Call::ResolveHost(name.clone()));
        let trusted = || self.hosts.iter().filter(|host| host.trusted().is_some());
        trusted()
            .find(|host| host.name() == name)
            .or_else(|| trusted().find(|host| host.name().eq_ignore_ascii_case(&name)))
            .cloned()
            .map(|host| Response::new(staging::ResolveHostResponse { host: Some(host) }))
            .ok_or_else(|| {
                Status::not_found(format!(
                    "no trusted host is named {name}; trusted hosts: laptop, Studio"
                ))
            })
    }
}

#[tonic::async_trait]
impl AgentService for Fleet {
    /// Made on the host the call is addressed to, which refuses a child of
    /// another host's agent that names no folder, as an owner does.
    async fn create_agent(
        &self,
        request: Request<CreateAgentRequest>,
    ) -> Result<Response<staging::CreateAgentResponse>, Status> {
        let to = authority(&request);
        let request = request.into_inner();
        self.record(Call::Create(to.clone(), Box::new(request.clone())));
        self.severed().await?;
        let host = answering(&to);
        if host != HERE && request.cwd.is_empty() {
            return Err(Status::invalid_argument(
                "starting an agent on Studio needs a folder there: pass cwd as a path on \
                 Studio, asking the person which one if you do not know one",
            ));
        }
        let name = request.name.clone().unwrap_or_else(|| "quiet-otter".into());
        Ok(Response::new(staging::CreateAgentResponse {
            agent: Some(row(&request.agent_id, &host, &name)),
        }))
    }

    async fn rename_agent(
        &self,
        _: Request<staging::RenameAgentRequest>,
    ) -> Result<Response<staging::RenameAgentResponse>, Status> {
        Err(Status::unimplemented("rename"))
    }

    async fn set_filing(
        &self,
        request: Request<SetFilingRequest>,
    ) -> Result<Response<staging::SetFilingResponse>, Status> {
        self.record(Call::Stop(authority(&request), request.into_inner()));
        self.severed().await?;
        match &self.refuse_stop {
            Some(status) => Err(status.clone()),
            None => Ok(Response::new(staging::SetFilingResponse {
                agent: Some(Row::default()),
            })),
        }
    }

    async fn resume_agent(
        &self,
        _: Request<staging::ResumeAgentRequest>,
    ) -> Result<Response<staging::ResumeAgentResponse>, Status> {
        Err(Status::unimplemented("resume"))
    }

    async fn delete_agent(
        &self,
        _: Request<staging::DeleteAgentRequest>,
    ) -> Result<Response<staging::DeleteAgentResponse>, Status> {
        Err(Status::unimplemented("delete"))
    }

    async fn preview_delete(
        &self,
        _: Request<staging::PreviewDeleteRequest>,
    ) -> Result<Response<staging::PreviewDeleteResponse>, Status> {
        Err(Status::unimplemented("preview_delete"))
    }

    async fn send_input(
        &self,
        _: Request<staging::SendInputRequest>,
    ) -> Result<Response<wire::SendInputResponse>, Status> {
        // No tool sends a person's input: the stop tool stops.
        Err(Status::unimplemented("send_input"))
    }

    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<Response<staging::SendMessageResponse>, Status> {
        let to = authority(&request);
        let request = request.into_inner();
        self.record(Call::Send(to, request.clone()));
        self.severed().await?;
        if let Some(status) = &self.refuse_send {
            return Err(status.clone());
        }
        Ok(Response::new(staging::SendMessageResponse {
            envelope_id: request.id,
        }))
    }

    async fn put_blob(
        &self,
        _: Request<staging::PutBlobRequest>,
    ) -> Result<Response<staging::PutBlobResponse>, Status> {
        Err(Status::unimplemented("put_blob"))
    }

    async fn get_diff(
        &self,
        _: Request<staging::GetDiffRequest>,
    ) -> Result<Response<staging::GetDiffResponse>, Status> {
        Err(Status::unimplemented("diff"))
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
        // Each call's authority, for the handlers to record: a tonic
        // request keeps the HTTP request's extensions but not its address.
        let addressed =
            tower::util::MapRequestLayer::new(|mut request: http::Request<tonic::body::Body>| {
                let authority = request
                    .uri()
                    .authority()
                    .map(|authority| authority.as_str().to_owned())
                    .unwrap_or_default();
                request.extensions_mut().insert(Authority(authority));
                request
            });
        let task = tokio::spawn(async move {
            let _ = tonic::transport::Server::builder()
                .layer(addressed)
                .add_service(staging::inventory_service_server(fleet.clone()))
                .add_service(staging::agent_service_server(fleet))
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
        incarnation: 1,
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

/// An agent directory with a spec, the tool server serving it, and the
/// agent process's end of its own socket, with what the agent's core
/// would have heard on it.
struct Mcp {
    _root: tempfile::TempDir,
    dir: PathBuf,
    cwd: PathBuf,
    to_server: tokio::io::DuplexStream,
    from_server: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    next_id: u64,
    /// Each word of a spawn the core was told, in order.
    spawns: Arc<Mutex<Vec<Spawn>>>,
}

impl Mcp {
    async fn start() -> Self {
        Self::start_with(true).await
    }

    /// With `agent_listens` unset, nothing listens on the agent's own
    /// socket: an agent process a build before the socket started.
    async fn start_with(agent_listens: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("agents").join(interpret::to_hex(&ME));
        let cwd = root.path().join("work");
        std::fs::create_dir_all(dir.join(agent::PRIVATE)).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let spawns = Arc::new(Mutex::new(Vec::new()));
        if agent_listens {
            let listener = LocalListener::bind(&agent_dir::agent_sock::path(&dir)).unwrap();
            let (heard, mut told) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(agent::spawns::serve(listener, dir.clone(), heard));
            let spawns = spawns.clone();
            tokio::spawn(async move {
                while let Some(agent::spawns::Heard { heard, taken, .. }) = told.recv().await {
                    spawns.lock().unwrap().push(heard);
                    let _ = taken.send(());
                }
            });
        }
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
        tokio::spawn(agent::serve_tools_on(dir.clone(), server_in, server_out));
        let mut mcp = Self {
            _root: root,
            dir,
            cwd,
            to_server,
            from_server: BufReader::new(from_server).lines(),
            next_id: 0,
            spawns,
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

#[tokio::test(flavor = "multi_thread")]
async fn the_server_lists_the_seven_tools() {
    let mut mcp = Mcp::start().await;
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

/// Claude defers every MCP tool behind its tool search, showing the model
/// only the tools' bare names, but always shows a server's instructions:
/// they say briefly what each tool the server lists is for, naming it.
#[tokio::test(flavor = "multi_thread")]
async fn the_server_says_at_initialize_what_each_tool_is_for() {
    let mut mcp = Mcp::start().await;
    let init = mcp
        .request(
            "initialize",
            json!({ "protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": { "name": "test", "version": "1" } }),
        )
        .await;
    let instructions = init["result"]["instructions"]
        .as_str()
        .unwrap_or_else(|| panic!("no instructions: {init}"));
    let listed = mcp.request("tools/list", json!({})).await;
    for tool in listed["result"]["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        assert!(
            instructions.contains(&format!("({name})")),
            "{name} is not said: {instructions}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_lists_the_fleet_read_to_caught_up_and_marks_you() {
    let mut mcp = Mcp::start().await;
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
    let mut mcp = Mcp::start().await;
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

/// The message goes to the recipient's host, by its id, and carries no
/// sender: the caller is the agent the socket serves. The model is
/// answered the message's id.
#[tokio::test(flavor = "multi_thread")]
async fn send_resolves_the_recipient_and_addresses_its_host() {
    let mut mcp = Mcp::start().await;
    let fleet = fleet();
    let _daemon = Daemon::listen(&mcp.dir, fleet.clone());
    let sent = mcp
        .ok(
            "send",
            json!({ "to": "reviewer", "text": "Please review.", "context": "parser" }),
        )
        .await;
    let calls = fleet.calls();
    let [Call::Resolve(name), Call::Send(addressed, request)] = calls.as_slice() else {
        panic!("resolve then send, got {calls:?}");
    };
    assert_eq!(name, "reviewer");
    assert_eq!(*addressed, to(STUDIO), "sent to the recipient's host");
    assert_eq!(request.agent_id, REVIEWER.to_vec());
    assert_eq!(request.id.len(), 16);
    assert_eq!(sent, json!({ "id": interpret::to_hex(&request.id) }));
    assert_eq!(
        request
            .message
            .as_ref()
            .map(|message| message.text.as_str()),
        Some("Please review.")
    );
    assert_eq!(request.context.as_deref(), Some(b"parser".as_slice()));
}

#[tokio::test(flavor = "multi_thread")]
async fn send_says_plainly_when_the_name_or_the_delivery_fails() {
    let mut mcp = Mcp::start().await;
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

/// With no host named the child is created by the daemon the call reached,
/// which is the parent's own, in the parent's directory. A named host is
/// resolved among this host and the trusted ones, and the create is
/// addressed to its id with the directory left to it: this host places the
/// child where the parent works, and another host is given a folder there
/// or refuses.
#[tokio::test(flavor = "multi_thread")]
async fn spawn_places_the_child_by_host_name_defaulting_to_the_parents_host() {
    let mut mcp = Mcp::start().await;
    let fleet = fleet();
    let _daemon = Daemon::listen(&mcp.dir, fleet.clone());

    let spawned = mcp
        .ok(
            "spawn",
            json!({ "kind": "codex", "prompt": "Write the tests.", "name": "tester" }),
        )
        .await;
    let calls = fleet.calls();
    let [Call::Create(addressed, here)] = calls.as_slice() else {
        panic!("one create, got {calls:?}");
    };
    assert_eq!(
        spawned,
        json!({ "name": "tester", "id": interpret::to_hex(&here.agent_id) })
    );
    assert_eq!(
        wire::host_of_authority(addressed),
        None,
        "the daemon that took the call makes it: {addressed}"
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

    let spawned = mcp
        .ok(
            "spawn",
            json!({ "kind": "claude", "prompt": "Review it.", "host": "studio", "cwd": "~/src/app" }),
        )
        .await;
    let calls = fleet.calls();
    let [_, Call::ResolveHost(name), Call::Create(addressed, there)] = calls.as_slice() else {
        panic!("the host resolved, then a second create, got {calls:?}");
    };
    assert_eq!(name, "studio");
    assert_eq!(*addressed, to(STUDIO), "the create goes to the host's id");
    assert_eq!(there.cwd, "~/src/app");
    assert_eq!(
        there.agent,
        Some(create_agent_request::Agent::ClaudeSdk(Default::default()))
    );
    assert_eq!(spawned["name"], "quiet-otter", "{spawned}");

    mcp.ok(
        "spawn",
        json!({ "kind": "codex", "prompt": "Look here.", "host": "laptop" }),
    )
    .await;
    let calls = fleet.calls();
    let [.., Call::Create(addressed, named_here)] = calls.as_slice() else {
        panic!("a third create, got {calls:?}");
    };
    assert_eq!(*addressed, to(HERE));
    assert_eq!(
        named_here.cwd, "",
        "a named host places the child itself: here, where the parent works"
    );

    assert_eq!(
        mcp.refused(
            "spawn",
            json!({ "kind": "codex", "prompt": "x", "host": "Studio" })
        )
        .await,
        "starting an agent on Studio needs a folder there: pass cwd as a path on Studio, \
         asking the person which one if you do not know one",
        "a path here means nothing there, so none is sent, and the host asks for one"
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
    let calls = fleet.calls();
    assert!(
        matches!(calls.as_slice(), [.., Call::ResolveHost(stranger)] if stranger == "stranger"),
        "no host is named stranger, so nothing was created, and the unknown kind never reached the daemon: {calls:?}"
    );
}

/// A spawn goes to the agent process, which asks the daemon once and hands
/// its answer back. The agent's core hears of the child before the daemon
/// does, then what became of it: made in an incarnation, refused, or taken
/// by a daemon that went away without saying.
#[tokio::test(flavor = "multi_thread")]
async fn a_spawn_goes_through_the_agent_process_which_asks_the_daemon_once() {
    let mut mcp = Mcp::start().await;
    let fleet = fleet();
    let daemon = Daemon::listen(&mcp.dir, fleet.clone());
    let spawned = mcp
        .ok(
            "spawn",
            json!({ "kind": "codex", "prompt": "Write the tests." }),
        )
        .await;
    let calls = fleet.calls();
    let [Call::Create(_, created)] = calls.as_slice() else {
        panic!("one create, got {calls:?}");
    };
    assert_eq!(
        spawned,
        json!({ "name": "quiet-otter", "id": interpret::to_hex(&created.agent_id) }),
        "the daemon's answer came back"
    );
    assert_eq!(
        *mcp.spawns.lock().unwrap(),
        [Spawn::Requested, Spawn::Created { incarnation: 1 }]
    );

    mcp.refused(
        "spawn",
        json!({ "kind": "codex", "prompt": "x", "host": "studio" }),
    )
    .await;
    assert_eq!(
        mcp.spawns.lock().unwrap()[2..],
        [Spawn::Requested, Spawn::Refused],
        "the host refused a child with no folder there"
    );
    daemon.stop().await;

    let severed = Fleet {
        sever: Some(Arc::default()),
        ..self::fleet()
    };
    let _daemon = Daemon::listen(&mcp.dir, severed.clone());
    let refusal = mcp
        .refused("spawn", json!({ "kind": "codex", "prompt": "x" }))
        .await;
    assert!(refusal.contains("may have taken it"), "{refusal}");
    assert_eq!(
        mcp.spawns.lock().unwrap()[4..],
        [Spawn::Requested, Spawn::Unconfirmed]
    );
    assert_eq!(severed.calls().len(), 1, "asked once");
}

/// With no daemon the agent process says so, as the tool server does for
/// its own calls, and no child was made.
#[tokio::test(flavor = "multi_thread")]
async fn a_spawn_with_no_daemon_is_refused_plainly() {
    let mut mcp = Mcp::start().await;
    assert_eq!(
        mcp.refused("spawn", json!({ "kind": "codex", "prompt": "x" }))
            .await,
        NOT_RUNNING
    );
    assert_eq!(
        *mcp.spawns.lock().unwrap(),
        [Spawn::Requested, Spawn::Refused]
    );
}

/// An agent process without a socket of its own (one a build before the
/// socket started) takes no spawn: the tool server says so at once and does not
/// go to the daemon in its place.
#[tokio::test(flavor = "multi_thread")]
async fn a_spawn_whose_agent_process_is_not_listening_is_refused_plainly() {
    let mut mcp = Mcp::start_with(false).await;
    let fleet = fleet();
    let _daemon = Daemon::listen(&mcp.dir, fleet.clone());
    assert_eq!(
        mcp.refused("spawn", json!({ "kind": "codex", "prompt": "x" }))
            .await,
        NOT_TAKING_SPAWNS
    );
    assert!(fleet.calls().is_empty(), "{:?}", fleet.calls());
}

/// An ask of a kind the agent process does not know, as a later build's
/// tool server would send an earlier agent process, is refused in words:
/// nothing is told to the core and nothing goes to the daemon.
#[tokio::test(flavor = "multi_thread")]
async fn an_ask_the_agent_process_does_not_know_is_refused_in_words() {
    let mcp = Mcp::start().await;
    let fleet = fleet();
    let _daemon = Daemon::listen(&mcp.dir, fleet.clone());
    let answer = agent_dir::agent_sock::ask(&mcp.dir, &wire::AgentAsk { of: None })
        .await
        .unwrap();
    assert_eq!(
        answer.of,
        Some(wire::agent_answer::Of::Refused(
            agent_dir::agent_sock::UNKNOWN_ASK.to_owned()
        ))
    );
    assert!(mcp.spawns.lock().unwrap().is_empty());
    assert!(fleet.calls().is_empty(), "{:?}", fleet.calls());
}

/// The stop tool stops, as a person's stop does: the child's host ends it
/// now and its descendants with it. It is not an interrupt. The stop goes
/// to the child's host, which refuses an agent that is not its parent.
#[tokio::test(flavor = "multi_thread")]
async fn stop_closes_the_named_child_on_its_host() {
    let mut mcp = Mcp::start().await;
    let fleet = fleet();
    let daemon = Daemon::listen(&mcp.dir, fleet.clone());
    assert_eq!(mcp.ok("stop", json!({ "name": "helper" })).await, json!({}));
    let calls = fleet.calls();
    let [Call::Resolve(_), Call::Stop(addressed, request)] = calls.as_slice() else {
        panic!("resolve then stop, got {calls:?}");
    };
    assert_eq!(*addressed, to(HERE), "the child's host");
    assert_eq!(request.agent_id, CHILD.to_vec());
    assert_eq!(request.filing(), wire::Filing::Closed);
    daemon.stop().await;

    let fleet = Fleet {
        refuse_stop: Some(Status::permission_denied(
            "an agent settles only its own children",
        )),
        ..self::fleet()
    };
    let _daemon = Daemon::listen(&mcp.dir, fleet.clone());
    assert_eq!(
        mcp.refused("stop", json!({ "name": "reviewer" })).await,
        "reviewer did not stop: an agent settles only its own children"
    );
    let calls = fleet.calls();
    let [Call::Resolve(_), Call::Stop(addressed, _)] = calls.as_slice() else {
        panic!("resolve then stop, got {calls:?}");
    };
    assert_eq!(*addressed, to(STUDIO), "the owner refused it");
}

/// status and attach never dial: here no daemon exists at all.
#[tokio::test(flavor = "multi_thread")]
async fn status_answers_ok_without_the_daemon() {
    let mut mcp = Mcp::start().await;
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
    assert!(started.elapsed() < RETRY_WINDOW, "no daemon was waited for");
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_stores_the_file_by_its_hash_and_answers_the_element() {
    use sha2::Digest as _;

    let mut mcp = Mcp::start().await;
    let bytes = b"\x89PNG\r\n\x1a\nnot really a picture";
    std::fs::write(mcp.cwd.join("chart.png"), bytes).unwrap();
    let hash = sha2::Sha256::digest(bytes).to_vec();
    let started = tokio::time::Instant::now();

    let (element, error) = mcp.call("attach", json!({ "path": "chart.png" })).await;
    assert!(!error, "{element}");
    assert!(started.elapsed() < RETRY_WINDOW, "no daemon was waited for");
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

/// The attach tool calls a file an image only if its bytes are one that
/// both providers read natively, typed by the bytes: an SVG is a file, and
/// a JPEG named `.png` is a JPEG.
#[tokio::test(flavor = "multi_thread")]
async fn attach_calls_a_file_an_image_by_its_bytes() {
    let mut mcp = Mcp::start().await;
    std::fs::write(
        mcp.cwd.join("logo.svg"),
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    )
    .unwrap();
    std::fs::write(mcp.cwd.join("shot.png"), b"\xff\xd8\xff\xe0 a jpeg").unwrap();
    let of = |element: &str| {
        attachments::parse(element).positioned.attachments[0]
            .of
            .clone()
            .unwrap()
    };
    let (svg, _) = mcp.call("attach", json!({ "path": "logo.svg" })).await;
    assert!(
        matches!(of(&svg), wire::attachment::Of::File(_)),
        "an SVG is attached as an image: {svg}"
    );
    let (jpeg, _) = mcp.call("attach", json!({ "path": "shot.png" })).await;
    let wire::attachment::Of::Image(blob) = of(&jpeg) else {
        panic!("a JPEG is not attached as an image: {jpeg}");
    };
    assert_eq!(blob.mime, "image/jpeg");
}

/// A file over the attachment limit is refused in words naming the limit,
/// before its bytes are read or stored: the recipient could not be sent it.
#[tokio::test(flavor = "multi_thread")]
async fn attach_refuses_a_file_over_the_attachment_limit() {
    let mut mcp = Mcp::start().await;
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
    let mut mcp = Mcp::start().await;
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
            .any(|call| matches!(call, Call::Send(..))),
        "nothing was sent"
    );
}

/// A daemon that comes up inside the window is reached; with none the call
/// says so once the window has passed.
#[tokio::test(flavor = "multi_thread")]
async fn a_fleet_call_retries_over_an_update_and_reports_a_missing_daemon() {
    let mut mcp = Mcp::start().await;
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
        waited >= RETRY_WINDOW - Duration::from_millis(600) && waited < RETRY_WINDOW * 2,
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
        let mut mcp = Mcp::start().await;
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
                Call::Send(..) => Some("send"),
                Call::Create(..) => Some("create"),
                Call::Stop(..) => Some("stop"),
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
            Call::Send(_, request) => Some(request),
            _ => None,
        })
        .expect("the daemon took a message");
    assert_eq!(
        sent.message.as_ref().map(|message| message.text.as_str()),
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
                "message to {} {:?} {} {}",
                message.to,
                message.send_state(),
                item.text,
                message.rejection
            ),
            Drawn::Tool(name) => format!("tool row {name}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            "message to reviewer Sent Ready for review. ",
            "message to ghost Rejected Anyone? no agent named ghost",
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
    use wire::{claude_item, codex_item, work};
    let body = item.body.as_slice();
    match kind {
        Kind::ClaudePty | Kind::ClaudeSdk => match wire::ClaudeItem::decode(body).ok()?.kind? {
            claude_item::Kind::AgentMessage(message) => Some(Drawn::Message(message)),
            claude_item::Kind::Tool(tool) => Some(Drawn::Tool(tool.name)),
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

/// A one-shot agent whose only turn spawns with `arguments` through the
/// whole path: the provider's call, the tool server at the install path,
/// the agent's own socket and its call to the daemon's tools.sock.
/// The daemon never sends a `children` frame.
async fn a_one_shot_agent_that_spawns(arguments: Value) -> (Agent, support::Daemon, Fleet, Daemon) {
    let agent = Agent::start(Setup {
        parent: true,
        steps: vec![
            Step::Tool(Tool {
                name: Some("mcp__amux__spawn".to_owned()),
                class: ToolClass::Consequential,
                input: Some(arguments),
                outcome: Default::default(),
                wait_for: None,
            }),
            Step::TurnEnd,
        ],
        ..Setup::sdk()
    })
    .await;
    let fleet = fleet();
    let tools = Daemon::listen(&agent.dir, fleet.clone());
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "go").await, Verdict::Accepted);
    agent
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    (agent, daemon, fleet, tools)
}

/// Whether the agent marked its one turn end as its incarnation's last.
fn marked_last(agent: &Agent) -> Vec<bool> {
    agent
        .log()
        .steps
        .iter()
        .filter_map(|step| step.turn_end.as_ref().map(|end| end.last))
        .collect()
}

/// A sub-agent ends the turn it spawned a child in, and its daemon has not
/// named the child: the turn's end and the daemon's word of the child come
/// on different sockets, in either order. It stays all the same, since its
/// own process carried the spawn to the daemon.
#[tokio::test(flavor = "multi_thread")]
async fn a_sub_agent_stays_for_the_child_it_spawned_before_its_daemon_names_it() {
    let (agent, mut daemon, fleet, _tools) =
        a_one_shot_agent_that_spawns(json!({ "kind": "codex", "prompt": "Write the tests." }))
            .await;
    assert_eq!(fleet.calls().len(), 1, "{:?}", fleet.calls());
    assert_eq!(marked_last(&agent), [false], "its child still works");
    assert!(!agent.finished());
    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
}

/// A sub-agent that spawns a child on another host stays for it too: its
/// process carried the create to that host and heard the child was made,
/// and the daemon here hears of the child only once that host's rows are
/// copied to it, if before the turn ends at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_sub_agent_stays_for_a_child_it_spawned_on_another_host() {
    let (agent, mut daemon, fleet, _tools) = a_one_shot_agent_that_spawns(
        json!({ "kind": "codex", "prompt": "Write the tests.", "host": "Studio", "cwd": "~/src/app" }),
    )
    .await;
    let calls = fleet.calls();
    let [Call::ResolveHost(_), Call::Create(addressed, _)] = calls.as_slice() else {
        panic!("the host resolved, then the create, got {calls:?}");
    };
    assert_eq!(*addressed, to(STUDIO));
    assert_eq!(marked_last(&agent), [false], "its child still works");
    assert!(!agent.finished());
    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
}

/// A spawn the child's host refused made no child, and the sub-agent's
/// turn end is its last.
#[tokio::test(flavor = "multi_thread")]
async fn a_sub_agent_whose_spawn_was_refused_finishes_at_its_turns_end() {
    let (agent, _daemon, fleet, _tools) =
        a_one_shot_agent_that_spawns(json!({ "kind": "codex", "prompt": "x", "host": "Studio" }))
            .await;
    let calls = fleet.calls();
    assert!(
        matches!(calls.as_slice(), [Call::ResolveHost(_), Call::Create(..)]),
        "{calls:?}"
    );
    assert_eq!(agent.exit().await, ExitCause::Finished);
    assert_eq!(marked_last(&agent), [true]);
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
