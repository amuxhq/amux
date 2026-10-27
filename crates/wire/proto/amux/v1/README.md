# amux.v1

The canonical protobuf source of the amux protocol. Generated Rust is
committed under `crates/wire/src/generated/`; regenerate it with
`just protobuf` after editing any file here (CI fails on stale output).

The runtime uses gRPC services for application calls. Host-to-host links
exchange length-prefixed `Message` values on their control stream and carry
application channels on separate native streams.

| File | Holds |
| --- | --- |
| `agent_service.proto` | `AgentService`: the fleet, agents' lifecycle and messages, blobs, diffs, catalogues, repositories |
| `session_service.proto` | `SessionService`: one agent's conversation, its input, the composer's draft |
| `peer_service.proto` | `PeerAgentService` and `PeerSessionService`, daemon to daemon |
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
on disk. So every change keeps what a released build wrote decoding as it
meant:

- Fields, oneof arms, enum values, messages, methods and services are only
  added.
- A number is never renumbered, retyped or reused; one that goes is left
  `reserved`. A field never moves into, out of or between oneofs.
- Names are not on the binary wire and may change, but for two: a service's
  and a method's, which are the gRPC path, and an error detail message's,
  which is its `Any` type URL. A field or method may take another message
  that reads the same numbers.

`just proto-compat` checks all of this against the latest release.
`PROTOCOL_VERSION` (`Hello.supported_protocol_versions`) moves only for a
break both ends of a link must take.

What a client writes into a dump is `amux.dump` (`../dump/session.proto`),
read only by the build that wrote it and outside these promises.
