# The amux protocol

*For developers working on the network layer, the services a client or a paired daemon calls, or anything that crosses from one host to another.*

This page covers how amux hosts find, trust and reach each other, and the
`amux.v1` services that ride on those connections and on the local sockets.
The processes around it are described in [Architecture](ARCHITECTURE.md);
the session stream a client reads is described in [the wire](WIRE.md); the
user-facing behaviour is in [How amux works](HOW_IT_WORKS.md).

The driver's code lives in the `node` crate: `crates/node/src/link/` (the link
socket and its paths, carriers, the link runtime, the channel pool, the
forwarder, tickets and early data), `crates/node/src/routing/`
(adjacency and the routing table), `crates/node/src/pairing/` and
`crates/node/src/services/pairing.rs` (pairing), `crates/node/src/discovery/`
(mDNS), `crates/node/src/edge/` (a profile's network edge, the cloud link and
the relay server), and `crates/node/src/grpc.rs`,
`crates/node/src/front_door.rs` and `crates/node/src/edge/peer.rs` (the
services). The protobuf source is in
[`crates/wire/proto/amux/v1/`](../crates/wire/proto/amux/v1/): link control in
[`link.proto`](../crates/wire/proto/amux/v1/link.proto), pairing in
[`pairing_service.proto`](../crates/wire/proto/amux/v1/pairing_service.proto),
the front door in
[`installation_service.proto`](../crates/wire/proto/amux/v1/installation_service.proto),
each profile's administration in
[`profile_admin_service.proto`](../crates/wire/proto/amux/v1/profile_admin_service.proto),
and the peer services in
[`peer_service.proto`](../crates/wire/proto/amux/v1/peer_service.proto).

## The promises, as properties

The decisions this page describes are made in two state-machine cores, and
`node` is their driver: the daemon core
([`crates/daemon-core`](../crates/daemon-core/README.md)) decides pairing and
trust, presence and routes, replication, families and deliveries; the
forwarding core ([`crates/relay-core`](../crates/relay-core/README.md))
decides which pairs of hosts a relay or a daemon forwards packets
between; the path machine ([`crates/path-core`](../crates/path-core/README.md))
decides which path each link's packets take. The promises are written once, as
properties the checker explores over those cores in
[`crates/models`](../crates/models/README.md), and the sentences there are
the specification; this page explains them. `cargo run -p models --bin
properties` lists every one. The main machines:

