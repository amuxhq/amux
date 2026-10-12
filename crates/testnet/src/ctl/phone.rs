//! The phone app on the simulator this session holds, driven through the
//! debug build's door the way the phone journeys drive it: a loopback
//! socket the app opens at launch, one JSON request and one reply per line.
//!
//! The simulator is the one the session was started with a lease on
//! (`WT_LEASE_IPHONE`): two sessions on one simulator would fight over it,
//! and taking a lease here would reinstall the app under the session's
//! feet. The app keeps running between commands; each command opens its
//! own connection to the door and closes it however it ends.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::session::{Folder, Outcome, alive, close_pairing, start_pairing};

const BUNDLE_ID: &str = "sh.amux.app";

#[derive(Subcommand, Debug)]
pub enum PhoneCommand {
    /// Install and launch the phone app on this session's simulator,
    /// browsing this net's discovery scope. With --host, pair with that
    /// host too. With --relay, the app reaches accounts through this net's
    /// relay (sign in with `phone connect ACCOUNT`).
    Open {
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        relay: bool,
    },
    /// Tap the element with this identifier (see `phone query`), as a
    /// finger does once it is there to tap.
    ///
    /// Waits up to ten seconds for the element to be on screen and enabled
    /// first, taps it again while the app has not yet drawn it able to take
    /// a tap, then waits for the screen to change and the app to settle, so
    /// the next command sees what the tap did.
    Tap { id: String },
    /// Choose the item with this label in the menu that is open: a menu's
    /// items are drawn by the system and carry no identifier (`phone shot`
    /// shows them).
    Choose { label: String },
    /// Type text into the element with this identifier.
    Type {
        text: String,
        /// The text field's identifier (see `phone query`).
        #[arg(long = "into")]
        into: String,
    },
    /// Scroll: up, down, top or bottom; in one element, or the screen.
    Scroll {
        direction: String,
        #[arg(long = "in")]
        within: Option<String>,
    },
    /// What is on screen: the screen's name and every element with its
    /// identifier, label, value and whether it is enabled.
    Query,
    /// A picture of the simulator's display, written to shots/.
    Shot,
    /// Dress the app light or dark.
    Appearance { appearance: String },
    /// Set the text size, as Settings does: xSmall, small, medium, large,
    /// xLarge, xxLarge, xxxLarge, accessibility1 ... accessibility5.
    TextSize { size: String },
    /// Pair the phone with a host by its PIN, as a person does. The host
    /// needs --lan and --bonjour, so the phone finds it on the network.
    Pair { host: String },
    /// Sign the phone in to a relay account (needs `phone open --relay`).
    Connect {
        account: String,
        #[arg(long, default_value = "pro")]
        tier: String,
    },
    /// Wait until the app has nothing in flight, then answer.
    Settle,
    /// Attach FILE to AGENT's draft as the photo library or file browser
    /// hands a pick back to the app (the pickers are the system's own
    /// screens, which nothing outside them drives). With --image it is a
    /// photo, otherwise a file. Open the agent's chat first.
    Attach {
        agent: String,
        file: PathBuf,
        #[arg(long)]
        image: bool,
    },
}

/// What the session knows of the running app.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Running {
    udid: String,
    container: PathBuf,
    ready: PathBuf,
}

fn state_file(folder: &Folder) -> PathBuf {
    folder.net_dir().join("phone.json")
}

fn running(folder: &Folder) -> Result<Running, String> {
    let text = std::fs::read_to_string(state_file(folder))
        .map_err(|_| "the phone is not open: `ctl phone open` first".to_owned())?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

pub(crate) fn is_open(folder: &Folder) -> bool {
    state_file(folder).exists()
}

/// The simulator this session holds a lease on.
fn leased() -> Result<String, String> {
    std::env::var("WT_LEASE_IPHONE")
        .ok()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            "this session holds no simulator: a phone session runs under a lease \
             (wt exec <tree> -- scripts/with iphone -- ...), and ctl never takes one itself"
                .to_owned()
        })
}

