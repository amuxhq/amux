# amux.v1

The canonical protobuf source of the amux protocol. Generated Rust is
committed under `crates/wire/src/generated/`; regenerate it with
`just protobuf` after editing any file here (CI fails on stale output).

The runtime uses gRPC services for application calls. Host-to-host links
exchange length-prefixed `Message` values on their control stream and carry
application channels on separate native streams.

| File | Holds |
| --- | --- |
| `inventory_service.proto` | `InventoryService`, a local service: the hosts and agent rows the node holds, and names resolved against them |
| `session_service.proto` | `SessionService`, a local service: the conversations the node holds and the files they reference |
| `draft_service.proto` | `DraftService`, a local service: what this machine's person has typed and not sent |
| `agent_service.proto` | `AgentService`, a host service: one host's own agents, their lifecycle, input, messages and files |
| `host_service.proto` | `HostService`, a host service: what a host offers before any agent exists, and pinning its folders |
| `review_service.proto` | `ReviewService`, a host service: what changed in one of a host's agents' working trees |
| `peer_service.proto` | `PeerService`, served on links only: what a paired daemon asks for itself to keep its copy of this host, and a child's report |
| `caller.proto` | `Caller`: who a call carried to another host is for, in the `amux-caller-bin` header |
| `session.proto` | What a conversation is streamed and driven with: session events, input and its verdict, the composer's draft |
| `installation_service.proto` | `InstallationService`: the installation and its profiles |
| `profile_admin_service.proto` | `ProfileAdminService`: one profile's account, pairing, peers and dump |
| `pairing_service.proto` | `PairingService`: two hosts coming to trust each other |
| `inventory.proto` | Hosts, the agent programs each runs, and agent rows |
| `records.proto` | The kind-neutral session records, and what an agent can do (`AgentCapabilities`) |
| `chat.proto` | What the kinds share inside a body |
| `claude.proto`, `codex.proto`, `acp.proto` | The per-kind bodies: terminal and headless Claude, Codex, and every agent that speaks the Agent Client Protocol |
| `agent_process.proto` | The agent process's spec, its `ctl.sock` and `pty.sock` frames, and what its tool server asks it on `private/agent.sock` |
| `link.proto` | Host-to-host link control |
| `dump.proto` | The diagnostic dump |
| `error.proto` | `ErrorReason`, the amux reasons a `google.rpc.ErrorInfo` names |

A local service is answered by the node a client is attached to, from what it
holds, and is never served on a link. A host service is answered by the host
a call is addressed to, for what that host owns; a call's address is its
authority, the host's id, and the node passes a call addressed to another
host on to it unread. [The wire](../../../docs/WIRE.md#where-clients-call)
says who calls what, and where.

A `Kind` is an interpreter; which agent program it runs is a provider id, a
plain string: `claude` (both Claude kinds), `codex`, or an ACP agent's
(`antigravity`, `opencode`, `grok`, or one a person added in a host's
config). Ids are strings rather than an enum because a person can add an
agent no build knows. `ProviderOnHost`, `GetCatalogueRequest`, `Agent` and
`AgentSpec` name a provider by its id. What a client offers for an agent it
reads from `AgentCapabilities` and the host's `ProviderDescriptor`, never
from the kind or the id, so an agent no client knows by name gets the same
chat as the ones it does.

## Compatibility

`amux.v1` is the released protocol. From amux 0.11.0 on, builds of different
versions meet: paired hosts, third-party clients, agent processes that
outlive a daemon upgrade, and the stores and journals an earlier build left
on disk. So every change only adds, and keeps what a released build wrote
decoding as it meant:

- Fields, oneof arms, enum values, messages, methods and services are only
  added.
- A number is never renumbered, retyped or reused; one that goes is left
  `reserved`. A field never moves into, out of or between oneofs.
- Names are frozen too. Three kinds are on the wire: a service's and a
  method's, which are the gRPC path, an error detail message's, which is its
  `Any` type URL, and an `ErrorReason` value's, which is `ErrorInfo.reason`.
  The rest are what every client's generated code calls these things.
- A new field's zero value means what builds without it did.
- Existing data never moves to a new arm. An agent row in an arm an older host
  cannot read is a row its next listing no longer names, so it drops the
  replica and stops the children.
- A new kind of message adds a field to `MessageBody`, keeping `text`, rather
  than an arm to `Envelope.body`. An older host then still delivers the words.
- New meaning goes in the per-kind body where it fits: bodies pass through a
  daemon byte for byte, and a new envelope field is lost at any older daemon.

A host-to-host control message (`link.proto`'s `Message.body`) may be added
only if a peer that predates it can ignore it and lose nothing it needs: a
receiver drops a control message it does not know once the handshake is
done, so the link carries on.

A change that cannot be made this way, or a sweeping clean-up, is the next
protocol: a new ALPN, `amux/2`, served beside `amux/1`, which TLS settles
before any amux message. There is no version number to move.

Two things a call carries outside any message are promised the same way,
since a client in any language builds them:

- **The address.** A host service is addressed to a host by the call's
  authority: the host's id as a UUID in its lowercase hyphenated form
  (`0192f0c8-7a51-7c3e-9d2f-3b1c5e6a7d80`), with no braces, suffix or port.
  Only that form addresses a host; any other authority, `localhost` or a
  name included, is answered by the node the call reached
  (`wire::authority`).
- **The caller header.** A node that carries a call to another host writes
  who it is for in `amux-caller-bin`, a `Caller` (`caller.proto`) encoded as
  gRPC encodes a binary header. A client never sends it: the node it is
  attached to replaces it.

The services took their present shape in 0.11.0. Builds before it served an
earlier `AgentService` and `SessionService` at the same paths, and do not
work with later builds: hosts, clients and apps on either side of that
release are updated together.

`just proto-compat`, which CI runs, lints these files with buf and checks them
against the newest release from 0.11.0 on; until that release is tagged there
is nothing to check against.

What a client writes into a dump is `amux.dump` (`crates/wire/proto/amux/dump/session.proto`),
read only by the build that wrote it and outside these promises.

The shapes of experiments, features shipped while their shape still moves,
are `amux.experimental` (`crates/wire/proto/amux/experimental/`), outside
these promises too. They travel as bytes in the `experimental` maps that
amux.v1 messages carry at field 1000, each entry keyed by the experiment's
version (`workspaces/1`); the maps are promised, their entries are not. An
experiment's service is a host service in `amux.experimental` with the
experiment's version in its name, served only by a host that runs the
experiment. See [the wire](../../../docs/WIRE.md#experiments).