| Chapter | Machine | Some of its promises |
| --- | --- | --- |
| [Presence](#chapter-3-presence), [routing](#chapter-4-routing-and-failover) | presence and routing | presence from links; a claimed host is dialled; a host with a working route eventually reads online |
| [Replica rows](JOURNAL_AND_STORE.md#own-rows-and-replica-rows) | replication | the block is contiguous; the block agrees with the origin; no row survives from another generation; the cursor is complete; a replica whose origin and link fall quiet becomes current |
| [The family rule](#the-family-rule) | family and deliveries | no child is deleted by a stale listing; an outbox row is deleted only on accept; each report reaches its parent exactly once; nothing is owed after unpair; every descendant of a stopped parent eventually stops on every reachable host |
| [Paths](#the-link-socket-and-its-paths) | the path machine | a path carries the link only after a probe on it was answered; the link never leaves a working path for another within the hold-down; the relay path stays ready while the link is direct; after a lapse no packet goes on a path across networks; if any path works the link eventually uses one |
| [Forwarding](#chapter-6-relay-forwarding-and-entitlement) | forwarding per pair | packets flow between two links only while both are up, on one account and paid, or both pinned; nothing is forwarded for a link not yet admitted, held packets included; a lapse or expiry removes every pair through the link and tells both ends |
| All of them at once | composed net | several daemons and their agents, every machine's promises judged together under link, crash and loss faults |

A test that restates one of these rules is a second copy and goes; what the
models do not reach (a real carrier, a handshake, a relay's token check)
keeps its tests, listed in [`tests/contracts.toml`](../tests/contracts.toml).

## The mental model

> **Paths carry links. Links carry streams. Streams carry channels. One
> pinned handshake per link authenticates every channel on it, whatever path
> carried its packets.**

The layers have separate jobs:

- A **path** is one way a link's packets travel: straight to one of the
  peer's addresses, punched through the routers in between, through a
  forwarder (the relay or a paired daemon) behind a header naming the other
  end, or framed over a byte stream (TLS on TCP to the relay, SSH, an
  in-process pipe). One path carries a link at a time, and the link moves
  between paths without its streams noticing.
- A **link** is one QUIC connection between two hosts, and there is one per
  pair of hosts that trust each other, end to end whichever path carries
  it. Its handshake is pinned mutual TLS 1.3 with the keys exchanged at
  pairing; its control stream exchanges protocol version, adjacency,
  addresses and link lifetime.
- A **stream** is one QUIC stream on the link. It carries no preface: it
  belongs to the host at the other end of its link.
- A **channel** is one HTTP/2 call connection inside a stream. The link's
  handshake, not its path or route, decides call authority.

A host's link to the relay is a link too, admitted by WebPKI TLS and a
token in `Hello` rather than a pin; it carries the relay's adjacency and
forwarding, and the links between hosts travel through it as packets the
relay cannot read.

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
candidate, listed in the inventory as a host entry whose standing is
`Candidate`, carrying the addresses pairing will dial. A result claiming a paired host's id is
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

Trusted peers call the agent operations of `PeerAgentService` and `PeerSessionService`
([chapter 7](#chapter-7-the-front-door-and-the-amuxv1-services)).
Installation and profile lifecycle live on the installation front door;
a profile's trust and pairing-window administration live on that profile's
own socket; where the daemon is embedded, the app calls the same in
process. None of them is ever a peer call.

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
route reaches the other host, `BeginPair` answers `FAILED_PRECONDITION` with
reason `HOST_UNREACHABLE`, naming what it tried, never `UNAVAILABLE`, which
means this daemon is not answering.
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
`GetDeviceIdentity`, are served only by `ProfileAdminService` on the
profile's own socket, never on an agent's tools socket or a peer stream.

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

A trusted host's presence follows from the links up to it, decided in one
place, the daemon core's presence state (`crates/daemon-core/src/presence.rs`):

- **Online:** a link of the host's own is up, whether its packets go on the
  network, over SSH or through a forwarder end to end. The kind of path that
  link takes is the one the host entry's `online` reach names.
- **Away:** no such link is up, and the relay last refused to forward to the
  host with `PAYMENT_REQUIRED`, because the account's free plan carries no
  connection between hosts. Upgrading reaches the host; nothing else reads
  away. A link of the host's own coming up, the relay saying it holds no link
  to the host, or no route listing it any more ends it.
- **Offline:** everything else. A host that said it no longer trusts this one
  is offline, with `revoked` set on its entry.

A host with no link of its own that a relay or a paired daemon claims to reach
is dialled through it: when the claim comes up, when the host's own last link
goes while the claim is listed, and, for a host refused for the plan, when the
account's plan changes. The dialler keeps trying, one second after a failure
and doubling to thirty seconds between tries, until a link of the host's own
is up or no host claims it. A host this profile does not trust, or one that
said it no longer trusts this one, is not dialled.

Each `Host` in a handshake or neighbor message carries the host's name, its
build version, its capabilities (whether it serves as a cloud relay, and the
agent kinds its platform runs: terminal Claude only where there is a Unix
terminal), whether it is signed in to an account, and `platform`, the operating
system it was built for: macOS, Linux, Windows or iOS. A host that announces
none (`PLATFORM_UNSPECIFIED`) is shown as unknown rather than as a guess. Nothing
routes or authorizes on these fields; they exist so a client can tell one
device from another and say why a host is out of reach.

## Chapter 4: Routing and failover

Routing has two rules:

1. **Advertise only adjacency.** A node tells its neighbors only about hosts
   to which it has a direct live link. It never repeats a neighbor learned from
   someone else.
2. **Forward only to adjacency.** A relay, or a daemon forwarding for its
   paired hosts, forwards a packet only when it has a live link to the host
   the packet's header names, and only between the pairs its core installs.

A route is therefore direct or via one adjacent forwarder. Either way the two
hosts hold one link of their own, pinned at pairing: through a forwarder its
packets travel behind a header naming the other end, and the forwarder reads
nothing else of them. A link through a forwarder lists no neighbors of its
own, so forwarding never takes a second hop. Non-recursive
forwarding makes routes loop-free and limits them to one relay without route
lists, hop counts or split horizon. Presence follows from the first rule; it
is not a separate wire claim. Direct routes win over relayed routes. Route
changes are make-then-break for later calls; existing channels stay attached
to their original link until that link or channel ends.

The routes are listed by the daemon core from the links it is told of
(`crates/daemon-core/src/links.rs`): a host's own links newest first, since a
second link to a peer is a crossed dial whose newer link both sides keep, then
each neighbor whose link claims the host. The adjacency discipline on the wire,
which link of a crossed dial stays and what each link is told of its
neighbors, is the forwarding core's (`crates/relay-core`), which every host's
link registry steps. What each host said of itself, the cap on how many
hosts this profile does not trust it holds, and which hosts said they no longer
trust this one are the daemon core's too (`crates/daemon-core/src/catalogue.rs`),
and so is the host set the inventory carries
(`crates/daemon-core/src/host_set.rs`).

## Chapter 5: Carriers, links, streams and channels

### Carriers

Every link is one QUIC connection (`QuicCarrier`,
`crates/node/src/link/quic.rs`): one control stream, open and accept for
additional streams, finish and typed reset, and link close.

| Link | Paths it may take | Link admission |
| --- | --- | --- |
| Between two paired hosts | direct, punched, through the relay over UDP or TCP, through a paired daemon, SSH | pinned mutual TLS, ALPN `amux/2` |
| A host's link to the relay | UDP, or framed inside TLS over TCP | WebPKI certificate, then the `Hello` token |

Device QUIC permits only bidirectional application streams, at most 64
concurrently. Every endpoint uses 1,200-byte datagrams with path MTU
discovery off, so a packet fits every path, the forwarding header included,
and no endpoint offers to grease QUIC's fixed bit. The keepalive and
idle timeout are 20 and 60 seconds on iOS, 10 and 30 seconds elsewhere, so a
desktop notices a link that died silently within about forty seconds (the
shorter timeout of a link's two ends applies).

Presence says whether a link to a host is up, so a link that cannot carry
calls must not stay up until that timeout. A call whose stream will not open on
a link that claims to be up, or that finds no channel within its patience,
closes the link at once; the host reads as the links that remain say, and
redialling and discovery bring the link back as they do any lost link. The
call then tries one other route the core lists, if any. A call refused while
the link is fine, by the far host or by a forwarder, closes nothing and says
nothing of the host. A link that comes back and still carries no call reads
online until its next call fails, and the redial backoff paces any repeat. A
client releases what it held for the host when it reads online again.

An SSH link reaches the far daemon through `amux relay`, a hidden command the
SSH session runs, which joins its stdio to the profile's `link.sock`. The QUIC
handshake through it pins both hosts' keys as a direct one does; the far
daemon takes the peer from the key it presented and refuses one it does not
trust.

### The link socket and its paths

A profile has one UDP socket, bound to `[::]` dual-stack (`0.0.0.0` where the
OS has no IPv6) on the port it had last, and every link it dials or answers
over UDP is a QUIC connection on it (`crates/node/src/link/socket.rs`). quinn
never sees the network: it sends to a stand-in address, one per peer in the
unique-local prefix `fd61:6d75:7800::/48`, and the socket puts each packet on
the path carrying that peer's link. Arrivals are mapped back to the peer's
stand-in whichever path brought them, so a path change is invisible to quinn
and to every stream on the link. A send or receive error on the socket is a
lost packet, never a dead link.

The socket tells its own datagrams from QUIC by the fixed bit, which QUIC sets
in every packet's first byte; ours clear it and name their kind there:

| Kind | Datagram | Meaning |
| --- | --- | --- |
| QUIC | fixed bit set | a link's packet, for this host |
| `0x00` Forwarded | kind, the destination's 16-byte host id, a QUIC packet | to a forwarder: send this to that host |
| `0x01` Probe | kind, the sender's host id, an 8-byte nonce | does this path reach you? |
| `0x02` Probe answer | kind, the answerer's host id, the nonce | it does |
| `0x03` Delivered | kind, the sender's host id, a QUIC packet | from a forwarder: this came from that host |

A byte-stream path carries the same datagrams, each behind a two-byte
big-endian length, so a frame holds at most 65,535 bytes. Each path gathers
its frames into 64 KiB writes and reads its stream 64 KiB at a time.

Which path carries a link is the path machine's decision, one per peer of the
profile (`crates/path-core`, driven by `crates/node/src/link/paths.rs`). Its
candidates are the addresses discovery finds, the addresses the peer lists
(below), the last address that worked, the address the relay observed the peer
at, and every forwarding link that reaches the peer. While the link is up the
machine probes each candidate every five seconds; a probe unanswered within
two seconds marks the path failed, and only an answered probe lets a path
carry the link, whose packets arriving on another path only prompt a probe
there. Among working paths, one that skips the forwarders beats a forwarded
one, then the lowest round trip, then IPv6 over IPv4. The link moves off a
working path only to a better kind, from forwarded to direct, and only once
it has carried for the three-second hold-down, so two working paths never
trade it back and forth; a path that stops working is left at once. While the
link is direct the relay's path keeps being probed, so it is ready the moment
the direct one fails. An address that falls silent is kept and probed until
the peer stops listing it, so a link returns to it after an outage of any
length. A move to a path of another kind is told to the daemon core, which
reads the host by its new route kind.

When the host's network changes (the phone's path monitor reports it, a
desktop daemon reads its interface addresses every two seconds) the profile
binds its socket afresh on its port, lists its addresses again and tells
every path machine, which probes its paths again; the link stays on whatever
still answers, the relay's path if nothing else, and moves back when a
better path answers.

**Punching** is for paid accounts. A host on a paid account learns from the
relay the public address its packets arrive from (`observed_addr`,
`ObservedAddress`) and passes it to its peers with its addresses; a peer's
probes to it open the routers on both sides when they allow it, and the
link goes direct across networks. A host on a free account is told no
observed address and never probes one. When the account lapses, every
punched path is dropped at once, an address the peer also lists aside, so
no packet goes on a path across networks after a lapse. amux never asks a
router for a port mapping.

### Launch: tickets, tokens and early calls

A launch costs one round trip to a host this profile has linked to before.

**Tickets.** Every listener issues a TLS 1.3 session ticket at each handshake
from a store kept across restarts (`crates/node/src/link/resumption.rs`): a
profile's in `profiles/<id>/tickets`, the relay's beside its certificate.
Each ticket is single-use, honoured for seven days, and sealed under a key
replaced daily. A client keeps the tickets it is given on disk too, per peer
and pinned key under `profiles/<id>/client-tickets/` (the relay's per relay
name), and deletes each from disk before presenting it, so a ticket is never
offered twice. A ticket from a key the profile no longer pins is never
offered. The listener sends a link's ticket once the client's Finished
reaches it: two round trips into a resumed link, one after its early calls
are answered. A process that ends sooner keeps no ticket, and its next
launch does a full handshake. Client tickets are written and read through a patched rustls
([architecture](ARCHITECTURE.md#the-rustls-patch)).

**Tokens.** A listener also gives each client two QUIC address-validation
tokens, which a client keeps beside its tickets (`client-tokens/`), so a
relaunch's first packets skip the listener's Retry round trip. The profile
keeps its relay token across launches and renews it hourly (see
[the cloud](CLOUD.md)), so a launch does not wait on the account service.

**The first flight.** At launch the profile dials every remembered host at
once, on every candidate path: the addresses it knows, the forwarders a
route names and, for a host on the account, the relay, including a relay
connection that comes up later or is not admitted yet. The socket sends the
link's opening packets on all of them until a packet from the host arrives
on one, which then carries the link; a path that joins late is sent the
first flight's packets again. The dial resumes with the host's ticket, so
the `Hello` and the calls the profile's core makes of a host as it begins
following it go as 0-RTT data, and their answers wait until the core makes
those calls. A dial with no ticket, or one whose resumed handshake has not
finished, gives up after two seconds.

**The early-call rule.** Only reads go in 0-RTT data, because the far end may
see early data more than once. The channel pool writes early only calls of
one type (`EarlyCall` in `crates/node/src/link/channels.rs`):
`SubscribeInventory`, `SubscribeSession`, `ListItems`, `GetBlob` and `GetDiff`. A send, a
stop or a pairing call has no constructor there, so it cannot be early data.
If the far end refuses the early data (it lost the ticket, or the ticket was
spent), the dialler opens the same streams again in the same order, control
first, and writes the same calls as ordinary data once the handshake
completes.

**The resumed identity.** A resumed handshake presents no certificate: the
ticket names the key the host proved when it was issued. The listener holds
the link's early streams until its daemon core has decided that key is still
the one pinned for the host the `Hello` names; a host whose pin was removed,
or is being replaced, is refused with the revocation close (`USER_REVOKED`)
before any early call is served, and the dialler reads it as the revocation.

### Link control

The connector opens the first bidirectional stream as control. Each control
message is protobuf encoded after a big-endian `u32` byte length and is bounded
by `MESSAGE_SIZE_LIMIT` (16 MiB). The complete control vocabulary is:

- `Hello`: supported protocol versions, this host, the current neighbor
  snapshot, the sender's runtime instance id, every interface address the
  sender can be reached at with its listener's port (`addrs`, as `host:port`,
  never loopback or link-local), the address the relay last told it its
  packets come from (`public_addr`, only a paid host has one), and an
  authentication token only for a cloud link. A host lists no addresses on its
  link to the relay.
- `HelloAck`: either the accepted version plus the acceptor's host, neighbor
  snapshot, runtime instance id and addresses, or an error. A relay accepting
  a paid host also fills `observed_addr`, the address it saw that host's
  packets come from.
- `NeighborUp` and `NeighborDown`: adjacency deltas after the handshake.
- `Addresses`: the sender's whole new address list and public address,
  replacing the ones its `Hello` or an earlier `Addresses` carried; sent
  whenever either changes.
- `ObservedAddress`: a relay telling a paid host that the address its packets
  come from has changed. The relay looks every two seconds; a free host and a
  host linked over TCP are never told one.
- `ForwardRefused`: a forwarder could not deliver packets naming `dst`, with
  one `ForwardRefusal` reason: `NO_ROUTE`, `PAYMENT_REQUIRED`,
  `RATE_LIMITED` or `SHUTTING_DOWN`. The packets themselves are dropped.
- `Reauth`: a fire-and-forget replacement token for a cloud link.
- `LinkClose`: an immediate close with a reason and optional error.

A host keeps what each trusted peer listed, tagged by where it came from:
listed over a direct link, over a forwarded one, or the peer's public address,
which only the relay's observation gave. The profile's trust file keeps the
list beside the address the peer last answered at.

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

### Streams

A stream after the control stream carries no preface: it belongs to the
host at the other end of its link, whom the link's handshake authenticated,
and no stream on a link reaches any other host. There is no source,
destination, stream id, payload, data message or close message in the
control vocabulary. Graceful stream finish is close; a reset abandons the
stream. A forwarder that will not carry a pair says so on the sender's link
with `ForwardRefused` (chapter 6), not on a stream.

### Channel authority and classes

The link is the authority for every call on it. Its QUIC handshake is
mutual TLS 1.3: the acceptor reads the live trust store and the dialler pins
the key paired for the host it dialled. That is the same handshake on every
route, the same network, SSH or through a forwarder, so a channel's stream
carries no handshake of its own and its first bytes leave at once. A host
presenting no certificate while a pairing window is open is admitted for
pairing only, on any route; through the relay that is the same exchange as
on the same network.

A host that revoked another closes any link it held with `USER_REVOKED`,
and refuses the revoked host's certificate in every later handshake with
TLS's `certificate_revoked` alert. Only a host that pins this one presents
its certificate, so the revoked host reads either as the revocation, on any
route: one whose links were down when it was revoked learns it at its next
dial. It records the revocation, closes its other links to the host, lists
the host as revoked and offline, and fails the call as `UNAUTHENTICATED`
("the host no longer trusts this machine").

The channel pool (`crates/node/src/link/channels.rs`) selects a live link by
peer and route, dialling one through the forwarder when the route is a
forwarder's and none is up, opens a stream, and gives it to tonic as one
HTTP/2 channel. It has two channel classes:

- `Calls`: one cached channel per peer and route. The inventory subscription,
  forwarded calls, `ListItems`, `GetBlob`, `GetDiff` and every other one-shot call
  ride it. A cached channel whose connection ended is dropped, and the next
  call opens a fresh stream.
- `Session`: one cached channel per peer and route for the replica agents'
  `SubscribeSession` streams, so they neither wait behind nor hold up the host's
  calls, and a host with many agents costs one stream, not one per agent.

Separate streams give separate flow control, so a long subscription does not
queue behind calls at the amux layer. Channel payloads are bounded by
`CHANNEL_MESSAGE_SIZE_LIMIT` (65 MiB): the largest attachment,
`ATTACHMENT_SIZE_LIMIT` (64 MiB), with room for the `PutBlobRequest` framing
around it.

### Compression

Every channel to a paired host is compressed, in all three classes, on every
route: one streaming zstd compressor each way, between HTTP/2 and the stream,
inside the link's encryption, so a forwarder carries it without knowing it is
there (`crates/node/src/transport/compress.rs`). The opener wraps its end in
`ChannelPool::channel_over`; the acceptor wraps its
end as `TunnelDispatcher` hands a paired host's stream to the peer services.
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

Packets are forwarded per pair of hosts. A host reaching a peer through a
forwarder sends its link's QUIC packets to the forwarder behind a 17-byte
header (the kind byte `0x00`, then the destination's host id); the forwarder
rewrites the header to the kind `0x03` and the sender's host id and sends the
packet on its own link to the destination, which sees it arrive from the
sender (`crates/node/src/link/socket.rs`). The two kinds keep the directions
apart: a daemon both forwards for its paired hosts and sends through them, so
which link a packet arrived on cannot say whether it is to be forwarded or
was delivered. The first packet for a pair the
forwarder has not judged asks its core (`crates/relay-core`), which installs
the pair when both links are up on one account and both paid, or both
pinned; the driver forwards from its table without asking again until the
core removes the pair (`crates/node/src/link/forwarder.rs`). A link going
down, a lapse to the free plan or a token's expiry removes every pair
through it, and both ends are told. Packets from a host whose own link to
the relay is not admitted yet, as a launch's first flight is, are held for up
to a second (at most 32 packets or 64 KB per sender, 64 MB in all) and
released the moment it is; nothing is forwarded for a sender before then. A
link may send at most 56,000 packets and 64 MiB a second through a
forwarder; past that its packets are dropped, as a congested router would.
The forwarder reads no packet beyond its header.

A forwarder that will not carry a pair answers on the sender's control link
with `ForwardRefused`, naming the destination and why, at most once per
destination every five seconds: `NO_ROUTE` when it holds no link to the
destination, `PAYMENT_REQUIRED` when the account's plan carries no
connections between hosts, `RATE_LIMITED`, `SHUTTING_DOWN`. The host closes
its links to that destination through the forwarder before its core hears
why, so presence reads the host away on a payment refusal while no other
route carries it, and offline at once on no route.

A daemon forwards the same way for its own paired hosts: it is a forwarder
with one account and no plan, whose pairs are between pinned links. A phone
reaching desktop B through desktop A sends to A with a header naming B, and
A forwards over its own link with B. A host's link to the relay over TCP is a
path of the same socket as its UDP links, so the relay forwards to and from
it as to any other.

A forwarded arrival needs neither a Retry nor a place under the per-source
handshake limit at the host answering it: the forwarder names the sender and
carries only hosts it admitted. Arrivals with an address of their own get
both, and a returning host's address-validation token skips the Retry but
still counts against the limit. A host keeps the tokens hosts and the relay
give it in its profile, so a relaunch presents them too. While a host has a link of its own on a path
of its own to a peer, that link carries the peer's calls ahead of one a
forwarder carries.

The cloud relay is the same code run as `amux server start --cloud`
(`crates/amux/src/relay.rs` and `crates/node/src/edge/relay_server.rs`). It is
not an installation: no profiles, no agents, no supervisor. It accepts every
host of an account over QUIC and a TLS-over-TCP fallback on one socket, checks
each host's connection token against the account service's signing keys,
advertises each host's adjacency to the others, and forwards packets between
them, across UDP and TCP alike. The account
service that mints those tokens is a separate deployment; see
[the cloud](CLOUD.md).

Every link records how it was admitted. A tier belongs to a token-admitted
link on the cloud relay only: `CloudToken { free | pro }`. Direct device and
SSH links are `PinnedKey`. A daemon forwarding between paired peers therefore
never consults an account or tier.

The cloud relay forwards nothing between two hosts when either link was
token-admitted as free. Neighbor control continues, so a free account
receives presence but no call or pairing connection through the relay.
Reauthentication may change a link's tier, and a pair refused while free is
installed as soon as both are paid.

The cloud listener serves QUIC on UDP beside the TCP fallback using the same
WebPKI certificate and key. Devices try QUIC immediately and begin TCP after a
300 ms fallback delay; the first completed link wins, with QUIC winning a tie.
A TCP-only win records that relay host as UDP-blocked for one hour, suppressing
QUIC attempts until the memory expires. The memory is process-local.

The fallback's limits are honest. Over TCP a link's QUIC packets are framed,
each behind its length, on one ordered byte stream: a lost segment holds up
every stream of every link it carries, and losing the connection loses the
path. TCP is only a cloud-relay fallback. A direct LAN whose UDP is blocked
is offline unless another route, such as the relay or SSH, is available.

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
| The installation front door | `InstallationService` | the CLI and the terminal client, to find profiles and manage them |
| A profile's client socket, `<data_dir>/profiles/<id>/sock` | `AgentService`, `SessionService`, `ProfileAdminService` | the terminal client, the CLI and third-party clients, at the path the front door reports as `Profile.socket_path` |
| An agent's `agents/<id>/tools.sock` | `AgentService` and `SessionService`, with that agent as the caller | the agent's MCP tool server, and nothing else |
| A profile's `link.sock` | a link, not a service | `amux relay`, carrying an SSH link |
| A link stream whose handshake presented a pinned key | `PeerAgentService`, `PeerSessionService` | paired daemons, including a phone's embedded daemon |
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
It serves no sockets: the app calls the profile `Registry`, each profile's
`Edge` for its pairing and peers, and `ClientApi` in process, and it is an
ordinary peer to the desktops it pairs with.

A client that finds nothing on the front door does not start a daemon itself.
Under a supervisor (`supervisor: on`) it starts `amux supervise` if none is
running, which starts the daemon; otherwise it says the daemon belongs to the
service manager and waits for it.

### The front door

`InstallationService` lists, watches, creates, binds, renames and deletes
profiles. A bind names its profile or lets the installation pick the one
already bound to that account, else one that holds nothing, else a new one.
A call about one profile names it by id, and profile and host ids travel as
their 16 bytes; the CLI reads and prints their text form. `SubscribeProfiles`
sends every profile, `CaughtUp`, then each change in the order it happened;
a subscriber that falls behind is ended with `ABORTED` and subscribes again.
It also answers `GetInstallation` (the daemon's version) and
`Shutdown`. The rules behind these calls are `node::Registry`'s; the front
door (`crates/node/src/front_door.rs`) converts.

`ProfileAdminService`, on each profile's own socket, is that profile's
administration, so none of its calls names a profile: it signs the profile
out, pauses and resumes its relay link (`SignOut`, `PauseProfile`,
`ResumeProfile`); opens, reports and cancels pairing
windows (`StartPairing`, `GetPairingStatus`, `CancelPairing`); runs the
initiator's side of pairing (`BeginPair`, `ConfirmPair`, `AbandonPair`);
reports this device's identity; records a peer an SSH exchange
authenticated (`TrustSshPeer`); lists, describes and unpairs peers; and
writes the profile's debug report (`Dump`, [chapter 8](#chapter-8-diagnostics)). The
implementation is `crates/node/src/profile_admin.rs`.

None of this is reachable from a link or from an agent's tools socket. No
client retries one of these calls. A call whose reply is lost, which on a
local socket means the daemon went away, fails, and the person runs the
command again.

### AgentService and SessionService

`AgentService` and `SessionService` are the client-facing surface of one profile. `AgentService` is the fleet and its agents:

| Call | What it does |
| --- | --- |
| `SubscribeInventory` | The host set and every agent row: current state, `CaughtUp`, then deltas. A listing is this stream read to `CaughtUp`. |
| `ResolveAgent` | A fleet-wide reference to one row: an id with or without dashes, the eight-character short id (or any longer prefix), a name, or `name@host`. `NOT_FOUND`, or `AmbiguousAgentName` with the candidates when a bare name matches agents on several hosts; the message offers the `name@host` forms that pick each. Every other agent call takes an id. |
| `CreateAgent` | Creates an agent, on this host or on a trusted host named by id or by name. |
| `RenameAgent`, `StopAgent`, `ResumeAgent`, `DeleteAgent` | Registry operations. A stop ends the agent now (its turn cancelled the provider's own way); a delete stops it if live and removes it. Both act on the named agent alone; its children follow on their own hosts by [the family rule](#the-family-rule), so the answer says nothing of them. |
| `SendMessage` | A message to an agent, whose sender the daemon stamps from the caller; see [agent tools](AGENT_TOOLS.md). |
| `PutBlob`, `GetBlob`, `GetDiff` | Bytes in an agent's directory; see [attachments](ATTACHMENTS.md). |
| `GetCatalogue` | What a provider on a host offers with no agent running, from the host's own copy. What an agent offers is a blob its snapshot names, read with `GetBlob`; see [the wire](WIRE.md). |
| `ListRepositories` | Where a host offers to start an agent: recent working directories and repositories under its configured roots. |

`SessionService` is one agent's conversation:

| Call | What it does |
| --- | --- |
| `SubscribeSession`, `ListItems`, `GetItem` | An agent's session stream, older pages, and one item; see [the wire](WIRE.md). |
| `SendInput` | A person's input to an agent; the answer is the interpreter's verdict. |
| `UpdateDraft`, `GetDraft` | An agent's unsent draft, kept by this machine's daemon for its own clients so it outlives a client closing; never synced, and refused to a paired host. |

The implementation is `ClientApi` in `crates/node/src/grpc.rs`, a thin mapping
onto the profile runtime. It holds the runtime weakly, so a connection that
outlives its runtime is answered `UNAVAILABLE` and the client redials.

### Calls on another host's agents

Only the host that owns an agent can act on it. When a call names an agent
whose row here is a replica, `ClientApi` makes the same call on the owner's
daemon through the peer services, exactly as a phone would, and answers with what
the owner answered (`crates/node/src/forward.rs`). A forwarded call fails in
one of two ways, and they are kept apart because only the second may have
reached the owner:

- **Not sent.** The call never left: no link to the owner, the owner is not
  trusted, or no channel opened within 10 seconds. That is
  reason `HOST_UNREACHABLE` naming the host by the name it was paired under,
  except for `SendInput`, which answers with a rejected verdict,
  `REFUSAL_HOST_UNREACHABLE`, so a composer treats it like any other refusal.
  When only the relay reaches the owner and the account is on the free plan
  (its subscription lapsed), the relay carries no calls between hosts, and
  the call is refused at once as that: reason `PAYMENT_REQUIRED` with a
  sentence naming the lapse, for `SendInput` the same rejected verdict
  with that sentence as its detail, and for `StopAgent`
  reason `HOST_UNREACHABLE` with that sentence, so a client holds the stop
  as it holds any stop that never left.
- **Uncertain.** The call went out and its answer did not come back: the
  link failed under it, or 30 seconds passed (enough for a spawn, which waits
  for the child's process to start on the far side). That is
  `ABORTED` for every call, `SendInput` included: the sender holds
  the input as uncertain and settles it at its next catch-up, and nothing
  sends it again.

Forwarding goes one hop. The peer services answer only for this host's own
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

### PeerAgentService and PeerSessionService

`PeerAgentService` and `PeerSessionService` (`crates/node/src/edge/peer.rs`)
are the daemon-to-daemon services; replication is a daemon being a client of its peer. The caller is the
host whose pinned key the stream's handshake presented, and a call without one
is refused as `UNAUTHENTICATED`. Only amux daemons call them, and each stamps
what it says on behalf of its own callers, a message's sender and a create's
parent, from those callers; the answering daemon takes them as given. They
differ from the client services in these ways:

- It answers only for this host's own agents. Its inventory stream carries
  this host's entry and this host's agents, never what this host knows of
  other hosts. Every call that names an agent (`SubscribeSession`, `ListItems`, `GetItem`,
  `SendInput`, `RenameAgent`, `StopAgent`, `ResumeAgent`, `DeleteAgent`,
  `PutBlob`, `GetBlob` and `GetDiff`) is `NOT_FOUND` for an agent this host only
  replicates, even when this host holds that agent's rows or blobs.
- `SubscribeSession` takes a `PeerSubscribeSessionRequest`, which may ask for what came
  `after` a revision: that is how a replica catches up from its cursor
  ([the wire](WIRE.md) describes it).
- `SendMessage` takes the `Envelope` the calling host's daemon stamped, and
  takes its sender as given.
- `ListRepositories` answers for this host only.
- `PeerAgentService.Dump` answers with this host's part of the caller's
  report, a `DumpPart`, since the caller is on another machine.
- There is no `ResolveAgent`, and no drafts: a draft stays on the machine it
  was typed on.

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
while the hosts were apart is acted on then. A parent the listing shows
exited stops every own child of it, however lately the child was made, since
an exited row is definitive. A parent missing from the listing deletes only
the agents that existed before that listing was asked for; a child made since
may belong to a parent made so lately the listing predates it, and the live
stream speaks for that parent (`inventory_caught_up` in
`crates/daemon-core/src/sources.rs`). A child made under a parent on another
host before this host caught up with it may so outlive a parent deleted
meanwhile: orphans are acceptable. Such a child finishes its work and exits,
and a later clean-up of leftover agents collects what remains.

A create names its parent (the caller, or for a create forwarded from another
host, the parent that host stamped). If this host already sees that parent as
exited, or as deleted (its own agent with no row), the create is refused as
`FAILED_PRECONDITION`. The parent check and the child's row are written under
one hold of the store, so a parent that ends after the check finds the child
and stops it. A parent on another host is checked only against a caught-up
row: while this host has not caught up with that host's listing since they
last connected, or holds no row for the parent yet, the create is made, and
the family rule above settles the child once the parent's row speaks.

A stop's or delete's answer covers the named agent only; it never confirms the
children, which may be on hosts that are away.

### The tools socket

Each agent's `tools.sock` serves the same `AgentService` and
`SessionService`, and not `ProfileAdminService`, with one difference: the socket is the caller's identity. The daemon binds it when it
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

A refusal is a `google.rpc.Status`, gRPC's richer error model (Rust:
`wire::Error`): a gRPC code, a message, and details packed as `Any`, among
them typed details (`AmbiguousAgentName`, `AmbiguousHostName`,
`ProtocolVersionMismatch`). It travels whole in the gRPC status details, its
code on the status itself (`crates/node/src/grpc.rs`, `status`), so generic
gRPC tooling reads it. What the code alone does not say, a
`google.rpc.ErrorInfo` detail names: domain `amux.sh`, reason an
`ErrorReason` value's name without its prefix. `UNAVAILABLE` means this
daemon is not answering, which a caller retries; another host being
unreachable is `FAILED_PRECONDITION` with reason `HOST_UNREACHABLE`, because
it is an answer, not an outage. A lapsed plan is `PERMISSION_DENIED` with
reason `PAYMENT_REQUIRED`. The googleapis files are vendored under
`crates/wire/proto/google/`.

The protobuf files change only as
[their README](../crates/wire/proto/amux/v1/README.md) and
[the wire](WIRE.md#compatibility) allow: added to, never renumbered, with a
removed field's number reserved; `just proto-compat` checks it against the
latest release. `PROTOCOL_VERSION` exists only in the link handshake. The
local sockets carry no version at all.

## Chapter 8: Diagnostics

`ProfileAdminService.Dump` on the profile socket writes a debug report on this host (rows, store
slices, journal tails and each agent process's own redacted part) and answers
with its path. For an agent another host runs, the report holds only this
host's row and store slice, and asks that host for its side through
`PeerAgentService.Dump`; the answer is that host's own bundle as one `DumpPart`,
and a host that cannot be reached is named in the
report's manifest. Diagnostics use the ordinary calls; they create no extra
link messages or authority path. [Debugging](DEBUGGING.md) covers reading a
report.

## Chapter 9: Blob routing

`GetBlob`, `PutBlob` and `GetDiff` on another host's agent, and `GetCatalogue` for
another host's provider, are forwarded calls like any other, on the `Calls`
channel. The bytes are content-addressed, so a relay never needs to understand
them and the reader verifies them against the hash it asked for. An agent's
catalogue is one of its blobs, so it is never decoded on the way and keeps its
hash through a daemon of any version. Where blobs live and how long they last
is described in [attachments](ATTACHMENTS.md).

## What the wire deliberately does not have

There are no transitive neighbor advertisements, route lists, source routes,
hop caps, split horizon, stream prefaces, stream data messages, housekeeping
acknowledgements, a second pairing protocol, transitive trust, transitive
presence, multi-hop forwarding, a handshake inside a stream, router port
mapping, or multipath: one path carries a link at a time. A path change keeps
the link and its streams; link loss closes its streams, and callers reconnect
over the best current route.

That small vocabulary is the point: adjacency chooses where packets may go,
the link carries them, and its pinned handshake decides who may call.
