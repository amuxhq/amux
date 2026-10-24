//! The C ABI as the phone calls it, against a desk served in process: the
//! test holds the pointers, parses the JSON, watches the wake descriptor
//! and steps on its own thread the way the app's queue does.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::ffi::{CStr, CString, c_char};
use std::time::{Duration, Instant};

use app_runtime::values::{ActOutcome, ChatChanges, CommandOutcome, PageOutcome};
use node::link_log::LinkTransition;
use provider_fakes::script::{Ask, Question, Step, Tool, ToolClass};
use serde_json::{Value, json};
use testnet::{AgentDecl, FakeKind, HostDecl, Net, Topology};
use wire::{Empty, StartPairingRequest, start_pairing_request};

use super::*;

const PATIENCE: Duration = Duration::from_secs(30);

fn text_step(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

fn topology() -> Topology {
    let steps = vec![
        text_step("turn one"),
        Step::TurnEnd,
        Step::Ask(Ask::Permission(Tool {
            name: None,
            class: ToolClass::Consequential,
            input: None,
            outcome: Default::default(),
            wait_for: None,
        })),
        text_step("ran it"),
        Step::TurnEnd,
        Step::Ask(Ask::Question {
            questions: vec![Question {
                question: "Which suite?".into(),
                header: "Suite".into(),
                options: vec!["unit".into(), "full".into()],
                multi_select: false,
                other: false,
                secret: false,
            }],
        }),
        text_step("answered"),
        Step::TurnEnd,
    ];
    Topology::new()
        .host_decl(HostDecl {
            name: "desk".into(),
            lan: true,
            ..HostDecl::default()
        })
        .agent(
            AgentDecl::new("worker", "desk")
                .kind(FakeKind::ClaudeSdk)
                .steps(steps)
                .prompt("go"),
        )
}

/// Takes a returned string and frees it.
fn take(text: *mut c_char) -> Value {
    assert!(!text.is_null(), "the call failed");
    // SAFETY: the library returned it.
    let value = serde_json::from_str(unsafe { CStr::from_ptr(text) }.to_str().unwrap()).unwrap();
    // SAFETY: returned by the library, freed once.
    unsafe { amux_string_free(text) };
    value
}

fn c(text: &str) -> CString {
    CString::new(text).unwrap()
}

/// What the steps handed the app and it has not dealt with yet.
#[derive(Default)]
struct Seen {
    /// Fleet and chat numbers, in the order the steps named them.
    redraws: VecDeque<(u64, u64)>,
    /// The acts' answers, in the order they came.
    answers: VecDeque<(u64, Value)>,
    /// The profile list the latest step that carried one handed over.
    profiles: Option<Value>,
}

/// A runtime handed to the queue's thread, which uses it only until the
/// phone stops the queue.
struct Live(*const AmuxRuntime);

// SAFETY: the library's handles may be used from any thread.
unsafe impl Send for Live {}

/// The app's queue: a thread that watches the wake descriptor and steps
/// whenever it is readable, handing each step's JSON to the test.
struct Queue {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    stepped: std::sync::mpsc::Receiver<Value>,
}

impl Queue {
    fn start(runtime: *const AmuxRuntime) -> Queue {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = stop.clone();
        let (steps, stepped) = std::sync::mpsc::channel();
        let live = Live(runtime);
        let thread = std::thread::spawn(move || {
            let live = live;
            // SAFETY: the runtime is live until the queue stops.
            let fd = unsafe { amux_runtime_wake_fd(live.0) };
            while !stopping.load(Ordering::SeqCst) {
                let mut watched = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one live pollfd.
                if unsafe { libc::poll(&mut watched, 1, 50) } <= 0 {
                    continue;
                }
                // SAFETY: as above.
                let _ = steps.send(take(unsafe { amux_runtime_step(live.0) }));
            }
        });
        Queue {
            stop,
            thread: Some(thread),
            stepped,
        }
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The phone's side: the pointers, and the queue its loop steps on.
struct Phone {
    runtime: *mut AmuxRuntime,
    /// The one profile a fresh installation has, opened.
    profile: *mut AmuxProfile,
    fleet: u64,
    id: CString,
    seen: RefCell<Seen>,
    queue: Option<Queue>,
    /// The tickets of the acts started and not yet answered.
    tickets: RefCell<HashSet<u64>>,
    dir: tempfile::TempDir,
}

impl Phone {
    fn start() -> Phone {
        let dir = tempfile::tempdir().unwrap();
        let config = StartConfig {
            data_dir: dir.path().join("data"),
            device_name: "phone".into(),
            log_path: None,
            discovery_scope: String::new(),
            lan: true,
            lan_bind: None,
            relay_tcp: None,
            relay_quic: None,
            relay_root: None,
            tail: 50,
            telemetry: true,
        };
        let overrides = EdgeOverrides {
            lan_bind: ([127, 0, 0, 1], 0).into(),
            ..EdgeOverrides::default()
        };
        let runtime = Box::into_raw(start(&config, overrides).unwrap());
        let queue = Queue::start(runtime);
        // The list comes with a step once the registry's watch has caught up.
        let profiles = loop {
            let stepped = queue
                .stepped
                .recv_timeout(PATIENCE)
                .expect("a step hands over the profile list");
            if !stepped["profiles"].is_null() {
                break stepped["profiles"].clone();
            }
        };
        assert_eq!(profiles.as_array().unwrap().len(), 1, "{profiles}");
        let id = c(profiles[0]["id"].as_str().unwrap());
        let mut error = std::ptr::null_mut();
        // SAFETY: the runtime is live; the strings live for the call.
        let profile = unsafe { amux_profile_open(runtime, id.as_ptr(), &mut error) };
        assert!(!profile.is_null());
        // SAFETY: the profile is open.
        let fleet = unsafe { amux_profile_fleet_id(profile) };
        Phone {
            runtime,
            profile,
            fleet,
            id,
            seen: RefCell::default(),
            queue: Some(queue),
            tickets: RefCell::default(),
            dir,
        }
    }

    /// Keeps what the queue's steps handed back, waiting up to `within`
    /// for the first.
    fn pump(&self, within: Duration) {
        let queue = self.queue.as_ref().expect("stepping");
        let Ok(first) = queue.stepped.recv_timeout(within) else {
            return;
        };
        for stepped in std::iter::once(first).chain(queue.stepped.try_iter()) {
            self.keep(&stepped);
        }
    }

    fn keep(&self, stepped: &Value) {
        let mut seen = self.seen.borrow_mut();
        if !stepped["profiles"].is_null() {
            seen.profiles = Some(stepped["profiles"].clone());
        }
        for redraw in stepped["redraws"].as_array().unwrap() {
            let named = (
                redraw["fleet"].as_u64().unwrap(),
                redraw["chat"].as_u64().unwrap(),
            );
            seen.redraws.push_back(named);
        }
        for answer in stepped["answers"].as_array().unwrap() {
            let json = serde_json::from_str(answer["json"].as_str().unwrap()).unwrap();
            seen.answers
                .push_back((answer["ticket"].as_u64().unwrap(), json));
        }
    }

    /// Holds on to an act's ticket for [`Phone::result`].
    fn started(&self, ticket: u64) {
        assert_ne!(ticket, 0, "the act did not start");
        assert!(self.tickets.borrow_mut().insert(ticket), "a ticket twice");
    }

    /// The next answer to come of the acts started, whichever ends first.
    fn result(&self) -> Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let next = self.seen.borrow_mut().answers.pop_front();
            if let Some((ticket, answer)) = next {
                assert!(
                    self.tickets.borrow_mut().remove(&ticket),
                    "an act nobody started"
                );
                return answer;
            }
            assert!(Instant::now() < deadline, "the act never answered");
            self.pump(Duration::from_millis(100));
        }
    }

    /// The next redraw naming `fleet`, within `within`.
    fn redrawn(&self, fleet: u64, within: Duration) -> Option<u64> {
        let deadline = Instant::now() + within;
        loop {
            {
                let mut seen = self.seen.borrow_mut();
                if let Some(at) = seen.redraws.iter().position(|(of, _)| *of == fleet) {
                    return seen.redraws.remove(at).map(|(_, chat)| chat);
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            self.pump(Duration::from_millis(50));
        }
    }

    /// Waits for a redraw naming `chat` of the phone's fleet, and takes
    /// that one's changes.
    fn turn(&self, chat: u64, open: *const AmuxChat) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let redrawn = self.redrawn(self.fleet, left).expect("redrawn");
            if redrawn == chat {
                break;
            }
            if redrawn == 0 {
                // SAFETY: the runtime is live.
                take(unsafe { amux_fleet_take_changes(self.profile) });
            }
        }
        if chat == 0 {
            // SAFETY: the runtime is live.
            take(unsafe { amux_fleet_take_changes(self.profile) });
        } else {
            // SAFETY: the chat is open.
            let changes: ChatChanges =
                serde_json::from_value(take(unsafe { amux_session_take_changes(open) })).unwrap();
            let _ = changes;
        }
    }

    fn until(&self, chat: u64, open: *const AmuxChat, what: &str, check: impl Fn() -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while !check() {
            assert!(Instant::now() < deadline, "never saw {what}");
            self.turn(chat, open);
        }
    }
}

impl Drop for Phone {
    fn drop(&mut self) {
        drop(self.queue.take());
        // SAFETY: from `amux_profile_open` and `start`, not used again.
        unsafe {
            amux_profile_close(self.profile);
            amux_runtime_stop(self.runtime);
        }
    }
}

fn pin(net: &Net, tokio: &tokio::runtime::Runtime) -> (String, Vec<String>) {
    let desk = net.host("desk").unwrap();
    let started = tokio.block_on(async {
        net.front_door("desk")
            .await
            .unwrap()
            .start_pairing(StartPairingRequest {
                profile_id: desk.profile.as_bytes().to_vec(),
                method: Some(start_pairing_request::Method::Pin(Empty {})),
                ttl_seconds: None,
            })
            .await
            .unwrap()
            .into_inner()
    });
    let Some(wire::pairing_window::Secret::Pin(pin)) = started.secret else {
        panic!("the desk shows a PIN");
    };
    (pin, started.addrs)
}

/// Pairs the phone with the desk by the PIN it shows; returns the desk's
/// fingerprint.
fn pair_desk(phone: &Phone, net: &Net, tokio: &tokio::runtime::Runtime) -> String {
    let (pin, addrs) = pin(net, tokio);
    let desk = net.host("desk").unwrap();
    let request = c(&json!({"Pin": {
        "host_id": desk.host_id.as_bytes().to_vec(),
        "pin": pin,
        "addrs": addrs,
    }})
    .to_string());
    // SAFETY: the runtime is live; the strings live for the call.
    phone.started(unsafe { amux_profile_begin_pair(phone.profile, request.as_ptr()) });
    let pending = phone.result();
    assert_eq!(pending["Ok"]["name"], "desk", "{pending}");
    assert_eq!(pending["Ok"]["via"], "Direct", "{pending}");
    let fingerprint = pending["Ok"]["fingerprint"].as_str().unwrap().to_owned();
    assert_eq!(fingerprint.len(), 64, "{pending}");
    let token = c(&pending["Ok"]["token"].to_string());
    // SAFETY: the runtime is live; the strings live for the call.
    phone.started(unsafe { amux_profile_confirm_pair(phone.profile, token.as_ptr()) });
    let paired = phone.result();
    assert_eq!(paired["Ok"]["name"], "desk", "{paired}");
    fingerprint
}

fn says(chat: *const AmuxChat, wanted: &str) -> bool {
    // SAFETY: the chat is open.
    let keys = take(unsafe { amux_session_keys(chat) });
    let keys = c(&keys.to_string());
    // SAFETY: the chat is open; the strings live for the call.
    let rows = take(unsafe { amux_session_rows_for(chat, keys.as_ptr()) });
    rows.to_string().contains(wanted)
}

fn card(chat: *const AmuxChat) -> Value {
    // SAFETY: the chat is open.
    take(unsafe { amux_session_ask_card(chat) })
}

#[test]
#[cfg_attr(
    miri,
    ignore = "starts an installation: SQLite's C code, files and sockets, which Miri cannot run"
)]
fn the_phone_pairs_opens_a_chat_answers_its_asks_and_pages_through_the_c_abi() {
    let tokio = tokio::runtime::Runtime::new().unwrap();
    let net = tokio.block_on(Net::start(topology())).unwrap();
    let phone = Phone::start();
    let version = unsafe { CStr::from_ptr(amux_version()) }.to_str().unwrap();
    assert!(version.starts_with(node::version()));

    let fingerprint = pair_desk(&phone, &net, &tokio);
    let desk = net.host("desk").unwrap();

    // The profile that paired it is the one that trusts it.
    let host = c(&json!(desk.host_id.as_bytes().to_vec()).to_string());
    // SAFETY: the runtime is live; the strings live for the call.
    phone.started(unsafe { amux_runtime_trusting_profiles(phone.runtime, host.as_ptr()) });
    let trusting = phone.result();
    assert_eq!(
        trusting["Ok"],
        json!([phone.id.to_str().unwrap()]),
        "{trusting}"
    );

    // The roster names this phone and the desk by the same fingerprint.
    // SAFETY: the runtime is live.
    phone.started(unsafe { amux_profile_roster(phone.profile) });
    let roster = phone.result();
    assert_eq!(roster["Ok"]["identity"]["name"], "phone", "{roster}");
    assert_eq!(roster["Ok"]["peers"][0]["name"], "desk", "{roster}");
    assert_eq!(
        roster["Ok"]["peers"][0]["fingerprint"],
        fingerprint.as_str()
    );

    // Never signed in: the account says so and lends no bearer.
    // SAFETY: the runtime is live.
    phone.started(unsafe { amux_profile_account(phone.profile) });
    let account = phone.result();
    assert_eq!(account["Ok"]["binding"], "Unbound", "{account}");
    // SAFETY: the runtime is live.
    phone.started(unsafe { amux_runtime_access_token(phone.runtime, phone.id.as_ptr()) });
    assert!(phone.result()["Err"].is_string());
    // Nor can it ask what an account buys.
    // SAFETY: the runtime is live.
    phone.started(unsafe { amux_runtime_refresh_entitlement(phone.runtime, phone.id.as_ptr()) });
    assert!(phone.result()["Err"].is_string());

    // What the phone's own browser found is handed over whole; one this
    // runtime cannot read is left out.
    let found = c(&json!([{
        "host_id": [1, 2, 3],
        "name": "unreadable",
        "version": 1,
        "addrs": ["127.0.0.1:1"],
        "scope": "",
    }])
    .to_string());
    // SAFETY: the runtime is live; the string lives for the call.
    unsafe { amux_runtime_discovered(phone.runtime, found.as_ptr()) };

    // The desk's agent reaches the fleet from this device's own rows.
    let worker = net.agent("worker").unwrap();
    let agent = json!({
        "host": worker.host_id.as_bytes().to_vec(),
        "agent": worker.id.as_bytes().to_vec(),
    });
    let agent_c = c(&agent.to_string());
    phone.until(0, std::ptr::null(), "the desk's agent", || {
        // SAFETY: the runtime is live.
        let view = take(unsafe { amux_fleet_view(phone.profile) });
        view["sections"][0]["rows"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1)
    });
    // SAFETY: the runtime is live; the string lives for the call.
    let card_json = take(unsafe { amux_fleet_card(phone.profile, agent_c.as_ptr()) });
    assert_eq!(card_json["name"], "worker");
    assert_eq!(card_json["host"], "desk");
    // SAFETY: the runtime is live.
    let hosts = take(unsafe { amux_fleet_hosts(phone.profile) });
    assert_eq!(hosts[0]["local"], true, "{hosts}");
    assert!(hosts.to_string().contains("\"desk\""));
    let desk_id = c(&json!(desk.host_id.as_bytes().to_vec()).to_string());
    // SAFETY: the runtime is live; the strings live for the call.
    phone.started(unsafe {
        amux_profile_directories(phone.profile, desk_id.as_ptr(), std::ptr::null(), 20)
    });
    // Answered either way; the desk may not list its repositories.
    let directories = phone.result();
    assert!(
        directories["Ok"]["recent"].is_array() || directories["Err"].is_string(),
        "{directories}"
    );

    // Renamed from outside its chat, the card follows.
    let rename = c(&json!({"Rename": "builder"}).to_string());
    // SAFETY: the runtime is live; the strings live for the call.
    phone.started(unsafe {
        amux_profile_agent_act(phone.profile, agent_c.as_ptr(), rename.as_ptr())
    });
    assert!(phone.result().get("Ok").is_some());
    phone.until(0, std::ptr::null(), "the new name", || {
        // SAFETY: the runtime is live; the string lives for the call.
        take(unsafe { amux_fleet_card(phone.profile, agent_c.as_ptr()) })["name"] == "builder"
    });
    let rename = c(&json!({"Rename": "worker"}).to_string());
    // SAFETY: the runtime is live; the strings live for the call.
    phone.started(unsafe {
        amux_profile_agent_act(phone.profile, agent_c.as_ptr(), rename.as_ptr())
    });
    assert!(phone.result().get("Ok").is_some());
    phone.until(0, std::ptr::null(), "the name back", || {
        // SAFETY: the runtime is live; the string lives for the call.
        take(unsafe { amux_fleet_card(phone.profile, agent_c.as_ptr()) })["name"] == "worker"
    });

    // Called from a thread outside the pool, as the app's main thread is.
    // SAFETY: the runtime is live.
    unsafe { amux_runtime_list_sources(phone.runtime, std::ptr::null()) };
    // SAFETY: the runtime is live and not stopped until the phone drops.
    let embedded = unsafe { &*phone.runtime }.embedded().clone();
    let id: ProfileId = phone.id.to_str().unwrap().parse().unwrap();
    assert_eq!(embedded.source_policy(id).unwrap(), SourcePolicy::OnDemand);
    // SAFETY: the runtime is live.
    assert!(!unsafe { amux_runtime_source_policy_listed(phone.runtime, phone.id.as_ptr()) });
    // SAFETY: the runtime is live.
    unsafe { amux_runtime_list_sources(phone.runtime, phone.id.as_ptr()) };
    assert_eq!(embedded.source_policy(id).unwrap(), SourcePolicy::Listed);

    // A chat: rows by key, a prompt, a permission answered by position.
    let mut error = std::ptr::null_mut();
    // SAFETY: the runtime is live; the string lives for the call.
    let chat = unsafe { amux_session_open(phone.profile, agent_c.as_ptr(), 0, &mut error) };
    assert!(!chat.is_null());
    // SAFETY: the chat is open.
    let id = unsafe { amux_session_id(chat) };
    // The rows can be on screen before the chat is caught up: the switch to
    // on-demand above may have closed the replica's source, and the one
    // reopened for listed agents serves the held rows as detached until its
    // catch-up lands.
    phone.until(id, chat, "the first turn, caught up", || {
        // SAFETY: the chat is open.
        says(chat, "turn one") && take(unsafe { amux_session_frame(chat) })["caught_up"] == true
    });
    // SAFETY: the chat is open.
    let frame = take(unsafe { amux_session_frame(chat) });
    assert_eq!(frame["name"], "worker", "{frame}");
    assert_eq!(frame["caught_up"], true, "{frame}");
    // SAFETY: the chat is open.
    let overview = take(unsafe { amux_session_overview(chat) });
    assert!(overview.is_object(), "{overview}");
    // SAFETY: the chat is open.
    let keys = take(unsafe { amux_session_keys(chat) });
    let newest = c(keys.as_array().unwrap().last().unwrap().as_str().unwrap());

    let draft = c(&json!({"parts": [{"Text": "please run it"}]}).to_string());
    // SAFETY: the chat is open; the string lives for the call.
    phone.started(unsafe { amux_session_send(chat, draft.as_ptr()) });
    let sent = phone.result();
    assert!(sent["Ok"]["input_id"].is_array(), "{sent}");
    phone.until(id, chat, "the permission ask", || !card(chat).is_null());
    let ask = card(chat);
    let allow = ask["choices"]
        .as_array()
        .unwrap()
        .iter()
        .position(|choice| choice["outcome"] == "AllowOnce")
        .expect("allow once is offered");
    let ask_key = c(ask["key"].as_str().unwrap());
    // SAFETY: the chat is open; the strings live for the call.
    phone.started(unsafe {
        amux_session_answer(chat, ask_key.as_ptr(), allow as u32, std::ptr::null())
    });
    let acted: ActOutcome = outcome(phone.result());
    assert_eq!(acted, ActOutcome::Done);
    phone.until(id, chat, "the call to run", || {
        card(chat).is_null() && says(chat, "ran it")
    });
    // The new rows arrived above the newest the phone held.
    // SAFETY: the chat is open; the string lives for the call.
    let above = take(unsafe { amux_session_new_keys_above(chat, newest.as_ptr()) });
    assert!(
        above.as_array().is_some_and(|keys| !keys.is_empty()),
        "{above}"
    );
    let unknown = c("no such key");
    // SAFETY: the chat is open; the string lives for the call.
    assert!(take(unsafe { amux_session_new_keys_above(chat, unknown.as_ptr()) }).is_null());

    // A question, answered with picks.
    let draft = c(&json!({"parts": [{"Text": "ask me"}]}).to_string());
    // SAFETY: the chat is open; the string lives for the call.
    phone.started(unsafe { amux_session_send(chat, draft.as_ptr()) });
    phone.result();
    phone.until(id, chat, "the question", || !card(chat).is_null());
    let ask = card(chat);
    assert!(ask["body"]["Question"].is_array(), "{ask}");
    let ask_key = c(ask["key"].as_str().unwrap());
    let picks = c(&json!([{"pick": {"Options": [1]}}]).to_string());
    // SAFETY: the chat is open; the strings live for the call.
    phone.started(unsafe { amux_session_answer_questions(chat, ask_key.as_ptr(), picks.as_ptr()) });
    assert_eq!(outcome::<ActOutcome>(phone.result()), ActOutcome::Done);
    phone.until(id, chat, "the answer", || says(chat, "answered"));

    // A draft naming one of the client's own commands answers with the
    // command's outcome itself, which is what the phone decodes.
    let draft = c(&json!({"parts": [{"Text": "/rename"}]}).to_string());
    // SAFETY: the chat is open; the string lives for the call.
    phone.started(unsafe { amux_session_command(chat, draft.as_ptr()) });
    assert_eq!(
        outcome::<CommandOutcome>(phone.result()),
        CommandOutcome::AskName
    );
    let not_a_draft = c("[]");
    // SAFETY: the chat is open; the string lives for the call.
    phone.started(unsafe { amux_session_command(chat, not_a_draft.as_ptr()) });
    assert!(matches!(
        outcome::<CommandOutcome>(phone.result()),
        CommandOutcome::Acted(ActOutcome::Failed(_))
    ));
    // SAFETY: the chat is open, and closed once.
    unsafe { amux_session_close(chat) };

    // A live agent's session stays with the fleet once its chat closes; an
    // exited one's goes, and a one-row chat on it pages its history in
    // below the oldest it holds.
    let stop = c(&json!("Stop").to_string());
    // SAFETY: the runtime is live; the strings live for the call.
    phone
        .started(unsafe { amux_profile_agent_act(phone.profile, agent_c.as_ptr(), stop.as_ptr()) });
    assert!(phone.result().get("Ok").is_some());
    phone.until(0, std::ptr::null(), "the agent exited", || {
        // SAFETY: the runtime is live; the string lives for the call.
        let card = take(unsafe { amux_fleet_card(phone.profile, agent_c.as_ptr()) });
        card.to_string().contains("Exited")
    });
    // SAFETY: the runtime is live; the string lives for the call.
    let chat = unsafe { amux_session_open(phone.profile, agent_c.as_ptr(), 1, &mut error) };
    assert!(!chat.is_null());
    // SAFETY: the chat is open.
    let held = take(unsafe { amux_session_keys(chat) });
    let oldest = c(held[0].as_str().unwrap());
    // SAFETY: the chat is open; the string lives for the call.
    phone.started(unsafe { amux_session_page_older(chat, 2) });
    assert_eq!(
        outcome::<PageOutcome>(phone.result()),
        PageOutcome::Arrived(2)
    );
    // SAFETY: the chat is open; the string lives for the call.
    let below = take(unsafe { amux_session_new_keys_below(chat, oldest.as_ptr()) });
    assert_eq!(below.as_array().map(Vec::len), Some(2), "{below}");

    // A dump carries the fleet and the open chat.
    let reason = c("asked for in a test");
    // SAFETY: the runtime is live; the string lives for the call.
    phone.started(unsafe { amux_profile_dump(phone.profile, reason.as_ptr()) });
    let dumped = phone.result();
    let bundle = std::path::PathBuf::from(dumped["Ok"].as_str().expect("a bundle"));
    assert!(bundle.join("client/fleet/state.txt").is_file());
    assert!(bundle.join("client/sessions").is_dir());

    // SAFETY: the chat is open, and closed once.
    unsafe { amux_session_close(chat) };
    assert_eq!(CAUGHT.with(std::cell::Cell::get), 0, "a call panicked");
    drop(phone);
    tokio.block_on(net.shutdown()).unwrap();
}

