# Writing a client

*For anyone writing their own amux client — a desktop app, an editor
plugin, a script: how to find amux on a machine, install it when it is
missing, sign in, choose a profile and read its agents.*

amux runs as a daemon on each machine. A client, amux's own terminal client
and apps included, keeps no state of its own and holds no account token: it
asks that daemon, in gRPC over local sockets. Every client does the same
five things:

1. **Find the installation** and its front door, the socket the daemon
   answers on.
2. **Install or start amux** when nothing answers.
3. **Pick a profile**: one identity, with its own account, paired machines
   and agents.
4. **Sign the profile in**, when the person wants to reach their other
   machines.
5. **Follow the profile's agents**, and act on them.

There are two ways to build one:

- **On amux's Rust crates**, which do most of that for you. This is how
  amux's own clients are built ([below](#building-on-amuxs-rust-crates)).
- **From the protocol**, in any language, with the messages and services in
  [`proto/amux/v1`](../proto/amux/v1).
  [The protocol, step by step](#the-protocol-step-by-step) explains each of
  the five; a client on the Rust crates reads it for what the calls mean.

The protocol is stable from 0.9.0: fields are added and never renumbered
([README](../proto/amux/v1/README.md)). Before 0.9.0 it changes from
release to release, so a client built against one release talks to that
release's daemon; `GetInstallation` says which one is running.

## Building on amux's Rust crates

| Crate | What it does for you |
| --- | --- |
| `installer` | Steps 1 and 2. Finds the installation and its front door; `ensure` starts amux, or installs it, checking the download against the release's signed manifest. It touches nothing already running, so a client calls it at every launch. |
| `client` | Typed calls to the front door (`GrpcInstallation`) and to each profile (`Client`), with every socket detail below handled. |
| `ui-runtime` | The drivers: `Fleet` follows a profile's agents and `Session` one chat. They hold the streams, reconnect when amux restarts and start over when they fall behind. |
| `ui-state`, `ui-view` | Optional. `ui-state` is the drivers' model; `ui-view` turns it into what a screen shows (the fleet's sections and rows, a chat's rows and ask cards) as plain values, and leaves wording, layout and theme to the client. The terminal client and the iPhone app draw these. |

Take as many as the client wants. `installer` stands apart. A client with
its own model of the fleet takes `client` alone and reads
`SubscribeInventory` itself; `ui-runtime` on top of it handles the streams;
`ui-view` adds the ready-made rows.

The crates are not published to crates.io. Depend on them from Git, pinned
to the tag of the amux release whose daemon the client talks to:

```toml
[dependencies]
client = { git = "https://github.com/amuxhq/amux", tag = "vX.Y.Z" }
installer = { git = "https://github.com/amuxhq/amux", tag = "vX.Y.Z" }
ui-runtime = { git = "https://github.com/amuxhq/amux", tag = "vX.Y.Z" }
ui-state = { git = "https://github.com/amuxhq/amux", tag = "vX.Y.Z" }
ui-view = { git = "https://github.com/amuxhq/amux", tag = "vX.Y.Z" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Unlike the protocol, the crates promise nothing, before 0.9.0 or after:
their APIs change whenever amux's own clients need them to, so moving to a
newer tag can mean changing code. They are under the same licence as the
rest of amux ([LICENSING](LICENSING.md)).

### Example: the fleet, live

A whole client in one file. It makes sure amux is running, opens the
installation's first profile, and prints its agents again each time they
change.

<!-- source: crates/ui-runtime/examples/fleet.rs -->
```rust
//! A whole amux client in one file: it makes sure amux is running, opens
//! the installation's first profile and prints its fleet, again each time
//! the fleet changes. docs/WRITING_A_CLIENT.md walks through it.
//!
//! cargo run -p ui-runtime --example fleet

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use client::{Clock, GrpcInstallation, Installation, SystemClock};
use ui_runtime::Fleet;
use ui_view::{FleetView, SecondLine};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Find amux, and install or start it. An installation named by
    // AMUX_CONFIG is a development or test one: attach to it, never start it.
    let config = std::env::var_os("AMUX_CONFIG").map(PathBuf::from);
    let front_door = installer::front_door(config.as_deref())?;
    if config.is_none() {
        // A client with a person in front of it asks them before adding a
        // login item; the default adds one.
        let options = installer::Options::default();
        installer::ensure(&options, |step| eprintln!("{step:?}")).await?;
    } else if !installer::answering(&front_door).await {
        return Err(format!("amux is not running at {}", front_door.display()).into());
    }

    // 2. Pick a profile and open its services.
    let installation = GrpcInstallation::connect(&front_door).await?;
    let profiles = installation.list_profiles().await?;
    let profile = profiles.first().ok_or("the installation has no profiles")?;
    let client = installation.open_profile(profile).await?;
    // Which host is this machine: the views say whether each agent's host
    // can be reached from here.
    let local_host = client.get_device_identity().await?.host_id;

    // 3. Follow the fleet. The driver keeps the stream open, reconnects
    // when amux restarts and starts over when it falls behind.
    let fleet = Fleet::connect(client, SystemClock).await?;
    let mut changed = fleet.changed();
    let mut shown = None;
    loop {
        // A change the list does not show (an agent's progress within a
        // step) wakes this too; print only what looks different.
        let now = view(&fleet, &local_host);
        if shown.as_ref() != Some(&now) {
            print(&now);
            shown = Some(now);
        }
        if let Some(error) = fleet.ended() {
            return Err(error.into());
        }
        changed.changed().await?;
    }
}

