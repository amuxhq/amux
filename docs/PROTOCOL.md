# The amux protocol

*For developers working on the network layer, the services a client or a paired daemon calls, or anything that crosses from one host to another.*

This page covers how amux hosts find, trust and reach each other, and the
`amux.v1` services that ride on those connections and on the local sockets.
The processes around it are described in [Architecture](ARCHITECTURE.md);
the session stream a client reads is described in [the wire](WIRE.md); the
user-facing behaviour is in [How amux works](HOW_IT_WORKS.md).

The code lives in the `node` crate: `crates/node/src/link/` (carriers, link
runtime, channel pool, relay forwarding), `crates/node/src/routing/`
(adjacency and the routing table), `crates/node/src/pairing/` and
`crates/node/src/services/pairing.rs` (pairing), `crates/node/src/discovery/`
(mDNS), `crates/node/src/edge/` (a profile's network edge, the cloud link and
the relay server), and `crates/node/src/grpc.rs`,
`crates/node/src/front_door.rs` and `crates/node/src/edge/peer.rs` (the
services). The protobuf source is in
[`crates/wire/proto/amux/v1/`](../crates/wire/proto/amux/v1/amux.proto).

## The mental model

> **Carriers carry links. Links carry streams. Streams carry channels. One
> pinned handshake authenticates every channel, whatever carried it.**

The layers have separate jobs:

- A **carrier** provides an ordered control stream and independent
  bidirectional byte streams.
- A **link** is the live relationship with one adjacent node. Its control
  stream exchanges identity, protocol version, adjacency and link lifetime.
- A **stream** is one native QUIC stream or one yamux stream. Its first message
  says where it is going; after admission, its bytes are opaque to a relay.
- A **channel** is the end-to-end call connection inside that stream. Its
  pinned TLS handshake, not its carrier or route, decides call authority.

The carrier may also authenticate the adjacent link: direct QUIC uses pinned
mutual TLS, the cloud carriers use WebPKI TLS plus a token in `Hello`, and SSH
uses the authenticated SSH channel. That outer admission never stands in for
the pinned handshake inside an ordinary peer channel.

## Chapter 0: Discovery

A listening profile advertises `_amux._udp` through DNS-SD. Its SRV record
names the ephemeral or configured QUIC port; its TXT record carries three
properties: `v` (the protocol version), `hid` (the host id, hyphenated) and
`scope` (the discovery scope). Resolving it produces a name and socket-address
dial hints. Browsing is a standing subscription: a goodbye or TTL expiry
removes a result, while a fresh resolution updates its addresses. On iOS the
app browses with `NWBrowser` and feeds the results in; `node` itself does
not run mDNS there.

The scope is the installation setting `discovery.scope`, empty by default,
matched exactly: a daemon lists only candidates whose scope equals its own.
Real machines leave it empty; a worktree's daemon and a test network each set
their own, so they never see a person's laptop or phone and never see each
other.

Discovery is **never trust or presence**. An unpaired result is only a pairing
candidate, listed in the inventory as a host entry with `trust = TRUST_CANDIDATE`
and the addresses pairing will dial. A result claiming a paired host's id is
only a dial hint; the pinned handshake must still present that host's key. A
paired host is online only after a link succeeds. Discovery is queried again
at startup, after the machine wakes (a monotonic gap of more than 30 seconds),
when a direct link ends, and when a pairing window opens.

## Chapter 1: Profiles, identity and trust

A profile is one device on the wire. An installation may run several profiles,
but each has its own Ed25519 keypair, random 128-bit `host_id`, trust store,
routing state and optional cloud link. A profile and its identity survive
restart. Binding or unbinding an account does not change them.

Trust is a public-key pin in a local, never-shared store. Pairing adds pins;
local revocation removes one. Revocation closes the peer's links and their
streams immediately. Deleting a profile destroys its whole trust store.
Profiles never inherit another profile's pairing window, pins, routes or
account.