#[test]
#[cfg_attr(
    miri,
    ignore = "starts an installation: SQLite's C code, files and sockets, which Miri cannot run"
)]
fn no_redraw_names_a_closed_profile_or_chat_while_its_acts_are_in_flight() {
    let tokio = tokio::runtime::Runtime::new().unwrap();
    let net = tokio.block_on(Net::start(topology())).unwrap();
    let phone = Phone::start();
    pair_desk(&phone, &net, &tokio);
    let worker = net.agent("worker").unwrap();
    let agent = c(&json!({
        "host": worker.host_id.as_bytes().to_vec(),
        "agent": worker.id.as_bytes().to_vec(),
    })
    .to_string());
    phone.until(0, std::ptr::null(), "the desk's agent", || {
        // SAFETY: the runtime is live.
        let view = take(unsafe { amux_fleet_view(phone.profile) });
        view["sections"][0]["rows"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1)
    });

    // A second handle on the same profile, the one to close, redrawn under
    // a number of its own; the phone's first handle watches the same moves.
    let mut error = std::ptr::null_mut();
    // SAFETY: the runtime is live; the strings live for the call.
    let closing = unsafe { amux_profile_open(phone.runtime, phone.id.as_ptr(), &mut error) };
    assert!(!closing.is_null());
    // SAFETY: the profile is open.
    let closing_fleet = unsafe { amux_profile_fleet_id(closing) };
    assert_ne!(closing_fleet, phone.fleet);
    // SAFETY: the profile is open; the string lives for the call.
    let chat = unsafe { amux_session_open(closing, agent.as_ptr(), 0, &mut error) };
    assert!(!chat.is_null());
    // SAFETY: as above.
    let watching = unsafe { amux_session_open(phone.profile, agent.as_ptr(), 0, &mut error) };
    assert!(!watching.is_null());

    // With the agent frozen, a send waits on it for seconds, holding the
    // chat past its close.
    net.freeze("worker").unwrap();
    let draft = c(&json!({"parts": [{"Text": "are you there"}]}).to_string());
    // SAFETY: the chat is open; the string lives for the call.
    phone.started(unsafe { amux_session_send(chat, draft.as_ptr()) });
    settle(&phone, closing_fleet, closing, chat);
    // SAFETY: from `amux_session_open`, not used again.
    unsafe { amux_session_close(chat) };
    rename(&phone, &agent, watching, "renamed");
    // The fleet moved too, and may be redrawn; the chat may not.
    while let Some(redrawn) = phone.redrawn(closing_fleet, Duration::from_secs(1)) {
        assert_eq!(redrawn, 0, "a closed chat was redrawn");
        // SAFETY: the profile is open.
        take(unsafe { amux_fleet_take_changes(closing) });
    }

    // And a dump waits on it too, holding the fleet past its close.
    let reason = c("closing under an act");
    // SAFETY: the profile is open; the string lives for the call.
    phone.started(unsafe { amux_profile_dump(closing, reason.as_ptr()) });
    settle(&phone, closing_fleet, closing, std::ptr::null());
    // SAFETY: from `amux_profile_open`, not used again.
    unsafe { amux_profile_close(closing) };
    assert!(
        phone.seen.borrow().answers.is_empty(),
        "the acts are still in flight"
    );
    rename(&phone, &agent, watching, "worker");
    assert_eq!(
        phone.redrawn(closing_fleet, Duration::from_secs(1)),
        None,
        "a closed profile was redrawn"
    );

    // The acts are still answered once the agent answers.
    net.thaw("worker").unwrap();
    phone.result();
    phone.result();
    // SAFETY: the chat is open, and closed once.
    unsafe { amux_session_close(watching) };
    assert_eq!(CAUGHT.with(std::cell::Cell::get), 0, "a call panicked");
    drop(phone);
    tokio.block_on(net.shutdown()).unwrap();
}

