//! The Mac side of the iPhone app's driving door.
//!
//! A debug build of the app listens on loopback and answers one JSON request
//! per line. This launches it on a pinned simulator, waits for the port it
//! writes, speaks the requests it is given and hands back the answers. Every
//! capture and every scripted interaction the flight makes goes through here,
//! so a failure has one place to be diagnosed rather than one per recipe.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug)]
pub enum DoorError {
    /// No simulator of that name, or more than one.
    NoSimulator(String),
    /// The app is not installed on the simulator.
    NotInstalled(String),
    /// The app was launched but never said which port it was listening on.
    NeverReady {
        seconds: u64,
    },
    /// The app answered something that is not a door reply.
    Unreadable(String),
    /// The simulator's display photographed at a size the device does not
    /// have. A capture of the wrong size is a capture of the wrong device.
    WrongSize {
        got: (u32, u32),
        device: (u32, u32),
    },
    Io(String),
    Tool {
        command: String,
        output: String,
    },
}

impl std::fmt::Display for DoorError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSimulator(name) => write!(out, "no simulator named {name}"),
            Self::NotInstalled(id) => write!(out, "{id} is not installed on the simulator"),
            Self::NeverReady { seconds } => {
                write!(out, "the app did not open its door within {seconds}s")
            }
            Self::Unreadable(line) => write!(out, "the door answered something unreadable: {line}"),
            Self::WrongSize { got, device } => write!(
                out,
                "the display photographed {}x{} but the simulator's screen is {}x{}",
                got.0, got.1, device.0, device.1
            ),
            Self::Io(message) => write!(out, "{message}"),
            Self::Tool { command, output } => write!(out, "{command} failed: {output}"),
        }
    }
}

impl std::error::Error for DoorError {}

impl From<std::io::Error> for DoorError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