/// The fleet as amux's own clients list it: sections of families, each row
/// with the line under its name.
fn view(fleet: &Fleet, local_host: &[u8]) -> FleetView {
    let now_ms = SystemClock.now_ms();
    // Read each session before the fleet, never holding both at once.
    let lines: HashMap<_, _> = fleet
        .sessions()
        .into_iter()
        .map(|session| {
            let state = session.state();
            (
                ui_state::agent_key(state.agent()),
                ui_view::session_line(&state, now_ms),
            )
        })
        .collect();
    let state = fleet.state();
    let expanded = HashSet::new();
    ui_view::fleet_view(&state, local_host, &lines, &expanded, &|_| true)
}

fn print(view: &FleetView) {
    println!("\n{} need you, {} working", view.need_you, view.working);
    for section in &view.sections {
        println!("{:?}", section.kind);
        for row in &section.rows {
            let indent = 2 + 2 * row.depth as usize;
            let card = &row.card;
            let line = second_line(&row.second_line);
            println!("{:indent$}{} on {}: {line}", "", card.name, card.host);
        }
    }
}

fn second_line(line: &SecondLine) -> String {
    match line {
        SecondLine::Ask(ask) => format!("asks {:?}", ask.subject),
        SecondLine::Step(step) => step.step.clone().unwrap_or_else(|| "working".into()),
        SecondLine::LastSaid(said) => said.clone(),
        SecondLine::Stuck(reason) => format!("stuck: {reason:?}"),
        SecondLine::Exited(cause) => format!("exited: {cause:?}"),
        SecondLine::HostRevoked => "its host no longer trusts this machine".into(),
        SecondLine::Blank => String::new(),
    }
}
```

From a checkout, `cargo run -p ui-runtime --example fleet` runs it. With
`AMUX_CONFIG` naming a development installation it only attaches;
without, it acts on the person's own installation, and installs amux if it
is missing.

What it leaves out, and where to find it:

- **Choosing a profile.** It takes the first. A real client lets the person
  choose, and follows `subscribe_profiles` so the list stays current.
- **Signing in.** Listing agents needs no account. `Installation::sign_in`
  streams the sign-in ([step 4](#4-sign-in)).
- **Opening a chat.** `Fleet::open` gives the agent's `Session`, and
  `ui_view::chat_rows_for` turns its state into the chat's rows. Pair every
  open with `Fleet::close`.
- **Acting on agents.** `Fleet::stop`, and the `Client` calls
  (`create_agent`, `rename_agent`, `send_input` and the rest), which are
  the protocol's calls of the same names.

## The protocol, step by step

### Connecting

Generate stubs with `proto` as the include root, compiling
`proto/amux/v1/*.proto` together with the Google status protos vendored
beside them in `proto/google/rpc`: `link.proto` imports
`google/rpc/status.proto`, and the services import one another. A project
that already depends on a published copy of the Google protos uses that
instead. Request and reply types live beside their services; `Empty` is
`amux.v1.Empty`, in `records.proto`.

Every socket speaks gRPC over plaintext HTTP/2, with no TLS and no
credentials: only the person's own account can reach it.

**Set the HTTP/2 `:authority` to `localhost`.** gRPC libraries built on
gRPC's C core (Python, Ruby, PHP, C++) otherwise send the socket's path,
which is not a valid authority. The daemon then resets the stream
(`INTERNAL: Stream removed`, RST_STREAM error code 1), which looks like a
broken daemon rather than a client setting. In those libraries it is the
`grpc.default_authority` channel option.

### 1. Find the installation

An installation is one config file and the root directory it names. Its
front door is `<root>/amux.sock`.

- **The config file** is the one `AMUX_CONFIG` names, else
  `$XDG_CONFIG_HOME/amux/config.toml`, which is `~/.config/amux/config.toml`
  when `XDG_CONFIG_HOME` is unset, on macOS and Linux alike. A missing file
  means every default.
- **The root** is the config's top-level `root` key, else
  `$XDG_DATA_HOME/amux`, which is `~/.local/share/amux` when
  `XDG_DATA_HOME` is unset.

To ask whether amux is up, call `GetInstallation` on the front door with a
deadline of a few seconds; it answers the daemon's version. A missing
socket, a refused connection or no answer means amux is not running.

Details:

- `root` is an absolute path, taken as written: no `~` and no variables.
- A Unix socket address holds at most 103 bytes of path on macOS and 107 on
  Linux. When `<root>/amux.sock` is longer, connect through a shorter path
  to the same file, such as a symbolic link of the client's own to its
  directory, as amux itself makes under `$TMPDIR/amux-<uid>/`.
- On Windows, `<root>\amux.sock` is a small file holding the name of the
  named pipe the daemon listens on. Read it and connect to that pipe.
- Every other amux socket, each profile's included, follows the same rules.

### 2. Install or start amux

amux lives at `~/.amux/bin/amux` (`%USERPROFILE%\.amux\bin\amux.exe` on
Windows) on every machine, whichever client put it there, and serves the
default installation. Whichever client arrives first installs it. From then
on amux starts at login on its own, and every client only attaches.
Removing a client leaves amux running; `amux uninstall` is the one way to
remove it.

- **Installed but not running** (the binary exists, the front door does not
  answer): run `~/.amux/bin/amux server start`, then try the front door
  every quarter second or so. amux's own clients give it 30 seconds.
- **Not installed:** install it, set it up and start it:

  ```sh
  curl -fsSL https://amux.sh/install | sh
  ~/.amux/bin/amux init --login-item yes
  ~/.amux/bin/amux server start
  ```

  On Windows the install script is PowerShell,
  `irm https://amux.sh/install | iex`; then `amux init` and
  `amux server start` as above.

Details:

- **An installation `AMUX_CONFIG` names** is a development or test one (a
  source checkout's, a test's) that may run another build. Attach to it
  when it answers; otherwise report it down, and never install or start
  anything for it. Every `amux` command reads `AMUX_CONFIG` too.
- **The login item.** `amux init --login-item yes` adds one (a LaunchAgent
  on macOS, a systemd user unit on Linux, a scheduled task on Windows) that
  starts amux at every login; `--login-item no` installs without one. Ask
  the person first: without a terminal, `amux init` asks nothing.
- **`PATH`.** An app opened from the desktop often has a sparse `PATH`, and
  `amux init` records the `PATH` the agents it starts will run with. Run it
  with the person's login-shell `PATH` (on macOS and Linux, what
  `$SHELL -ilc 'printf %s "$PATH"'` prints), or the agents will not find
  the tools they run.
- **Downloading amux yourself.** Read
  `https://amux.sh/releases/stable.json`, the stable channel's signed
  manifest, as the install script does. Each target
  (`aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`,
  `x86_64-pc-windows-msvc`) names a URL, a size and a SHA-256, and the
  manifest carries an Ed25519 signature over them. Check the download's
  size and SHA-256 against it.
- **Exit codes.** Every `amux` command exits 0 on success and 1 on failure,
  with the reason on standard error; 2 is a usage error.

### 3. Profiles

An installation holds one or more profiles. Each is one identity on the
network: an account it may be signed in to, the machines it is paired
with, its agents. A client shows one profile at a time and lets the person
switch.

On the front door (`InstallationService`), `ListProfiles` answers the
profiles now. `SubscribeProfiles` answers the whole list at once, and again
whenever anything about any profile changes; keep that stream open rather
than polling. `CreateProfile`, `RenameProfile` and `DeleteProfile` manage
them, and an installation always keeps at least one.

| `Profile` field | What it holds |
| --- | --- |
| `id` | A 16-byte UUID. Show it in the usual dashed form, which is also how it appears in `socket_path`. |
| `label` | The profile's name. |
| `email`, `account_name`, `picture_url` | The account's. `picture_url` is an https URL, empty for none. |
| `cloud` | Whether it is signed in (below). |
| `access` | Whether the account is on the paid plan. |
| `socket_path` | The profile's own socket ([step 5](#5-agents)). |

A profile is signed in when `cloud` holds `signed_in`, whatever its relay
link is doing. A link in `sign_in_again` means the account service refused
the profile's credentials, and the person signs in again. `signed_out` is a
profile tied to an account and signed out of it; `unbound` was never signed
in.

### 4. Sign in

Signing in binds a profile to an amux.sh account. The daemon does it, so a
client never sees a password or holds a token. `SignIn` runs the OAuth
device flow and streams what happens:

1. **A `code` event.** Show the person `user_code` and open
   `verification_uri_complete` in their browser, or show `verification_uri`
   and let them type the code. The code stops working at `expires_at_ms`.
2. **An `adopt` event**, only when the profile already holds agents or
   paired machines. Ask the person, then answer with `AnswerSignIn`, naming
   the event's `sign_in_id`. Setting `adopt_non_pristine` in the request
   skips the question.
3. **A `signed_in` event** with the profile, and the stream ends.

Leave `profile_id` empty to let the installation pick: the profile already
bound to this account, else one that holds nothing yet, else a new one.
Closing the stream abandons the sign-in.

An app that runs its own browser sign-in (authorization code with PKCE)
hands the refresh token it obtained to the daemon with `BindProfile`,
naming the OAuth client that obtained it; the daemon spends the token from
then on. Signing out is `SignOut` on the profile's own socket.

### 5. Agents

Everything inside a profile is on its own socket, `Profile.socket_path`:

- `AgentService`: the fleet, and acting on agents.
- `SessionService`: reading and writing a chat; the [wire](WIRE.md)
  describes its subscriptions, items and inputs.
- `ProfileAdminService`: the profile's account, pairing and peers.

`AgentService.SubscribeInventory` is the fleet. It sends every host the
profile knows and every agent on them, then `caught_up`, and from then on
each change as it happens:

- `agent` is one agent's whole row. Add it, or replace the one with the
  same `agent_id`, a UUID unique across the fleet. An agent's `name` is
  always set.
- `agent_removed` removes one.
- `host` and `host_removed` do the same for hosts.
- `lagged`, before or after `caught_up`, means the client fell behind. The
  stream closes; subscribe again, and replace what you held with the new
  listing.

Rows from a host this machine cannot reach right now are as that host last
reported them; the host's entry says whether it is reachable.

`CreateAgent`, `FileAgent`, `ResumeAgent`, `RenameAgent` and `DeleteAgent`
act on agents.

## Errors

Calls fail with ordinary gRPC status codes, the reason in the status
message, written for a person to read: `NOT_FOUND` for an id the daemon
does not know, `FAILED_PRECONDITION` for an agent name that matches more
than one agent. Where a client may want to act on the reason, the status
details carry a `google.rpc.ErrorInfo` whose `reason` is one of
`error.proto`'s `ErrorReason` names. `UNAVAILABLE` on the front door means
amux is not running.

No call is retried for a client. A mutation that lost its reply is the
person's to run again.

## Streams

The subscriptions (`SubscribeProfiles`, `SubscribeInventory`,
`SubscribeSession`) stay open for as long as the client wants them: give
them no deadline, and cancel the call to close one. They end when the
daemon stops or restarts, which an update does on its own. The client then
waits for the front door to answer again and subscribes afresh, as after a
`lagged` event. (The Rust drivers do this themselves.)
