# Supervisor

*For developers working on how amux starts, stops, restarts and updates its daemon on a desktop machine.*

`amux supervise` is a small parent process for the daemon. It starts `amux
daemon` as its child, restarts it whenever it exits or asks to be, and, when the install
allows it, installs new releases and rolls back a release that never comes up.
It also keeps the machine awake while it is on power. Agents do not depend on it: every
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
| `supervisor` | `true`, `false` | `true` | Whether the daemon runs under `amux supervise`. A host whose service manager runs `amux daemon` directly sets it `false`. |
| `updates` | `"auto"`, `"manual"` | `auto` with a supervisor, `manual` without | Whether the supervisor installs releases on its own hourly check. |
| `channel` | `"stable"`, `"preview"` | `stable` | Which release manifest the supervisor follows. |
| `keep_awake` | `true`, `false` | `true` | Whether amux keeps the machine awake: on power whenever it runs, on battery while an agent works. Read again as it changes. |
| `start_at_login` | `true`, `false` | `false` | Whether a login item starts the supervisor at login ([login items](#login-items)). |
| `releases_url` | URL | `https://amux.sh/releases` | Where the channel manifests live. |

The four cells of `supervisor` × `updates`:

| | `updates = "auto"` | `updates = "manual"` |
| --- | --- | --- |
| `supervisor = true` | The desktop case: crash restarts and automatic updates. | Crash restarts; releases install only when someone runs `amux update`. |
| `supervisor = false` | Rejected when the config is read: only a supervisor installs releases. | A deployed host whose service manager runs `amux daemon` directly, such as the relay under systemd. Updates are deploys. |

Every behaviour on this page keys off `supervisor`, never off `updates`. The
daemon itself does not read either value to decide how it was started: it
knows it is supervised only because a supervisor handed it the activation pipe
(see [Activation](#activation)).

A desktop install needs no config file at all: the defaults are a supervisor
with automatic updates. A host that runs `amux daemon` under its own service
manager writes `supervisor = false` in the installation config
(`~/.config/amux/config.toml` by default). The settings live in
[`crates/settings/src/lib.rs`](../crates/settings/src/lib.rs) as
`InstallationConfig`.

## Who starts what

The service manager owns the supervisor, and the supervisor owns the daemon.
Nothing ever starts a daemon beside a supervisor.

- `amux server start` starts `amux supervise`, detached from the terminal,
  when the install has a supervisor, and returns once the daemon's front door
  answers. Without one it starts `amux daemon` directly. What it leaves
  running holds none of its standard streams, so a caller reading its output
  to the end (`amux server start | cat`, a desktop app's installer) sees the
  end when `start` exits: every `amux` first makes its own standard handles
  uninheritable (`os::process::harden_start`; on Windows a child otherwise
  inherits every inheritable handle, whatever its `Stdio` says), and the
  supervisor, daemon and agent spawns name all three streams and every
  other handle the child gets (`os::process::Given`). On Windows that is an
  explicit list (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), so a handle this
  process was handed without knowing it, such as a desktop app's own pipe,
  goes no further. Each of the
  three starts through `os::process::background`: in its own session (an
  agent in its own group), in the root (an agent in its folder) and, on
  Windows, with a console that has no window, so none ever shows one; the
  supervisor and the daemon log `console window=false` as they start. A job that forbids leaving it
  keeps the process in it, which the starter logs.
- Any other CLI verb that finds no daemon on the front door starts the
  supervisor if the install has one and none is running, then waits for the
  daemon. Without a supervisor it starts nothing: the daemon belongs to
  whatever service manager runs it, and the CLI says so while it waits.
- `amux supervise` refuses to run when the config says `supervisor = false`.
- A second supervisor on the same data directory fails at once: the first
  holds `<data_dir>/supervisor.lock`, which also records its pid, readable
  while the lock is held (`amux status` and the second supervisor's refusal
  name it). On Windows the lock is one byte past the file's data, since a
  Windows lock is mandatory over the bytes it covers.

The supervisor logs to `<data_dir>/supervisor.log`; the daemon logs to
`<data_dir>/daemon.log` unless `AMUX_LOG` names another file. `<data_dir>` is
the config's `root`, `~/.local/share/amux` by default. Each moves its own
standard error onto its log at start, so a panic lands there, and each log is
capped at 8 MiB with one previous file beside it (see
[Debugging](DEBUGGING.md#where-things-are)). A report carries the minute of
both before it, at INFO and above.

When the supervisor's core breaks one of its invariants it logs the reason
and writes `<data_dir>/reports/supervisor-<unix ms>-<id>.json`: the reason,
the core's state when its window began, every input stepped since (at least
the last 256) and its state now, as JSON, with the build that wrote it. The
file holds what the log does (manifests, paths, exit codes, failures), no
secret. One is written per kind of reason every ten minutes. `just repro`
replays it: the core from the window's start through every input, naming the
first invariant broken (see
[Debugging](DEBUGGING.md#reproducing-a-report)).

## Login items

Whether amux starts at login is the `start_at_login` setting, off until a
person turns it on: in a client's onboarding or settings, or with
`amux config set start_at_login true`. Turning it on writes a login item that
runs `amux supervise` for the next login; turning it off removes this
installation's item. Neither touches the amux running now: registering the
item at once would start a second supervisor, which finds the first one's
lock and exits, and a service manager told to restart failures would start it
again and again. Until the next login the running supervisor keeps the
machine reachable, and `amux server stop` stops it as before. An install with
`supervisor = false` gets no login item: its service manager already starts
the daemon at boot. `amux status` says whether amux starts at login.

Every login item follows the same three rules:

- **Start at login, restart on failure only.** A deliberate stop makes the
  supervisor exit cleanly, so it stays stopped until the next login. A crash
  brings it back, which is what keeps the machine on the fleet while nobody is
  at it.
- **Never take the agents down with the supervisor.** systemd's default stop
  kills every process in the unit's cgroup and launchd's kills the job's
  process group, agents and their providers included. The generated units opt
  out of both.
- **Carry the PATH of whoever turned it on.** Service managers start jobs
  with a sparse environment, and agents find their provider binaries through
  PATH: the item records the PATH of the daemon or the command that wrote it.
  Windows starts the `Run` key's values in the person's session with their own
  environment, so the item there carries none.

What the item is on each platform
([`crates/node/src/supervisor/login/`](../crates/node/src/supervisor/login/mod.rs), a
module per platform):

| Platform | Item | Registered with | How it restarts, and how it spares agents |
| --- | --- | --- | --- |
| macOS | `~/Library/LaunchAgents/sh.amux.supervise.plist`, a LaunchAgent labelled `sh.amux.supervise` | Nothing: launchd loads every LaunchAgent at login | `RunAtLoad`, `KeepAlive` with `SuccessfulExit` false; `AbandonProcessGroup` true |
| Linux | `$XDG_CONFIG_HOME/systemd/user/amux.service` (`~/.config/systemd/user/amux.service` by default), a systemd user unit | `systemctl --user daemon-reload`, then `systemctl --user enable amux.service`; off is `disable` | `Restart=on-failure`, `WantedBy=default.target`; `KillMode=process` |
| Windows | A string value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`: `amux` for the default root, `amux-<8 hex>` (the first four bytes of the SHA-256 of the root's path, lowercased) for any other | Nothing: Windows starts every `Run` value at logon; off deletes the value | `amux-launch.exe`, which has no console, starts `amux supervise` and restarts it with backoff unless it stopped on purpose. Windows starts it in explorer's job, which neither kills on close nor refuses breakaway |

On macOS and Linux each item runs `<binary> [--config <path>] supervise`, where
`<binary>` is the `amux` executable that wrote it, and sends the supervisor's
own output to `<data_dir>/supervisor.log`. An item is this installation's when
it names this installation's `supervisor.log`, so turning the setting off in a
worktree's installation leaves the real one starting at login.

On Windows the value is `"<bin>\amux-launch.exe" supervise [--config
"<path>"]`, the launcher beside the `amux.exe` that wrote it; a build with no
launcher there names `amux.exe` itself, which starts the supervisor in a
console window and without the restart. The supervisor writes its own output
to `supervisor.log`. The value is this installation's by its name, so two
roots never overwrite each other and a worktree's never touches the real one.
Windows starts no `Run` command line longer than 260 characters, so a longer
one (a deep config or install path) is written with each path by its 8.3
short names (`os::fs::short_path`), and only one still too long, on a volume
keeping no short names, is refused with an error saying so, with nothing
written.

`amux uninstall` removes it at once: it unregisters the login item
(`launchctl bootout`, `systemctl --user disable --now`, or deleting the `Run`
value) and deletes its file, again only when it is this installation's.
It stops this
machine's agents first and amux after, so nothing is left running without
its binary; the supervisor's deliberate stop ends the launcher that started
it too. A launcher waiting out a backoff when the uninstall runs has no
supervisor to hear the stop: it finds `amux.exe` gone at its next attempt
and exits, and its image, still running when the uninstall deletes the
programs, is moved aside like any other (below).

For the default installation it then deletes, beside `amux` and
`amux-launch`, everything their updates leave
([`Files::beside`](../crates/node/src/supervisor/files.rs)): `.prev`,
`.rejected`, both `.staged` downloads and every `.old.<n>`, and takes `~/.amux/bin`
off PATH (the install script's lines in the shell profiles; on Windows the
folder's entry in `HKCU\Environment`'s `Path`, every other entry kept as
written, then `WM_SETTINGCHANGE`). Windows deletes no program a process is
running, and the uninstalling `amux.exe` is one: a file still held after a
few seconds is renamed to a fresh `amux.exe.old.<n>`, and the command says
so. That is the one thing an uninstall leaves.

## The launcher (Windows)

`amux.exe` is a console program, so anything that starts it at logon
without a console of its own puts a window on the desktop. Windows' login
item therefore runs `amux-launch.exe`, shipped beside `amux.exe`, built for
the GUI subsystem so it never has a console. `amux-launch <args>` runs
`amux <args>` from its own folder in the background with no window
(`os::process::background`), so the item is `amux-launch supervise
--config <path>`. It is the restart policy, as `KeepAlive` and
`Restart=on-failure` are for launchd and systemd; only the login item uses
it, since `amux server start` starts the supervisor itself and a start from
the CLI gets no crash restart on any OS.

It knows no pid, reads no exit code and takes no lock
([`crates/launch`](../crates/launch/src/main.rs)). It starts amux holding
the write end of a pipe, named as a handle value in `AMUX_LAUNCH_LINEAGE`,
and reads the other end:

- **A byte, then the end:** amux stopped on purpose. `amux supervise`
  writes `0` before any deliberate exit (`amux server stop`, the OS asking
  it to stop), and the launcher exits.
- **The end with nothing written:** the line died (a crash, a kill, a
  supervisor that could not start). The launcher starts amux again after a
  backoff of 1 second doubling to 60, back to 1 after a line that ran 10
  minutes. A start that fails, say while an update has the binary moved
  aside, counts as a death too. If no `amux.exe` is left beside the
  launcher when the backoff ends (an uninstall removed it while the
  launcher waited), there is nothing to start and no supervisor will ever
  end the line, so the launcher exits.

A supervisor the launcher starts that finds another supervisor holding the
root's lock (`amux server start` ran before the login item did, or while
the launcher waited) writes `0` before it exits with its error: that
supervisor owns the root, and a launcher left waiting would start amux
again after its `amux server stop`, which found a supervisor to tell and
so recorded nothing. A successor started at an update that finds the lock
held leaves the line alone, since the supervisor it replaces still holds
it. A direct `amux supervise` has no line and just fails.

The supervisor holds the pipe for its whole life and never passes it to
the daemon. At its own update it hands the pipe to its successor with the
child and the activation pipe ([below](#the-supervisor-updates-itself)), so
the pipe stays open across the handover and the launcher sees nothing end.

A stop while the launcher waits out a backoff finds no supervisor to tell.
`amux server stop`, finding none where the install has one, writes
`<data_dir>/supervisor.stopped`. A supervisor it then finds holding the
lock started meanwhile, and may be one that read the file and exits
without ever opening its socket: the stop waits a few seconds for the lock
to go (stopped) or the socket to take the stop, and only then asks as for
any supervisor. Every start after the launcher's first carries
`AMUX_LAUNCH_RESTART=1`; a supervisor started so that finds the file writes
`0`, then removes the file (dying in between leaves the stop for the next
restart), and exits before it starts a daemon, and the launcher exits. Any
other start (the login's, `amux server start`) removes
the file and runs: a deliberate start undoes the stop. The launcher can't
read the file itself, since it knows no root, only the arguments it passes
on; and amux, not the launcher, deciding keeps the launcher's contract to
one more variable. A named event was the other way, but `server stop`
would have needed the launcher's name for it from a file in the root
anyway, and the launcher would have needed a wait it can be woken from.

The command line, `AMUX_LAUNCH_LINEAGE` and `AMUX_LAUNCH_RESTART` are
promised from the first release that ships the launcher: a login item
written by one release starts whichever release is installed later, and a
running launcher meets the supervisors of later releases.

## Stop goes to the owner

A stop request reaches the process that owns the daemon, or the daemon would
come straight back.

- **With a supervisor**, `amux server stop` stops the supervisor. It writes
  `stop` to the supervisor's control socket, `<data_dir>/supervisor.sock`, on
  every OS; where that fails and the OS has signals, it sends SIGTERM to the
  pid recorded in `supervisor.lock`. The supervisor then stops its daemon (on
  Unix SIGTERM, on Windows, which has no signals, closing the activation
  pipe), up to the 30-second stop deadline, then a kill, and exits cleanly.
  The CLI waits up to 45 seconds for both locks to be released and prints
  `Stopped amux supervise and its daemon.`
- **Without a supervisor**, `amux server stop` asks the daemon to shut down
  over its front door and waits until the daemon has released the
  installation lock.

The OS asking amux to stop is heard the same way by the supervisor and the
daemon (`os::signal::terminated`): SIGTERM, SIGINT or SIGHUP on Unix; on
Windows Ctrl+C, the console closing, logoff and shutdown. Each shuts down
cleanly, the supervisor stopping its daemon first, and only then lets the OS
end it (`os::signal::finished`), so a reboot leaves the clean-shutdown flag
set and the generation unchanged.

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
   lock, opened its stores and looked at every agent directory without
   writing to any of them.
2. **`go`**, the supervisor's answer. Only after `go` does the daemon finish
   its startup sweep, bind its sockets and serve.
3. **End of file**, meaning the supervisor is gone. The daemon shuts down
   cleanly, so whoever starts a supervisor next gets a fresh daemon under it.

A daemon with no pipe has no supervisor and goes straight on. Before `go`,
nothing the daemon has done needs undoing if the supervisor puts the previous
binary back: the only writes are the generation file and opening each
profile's store, which creates it or runs the migrations it lacks, and
migrations only add, so the previous binary still reads it. That is what
makes activation the rollback checkpoint. The
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

## Restarting to apply an experiment switch

The daemon reads which experiments are on once, as it starts, and restarts
itself when they are switched ([experiments](WIRE.md#experiments) says when
and what it waits for). The restart takes the path a daemon start already
has, in each install cell:

- **With a supervisor**, the daemon writes `restart` to the control socket.
  The supervisor's core stops the serving daemon as it does onto an update
  (SIGTERM, or closing the pipe on Windows, then the stop deadline and a
  kill) and starts the same binary at once once it has exited, with no
  backoff and nothing beside the binary touched; the supervisor itself keeps
  running. A daemon not yet serving is left to the start it is making. A
  supervisor from before `restart` answers `unknown request "restart"`, and
  so does nothing listening: the daemon then shuts down and exits by itself,
  and the supervisor starts it again after its backoff. So does a daemon
  the supervisor has not stopped within ten seconds of answering.
- **Without a supervisor**, the daemon shuts down cleanly, which releases
  the installation lock and closes its sockets, and on Unix execs its own
  executable with the same arguments and environment: the same process, so
  a service manager running `amux daemon` sees nothing happen. Every
  descriptor it holds closes on exec, and the new image takes the lock and
  binds the front door as any start does. The agents the old image started
  are still the process's children across the exec, with nothing in the new
  image waiting for them, so a daemon reaps, as each exits, every child it
  finds it already has as it starts; one started any other way has none.
  Windows has no exec, and the nearest honest equivalent is a fresh
  process: the daemon starts its own executable with the same arguments and
  environment, in the background with no window, and exits. Its process id changes, so a service
  manager watching the first process sees it exit cleanly and no longer
  tracks the one that took over.

Neither happens while the config would not read at a start (a named file
that is gone counts, since a start refuses it), because the next start would
fail; the quiet before a restart starts over once it reads again.

## Updates and channels

Each channel is one manifest at `<releases_url>/<channel>.json`:
`https://amux.sh/releases/stable.json` or
`https://amux.sh/releases/preview.json` by default. [Release](RELEASE.md)
describes the manifest's format, how releases are signed, and what is and is
not set up for publishing them.

**When a check runs.** Under `updates = "auto"` the supervisor checks every hour.
`amux update` asks for a check now, over the control socket, under either
`updates` value. A check runs only while the daemon is activated and no update
is in progress; otherwise `amux update` is told to try again shortly. The
supervisor re-reads the config before every check, so `amux config set
channel preview` (or `stable`) takes effect at the next check without a restart.

**What it installs.** The manifest names one build per target triple. The
supervisor installs it only when all of these hold:

- the version is newer than the running one; updates only move forward;
- it is not the build this machine rolled back (see below), unless the check
  came from `amux update --retry`;
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
`amux.staged.part` beside the binary, no more than its signed size, its
length and SHA-256 compared with the manifest's, and only then renamed to
`amux.staged`: a staged file is always a verified one. On Unix the current binary
is then hard-linked to `amux.prev`, so the install path is never missing, and
the staged file is renamed over the install path. Running processes keep the
file they started from: agents, tool servers and terminal clients are
untouched. The supervisor then stops its daemon and starts the new binary.

On Windows a running executable cannot be replaced, so the supervisor stops
the daemon first and renames the current binary aside to `amux.prev`.

On Windows the release's launcher, `amux-launch.exe`, comes with it: the
manifest names it beside the Windows binary
([Release](RELEASE.md#the-manifest)), the supervisor downloads and verifies
it with the binary (a bad one fails the update), and puts it beside
`amux.exe` in the same swap, so the two stay from one release. A launcher
running as the login item keeps its old image, which is harmless: its
command line and its pipe are all it shares with amux, and both are
promised. A launcher that fails to go in leaves the one there, and the
next release tries again; a rollback puts back `amux.exe` alone. An update
stopped between moving the launcher aside and renaming the staged one in
(the supervisor killed, the power cut) leaves the login item naming no
file, so every supervisor start, before it clears downloads or sweeps,
puts a launcher back where there is none: the staged one if there is one,
else, or when Windows holds it so it won't move, the one moved aside last
(the newest by its time, which a rename keeps). While the path stays empty
no start or sweep removes the staged launcher or the launcher's asides,
so the next start tries again with them.

![The agent never learns an update happened except by EOF on its control socket. A crash is the same timeline: the supervisor restarts the child either way, and after K starts that never activate it restarts the previous binary instead. A daemon that never returns ends at the grace timer: the agent drains and exits, its journal remainder is ingested by whichever daemon comes next, and the agent shows as exited (daemon lost) with one-tap resume.](figures/update-timeline.svg)

From everywhere else an update looks like a daemon restart. Agents see their
control socket close, start their grace timer, and the new daemon cancels it
when it dials them. Clients reconnect and catch up; an input sent during the
gap fails back to the client that sent it. A terminal client older than the daemon keeps working and says `amux 1.3.0 is
running · restart to update` on the fleet screen.

**`amux update`** prints `Installing amux <version>.`, then waits up to three
minutes for the new daemon to report that version and prints `amux <version>
is running.` If the supervisor rolls the build back instead, which `amux
update` sees as `amux.rejected` beside its own binary changing to name that
version, it fails saying the build did not start, which version the
supervisor put back, where the log is, and that `amux update --retry` tries
it again. Without a supervisor it prints that updates on this install are
deploys and does nothing. A build that trusts no release key for its feed
answers every check with `this build trusts no release key for its feed, so
it installs nothing`: a release build made by hand, and a debug build on any
feed but a test's own on the loopback.

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
  names a different one, and so does `amux update`, which says the build was
  rolled back here. `amux update --retry` tries that build again.

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
store backup: the new binary's migrations only add to the store, so the
previous binary reads and writes it as the new one left it. A bad release that has already activated is fixed by a
corrective release with a higher version, never by moving back.

The supervisor needs write access to the directory holding the `amux` binary,
because every file above lives beside it, with the `.old.<n>` names on
Windows ([Binaries in use](#binaries-in-use-windows)).

## Binaries in use (Windows)

Windows keeps the file a process started from mapped until it exits, and
refuses to delete a mapped file or rename another over it; it can only be
renamed away. Running agents and tool servers keep the image of the build
that started them, so after two updates three builds may be running from
three files. Wherever an update on Unix would delete a binary or rename
over one, the Windows supervisor renames it aside to a name of its own,
`amux.exe.old.<n>` (the first `n` free, never over an existing file), and
deletes it there if nothing runs it:

| Step | Unix | Windows |
| --- | --- | --- |
| The swap | hard-link the binary to `amux.prev`, rename the staged file over the path | rename the binary to `amux.prev`, then the staged file to the free path (put back if that fails) |
| Activation | delete `amux.prev` | rename `amux.prev` aside |
| Rollback | rename `amux.prev` over the path | rename the failed build aside, then `amux.prev` to the free path |
| The launcher | rename the staged launcher over it | rename the running launcher aside (`amux-launch.exe.old.<n>`), then the staged one to the free path |

Every supervisor start and every update check sweeps the `.old.<n>` names
beside the binary with `os::fs::remove_patiently`, and leaves what is still
mapped for the next sweep, so a build's file goes within the hour after its
last agent does. No sweep ever stops the supervisor. Renames wait out a
virus scanner holding a fresh file for a moment (`rename_patiently`).
`.prev`, `.rejected` and `.staged` keep their names and meaning everywhere,
since the next release's supervisor reads them.

## The supervisor updates itself

After `go`, and last, a supervisor whose own executable is no longer the one at
the install path replaces itself with the installed binary. On Unix it execs
`amux supervise --inherit <state>` in place: same pid, and the child, both pipe
ends and the lock all cross the exec, so the daemon never notices. On Windows
it starts the new supervisor and exits. The child is handed over as an
inheritable process handle beside the pipe's ends, never as a pid the new
supervisor would open, which a process started meanwhile could have taken;
the lock goes with the process that took it, so the new one waits up to ten
seconds to take it again. The launcher's pipe, when one started the line,
is handed over the same way, named in the successor's `AMUX_LAUNCH_LINEAGE`,
so the line goes on with a new pid. If the handover fails, the old supervisor keeps running, which is
still correct. Either way the supervisor is never the stale part for long, and
it never restarts the daemon to update itself.

## Keep-awake

Under `keep_awake = true`, the default, the machine stays awake on power
whenever amux runs, so a phone can start an agent on it while nobody is at
it, and on battery only while one of its agents is working, so a laptop
unplugged and idle is not drained by amux. The setting takes effect when it
changes, through a client's settings or `amux config`, with nothing
restarted.

The two halves have two holders. The supervisor holds the one for power for
its whole lifetime, so it survives daemon restarts and the gap during an
update; it reads `keep_awake` again every two seconds. The daemon holds the
one battery honours while any profile has one of its own agents working,
which it learns from each profile's own inventory, and drops it as the last
turn ends or the setting goes off. An install without a supervisor holds
neither.

| Platform | Supervisor (on power) | Daemon (while an agent works) |
| --- | --- | --- |
| macOS | The IOKit assertion `PreventSystemSleep`, which the system honours on AC power only. | `PreventUserIdleSystemSleep`, honoured on battery too. |
| Windows | `SetThreadExecutionState(ES_CONTINUOUS \| ES_SYSTEM_REQUIRED)` on a thread kept for the purpose, set while the machine reports AC power or an unknown power status (255, as a desktop or a VM without a battery may report; looked at every 30 seconds) and cleared on battery. | The same state on a thread of the daemon's, while an agent works. |
| Linux | Nothing. | Nothing. |

Both are named `amux keeps this machine reachable`; `pmset -g assertions`
shows the supervisor's against its pid, and the daemon's against its pid
while an agent works. What this gives on a Mac is that the machine stays
awake while it is **plugged in with the lid open**. Whether the assertion
holds a Mac awake with the lid closed on AC power has not been measured.
`pmset disablesleep` is the owner's setting to make; amux never changes it.
A failure to take an assertion is logged and leaves the machine free to
sleep, which is no worse than `keep_awake = false`.

## Tests

| Where | What it shows |
| --- | --- |
| [`crates/supervisor-core/src/tests.rs`](../crates/supervisor-core/src/tests.rs) | The core's rules one input at a time; among them a restart the daemon asks for stopping it and starting it again at once, with no backoff and nothing moved, and nothing started when the supervisor is stopping. |
| [`crates/node/src/supervisor/files.rs`](../crates/node/src/supervisor/files.rs) | The files beside the binary: a failed swap leaving the binary at its path, rollback over a build held for a moment, each retired build moved aside to a name of its own and swept once nothing holds it, the staged launcher replacing the one there, a start clearing downloads (finished or not) and what was moved aside, and a start putting back the launcher an interrupted update left missing, the staged one or else (also when the staged one is held) the one moved aside last, and nothing that could fill the launcher's path swept while it's empty. |
| [`crates/node/tests/supervisor.rs`](../crates/node/tests/supervisor.rs) | The real supervisor driving a stand-in binary (`fake-amux`) whose daemon mode is scripted per version, against a local manifest server, with releases signed by the test key. Restart on every exit and stop on request; install a newer signed release and exec it keeping pid, child, pipe and lock (on Windows, a successor that takes the lock again); two updates in a row with a process of each earlier build still running from its image (on Windows each retired build moved aside to a name of its own and swept once nothing runs it); the release's launcher installed beside the binary while it runs; under `amux-launch`, a dead supervisor started again after a doubling backoff, the launcher's line kept open across the self-update, and the launcher ended by a deliberate stop; refuse a bad hash or signature; roll back after K starts that never prepare and skip the rejected build; stop a daemon that hangs before `prepared` at the start deadline; restart without rollback after `go`; kill at the stop deadline; the three recovery cases above; the real `amux daemon` exiting when its supervisor dies; rollout and version choice. |
| [`crates/amux/tests/supervise_cli.rs`](../crates/amux/tests/supervise_cli.rs) | The CLI around it, with the real binary: `start_at_login` writing the login item for the next login and removing it, and finding no supervisor to write one for, stop going to the supervisor first or to the daemon when there is none, and ending the launcher that started it, or one waiting out its backoff after the supervisor was killed, which then starts nothing, a client starting the supervisor only where the install has one, a client's experiment switch restarting the daemon through the supervisor with the agent mid-turn keeping its process, the supervisor's power assertion under `keep_awake`, let go and taken again as the setting changes (macOS), `amux config set channel`, and `amux update` reporting a build that never started and leaving it alone after, and `amux update --retry` installing a build this machine had rolled back. |
| [`crates/amux/tests/windows_install.rs`](../crates/amux/tests/windows_install.rs) | Windows only: the whole install run in a scratch machine. `install.ps1` installs amux and its launcher from a feed on the loopback, `server start` and `stop`, `start_at_login` writes the `Run` value and a simulated logon starts what it names, an agent on the fakes keeps its process and its turn through two updates from the feed and answers after, and `amux uninstall` leaves only a program a running process held, with the user's PATH as it was; no step shows a window of the run's, and every supervisor and daemon logs `console window=false`. The `Run` key and `HKCU\Environment` are scratch keys, and nothing runs that doesn't read them. |
| [`crates/node/tests/settings.rs`](../crates/node/tests/settings.rs) | The daemon's half of keep-awake: wanted while an agent works under `keep_awake`, not once the turn ends or the setting goes off. |
| [`crates/amux/tests/overlap.rs`](../crates/amux/tests/overlap.rs) | Releases overlapping on one machine. Agents started by the first build work on through two updates in a row and a daemon killed mid-turn; a terminal client of the previous build keeps working and names the newer daemon; the previous build reopens and works in a store the new build opened before it was killed ahead of `prepared`; restarting the generated LaunchAgent under a running turn leaves the agent running. "The previous build" is a copy of the build under test re-stamped one version lower (`node::release::restamp`). |
| [`crates/amux/tests/process.rs`](../crates/amux/tests/process.rs) | A daemon killed mid-turn loses nothing: agents finish without it and the next daemon adopts them. A daemon with no supervisor restarts itself in place, once, for several `amux config set experiments.enabled` in a row, keeping its agent mid-turn and, where exec keeps it, its process id (on Windows the first daemon exits cleanly and its successor serves the new experiments), restarts nothing for a name the build lacks, and reaps the agent the replaced image started (Unix, where it is the image's child). |

`just journey system survive-daemon` tells the same story end to end: agents of
every kind finish their turns with amux killed, amux comes back under its
supervisor, a newer build is activated, and resuming loses nothing. The
suites run on Windows too, and `windows_install.rs` only there; a case
that belongs to one OS (a LaunchAgent, macOS's power assertion, Unix's stop
signal, a pid an exec keeps) is skipped or branches elsewhere, and says
so. See [Testing](TESTING.md) for how to run them.