fn simctl(args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("xcrun")
        .arg("simctl")
        .args(args)
        .output()
        .map_err(|error| format!("running simctl: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "simctl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn udid(name: &str) -> Result<String, String> {
    let listed: Value = serde_json::from_str(&simctl(&["list", "devices", "available", "-j"])?)
        .map_err(|error| error.to_string())?;
    let found: Vec<String> = listed["devices"]
        .as_object()
        .into_iter()
        .flat_map(|runtimes| runtimes.values())
        .flat_map(|devices| devices.as_array().into_iter().flatten())
        .filter(|device| device["name"] == name)
        .filter_map(|device| device["udid"].as_str().map(str::to_owned))
        .collect();
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!("no simulator named {name}")),
        _ => Err(format!("more than one simulator is named {name}")),
    }
}

/// The type a picker would name for a file, by its extension.
fn mime(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "heic" => "image/heic",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "zip" => "application/zip",
        "txt" | "md" | "log" => "text/plain",
        "csv" => "text/csv",
        _ => "application/octet-stream",
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(char::from(ALPHABET[(word >> shift) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The debug app the session installs: the one `just qa` built, or the
/// tree's own build.
fn app() -> Result<PathBuf, String> {
    let path = match std::env::var_os("AMUX_QA_APP") {
        Some(path) => PathBuf::from(path),
        None => {
            let exe = std::env::current_exe().map_err(|error| error.to_string())?;
            // target/debug/testnet -> target/ios/...
            exe.parent()
                .and_then(Path::parent)
                .map(|target| {
                    target.join("ios/DerivedData/Build/Products/Debug-iphonesimulator/Amux.app")
                })
                .unwrap_or_default()
        }
    };
    if path.is_dir() {
        Ok(path)
    } else {
        Err(format!(
            "no debug phone app at {}: build it with `just ios build`",
            path.display()
        ))
    }
}

pub(crate) fn run(folder: &Folder, command: PhoneCommand) -> Result<Outcome, String> {
    match command {
        PhoneCommand::Open { host, relay } => {
            let mut outcome = open(folder, relay)?;
            if let Some(host) = host {
                let paired = pair(folder, &host)?;
                outcome.text.push('\n');
                outcome.text.push_str(&paired.text);
            }
            Ok(outcome)
        }
        PhoneCommand::Tap { id } => tap(folder, &id),
        PhoneCommand::Choose { label } => {
            request(folder, json!({"kind": "choose", "label": label}))
                .map(|reply| Outcome::new(format!("chose {label}"), reply))
        }
        PhoneCommand::Type { text, into } => request(
            folder,
            json!({"kind": "type", "identifier": into, "text": text}),
        )
        .map(|reply| Outcome::new(format!("typed into {into}"), reply)),
        PhoneCommand::Scroll { direction, within } => {
            let mut asked = json!({"kind": "scroll", "direction": direction});
            if let Some(within) = within {
                asked["identifier"] = json!(within);
            }
            request(folder, asked).map(|reply| Outcome::new(format!("scrolled {direction}"), reply))
        }
        PhoneCommand::Query => {
            let reply = request(folder, json!({"kind": "query"}))?;
            Ok(Outcome::new(describe(&reply["state"]), reply))
        }
        PhoneCommand::Shot => {
            let path = shot(folder)?;
            Ok(
                Outcome::new(path.display().to_string(), json!({"png": path}))
                    .with_files(vec![path]),
            )
        }
        PhoneCommand::Appearance { appearance } => request(
            folder,
            json!({"kind": "appearance", "appearance": appearance}),
        )
        .map(|reply| Outcome::new(format!("the app is {appearance}"), reply)),
        PhoneCommand::TextSize { size } => {
            request(folder, json!({"kind": "dynamicType", "size": size}))
                .map(|reply| Outcome::new(format!("text size {size}"), reply))
        }
        PhoneCommand::Pair { host } => pair(folder, &host),
        PhoneCommand::Connect { account, tier } => {
            let record = folder.record()?;
            let cloud = record.readiness["cloud_url"]
                .as_str()
                .ok_or("the net has no relay")?
                .to_owned();
            request(
                folder,
                json!({
                    "kind": "connect",
                    "relay": cloud,
                    "token": format!("refresh-{account}"),
                    "user": account,
                    "tier": tier,
                }),
            )
            .map(|reply| Outcome::new(format!("the phone signed in as {account}"), reply))
        }
        PhoneCommand::Attach { agent, file, image } => {
            let path = folder.dir.join(&file);
            let bytes = std::fs::read(&path)
                .map_err(|error| format!("reading {}: {error}", path.display()))?;
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            request(
                folder,
                json!({
                    "kind": "attach",
                    "agent": agent,
                    "attachment": if image { "image" } else { "file" },
                    "name": name,
                    "mime": mime(&path),
                    "base64": base64(&bytes),
                }),
            )
            .map(|reply| Outcome::new(format!("attached {name} to {agent}'s draft"), reply))
        }
        PhoneCommand::Settle => request(folder, json!({"kind": "settle"}))
            .map(|reply| Outcome::new("the app has settled", reply)),
    }
}

/// How long a tap waits for its element to be on screen and enabled, and
/// for a just-enabled control to take it.
const TAP_PATIENCE: Duration = Duration::from_secs(10);
/// How long a tap waits for the screen to change after it.
const TAP_ANSWER: Duration = Duration::from_secs(3);

/// The element a query's state lists under `id`.
fn element<'a>(state: &'a Value, id: &str) -> Option<&'a Value> {
    state["elements"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|element| element["identifier"] == id)
}

/// A tap as a person makes one: on a control that is there and enabled,
/// and over once the screen has answered it. The app enables a control
/// in its model a frame or more before it draws it enabled, and a control
/// drawn disabled does not take the tap.
fn tap(folder: &Folder, id: &str) -> Result<Outcome, String> {
    let deadline = Instant::now() + TAP_PATIENCE;
    let before = loop {
        let state = request(folder, json!({"kind": "query"}))?["state"].take();
        let found = element(&state, id);
        if found.is_some_and(|element| element["enabled"] != false) {
            break state;
        }
        if Instant::now() > deadline {
            return Err(match found {
                Some(_) => format!(
                    "{id} stayed disabled for {} seconds",
                    TAP_PATIENCE.as_secs()
                ),
                None => format!(
                    "no element {id} on screen within {} seconds; on screen:\n{}",
                    TAP_PATIENCE.as_secs(),
                    describe(&state)
                ),
            });
        }
        std::thread::sleep(Duration::from_millis(150));
    };
    let reply = loop {
        match request(folder, json!({"kind": "tap", "identifier": id})) {
            Ok(reply) => break reply,
            Err(error) if error.ends_with("did not activate") && Instant::now() < deadline => {
                request(folder, json!({"kind": "settle"}))?;
            }
            Err(error) => return Err(error),
        }
    };
    let answered = Instant::now() + TAP_ANSWER;
    while Instant::now() < answered {
        let state = request(folder, json!({"kind": "query"}))?["state"].take();
        if state != before {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    request(folder, json!({"kind": "settle"}))?;
    Ok(Outcome::new(format!("tapped {id}"), reply))
}

fn open(folder: &Folder, relay: bool) -> Result<Outcome, String> {
    let name = leased()?;
    let udid = udid(&name)?;
    let app = app()?;
    // Booting a booted device fails harmlessly.
    let _ = simctl(&["boot", &udid]);
    simctl(&["bootstatus", &udid, "-b"])?;
    let _ = simctl(&["terminate", &udid, BUNDLE_ID]);
    // Installed from the net's own copy: the build folder is shared, and
    // another session rebuilding the app there mid-install breaks it.
    let app = own_copy(folder, &app)?;
    simctl(&["install", &udid, app.to_string_lossy().as_ref()])?;
    let container = PathBuf::from(
        simctl(&["get_app_container", &udid, BUNDLE_ID, "data"])?
            .trim()
            .to_owned(),
    );
    let ready = container.join("tmp/door-ready.json");
    std::fs::create_dir_all(container.join("tmp")).map_err(|error| error.to_string())?;
    let _ = std::fs::remove_file(&ready);
    let record = folder.record()?;
    let ready_text = ready.to_string_lossy().into_owned();
    let mut args: Vec<String> = vec![
        "launch".into(),
        udid.clone(),
        BUNDLE_ID.into(),
        "-amux-door-ready".into(),
        ready_text,
        "-amux-discovery-scope".into(),
        record.scope.clone(),
        "-amux-lan-bind".into(),
        "127.0.0.1:0".into(),
        "-amux-element-geometry".into(),
    ];
    if relay {
        let readiness = &record.readiness;
        let cloud = readiness["cloud_url"]
            .as_str()
            .ok_or("the net has no relay")?;
        let gate = folder.call(json!({"RelayGate": {}}))?;
        args.extend(
            [
                "-amux-scripted-cloud",
                "-amux-relay",
                cloud,
                "-amux-relay-tcp",
                readiness["relay_tcp"].as_str().unwrap_or_default(),
                "-amux-relay-quic",
                gate["addr"].as_str().unwrap_or_default(),
                "-amux-relay-root",
                readiness["relay_root"].as_str().unwrap_or_default(),
            ]
            .map(str::to_owned),
        );
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    simctl(&args)?;
    let deadline = Instant::now() + Duration::from_secs(120);
    while door_port(&ready).is_none() {
        if Instant::now() > deadline {
            return Err("the app's door never opened (no door-ready.json)".to_owned());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::fs::write(
        state_file(folder),
        serde_json::to_string_pretty(&Running {
            udid: udid.clone(),
            container,
            ready,
        })
        .unwrap_or_default(),
    )
    .map_err(|error| error.to_string())?;
    // The app is up once it answers.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match request(folder, json!({"kind": "query"})) {
            Ok(reply) => {
                return Ok(Outcome::new(
                    format!(
                        "the phone app is open on {name}, browsing scope {}\n{}",
                        record.scope,
                        describe(&reply["state"])
                    ),
                    reply,
                ));
            }
            Err(error) if Instant::now() > deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

/// The door's port, when the app that wrote it is still running: the file
/// outlives the app.
fn door_port(ready: &Path) -> Option<u16> {
    let text = std::fs::read_to_string(ready).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let pid = value["pid"].as_u64()? as u32;
    alive(pid)
        .then(|| value["port"].as_u64())
        .flatten()
        .map(|port| port as u16)
}

/// One request on its own connection, closed whatever happens.
fn request(folder: &Folder, asked: Value) -> Result<Value, String> {
    let app = running(folder)?;
    let port = door_port(&app.ready)
        .ok_or("the phone app is not running any more: `ctl phone open` again")?;
    let stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|error| format!("the app's door does not answer: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .map_err(|error| error.to_string())?;
    let mut writer = &stream;
    writeln!(writer, "{asked}").map_err(|error| error.to_string())?;
    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .map_err(|error| format!("the app did not answer: {error}"))?;
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let reply: Value =
        serde_json::from_str(line.trim()).map_err(|error| format!("{error}: {line}"))?;
    if reply["kind"] == "error" {
        return Err(format!(
            "the app refused: {}",
            reply["message"].as_str().unwrap_or("?")
        ));
    }
    Ok(reply)
}

fn describe(state: &Value) -> String {
    let mut out = format!(
        "screen {}  text size {}\n",
        state["screen"].as_str().unwrap_or("?"),
        state["typeSize"].as_str().unwrap_or("?")
    );
    for element in state["elements"].as_array().into_iter().flatten() {
        let mut line = format!("  {}", element["identifier"].as_str().unwrap_or("?"));
        if let Some(label) = element["label"].as_str().filter(|label| !label.is_empty()) {
            line.push_str(&format!("  {label:?}"));
        }
        if let Some(value) = element["value"].as_str().filter(|value| !value.is_empty()) {
            line.push_str(&format!("  = {value:?}"));
        }
        if element["enabled"] == false {
            line.push_str("  (disabled)");
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// A picture of the simulator's whole display, as a person sees it.
pub(crate) fn shot(folder: &Folder) -> Result<PathBuf, String> {
    let app = running(folder)?;
    let dir = folder.dir.join("shots");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let path = dir.join(format!(
        "phone-{}.png",
        chrono::Utc::now().format("%H%M%S%3f")
    ));
    simctl(&[
        "io",
        &app.udid,
        "screenshot",
        "--type=png",
        path.to_string_lossy().as_ref(),
    ])?;
    Ok(path)
}

/// The phone's state as text, for a capture.
pub(crate) fn query_text(folder: &Folder) -> Result<String, String> {
    let reply = request(folder, json!({"kind": "query"}))?;
    Ok(describe(&reply["state"]))
}

/// Has the app write its own dump, client parts and all, and copies it to
/// `into`. The app writes one when it freezes the screen for a report,
/// which a screenshot starts; the report it offers is then dismissed.
pub(crate) fn dump(folder: &Folder, into: &Path) -> Result<PathBuf, String> {
    let app = running(folder)?;
    let reports = app
        .container
        .join("Library/Application Support/amux/installation/reports");
    let before: Vec<PathBuf> = dumps_in(&reports);
    request(folder, json!({"kind": "screenshot"}))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let made = loop {
        if let Some(made) = dumps_in(&reports)
            .into_iter()
            .find(|dir| !before.contains(dir) && dir.join("manifest.json").exists())
        {
            break made;
        }
        if Instant::now() > deadline {
            return Err("the app wrote no dump within 30 seconds of a screenshot".to_owned());
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    // Its manifest is written last, so the dump is whole once it shows.
    let dest = into.join(made.file_name().unwrap_or_default());
    copy_dir(&made, &dest)?;
    let _ = request(
        folder,
        json!({"kind": "tap", "identifier": "report.prompt"}),
    );
    let _ = request(
        folder,
        json!({"kind": "tap", "identifier": "report.cancel"}),
    );
    Ok(dest)
}

fn dumps_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("dump-"))
        })
        .collect()
}

/// A copy of the app in the net's folder, made afresh at each open.
fn own_copy(folder: &Folder, app: &Path) -> Result<PathBuf, String> {
    let copy = folder.net_dir().join("app").join("Amux.app");
    if copy.exists() {
        std::fs::remove_dir_all(&copy)
            .map_err(|error| format!("clearing {}: {error}", copy.display()))?;
    }
    std::fs::create_dir_all(folder.net_dir().join("app")).map_err(|error| error.to_string())?;
    // ditto keeps the bundle whole: its links, modes and signature.
    let status = std::process::Command::new("ditto")
        .arg(app)
        .arg(&copy)
        .status()
        .map_err(|error| format!("running ditto: {error}"))?;
    if !status.success() {
        return Err(format!(
            "copying {} into the net's folder failed",
            app.display()
        ));
    }
    Ok(copy)
}

pub(crate) fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|error| error.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target)?;
        } else if path.is_file() {
            std::fs::copy(&path, &target).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn pair(folder: &Folder, host: &str) -> Result<Outcome, String> {
    let entry = folder.host(host)?;
    let config = entry["config"]
        .as_str()
        .ok_or("the host has no config")?
        .to_owned();
    let id = entry["host_id"]
        .as_str()
        .ok_or("the host has no id")?
        .to_owned();
    let (pin, _) = start_pairing(folder, host, &config, &["pair".to_owned()])?;
    // The phone must have found the host before it can pair by code; until
    // then the door says it does not know it.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match request(
            folder,
            json!({"kind": "pairByCode", "host": id, "pin": pin}),
        ) {
            Ok(reply) => {
                return Ok(Outcome::new(
                    format!("the phone paired with {host} (PIN {pin})"),
                    reply,
                ));
            }
            Err(error) if Instant::now() > deadline => {
                // The window this pairing opened would refuse the next
                // attempt as already open.
                close_pairing(folder, host, &config);
                return Err(format!(
                    "the phone could not pair with {host}: {error} \
                     (does it have --lan and --bonjour?); its pairing window is closed again"
                ));
            }
            Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    }
}

/// Ends the app at the end of a session.
pub(crate) fn close(folder: &Folder) {
    if let Ok(app) = running(folder) {
        let _ = simctl(&["terminate", &app.udid, BUNDLE_ID]);
    }
    let _ = std::fs::remove_file(state_file(folder));
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        for (bytes, encoded) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foobar", "Zm9vYmFy"),
            (&[0xff, 0xfe, 0x00], "//4A"),
        ] {
            assert_eq!(super::base64(bytes), encoded);
        }
    }
}