/// Takes what a fleet's redraws name until none comes for half a second,
/// so its next change names it again.
fn settle(phone: &Phone, fleet: u64, profile: *const AmuxProfile, chat: *const AmuxChat) {
    while let Some(moved) = phone.redrawn(fleet, Duration::from_millis(500)) {
        // SAFETY: both are open, or the chat is null and not named.
        unsafe {
            if moved == 0 {
                take(amux_fleet_take_changes(profile));
            } else {
                take(amux_session_take_changes(chat));
            }
        }
    }
}

/// Renames the agent from the phone's own handle and waits until its fleet
/// and its open chat both show the name.
fn rename(phone: &Phone, agent: &CStr, chat: *const AmuxChat, name: &str) {
    let act = c(&json!({ "Rename": name }).to_string());
    // SAFETY: the runtime is live; the strings live for the call.
    phone.started(unsafe { amux_profile_agent_act(phone.profile, agent.as_ptr(), act.as_ptr()) });
    assert!(phone.result().get("Ok").is_some());
    // SAFETY: the chat is open.
    let id = unsafe { amux_session_id(chat) };
    phone.until(id, chat, "the chat renamed", || {
        // SAFETY: the chat is open.
        take(unsafe { amux_session_frame(chat) })["name"] == name
    });
    phone.until(0, std::ptr::null(), "the fleet renamed", || {
        // SAFETY: the runtime is live; the string lives for the call.
        take(unsafe { amux_fleet_card(phone.profile, agent.as_ptr()) })["name"] == name
    });
}

