//! The C ABI as the phone calls it, against a desk served in process: the
//! test holds the pointers, parses the JSON and waits on wakes the way the
//! app's main thread does.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use app_runtime::values::{ActOutcome, ChatChanges, PageOutcome};
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

extern "C" fn on_wake(context: *mut c_void, chat: u64) {
    // SAFETY: the test passes a sender that outlives the runtime.
    let wakes = unsafe { &*(context as *const mpsc::Sender<u64>) };
    let _ = wakes.send(chat);
}

extern "C" fn on_result(context: *mut c_void, json: *const c_char) {
    // SAFETY: the test passes a sender that outlives every act.
    let results = unsafe { &*(context as *const mpsc::Sender<String>) };
    // SAFETY: the library passes a NUL-terminated string.
    let text = unsafe { CStr::from_ptr(json) }.to_str().unwrap().to_owned();
    let _ = results.send(text);
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

/// The phone's side: the pointers and the queues its callbacks feed.
struct Phone {
    runtime: *mut AmuxRuntime,
    /// The one profile a fresh installation has, opened.
    profile: *mut AmuxProfile,
    id: CString,
    wakes: mpsc::Receiver<u64>,
    listed: mpsc::Receiver<u64>,
    results: mpsc::Receiver<String>,
    results_sender: Box<mpsc::Sender<String>>,
    _wake_sender: Box<mpsc::Sender<u64>>,
    _list_sender: Box<mpsc::Sender<u64>>,
    _dir: tempfile::TempDir,
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
        let (wake_sender, wakes) = mpsc::channel();
        let wake_sender = Box::new(wake_sender);
        let (list_sender, listed) = mpsc::channel();
        let list_sender = Box::new(list_sender);
        let (results_sender, results) = mpsc::channel();
        let overrides = EdgeOverrides {
            lan_bind: ([127, 0, 0, 1], 0).into(),
            ..EdgeOverrides::default()
        };
        let runtime = start(
            &config,
            overrides,
            on_wake,
            &*list_sender as *const _ as *mut c_void,
        )
        .unwrap();
        let runtime = Box::into_raw(runtime);
        // SAFETY: the runtime is live.
        let profiles = take(unsafe { amux_runtime_profiles(runtime) });
        assert_eq!(profiles.as_array().unwrap().len(), 1, "{profiles}");
        let id = c(profiles[0]["id"].as_str().unwrap());
        let mut error = std::ptr::null_mut();
        // SAFETY: the runtime is live; the strings live for the call.
        let profile = unsafe {
            amux_profile_open(
                runtime,
                id.as_ptr(),
                on_wake,
                &*wake_sender as *const _ as *mut c_void,
                &mut error,
            )
        };
        assert!(!profile.is_null());
        Phone {
            runtime,
            profile,
            id,
            wakes,
            listed,
            results,
            results_sender: Box::new(results_sender),
            _wake_sender: wake_sender,
            _list_sender: list_sender,
            _dir: dir,
        }
    }

    fn context(&self) -> *mut c_void {
        &*self.results_sender as *const _ as *mut c_void
    }

    fn result(&self) -> Value {
        let text = self
            .results
            .recv_timeout(PATIENCE)
            .expect("the callback ran");
        serde_json::from_str(&text).unwrap()
    }

    /// Waits for a wake naming `chat`, and takes that one's changes.
    fn turn(&self, chat: u64, open: *const AmuxChat) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let woken = self.wakes.recv_timeout(left).expect("woken");
            if woken == chat {
                break;
            }
            if woken == 0 {
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
    unsafe { amux_profile_begin_pair(phone.profile, request.as_ptr(), on_result, phone.context()) };
    let pending = phone.result();
    assert_eq!(pending["Ok"]["name"], "desk", "{pending}");
    assert_eq!(pending["Ok"]["via"], "Direct", "{pending}");
    let fingerprint = pending["Ok"]["fingerprint"].as_str().unwrap().to_owned();
    assert_eq!(fingerprint.len(), 64, "{pending}");
    let token = c(&pending["Ok"]["token"].to_string());
    // SAFETY: the runtime is live; the strings live for the call.
    unsafe { amux_profile_confirm_pair(phone.profile, token.as_ptr(), on_result, phone.context()) };
    let paired = phone.result();
    assert_eq!(paired["Ok"]["name"], "desk", "{paired}");
    fingerprint
}

fn says(chat: *const AmuxChat, wanted: &str) -> bool {
    // SAFETY: the chat is open.
    let keys = take(unsafe { amux_session_keys(chat) });
    let keys = c(&keys.to_string());
    // SAFETY: the chat is open; the strings live for the call.
    let rows = take(unsafe { amux_session_rows_for(chat, keys.as_ptr(), std::ptr::null()) });
    rows.to_string().contains(wanted)
}

fn card(chat: *const AmuxChat) -> Value {
    // SAFETY: the chat is open.
    take(unsafe { amux_session_ask_card(chat) })
}

#[test]
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
    unsafe {
        amux_runtime_trusting_profiles(phone.runtime, host.as_ptr(), on_result, phone.context())
    };
    let trusting = phone.result();
    assert_eq!(
        trusting["Ok"],
        json!([phone.id.to_str().unwrap()]),
        "{trusting}"
    );

    // The roster names this phone and the desk by the same fingerprint.
    // SAFETY: the runtime is live.
    unsafe { amux_profile_roster(phone.profile, on_result, phone.context()) };
    let roster = phone.result();
    assert_eq!(roster["Ok"]["identity"]["name"], "phone", "{roster}");
    assert_eq!(roster["Ok"]["peers"][0]["name"], "desk", "{roster}");
    assert_eq!(
        roster["Ok"]["peers"][0]["fingerprint"],
        fingerprint.as_str()
    );

    // Never signed in: the account says so and lends no bearer.
    // SAFETY: the runtime is live.
    unsafe { amux_profile_account(phone.profile, on_result, phone.context()) };
    let account = phone.result();
    assert_eq!(account["Ok"]["binding"], "Unbound", "{account}");
    // SAFETY: the runtime is live.
    unsafe {
        amux_runtime_access_token(phone.runtime, phone.id.as_ptr(), on_result, phone.context())
    };
    assert!(phone.result()["Err"].is_string());
    // Nor can it ask what an account buys.
    // SAFETY: the runtime is live.
    unsafe {
        amux_runtime_refresh_entitlement(
            phone.runtime,
            phone.id.as_ptr(),
            on_result,
            phone.context(),
        )
    };
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
        let view = take(unsafe { amux_fleet_view(phone.profile, std::ptr::null()) });
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
    unsafe {
        amux_profile_directories(
            phone.profile,
            desk_id.as_ptr(),
            std::ptr::null(),
            20,
            on_result,
            phone.context(),
        )
    };
    // Answered either way; the desk may not list its repositories.
    let directories = phone.result();
    assert!(
        directories["Ok"]["recent"].is_array() || directories["Err"].is_string(),
        "{directories}"
    );

    // Renamed from outside its chat, the card follows.
    let rename = c(&json!({"Rename": "builder"}).to_string());
    // SAFETY: the runtime is live; the strings live for the call.
    unsafe {
        amux_profile_agent_act(
            phone.profile,
            agent_c.as_ptr(),
            rename.as_ptr(),
            on_result,
            phone.context(),
        )
    };
    assert!(phone.result().get("Ok").is_some());
    phone.until(0, std::ptr::null(), "the new name", || {
        // SAFETY: the runtime is live; the string lives for the call.
        take(unsafe { amux_fleet_card(phone.profile, agent_c.as_ptr()) })["name"] == "builder"
    });
    let rename = c(&json!({"Rename": "worker"}).to_string());
    // SAFETY: the runtime is live; the strings live for the call.
    unsafe {
        amux_profile_agent_act(
            phone.profile,
            agent_c.as_ptr(),
            rename.as_ptr(),
            on_result,
            phone.context(),
        )
    };
    assert!(phone.result().get("Ok").is_some());
    phone.until(0, std::ptr::null(), "the name back", || {
        // SAFETY: the runtime is live; the string lives for the call.
        take(unsafe { amux_fleet_card(phone.profile, agent_c.as_ptr()) })["name"] == "worker"
    });

    // Called from a thread outside the pool, as the app's main thread is.
    // SAFETY: the runtime is live.
    unsafe { amux_runtime_set_source_policy(phone.runtime, phone.id.as_ptr(), false) };
    // SAFETY: the runtime is live and not stopped until the phone drops.
    let embedded = unsafe { &*phone.runtime }.embedded().clone();
    let id: ProfileId = phone.id.to_str().unwrap().parse().unwrap();
    assert_eq!(embedded.source_policy(id).unwrap(), SourcePolicy::OnDemand);
    // SAFETY: the runtime is live.
    assert!(!unsafe { amux_runtime_source_policy_listed(phone.runtime, phone.id.as_ptr()) });
    // SAFETY: the runtime is live.
    unsafe { amux_runtime_set_source_policy(phone.runtime, phone.id.as_ptr(), true) };
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
    unsafe { amux_session_send(chat, draft.as_ptr(), on_result, phone.context()) };
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
    unsafe {
        amux_session_answer(
            chat,
            ask_key.as_ptr(),
            allow as u32,
            std::ptr::null(),
            on_result,
            phone.context(),
        )
    };
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
    unsafe { amux_session_send(chat, draft.as_ptr(), on_result, phone.context()) };
    phone.result();
    phone.until(id, chat, "the question", || !card(chat).is_null());
    let ask = card(chat);
    assert!(ask["body"]["Question"].is_array(), "{ask}");
    let ask_key = c(ask["key"].as_str().unwrap());
    let picks = c(&json!([{"pick": {"Options": [1]}}]).to_string());
    // SAFETY: the chat is open; the strings live for the call.
    unsafe {
        amux_session_answer_questions(
            chat,
            ask_key.as_ptr(),
            picks.as_ptr(),
            on_result,
            phone.context(),
        )
    };
    assert_eq!(outcome::<ActOutcome>(phone.result()), ActOutcome::Done);
    phone.until(id, chat, "the answer", || says(chat, "answered"));
    // SAFETY: the chat is open, and closed once.
    unsafe { amux_session_close(chat) };

    // A live agent's session stays with the fleet once its chat closes; an
    // exited one's goes, and a one-row chat on it pages its history in
    // below the oldest it holds.
    let stop = c(&json!("Stop").to_string());
    // SAFETY: the runtime is live; the strings live for the call.
    unsafe {
        amux_profile_agent_act(
            phone.profile,
            agent_c.as_ptr(),
            stop.as_ptr(),
            on_result,
            phone.context(),
        )
    };
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
    unsafe { amux_session_page_older(chat, 2, on_result, phone.context()) };
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
    unsafe { amux_profile_dump(phone.profile, reason.as_ptr(), on_result, phone.context()) };
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
fn no_wake_reaches_a_closed_profile_or_chat_while_its_acts_are_in_flight() {
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
        let view = take(unsafe { amux_fleet_view(phone.profile, std::ptr::null()) });
        view["sections"][0]["rows"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1)
    });

    // A second handle on the same profile, the one to close, with a wake
    // of its own; the phone's first handle watches the same moves.
    let (closing_wakes, closing_woken) = mpsc::channel::<u64>();
    let closing_wakes = Box::new(closing_wakes);
    let mut error = std::ptr::null_mut();
    // SAFETY: the runtime is live; the strings live for the call.
    let closing = unsafe {
        amux_profile_open(
            phone.runtime,
            phone.id.as_ptr(),
            on_wake,
            &*closing_wakes as *const _ as *mut c_void,
            &mut error,
        )
    };
    assert!(!closing.is_null());
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
    unsafe { amux_session_send(chat, draft.as_ptr(), on_result, phone.context()) };
    settle(&closing_woken, closing, chat);
    // SAFETY: from `amux_session_open`, not used again.
    unsafe { amux_session_close(chat) };
    rename(&phone, &agent, watching, "renamed");
    // The fleet moved too, and may wake; the chat may not.
    while let Ok(woken) = closing_woken.recv_timeout(Duration::from_secs(1)) {
        assert_eq!(woken, 0, "a closed chat was woken");
        // SAFETY: the profile is open.
        take(unsafe { amux_fleet_take_changes(closing) });
    }

    // And a dump waits on it too, holding the fleet past its close.
    let reason = c("closing under an act");
    // SAFETY: the profile is open; the string lives for the call.
    unsafe { amux_profile_dump(closing, reason.as_ptr(), on_result, phone.context()) };
    settle(&closing_woken, closing, std::ptr::null());
    // SAFETY: from `amux_profile_open`, not used again.
    unsafe { amux_profile_close(closing) };
    assert!(
        phone.results.try_recv().is_err(),
        "the acts are still in flight"
    );
    rename(&phone, &agent, watching, "worker");
    assert_eq!(
        closing_woken.recv_timeout(Duration::from_secs(1)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "a closed profile was woken"
    );

    // The acts' callbacks still run once the agent answers.
    net.thaw("worker").unwrap();
    phone.result();
    phone.result();
    // SAFETY: the chat is open, and closed once.
    unsafe { amux_session_close(watching) };
    assert_eq!(CAUGHT.with(std::cell::Cell::get), 0, "a call panicked");
    drop(phone);
    tokio.block_on(net.shutdown()).unwrap();
}

