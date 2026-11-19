//! The terminal client against served hosts: a testnet of real daemons and
//! agent processes on the fake providers, observed through a client
//! service the way a terminal subscribes. Drafts across leaving a chat and
//! the client closing, and chats on agents a far host deleted or stopped
//! linking.
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

/// A client of `host`'s node in memory, through its router and carrier, as
/// the fleet and its chats call it.
fn node(net: &Net, host: &str) -> client::next::Client {
    client::next::Client::in_memory(net.entrance(host).unwrap(), client::next::Surface::Terminal)
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
    let fleet = ui_runtime::Fleet::connect(node(&net, "desk"), client::SystemClock)
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
        system_clipboard: false,
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
async fn desk_of(
    names: &[&str],
) -> (
    Net,
    std::sync::Arc<dyn client::Client>,
    client::next::Client,
    tui::TuiConfig,
) {
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
        system_clipboard: false,
    };
    let node = node(&net, "desk");
    (net, client, node, config)
}

/// A client of desk whose fleet, on `node`, lists every one of `ids`.
async fn app_listing(
    client: std::sync::Arc<dyn client::Client>,
    node: client::next::Client,
    config: tui::TuiConfig,
    ids: &[&[u8]],
) -> tui::App {
    let fleet = ui_runtime::Fleet::connect(node, client::SystemClock)
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
    let (net, client, node, config) = desk_of(&["worker"]).await;
    let worker = net.agent("worker").unwrap().id;
    let mut app = app_listing(
        client.clone(),
        node.clone(),
        config.clone(),
        &[worker.as_bytes()],
    )
    .await;
    open_chat(&mut app, worker.as_bytes()).await;
    for c in "important unsent draft".chars() {
        press(&mut app, crossterm::event::KeyCode::Char(c));
    }
    // Quitting saves the drafts before the client goes, as `run` does.
    app.keep_drafts().await;
    drop(app);

    let mut app = app_listing(client, node, config, &[worker.as_bytes()]).await;
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

/// The daemon's drafts as the fleet reaches them, with its saves of an
/// emptied draft waiting while `hold` is set: the clearing a person did is
/// still on its way. It notes the agent of every draft saved.
#[derive(Clone)]
struct SlowClearing {
    inner: client::next::Client,
    hold: std::sync::Arc<tokio::sync::watch::Sender<bool>>,
    saved: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
}

impl SlowClearing {
    fn new(inner: client::next::Client, held: bool) -> SlowClearing {
        SlowClearing {
            inner,
            hold: std::sync::Arc::new(tokio::sync::watch::Sender::new(held)),
            saved: Default::default(),
        }
    }

    /// A client of `net`'s desk whose drafts go through this, and every
    /// other call to the desk's node as it is.
    fn client(&self, net: &Net) -> client::next::Client {
        use tonic::codegen::http;
        use tower::ServiceExt as _;
        type Server = wire::amux::staging::draft_service_server::DraftServiceServer<SlowClearing>;
        let prefix = format!("/{}/", <Server as tonic::server::NamedService>::NAME);
        let drafts =
            tonic::service::Routes::new(wire::amux::staging::draft_service_server(self.clone()));
        let rest = net.entrance("desk").unwrap();
        let split = tower::service_fn(move |request: http::Request<tonic::body::Body>| {
            let (drafts, rest) = (drafts.clone(), rest.clone());
            let ours = request.uri().path().starts_with(&prefix);
            async move {
                if ours {
                    drafts.oneshot(request).await
                } else {
                    rest.oneshot(request).await
                }
            }
        });
        client::next::Client::in_memory(split, client::next::Surface::Terminal)
    }
}

#[tonic::async_trait]
impl wire::amux::staging::draft_service_server::DraftService for SlowClearing {
    async fn update_draft(
        &self,
        request: tonic::Request<wire::amux::staging::UpdateDraftRequest>,
    ) -> Result<tonic::Response<wire::amux::staging::UpdateDraftResponse>, tonic::Status> {
        let request = request.into_inner();
        self.saved.lock().unwrap().push(request.agent_id.clone());
        let draft = request.draft.unwrap_or_default();
        if draft.text.is_empty() && draft.attachments.is_empty() {
            let _ = self.hold.subscribe().wait_for(|held| !*held).await;
        }
        let kept = self
            .inner
            .drafts()
            .update(&request.agent_id, draft)
            .await
            .map_err(client::RpcError::into_status)?;
        Ok(tonic::Response::new(
            wire::amux::staging::UpdateDraftResponse { draft: Some(kept) },
        ))
    }

    async fn get_draft(
        &self,
        request: tonic::Request<wire::amux::staging::GetDraftRequest>,
    ) -> Result<tonic::Response<wire::amux::staging::GetDraftResponse>, tonic::Status> {
        let kept = self
            .inner
            .drafts()
            .get(&request.into_inner().agent_id)
            .await
            .map_err(client::RpcError::into_status)?;
        Ok(tonic::Response::new(
            wire::amux::staging::GetDraftResponse { draft: Some(kept) },
        ))
    }
}

/// A draft the person cleared stays cleared while the daemon's copy is
/// still being emptied: reopening the chat at once waits for the emptying
/// and finds the composer empty, not the words this machine kept before.
#[tokio::test(flavor = "multi_thread")]
async fn a_cleared_draft_stays_cleared_before_the_daemon_hears_of_it() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (net, daemon, node, config) = desk_of(&["worker"]).await;
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
    let slow = SlowClearing::new(node, true);
    let mut app = app_listing(
        daemon.clone(),
        slow.client(&net),
        config,
        &[worker.as_bytes()],
    )
    .await;
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
    let (net, daemon, node, config) = desk_of(&["doomed"]).await;
    let doomed = net.agent("doomed").unwrap().id;
    let recording = SlowClearing::new(node, false);
    let mut app = app_listing(
        daemon.clone(),
        recording.client(&net),
        config,
        &[doomed.as_bytes()],
    )
    .await;
    open_chat(&mut app, doomed.as_bytes()).await;
    for c in "half a thought".chars() {
        press(&mut app, KeyCode::Char(c));
    }

    daemon
        .delete_agent(wire::DeleteAgentRequest {
            agent_id: doomed.as_bytes().to_vec(),
            ..Default::default()
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
    let (net, client, node, config) = desk_of(&["doomed", "healthy"]).await;
    let doomed = net.agent("doomed").unwrap().id;
    let healthy = net.agent("healthy").unwrap().id;
    let mut app = app_listing(
        client.clone(),
        node,
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
            ..Default::default()
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

/// The leader then `x` in a chat settles its agent and goes home; an idle
/// agent has no work to cut short, so nothing asks first.
#[tokio::test(flavor = "multi_thread")]
async fn the_leader_then_x_settles_the_chats_agent_and_goes_home() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (net, client, node, config) = desk_of(&["finished"]).await;
    let finished = net.agent("finished").unwrap().id;
    let mut app = app_listing(client, node, config, &[finished.as_bytes()]).await;
    open_chat(&mut app, finished.as_bytes()).await;
    let mut changed = app.fleet.changed();
    app.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    press(&mut app, KeyCode::Char('x'));
    assert!(app.chat.is_none(), "home");
    tokio::time::timeout(PATIENCE, async {
        while app
            .fleet
            .state()
            .find(finished.as_bytes())
            .is_none_or(|agent| agent.filing() != wire::Filing::Closed)
        {
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("the agent is settled");
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
        system_clipboard: false,
    };
    let mut app = app_listing(client, node(&net, "laptop"), config, &[far.as_bytes()]).await;
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
    let config = tui::TuiConfig {
        working_dir: "/".into(),
        leader: 'a',
        theme: theme(),
        initial_chat: None,
        version: String::new(),
        local_host: net.host("desk").unwrap().host_id.as_bytes().to_vec(),
        layout: None,
        chat_in: settings::ChatInSetting::Amux,
        system_clipboard: false,
    };
    let mut app = app_listing(client, node(&net, "desk"), config, &[id.as_bytes()]).await;
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

/// A prompt Enter cannot send because its host can't be reached never
/// leaves the composer: refused at once while the chat reads away, or
/// refused by the daemon and put back when the chat still read current.
/// Either way the words are in the composer, its top edge says the host is
/// away, nothing holds the prompt to send later, and the draft this machine
/// keeps has the words before the client quits, so a terminal closed
/// abruptly (its pane killed, no quit to save from) finds them again.
#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_for_a_host_that_cannot_be_reached_stays_in_the_composer_and_the_kept_draft() {
    use crossterm::event::KeyCode;
    let (mut net, mut app) = far_chat("far").await;
    let id = net.agent("far").unwrap().id;
    let daemon: std::sync::Arc<dyn client::Client> =
        std::sync::Arc::new(client::InProcess::new(net.client("desk").unwrap()));
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
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    let screen = tokio::time::timeout(PATIENCE, async {
        loop {
            app.housekeep();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let screen = screen_text(terminal.backend().buffer());
            let chat = app.chat.as_ref().unwrap();
            let underway = ui_view::prompts_underway(&chat.session.state());
            let kept = daemon
                .get_draft(wire::GetDraftRequest {
                    agent_id: id.as_bytes().to_vec(),
                })
                .await
                .unwrap();
            if chat.view.editor.text() == "race A"
                && underway.is_empty()
                && screen.contains("away")
                && kept.text == "race A"
            {
                return screen;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect(
        "the prompt is back in the composer and the kept draft, the edge saying the host is away",
    );
    assert!(!screen.contains("sends when"), "{screen}");
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

/// A file pasted into a new agent's first message waits with the draft, and
/// reaches the agent with the words once the agent exists.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_pasted_into_a_new_agents_first_message_reaches_it() {
    use crossterm::event::KeyCode;

    let (net, client, node, config) = desk_of(&[]).await;
    let host = config.local_host.clone();
    let fleet = ui_runtime::Fleet::connect(node, client::SystemClock)
        .await
        .unwrap();
    let mut app = tui::App::new(client.clone(), fleet, config);
    tokio::time::timeout(PATIENCE, async {
        while app.fleet.state().host(&host).is_none() {
            next_app_event(&mut app).await;
        }
    })
    .await
    .expect("the fleet lists the host");

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    press(&mut app, KeyCode::Char('n'));
    // The draft takes its settings from the defaults as it first shows.
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let bytes = b"\x89PNG\r\n\x1a\nnot really an image".to_vec();
    app.home
        .paste_clipboard(tui::clipboard::ClipboardContent::Image {
            mime: "image/png".to_owned(),
            bytes: bytes.clone(),
        })
        .unwrap();
    for c in "look at this".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);

    let prompt = tokio::time::timeout(PATIENCE, async {
        loop {
            while let Ok(event) = app.receiver.try_recv() {
                app.event(event);
            }
            app.housekeep();
            let found = app.chat.as_ref().and_then(|chat| {
                chat.session
                    .state()
                    .transcript()
                    .iter()
                    .find(|held| matches!(held.class, ui_state::ItemClass::Prompt))
                    .map(|held| held.item.clone())
            });
            if let Some(found) = found {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the first message arrives");
    let agent = app.chat.as_ref().unwrap().view.agent_id.clone();
    assert!(prompt.text.contains("look at this"), "{prompt:?}");
    let Some(wire::attachment::Of::Image(blob)) = prompt
        .attachments
        .first()
        .and_then(|attachment| attachment.of.clone())
    else {
        panic!("the image is not on the message: {prompt:?}");
    };
    let stored = client
        .get_blob(wire::GetBlobRequest {
            agent_id: agent,
            hash: blob.hash,
        })
        .await
        .expect("the file is stored with the agent");
    assert_eq!(stored.bytes, bytes);

    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}

// --- a suspended agent ---------------------------------------------------------

/// A message typed into a suspended agent's chat wakes it: Enter sends it
/// at once, and neither the header nor the composer ever says the agent
/// stopped or the message was not sent. They said "stopped" and "not sent:
/// it had exited", the host refusing a person's input to an agent with no
/// process as if it had exited, and the refusal made the chat read exited.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_typed_to_a_suspended_agent_wakes_it_and_never_reads_stopped() {
    use crossterm::event::KeyCode;
    let options = testnet::NetOptions {
        launch: Some(std::sync::Arc::new(
            |_host: &str, launch: &mut node::Launch| {
                launch.experiments =
                    settings::experiments::Enabled::of(&[&settings::experiments::SUSPENSION]);
                launch.suspension = settings::experiments::suspension::Suspension {
                    after: Duration::from_millis(500),
                };
            },
        )),
        ..testnet::NetOptions::default()
    };
    let net = Net::start_with(
        Topology::new().host("desk").agent(
            AgentDecl::new("napper", "desk")
                .kind(FakeKind::ClaudeSdk)
                .steps(vec![text("Ready."), Step::TurnEnd])
                .prompt("go"),
        ),
        options,
    )
    .await
    .unwrap();
    let id = net.agent("napper").unwrap().id;
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
        system_clipboard: false,
    };
    let mut app = app_listing(client, node(&net, "desk"), config, &[id.as_bytes()]).await;
    open_chat(&mut app, id.as_bytes()).await;
    let suspended = |app: &tui::App| {
        app.fleet
            .state()
            .find(id.as_bytes())
            .is_some_and(|agent| agent.exit_cause() == wire::ExitCause::Suspended)
    };
    tokio::time::timeout(PATIENCE, async {
        while !suspended(&app) {
            app.housekeep();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("napper suspends");
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let screen = screen_text(terminal.backend().buffer());
    assert!(
        !screen.contains("stopped"),
        "suspended, it reads stopped:\n{screen}"
    );

    for c in "wake up".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    tokio::time::timeout(PATIENCE, async {
        loop {
            app.housekeep();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let screen = screen_text(terminal.backend().buffer());
            assert!(
                !screen.contains("stopped") && !screen.contains("not sent"),
                "the message to the suspended agent read as refused:\n{screen}"
            );
            // The session's state is a lock: let go of it before waiting,
            // or the send it would block stalls the runtime's timers.
            let done = {
                let state = app.chat.as_ref().unwrap().session.state();
                let taken = state.transcript().iter().any(|held| {
                    held.item.text.contains("wake up") && !held.item.input_id.is_empty()
                });
                taken && ui_view::prompts_underway(&state).is_empty()
            };
            if done {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        let state = app.chat.as_ref().unwrap().session.state();
        let items: Vec<String> = state
            .transcript()
            .iter()
            .map(|held| format!("{:?} {:?}", held.item.key, held.item.text))
            .collect();
        let agent = state.agent();
        panic!(
            "the woken agent did not take the message: {} prompts underway, the inputs \
             {:?}, the agent {:?} {:?} at incarnation {}, its items {items:?}",
            ui_view::prompts_underway(&state).len(),
            state.inputs(),
            agent.lifecycle(),
            agent.exit_cause(),
            agent.incarnation,
        );
    });
    drop(app);
    tokio::time::timeout(Duration::from_secs(60), net.shutdown())
        .await
        .expect("the net shuts down")
        .unwrap();
}
