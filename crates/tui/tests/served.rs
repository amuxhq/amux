//! The terminal client against served hosts: a testnet of real daemons and
//! agent processes on the fake providers, observed through a client
//! service the way a terminal subscribes. Drafts across leaving a chat and
//! the client closing, prompts held while a host is out of reach, and
//! chats on agents a far host deleted or stopped linking.
//!
//! Unix only: every served host here runs terminal Claude agents, which
//! Windows does not host (see docs/ARCHITECTURE.md, "Windows, as a stated
//! cost").
#![cfg(unix)]

use std::time::Duration;

use provider_fakes::script::Step;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use testnet::{AgentDecl, FakeKind, Net, PATIENCE, Topology};
use tui::{ColorMode, Theme};

fn theme() -> Theme {
    Theme::dark(ColorMode::TrueColor)
}

/// A desk and a laptop that reach each other.
fn topology() -> Topology {
    Topology::new()
        .host("desk")
        .host("laptop")
        .link("desk", "laptop")
}

fn text(chunk: &str) -> Step {
    Step::Text {
        chunks: vec![chunk.to_owned()],
    }
}

// --- a draft across leaving the chat -----------------------------------------

/// The client's next event from a task it spawned (a chat opening), applied.
async fn next_app_event(app: &mut tui::App) {
    let event = tokio::time::timeout(PATIENCE, app.receiver.recv())
        .await
        .expect("the client's task answers")
        .expect("the client's event channel is open");
    app.event(event);
}

/// Opens `id`'s chat the way Enter on its home row does, and waits for it.
async fn open_chat(app: &mut tui::App, id: &[u8]) {
    app.config.initial_chat = Some(id.to_vec());
    app.fleet_changed();
    next_app_event(app).await;
    assert!(
        app.chat
            .as_ref()
            .is_some_and(|chat| chat.view.agent_id == id),
        "the chat opened"
    );
}

