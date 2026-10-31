# TestNet

*For developers writing tests that span several hosts, and for anyone driving a real client against a served test network.*

TestNet (`crates/testnet`) starts a declared network of amux hosts, breaks it in controlled ways and observes the
consequences. The same network runs inside a Rust test, where the test holds a `Net` and calls its verbs, or in
its own process as `testnet serve`, where a driver sends the same verbs over a control socket. `testnet ctl`
drives a served net one shell command at a time, for a person or an agent exploring by hand. Terminal and phone
journeys, the phone's accessibility audit, the flood performance workload and [QA](QA.md) all run on it. How it fits among
the other suites is on [Testing](TESTING.md).

## What is real

Each host is a production daemon runtime (`node`) started inside the harness process, on its own temporary
installation with its own identity, trust store, profile store and front door socket. Agents are real `amux
agent` processes, running the scripted fake providers from `crates/provider-fakes` (`fake-claude-pty`,
`fake-claude-sdk`, `fake-codex`) in place of Claude and Codex. Links between hosts are real authenticated QUIC
links over loopback. A declared relay is the production relay server on loopback, beside a stand-in for the
account service that signs accounts in and mints relay credentials.

This is the real net; a [simulated net](#two-modes) runs the same stories with every core in memory, stepped by a
seed. What is replaced on the real net: the providers (by the fakes, unless a host or agent asks for the real ones), the account service
(by the stand-in), multicast discovery (by a scripted bus, unless a host asks for real mDNS), and, in a Rust test,
policy time (by a driven clock).

Because the daemons are runtimes inside one process, "kill a daemon" drops its runtime rather than killing a
process; the agent processes do keep running, as they would. A host can instead be a real `amux` daemon process,
or one under `amux supervise` (see [process and supervised hosts](#process-and-supervised-hosts)), so a crash,
a restart or an update is the shipped binary's own. Claims about the CLI and installation themselves still
belong to the process tests in `crates/amux/tests`.

The harness never builds `amux` or the fake providers. In a test, the nextest setup script built them before the
first test started and names their directory in `TESTNET_BINARIES`, so agents always run the binary under test; a
test without it fails at once. A driver that is not a test (`testnet serve`, `perf`) takes them from
`TESTNET_BINARIES` if it names a directory, else from the directory it runs from, where its recipe built them in
the same cargo invocation as the driver: a build of their own would unify features otherwise and rebuild much of
the workspace. The terminal and phone journey drivers hand `testnet serve` that directory and stop at once if a
binary is missing there.

## Two modes

Everything above describes the real net. The same topology can also start as a simulated net (`SimNet`,
`crates/testnet/src/sim`), and a story written against the `World` trait (`crates/testnet/src/world.rs`) runs
unchanged on either:

| | Real | Simulated |
| --- | --- | --- |
| A host | a production daemon runtime on loopback | its daemon core over an in-memory store |
| An agent | an `amux agent` process on a fake provider | its agent core over the test kind's interpreter, playing the script or a recorded headless Claude journal |
| Links, jobs, timers, processes | real sockets, files, tasks and the driven clock | values in memory, moved one delivery at a time |
| Who picks what happens next | the operating system's scheduler | a seed: each step makes the delivery the seed picks among those waiting |
| A wait that cannot come true | fails at its deadline | fails at once, when nothing is left to deliver without the story acting |
| A failure is reported as | the assertion and the net's trace | the seed, the assertion and the trace of deliveries, with the command that reruns that seed |

The simulated net is the composed-net model of `crates/models` walked by seed: the story's verbs are the model's
actions, so the checker and the simulator explore one definition. After every step it checks each core's own
invariants and every store's after a write, so a broken invariant fails the seed where it broke, not where a story
later noticed. One seed is one schedule: the same seed gives the same trace on every run. `SimNet::ring` records the
choices a run made, `SimNet::replay` replays any prefix of them, and `testnet::sim::shrink` looks for a shorter
failing run, first among other seeds, then by cutting the ring.

What a simulated net does not have it refuses at start rather than pretending: the relay, the LAN and discovery,
process and supervised hosts, real providers, repositories, and agents of a kind other than headless Claude.
Stories that need any of those run on the real net only.

`testnet::modes::run_story(topology, real, simulated)` runs a story on the net `TESTNET_MODE` names: `real` (or
nothing) once, `simulated` under each seed, a few by default, `TESTNET_SEEDS=N` for N of them, `TESTNET_SEED=N` for
exactly one. `just sim` runs every story the catalogue gives a simulated mode, and `just sim --seeds N` walks N seeds
per story, as the nightly workflow does with thousands; see [Testing](TESTING.md#two-modes-of-the-many-daemons-harness).

## Topologies

A `Topology` declares the hosts, the links between them, an optional relay with its accounts, and the agents to
start. The Rust builder and the JSON loader produce the same value, and both reject unknown and duplicate names
before anything starts. A topology may declare no hosts at all: a net can start empty and
[grow while it runs](#growing-a-net-while-it-runs).

```rust
let topology = Topology::new()
    .host("desk")
    .host("laptop")
    .link("desk", "laptop")
    .agent(AgentDecl::new("worker", "desk").prompt("go").steps(vec![
        Step::Text { chunks: vec!["before".into()] },
        Step::WaitFor { path: "release".into() },
        Step::TurnEnd,
    ]));
```

The same network as JSON, as `testnet serve` reads it:

```json
{
  "hosts": [{"name": "desk"}, {"name": "laptop"}],
  "links": [{"a": "desk", "b": "laptop"}],
  "agents": [
    {"name": "worker", "host": "desk", "prompt": "go",
     "script": {"steps": [{"text": {"chunks": ["before"]}}, {"wait_for": {"path": "release"}}, "turn_end"]}}
  ]
}
```

| Field | Meaning |
| --- | --- |
| `scope` | The discovery scope every host advertises and filters by, unless a host names its own. |
| `hosts[].name` | Letters, digits, `-` or `_`; unique. |
| `hosts[].lan` | Listen for direct links on a loopback QUIC listener. |
| `hosts[].discovery` | Advertise and browse on the net's scripted discovery bus, and dial trusted hosts found there. A process host cannot reach the bus: the net announces it there at its LAN listener (so it needs `lan`), and it does not browse. |
| `hosts[].bonjour` | Advertise on the machine's real local network through the system's mDNS responder, for a client outside the net (a simulator) to find. Needs `lan` and not `discovery`. |
| `hosts[].scope` | This host's discovery scope, where it differs from the net's. |
| `hosts[].account` | The relay account this host's profile signs in to at start; needs a `relay`. |
| `hosts[].script` | What agents a client creates on this host play; none plays nothing. |
| `hosts[].repositories` | Git repositories made under the host's repository root, by path below it. |
| `hosts[].files` | Files committed in those repositories when they are made, by path below the root (`amux/README.md`), so an agent's edits show as a working-tree diff. |
| `hosts[].provider` | `fake` (the default) or `real`: what every agent created on this host runs, whichever client creates it. See [real providers](#real-providers). |
| `hosts[].process` | Run the host as a real `amux` daemon process with its own installation config. |
| `hosts[].supervised` | Run it under `amux supervise`, taking releases from the net's feed; implies `process`. |
| `hosts[].ipv6_only` | Bind the host's socket to IPv6 loopback alone, so every link it has runs over IPv6; `Net::bind_ipv6_only` sets it from the host's next start. In-process hosts only. |
| `hosts[].router` | `open`, `ordinary` or `strict`: put the host on a network of its own, behind an imitated router of that kind from its first start, as `set_router` does (below). In-process hosts only. |
| `links[]` | `{"a", "b"}`: two hosts that trust each other and are linked at start. |
| `relay.accounts[]` | `{"name", "tier"}`, tier `pro` (the default: relayed tunnels) or `free` (hosts are listed; the relay opens no tunnels). |
| `settle` | Start each declared agent only once the one before it has settled (see `settle(name)` below), so agents that come to rest in the same standing are listed in declaration order, the last declared as most recently active. Every declared agent must come to rest. Without it the agents start together and which first turn ends last is a race. |
| `agents[].name`, `host` | Unique name; the host it runs on. |
| `agents[].kind` | `claude_pty`, `claude_sdk` (the default) or `codex`: which fake runs and which interpreter reads it. |
| `agents[].script` or `script_file` | What the fake plays, inline or from a file relative to the topology file; not both. |
| `agents[].prompt` | The first prompt, sent at creation. |
| `agents[].parent` | An agent declared earlier, on any host. |
| `agents[].cwd` | Its working directory; none is the host's work directory, and a relative path a folder below it. |
| `agents[].repository` | One of its host's repositories to work in, instead of `cwd`. |
| `agents[].branch` | The branch its `cwd` folder is on: the net makes that folder a git repository on this branch, so its fleet row has a branch. Needs a relative `cwd`; agents sharing a folder share its branch. |
| `agents[].permission` | The permission it is created with, as its catalogue names it (`plan` starts Claude planning); none leaves the provider's default, except that a real Claude starts with `default`. |
| `agents[].mode` | The collaboration mode a Codex agent starts in (`plan` starts it planning). |
| `agents[].model` | The model it is created on; a real agent with none starts on Haiku (Claude) or `gpt-5.6-luna` (Codex). |
| `agents[].provider` | `fake` or `real`, where it differs from its host's. |

JSON topologies live in `journeys/topologies`; scripts shared between them in `journeys/scripts`.

### Scripts

A script is the model's side of a session; the fake supplies the provider's own protocol (handshakes, echoes,
queueing, control replies), so every script gets the same provider behaviour. Steps run in order: when idle, the
fake waits for a prompt or an injected message, then plays steps until `turn_end`.

| Step | Effect |
| --- | --- |
| `text` | Streamed assistant text, chunk by chunk where the provider streams. |
| `thinking` | Reasoning before the next step. |
| `tool` | A tool call that runs without asking. |
| `ask` | Something the host must answer first: `permission`, `question`, `plan`, `form`, `link`, `grant` (Codex) or `tool_server_dialog` (terminal Claude). |
| `wait_for` | Hold the turn until a file exists; a relative path names a file in the net's gates directory. |
| `pause` | Stay busy for some milliseconds. |
| `turn_end` | Finish the turn. |
| `exit` | Exit the provider process with a code. |
| `repeat` | Play some steps a number of times, without writing them out. |

A script can also set the `model`, the `models` and `commands` a session offers, `turn_ms`, how long headless
Claude says each finished turn took (the chat's "Worked 4m"), and, for terminal Claude, `offers_auto_mode` and
`untrusted_folder`. A Codex command that asks gives the `reason` in its input as the request's reason. A script that asks a provider for an ask kind it cannot raise fails to
load.

## Real providers

A host's `provider` decides what agents created on it run, whoever creates them: `ctl spawn`, `amux new`, the
terminal client, the phone, or another agent's `spawn` tool. With `real`, they are the real Claude and Codex
under the operator's own logins; an agent's own `provider` overrides its host's for that one agent.

Each host has its own `claude` and `codex` wrapper commands in its directory. An in-process host names them in its
launch settings; a process host, which runs whatever `claude` and `codex` its PATH holds, is started with the
wrappers first on its PATH, and so are its agents, their provider children and a daemon started after an update.
A wrapper finds which agent started it, then runs either the fake that agent was given or the real provider:

- the real provider runs behind its recording probe (`claude-probe`, `codex-probe` from `claude-specs` and
  `codex-specs`, which must be built beside the other binaries), so every message amux wrote to it and read from
  it is kept, by host and agent, under the recordings folder (`NetOptions::recordings`, `serve --recordings`;
  under the net's root when none is given);
- Claude runs with `--setting-sources project --strict-mcp-config`. Codex runs with a scratch Codex home
  holding only the operator's login and a config that switches off the account's plugins and connected apps,
  and with a scratch home directory, because Codex also reads skills from the user's own. The operator's own
  settings, skills, plugins and tool servers stay out; an agent carries only amux's tool server, hooks and
  built-in skills.

A real host's installation config sets the person's defaults for a new agent to Haiku and `gpt-5.6-luna`, so an
agent made from the terminal client starts on the small models unless someone picks another. An agent the net
spawns on a real provider (`Net::spawn`, `ctl spawn`) starts on the small model and, for Claude, the `default`
permission, unless its declaration names others. Any other agent amux starts with no model or permission named,
such as one left at the phone's "Host default" or a child made by amux's spawn tool, gets the same from the
host's provider commands: the Claude command adds `--model haiku` and `--permission-mode default` when amux
passed neither, and Codex's scratch config names `gpt-5.6-luna` as its model. Left unset, Claude would take a
large model and the operator's own default permission mode, which may act without asking. Waits on a real agent
(`settle`, `turn_ended`, `input_settled`) allow `REAL_PATIENCE`, 240 seconds, rather than `PATIENCE`. On a real
host, refreshing what each provider offers (models, commands) runs the real provider.

## Process and supervised hosts

A process host is a real `amux` daemon in its own process, with its own installation config under the net's
root (`keep_awake = false`, `telemetry = false`, `updates = "manual"`, the net's discovery scope and release feed). The
net starts it with `amux server start` and stops it with `amux server stop`, as a person's CLI would, and kills
it with SIGKILL on the process that holds its installation lock. Its agents keep running through a kill, in their
grace, and the next daemon adopts them. The net reaches it as a client does, over its front door and profile
socket: the waits that listen inside an in-process runtime (`fence`, `turn_ended`, `input_settled`) use the
profile's Subscribe stream instead, back-filling with Fetch past the 200 rows a subscription opens with.
Subscribing on a host that only replicates an agent can start replication there, which the in-process wait does
not. It lists no interface addresses to its peers (`AMUX_TEST_INTERFACES` is set empty), as an in-process host on
loopback lists none: the machine's real interfaces would give a peer's path machine a way around a cut, gated or
imitated network.

A supervised host runs under `amux supervise`, from a copy of the build under test stamped as version `0.0.1`
(`SUPERVISED_VERSION`), so any ordinary version is newer. Its supervisor takes releases from the net's
[release feed](#the-release-feed), and starts the daemon again by itself after a kill.

What a process host refuses, because it reaches into a runtime the net does not hold: `checkpoint_host`,
`rewind_host` and `assert_block_invariant`. `set_tier` changes the account, but a process host's
daemon learns of it on its own schedule rather than at once.

A process host keeps the machine's time, so a net with one runs on wall time (`ClockMode::Wall`); on a driven
clock adding one is refused. Two hosts of which one is a process pair for real, over the LAN, so both need `lan`.
A paired host redials its peer's last working address whether or not discovery finds it, with a backoff up to
thirty seconds: after a process host crashes and restarts, its peers dial it again once their old connections
time out (QUIC's idle timeout, thirty seconds between desktops), and it dials them at the addresses it stored. The gates
carry datagrams either way, so a host that answered a pairing through one can dial back through it.

### The release feed

The first supervised host starts a release feed: a plain `http://127.0.0.1` server the supervised hosts'
`releases_url` names, which debug builds accept, serving manifests signed with the test release key (the
`release` crate's `test-key` feature). `publish_release(version, channel, rollout, broken)`, the door's
`Release` and `ctl release publish` re-stamp the build under test as that version, sign and list it on the
channel (to `rollout` percent of hosts by host id, or all), launch it once (a fresh binary's first launch can
stall on macOS), and ask every supervised host's supervisor to check now rather than at its hourly check. With
`broken`, the listed build is a script that exits at once, so it never prepares and the supervisor rolls back
after its failed starts.

## Bad networks

The net puts UDP gates in the paths it can steer. A gate forwards datagrams both ways and can drop them all
(a cut) or treat them as a poor network does. `Faults` sets what a gate does to each datagram it lets through:

| Field | Effect |
| --- | --- |
| `loss_percent` | Drops this many datagrams in a hundred. |
| `delay` | Holds each datagram this long. |
| `jitter` | Adds up to this much more delay, drawn for each datagram; order is kept. |
| `reorder_percent` | Holds this many in a hundred back once more, out of the queue, so the ones behind overtake them. |
| `rate_bytes_per_sec` | Passes at most this many bytes a second, queueing up to a second's worth and dropping beyond it. |

The draws are seeded, so runs differ only by timing.

Every host with a relay reaches it through a gate of its own. On a **gated LAN** (`NetOptions::gated_lan`,
`serve --gated-lan`), the LAN is gated too: one gate for each ordered pair of hosts, which the dialling host is
told to use in its peer's place (the net rewrites the discovery bus's advertisements for each browser, and a
pairing by address through `pair_gate`), and one gate in front of each host for a client outside the net, which a
Bonjour advertisement names. So any pair of hosts can be cut however they came to trust each other, and any
host's network degraded, including the phone's way in. A cut on a pairing is silent, as a dead network is: each
side notices when its connection times out. Cutting a pairing needs a gated LAN; on an ungated one a host that
found its peer by discovery dials the peer's own listener, which no gate stands in front of. Links made in the
net's process (declared, or by `add_link` between two in-process hosts) carry no datagrams; `sever_link` cuts
them directly.

| Verb | Effect |
| --- | --- |
| `set_net_faults(target, faults)` | Make a host's network bad (every gate its datagrams cross: its way to the relay, its LAN pairings, its front gate) or the relay's (`"relay"`: every way to it). `Faults::default()` makes it clean again. |
| `set_lan_faults(host, faults)`, `lan_gate(host)` | The front gate alone: its faults, and its address for a client to dial in the host's place. |
| `set_relay_faults(faults)`, `relay_gate()` | A gate in front of the relay's QUIC carrier for a client outside the net. |
| `pair_gate(from, to)` | The gate `from` dials to reach `to`'s LAN listener: give it to a pairing in place of `to`'s address. |
| `block_udp(host, blocked)` | Drop every datagram on the host's way to the relay, so it falls back to TCP. |

### Routers

`Router` imitates the home router in front of a host, for the stories that decide whether two hosts can punch
through to each other. It is the substrate under the host's link socket: the host's own address is a private one
(`10.n.0.2`) nobody can reach, what it sends leaves from a public loopback port the router's table hands out, and
what arrives passes only through a mapping the router's filter allows. A mapping nothing has used for the router's
idle time expires, and the next packet out gets a new port. `public_addr_for(inner, dst)` says which public address
packets to `dst` leave from now, which is what the relay sees.

| `RouterKind` | Mapping | Inbound filter |
| --- | --- | --- |
| `Open` | none: the host's own address is public | anyone |
| `Ordinary` | one public port for every destination | from any address and port the host sent to through it |
| `Strict` | a public port per destination | only from the exact address and port that port was opened toward |

A punch works behind `Ordinary`: the port the relay saw is the port the host punches from, so the peer's answer to
the address the relay told it gets in. Behind `Strict` the punch leaves from a different port, and the answer to the
relay's address is dropped.

A story puts a router in front of an in-process host and moves it between networks with these verbs; each is also
on `World`, and a simulated net, which has no sockets, refuses them:

| Verb | Effect |
| --- | --- |
| `set_router(host, kind)` | Put a router of that kind in front of the host's link socket, now if it runs and on every start after. Its links migrate onto the router's public port; a mapping idle for 30 seconds expires. A host started behind a router reaches the relay's QUIC listener straight from its public port, not through the net's gate, so the relay sees and reports the router's port. |
| `change_network(host)` | Move a running host onto another network, as a phone leaving Wi-Fi does: its link socket rebinds to a fresh port, or behind a fresh router of the same kind with new public ports, its links migrate without going down, and its path machines are told as the phone's runtime tells them. |
| `bind_ipv6_only(host)` | Bind the host's socket to IPv6 loopback alone from its next start. |

Without a router an in-process host reaches the relay's QUIC listener through a gate of its own, whose socket toward
the relay takes datagrams from the relay alone, so the address the relay reports for it is one no peer can punch to.
A story or journey that means hosts on different networks says so with the router instead: `strict` for hosts reached
through the relay alone, whose ports take datagrams only from where they were opened toward, and `ordinary` for
networks a punch crosses. A routed host reaches the relay from its router's public port, not through its gate, so the
gate's verbs (`block_udp`) do not reach it. A phone launched against a net dials the relay from its own socket, which
a host's probe reaches; a phone journey whose topology routes any host launches the app against the net's relay gate
(`RelayGate`) instead, so the phone too is seen at a socket that takes datagrams from the relay alone.

The router stories in `spec_links.rs` put two paid hosts behind routers with nothing between them but the relay:

| Story | What it shows |
| --- | --- |
| `punch_ordinary_routers_…` | Behind `Ordinary` routers each host probes the address the relay saw the other at, both routers open, and the link moves from `relay_udp` to `punched` with the session streaming. |
| `punch_strict_routers_…` | Behind `Strict` routers both hosts learn the other's observed address, every probe is dropped, and the link stays on `relay_udp`. |
| `punch_free_never_…` | Two hosts on one free account on the same network: neither learns where the relay sees the other, so neither has anything to punch to, and the link stays on `lan`. |
| `another_network_strict_routers_…` | Two paid hosts declared behind `strict` routers: each end's link stays on `relay_udp` after every punch has given up, never `punched` or `lan`. |
| `another_network_ordinary_routers_…` | The same declaration with `ordinary` routers: the link moves onto `punched`. |
| `punch_lapse_drops_…` | A lapse takes the punched path away: the link leaves `punched`, and with no free path between the routers goes down. |
| `path_change_lan_relay_lan_…` | One subscription stays open while the LAN between two signed-in hosts is cut (the link moves to `relay_udp`) and restored (back to `lan`): every line once, one opening, no reset, lag or detach, and the link never goes down. |
| `path_change_phone_network_…` | `change_network` under a host mid-stream: the same subscription reads on with no gap and the link stays up. |

### Which path carries a link

Every daemon writes the link log: one line at INFO each time one of its links comes up, goes down or changes path,
naming the peer and the path (`lan`, `direct`, `punched`, `relay_udp`, `relay_tcp`, `ssh`, `via_daemon`). The net
reads an in-process host's lines from its own trace and a process host's from its daemon log, so a story says which
path carried a link the way a person reading the log would:

| Verb | Effect |
| --- | --- |
| `link_log(host)` | Every line the host has written, oldest first, across restarts. |
| `carrying_path(host, peer)` | The path the host's last line for that peer names, or none once that line says the link went down. |

### SSH

A host reaches a peer over SSH by running `amux internal ssh-link --profile <id>` on it through `ssh <target>`, where the id is the peer's host id
(a profile's id is its host's), and carrying the link over that
process's stdio into the far daemon's link socket (`link.sock` in its profile directory), where the pinned QUIC
handshake admits only a paired key. Every in-process host serves its link socket. The net puts a stand-in `ssh`
(`crates/testnet/stand-in/ssh`) first on its process's PATH, which process hosts inherit; no real SSH runs. A target
is a host's directory, which `ssh_target(host)` answers: the stand-in runs amux's command through a shell, as sshd
would, with the far host's own `amux` against its own installation first on that shell's PATH, so `amux internal ssh-link` joins
the far daemon's link socket, and `amux internal ssh-pair` answers an SSH pairing.
Give the target to a peer as an SSH pairing does; `amux pair` reads a target as SSH in `user@host` form, and the
stand-in drops the user.

## Growing a net while it runs

A net may start with no hosts and grow, and every growth verb is on the door:

| Verb | Effect |
| --- | --- |
| `add_host(decl)` | Create and start a host declared as in a topology, signing it in to its account if it names one. |
| `remove_host(name)` | Kill its agents and take its daemon down; its gates and links go. Its peers still trust it, as they would a machine switched off for good. |
| `add_link(a, b)` | Trust and link two hosts, returning once the link is up: in process when both run in the net's process, otherwise by real pairing over the LAN through the pair's gate. |
| `remove_link(a, b)` | Make both forget each other, as unpairing does. |
| `add_account(account)` | Add an account to the relay's cloud, starting the relay if none runs. A host that was up before the relay started has no way to it until it restarts. |

`topology()` answers the net as it stands, hosts and links added at run time included.

## The net in a Rust test

`Net::start(topology)` starts the relay and every host, signs hosts in to their accounts, trusts and links the
declared pairs, waits until each link carries traffic both ways, and spawns the declared agents, each returning
once its process has said hello (or, when the topology sets `settle`, once it has settled). When it returns the net is ready. `Net::start_with(topology, NetOptions)` takes a clock
mode, a shared `DrivenClock`, hooks that adjust each host's edge or launch parameters (the tail size, a
retention budget), a fixed root directory, each agent's facts ring size (`facts_ring_mib`, large enough that a
ring holds an agent's whole life), where real providers' traffic is recorded (`recordings`), and whether the LAN
is gated (`gated_lan`).

The harness owns resources, verbs and observations, never scenarios. Scenarios live in the tests.

**Verbs** change the world and return an `Ack { installed, at_ms }` once the change is in place.

| Verb | Effect |
| --- | --- |
| `sever_link(a, b)`, `restore_link(a, b)` | Cut a link the way a dead connection goes (no close, the carrier stops), and bring it back. |
| `trust(a, b)`, `untrust(a, b)` | Trust as pairing would, without linking; forget, as unpairing does. |
| `kill_daemon(host)` | Crash the host's daemon; its agent processes keep running in their grace. |
| `stop_daemon(host)` | Shut it down cleanly; its agents keep running. |
| `restart_daemon(host)` | Start it again from what its installation holds, under the same boot, and relink. |
| `checkpoint_host(host)` | Record what has reached the drive: the store flushed and its file as it stands. |
| `rewind_host(host, cuts)` | Power loss: daemon and agents die at once, the store goes back to the checkpoint without its write-ahead log, each named journal is cut at a byte, and the host returns under a new boot id. |
| `advance(by)` | Move policy time on every host (driven nets only). |
| `spawn(decl)`, `resume(name, text)` | Start an agent; start an exited agent's next incarnation. |
| `settle(name)` | Wait until the agent rests on its host: idle or needing the person, nothing queued, so a creation prompt's turn has run. The fleet lists agents of one standing most recently active first; settling each before starting the next fixes that order. `settle_within` and `input_settled_within` take their own deadline; `patience_of(name)` is the agent's usual one. |
| `turn_ended(name, k)` | Wait until the agent's `k`th turn has ended on its own host; returns the order of the row that ended it. |
| `input_settled(name, input_id)` | Wait until the input the agent accepted under that id has its reflecting row; returns its order. A rejected input never settles: rejection is the answer `send` and `input` return. |
| `fence(host, name, order)` | Wait until the host holds the agent's row at that order, on the origin or a replica. After it, what the host holds through that order is what its store reports. |
| `current(host, name)` | Wait until the host holds what the origin holds: caught up through the origin's newest revision, with the block intact. |
| `send(name, text)`, `input(...)` | Send a prompt, or any input (an answer, a withdrawal, an interrupt), through the agent's own host. |
| `stop(name)` | Stop the agent on its own host, as a person does; it can be resumed. |
| `delete(name)` | Delete an agent on its own host, as a person does. Its children follow on their own hosts by the family rule, after the call returns. |
| `spawn_child(parent, decl)` | Spawn through the parent's tool socket, naming the child's host. |
| `next_start(name, script)` | Give the next process the host starts for `name` this script, for a resume the host makes itself. |
| `freeze(name)`, `thaw(name)` | Stop an agent process where it stands, holding its lock and connection, and let it run again. |
| `sign_in(host, account)`, `set_tier(account, tier)` | Sign a host in through its front door; change what an account has bought. |
| `block_udp(host, blocked)` | Drop every datagram on the host's way to the relay, or stop dropping them. |
| `open_gate(name)` | Create a gate file, releasing every `wait_for` step waiting on it. |
| `publish_release(version, channel, rollout, broken)` | List a release on the net's feed and have supervised hosts check now (see [the release feed](#the-release-feed)). |

The verbs that shape the network and grow the net are under [bad networks](#bad-networks) and
[growing a net](#growing-a-net-while-it-runs).

**Fences** wait on a host's own cursors. A test asserts about a host only after the step it triggered has landed
there by the host's account: a turn's end, an input's reflecting row, a row at an order, the replica's catch-up.
`turn_ended`, `input_settled` and `fence` wake on the host's store writes (`ProfileRuntime::committed`), so they
return the moment the write lands and never infer from time; `current` reads two hosts and looks again every
poll. A missed deadline reports what the host last held.

**Windows** are the exception. A test that must show something does *not* happen waits on the outcome of the
thing that must have no effect (an input's rejection, a dial's stored error, a tick's re-armed deadline) and then
asserts the absence. Only when nothing the test controls gates it, such as a purchase the runtime has not asked
about yet, does `holds_for` hold a window of real time, and each such window says so in a comment beside it.

**Observations** wait for consequences and return what they saw. `observe(host, agent, tail)` opens a Subscribe
stream as a client would and records it; `observe_inventory(host)` does the same for the inventory. Their
`observe_until(predicate, deadline)` passes only when the predicate holds: it fails with `Stuck::Deadline` when
the deadline passes and with `Stuck::Closed` at once when the stream ends. `until` (from the `patience` crate every test crate shares) and
`holds_for` poll with the same rule, a missed deadline reports what the probe last saw, and a check that never
answers fails too. `PATIENCE`, 30 seconds, is
the usual deadline; real work on loopback settles well inside it. `assert_block_invariant(host, agent)` checks a
replica's rows against its origin: empty, or one contiguous block ending at the origin's newest row at the
origin's revisions.

**Resources** answer questions about the net: `host(name)` (its data directory, front door and installation
config), `runtime(name)`, `agent_dir(name)`, `journal_end(name)`, `provider_input(name)` (every line the stdio
fakes read), `client(host)` and `tools(name)` (the client service as a person or an agent reaches it), and
`relay()`.

A test ends with `net.shutdown().await`, which stops every agent and shuts every daemon down cleanly. Dropping a
net kills what it can and is for panics.

```rust
let mut net = Net::start(topology).await?;
let mut chat = net.observe("desk", "worker", 50).await?;
chat.observe_until(|events| says(events, "before"), PATIENCE).await?;
net.kill_daemon("desk").await?;
net.open_gate("release")?;          // the agent writes on with no daemon
net.restart_daemon("desk").await?;
let mut chat = net.observe("desk", "worker", 50).await?;
chat.observe_until(|events| says(events, "after"), PATIENCE).await?;
net.shutdown().await?;
```

### The driven clock

A net started with `Net::start` runs every daemon's policy timers (retention, outbox retries and notification
delays, credential refresh, reply and start deadlines) on one `DrivenClock`. It starts at the wall time the net was
built, so certificates and credentials minted against it look current, and moves only on `advance`.
`DrivenClock::armed(at_ms)` resolves once something sleeps until exactly that moment, so a test can step past a
deadline knowing it was set. Relay credentials expire on the same clock (`CREDENTIAL_TTL`, ten minutes).

Transports stay on real time: a QUIC idle timer or a socket read is not policy. So does the core's link time, which
every dial's deadline and pause is reckoned in: a link that drops is redialled as it would be off the net. Never
pause the async runtime around real IO. `testnet serve` and the flood run on wall time, where `advance` is refused.

## `testnet serve`

```sh
cargo build -p testnet --bins          # or `just ios tools`, which builds it with the phone's tools
target/debug/testnet serve journeys/topologies/terminal-stories.json
target/debug/testnet serve TOPOLOGY --control 127.0.0.1:7000 --root-in /tmp/fixed
target/debug/testnet serve TOPOLOGY --gated-lan --facts-ring-mib 64 --recordings /tmp/providers
```

`serve` loads and validates the topology, starts it on wall time and, only once the net is ready, prints one
readiness line of JSON on standard output. `--control` fixes the control socket's address (an ephemeral loopback
port by default). `--root-in DIR` puts the net's root at `DIR/net` instead of a fresh temporary directory, so every
path a client draws is the same each run; the directory must not already hold a `net`. `--gated-lan` routes the
LAN through the net's gates ([bad networks](#bad-networks)), `--facts-ring-mib` sets each agent's facts ring size,
and `--recordings DIR` is where real providers' traffic is recorded. A topology file with no hosts starts an
empty net. With `RUST_LOG` set, the daemons' tracing goes to standard error.

### Readiness

```json
{
  "control": "127.0.0.1:53211",
  "root": "/tmp/testnetAbc123",
  "gates": "/tmp/testnetAbc123/gates",
  "hosts": [
    {"name": "desk", "host_id": "…", "profile": "…",
     "front_door": "/tmp/testnetAbc123/desk/door.sock",
     "config": "/tmp/testnetAbc123/desk/config.toml",
     "dir": "/tmp/testnetAbc123/desk", "process": false, "supervised": false,
     "provider": "fake", "up": true, "lan_port": 53990}
  ],
  "agents": [{"name": "worker", "host": "desk", "id": "…", "kind": "claude_sdk", "provider": "fake"}],
  "cloud_url": "http://127.0.0.1:53212",
  "relay_tcp": "127.0.0.1:53213"
}
```

Each host lives under `<root>/<name>/`: its installation in `data/`, its agents' working directory in `work/`,
and `door.sock` and `config.toml` beside them. A host's `config` is that installation config, so the real terminal client runs against it
with `amux --config <config>`. `cloud_url` and `relay_tcp` appear only when the topology declares a relay:
`cloud_url` is the account-service stand-in, where `refresh-<account>` is the login for a declared account, and
`relay_tcp` is the relay's plain TCP carrier for a client outside the net. `lan_port` appears for a host with a LAN
listener; `up` says whether its daemon is running.

### The control door

Connect to `control` over TCP and send one JSON request per line; each gets one reply line, in order. Several
drivers may connect at once. Requests use the verb as the key (`{"Sever": {"a": "desk", "b": "laptop"}}`), and a
verb without fields is a bare string (`"Shutdown"`).

| Request | Effect and reply |
| --- | --- |
| `{"Sever": {"a", "b"}}`, `{"Restore": {"a", "b"}}` | Cut or restore a link: a declared or added one, or on a gated LAN any pairing between the two. |
| `{"Link": {"a", "b"}}` | Whether both ends route to each other directly: `{"up": true}`. |
| `{"Trust": {"a", "b"}}` | Trust as pairing does. |
| `{"SetTier": {"account", "tier"}}` | Change what an account has bought (`pro` or `free`). |
| `{"SignIn": {"host", "account"}}` | Sign a host's profile in to a declared account. |
| `{"KillDaemon": {"host"}}`, `{"StopDaemon": {"host"}}` | Crash or cleanly stop a host's daemon. |
| `{"RestartDaemon": {"host"}}` | Kill it if it is running, then start it again. |
| `{"Checkpoint": {"host"}}`, `{"Rewind": {"host", "cuts": [{"agent", "byte"}]}}` | Record what reached the drive; lose power back to it. |
| `{"Spawn": {"agent": {…}}}` | Start an agent declared as in a topology: `{"id": "…"}`. |
| `{"Resume": {"agent", "text"}}` | Start an exited agent again, optionally with a prompt: `{"incarnation": n}`. |
| `{"Send": {"agent", "text"}}` | Send a prompt through the agent's host: `{"verdict": "…", "input_id": "<hex>"}`. |
| `{"Settle": {"agent", "input", "timeout_ms"}}` | Wait until the agent rests, after the input named by its hex id when `input` is given; `timeout_ms` defaults to the agent's usual patience. |
| `{"Stop": {"agent"}}` | Stop the agent on its host, as a person does; it can be resumed. |
| `{"Freeze": {"agent"}}`, `{"Thaw": {"agent"}}` | Stop the agent's process where it stands, answering nothing, and let it run again. |
| `{"OpenGate": {"name"}}` | Release `wait_for` steps waiting on that file. |
| `{"AddHost": {"host": {…}}}`, `{"RemoveHost": {"host"}}` | Add a host declared as in a topology and start it; take one out. |
| `{"AddLink": {"a", "b"}}`, `{"RemoveLink": {"a", "b"}}` | Trust and link two hosts, returning once the link is up; make them forget each other. |
| `{"AddAccount": {"account": {"name", "tier"}}}` | Add an account to the relay, starting the relay if none runs. |
| `{"NetFaults": {"target", "delay_ms", "loss_percent", "jitter_ms", "reorder_percent", "rate_bytes_per_sec"}}` | Make a host's network, or the relay's (`"relay"`), bad; all zero makes it clean. Every field but `target` may be left out. |
| `{"LanGate": {"host"}}`, `{"LanFaults": {"host", …}}` | A host's front gate for a client outside the net: `{"addr": "…"}`; its faults, with the fields `NetFaults` takes. |
| `{"RelayGate": {}}` | A gate in front of the relay's QUIC carrier: `{"addr": "…"}`; `NetFaults` with `"relay"` makes it bad. |
| `{"PairGate": {"from", "to"}}` | The gate `from` dials to reach `to`'s LAN listener: `{"addr": "…"}`. |
| `{"BlockUdp": {"host", "blocked"}}` | Drop every datagram on the host's way to the relay, or stop. |
| `{"Flap": {"a", "b", "down_ms", "up_ms"}}`, `{"FlapStop": {}}` | Sever and restore a link on a period, until stopped; stop every flap, leaving each link restored. |
| `{"Release": {"version", "channel", "rollout", "broken"}}` | Publish the build under test on the release feed (channel `stable` by default) and have supervised hosts check now. |
| `{"Status": {}}` | The readiness record as it stands now, with `topology`: the declaration that would start this net again, run-time additions included. |
| `{"Inventory": {"host"}}` | The host's fleet once its inventory has caught up: `{"agents": [{"name", "id", "host_id", "lifecycle", "phase", "cwd", "branch", "kind", "lifecycle_name", "phase_name", "working_on", "incarnation", "exit_cause", "exit_detail"}], "hosts": [{"name", "id", "trust", "presence"}]}`; `branch` is null outside a repository, `exit_cause` null unless the agent has exited; `trust` is `TRUSTED` or `CANDIDATE`. |
| `{"Chat": {"host", "agent"}}` | Everything the host holds of a chat, read to its first CaughtUp: `{"items": [{"key", "order", "text", "input_id", "attachments"}], "phase", "model"}`, `model` as the newest snapshot names it. `agent` is a declared name or, for an agent a client created, its id. |
| `{"Block": {"host", "agent"}}` | Check the replica block invariant: `"holds"`. |
| `{"ProviderInput": {"agent"}}` | Every line the agent's provider read: `{"lines": […]}`. |
| `"Shutdown"` | Stop every agent and daemon, remove the net's root, reply, and exit. |

A reply is `{"ok": <value>}`, where a verb's value is usually its acknowledgement
(`{"installed": "link desk - laptop severed", "at_ms": …}`), or
`{"error": {"kind", "message"}}`, where `kind` tells a driver's own mistake from the net's refusal:

| Kind | Meaning |
| --- | --- |
| `invalid` | The request did not parse, or named a host, agent or link the net does not have. |
| `refused` | The net refused: a host down or running, no checkpoint. |
| `stuck` | An observation did not see its consequence in time. |
| `violation` | The replica block invariant does not hold. |
| `closed` | The net has shut down. |

An error does not undo a verb that had already started.

`Shutdown` is the clean end. An interrupt (Ctrl-C) also shuts the net down; in both cases the root directory is
removed, so copy anything you need (a dump, a log) out of it first.

## `testnet ctl`

`testnet ctl` drives a served net from a shell, one command at a time. It is the only tool a QA tester has, and
its `--help` and each group's `--help` describe every command fully; this is the overview.

Run every command from the same folder, or name that folder in `TESTNET_FOLDER`, which `ctl` then uses whatever
the current folder is (a QA tester's is set). `ctl up [TOPOLOGY]` starts a net there, from a topology file or
empty, and later commands find it through the folder. A relative file a command reads (`tui paste --file`,
`tui clip`, `phone attach`) is looked for in the current folder first, then in the session's:

| In the folder | What it is |
| --- | --- |
| `.testnet/` | The net: its topology, its root (`.testnet/net`, removed by `ctl down`), `testnet.log`, and the panes' and the phone's state. |
| `session.jsonl` | Every `ctl` command run here: its time, arguments, whether it succeeded, its reply, and any files it wrote. |
| `shots/` | Every picture `tui shot` and `phone shot` took. |
| `findings/<name>/` | What `ctl capture` kept. |
| `providers/` | Real agents' recorded provider traffic, by host and agent. |

A net `ctl up` starts is served with a gated LAN, facts rings large enough to hold each agent's whole life, its
recordings under `providers/`, a relay with no accounts yet, and a discovery scope of its own (the folder's, or
`AMUX_QA_SCOPE`'s), so two folders' nets never see each other. `ctl down` stops every agent, daemon, pane and the
phone app, and removes the net's root; the session log, shots, findings and recordings stay. Every command prints
readable text; `--json` prints the door's reply instead.

A net's agents outlive a stopped daemon by 20 seconds, where a real installation waits five minutes: long enough to
ride out a restart, short enough that a failed run's agents go away on their own. A host kept down for longer ends
every agent on it, and a finished child then reads as having exited while the daemon was away, correctly; `ctl
status` says so. `host add --lan` brings discovery with it, as a daemon on a real LAN has, unless `--bonjour` puts
the host on the machine's real mDNS instead; a process host with a LAN listener is announced on the net's
discovery bus. `restart` waits longer than the net's own wait for the host's LAN port, so a restart that fails says
what holds the port.

| Group | Commands |
| --- | --- |
| The net | `up [TOPOLOGY]`, `down`, `status` |
| Topology | `host add NAME [--lan] [--discovery] [--bonjour] [--account A] [--process] [--supervised] [--provider real\|fake] [--script FILE]`, `host remove NAME`, `link A B`, `unlink A B`, `account add NAME [--tier pro\|free]` |
| Faults | `sever A B`, `restore A B`, `flap A B --down D --up U`, `flap stop`, `net HOST\|relay [--loss P] [--delay D] [--jitter D] [--reorder P] [--rate R]`, `block-udp HOST on\|off`, `kill HOST`, `stop HOST`, `restart HOST`, `freeze AGENT`, `thaw AGENT` |
| Accounts and releases | `sign-in HOST ACCOUNT`, `tier ACCOUNT pro\|free`, `release publish VERSION [--channel C] [--rollout P] [--broken]` |
| Agents | `spawn NAME --host H [--kind claude-sdk\|claude-pty\|codex] [--provider real\|fake] [--script FILE] [--prompt P] [--model M] [--permission P] [--mode M] [--parent A] [--cwd D] [--wait] [--timeout S]`, `send AGENT TEXT [--wait]`, `wait AGENT`, `stop-agent AGENT`, `resume AGENT [TEXT]`, `gate open NAME` |
| A host's own record | `fleet HOST` (its own agents and its trusted peers', as its clients show them; rows a host it no longer trusts left behind are listed apart), `chat HOST AGENT` (each item with what its body decodes as, and the asks waiting on a person), `block HOST AGENT`, `provider-input AGENT` |
| The real CLI | `amux HOST -- ARGS…`, with standard input passed through; bare `pair` runs in the background and prints its PIN and address, and `pair ADDRESS` naming one of the net's hosts goes through the pair's gate |
| Terminal client | `tui open HOST [--size COLSxROWS] [--theme light\|dark \| --palette FILE] [--name N]`, `tui keys PANE KEY…`, `tui type PANE TEXT`, `tui paste PANE TEXT\|--file FILE\|--file -`, `tui clip PANE FILE [--image]`, `tui screen PANE`, `tui shot PANE`, `tui record PANE FILE [--every D] [--for D]`, `tui resize PANE COLSxROWS`, `tui attach PANE AGENT`, `tui watch PANE`, `tui close PANE`, `tui list` |
| Phone | `phone open [--host H] [--relay]`, `phone pair HOST`, `phone connect ACCOUNT`, `phone tap ID`, `phone perform ID ACTION` (an element's named accessibility action, such as a review line's "Select line"), `phone choose LABEL` (an item of the dialog or menu that is open), `phone type TEXT --into ID`, `phone search TEXT`, `phone scroll DIRECTION [--in ID]`, `phone query`, `phone shot`, `phone appearance light\|dark`, `phone text-size SIZE`, `phone attach AGENT FILE [--image]`, `phone settle` |
| Evidence | `capture NAME [--note TEXT]`, `log [--tail N]`, `dump show BUNDLE [--agent A]` |
| Files to test with | `make-file NAME SIZE [--fill zero\|random\|text]`: a file below the folder (no `..`), up to 2g |
| Ledger | `ledger read`, `ledger findings`, `ledger claim TEXT`, `ledger idea TEXT`, `ledger finding NAME [--new\|--duplicate-of TESTER/NAME]`, `ledger done SUMMARY` |

Durations take `ms` or `s` (`200ms`, `1.5s`); a rate is bytes a second, with `k` or `m` (`64k`). `--wait` on
`spawn` and `send`, and `wait`, allow 100 seconds for a fake agent and 240 for a real one unless `--timeout` says
otherwise.

**The terminal client** runs as the real `amux` binary against a host's installation config in a tmux pane of the
folder's own tmux server, at 24-bit colour in the chosen theme. With `--palette FILE` (a JSON file of `background`,
`foreground` and the sixteen `ansi` colours, as `#rrggbb`) the pane is instead a terminal of those colours: it answers
the client's colour queries with them, the client derives its palette from them as it would from a person's
terminal (`theme: terminal`), a pane `tui attach` opens from it has them too, and shots draw the palette's colours
where a program used the terminal's own sixteen. That server reads only the net's own tmux
configuration (`crates/testnet/src/ctl/tmux.conf`), never the operator's, so keys reach the client the same way on
every machine: legacy bytes by default and xterm's modifyOtherKeys form to a program that asks for it, as Claude
Code does in `tui attach`. `tui keys` presses each key separately, as tmux names
them. `tui paste` loads the text into tmux's buffer from a file and pastes it as a bracketed paste byte for byte:
line endings arrive as written, and a paste of any size goes through. `tui shot` captures the pane with its styles, turns it into the frame format the terminal's own report writes
(`.frame.json`) and draws that to PNG with `crates/shot`; the exact text sits beside it (`.txt`). `tui record` films a
pane: each time its screen changes (checked every `--every`, 50ms by default), it appends one JSON line holding the
milliseconds since it started and the frame, until `--for` has passed, the pane closes or it is stopped. Background colour a
program paints by clearing to the end of a line is not kept, and the font has no CJK or emoji glyphs. `tui attach`
opens `amux attach` for an agent in a second pane: Claude's own interface for terminal Claude, Codex's own for Codex.
`tui watch` attaches this terminal to a pane, to watch and use the client in it by hand; tmux's C-b d leaves it
open.
`tui clip` attaches as a person does in the terminal client: it puts a file (or, with `--image`, a PNG as picture
data) on the machine's clipboard and presses Ctrl+V in the pane. On macOS the file goes as its path; on Linux it
goes through `xclip` as a file manager copies one (`text/uri-list`), on the X display named by `DISPLAY`, so a
headless Linux host needs an X server such as Xvfb and `DISPLAY` set before `ctl up`. The clipboard is the
machine's own, so `clip` holds a lock across every session for the moment the client reads it and then puts back
the text the clipboard held; an image or file copied on the machine is replaced.
`phone attach` hands the open chat's draft a file or photo as the system's photo library or file browser would; those
pickers are the system's own screens, which nothing outside them drives.

**The phone** is the debug app on the simulator the session holds a lease on (`WT_LEASE_IPHONE`); `ctl` never
takes a lease itself, so a session that drives the phone runs entirely under one
(`wt exec <tree> -- scripts/with iphone -- …`). `phone open` copies the app from
`AMUX_QA_APP`, or the tree's debug build under `target/ios`, into the net's folder, so another session rebuilding
it cannot break the install, then installs and launches that copy browsing the net's discovery scope; later
commands talk to the app's door, which serves several connections at once. `phone tap` waits up to ten seconds for
its element to be on screen and enabled, taps again while the app has not yet drawn a just-enabled control able to
take it, then waits for the screen to change and the app to settle, so the screen a tap opens is there for the next
command. `phone type` and `phone search` settle too. `phone shot` waits for two pictures a moment apart to agree, so
a sheet still sliding is not caught halfway, and takes the last one after about six seconds of a screen that keeps
moving. The phone finds hosts by real mDNS, so a host it pairs with needs `--lan` and `--bonjour`; a `phone pair`
that fails closes the pairing window it opened, so the next attempt can open one. `phone open --relay` and
`phone connect ACCOUNT` reach accounts through the net's relay instead; signing in reads what the account may do
from the net's account service, as the app does from amux.sh.

**Accounts.** The relay's account service answers sign-in, relay credentials and the plan read a phone makes
(`/api/graphql`): a Pro account has access given to it, one moved from Pro to Free had that access end when it
moved, and one that was always Free has none. `tier ACCOUNT pro|free` changes the account, and every host signed in
to it learns at once: an in-process host asks over its relay link, and a process host's profile signs out and in
again, which dials the relay afresh (its relay link drops for that moment). `status` prints each account's tier
as it is now. A process host's debug daemon trusts the relay's certificate through `AMUX_CLOUD_TLS_CA`, so it reaches
the relay where the account service names it, but not through the net's gates: `block-udp` and `net` do not touch
its way to the relay.

**Evidence.** `capture NAME` copies, never moves, everything about the moment into `findings/NAME/`: a
`finding.md` to fill in (from `qa/FINDING.md`), the topology as it stands, the session log, an `amux dump` of
every running host with the note as its reason (`dumps/<host>/`), the phone app's own dump when it is open
(`dumps/phone/`), each real agent's provider traffic (`providers/<agent>/`), a fresh shot and screen of every
pane and of the phone plus every earlier shot (`screens/`), and the net's and each process host's logs (`logs/`).
Anything it could not take is named in `errors.txt`. `dump show` prints a captured dump bundle as text with the
`dump-show` binary from `crates/replay-support`, which must be built beside `testnet`. The ledger commands are
for a [QA](QA.md) run and need its `AMUX_QA_RUN`. Inside such a run agents are real only: `host add` defaults to
`--provider real`, `host add` and `spawn` refuse `--provider fake` and `--script`, and `up` takes a topology file
only when every host in it declares `"provider": "real"` and no agent plays a script or journal.

### Environments

An environment is a named world of real agents to use the product in: `environments/NAME.json`, an ordinary topology
file with a `clients` section saying which clients come with it.

```json
"clients": {
  "phone": {"paired": ["studio", "laptop"], "account": "ada"},
  "terminal": {"host": "studio"}
}
```

`ctl up` opens what the section declares once the net is ready: the phone, paired with each listed host through the
real pairing flow (each needs `lan` and `bonjour`) and then signed in to the account if one is named, and the
terminal client in a pane against its host. A world with no `phone` never touches the simulator. The section is
ctl's alone; the net is served the file without it, and `ctl up` names a client that cannot be opened (a host that
is not declared, an account the relay does not know) before anything starts.

`just env NAME` builds what the world needs and starts it in its own session folder,
`~/.local/state/amux-env/<tree>/NAME`, taking down whatever an earlier start left there first. The folder is outside
the checkout, as a QA run's are: inside it, git would find the repository above every agent's folder. It stays in the foreground holding the net, and the simulator lease
when the world has a phone, until Ctrl-C takes the net down: a lease given back while the app still runs would let
another tree take that simulator. Everything else is `testnet ctl` from that folder in another terminal, the world
being only where a session starts: add a host, spawn an agent, sever a link, pair the phone with another host, `tui
watch` the terminal pane. Every environment is real throughout, so a QA run may start from one too.

| Environment | What it is |
| --- | --- |
| `busy` | Four hosts with six agents between them: two at work editing, one Codex reading a repository, a terminal Claude waiting on a permission, and quick ones that come to rest. The phone is paired with three hosts; `nas` stays discoverable for pairing by hand. The terminal client runs on `studio`. |
| `asks` | One host whose agents stop to ask: a Claude permission for a new file, a Claude plan, a Claude question, a terminal Claude's permission and a Codex approval. The phone is paired with it and the terminal client runs on it. |
| `first-run` | Two hosts on the network, neither paired: the empty fleet, discovery and the pairing flow, on the phone only. |

## The harness's own tests

```sh
just suite testnet-harness                 # topology validation, clock, waits, faults, growth, process hosts, the door
just suite testnet-ctl                     # testnet ctl end to end: up, hosts, a link, a fake agent, a shot, a capture, down
just suite many-daemon-protocol-spec       # the spec_* suites but spec_links
just suite link-stories                    # spec_links
```

The `spec_*` suites (modules of testnet's integration test binary) are the many-daemon specifications: pairing, trust, discovery, routing, relay carriers,
credential refresh, revocation, account isolation, the inventory, replication and agent families across hosts.
`spec_links` holds one story per route a link takes (the same network, SSH, IPv6 alone), each reaching an agent's
session, streaming it live and naming the carrying path from the link log. A story on the relay alone puts every
host on a network of its own behind a `strict` router and ends by asserting, with `still_relayed`, that each end's
link is still carried through the relay, so it cannot pass by finishing before a punch or a LAN dial moves the link.
Signing out takes only what the relay does: `route_relay_signing_out_…` closes a link the relay alone carried, while
`route_lan_signing_out_…` keeps a direct link between two hosts on one network up on `lan`, and the signed-out laptop
still reaches the desk's session over it.
Each reads top to bottom as a description of one behaviour, with its assertions in the test body.