impl Phone {
    /// The profile list the steps handed over since it was last taken,
    /// waiting for one.
    fn profiles_after_a_wake(&self) -> Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(profiles) = self.seen.borrow_mut().profiles.take() {
                return profiles;
            }
            assert!(Instant::now() < deadline, "the profile list never moved");
            self.pump(Duration::from_millis(100));
        }
    }
}

#[test]
#[cfg_attr(
    miri,
    ignore = "starts an installation: SQLite's C code, files and sockets, which Miri cannot run"
)]
fn the_profile_registry_through_the_c_abi() {
    let phone = Phone::start();
    // SAFETY: the runtime is live.
    phone.started(unsafe { amux_runtime_create_profile(phone.runtime) });
    let created = phone.result();
    assert!(
        phone.seen.borrow().profiles.is_some(),
        "the list the act left comes no later than its answer"
    );
    let other = c(created["Ok"]["id"].as_str().unwrap());
    assert_eq!(created["Ok"]["account"]["binding"], "Unbound", "{created}");
    assert_eq!(created["Ok"]["subject"], "", "{created}");
    let profiles = phone.profiles_after_a_wake();
    assert_eq!(profiles.as_array().unwrap().len(), 2, "{profiles}");
    assert_eq!(
        profiles[0]["id"],
        phone.id.to_str().unwrap(),
        "oldest first"
    );

    // Each profile has its own identity and its own source policy.
    let mut error = std::ptr::null_mut();
    // SAFETY: the runtime is live; the strings live for the call.
    let opened = unsafe { amux_profile_open(phone.runtime, other.as_ptr(), &mut error) };
    assert!(!opened.is_null());
    // SAFETY: both profiles are open.
    let (mine, theirs) = unsafe {
        (
            take(amux_fleet_hosts(phone.profile)),
            take(amux_fleet_hosts(opened)),
        )
    };
    assert_ne!(mine[0]["host_id"], theirs[0]["host_id"], "{mine} {theirs}");
    // SAFETY: the runtime is live; the string lives for the call.
    unsafe { amux_runtime_list_sources(phone.runtime, phone.id.as_ptr()) };
    // SAFETY: as above.
    assert!(!unsafe { amux_runtime_source_policy_listed(phone.runtime, other.as_ptr()) });
    // SAFETY: as above.
    assert!(unsafe { amux_runtime_source_policy_listed(phone.runtime, phone.id.as_ptr()) });
    // SAFETY: opened above, not used again.
    unsafe { amux_profile_close(opened) };

    // Only a driving build offers pairing from the phone.
    // SAFETY: the runtime is live; the string lives for the call.
    phone.started(unsafe { amux_runtime_offer_pairing(phone.runtime, other.as_ptr()) });
    let offered = phone.result();
    assert_eq!(
        offered["Ok"].is_string(),
        cfg!(feature = "debug-tools"),
        "{offered}"
    );

    // SAFETY: the runtime is live; the string lives for the call.
    phone.started(unsafe { amux_runtime_delete_profile(phone.runtime, other.as_ptr()) });
    let deleted = phone.result();
    assert!(deleted["Ok"].is_null(), "{deleted}");
    let profiles = phone.profiles_after_a_wake();
    assert_eq!(profiles.as_array().unwrap().len(), 1, "{profiles}");
    // The installation keeps at least one.
    // SAFETY: as above.
    phone.started(unsafe { amux_runtime_delete_profile(phone.runtime, phone.id.as_ptr()) });
    assert!(phone.result()["Err"].is_string());
    // Nobody trusts a host nobody paired.
    let host = c(&json!(vec![7u8; 16]).to_string());
    // SAFETY: as above.
    phone.started(unsafe { amux_runtime_trusting_profiles(phone.runtime, host.as_ptr()) });
    let trusting = phone.result();
    assert_eq!(trusting["Ok"], json!([]), "{trusting}");
    // A profile id that does not parse is refused.
    let nonsense = c("not a profile");
    // SAFETY: as above.
    phone.started(unsafe { amux_runtime_sign_out(phone.runtime, nonsense.as_ptr()) });
    assert!(phone.result()["Err"].is_string());
}

