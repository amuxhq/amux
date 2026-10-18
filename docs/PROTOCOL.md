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
[`crates/wire/proto/amux/v1/`](../crates/wire/proto/amux/v1/): link control in
[`link.proto`](../crates/wire/proto/amux/v1/link.proto), pairing in
[`pairing.proto`](../crates/wire/proto/amux/v1/pairing.proto), the front door in
[`front_door.proto`](../crates/wire/proto/amux/v1/front_door.proto).

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
(a `RouteKind`: direct or through the relay). When no address and no relay
route reaches the other host, `BeginPair` answers `ERROR_CODE_UNREACHABLE`
naming what it tried, never `UNAVAILABLE`, which means this daemon is not
answering.
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

A route is a derivation, not an independent assertion: a host is reachable
through a relay when that relay says it has an adjacent link to the host. This
can make an untrusted host a pairing candidate, but does not grant authority or
make a discovery result online. A trusted host stays in the inventory whatever
its presence, and only untrusting it removes it.

A trusted host's presence is the outcome of opening a channel to it, decided in
one place, the daemon core's presence state (`crates/daemon-core/src/presence.rs`):

- **Online:** a channel to the host opened on a route still listed. The route
  it opened on is the one the host entry's `via` names.
- **Away:** the relay refused the channel with `PAYMENT_REQUIRED`, because the
  account's free plan carries no connection between hosts. Upgrading reaches
  the host; nothing else reads away.
- **Offline:** everything else. No route is listed, or the channel failed for
  any other reason: no route at the relay, rate limited, shutting down,
  revoked, a TLS, handshake or identity failure, a timeout. A host that said it
  no longer trusts this one is offline and carries `revoked` on its entry.

Every route that comes up is tried at once: a direct link's first channel opens
as the link's event is handled, and a relay's claim for a trusted host is tried
in the background. Every channel any call opens records its outcome too. A host
whose route has come up and whose first attempt on it has not concluded keeps
the presence it had, so a host that connects goes from offline straight to
online, and one whose link went while the relay still lists it reads online
until the try on the relay concludes. A failure counts only on the route a
fresh call would take first by then, and only while no other route carries the
host's traffic. A channel failing because its link went says nothing: the
link's end is what decides. A host no channel is open to while a route lists
it is tried again in the background, one second after the failure and then
doubling to thirty seconds between tries.

When the account's plan changes, every host the relay carried or refused is
tried again at once on a fresh channel, so a host away for the plan turns
online as soon as the account is paid, and one the relay carried stops reading
online once the plan lapses.

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

The routes are listed by the daemon core from the links it is told of
(`crates/daemon-core/src/links.rs`): a host's own links newest first, since a
second link to a peer is a crossed dial whose newer link both sides keep, then
each neighbor whose link claims the host. The adjacency discipline on the wire
is `LinkRegistry` in `crates/node/src/routing/link_registry.rs`, and what each
host said of itself is kept beside it in `crates/node/src/routing/core.rs`.

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
idle timeout are 20 and 60 seconds on iOS, 10 and 30 seconds elsewhere, so a
desktop notices a link that died silently within about forty seconds (the
shorter timeout of a link's two ends applies).

Presence says whether a channel to a host opens, so a call that cannot open
one on a route still listed does not wait for that timeout: the host reads
offline at once. On the host's own direct or SSH link the call closes the link,
as the idle timeout would, and redialling and discovery bring it back as they
do any lost link; the new link's first channel says whether the host is online
again, so a link that comes back and still carries no call leaves it offline. On
the relay, or on a link through another host, the channel cached on the route
is dropped. While a route stays listed and no channel to the host is open, one
background retry per host tries a fresh channel, one second after the failure
and doubling to thirty; an away host is retried the same way. A client
releases what it held for the host when it reads online again.

An SSH link reaches the far daemon through `amux relay`, a hidden command the
SSH session runs, which joins its stdio to the profile's `link.sock`.

### Link control

The connector opens the first bidirectional stream as control. Each control
message is protobuf encoded after a big-endian `u32` byte length and is bounded
by `MESSAGE_SIZE_LIMIT` (16 MiB). The complete control vocabulary is:

- `Hello`: supported protocol versions, this host, the current neighbor
  snapshot, the sender's runtime instance id, and an authentication token only
  for a cloud link.
- `HelloAck`: either the accepted version plus the acceptor's host, neighbor
  snapshot and runtime instance id, or an error.
- `NeighborUp` and `NeighborDown`: adjacency deltas after the handshake.
- `Reauth`: a fire-and-forget replacement token for a cloud link.
- `LinkClose`: an immediate close with a reason and optional error.

A runtime instance id is 16 random bytes a host's runtime draws when it
starts and keeps until it stops; it is not an agent's incarnation, which
counts that agent's process runs. A process that is killed or crashes closes
nothing, so its peers keep its direct links until QUIC's idle timeout. When
two direct links to the same host are live, the runtime instance ids decide
which stays:

- A link from a different runtime instance than the one already held means the
  host restarted. The held link is dead, and the later link takes its place
  whichever direction either was dialled in.
- Two links from the same runtime instance are a crossed dial. Both peers keep the
  link dialled by the lower host id and refuse the other. A second link in the
  same direction is refused too.

While a paired peer has no direct link, a host keeps dialling it: at the
addresses discovery lists for it and at the last direct address that worked,
waiting a second after a failed dial and doubling the wait each time up to
thirty seconds, whether or not discovery finds it, so peers paired by address
across a network discovery cannot see come back on their own. Each side has an
address to dial: the dialler stores the one it reached, and the side that
answered a pairing stores the address the dialler was seen at, since a host
dials from the same QUIC endpoint it listens on. A dialler whose direct link
closes within a second of coming up waits out that second before dialling
again, so a refused link is not redialled in a loop.

Versioning is equality, not feature negotiation: both peers must select
`PROTOCOL_VERSION` (`crates/wire/src/lib.rs`, currently 1). A peer offering
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
`CHANNEL_MESSAGE_SIZE_LIMIT` (65 MiB): the largest attachment,
`ATTACHMENT_SIZE_LIMIT` (64 MiB), with room for the `PutBlobRequest` framing
around it.

### Compression

Every channel to a paired host is compressed, in all three classes, on every
route: one streaming zstd compressor each way, between HTTP/2 and the channel's
TLS (or the plain stream), so a relay copies the compressed ciphertext
without knowing it is there (`crates/node/src/transport/compress.rs`). The
opener wraps its end in `ChannelPool::secure_channel`; the acceptor wraps its
end as `TunnelDispatcher` hands a paired host's stream to `PeerService`.
Pairing channels, the link's control stream and local sockets are not
compressed. Nothing on the wire announces it: both ends always compress.

A channel's messages are small and mostly repeat each other (every snapshot is
the whole snapshot again), so compressing each message alone finds almost
nothing. One compressor per channel keeps its history across every message
and every call on the channel, which removes most of it: about 5x on replayed
agent journals, twenty agents to a channel. Every HTTP/2 flush ends a
zstd block, so each message reaches the other end as bytes it can decompress
at once and a live append never waits in the compressor. Bytes that do not
compress, such as an image's, pass through as zstd's raw blocks; a blob keeps
its hash, since only the bytes in transit are compressed. Level, window and
table sizes are on [Parameters](PARAMETERS.md).