Trusted peers call the agent operations of `PeerService`
([chapter 7](#chapter-7-the-front-door-and-the-amuxv1-services)).
Installation lifecycle, profile lifecycle, trust administration and
pairing-window administration live on the installation front door, or on an
in-process owner handle where the daemon is embedded; they are never peer
calls.

## Chapter 2: Pairing

PIN and QR pairing use one SPAKE2 exchange. The PIN is six digits. The QR
contains JSON `{host_id, secret, addrs, cloud_url?}` inside an
`amux://pair?payload=...` deep link (URL-safe base64, no padding); `secret` is
a one-shot 256-bit value and `addrs` let pairing work when multicast is
unavailable. A found advertisement, a typed address, the QR addresses, SSH, or
a relay can all lead to the same exchange. Direct candidates are tried with a
two-second QUIC dial each, so a silent address does not prevent trying the
next one; the relay is tried after them when the other host has a cloud route.

SPAKE2 follows RFC 9382 over edwards25519, responder B and initiator A, with
messages B to A then A to B. Both sides hash the big-endian
`PROTOCOL_VERSION` and length-prefixed SPAKE2 messages. HKDF-SHA256 with salt
`amux-pair-spake2-v1` derives confirmation keys (`kc/A`, `kc/B`) and
ChaCha20-Poly1305 keys (`aead/A→B`, `aead/B→A`). The sealed identity contains
the public key and a name of at most 256 bytes, with direction and transcript
in the additional authenticated data. `PairingComplete` commits both stores.
Every secret failure is the same opaque `INVALID_PIN`.

Before trust exists, an open pairing window lets the dispatcher admit one
anonymous channel to `PairingService`. SPAKE2 authenticates that exchange and
creates the pins later channel handshakes check. A window lasts the length
its `StartPairing` names, five minutes when it names none, and never more
than 90 days, whatever its method. PIN and QR windows are one-shot and allow
five attempts. A demo PIN window holds a PIN the operator chose
(`amux pair --demo`) and is reusable: success does not consume it and failed
attempts do not close it. A window lives in the daemon's memory, so none
survives a restart, and a start run again afterwards opens a fresh one.
SSH pairing instead exchanges
identities over the already-authenticated SSH stream (`amux pair user@host`
runs `amux pair-recv` on the far side) and records outbound reachability only
on the side that knows how to dial it.

A client can pause SPAKE2 before granting trust. `BeginPair` (PIN or QR
secret, optional host id and addresses) returns a `PendingPair`: an
opaque token, the responder's `PairingIdentity` (host id, public key, name and
an expiry sealed inside the responder identity) and the route pairing found
(a `RouteKind`: direct or through the relay).
Neither trust store changes during this phase. The profile's pairing
administration holds the open stream behind the single-use token until that
expiry, and holds at most 32 at once. The token is a local capability, not a
trust decision a client could recreate from the identity fields it displays.
`ConfirmPair` sends the initiator's sealed identity, waits for the responder's
trust commit, stores the peer and dials it. A `ConfirmPair` or `AbandonPair`
that names no token is refused as malformed (`INVALID_ARGUMENT`); a token that
names no pending pairing reads as every other secret failure. `AbandonPair` sends a rejection
and waits for `PairingAbandoned`, which the responder sends after releasing
the attempt. Abandoning leaves existing trust unchanged and consumes no guess,
and a token left to expire grants no trust. These calls, with
`GetDeviceIdentity`, are served only by `ProfileService` on the front door,
never by a profile socket or a peer stream.

## Chapter 3: Presence

Presence comes from the link control protocol. `Hello` and `HelloAck` each
carry the sender's current adjacent-neighbor snapshot. Later `NeighborUp` and
`NeighborDown` messages are deltas from that snapshot. A cloud relay scopes
these claims to one account; a direct or self-hosted link scopes them to that
adjacent peer.

Presence is a derivation, not an independent assertion: a host is reachable
through a relay when that relay says it has an adjacent link to the host. This
can make an untrusted host a pairing candidate, but does not grant authority or
make a discovery result online. A trusted host stays in the inventory whatever
its presence; losing the last route shows it offline, and only untrusting it
removes it. A host the relay alone reaches while the account is on the free
tier shows away: the relay lists it but carries no stream to it. A profile is
one account, so this host's own tier is the peer's.

Each `Host` in a handshake or neighbor message carries the host's name, its
build version, its capabilities (whether it serves as a cloud relay, and the
agent kinds its platform runs: terminal Claude only where there is a Unix
terminal), whether it is signed in to an account, and `platform`, the operating
system it was built for in its own words. `platform` is optional: a host that
does not announce it is shown as unknown rather than as a guess. Nothing
routes or authorizes on these fields; they exist so a client can tell one
device from another and say why a host is out of reach.

## Chapter 4: Routing and failover

Routing has two rules:

1. **Advertise only adjacency.** A node tells its neighbors only about hosts
   to which it has a direct live link. It never repeats a neighbor learned from
   someone else.
2. **Forward only to adjacency.** A relay forwards a stream only when it has a
   direct live link to the stream preface's destination.

A route is therefore direct or via one adjacent relay. Non-recursive
forwarding makes routes loop-free and limits them to one relay without route
lists, hop counts or split horizon. Presence follows from the first rule; it
is not a separate wire claim. Direct routes win over relayed routes. Route
changes are make-then-break for later calls; existing channels stay attached
to their original link until that link or channel ends.

The routing table is `RoutingCore` in `crates/node/src/routing/core.rs`; the
adjacency discipline on the wire is `LinkRegistry` in
`crates/node/src/routing/link_registry.rs`.

## Chapter 5: Carriers, links, streams and channels

### Carriers

All carriers implement the same link interface
(`crates/node/src/link/carrier.rs`): one control stream, open and accept for
additional streams, finish and typed reset, and link close.

| Use | Carrier | Link admission | Migration |
| --- | --- | --- | --- |
| Direct device link | QUIC/TLS 1.3, ALPN `amux/2` | pinned mutual TLS | yes |
| Preferred cloud link | QUIC/TLS 1.3, ALPN `amux/2` | WebPKI certificate, then hello token | yes |
| Cloud fallback | TLS over TCP, then symmetric yamux | WebPKI certificate, then hello token | no |
| SSH link | symmetric yamux over SSH stdio | pinned peer reached through SSH | no |

Device QUIC has session tickets, resumption and 0-RTT disabled. It permits only
bidirectional application streams, at most 64 concurrently. The keepalive and
idle timeout are 20 and 60 seconds on iOS, 30 and 120 seconds elsewhere.

An SSH link reaches the far daemon through `amux relay`, a hidden command the
SSH session runs, which joins its stdio to the profile's `link.sock`.

### Link control

The connector opens the first bidirectional stream as control. Each control
message is protobuf encoded after a big-endian `u32` byte length and is bounded
by `MESSAGE_SIZE_LIMIT` (16 MiB). The complete control vocabulary is:

- `Hello`: supported protocol versions, this host, the current neighbor
  snapshot, the sender's incarnation, and an authentication token only for a
  cloud link.
- `HelloAck`: either the accepted version plus the acceptor's host, neighbor
  snapshot and incarnation, or an error.
- `NeighborUp` and `NeighborDown`: adjacency deltas after the handshake.
- `Reauth`: a fire-and-forget replacement token for a cloud link.
- `LinkClose`: an immediate close with a reason and optional error.

A link incarnation is 16 random bytes a host's runtime draws when it starts
and keeps until it stops. A process that is killed or crashes closes nothing,
so its peers keep its direct links until QUIC's idle timeout. When two direct
links to the same host are live, the incarnations decide which stays:

- A link from a different incarnation than the one already held means the host
  restarted. The held link is dead, and the later link takes its place
  whichever direction either was dialled in.
- Two links from the same incarnation are a crossed dial. Both peers keep the
  link dialled by the lower host id and refuse the other. A second link in the
  same direction is refused too.

A dialler whose direct link closes within a second of coming up waits out that
second before asking the local network for the peer again, so a refused link is
not rediscovered and redialled in a loop.

Versioning is equality, not feature negotiation: both peers must select
`PROTOCOL_VERSION` (`crates/wire/src/lib.rs`, currently 4). A peer offering
no common version is closed with `VERSION_MISMATCH` and the error detail
`ProtocolVersionMismatch`; clients show that host as "the other machine needs
updating", never as a silent absence. A failed or expired cloud token closes
the link with `AUTH_EXPIRED`. Reauthentication is not acknowledged; success is
silence and failure is a close.

### Stream preface and refusal

Every non-control stream starts with one length-prefixed `StreamPreface`:
`dst`, the destination host id, and `plain`, described under channel
authority below. There is no source, stream id, payload, data message or
close message in the control vocabulary. The pinned channel handshake, or
the link itself for a plain stream, proves the caller. Graceful stream finish
is close. A reset before acceptance is a refused open and carries one
`StreamRefusal` code:

- `STREAM_REFUSAL_NO_ROUTE`: the destination has no adjacent live link, or the route vanished
  while opening it.
- `STREAM_REFUSAL_PAYMENT_REQUIRED`: a cloud-relay link at either end was admitted as free.
- `STREAM_REFUSAL_RATE_LIMITED`: the relay refused the origin's stream-open rate.
- `STREAM_REFUSAL_NOT_ADJACENT`: the destination is invalid, is the relay itself, or the
  stream did not provide a valid preface.
- `STREAM_REFUSAL_SHUTTING_DOWN`: the accepting link is closing.
- `STREAM_REFUSAL_UNSPECIFIED`: an unknown or unmapped reset code.

### Channel authority and classes

After the preface is accepted, the endpoints run TLS 1.3 inside the stream.
The server reads the live trust store and the client pins the expected peer.
This is the authority decision for calls on relay QUIC, relay TCP and SSH,
and for any stream whose opener is not the host at the other end of the
link. Relays only copy its ciphertext.

No opener waits to be accepted before it sends. On every route the
handshake's first flight leaves in the same flight as the preface, and a
refusal comes back as the reset it always was, read under the handshake
instead of before it: the channel fails with the refusal's reason, exactly
as it did when the open was answered first. Only pairing, whose opener asks
for the pinned handshake outright, waits for the answer. A relay still waits
for the host to accept before it copies anything, so what it forwards and
refuses is unchanged; the opener's first bytes sit in the relay's stream
for that moment. Across a 100 ms path this takes one round trip off every
stream through the relay.

A stream from a paired host over a direct QUIC link of its own is `plain`:
the link's mutual TLS already authenticated both ends, so the stream carries
no handshake at all; the first channel bytes leave with the preface, and a
refusal comes back out of the opener's first read as `UNAVAILABLE` ("stream
refused"). The acceptor honours `plain` only on a direct QUIC link from a
host in its trust store, and attributes the stream to that host. A relay
forwards every stream with `plain` cleared, since it vouches for nobody. On
a phone reaching a host across a 100 ms path this takes two round trips and
a handshake's worth of CPU off every stream, which is what brings
reconciliation at launch inside its budget.

A server that does not hold the client's pin refuses its certificate with the
TLS `certificate_revoked` alert. Only a host that pins the server presents a
certificate there, so the alert tells it the server has revoked it. Over a
relay nothing else carries that news: no link joins the two hosts to close
with `USER_REVOKED`. The client records the revocation when it reads the
alert, fails the call as `UNAUTHENTICATED` ("the host no longer trusts this
machine"), and lists the host as revoked and offline. The mark clears when
the server next accepts the client's certificate on either route.

The channel pool (`crates/node/src/link/channels.rs`) selects a live link by
peer and route, opens a stream, performs that handshake, and gives the result
to tonic as one HTTP/2 channel. It has three channel classes:

- `Calls`: one cached channel per peer and route. The inventory subscription,
  forwarded calls, `Fetch`, `GetBlob`, `Diff` and every other one-shot call
  ride it. A cached channel whose connection ended is dropped, and the next
  call opens a fresh stream.
- `Session`: one cached channel per peer and route for the replica agents'
  `Subscribe` streams, so they neither wait behind nor hold up the host's
  calls, and a host with many agents costs one stream, not one per agent.
- `Bulk`: a fresh channel per transfer, for a caller that wants a large read
  kept apart from calls. The daemon's own paths do not use it; blob and diff
  reads ride `Calls`.

Separate streams give separate flow control, so a long subscription does not
queue behind calls at the amux layer. Channel payloads are bounded by
`CHANNEL_MESSAGE_SIZE_LIMIT` (64 MiB).

## Chapter 6: Relay forwarding and entitlement

A relay receives a non-control stream, validates its preface, looks up a direct
link to `dst`, opens a stream there with the same `dst` and `plain` cleared,
waits for its acceptance so a refusal keeps its reason, and copies bytes in
both directions until each side ends (`crates/node/src/link/piper.rs`). It does
not parse channel bytes or keep protocol-level stream ids, sources, payload
buffers or close state. Either end may open, so a host reached through a relay
can call back on the same link, and one relay can pipe between QUIC and TCP
carriers in either direction. Any node pipes a stream whose preface names
another host, under the same rules; a stream that names the node itself goes
to its own services.

The cloud relay is the same code run as `amux server start --cloud`
(`crates/amux/src/relay.rs` and `crates/node/src/edge/relay_server.rs`). It is
not an installation: no profiles, no agents, no supervisor. It accepts every
host of an account over QUIC and a TLS-over-TCP fallback, checks each host's
connection token against the account service's signing keys, advertises each
host's adjacency to the others, and forwards streams between them. The account
service that mints those tokens is a separate deployment; see
[the cloud](CLOUD.md).

Every link records how it was admitted. A tier belongs to a token-admitted
link on the cloud relay only: `CloudToken { free | pro }`. Direct device and
SSH links are `PinnedKey`. A self-hosted relay between paired peers therefore
never consults an account or tier and pipes their streams normally.

The cloud relay refuses an open with `PAYMENT_REQUIRED` when either its origin
link or its destination link was token-admitted as free. Neighbor control
continues, so a free account receives presence but no call or pairing channel.
Reauthentication may change a link's tier. The relay also refuses a missing
route, its own address, malformed prefaces and excess opens with the specific
codes above.

The cloud listener serves QUIC on UDP beside the TCP fallback using the same
WebPKI certificate and key. Devices try QUIC immediately and begin TCP after a
300 ms fallback delay; the first completed link wins, with QUIC winning a tie.
A TCP-only win records that relay host as UDP-blocked for one hour, suppressing
QUIC attempts until the memory expires. The memory is process-local.

The fallback's limits are honest. Yamux gives the same symmetric open/accept
interface and per-stream windows, but all streams still share one ordered TCP
byte stream. Packet loss can therefore cause cross-stream head-of-line
blocking; the connection cannot migrate across a network change; and losing
it ends every stream it carries. TCP is only a cloud-relay fallback. A direct
LAN whose UDP is blocked is offline unless another route, such as the relay or
SSH, is available.

## Chapter 7: The front door and the amux.v1 services

Chapters 0 to 6 move bytes between hosts. What rides on them, and on the
daemon's local sockets, is one set of gRPC services in the `amux.v1` protobuf
package, generated into the `wire` crate. A terminal client, the CLI, an
agent's tool server and a paired daemon all speak it; the agent process itself
never does, because its directory and journal are its whole interface (see
[the agent process](AGENT_PROCESS.md)).

### Who listens where

| Endpoint | Services | Who dials it |
| --- | --- | --- |
| The installation front door | `ProfileService`, `InstallationService` | the CLI and the terminal client, to find profiles and manage them |
| A profile's client socket, `<data_dir>/profiles/<id>/sock` | `ClientService` | the terminal client and the CLI, at the path the front door reports as `ProfileInfo.socket_path` |
| An agent's `agents/<id>/tools.sock` | `ClientService`, with that agent as the caller | the agent's MCP tool server, and nothing else |
| A profile's `link.sock` | a link, not a service | `amux relay`, carrying an SSH link |
| A link stream whose handshake presented a pinned key | `PeerService` | paired daemons, including a phone's embedded daemon |
| A link stream admitted by an open pairing window | `PairingService` | a host pairing with this one |

The front door's path is the installation setting `front_door_socket`. By
default it is `amux.sock` in a per-user runtime directory: `$TMPDIR/amux/` on
macOS, `$XDG_RUNTIME_DIR/amux/` on Linux, `/tmp/amux-<uid>/` when neither is
set, and the named pipe `\\.\pipe\amux-<user>` on Windows. Local sockets are
Unix domain sockets on Unix and named pipes on Windows
(`crates/agent-dir/src/local_socket.rs`); a path too long for a Unix socket
address is reached through a short symbolic link in the per-user runtime
directory, so both ends derive the same address from the same path.

An agent's `ctl.sock` and `pty.sock` are not part of this surface. The daemon
dials `ctl.sock` to hand the agent its inputs, and a terminal on the same
machine dials `pty.sock` directly for the raw screen; neither crosses a link,
and a remote host has no raw terminal attach.

On a phone the same daemon runs in the app's process (`crates/app-embedded`).
It serves no sockets: the app calls `FrontDoor` and `ClientApi` in process,
and it is an ordinary peer to the desktops it pairs with.

A client that finds nothing on the front door does not start a daemon itself.
Under a supervisor (`supervisor: on`) it starts `amux supervise` if none is
running, which starts the daemon; otherwise it says the daemon belongs to the
service manager and waits for it.

### The front door

`ProfileService` lists, watches, creates, binds, signs out, pauses, resumes,
renames and deletes profiles; opens, reports and cancels pairing windows
(`StartPairing`, `GetPairingStatus`, `CancelPairing`); runs the initiator's
side of pairing (`BeginPair`, `ConfirmPair`, `AbandonPair`); reports this
device's identity; records a peer an SSH exchange authenticated
(`TrustSshPeer`); and lists, describes and unpairs peers. A call about one
profile names it by id, and profile and host ids travel as their 16 bytes; the
CLI reads and prints their text form. `WatchProfiles` sends every profile,
`CaughtUp`, then each change in the order it happened; a watcher that falls
behind is ended with `ABORTED` and watches again. `InstallationService`
answers `GetInfo` (the daemon's version) and `Shutdown`. None of this is
reachable from a link. The implementation is
`crates/node/src/front_door.rs`.

No client retries a front-door call. A call whose reply is lost, which on a
local socket means the daemon went away, fails, and the person runs the
command again. Rename and delete carry the revision the caller read and
refuse a profile that changed since, so a re-run cannot undo a change made
meanwhile.

### ClientService

`ClientService` is the whole client-facing surface of one profile:

| Call | What it does |
| --- | --- |
| `SubscribeInventory` | The host set and every agent row: current state, `CaughtUp`, then deltas. A listing is this stream read to `CaughtUp`. |
| `ResolveAgent` | A fleet-wide name to one row, `NOT_FOUND`, or `AmbiguousAgentName` with the candidates. Every other agent call takes an id. |
| `Subscribe`, `Fetch`, `Get` | An agent's session stream, older pages, and one item; see [the wire](WIRE.md). |
| `SendInput` | A person's input to an agent; the answer is the interpreter's verdict. |
| `CreateAgent` | Creates an agent, on this host or on a trusted host named by id or by name. |
| `RenameAgent`, `StopAgent`, `ResumeAgent`, `DeleteAgent` | Registry operations. Delete cascades to children. |
| `SendMessage` | A message to an agent, whose sender the daemon stamps from the caller; see [agent tools](AGENT_TOOLS.md). |
| `PutBlob`, `GetBlob`, `Diff` | Bytes in an agent's directory; see [attachments](ATTACHMENTS.md). |
| `GetCatalogue` | What a provider on a host offers with no agent running, from the host's own copy. What an agent offers is a blob its snapshot names, read with `GetBlob`; see [the wire](WIRE.md). |
| `ListRepositories` | Where a host offers to start an agent: recent working directories and repositories under its configured roots. |
| `Dump` | A debug report; see [debugging](DEBUGGING.md). |

The implementation is `ClientApi` in `crates/node/src/grpc.rs`, a thin mapping
onto the profile runtime. It holds the runtime weakly, so a connection that
outlives its runtime is answered `UNAVAILABLE` and the client redials.

### Calls on another host's agents

Only the host that owns an agent can act on it. When a call names an agent
whose row here is a replica, `ClientApi` makes the same call on the owner's
daemon through `PeerService`, exactly as a phone would, and answers with what
the owner answered (`crates/node/src/forward.rs`). A forwarded call fails in
one of two ways, and they are kept apart because only the second may have
reached the owner:

- **Not sent.** The call never left: no link to the owner, the owner is not
  trusted, or no channel opened within 10 seconds. That is
  `ERROR_CODE_UNREACHABLE` naming the host by the name it was paired under,
  except for `SendInput`, which answers with a rejected verdict,
  `REFUSAL_HOST_UNREACHABLE`, so a composer treats it like any other refusal.
- **Uncertain.** The call went out and its answer did not come back: the
  link failed under it, or 30 seconds passed (enough for a spawn, which waits
  for the child's process to start on the far side). That is
  `ERROR_CODE_ABORTED` for every call, `SendInput` included: the sender holds
  the input as uncertain and settles it at its next catch-up, and nothing
  sends it again.

Forwarding goes one hop. `PeerService` answers only for this host's own
agents and never passes a call on, so no two hosts can bounce a call between
them, and no host reaches a third host's agent, or speaks for one, through
another.

`GetBlob` is answered from this host first: a replica's blob that was fetched
before is read from the replica directory. Otherwise the owner is asked, the
bytes are checked against their hash, and they are kept under the replica for
the next reader.

`CreateAgent` whose `host` names another host, by id or by name, becomes the
same create on that host, its kind's settings passed on as the bytes they came
as. A name is resolved against this host and the trusted
hosts, an exact match first and then one ignoring case; no match is
`NOT_FOUND` listing the known names, and several are `AmbiguousHostName`
with the candidates.

`CreateAgent` with `new_worktree` starts the agent in a new git worktree,
made by the agent's host before the agent exists (`crates/node/src/worktree.rs`).
The client says only yes: the host puts the worktree under its installation
folder, `worktrees/<repository>/<agent name>`, on a new branch named after the
agent, made from whatever the chosen folder has checked out, and records that
branch in the agent's spec as the base its git facts measure against. A
folder outside a repository or with no branch checked out is
`FAILED_PRECONDITION`, and a name a branch already has is `ALREADY_EXISTS`;
the name needs no translating, since every agent name is valid as a branch
and a folder (lowercase letters, digits and hyphens, `wire::agent_name_problem`);
either way no agent is made. Renaming the agent leaves its branch alone, and
amux never removes a worktree or its branch. The worktree is made behind one
seam, `MakeWorktree`, so another tool could make it instead; the daemon takes
the folder, branch and base it gets back as given.

### PeerService

`PeerService` (`crates/node/src/edge/peer.rs`) is the daemon-to-daemon
service; replication is a daemon being a client of its peer. The caller is the
host whose pinned key the stream's handshake presented, and a call without one
is refused as `UNAUTHENTICATED`. It differs from `ClientService` in these
ways:

- It answers only for this host's own agents. Its inventory stream carries
  this host's entry and this host's agents, never what this host knows of
  other hosts; `Subscribe`, `Fetch`, `Get` and `SendInput` for an agent this
  host only replicates are `NOT_FOUND`.
- `Subscribe` takes a `PeerSubscribeRequest`, which may ask for what came
  `after` a revision: that is how a replica catches up from its cursor
  ([the wire](WIRE.md) describes it).
- `CreateAgent` refuses a parent that is not one of the calling host's own
  agents: a host creates children only for its own agents or for the person.
- `SendMessage` takes the `Envelope` the calling host's daemon stamped, and
  takes an agent sender from it only if that agent belongs to the calling
  host; a host speaks only for its own agents.
- `ListRepositories` answers for this host only.
- `Dump` answers with this host's part of the caller's report, a `DumpPart`,
  since the caller is on another machine.

### The tools socket

Each agent's `tools.sock` serves the same `ClientService`, with one
difference: the socket is the caller's identity. The daemon binds it when it
starts the agent's process, and whatever arrives on it is from that agent, so
no request carries an agent id or a token claiming one. On that socket:

- `CreateAgent` makes the caller the child's parent.
- `SendMessage` stamps the caller as the message's sender.
- `SendInput` goes only to one of the caller's own direct children; the stop
  tool's interrupt is the only input an agent sends.
- `RenameAgent`, `StopAgent`, `ResumeAgent`, `DeleteAgent`, `PutBlob`, `Diff`,
  `ListRepositories` and `Dump` are refused with `PERMISSION_DENIED`: those are
  a person's acts.

[Agent tools](AGENT_TOOLS.md) describes the tool server that dials it.

### Agent messaging across hosts

`SendMessage` carries a `SendMessageRequest` whose `to` is a host and an
agent. The daemon makes the `Envelope`, its sender stamped from the caller (the
person, or the calling agent with this host's id, name and kind). When the
host is another one, it makes the call on the recipient's daemon with that
envelope. That daemon accepts an agent sender only if it
belongs to the calling host, then delivers through the recipient's lane
exactly as it would for a local sender. A message that arrived from a peer
for a third host is not passed on. A child's "finished" or "failed" report to
a parent on another host travels the same way, carrying the parent's
incarnation so the parent's own daemon can drop it for any other incarnation.
[Agent tools](AGENT_TOOLS.md#across-hosts) describes the whole path.

### Errors

A refusal is a `wire::Error`: an `ErrorCode`, a message, and typed details
named by their protobuf message type (`AmbiguousAgentName`,
`AmbiguousHostName`, `ProtocolVersionMismatch`). On the gRPC status the whole
error travels in the details, beside a coarse gRPC code for generic clients
(`crates/node/src/grpc.rs`, `status`). `UNAVAILABLE` means this daemon is not
answering, which a caller retries; another host being unreachable, and a
failed precondition, both map to `FAILED_PRECONDITION`, because they are
answers, not outages. `PAYMENT_REQUIRED` maps to `PERMISSION_DENIED`.

The protobuf files follow three rules, stated at the top of `amux.proto`:
never renumber a field, never reuse a number, never make a field required.
`PROTOCOL_VERSION` exists only in the link handshake and changes only for a
deliberate break. The local sockets carry no version at all.

## Chapter 8: Diagnostics

`Dump` on the profile socket writes a debug report on this host (rows, store
slices, journal tails and each agent process's own redacted part) and answers
with its path. For an agent another host runs, the report holds only this
host's row and store slice, and asks that host for its side through
`PeerService.Dump`; the answer is that host's own bundle as one `DumpPart`,
and a host that cannot be reached is named in the
report's manifest. Diagnostics use the ordinary calls; they create no extra
link messages or authority path. [Debugging](DEBUGGING.md) covers reading a
report.

## Chapter 9: Blob routing

`GetBlob`, `PutBlob` and `Diff` on another host's agent, and `GetCatalogue` for
another host's provider, are forwarded calls like any other, on the `Calls`
channel. The bytes are content-addressed, so a relay never needs to understand
them and the reader verifies them against the hash it asked for. An agent's
catalogue is one of its blobs, so it is never decoded on the way and keeps its
hash through a daemon of any version. Where blobs live and how long they last
is described in [attachments](ATTACHMENTS.md).

## What the wire deliberately does not have

There are no transitive neighbor advertisements, route lists, source routes,
hop caps, split horizon, per-channel forwarding records, stream data messages,
housekeeping acknowledgements, a second pairing protocol, transitive trust,
transitive presence, or multi-hop call forwarding. Link loss closes its
streams; callers reconnect over the best current route instead of trying to
splice byte streams between links.

That small vocabulary is the point: adjacency chooses where bytes may go, the
stream carries them, and the pinned channel handshake decides who may call.