/// docs/TERMINAL.md: the client owns the drafts, and "the composer takes a
/// draft at any time". A half-written prompt is the person's work: going
/// home with the leader then `s` and coming back to the same chat must find
/// it in the composer, as the phone keeps it (docs/IOS.md: leaving the page
/// loses none of the draft).
#[tokio::test(flavor = "multi_thread")]
async fn a_draft_left_in_a_chat_is_in_its_composer_when_the_chat_opens_again() {
    let net = Net::start(
        Topology::new().host("desk").agent(
            AgentDecl::new("worker", "desk")
                .kind(FakeKind::ClaudeSdk)
                .steps(vec![text("Ready."), Step::TurnEnd])
                .prompt("go"),
        ),
    )
    .await
    .unwrap();
    net.settle("worker").await.unwrap();
    let worker = net.agent("worker").unwrap().id;
    let client: std::sync::Arc<dyn client::Client> =
        std::sync::Arc::new(client::InProcess::new(net.client("desk").unwrap()));
    let fleet = ui_runtime::Fleet::connect(client.clone(), client::SystemClock)
        .await
        .unwrap();
    let config = tui::TuiConfig {
        working_dir: "/".into(),
        leader: 'a',
        theme: theme(),
        initial_chat: None,
        version: String::new(),
        local_host: net.host("desk").unwrap().host_id.as_bytes().to_vec(),
        layout: None,
        chat_in: settings::ChatInSetting::Amux,
        defaults: settings::NewAgentSettings::default(),
    };
    let mut app = tui::App::new(client, fleet, config);
    let mut changed = app.fleet.changed();
    while app.fleet.state().find(worker.as_bytes()).is_none() {
        tokio::time::timeout(PATIENCE, changed.changed())
            .await
            .expect("the fleet lists the worker")
            .unwrap();
    }

    open_chat(&mut app, worker.as_bytes()).await;
    for c in "important unsent draft".chars() {
        app.key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char(c),
        ));
    }
    assert_eq!(
        app.chat.as_ref().unwrap().view.editor.text(),
        "important unsent draft"
    );
    // The leader, then `s`: home.
    app.key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('a'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    app.key(crossterm::event::KeyEvent::from(
        crossterm::event::KeyCode::Char('s'),
    ));
    assert!(app.chat.is_none(), "home");

    open_chat(&mut app, worker.as_bytes()).await;
    assert_eq!(
        app.chat.as_ref().unwrap().view.editor.text(),
        "important unsent draft",
        "the draft waits in the chat's composer"
    );

    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

// --- the client closing, and a chat closing under the person ----------------

/// A served net of idle headless Claude agents on desk, and a client of desk.
async fn desk_of(names: &[&str]) -> (Net, std::sync::Arc<dyn client::Client>, tui::TuiConfig) {
    let mut topology = Topology::new().host("desk");
    for name in names {
        topology = topology.agent(
            AgentDecl::new(name, "desk")
                .kind(FakeKind::ClaudeSdk)
                .steps(vec![text("Ready."), Step::TurnEnd])
                .prompt("go"),
        );
    }
    let net = Net::start(topology).await.unwrap();
    for name in names {
        net.settle(name).await.unwrap();
    }
    let client: std::sync::Arc<dyn client::Client> =
        std::sync::Arc::new(client::InProcess::new(net.client("desk").unwrap()));
    let config = tui::TuiConfig {
        working_dir: "/".into(),
        leader: 'a',
        theme: theme(),
        initial_chat: None,
        version: String::new(),
        local_host: net.host("desk").unwrap().host_id.as_bytes().to_vec(),
        layout: None,
        chat_in: settings::ChatInSetting::Amux,
        defaults: settings::NewAgentSettings::default(),
    };
    (net, client, config)
}

/// A client of desk whose fleet lists every one of `ids`.
async fn app_listing(
    client: std::sync::Arc<dyn client::Client>,
    config: tui::TuiConfig,
    ids: &[&[u8]],
) -> tui::App {
    let fleet = ui_runtime::Fleet::connect(client.clone(), client::SystemClock)
        .await
        .unwrap();
    let app = tui::App::new(client, fleet, config);
    let mut changed = app.fleet.changed();
    while ids.iter().any(|id| app.fleet.state().find(id).is_none()) {
        tokio::time::timeout(PATIENCE, changed.changed())
            .await
            .expect("the fleet lists the agents")
            .unwrap();
    }
    app
}

fn press(app: &mut tui::App, code: crossterm::event::KeyCode) {
    app.key(crossterm::event::KeyEvent::from(code));
}

/// A draft is the person's work and outlives the client: closing the
/// terminal client (or detaching) and opening it again finds the unsent
/// words in the agent's composer. Kept on this device, never synced. They
/// were held only in the client's memory and lost.
#[tokio::test(flavor = "multi_thread")]
async fn a_draft_outlives_the_terminal_client_closing() {
    let (net, client, config) = desk_of(&["worker"]).await;
    let worker = net.agent("worker").unwrap().id;
    let mut app = app_listing(client.clone(), config.clone(), &[worker.as_bytes()]).await;
    open_chat(&mut app, worker.as_bytes()).await;
    for c in "important unsent draft".chars() {
        press(&mut app, crossterm::event::KeyCode::Char(c));
    }
    // Quitting saves the drafts before the client goes, as `run` does.
    app.keep_drafts().await;
    drop(app);

    let mut app = app_listing(client, config, &[worker.as_bytes()]).await;
    open_chat(&mut app, worker.as_bytes()).await;
    assert_eq!(
        app.chat.as_ref().unwrap().view.editor.text(),
        "important unsent draft",
        "the draft did not outlive the client"
    );
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// A client of the daemon whose saves of an emptied draft wait while
/// `hold` is set: the clearing a person did is still on its way. It notes
/// the agent of every draft saved.
struct SlowClearing {
    inner: std::sync::Arc<dyn client::Client>,
    hold: tokio::sync::watch::Sender<bool>,
    saved: std::sync::Mutex<Vec<Vec<u8>>>,
}

#[async_trait::async_trait]
impl client::Client for SlowClearing {
    async fn update_draft(
        &self,
        request: wire::UpdateDraftRequest,
    ) -> Result<wire::Draft, client::RpcError> {
        self.saved.lock().unwrap().push(request.agent_id.clone());
        let empty = request
            .draft
            .as_ref()
            .is_none_or(|draft| draft.text.is_empty() && draft.attachments.is_empty());
        if empty {
            let _ = self.hold.subscribe().wait_for(|held| !*held).await;
        }
        self.inner.update_draft(request).await
    }
    async fn get_draft(
        &self,
        request: wire::GetDraftRequest,
    ) -> Result<wire::Draft, client::RpcError> {
        self.inner.get_draft(request).await
    }
    async fn subscribe_inventory(
        &self,
    ) -> Result<client::EventStream<wire::InventoryEvent>, client::RpcError> {
        self.inner.subscribe_inventory().await
    }
    async fn resolve_agent(
        &self,
        request: wire::ResolveAgentRequest,
    ) -> Result<wire::Agent, client::RpcError> {
        self.inner.resolve_agent(request).await
    }
    async fn subscribe_session(
        &self,
        request: wire::SubscribeSessionRequest,
    ) -> Result<client::EventStream<wire::SessionEvent>, client::RpcError> {
        self.inner.subscribe_session(request).await
    }
    async fn list_items(
        &self,
        request: wire::ListItemsRequest,
    ) -> Result<wire::ListItemsResponse, client::RpcError> {
        self.inner.list_items(request).await
    }
    async fn get_item(
        &self,
        request: wire::GetItemRequest,
    ) -> Result<wire::Item, client::RpcError> {
        self.inner.get_item(request).await
    }
    async fn send_input(
        &self,
        request: wire::SendInputRequest,
    ) -> Result<wire::SendInputResponse, client::RpcError> {
        self.inner.send_input(request).await
    }
    async fn create_agent(
        &self,
        request: wire::CreateAgentRequest,
    ) -> Result<wire::Agent, client::RpcError> {
        self.inner.create_agent(request).await
    }
    async fn rename_agent(
        &self,
        request: wire::RenameAgentRequest,
    ) -> Result<wire::Agent, client::RpcError> {
        self.inner.rename_agent(request).await
    }
    async fn stop_agent(&self, request: wire::StopAgentRequest) -> Result<(), client::RpcError> {
        self.inner.stop_agent(request).await
    }
    async fn resume_agent(
        &self,
        request: wire::ResumeAgentRequest,
    ) -> Result<wire::Agent, client::RpcError> {
        self.inner.resume_agent(request).await
    }
    async fn delete_agent(
        &self,
        request: wire::DeleteAgentRequest,
    ) -> Result<(), client::RpcError> {
        self.inner.delete_agent(request).await
    }
    async fn send_message(
        &self,
        request: wire::SendMessageRequest,
    ) -> Result<wire::SendMessageResponse, client::RpcError> {
        self.inner.send_message(request).await
    }
    async fn put_blob(
        &self,
        request: wire::PutBlobRequest,
    ) -> Result<wire::BlobRef, client::RpcError> {
        self.inner.put_blob(request).await
    }
    async fn get_blob(
        &self,
        request: wire::GetBlobRequest,
    ) -> Result<wire::GetBlobResponse, client::RpcError> {
        self.inner.get_blob(request).await
    }
    async fn get_diff(
        &self,
        request: wire::GetDiffRequest,
    ) -> Result<wire::Diff, client::RpcError> {
        self.inner.get_diff(request).await
    }
    async fn get_catalogue(
        &self,
        request: wire::GetCatalogueRequest,
    ) -> Result<wire::GetCatalogueResponse, client::RpcError> {
        self.inner.get_catalogue(request).await
    }
    async fn list_repositories(
        &self,
        request: wire::ListRepositoriesRequest,
    ) -> Result<wire::ListRepositoriesResponse, client::RpcError> {
        self.inner.list_repositories(request).await
    }
    async fn dump(
        &self,
        request: wire::DumpRequest,
    ) -> Result<wire::DumpResponse, client::RpcError> {
        self.inner.dump(request).await
    }
    async fn submit_report(
        &self,
        request: wire::SubmitReportRequest,
    ) -> Result<wire::SubmitReportResponse, client::RpcError> {
        self.inner.submit_report(request).await
    }
}

/// A draft the person cleared stays cleared while the daemon's copy is
/// still being emptied: reopening the chat at once waits for the emptying
/// and finds the composer empty, not the words this machine kept before.
#[tokio::test(flavor = "multi_thread")]
async fn a_cleared_draft_stays_cleared_before_the_daemon_hears_of_it() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (net, daemon, config) = desk_of(&["worker"]).await;
    let worker = net.agent("worker").unwrap().id;
    let stale = || wire::GetDraftRequest {
        agent_id: worker.as_bytes().to_vec(),
    };
    daemon
        .update_draft(wire::UpdateDraftRequest {
            agent_id: worker.as_bytes().to_vec(),
            draft: Some(wire::Draft {
                text: "kept from before".into(),
                attachments: Vec::new(),
            }),
        })
        .await
        .unwrap();
    let slow = std::sync::Arc::new(SlowClearing {
        inner: daemon.clone(),
        hold: tokio::sync::watch::Sender::new(true),
        saved: Default::default(),
    });
    let client: std::sync::Arc<dyn client::Client> = slow.clone();
    let mut app = app_listing(client, config, &[worker.as_bytes()]).await;
    let home = |app: &mut tui::App| {
        app.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        press(app, KeyCode::Char('s'));
        assert!(app.chat.is_none(), "home");
    };
    open_chat(&mut app, worker.as_bytes()).await;
    assert_eq!(
        app.chat.as_ref().unwrap().view.editor.text(),
        "kept from before"
    );
    app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.chat.as_ref().unwrap().view.editor.is_empty(), "cleared");
    home(&mut app);

    // The emptying lands only once the chat is opening again.
    let release = slow.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        release.hold.send_replace(false);
    });
    open_chat(&mut app, worker.as_bytes()).await;
    assert_eq!(
        app.chat.as_ref().unwrap().view.editor.text(),
        "",
        "the cleared draft came back from the daemon's old copy"
    );
    home(&mut app);
    tokio::time::timeout(PATIENCE, app.keep_drafts())
        .await
        .expect("the quit's save lands");
    let left = daemon.get_draft(stale()).await.unwrap();
    assert!(
        left.text.is_empty(),
        "the daemon still keeps {:?} after the client quit",
        left.text
    );
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// A chat that closes because its agent was deleted keeps no draft: the
/// words in its composer went back to this machine's daemon as the chat
/// closed, and again as the client quit, for an agent that no longer
/// exists.
#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_agents_chat_keeps_no_draft() {
    use crossterm::event::KeyCode;
    let (net, daemon, config) = desk_of(&["doomed"]).await;
    let doomed = net.agent("doomed").unwrap().id;
    let recording = std::sync::Arc::new(SlowClearing {
        inner: daemon.clone(),
        hold: tokio::sync::watch::Sender::new(false),
        saved: Default::default(),
    });
    let client: std::sync::Arc<dyn client::Client> = recording.clone();
    let mut app = app_listing(client, config, &[doomed.as_bytes()]).await;
    open_chat(&mut app, doomed.as_bytes()).await;
    for c in "half a thought".chars() {
        press(&mut app, KeyCode::Char(c));
    }

    daemon
        .delete_agent(wire::DeleteAgentRequest {
            agent_id: doomed.as_bytes().to_vec(),
        })
        .await
        .unwrap();
    tokio::time::timeout(PATIENCE, async {
        while app.chat.is_some() {
            app.housekeep();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the deleted agent's chat closes");
    tokio::time::timeout(PATIENCE, app.keep_drafts())
        .await
        .expect("the quit's save lands");

    assert!(
        !recording
            .saved
            .lock()
            .unwrap()
            .contains(&doomed.as_bytes().to_vec()),
        "the deleted agent's draft was saved"
    );
    let left = daemon
        .get_draft(wire::GetDraftRequest {
            agent_id: doomed.as_bytes().to_vec(),
        })
        .await
        .unwrap();
    assert!(left.text.is_empty(), "the daemon keeps {:?}", left.text);
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// When a chat closes under the person (its agent deleted elsewhere), the
/// rest of what they are typing never acts on home: "fix x" then Enter
/// opened the window to end the focused agent and confirmed it. Home holds
/// keys for the same half second an ask does; after that its keys work.
#[tokio::test(flavor = "multi_thread")]
async fn words_typed_into_a_chat_that_closes_under_the_person_end_no_other_agent() {
    use crossterm::event::KeyCode;
    let (net, client, config) = desk_of(&["doomed", "healthy"]).await;
    let doomed = net.agent("doomed").unwrap().id;
    let healthy = net.agent("healthy").unwrap().id;
    let mut app = app_listing(
        client.clone(),
        config,
        &[doomed.as_bytes(), healthy.as_bytes()],
    )
    .await;
    // Home's focus on healthy, as the person left it.
    press(&mut app, KeyCode::Char('/'));
    for c in "healthy".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down);
    open_chat(&mut app, doomed.as_bytes()).await;

    client
        .delete_agent(wire::DeleteAgentRequest {
            agent_id: doomed.as_bytes().to_vec(),
        })
        .await
        .unwrap();
    tokio::time::timeout(PATIENCE, async {
        while app.chat.is_some() {
            app.housekeep();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the deleted agent's chat closes");
    for c in "fix x".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    let ended = |app: &tui::App| {
        app.fleet
            .state()
            .find(healthy.as_bytes())
            .is_none_or(|agent| agent.lifecycle() != wire::Lifecycle::Live)
    };
    // A window: a key that home swallows leaves no mark to wait on.
    testnet::holds_for(
        "the words typed as the chat closed to leave healthy running",
        Duration::from_millis(300),
        || std::future::ready(!ended(&app)),
    )
    .await
    .expect("the words typed as the chat closed ended healthy");
    let mut changed = app.fleet.changed();
    // After half a second with no key, home's keys act again.
    tokio::time::sleep(Duration::from_millis(600)).await;
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Enter);
    tokio::time::timeout(PATIENCE, async {
        while !ended(&app) {
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("x then Enter on home ends the focused agent");
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// The whole screen as text, one row a line.
fn screen_text(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Raw attach asked for from inside a chat on another host's agent says
/// why in words that fit the chat: the notice was home's ("enter opens
/// its chat"), and in the chat Enter sends the draft.
#[tokio::test(flavor = "multi_thread")]
async fn raw_attach_from_a_far_agents_chat_does_not_say_enter_opens_its_chat() {
    let net = Net::start(
        topology().agent(
            AgentDecl::new("far", "desk")
                .kind(FakeKind::ClaudePty)
                .steps(vec![text("Ready."), Step::TurnEnd])
                .prompt("go"),
        ),
    )
    .await
    .unwrap();
    net.settle("far").await.unwrap();
    let far = net.agent("far").unwrap().id;
    let client: std::sync::Arc<dyn client::Client> =
        std::sync::Arc::new(client::InProcess::new(net.client("laptop").unwrap()));
    let config = tui::TuiConfig {
        working_dir: "/".into(),
        leader: 'a',
        theme: theme(),
        initial_chat: None,
        version: String::new(),
        local_host: net.host("laptop").unwrap().host_id.as_bytes().to_vec(),
        layout: None,
        chat_in: settings::ChatInSetting::Amux,
        defaults: settings::NewAgentSettings::default(),
    };
    let mut app = app_listing(client, config, &[far.as_bytes()]).await;
    open_chat(&mut app, far.as_bytes()).await;
    app.key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('a'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    press(&mut app, crossterm::event::KeyCode::Char('a'));
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let screen = screen_text(terminal.backend().buffer());
    assert!(
        screen.contains("another machine"),
        "the chat says the terminal is elsewhere:\n{screen}"
    );
    assert!(
        !screen.contains("enter opens its chat"),
        "inside far's chat the notice says Enter opens the chat:\n{screen}"
    );
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// desk's client with `name`'s chat open, `name` a terminal-less agent on
/// laptop, the two linked.
async fn far_chat(name: &str) -> (Net, tui::App) {
    far_chat_via(name, |client| client).await
}

/// [`far_chat`], its client reaching the daemon through `via`.
async fn far_chat_via(
    name: &str,
    via: impl FnOnce(std::sync::Arc<dyn client::Client>) -> std::sync::Arc<dyn client::Client>,
) -> (Net, tui::App) {
    let net = Net::start(
        topology().agent(
            AgentDecl::new(name, "laptop")
                .kind(FakeKind::ClaudeSdk)
                .steps(vec![text("Ready."), Step::TurnEnd])
                .prompt("go"),
        ),
    )
    .await
    .unwrap();
    net.settle(name).await.unwrap();
    let id = net.agent(name).unwrap().id;
    let client: std::sync::Arc<dyn client::Client> =
        std::sync::Arc::new(client::InProcess::new(net.client("desk").unwrap()));
    let client = via(client);
    let config = tui::TuiConfig {
        working_dir: "/".into(),
        leader: 'a',
        theme: theme(),
        initial_chat: None,
        version: String::new(),
        local_host: net.host("desk").unwrap().host_id.as_bytes().to_vec(),
        layout: None,
        chat_in: settings::ChatInSetting::Amux,
        defaults: settings::NewAgentSettings::default(),
    };
    let mut app = app_listing(client, config, &[id.as_bytes()]).await;
    open_chat(&mut app, id.as_bytes()).await;
    (net, app)
}

/// Every screen `app` draws for `patience`, housekeeping between.
async fn screens_for(app: &mut tui::App, patience: Duration) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    let mut screens = Vec::new();
    let until = tokio::time::Instant::now() + patience;
    while tokio::time::Instant::now() < until {
        app.housekeep();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        screens.push(screen_text(terminal.backend().buffer()));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    screens
}

/// Unlinking the agent's host is not the agent ceasing to exist: a chat
/// open on laptop's agent when desk and laptop were unlinked closed with
/// "the chat closed: no agent with that id", while laptop still ran it
/// with an approval open.
#[tokio::test(flavor = "multi_thread")]
async fn unlinking_the_agents_host_never_says_there_is_no_such_agent() {
    let (mut net, mut app) = far_chat("cx3").await;
    net.remove_link("desk", "laptop").await.unwrap();
    let screens = screens_for(&mut app, Duration::from_secs(5)).await;
    if let Some(screen) = screens
        .iter()
        .find(|screen| screen.contains("no agent with that id"))
    {
        panic!("unlinking laptop said cx3 does not exist:\n{screen}");
    }
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// A prompt held while its host can't be reached goes back to the draft if
/// the client closes first, even when it closes abruptly (its pane killed,
/// no quit to save from): the draft this machine keeps takes the held
/// prompt as soon as it is held, as the phone's does, in place of the
/// older draft the daemon kept.
#[tokio::test(flavor = "multi_thread")]
async fn a_held_prompt_is_in_the_kept_draft_before_the_client_quits() {
    use crossterm::event::KeyCode;
    let (mut net, mut app) = far_chat("far").await;
    let id = net.agent("far").unwrap().id;
    let daemon: std::sync::Arc<dyn client::Client> =
        std::sync::Arc::new(client::InProcess::new(net.client("desk").unwrap()));
    daemon
        .update_draft(wire::UpdateDraftRequest {
            agent_id: id.as_bytes().to_vec(),
            draft: Some(wire::Draft {
                text: "an older draft".into(),
                attachments: Vec::new(),
            }),
        })
        .await
        .unwrap();
    net.sever_link("desk", "laptop").unwrap();
    tokio::time::timeout(PATIENCE, async {
        loop {
            let away = app
                .chat
                .as_ref()
                .unwrap()
                .session
                .state()
                .host()
                .is_some_and(|host| host.online().is_none());
            if away {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the laptop reads away");
    for c in "race A".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    let kept = tokio::time::timeout(PATIENCE, async {
        loop {
            app.housekeep();
            let held = app.chat.as_ref().unwrap().session.state().held().count();
            let draft = daemon
                .get_draft(wire::GetDraftRequest {
                    agent_id: id.as_bytes().to_vec(),
                })
                .await
                .unwrap();
            if held == 1 && draft.text == "race A" {
                return draft;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the held prompt is kept as the draft while the client runs");
    assert_eq!(kept.text, "race A");
    // Closed abruptly: no quit, no drop.
    std::mem::forget(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

/// A chat open on an agent deleted from its own host says it was deleted
/// (or closes): it said "exited · aborted" and "Enter resumes", and Enter
/// answered "could not resume: no agent <uuid>".
#[tokio::test(flavor = "multi_thread")]
async fn a_chat_on_an_agent_deleted_on_its_far_host_never_offers_to_resume_it() {
    let (mut net, mut app) = far_chat("i1").await;
    net.delete("i1").await.unwrap();
    let screens = screens_for(&mut app, Duration::from_secs(5)).await;
    let last = screens.last().cloned().unwrap_or_default();
    assert!(
        app.chat.is_none() || last.contains("deleted"),
        "five seconds after i1 was deleted its chat neither closed nor said so:\n{last}"
    );
    if let Some(screen) = screens
        .iter()
        .find(|screen| screen.contains("Enter resumes"))
    {
        panic!("the deleted i1's chat offered to resume it:\n{screen}");
    }
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}