#[test]
#[cfg_attr(
    miri,
    ignore = "starts an installation: SQLite's C code, files and sockets, which Miri cannot run"
)]
fn a_suspending_input_checkpoints_every_profile_before_it_returns() {
    let phone = Phone::start();
    let ring = phone
        .dir
        .path()
        .join("data")
        .join(node::PROFILES)
        .join(phone.id.to_str().unwrap())
        .join(node::CORE_RING);
    // Each ring segment is headed by the checkpoint it starts at; the
    // start wrote the first.
    let checkpoints = |ring: &std::path::Path| {
        std::fs::read_dir(ring)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.file_name().to_string_lossy().ends_with(".checkpoint"))
                    .count()
            })
            .unwrap_or(0)
    };
    let before = checkpoints(&ring);
    let mut error = std::ptr::null_mut();
    // SAFETY: the runtime is live; the strings live for the call.
    assert!(unsafe { amux_runtime_feed(phone.runtime, c"\"Suspending\"".as_ptr(), &mut error) });
    assert!(
        checkpoints(&ring) > before,
        "no new segment headed by a checkpoint in {}",
        ring.display()
    );
    // SAFETY: as above.
    assert!(unsafe { amux_runtime_feed(phone.runtime, c"\"Foreground\"".as_ptr(), &mut error) });
    // SAFETY: as above.
    assert!(!unsafe { amux_runtime_feed(phone.runtime, c"\"Asleep\"".as_ptr(), &mut error) });
    assert!(!error.is_null());
    // SAFETY: returned by the library, freed once.
    unsafe { amux_string_free(error) };
}

