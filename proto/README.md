# The amux protocol

The protobuf source of amux's public contract, for amux itself and for
anyone building a client or a host against it. Every import is relative to
this directory, so it is the one include path a code generator needs.

| Directory | Holds |
| --- | --- |
| [`amux/v1/`](amux/v1/) | The protocol: its services, messages and compatibility rules |
| [`google/rpc/`](google/rpc/) | `google.rpc.Status`, `Code` and `ErrorInfo`, vendored from googleapis (see [`google/README.md`](google/README.md)) |

The well-known types (`google/protobuf/*.proto`) come with protoc itself.
amux's Rust is generated from here into `crates/wire/src/generated/` with
`just protobuf`.