fn simctl(arguments: &[&str]) -> Result<String, DoorError> {
    let output = Command::new("xcrun")
        .arg("simctl")
        .args(arguments)
        .output()
        .map_err(|error| DoorError::Io(error.to_string()))?;
    if !output.status.success() {
        return Err(DoorError::Tool {
            command: format!("simctl {}", arguments.join(" ")),
            output: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[derive(Deserialize)]
struct Device {
    name: String,
    udid: String,
}

#[derive(Deserialize)]
struct Devices {
    devices: std::collections::BTreeMap<String, Vec<Device>>,
}

/// The udid of the pinned simulator with this name. `just ios simulator`
/// creates it; this never does, so a run cannot silently measure a device
/// nobody pinned.
/// The udid of the device a simulator kind or name means. A kind resolves
/// through `simulator::resolve` first, so every caller sees the leased device.
pub fn simulator_udid(simulator: &str) -> Result<String, DoorError> {
    let name = crate::simulator::resolve(simulator)?;
    let name = name.as_str();
    let listed = simctl(&["list", "devices", "available", "-j"])?;
    let devices: Devices =
        serde_json::from_str(&listed).map_err(|error| DoorError::Unreadable(error.to_string()))?;
    let mut matching: Vec<String> = devices
        .devices
        .values()
        .flatten()
        .filter(|device| device.name == name)
        .map(|device| device.udid.clone())
        .collect();
    match matching.len() {
        1 => Ok(matching.remove(0)),
        _ => Err(DoorError::NoSimulator(name.to_string())),
    }
}

/// Where the installed app keeps its data. Both sides need a directory the
/// sandboxed app may write and the Mac may read; this is the only one.
pub fn app_container(udid: &str, bundle_id: &str) -> Result<PathBuf, DoorError> {
    match simctl(&["get_app_container", udid, bundle_id, "data"]) {
        Ok(path) => Ok(PathBuf::from(path)),
        Err(DoorError::Tool { .. }) => Err(DoorError::NotInstalled(bundle_id.to_string())),
        Err(other) => Err(other),
    }
}

/// Installs a built `.app` on the simulator, replacing whatever was there.
pub fn install(udid: &str, application: &Path) -> Result<(), DoorError> {
    simctl(&["boot", udid]).ok();
    simctl(&["bootstatus", udid, "-b"])?;
    simctl(&["install", udid, &application.to_string_lossy()])?;
    Ok(())
}

/// Launches the app on the simulator and speaks the given requests to it, in
/// order, returning one reply for each.
///
/// A request that names a path names one on the Mac, and the app is sandboxed
/// away from all of them. A `capture` or a `report` is asked to write inside
/// the app's own container and what it wrote is moved out afterwards; a
/// `replay` has its bundle copied into the container first and is pointed at
/// the copy. Either way the reply names the path the caller asked for, and
/// every other request is passed through untouched.
pub fn door(
    simulator: &str,
    bundle_id: &str,
    requests: Vec<Value>,
    timeout: Duration,
) -> Result<Vec<Value>, DoorError> {
    let udid = simulator_udid(simulator)?;
    let container = app_container(&udid, bundle_id)?;
    let ready = container.join("tmp/door-ready.json");
    std::fs::create_dir_all(ready.parent().expect("the ready path has a directory"))?;
    std::fs::remove_file(&ready).ok();

    simctl(&["terminate", &udid, bundle_id]).ok();
    simctl(&[
        "launch",
        &udid,
        bundle_id,
        "-amux-door-ready",
        &ready.to_string_lossy(),
    ])?;

    let outcome = converse(&udid, &container, &ready, requests, timeout);
    simctl(&["terminate", &udid, bundle_id]).ok();
    outcome
}

#[derive(Deserialize)]
struct Ready {
    port: u16,
}

fn converse(
    udid: &str,
    container: &Path,
    ready: &Path,
    requests: Vec<Value>,
    timeout: Duration,
) -> Result<Vec<Value>, DoorError> {
    let port = wait_for_port(ready, timeout)?;
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let stream = TcpStream::connect_timeout(&address.into(), timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut writing = stream.try_clone()?;
    let mut reading = BufReader::new(stream);

    // Where anything crossing the sandbox boundary is staged, in both
    // directions: what the app writes for the Mac, and what the Mac hands the
    // app to read.
    let scratch = container.join("tmp/door");
    std::fs::create_dir_all(&scratch)?;

    let mut replies = Vec::with_capacity(requests.len());
    for (index, request) in requests.into_iter().enumerate() {
        // A photograph of the display is taken from this side rather than
        // asked of the app. See `display`.
        if request.get("kind").and_then(Value::as_str) == Some("display") {
            let destination = PathBuf::from(
                request
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DoorError::Unreadable("display needs a path".into()))?,
            );
            display(udid, &destination)?;
            replies.push(json!({"kind": "ack", "path": destination.to_string_lossy()}));
            continue;
        }
        let wanted = traffic(&request);
        let sent = match &wanted {
            Some((Traffic::CaptureOut, _)) => {
                rewrite_path(&request, &scratch.join(format!("capture-{index}.png")))
            }
            Some((Traffic::BundleOut, _)) => {
                let inside = scratch.join(format!("bundle-{index}"));
                std::fs::create_dir_all(&inside)?;
                rewrite_path(&request, &inside)
            }
            None => request,
        };
        writeln!(writing, "{sent}")?;
        writing.flush()?;
        let mut line = String::new();
        if reading.read_line(&mut line)? == 0 {
            return Err(DoorError::Unreadable(
                "the door closed mid-conversation".into(),
            ));
        }
        let mut reply: Value = serde_json::from_str(line.trim())
            .map_err(|_| DoorError::Unreadable(line.trim().to_string()))?;
        match wanted {
            Some((Traffic::CaptureOut, destination)) => collect_file(&mut reply, &destination)?,
            Some((Traffic::BundleOut, destination)) => collect_directory(&mut reply, &destination)?,
            None => {}
        }
        replies.push(reply);
    }
    Ok(replies)
}

/// Photographs the simulator's display, as the device itself draws it, once it
/// has stopped changing.
///
/// The app cannot take this picture. Drawing a view hierarchy into an image
/// asks every system material on it to resolve against a renderer that is not
/// the one on screen, and glass resolved that way is not stable: the lensing
/// along a card's top edge appeared on some passes and not others, so a
/// baseline of a glass screen failed about one run in three whichever pass it
/// was taken from. The render server does not have that problem, because it is
/// the thing that draws the material in the first place, so the picture is
/// taken from its output instead.
///
/// Settling is decided here rather than inside the app for the same reason.
/// The app can only see its own view tree, and a glass surface that has just
/// been built animates into place in the render server for a length of time
/// nobody publishes; a tree that has stopped changing is not a screen that has
/// stopped moving. So the display is photographed repeatedly until it has come
/// to rest, which is the only evidence available that the thing being captured
/// has stopped moving.
///
/// Rest means a run of identical photographs rather than a pair of them,
/// because a pair half a second apart can both catch something that is about
/// to move, and the first screen of a run then says something the twentieth
/// does not.
///
/// The app is the only thing running on the device and the door has already
/// said the screen is built, so what is photographed is the screen under test
/// with the system's own chrome over it. The status bar is pinned to 9:41
/// with a full battery, which the design's own references draw too. The home
/// indicator cannot be pinned: SpringBoard draws it at launch and withdraws it
/// when backboardd's attention timer fires, and on a loaded GitHub runner that
/// event reaches a stale client and the bar stays for the whole run. So the
/// comparison, not the photograph, is what looks past it: the manifest names
/// the bar's rectangle for the device and no pixel under it is compared.
fn display(udid: &str, destination: &Path) -> Result<(), DoorError> {
    if let Some(directory) = destination.parent() {
        std::fs::create_dir_all(directory)?;
    }
    std::fs::remove_file(destination).ok();
    let device = display_size(udid)?;

    // The ceiling exists because some screens never come to rest — a row that
    // is only remembered has a sweep passing over it forever — and a capture
    // of one still has to happen. Whatever the last photograph caught is what
    // is kept, and the comparison against the baseline is what judges it.
    let mut previous: Option<Vec<u8>> = None;
    let mut agreed = 1;
    for _ in 0..STEADY_SHOTS {
        simctl(&[
            "io",
            udid,
            "screenshot",
            "--type",
            "png",
            &destination.to_string_lossy(),
        ])?;
        let got = png_size(destination)?;
        if got != device {
            return Err(DoorError::WrongSize { got, device });
        }
        let bytes = std::fs::read(destination)?;
        if previous.as_deref() == Some(bytes.as_slice()) {
            agreed += 1;
            if agreed >= STEADY_RUN {
                return Ok(());
            }
        } else {
            agreed = 1;
        }
        previous = Some(bytes);
    }
    Ok(())
}

/// How many photographs of one screen are taken while waiting for a run of
/// them to agree.
///
/// What one costs is the machine's, not ours: measured at 0.13s on a
/// developer's Mac and 0.7s on a continuous-integration runner, so this
/// ceiling is worth between three and seventeen seconds depending on who is
/// holding the camera.
const STEADY_SHOTS: usize = 24;

/// How many photographs in a row have to be the same file before the display
/// counts as still.
///
/// This says the display has stopped changing. It cannot say the display is
/// showing the right thing, and the two are not the same: a run of identical
/// frames is equally consistent with a screen that has come to rest and with
/// one holding something that should have gone away. So a capture that comes
/// back wrong is not evidence against this number until somebody has watched
/// the frames and seen the screen still moving when it was taken.
const STEADY_RUN: usize = 8;

/// A PNG's width and height, read from the header rather than decoded.
fn png_size(path: &Path) -> Result<(u32, u32), DoorError> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err(DoorError::Unreadable(format!(
            "{} is not a PNG",
            path.display()
        )));
    }
    let read =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Ok((read(16), read(20)))
}

/// The pinned device's own screen size, asked of the simulator.
///
/// The pin is what makes a capture comparable at all: a screenshot taken on a
/// device of another size is a picture of another phone, and it must fail
/// rather than be scaled or cropped into agreement. A simulator enumerates
/// several ports; the one being photographed is the built-in display, which is
/// display class 0.
fn display_size(udid: &str) -> Result<(u32, u32), DoorError> {
    let enumerated = simctl(&["io", udid, "enumerate"])?;
    let mut width = None;
    let mut height = None;
    let mut builtin = false;
    for line in enumerated.lines() {
        let line = line.trim();
        if let Some(class) = line.strip_prefix("Display class:") {
            builtin = class.trim() == "0";
            width = None;
            height = None;
        }
        if let Some(value) = line.strip_prefix("Default width:") {
            width = value.trim().parse::<u32>().ok();
        }
        if let Some(value) = line.strip_prefix("Default height:") {
            height = value.trim().parse::<u32>().ok();
        }
        if builtin && let (Some(width), Some(height)) = (width, height) {
            return Ok((width, height));
        }
    }
    Err(DoorError::Unreadable(format!(
        "{udid} does not enumerate a built-in display"
    )))
}

/// Which way a request's path has to travel across the sandbox boundary.
enum Traffic {
    /// One file the app writes and the Mac keeps.
    CaptureOut,
    /// A directory of files the app writes and the Mac keeps.
    BundleOut,
}

fn traffic(request: &Value) -> Option<(Traffic, PathBuf)> {
    let path = PathBuf::from(request.get("path")?.as_str()?);
    let direction = match request.get("kind")?.as_str()? {
        "capture" => Traffic::CaptureOut,
        "report" => Traffic::BundleOut,
        _ => return None,
    };
    Some((direction, path))
}

fn rewrite_path(request: &Value, inside: &Path) -> Value {
    let mut rewritten = request.clone();
    rewritten["path"] = json!(inside.to_string_lossy());
    rewritten
}

/// Moves a capture out of the app's container to where the caller asked for
/// it, and rewrites the reply to name that path.
fn collect_file(reply: &mut Value, destination: &Path) -> Result<(), DoorError> {
    let Some(written) = reply.get("path").and_then(Value::as_str) else {
        return Ok(());
    };
    if let Some(directory) = destination.parent() {
        std::fs::create_dir_all(directory)?;
    }
    std::fs::copy(written, destination)?;
    std::fs::remove_file(written).ok();
    reply["path"] = json!(destination.to_string_lossy());
    Ok(())
}

/// Moves a written bundle out of the container. Only the files the reply named
/// are taken, so whatever else the app keeps in its container stays there.
fn collect_directory(reply: &mut Value, destination: &Path) -> Result<(), DoorError> {
    let Some(written) = reply.get("path").and_then(Value::as_str) else {
        return Ok(());
    };
    let parts: Vec<String> = reply
        .get("parts")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    std::fs::create_dir_all(destination)?;
    for part in parts {
        let from = Path::new(written).join(&part);
        std::fs::copy(&from, destination.join(&part))?;
        std::fs::remove_file(&from).ok();
    }
    reply["path"] = json!(destination.to_string_lossy());
    Ok(())
}

fn wait_for_port(ready: &Path, timeout: Duration) -> Result<u16, DoorError> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(ready)
            && let Ok(Ready { port }) = serde_json::from_str::<Ready>(&text)
        {
            return Ok(port);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(DoorError::NeverReady {
        seconds: timeout.as_secs(),
    })
}

// MARK: - The command

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(2);
    let mut simulator = "golden".to_string();
    let mut bundle_id = "sh.amux.app".to_string();
    let mut timeout = Duration::from_secs(120);
    let mut install_from: Option<PathBuf> = None;
    let mut requests: Vec<Value> = Vec::new();
    let mut allow_errors = false;

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--simulator" => simulator = arguments.next().unwrap_or_default(),
            "--bundle-id" => bundle_id = arguments.next().unwrap_or_default(),
            "--install" => install_from = arguments.next().map(PathBuf::from),
            "--allow-errors" => allow_errors = true,
            "--timeout" => {
                timeout = Duration::from_secs(arguments.next().unwrap_or_default().parse()?)
            }
            "--requests" => {
                let path = arguments.next().unwrap_or_default();
                let text = std::fs::read_to_string(&path)?;
                match serde_json::from_str::<Value>(&text)? {
                    Value::Array(listed) => requests.extend(listed),
                    other => requests.push(other),
                }
            }
            other => requests.push(serde_json::from_str(other)?),
        }
    }

    if let Some(application) = install_from {
        install(&simulator_udid(&simulator)?, &application)?;
    }
    let replies = door(&simulator, &bundle_id, requests, timeout)?;
    println!("{}", serde_json::to_string_pretty(&replies)?);
    let refused: Vec<&Value> = replies
        .iter()
        .filter(|reply| reply.get("kind").and_then(Value::as_str) == Some("error"))
        .collect();
    if !refused.is_empty() && !allow_errors {
        eprintln!(
            "the door refused {} of {} requests",
            refused.len(),
            replies.len()
        );
        std::process::exit(1);
    }
    Ok(())
}