#[test]
#[cfg_attr(
    miri,
    ignore = "starts an installation: SQLite's C code, files and sockets, which Miri cannot run"
)]
fn network_changed_rebinds_the_link_socket_and_tells_every_path_machine() {
    let tokio = tokio::runtime::Runtime::new().unwrap();
    let net = tokio.block_on(Net::start(topology())).unwrap();
    let phone = Phone::start();
    pair_desk(&phone, &net, &tokio);
    let profile: ProfileId = phone.id.to_str().unwrap().parse().unwrap();
    // SAFETY: the runtime is live until the phone drops.
    let embedded = unsafe { &*phone.runtime }.embedded();
    let host = node::HostId::from_slice(&embedded.host_id(profile).unwrap()).unwrap();
    assert_eq!(testnet::link_log::rebinds(host), vec![]);
    // The pairing's link to the desk is up at the phone.
    let desk = net.host("desk").unwrap().host_id;
    let deadline = Instant::now() + PATIENCE;
    while !testnet::link_log::captured(host)
        .iter()
        .any(|event| event.peer == desk && event.event == LinkTransition::Up)
    {
        assert!(
            Instant::now() < deadline,
            "the phone's link to the desk never came up"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut error = std::ptr::null_mut();
    // SAFETY: the runtime is live; the strings live for the call.
    let fed =
        unsafe { amux_runtime_feed(phone.runtime, c"\"NetworkChanged\"".as_ptr(), &mut error) };
    assert!(
        fed,
        "{:?}",
        (!error.is_null()).then(|| unsafe { CStr::from_ptr(error) })
    );

    // The feed returns once every profile was told, and the link log says
    // what it did: the phone's own socket bound afresh, and the desk's
    // path machine told.
    let rebinds = testnet::link_log::rebinds(host);
    println!("the phone's link log after the change: {rebinds:?}");
    let [rebind] = rebinds[..] else {
        panic!("one rebind line, not {rebinds:?}");
    };
    assert!(rebind.rebound, "{rebind:?}");
    assert!(rebind.machines_told >= 1, "{rebind:?}");
}

#[test]
#[cfg_attr(
    miri,
    ignore = "starts an installation: SQLite's C code, files and sockets, which Miri cannot run"
)]
fn malformed_calls_are_refused_rather_than_crashing() {
    let phone = Phone::start();
    let mut error = std::ptr::null_mut();
    let not_json = c("not json");
    // SAFETY: the runtime is live; the string lives for the call.
    let chat = unsafe { amux_session_open(phone.profile, not_json.as_ptr(), 0, &mut error) };
    assert!(chat.is_null());
    assert!(!error.is_null());
    // SAFETY: returned by the library, freed once.
    unsafe { amux_string_free(error) };
    let nobody = c(&json!({"host": [1], "agent": [2]}).to_string());
    let mut error = std::ptr::null_mut();
    // SAFETY: the runtime is live; the string lives for the call.
    let chat = unsafe { amux_session_open(phone.profile, nobody.as_ptr(), 0, &mut error) };
    assert!(chat.is_null());
    // SAFETY: returned by the library.
    let reason = unsafe { CStr::from_ptr(error) }
        .to_str()
        .unwrap()
        .to_owned();
    assert!(reason.contains("no agent"), "{reason}");
    // SAFETY: returned by the library, freed once.
    unsafe { amux_string_free(error) };
    // SAFETY: the runtime is live; the strings live for the call.
    phone.started(unsafe {
        amux_profile_host_catalogue(phone.profile, not_json.as_ptr(), c"\"Claude\"".as_ptr())
    });
    assert!(phone.result()["Err"].is_string());
    // SAFETY: the runtime is live; the string lives for the call.
    phone.started(unsafe { amux_profile_begin_pair(phone.profile, not_json.as_ptr()) });
    assert!(phone.result()["Err"].is_string());
    let config = c("{}");
    let mut error = std::ptr::null_mut();
    // SAFETY: the strings live for the call.
    let runtime = unsafe { amux_runtime_start(config.as_ptr(), &mut error) };
    assert!(runtime.is_null());
    // SAFETY: returned by the library, freed once.
    unsafe { amux_string_free(error) };
}

