# Writing a client

*For anyone writing their own amux client — a desktop app, an editor
plugin, a script: how to find amux on a machine, install it when it is
missing, sign in, choose a profile and read its agents.*

amux runs as a daemon on each machine. Every client, the terminal one and
amux's own apps included, is a client of that daemon: it keeps no state of
its own and holds no account token. It talks gRPC to the daemon over local
sockets, with the messages and services in [`proto/amux/v1`](../proto/amux/v1),
which are stable from 0.9.0: fields are added and never renumbered
([README](../proto/amux/v1/README.md)). Before 0.9.0 they change from release
to release, so a client built from one release's protos talks to that
release's daemon; `GetInstallation` says which one is running.

## 0. Generate the stubs and connect

Compile with `proto` as the include root, together with the Google status
protos vendored beside amux's, since `link.proto` imports
`google/rpc/status.proto` and the services import one another:

```sh
mkdir -p gen
python -m grpc_tools.protoc -I proto --python_out=gen --grpc_python_out=gen \
    proto/amux/v1/*.proto proto/google/rpc/*.proto
```

(A project that already depends on a published copy of the Google protos,
such as Python's `googleapis-common-protos`, uses that instead of generating
them.) The generated modules import one another as `amux.v1...`, so `gen`
goes on the import path. Request and reply types live beside the services; `Empty` is
`amux.v1.Empty`, from `records.proto`.

Every socket speaks gRPC over plaintext HTTP/2: no TLS and no credentials,
since only the person's own account can reach the socket. Set the HTTP/2
`:authority` to `localhost`. gRPC libraries built on gRPC's C core (Python,
Ruby, PHP, C++) otherwise send the socket's path, which is not a valid
authority, and the daemon's HTTP/2 server resets the stream (`INTERNAL:
Stream removed`, RST_STREAM error code 1), which looks like a broken daemon
rather than a client setting. In Python:

```python
channel = grpc.insecure_channel(
    "unix:" + front_door, options=[("grpc.default_authority", "localhost")])
```

Whichever client arrives first installs amux. After that amux starts at
login on its own, and every client, including that one, only attaches.
Removing a client leaves amux running; `amux uninstall` is the one way to
remove amux.

## 1. Find the installation

An installation is one config file and the root directory it names.

- The config file is the one `AMUX_CONFIG` names when it is set, else
  `$XDG_CONFIG_HOME/amux/config.toml`, which is `~/.config/amux/config.toml`
  when `XDG_CONFIG_HOME` is unset, on macOS and Linux alike. A missing file
  means every default.
- The root is the config's top-level `root` key, an absolute path taken as
  written (no `~` or variables), else `$XDG_DATA_HOME/amux`, which is
  `~/.local/share/amux` when `XDG_DATA_HOME` is unset.
- The daemon's front door is `<root>/amux.sock`.

On macOS and Linux the front door is a Unix domain socket. A Unix socket
address holds at most 103 bytes of path on macOS and 107 on Linux; when
`<root>/amux.sock` is longer (an unusually deep root), connect through a
shorter path to the same file, such as a symbolic link of the client's own
to its directory, as amux itself makes under `$TMPDIR/amux-<uid>/`. On Windows `<root>\amux.sock`
is a small file whose contents are the name of the named pipe the daemon
listens on; read it and connect to that pipe. Every other amux socket
(each profile's, below) follows the same two rules.

A client asks the front door whether amux is up: `GetInstallation` answers
the daemon's version; give it a deadline of a few seconds, so a daemon that
is stuck reads as down. A socket that is missing or refuses the connection
means amux is not running.

## 2. Install or start amux

amux lives at `~/.amux/bin/amux` (`%USERPROFILE%\.amux\bin\amux.exe` on
Windows) on every machine, whichever client put it there. That binary
serves the default installation. An installation named by `AMUX_CONFIG` is
a development or test one (a source checkout's, a test's) that may run
another build; a client attaches to it when it answers and otherwise
reports it down rather than installing or starting anything. Every `amux` command reads
`AMUX_CONFIG` too, so a command run with it set acts on that installation.

- **Installed but not running** (the binary exists, the front door does
  not answer): run `~/.amux/bin/amux server start`, then try the front door
  every quarter second or so; amux's own clients give it 30 seconds.
- **Not installed:** install it, then set it up and start it:

  ```sh
  curl -fsSL https://amux.sh/install | sh
  ~/.amux/bin/amux init --login-item yes
  ~/.amux/bin/amux server start
  ```

  On Windows the script is PowerShell: `irm https://amux.sh/install | iex`,
  then `amux init` and `amux server start` as above.

  The install script reads `https://amux.sh/releases/stable.json`, the stable
  channel's signed manifest, and checks the download's size and SHA-256
  against it. A client that downloads the binary itself reads the same
  manifest: each target (`aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`,
  `x86_64-pc-windows-msvc`) names a URL, a size and a SHA-256, and the
  manifest carries an Ed25519 signature over them.

  `amux init` sets up the installation and, with `--login-item yes`, a login
  item (a LaunchAgent on macOS, a systemd user unit on Linux, a scheduled
  task on Windows) that starts amux at every login. `--login-item no`
  installs without one. Without a terminal `amux init` asks nothing.