/// Takes what a handle's wakes name until none comes for half a second, so
/// its next change wakes it again.
fn settle(woken: &mpsc::Receiver<u64>, profile: *const AmuxProfile, chat: *const AmuxChat) {
    while let Ok(moved) = woken.recv_timeout(Duration::from_millis(500)) {
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
    unsafe {
        amux_profile_agent_act(
            phone.profile,
            agent.as_ptr(),
            act.as_ptr(),
            on_result,
            phone.context(),
        )
    };
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
    /// Waits for the runtime to say the profile list moved, and reads it.
    fn profiles_after_a_wake(&self) -> Value {
        assert_eq!(self.listed.recv_timeout(PATIENCE), Ok(0));
        while self.listed.try_recv().is_ok() {}
        // SAFETY: the runtime is live.
        take(unsafe { amux_runtime_profiles(self.runtime) })
    }
}

#[test]
fn the_profile_registry_through_the_c_abi() {
    let phone = Phone::start();
    // SAFETY: the runtime is live.
    unsafe { amux_runtime_create_profile(phone.runtime, on_result, phone.context()) };
    let created = phone.result();
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
    let (their_wakes, _woken) = mpsc::channel::<u64>();
    let their_wakes = Box::new(their_wakes);
    let mut error = std::ptr::null_mut();
    // SAFETY: the runtime is live; the strings live for the call.
    let opened = unsafe {
        amux_profile_open(
            phone.runtime,
            other.as_ptr(),
            on_wake,
            &*their_wakes as *const _ as *mut c_void,
            &mut error,
        )
    };
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
    unsafe { amux_runtime_set_source_policy(phone.runtime, other.as_ptr(), false) };
    // SAFETY: as above.
    assert!(!unsafe { amux_runtime_source_policy_listed(phone.runtime, other.as_ptr()) });
    // SAFETY: as above.
    assert!(unsafe { amux_runtime_source_policy_listed(phone.runtime, phone.id.as_ptr()) });
    // SAFETY: opened above, not used again.
    unsafe { amux_profile_close(opened) };

    // Only a driving build offers pairing from the phone.
    // SAFETY: the runtime is live; the string lives for the call.
    unsafe {
        amux_runtime_offer_pairing(phone.runtime, other.as_ptr(), on_result, phone.context())
    };
    let offered = phone.result();
    assert_eq!(
        offered["Ok"].is_string(),
        cfg!(feature = "debug-tools"),
        "{offered}"
    );

    // Only a profile bound to an account has a relay link to pause.
    // SAFETY: the runtime is live; the string lives for the call.
    unsafe { amux_runtime_pause(phone.runtime, other.as_ptr(), on_result, phone.context()) };
    assert!(phone.result()["Err"].is_string());

    // SAFETY: as above.
    unsafe {
        amux_runtime_delete_profile(phone.runtime, other.as_ptr(), on_result, phone.context())
    };
    let deleted = phone.result();
    assert!(deleted["Ok"].is_null(), "{deleted}");
    let profiles = phone.profiles_after_a_wake();
    assert_eq!(profiles.as_array().unwrap().len(), 1, "{profiles}");
    // The installation keeps at least one.
    // SAFETY: as above.
    unsafe {
        amux_runtime_delete_profile(phone.runtime, phone.id.as_ptr(), on_result, phone.context())
    };
    assert!(phone.result()["Err"].is_string());
    // Nobody trusts a host nobody paired.
    let host = c(&json!(vec![7u8; 16]).to_string());
    // SAFETY: as above.
    unsafe {
        amux_runtime_trusting_profiles(phone.runtime, host.as_ptr(), on_result, phone.context())
    };
    let trusting = phone.result();
    assert_eq!(trusting["Ok"], json!([]), "{trusting}");
    // A profile id that does not parse is refused.
    let nonsense = c("not a profile");
    // SAFETY: as above.
    unsafe { amux_runtime_resume(phone.runtime, nonsense.as_ptr(), on_result, phone.context()) };
    assert!(phone.result()["Err"].is_string());
}

#[test]
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
    // SAFETY: null chats and runtimes read as nothing.
    assert!(unsafe { amux_session_keys(std::ptr::null()) }.is_null());
    // SAFETY: as above.
    assert!(unsafe { amux_fleet_view(std::ptr::null(), std::ptr::null()) }.is_null());
    // SAFETY: as above.
    assert!(unsafe { amux_session_keep_open_runs(std::ptr::null(), std::ptr::null()) }.is_null());
    // SAFETY: a null profile is ignored.
    unsafe { amux_profile_set_foreground(std::ptr::null(), false) };
    // SAFETY: the runtime is live; the strings live for the call.
    unsafe {
        amux_profile_host_catalogue(
            phone.profile,
            not_json.as_ptr(),
            c"\"Claude\"".as_ptr(),
            on_result,
            phone.context(),
        )
    };
    assert!(phone.result()["Err"].is_string());
    // SAFETY: the runtime is live; the string lives for the call.
    unsafe {
        amux_profile_begin_pair(phone.profile, not_json.as_ptr(), on_result, phone.context())
    };
    assert!(phone.result()["Err"].is_string());
    let config = c("{}");
    let mut error = std::ptr::null_mut();
    // SAFETY: the strings live for the call.
    let runtime =
        unsafe { amux_runtime_start(config.as_ptr(), on_wake, std::ptr::null_mut(), &mut error) };
    assert!(runtime.is_null());
    // SAFETY: returned by the library, freed once.
    unsafe { amux_string_free(error) };
}