fn outcome<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

/// An outbox on its own, counting its rings.
fn outbox() -> (Arc<Outbox>, Arc<AtomicU64>) {
    let rang = Arc::new(AtomicU64::new(0));
    let ringing = rang.clone();
    let outbox = Arc::new(Outbox {
        tickets: AtomicU64::new(0),
        fleets: AtomicU64::new(0),
        waiting: Mutex::new(Stepped::default()),
        ring: Box::new(move || {
            ringing.fetch_add(1, Ordering::SeqCst);
        }),
    });
    (outbox, rang)
}

#[test]
#[cfg_attr(
    miri,
    ignore = "drives a tokio runtime, whose I/O driver Miri cannot run on every host"
)]
fn an_act_dropped_by_the_transports_shutdown_answers_that_the_runtime_stopped() {
    let transport = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (outbox, rang) = outbox();
    let ticket = outbox.act(transport.handle(), std::future::pending::<()>());
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        outbox.take().answers.is_empty(),
        "nothing answers while it runs"
    );

    transport.shutdown_timeout(Duration::from_secs(5));

    let answers = outbox.take().answers;
    assert_eq!(answers.len(), 1, "answered once");
    assert_eq!(answers[0].ticket, ticket);
    assert_eq!(answers[0].json, r#"{"Err":"the runtime has stopped"}"#);
    assert_eq!(rang.load(Ordering::SeqCst), 1, "and rang for its step");
}

#[test]
#[cfg_attr(
    miri,
    ignore = "drives a tokio runtime, whose I/O driver Miri cannot run on every host"
)]
fn an_act_that_finishes_answers_once_with_its_result() {
    let transport = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (outbox, rang) = outbox();
    let ticket = outbox.act(transport.handle(), async { 7u8 });
    let deadline = Instant::now() + Duration::from_secs(5);
    while rang.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "the act never answered");
        std::thread::sleep(Duration::from_millis(10));
    }
    transport.shutdown_timeout(Duration::from_secs(5));
    let answers = outbox.take().answers;
    assert_eq!(answers.len(), 1, "the drop does not answer again");
    assert_eq!((answers[0].ticket, answers[0].json.as_str()), (ticket, "7"));
}

/// What the loop's shape promises: the cores step on the app's queue and
/// nowhere else, and nothing calls into the app.
mod confinement {
    use super::*;

    #[test]
    #[cfg_attr(
        miri,
        ignore = "starts an installation: SQLite's C code, files and sockets, which Miri cannot run"
    )]
    fn the_cores_step_only_on_the_queue_that_steps_and_nothing_calls_the_app() {
        // Nothing in the library can call into the app: its header declares
        // no function type, and the loop's three calls are there.
        let header = include_str!(concat!(env!("OUT_DIR"), "/amux_app.h"));
        assert!(!header.contains("(*"), "a function type in the header");
        for call in [
            "amux_runtime_wake_fd(",
            "amux_runtime_step(",
            "amux_runtime_feed(",
        ] {
            assert!(header.contains(call), "{call} is missing");
        }

        // The app's queue is this thread, and nothing else steps.
        let dir = tempfile::tempdir().unwrap();
        let config = c(&json!({
            "data_dir": dir.path().join("data"),
            "device_name": "phone",
            "log_path": null,
            "discovery_scope": "",
            "lan": false,
            "lan_bind": null,
            "relay_tcp": null,
            "relay_quic": null,
            "relay_root": null,
            "tail": 50,
            "telemetry": false,
        })
        .to_string());
        let mut error = std::ptr::null_mut();
        // SAFETY: the strings live for the call.
        let runtime = unsafe { amux_runtime_start(config.as_ptr(), &mut error) };
        // A sandbox that refuses every socket fails here, even without
        // the LAN: the edge still binds a socket to dial the relay from.
        assert!(!runtime.is_null(), "{:?}", {
            // SAFETY: set by the failed start.
            unsafe { CStr::from_ptr(error) }
        });
        let queue = std::thread::current().id();
        // SAFETY: the runtime is live for the test.
        let shared = unsafe { live_runtime(runtime) }.unwrap().shared.clone();
        // SAFETY: the runtime is live.
        let fd = unsafe { amux_runtime_wake_fd(runtime) };
        let readable = |within: i32| {
            let mut watched = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one live pollfd.
            unsafe {
                libc::poll(&mut watched, 1, within) == 1 && watched.revents & libc::POLLIN != 0
            }
        };
        // Stepped until the profile list came and nothing waits, the
        // descriptor reads quiet.
        let deadline = Instant::now() + PATIENCE;
        let mut profiles = Value::Null;
        loop {
            // SAFETY: the runtime is live.
            let stepped = take(unsafe { amux_runtime_step(runtime) });
            if !stepped["profiles"].is_null() {
                profiles = stepped["profiles"].clone();
            }
            let quiet = !readable(200);
            if quiet && !profiles.is_null() {
                break;
            }
            assert!(Instant::now() < deadline, "the runtime never went quiet");
        }
        let profile = profiles[0]["id"]
            .as_str()
            .unwrap()
            .parse::<ProfileId>()
            .unwrap();

        // Another thread posts work onto the profile's core and waits on it.
        let (stepped_on, stepped) = std::sync::mpsc::channel();
        let embedded = shared.embedded().clone();
        let handle = shared.handle().clone();
        let poster = std::thread::spawn(move || {
            handle.spawn(async move {
                let ran_on = embedded
                    .between_steps(profile, || std::thread::current().id())
                    .await;
                let _ = stepped_on.send(ran_on);
            });
        });
        // With nobody stepping, the core does nothing, and the descriptor says
        // something waits for it.
        assert!(readable(5_000), "nothing woke");
        std::thread::sleep(Duration::from_millis(300));
        assert!(stepped.try_recv().is_err(), "the core stepped by itself");
        // A step on this thread drains it, and the work ran here.
        let deadline = Instant::now() + PATIENCE;
        let ran_on = loop {
            // SAFETY: the runtime is live.
            take(unsafe { amux_runtime_step(runtime) });
            if let Ok(ran_on) = stepped.recv_timeout(Duration::from_millis(100)) {
                break ran_on;
            }
            assert!(Instant::now() < deadline, "the step never drained it");
        };
        assert_eq!(ran_on.unwrap(), queue);
        poster.join().unwrap();
        drop(shared);
        // SAFETY: from `amux_runtime_start`, not used again.
        unsafe { amux_runtime_stop(runtime) };
    }
}