## Chapter 6: Relay forwarding and entitlement

A relay receives a non-control stream and asks its core
(`crates/relay-core`) whether to carry it: the core judges the preface, the
plan on both ends, the opener's rate and the account's adjacency, and answers
with a refusal, which is reset before a byte is copied, or the link to splice
it onto. The driver opens a stream there with the same `dst` and `plain`
cleared, waits for its acceptance so a refusal keeps its reason, and copies
bytes in both directions until each side ends
(`crates/node/src/link/piper.rs`). A link lapsing to the free plan has the
core cut every stream through it, including one still opening. It does
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
| `ResolveAgent` | A fleet-wide reference to one row: an id with or without dashes, the eight-character short id (or any longer prefix), a name, or `name@host`. `NOT_FOUND`, or `AmbiguousAgentName` with the candidates when a bare name matches agents on several hosts; the message offers the `name@host` forms that pick each. Every other agent call takes an id. |
| `Subscribe`, `Fetch`, `Get` | An agent's session stream, older pages, and one item; see [the wire](WIRE.md). |
| `SendInput` | A person's input to an agent; the answer is the interpreter's verdict. |
| `CreateAgent` | Creates an agent, on this host or on a trusted host named by id or by name. |
| `RenameAgent`, `StopAgent`, `ResumeAgent`, `DeleteAgent` | Registry operations. A stop ends the agent now (its turn cancelled the provider's own way); a delete stops it if live and removes it. Both act on the named agent alone; its children follow on their own hosts by [the family rule](#the-family-rule), so the answer says nothing of them. |
| `SendMessage` | A message to an agent, whose sender the daemon stamps from the caller; see [agent tools](AGENT_TOOLS.md). |
| `PutBlob`, `GetBlob`, `Diff` | Bytes in an agent's directory; see [attachments](ATTACHMENTS.md). |
| `GetCatalogue` | What a provider on a host offers with no agent running, from the host's own copy. What an agent offers is a blob its snapshot names, read with `GetBlob`; see [the wire](WIRE.md). |
| `ListRepositories` | Where a host offers to start an agent: recent working directories and repositories under its configured roots. |
| `Dump` | A debug report; see [debugging](DEBUGGING.md). |
| `SaveDraft`, `GetDraft` | An agent's unsent draft, kept by this machine's daemon for its own clients so it outlives a client closing; never synced, and refused to a paired host. |

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
  When only the relay reaches the owner and the account is on the free plan
  (its subscription lapsed), the relay carries no calls between hosts, and
  the call is refused at once as that: `ERROR_CODE_PAYMENT_REQUIRED` with a
  sentence naming the lapse, for `SendInput` the same rejected verdict
  with that sentence as its detail, and for `StopAgent`
  `ERROR_CODE_UNREACHABLE` with that sentence, so a client holds the stop
  as it holds any stop that never left.
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
is refused as `UNAUTHENTICATED`. Only amux daemons call it, and each stamps
what it says on behalf of its own callers, a message's sender and a create's
parent, from those callers; the answering daemon takes them as given. It
differs from `ClientService` in these ways:

- It answers only for this host's own agents. Its inventory stream carries
  this host's entry and this host's agents, never what this host knows of
  other hosts. Every call that names an agent (`Subscribe`, `Fetch`, `Get`,
  `SendInput`, `RenameAgent`, `StopAgent`, `ResumeAgent`, `DeleteAgent`,
  `PutBlob`, `GetBlob` and `Diff`) is `NOT_FOUND` for an agent this host only
  replicates, even when this host holds that agent's rows or blobs.
- `Subscribe` takes a `PeerSubscribeRequest`, which may ask for what came
  `after` a revision: that is how a replica catches up from its cursor
  ([the wire](WIRE.md) describes it).
- `SendMessage` takes the `Envelope` the calling host's daemon stamped, and
  takes its sender as given.
- `ListRepositories` answers for this host only.
- `Dump` answers with this host's part of the caller's report, a `DumpPart`,
  since the caller is on another machine.

### The family rule

A host enforces family rules only for its own agents, from the parent's row as
it holds it (an own row, or a replica kept by the inventory stream), and only
on what that row actually says (`ParentSays` and `parent_changed` in
`crates/node/src/runtime.rs`):

- **The parent's row says it exited**, whatever the cause: the child is
  stopped.
- **The parent is gone from its host's listing**: the child is stopped and
  deleted. A parent's absence counts only when the host said so: an
  `AgentRemoved` on its inventory stream, or a listing read to `CaughtUp`
  that no longer has it. A row not yet replicated is not a deleted one.
- **Anything else does nothing**: the parent's host out of reach, this host
  still catching up with it, or the hosts unpaired. The child carries on, and
  the report it owes its parent waits in the
  [deliveries outbox](AGENT_TOOLS.md#completion-and-exit) until the parent's
  host is reachable again. Unpairing drops the reports owed to that host's
  agents, which can never be delivered.

A child on the parent's own host follows the same path: there the parent's
row is a local one. The rule is checked when a parent's row changes (an own
agent exits or is deleted or removed by retention; a replica row arrives
saying the agent has just exited, or is removed), and when this host finishes
catching up with a peer after a (re)connect, which settles every own agent
whose parent lives there against that complete listing: whatever happened
while the hosts were apart is acted on then. Only agents that existed before
that listing was asked for are judged by it; a child made since may belong to
a parent the listing predates, and the live stream speaks for that parent
(`inventory_caught_up` in `crates/daemon-core/src/sources.rs`). That is safe
because no child under another host's parent is made before this host has
caught up with that host and holds the parent's row (see the create below),
so every such child was judged by a listing or by the stream after one.

A create names its parent (the caller, or for a create forwarded from another
host, the parent that host stamped). If this host already sees that parent as
exited, or as deleted (its own agent with no row), the create is refused as
`FAILED_PRECONDITION`. The parent check and the child's row are written under
one hold of the store, so a parent that ends after the check finds the child
and stops it. A parent on another host is checked only against a caught-up
row: while this host has not caught up with that host's listing since they
last connected, or holds no row for the parent yet (deleted, or made so
lately its row is still on the way), the create is refused, also as
`FAILED_PRECONDITION`, in words that say to spawn again in a moment. The
refusal is definite (no child was made), unlike an unreachable host's
uncertain answer.

A stop's or delete's answer covers the named agent only; it never confirms the
children, which may be on hosts that are away.

### The tools socket

Each agent's `tools.sock` serves the same `ClientService`, with one
difference: the socket is the caller's identity. The daemon binds it when it
starts the agent's process, and whatever arrives on it is from that agent, so
no request carries an agent id or a token claiming one. On that socket:

- `CreateAgent` makes the caller the child's parent.
- `SendMessage` stamps the caller as the message's sender.
- `SendInput` goes only to one of the caller's own direct children; the stop
  tool's interrupt is the input an agent sends.

[Agent tools](AGENT_TOOLS.md) describes the tool server that dials it.

### Agent messaging across hosts

`SendMessage` carries a `SendMessageRequest` whose `to` is a host and an
agent. The daemon makes the `Envelope`, its sender stamped from the caller
(the person, or the calling agent with this host's id and its name). When the
host is another one, it makes the call on the recipient's daemon with that
envelope. That daemon takes the sender the calling daemon stamped and delivers
through the recipient's lane exactly as it would for a local sender. A message
that arrived from a peer for a third host is not passed on. A child's
"finished" or "failed" report to a parent on another host travels the same
way, carrying the parent's incarnation so the parent's own daemon can drop it
for any other incarnation.
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

Until the first release the protobuf files change freely, as the top of
`service.proto` says: a removed field is deleted outright, leaving no
reservation, and the remaining fields are renumbered so the numbers stay in
reading order (see [the wire](WIRE.md#compatibility)). `PROTOCOL_VERSION`
exists only in the link handshake and stays 1 until then. The local sockets
carry no version at all.

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
