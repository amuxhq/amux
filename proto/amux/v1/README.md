# amux.v1

The canonical protobuf source of the amux protocol. Generated Rust is
committed under `crates/wire/src/generated/`; regenerate it with
`just protobuf` after editing any file here (CI fails on stale output).

The runtime uses gRPC services for application calls. Host-to-host links
exchange length-prefixed `Message` values on their control stream and carry
application channels on separate native streams.

| File | Holds |
| --- | --- |
| `agent_service.proto` | `AgentService`: the fleet, agents' lifecycle and messages, blobs, diffs, catalogues, repositories; served to clients and to paired daemons |
| `session_service.proto` | `SessionService`: one agent's conversation, its input, the composer's draft |
| `installation_service.proto` | `InstallationService`: the installation and its profiles |
| `profile_admin_service.proto` | `ProfileAdminService`: one profile's account, pairing, peers and dump |
| `pairing_service.proto` | `PairingService`: two hosts coming to trust each other |
| `inventory.proto` | Hosts and agent rows |
| `records.proto` | The kind-neutral session records |
| `chat.proto` | What the kinds share inside a body |
| `claude.proto`, `codex.proto` | The per-kind bodies |
| `agent_process.proto` | The agent process's spec and its `ctl.sock` and `pty.sock` frames |
| `link.proto` | Host-to-host link control |
| `dump.proto` | The diagnostic dump |
| `error.proto` | `ErrorReason`, the amux reasons a `google.rpc.ErrorInfo` names |

## Compatibility

`amux.v1` is the released protocol. From amux 0.9.0 on, builds of different
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

`just proto-compat`, which CI runs, lints these files with buf and checks them
against the latest release.

What a client writes into a dump is `amux.dump` (`crates/wire/proto/amux/dump/session.proto`),
read only by the build that wrote it and outside these promises.