Every `amux` command exits 0 on success and 1 on failure with the reason on
standard error; 2 is a usage error.

An app opened from the desktop often has a sparse `PATH`. `amux init` records
the `PATH` the agents it starts will run with, so run it with the person's
login-shell `PATH` (on macOS and Linux, what `$SHELL -ilc 'printf %s "$PATH"'`
prints), or the agents will not find the tools they run.

## 3. Profiles

An installation holds one or more profiles. Each profile is one identity on
the network: an account it may be signed in to, the machines it is paired
with, its agents. A client shows one profile at a time and lets the person
switch.

On the front door (`InstallationService`):

- `ListProfiles` answers the profiles now; `SubscribeProfiles` answers the
  whole list at once and again whenever anything about any profile changes.
  A client keeps that stream open rather than polling.
- Each `Profile` carries its `id`, a `label`, the account's `email`,
  `account_name` and `picture_url` (an https URL, empty for none), its
  `cloud` standing, its `access` (whether the account is on the paid plan),
  and its `socket_path`.
- `id` is a 16-byte UUID. Show it in the usual dashed form; that is also
  how it appears in `socket_path`.
- A profile is signed in when `cloud` holds `signed_in`, whatever its relay
  link is doing. A link in `sign_in_again` means the account service refused
  the profile's credentials: the person signs in again. `signed_out` is a
  profile tied to an account and signed out of it; `unbound` was never
  signed in.
- `CreateProfile`, `RenameProfile` and `DeleteProfile` manage them. An
  installation always keeps at least one.

## 4. Sign in

Signing in binds a profile to an amux.sh account. The daemon does it, so a
client never sees a password or holds a token. `SignIn` runs the OAuth
device flow and streams what happens:

1. A `code` event: show the person `user_code` and open
   `verification_uri_complete` in their browser (or show `verification_uri`
   and let them type the code). The code stops working at `expires_at_ms`.
2. An `adopt` event, only when the profile being signed in already holds
   agents or paired machines: ask the person, then answer with
   `AnswerSignIn` naming the event's `sign_in_id`. Setting
   `adopt_non_pristine` in the request skips the question.
3. A `signed_in` event with the profile, and the stream ends.

Leave `profile_id` empty to let the installation pick: the profile already
bound to this account, else one that holds nothing yet, else a new one.
Closing the stream abandons the sign-in.

An app that runs its own browser sign-in (authorization code with PKCE)
hands the refresh token it obtained to the daemon with `BindProfile`, naming
the OAuth client that obtained it; the daemon spends the token from then on.
Signing out is `SignOut` on the profile's own socket.

## 5. Agents

Everything inside a profile is on its socket, `Profile.socket_path`, a local
socket like the front door: `AgentService` for the fleet,
`SessionService` for a chat, `ProfileAdminService` for its account, pairing
and peers.

`AgentService.SubscribeInventory` is the fleet. It sends every host the
profile knows and every agent on them, then `caught_up`, and from then on
each change as it happens: an `agent` event is the agent's whole row (add or
replace it by `agent_id`, a UUID unique across the fleet), `agent_removed` removes one, and `host` and
`host_removed` do the same for hosts. A `lagged` event, before or after
`caught_up`, means the client fell behind: the stream closes and the client
subscribes again, replacing what it held with the new listing. An agent's
`name` is always set. Rows from a host this machine cannot reach right now
are as that host last reported them; the host's entry says whether it is
reachable.

`CreateAgent`, `StopAgent`, `ResumeAgent`, `RenameAgent` and `DeleteAgent`
act on agents. Reading and writing a chat is `SessionService`; the
[wire](WIRE.md) describes subscriptions, items and inputs.

## Errors

Calls fail with ordinary gRPC status codes, with the reason in the status
message, meant for a person to read: `NOT_FOUND` for an id the daemon does
not know, `FAILED_PRECONDITION` for an agent name that matches more than
one agent. Where a client may want to act on the reason, the status details
carry a `google.rpc.ErrorInfo` whose `reason` is one of `error.proto`'s
`ErrorReason` names. `UNAVAILABLE` on the front door means amux is not
running. No call is retried for a client; a mutation that lost its reply is
the person's to run again.

## Streams

The subscriptions (`SubscribeProfiles`, `SubscribeInventory`,
`SubscribeSession`) stay open for as long as the client wants them: give
them no deadline, and cancel the call to close one. They end when the daemon stops or restarts, which an
update does on its own; the client then waits for the front door to answer
again and subscribes afresh, as on a `lagged` event.
