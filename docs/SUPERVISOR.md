# Supervisor

*For developers working on how amux starts, stops, restarts and updates its daemon on a desktop machine.*

`amux supervise` is a small parent process for the daemon. It starts `amux
daemon` as its child, restarts it whenever it exits, and, when the install
allows it, installs new releases and rolls back a release that never comes up.
It also holds the machine's sleep assertion. Agents do not depend on it: every
agent is its own process and outlives any daemon (see [the agent
process](AGENT_PROCESS.md)), so the supervisor's job is to make sure a daemon
comes back to adopt them.

The code lives in [`crates/node/src/supervisor`](../crates/node/src/supervisor)
(the supervisor itself), [`crates/node/src/activation.rs`](../crates/node/src/activation.rs)
(the daemon's end of the pipe), [`crates/node/src/release.rs`](../crates/node/src/release.rs)
(manifests, signatures, rollout) and the CLI verbs in
[`crates/amux/src/supervise.rs`](../crates/amux/src/supervise.rs),
[`crates/amux/src/setup.rs`](../crates/amux/src/setup.rs) and
[`crates/amux/src/server.rs`](../crates/amux/src/server.rs).

## Two settings

Two values in the installation config decide everything here, and they are
independent:

| Key | Values | Default | Meaning |
| --- | --- | --- | --- |
| `supervisor` | `on`, `off` | `off` | Whether the daemon runs under `amux supervise`. The install decides this, not the person. |
| `updates` | `auto`, `manual` | `auto` with a supervisor, `manual` without | Whether the supervisor installs releases on its own hourly check. |
| `channel` | `stable`, `preview` | `stable` | Which release manifest the supervisor follows. |
| `keep_awake` | `on`, `off` | `on` | Whether the supervisor holds the sleep assertion. |
| `releases_url` | URL | `https://amux.sh/releases` | Where the channel manifests live. |

The four cells of `supervisor` × `updates`:

| | `updates: auto` | `updates: manual` |
| --- | --- | --- |
| `supervisor: on` | The desktop case: crash restarts and automatic updates. | Crash restarts; releases install only when someone runs `amux update`. |
| `supervisor: off` | Rejected when the config is read: only a supervisor installs releases. | A deployed host whose service manager runs `amux daemon` directly, such as the relay under systemd. Updates are deploys. |

Every behaviour on this page keys off `supervisor`, never off `updates`. The
daemon itself does not read either value to decide how it was started: it
knows it is supervised only because a supervisor handed it the activation pipe
(see [Activation](#activation)).

A desktop install is expected to write `supervisor: on`. This repository ships
no installer that writes it, so set it by hand in the installation config
(`~/.config/amux/config.yaml` by default). The settings live in
[`crates/settings/src/lib.rs`](../crates/settings/src/lib.rs) as
`InstallationConfig`.

## Who starts what

The service manager owns the supervisor, and the supervisor owns the daemon.
Nothing ever starts a daemon beside a supervisor.

- `amux server start` starts `amux supervise`, detached from the terminal,
  when the install has a supervisor, and returns once the daemon's front door
  answers. Without one it starts `amux daemon` directly.
- Any other CLI verb that finds no daemon on the front door starts the
  supervisor if the install has one and none is running, then waits for the
  daemon. Without a supervisor it starts nothing: the daemon belongs to
  whatever service manager runs it, and the CLI says so while it waits.
- `amux supervise` refuses to run when the config says `supervisor: off`.
- A second supervisor on the same data directory fails at once: the first
  holds `<data_dir>/supervisor.lock`, which also records its pid.

The supervisor logs to `<data_dir>/supervisor.log`; the daemon logs to
`<data_dir>/daemon.log` unless `AMUX_LOG` names another file. `<data_dir>` is
the config's `root`, `~/.local/share/amux` by default.

When the supervisor's core breaks one of its invariants it logs the reason
and writes `<data_dir>/reports/supervisor-<unix ms>-<id>.txt`: the reason, the
core's state when its window began, every input stepped since (at least the
last 256) and its state now, under a header naming this build. The file
holds what the log does (manifests, paths, exit codes, failures), no secret.
One is written per kind of reason every ten minutes. Nothing replays it; it
is what a step test of the supervisor core is written from by hand.

## Login items

`amux init` asks once, on a terminal, whether amux should start at login.
`amux init --login-item yes` or `--login-item no` answers without asking;
without a terminal and without the flag it asks nothing and adds nothing. On
yes it writes a login item that runs `amux supervise` and registers it with
the platform's service manager. An install with `supervisor: off` gets no
login item: its service manager already starts the daemon at boot.

Every login item follows the same three rules:

- **Start at login, restart on failure only.** A deliberate stop makes the
  supervisor exit cleanly, so it stays stopped until the next login. A crash
  brings it back, which is what keeps the machine on the fleet while nobody is
  at it.
- **Never take the agents down with the supervisor.** systemd's default stop
  kills every process in the unit's cgroup and launchd's kills the job's
  process group, agents and their providers included. The generated units opt
  out of both.
- **Carry the PATH `amux init` ran with.** Service managers start jobs with a
  sparse environment, and agents find their provider binaries through PATH.

What `amux init` installs on each platform
([`crates/node/src/supervisor/login.rs`](../crates/node/src/supervisor/login.rs),
registration in [`crates/amux/src/setup.rs`](../crates/amux/src/setup.rs)):

| Platform | File | Registered with | How it restarts, and how it spares agents |
| --- | --- | --- | --- |
| macOS | `~/Library/LaunchAgents/sh.amux.supervise.plist`, a LaunchAgent labelled `sh.amux.supervise` | `launchctl bootout` (ignored if not loaded), `launchctl bootstrap gui/<uid>`, then `launchctl kickstart` so it starts now rather than whenever launchd gets to it | `RunAtLoad`, `KeepAlive` with `SuccessfulExit` false; `AbandonProcessGroup` true |
| Linux | `$XDG_CONFIG_HOME/systemd/user/amux.service` (`~/.config/systemd/user/amux.service` by default), a systemd user unit | `systemctl --user daemon-reload`, then `systemctl --user enable --now amux.service` | `Restart=on-failure`, `WantedBy=default.target`; `KillMode=process` |
| Windows | `<data_dir>/amux-task.xml`, a Task Scheduler definition in UTF-16 with a byte-order mark | `schtasks /Create /F /TN amux /XML <file>`, then `schtasks /Run /TN amux` | A logon trigger, `RestartOnFailure` every minute up to 999 times, no execution time limit, and battery power neither blocks nor stops it |

Each item runs `<binary> [--config <path>] supervise`, where `<binary>` is the
running `amux` executable, and sends the supervisor's own output to
`<data_dir>/supervisor.log`. If a supervisor started by hand is already
running, `amux init` stops it first so the login item's supervisor owns the
install; agents keep running and the new daemon adopts them. The committed
goldens under
[`crates/amux/tests/goldens/login`](../crates/amux/tests/goldens/login) show the
exact text of all three files.

## Stop goes to the owner

A stop request reaches the process that owns the daemon, or the daemon would
come straight back.

- **With a supervisor**, `amux server stop` stops the supervisor. On Unix it
  sends SIGTERM to the pid recorded in `supervisor.lock`; Windows has no
  signals, so it writes `stop` to the supervisor's control socket,
  `<data_dir>/supervisor.sock`. The supervisor then stops its daemon — SIGTERM,
  up to the 30-second stop deadline, then a kill — and exits cleanly. The CLI
  waits up to 45 seconds for both locks to be released and prints
  `Stopped amux supervise and its daemon.`
- **Without a supervisor**, `amux server stop` asks the daemon to shut down
  over its front door and waits until the daemon has released the
  installation lock.

Either way agents keep running. Each sees its control socket close and waits
out its grace period (`agent.grace_secs`, five minutes by default) for the
next daemon to dial it.

Because the login items restart only on failure, the supervisor's clean exit
makes the stop stick until the next login. The same reasoning is why a unit
that runs `amux daemon` directly under a service manager should use
`Restart=on-failure` and never `always`: under `always` a signalled daemon is a
restarted daemon.

## Activation

One pair of anonymous pipes joins the supervisor to its daemon. The supervisor
names both ends to the child in `AMUX_SUPERVISOR_PIPE` (file descriptors on
Unix, handle values on Windows); the daemon takes them out of its environment
before anything else runs, so nothing it starts inherits them. The pipe carries
three things:

1. **`prepared`**, written by the daemon once it has taken the installation
   lock, migrated its stores and looked at every agent directory without
   writing to any of them.
2. **`go`**, the supervisor's answer. Only after `go` does the daemon finish
   its startup sweep, bind its sockets and serve.
3. **End of file**, meaning the supervisor is gone. The daemon shuts down
   cleanly, so whoever starts a supervisor next gets a fresh daemon under it.

A daemon with no pipe has no supervisor and goes straight on. Before `go`,
nothing the daemon has done needs undoing if the supervisor puts the previous
binary back: the only writes are the generation file and the store migration,
which from the first release on is additive. That is what makes activation the
rollback checkpoint. The
daemon's startup order is described in [Architecture](ARCHITECTURE.md).

## Restarts

The supervisor waits the way a parent waits: blocked on the child's exit, the
pipe, its control socket and an hourly timer. It costs nothing while the
daemon runs.

- A daemon that exits after `go` is restarted after a backoff that starts at
  1 second and doubles to 30 seconds. A daemon that stayed up for a minute
  starts the backoff over. A crash after `go` is never rolled back.
- A daemon that does not write `prepared` within the 60-second start deadline
  is stopped and counted as a failed start, like one that exits before
  `prepared`.
- Stopping a daemon is SIGTERM (on Windows, closing the pipe it reads `go`
  from), the 30-second stop deadline, then a kill.

These timings are `Params::default()` in
[`crates/node/src/supervisor/mod.rs`](../crates/node/src/supervisor/mod.rs);
they are compiled in and not read from the config. The rules above are
decided by one state machine,
[`crates/supervisor-core`](../crates/supervisor-core/src/lib.rs), which checks
after every step that an installed binary able to start always exists and
that at most one daemon serves, and asks for a dump if either fails or a
step takes longer than its 10 ms budget (`Params.step_budget`);
`node`'s supervisor module runs the processes, the pipe, the feed and the
files for it. [Parameters](PARAMETERS.md)
lists them with the rest.

## Updates and channels

Each channel is one manifest at `<releases_url>/<channel>.json`:
`https://amux.sh/releases/stable.json` or
`https://amux.sh/releases/preview.json` by default. [Release](RELEASE.md)
describes the manifest's format, how releases are signed, and what is and is
not set up for publishing them.

**When a check runs.** Under `updates: auto` the supervisor checks every hour.
`amux update` asks for a check now, over the control socket, under either
`updates` value. A check runs only while the daemon is activated and no update
is in progress; otherwise `amux update` is told to try again shortly. The
supervisor re-reads the config before every check, so `amux config channel
preview` (or `stable`) takes effect at the next check without a restart.

**What it installs.** The manifest names one build per target triple. The
supervisor installs it only when all of these hold:

- the version is newer than the running one; updates only move forward;
- it is not the build this machine rolled back (see below), unless the check
  came from `amux update`;
- the host is inside the manifest's `rollout` percentage, if it has one. A
  host's place is fixed: SHA-256 of its first profile's host id, mod 100,
  compared with the percentage. A host with no id yet waits for a full
  rollout.

Otherwise the check ends with the reason, which `amux update` prints: `Up to
date: 1.2.0 is running and the channel names 1.2.0.`, a rollout that has not
reached this host, a rolled-back build, or a channel with no build for this
target.

Because semver orders a prerelease below its release, a machine that moves
from `preview` to `stable` lands on the stable build of the same release as
soon as stable names a version above the one running.

**The swap.** The manifest's signature is verified against the key compiled
into the running binary, and its channel against the one followed, before
anything in it is read. A build to install is then downloaded to
`amux.staged` beside the binary, no more than its signed size, and its
length and SHA-256 compared with the manifest's. On Unix the current binary
is then hard-linked to `amux.prev`, so the install path is never missing, and
the staged file is renamed over the install path. Running processes keep the
file they started from: agents, tool servers and terminal clients are
untouched. The supervisor then stops its daemon and starts the new binary.

On Windows a running executable cannot be replaced, so the supervisor stops
the daemon first and renames the current binary aside to `amux.prev`.

![The agent never learns an update happened except by EOF on its control socket. A crash is the same timeline: the supervisor restarts the child either way, and after K starts that never activate it restarts the previous binary instead. A daemon that never returns ends at the grace timer: the agent drains and exits, its journal remainder is ingested by whichever daemon comes next, and the agent shows as exited (daemon lost) with one-tap resume.](figures/update-timeline.svg)

From everywhere else an update looks like a daemon restart. Agents see their
control socket close, start their grace timer, and the new daemon cancels it
when it dials them. Clients reconnect and catch up; an input sent during the
gap fails back to the client that sent it. A terminal client older than the daemon keeps working and says `amux 1.3.0 is
running · restart to update` on the fleet screen.

**`amux update`** prints `Installing amux <version>.`, then waits up to three
minutes for the new daemon to report that version and prints `amux <version>
is running.` Without a supervisor it prints that updates on this install are
deploys and does nothing. A build that trusts no release key answers every
check with `this build trusts no release key, so it installs nothing`.

## Rollback

`amux.prev` existing beside the binary is the whole record of an update in
progress.

- When the new daemon writes `prepared`, the supervisor deletes `amux.prev`,
  syncs the directory, and writes `go`. The update is done; from here on a
  crash is restarted, never rolled back.
- While `amux.prev` exists, every start that never activates — the daemon
  exits before `prepared`, or misses the start deadline — is counted. After
  **K = 3** such starts (`rollback_after` in `Params`), the supervisor writes
  the rejected build's version to `amux.rejected`, syncs it, then renames
  `amux.prev` back over the install path and starts it at once.
- The hourly check skips the version in `amux.rejected` until the channel
  names a different one. `amux update` ignores it and tries that build again.

The order of the last two writes matters. A crash between them leaves a
rejected version equal to the installed binary with `amux.prev` present, which
the next supervisor to start reads as "finish the rollback". The other order
could leave no record at all, and the next hourly check would reinstall the
build that just failed.

Counters and timers live in memory. A supervisor that starts and finds
`amux.prev` — after a reboot in the middle of an update, say — handles it
three ways:

| What it finds | What it does |
| --- | --- |
| `amux.prev` is the same file as the install path | The swap never happened: delete `amux.prev` and carry on. |
| `amux.rejected` names the version of the installed binary | A rollback was interrupted: put `amux.prev` back. |
| Anything else | An update is in progress: count failed starts against it as if this supervisor had done the swap. |

Rollback restores the binary only, and only before activation. There is no
store backup: the store the previous binary reopens holds nothing the new one
wrote except its migration, which from the first release on is additive,
transactional and stamped as a minimum version. A bad release that has already activated is fixed by a
corrective release with a higher version, never by moving back.

The supervisor needs write access to the directory holding the `amux` binary,
because every file above lives beside it. On Windows `amux.old` also appears
there briefly: a running supervisor cannot delete its own executable, so it
moves it aside and the next start removes it.

## The supervisor updates itself

After `go`, and last, a supervisor whose own executable is no longer the one at
the install path replaces itself with the installed binary. On Unix it execs
`amux supervise --inherit <state>` in place: same pid, and the child, both pipe
ends and the lock all cross the exec, so the daemon never notices. On Windows
it starts the new supervisor and exits; the new one waits up to ten seconds for
the lock. If the handover fails, the old supervisor keeps running, which is
still correct. Either way the supervisor is never the stale part for long, and
it never restarts the daemon to update itself.

## Keep-awake

Under `keep_awake: on`, the default, the supervisor holds a sleep assertion for
its whole lifetime, so a phone can start an agent on the machine while nobody
is at it, whether or not any agent is working. Neither the daemon nor any agent
holds one, so the assertion survives daemon restarts and the gap during an
update. An install without a supervisor holds none.

| Platform | What it takes |
| --- | --- |
| macOS | Two IOKit assertions named `amux keeps this machine reachable`: `PreventSystemSleep`, honoured on AC power only, and `PreventUserIdleSystemSleep`, honoured on battery too. `pmset -g assertions` shows both against the supervisor's pid. |
| Windows | `SetThreadExecutionState(ES_CONTINUOUS \| ES_SYSTEM_REQUIRED)` on a thread kept for the purpose. |
| Linux | Nothing. |

What this gives on a Mac is that the machine stays awake while it is **plugged
in with the lid open**. Whether the assertion holds a Mac awake with the lid
closed on AC power has not been measured. `pmset disablesleep` is the owner's
setting to make; amux never changes it. A failure to take an assertion is
logged and leaves the machine free to sleep, which is no worse than
`keep_awake: off`.

## Tests

| Where | What it shows |
| --- | --- |
| [`crates/node/tests/supervisor.rs`](../crates/node/tests/supervisor.rs) | The real supervisor driving a stand-in binary (`fake-amux`) whose daemon mode is scripted per version, against a local manifest server, with releases signed by the test key. Restart on every exit and stop on request; install a newer signed release and exec it keeping pid, child, pipe and lock; refuse a bad hash or signature; roll back after K starts that never prepare and skip the rejected build; stop a daemon that hangs before `prepared` at the start deadline; restart without rollback after `go`; kill at the stop deadline; the three recovery cases above; the real `amux daemon` exiting when its supervisor dies; rollout and version choice. |
| [`crates/amux/tests/supervise_cli.rs`](../crates/amux/tests/supervise_cli.rs) | The CLI around it, with the real binary: login units matching their goldens, `amux init` writing and registering one, stop going to the supervisor first or to the daemon when there is none, a client starting the supervisor only where the install has one, the sleep assertion under `keep_awake` (macOS), `amux config channel`, and `amux update` installing a build this machine had rolled back. |
| [`crates/amux/tests/overlap.rs`](../crates/amux/tests/overlap.rs) | Releases overlapping on one machine. Agents started by the previous build work on after an update; a terminal client of the previous build keeps working and names the newer daemon; the previous build reopens and works in a store the new build migrated before it was killed ahead of `prepared`; restarting the generated LaunchAgent under a running turn leaves the agent running. "The previous build" is a copy of the build under test re-stamped one version lower (`node::release::restamp`). |
| [`crates/amux/tests/process.rs`](../crates/amux/tests/process.rs) | A daemon killed mid-turn loses nothing: agents finish without it and the next daemon adopts them. |

`just journey system survive-daemon` tells the same story end to end: agents of
every kind finish their turns with amux killed, amux comes back under its
supervisor, a newer build is activated, and resuming loses nothing. All of
these suites are Unix-only; the Windows code paths are compiled on the Windows
CI runner but not exercised there. See [Testing](TESTING.md) for how to run
them.