fn outcome<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

/// Hands each callback's text to the sender the context points at.
extern "C" fn record(context: *mut c_void, json: *const c_char) {
    // SAFETY: the context is the test's own sender, alive for the test.
    let sender = unsafe { &*(context as *const mpsc::Sender<String>) };
    // SAFETY: the library hands a NUL-terminated string.
    let text = unsafe { CStr::from_ptr(json) }
        .to_string_lossy()
        .into_owned();
    let _ = sender.send(text);
}

#[test]
fn an_act_dropped_by_the_pools_shutdown_answers_that_the_runtime_stopped() {
    let pool = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (tx, rx) = mpsc::channel::<String>();
    let context = Box::into_raw(Box::new(tx)) as *mut c_void;
    spawn_on(pool.handle(), record, context, std::future::pending::<()>());
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "nothing answers while it runs"
    );

    pool.shutdown_timeout(Duration::from_secs(5));

    let answer = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the drop answers");
    assert_eq!(answer, r#"{"Err":"the runtime has stopped"}"#);
    assert!(
        rx.recv_timeout(Duration::from_millis(100)).is_err(),
        "answered once"
    );
    // SAFETY: the pool is down; nothing holds the context now.
    drop(unsafe { Box::from_raw(context as *mut mpsc::Sender<String>) });
}

#[test]
fn an_act_that_finishes_answers_once_with_its_result() {
    let pool = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (tx, rx) = mpsc::channel::<String>();
    let context = Box::into_raw(Box::new(tx)) as *mut c_void;
    spawn_on(pool.handle(), record, context, async { 7u8 });
    let answer = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the act answers");
    assert_eq!(answer, "7");
    pool.shutdown_timeout(Duration::from_secs(5));
    assert!(
        rx.recv_timeout(Duration::from_millis(100)).is_err(),
        "the drop does not answer again"
    );
    // SAFETY: the pool is down; nothing holds the context now.
    drop(unsafe { Box::from_raw(context as *mut mpsc::Sender<String>) });
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
