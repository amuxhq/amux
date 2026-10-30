# node

The daemon around the state-machine cores. Each profile's protocol
decisions are its daemon core's (`crates/daemon-core`): the store, the
registry, replication, families and deliveries, presence and routes (what a
host reads as from the links up to it, which route a call takes, and which
hosts to dial through a forwarder), pairing and trust, the host catalogue and the host set the inventory carries. Which
streams a relay carries, and the adjacency every link follows, are the
forwarding core's (`crates/relay-core`). node is their driver: it turns
sockets, files, processes and the clock into decoded inputs, carries out the
outputs each step asks for, and serves the calls clients and paired hosts
make. The cores never lock, spawn, await or read the world; node does all
four, and this page says where and why.

The boundary rule is the architecture's
([the mental model](../../docs/ARCHITECTURE.md#the-mental-model)): anything
whose interleaving with a core's state can matter is in the core; deadlines,
retries and streaming mechanics, transport and codec stay with the driver;
the driver performs a handshake with the key the core gave it, and token and
certificate checks are cryptographic mechanism.

## Driver policy

What node still decides is mechanism under that rule, not protocol state a
client is told. Each module below owns one such policy.

| Module | Policy | Why it is the driver's |
| --- | --- | --- |
| `services/reachability.rs` | Making the one attempt the core asks for (`Output::Dial`): its first flight on every path at once beside SSH where the host has a destination, the first answer taken, and how it ended reported (`Input::AttemptEnded`). Also the deadlines inside one handshake that nothing is decided on: the link's hello, the SSH handshake. | Which hosts to dial, on which paths, when, how long an attempt may run and how long to pause after failures are the core's (`daemon_core::reach`). Opening connections and reading a TLS alert as "revoked" is mechanism. |
| `edge/cloud.rs`, `edge/token.rs` | Making the relay attempt the core asks for (`Output::DialRelay`): the token, kept between launches and presented while more than five minutes from expiry, else fetched; the carriers the core chose, raced; the link run until it ends; how it ended, and what QUIC came to, reported. | When to dial the relay again, the carriers and the memory of a host that drops UDP are the core's (`daemon_core::reach::relay`), as are the credential's expiry and refresh (`Input::Credential`, `TimerId::Refresh`). |
| `edge/admin.rs` | Reaching a host to pair with: its pairing addresses, then the relay. | Dialling for pairing is transport; the pairing window, the exchange and every trust change are the core's. |
| `link/run.rs`, `routing/link.rs` | The link handshake: protocol version negotiation, refusing a peer that is not the one dialled or that claims this host's own id, a relay's token check and its same-user reauthentication. | The handshake is codec and cryptographic mechanism; the core is told the link once it is registered and what the peer's handshake said. |
| `dispatcher.rs`, `link/relaying.rs` | Which certificate class an arriving connection is (a paired host, a pairing client), and which links this host sends its own packets through a forwarder on. | Certificate checks are mechanism; which pairs a relay or daemon forwards between is the forwarding core's. |
| `grpc.rs`, `forward.rs`, `edge/peer.rs`, `relay.rs` | Serving calls: looking up which host owns an agent before a call, forwarding one hop to the owner, mapping core and peer answers to gRPC statuses, stamping a message's sender from the socket it came in on, refusing a person's input to an agent that has exited. | RPC mechanics. A person's input goes straight to the agent's control connection and its interpreter's verdict is the answer, so the exited row is the only refusal there is, read before the write. |
| `runtime.rs` | Refusing a spawn on a host whose announced capabilities do not list the agent's kind. | A check of the host's own announced word, worded for the person before a call goes out; the far host's own create makes the same check against its capabilities. |

Deadlines are the driver's too, by the same rule: how long a call waits to
reach a host before it closes the host's links (`forward.rs`), how long a page asked of a peer may take (`driver/mod.rs`), how long a message waits for
its verdict on the agent's connection (`runtime.rs`). So are the largest blob
the driver will send and the replica blob files' least-recently-used eviction
against their budget (`retention.rs`): the core decides which rows are kept,
and the files are the driver's.

## Tasks

Every task or thread node starts, by module, and why it is one. A core never
awaits, so whatever waits on a socket, a file, a process or the clock is a
task here.

| Module | Task | Why |
| --- | --- | --- |
| `driver/mod.rs` | The `amux-core` thread stepping the profile's core | The core is synchronous: it blocks on its input channel and steps on one thread of its own. |
| `driver/mod.rs` | One per armed timer | A timer is an awaited sleep; cancelling the timer aborts it. |
| `driver/mod.rs` | One per job (checkpoint, fsync, worktree, push, blob, spec, compare) and one per dump bundle; blocking writes on `spawn_blocking` | Jobs do file and network IO the core must not wait on; each result comes back as `JobDone`. |
| `driver/mod.rs` | One per peer call the core makes, streaming or unary | A call to a peer awaits the network; the core's `HangUp` aborts it. |
| `driver/mod.rs` | Serving a `GetBlob` from its file | The bytes never pass through the core. |
| `driver/mod.rs`, `driver/calls.rs` | Waiting for room on a full core input channel (`post`, a dropped call's cancel) | The caller is synchronous and the input must not be lost. |
| `driver/pairing.rs` | Reading a pairing conversation's stream into the core | Awaits the stream's next message. |
| `connection.rs`, `link/channels.rs` | One per closing of a host's links after a call found no channel, or of a link a stream would not open on | Closing a link awaits the link registry. |
| `runtime.rs` | Per agent: the control writer, the watcher reading its control connection, the reaper awaiting its exit, starting its process, awaiting a message's verdict | Each awaits the agent's socket or process. |
| `runtime.rs` | The repositories scan; the daily check-in | Blocking directory walk; a sleep until the check-in is due. |
| `retention.rs` | The retention sweep loop | Sleeps between sweeps and removes files. |
| `offers.rs` | Writing a provider's offer copy | Blocking file IO. |
| `link/quic.rs`, `link/framed.rs` | Per link: the connection driver, the control stream, the close watcher | A carrier must be polled continuously. |
| `link/run.rs` | Per link: running it; per inbound stream: delivering it here or piping it on | A link lives as long as its carrier; a stream waits on its handshake or pipe. |
| `link/channels.rs` | The inbound stream loop and one per inbound stream | Awaits streams and their handshakes. |
| `routing/link_registry.rs` | Waiting for room on a full link output queue | Backpressure from a synchronous caller. |
| `dispatcher.rs`, `edge/relay_server.rs` | Accept loops (QUIC, TCP) and one per accepted connection; the relay's drain of streams addressed to itself | Each awaits accepts, a TLS handshake under its deadline, or a link's life. |
| `edge/mod.rs` | The SSH link socket's accept loop and its links; an in-process loopback link; the pairing service; following the account's plan | Each awaits a socket, a server or a watch. |
| `edge/peer.rs`, `grpc.rs`, `front_door.rs` | The peer, client and front-door gRPC servers | Servers await connections for their lifetime. |
| `edge/cloud.rs` | The relay attempt's task; the losing QUIC dial after TCP won | A network dial and the link it runs; the losing dial is still in flight. |
| `edge/account.rs` | A token exchange and the save of its rotated refresh token | Detached so a caller's cancellation cannot land between the two and lose the token. |
| `services/reachability.rs` | The discovery event loop; one per dial; redialling once a dial in flight settles; one per host dialled through forwarders | Each awaits discovery, a link's establishment or a backoff. |
| `discovery/mdns.rs` | The browse receiver thread and the wake monitor thread | The mDNS library's channel and the sleep detector block. |
| `daemon.rs` | Republishing a profile's description when its edge's status moves | Awaits a watch. |
| `supervisor/*` | The child's readiness pipe, its exit wait (`waitpid` or the Windows wait), the control socket's accept loop and its connections, the update check, the Windows keep-awake thread | Blocking OS waits, a socket, a network fetch; the sleep assertion belongs to one thread. |

## Locks

Every lock node holds as state, by module, and why. Most are mirrors of what
a core published, which the core thread writes and the shell reads, or
slots shared between the tasks above.

| Module | Lock | Why |
| --- | --- | --- |
| `trust.rs` (`SharedTrustStore`), held by `driver`, `edge`, `dispatcher.rs`, `identity.rs`, `link/channels.rs`, `services/reachability.rs` | One `RwLock` per profile over the trust store as the core last published it | The core thread writes it on `ShellWork::Trust`; handshakes and certificate verifiers on tokio's workers read it at once. |
| `runtime.rs` | `hosts` | The host set the core last published (`ShellWork::Hosts`), read by the gRPC handlers. |
| `driver/mod.rs` | `reports`, `outgoing`, `connections` | Links, channels, timers and peer calls queue reports the core thread drains; the core thread registers peer calls it may hang up and the shell aborts them all at shutdown; the edge attaches its connection manager once. |
| `driver/calls.rs` | The call table and each call's answer queue | The core thread answers and ends calls; gRPC handlers open them and their stream tasks take the answers. |
| `driver/pairing.rs` | The pairing streams carried | gRPC pairing handlers add them; the core thread's pairing outputs write to and close them. |
| `link/relaying.rs` | `Relaying`: the forwarding core and its forwarder's links | A relay shares one forwarding core across every account and every link task steps it; its step budget is 1 ms, so a short lock is cheaper than a thread to queue on, and a lapse reaches the pair table in the same step. |
| `routing/link_registry.rs` | `state`, the live links | Link tasks register and remove them; channel openers, relaying and closing read them. |
| `link/channels.rs` | Cached channels, one opener lock per channel key, each channel's cancellation, the test hooks for held bulk answers and refused peers | Concurrent calls share a channel and wait on one opener; the core's route drops cancel what ran on a route. |
| `link/quic.rs`, `link/relay_host.rs` | The inbound stream receiver; the control stream halves | A carrier is shared, so its receiver is awaited and its control stream taken once through `&self`. |
| `link/run.rs` | An accepted link's authenticated user; the establishment signal; the entitlement refresh receiver | The link's reauthentication writes what admission and the expiry timer read; the signal is taken once by whichever side ends first; the refresh receiver passes from one cloud link run to the next. |
| `transport/io.rs`, `transport/single_io.rs` | Live trusted connections per peer; the one stream handed to tonic | Accepts register and drops remove connections while trust changes close them; tonic takes the stream from its own task. |
| `runtime.rs` | `launch`, `agents`, `background`, `recent`, `join_hook`, `replicas` (source policy, open sources, when each replica was last read), `replica_blobs` | Settings and live agent handles read and written from gRPC handlers, the core thread and tasks; replica files and their recency are the driver's, handed to the core's retention as an input. |
| `runtime.rs` (`AgentHandle`) | The control socket's write half; replies and dumps awaiting the agent; its pids; why it failed to start; its tasks | The writer, the watcher, callers and kill paths each reach the one agent from their own task. |
| `offers.rs` | Each provider's offer and each folder's; one helper run per provider | Refreshes, spawns and the host entry read and write them; the run lock keeps one catalogue subprocess per provider. |
| `edge/account.rs` | The account record; the access token; one change at a time | Admin handlers, the cloud link and the host entry share them; a refresh's exchange and save must not interleave with a sign-out. |
| `edge/mod.rs`, `edge/cloud.rs` | The running cloud link and its attempt; the edge's tasks; the refresh receiver | Start, stop, refresh and the core's dial paths hold the link across its stop. |
| `edge/relay_server.rs`, `dispatcher.rs` | Each relay account's state; QUIC retry admission | Accepting link tasks create and read accounts; the accept path admits addresses not yet validated. |
| `auth/jwt.rs` | The signing keys and when they were fetched | Many link tasks validate tokens while one refreshes the keys over the network. |
| `services/reachability.rs` | The attempt running for each host, the links it brought up, the shutdown signal, the discovery handle and QUIC endpoint set at edge start | Attempt tasks, calls waiting on a wanted host's attempt, and the core's `Dial` and `Abandon` outputs meet here. |
| `discovery/*` | Hosts found, the advertised name, mDNS names resolved, the scripted bus | Browse tasks write and dialers and listings read; the scripted bus is shared by every profile in a test. |
| `daemon.rs` | The hosted profiles; one registry change at a time; publishing a profile's description | Front-door handlers start, stop and change profiles across awaits; a watcher's snapshot and subscription see one moment. |
| `telemetry.rs` | Turn counts since the last check-in | Ingest adds, the check-in takes. |