/// A stop is a clean shutdown every time, not only when the profile-list
/// watch happens to have let go of the installation first: the generation
/// file says so, and the next start would otherwise read a crash.
#[test]
#[cfg_attr(
    miri,
    ignore = "starts an installation: SQLite's C code, files and sockets, which Miri cannot run"
)]
fn every_stop_leaves_the_installation_marked_clean() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let config = c(&json!({
        "data_dir": data_dir,
        "device_name": "phone",
        "log_path": null,
        "discovery_scope": "",
        "lan": false,
        "lan_bind": null,
        "relay_tcp": null,
        "relay_quic": null,
        "relay_root": null,
        "tail": 50,
        "telemetry": false,
    })
    .to_string());
    for run in 1..=10 {
        let mut error = std::ptr::null_mut();
        // SAFETY: the strings live for the call.
        let runtime = unsafe { amux_runtime_start(config.as_ptr(), &mut error) };
        assert!(!runtime.is_null(), "run {run}: {:?}", {
            // SAFETY: set by the failed start.
            unsafe { CStr::from_ptr(error) }
        });
        // SAFETY: from `amux_runtime_start`, not used again.
        unsafe { amux_runtime_stop(runtime) };
        let generation: Value =
            serde_json::from_slice(&std::fs::read(data_dir.join("generation")).unwrap()).unwrap();
        assert_eq!(
            generation["clean"],
            json!(true),
            "run {run} left {generation}"
        );
    }
}

#[test]
fn null_handles_read_as_nothing_and_null_results_free_as_nothing() {
    // SAFETY: null chats and fleets read as nothing.
    assert!(unsafe { amux_session_keys(std::ptr::null()) }.is_null());
    // SAFETY: as above.
    assert!(unsafe { amux_fleet_view(std::ptr::null()) }.is_null());
    // SAFETY: a null chat or fleet ignores what it is told.
    unsafe { amux_session_toggle_run(std::ptr::null(), c"k".as_ptr()) };
    // SAFETY: as above.
    unsafe { amux_fleet_toggle_family(std::ptr::null(), c"[1]".as_ptr()) };
    // SAFETY: as above.
    let blob = unsafe { amux_session_blob(std::ptr::null(), c"[1]".as_ptr()) };
    assert!(blob.data.is_null() && blob.len == 0);
    // SAFETY: a null profile is ignored.
    unsafe { amux_profile_set_foreground(std::ptr::null(), false) };
    // SAFETY: null results free as nothing.
    unsafe { amux_bytes_free(blob) };
    // SAFETY: as above.
    unsafe { amux_string_free(std::ptr::null_mut()) };
}

#[test]
fn bytes_handed_out_read_whole_and_free_once() {
    let bytes = AmuxBytes::leak(b"an attachment");
    // SAFETY: the library just handed out these bytes.
    let read = unsafe { std::slice::from_raw_parts(bytes.data, bytes.len) };
    assert_eq!(read, b"an attachment");
    // SAFETY: freed once.
    unsafe { amux_bytes_free(bytes) };
    let none = AmuxBytes::leak(&[]);
    assert_eq!(none.len, 0);
    // SAFETY: an empty slice frees like any other.
    unsafe { amux_bytes_free(none) };
}

#[test]
fn the_version_is_one_string_for_the_life_of_the_library() {
    let first = amux_version();
    // SAFETY: the library's static string.
    let version = unsafe { CStr::from_ptr(first) }.to_str().unwrap();
    assert!(!version.is_empty());
    assert_eq!(amux_version(), first, "the same pointer every time");
}

#[test]
fn a_name_field_hears_the_hosts_rule_while_it_is_typed() {
    let problem = |name: &str| {
        let name = CString::new(name).unwrap();
        // SAFETY: the string lives for the call.
        take(unsafe { amux_agent_name_problem(name.as_ptr()) })
    };
    assert_eq!(problem("fix-login"), Value::Null);
    assert_eq!(problem("fix login"), json!("Characters"));
    assert_eq!(problem(""), json!("Empty"));
}

#[test]
fn a_new_agents_settings_and_picks_come_from_the_shared_view() {
    let catalogue = c(&json!({
        "hash": [1],
        "models": [
            {"value": "astra", "display_name": "Astra", "description": "", "efforts": ["low", "high"],
             "default_effort": "low", "resolved_model": ""},
            {"value": "sol", "display_name": "", "description": "", "efforts": ["low", "medium"],
             "default_effort": "medium", "resolved_model": ""},
        ],
        "commands": [],
        "permissions": [],
        "modes": [],
    })
    .to_string());
    let chosen = c(r#"{"model": "astra", "effort": "high", "permission": null, "mode": null}"#);
    // SAFETY: the strings live for the call.
    let view = take(unsafe { amux_new_agent_settings(catalogue.as_ptr(), chosen.as_ptr()) });
    assert_eq!(view["models"][1]["display_name"], json!("sol"));
    assert_eq!(view["changeable"]["effort"], json!("Pick"));
    let pick = c(r#"{"Model": "sol"}"#);
    // SAFETY: the strings live for the call.
    let next =
        take(unsafe { amux_new_agent_pick(catalogue.as_ptr(), chosen.as_ptr(), pick.as_ptr()) });
    assert_eq!(next["model"], json!("sol"));
    assert_eq!(
        next["effort"],
        json!("medium"),
        "another model, its own default"
    );
    let broken = c("{");
    // SAFETY: the strings live for the call.
    assert!(unsafe { amux_new_agent_settings(broken.as_ptr(), chosen.as_ptr()) }.is_null());
}

#[test]
fn a_forms_problems_come_from_the_shared_view() {
    let fields = c(&json!([
        {"name": "title", "title": "Title", "description": "", "required": true,
         "kind": {"Text": {"min_length": null, "max_length": null}}, "initial": {"Text": ""}},
        {"name": "estimate", "title": "Estimate", "description": "", "required": false,
         "kind": {"Number": {"integer": true, "minimum": 1.0, "maximum": null}}, "initial": {"Text": ""}},
        {"name": "urgent", "title": "Urgent", "description": "", "required": true,
         "kind": "Toggle", "initial": {"Toggle": false}},
    ])
    .to_string());
    let values = c(r#"[{"Text": " "}, {"Text": "2.5"}]"#);
    // SAFETY: the strings live for the call.
    let problems = take(unsafe { amux_form_problems(fields.as_ptr(), values.as_ptr()) });
    assert_eq!(
        problems,
        json!([
            {"field": 0, "problem": "Required"},
            {"field": 1, "problem": "NotWholeNumber"},
        ]),
        "an untouched required toggle is answered, off"
    );
    let filled = c(r#"[{"Text": "Flake"}, {"Text": "3"}, {"Toggle": true}]"#);
    // SAFETY: the strings live for the call.
    let none = take(unsafe { amux_form_problems(fields.as_ptr(), filled.as_ptr()) });
    assert_eq!(none, json!([]));
    let broken = c("{");
    // SAFETY: the strings live for the call.
    assert!(unsafe { amux_form_problems(fields.as_ptr(), broken.as_ptr()) }.is_null());
}
