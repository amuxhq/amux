# Vendored googleapis protos

`rpc/status.proto` and `rpc/code.proto` are copied unchanged from
[googleapis](https://github.com/googleapis/googleapis) at commit
`7721fc7742901980ae9b622bf546b72d5c81f65f`; `rpc/error_details.proto` is the
same file cut down to `ErrorInfo`, the one detail amux sends. amux's errors are `google.rpc.Status`
values, as gRPC's richer error model carries them, with `google.rpc.ErrorInfo`
naming amux's own reasons.

They keep their own licence, the Apache License 2.0 stated in each file's
header; so does the Rust generated from them.
