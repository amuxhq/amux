# amux documentation

*For anyone arriving at amux: this is the map, one line per page.*

## Readers first

- [How amux works](HOW_IT_WORKS.md) — for people using amux: devices, accounts, pairing, hosts, the relay,
  revocation, and agents that keep running while the daemon restarts.
- [What amux sends](ANALYTICS.md) — the usage events a machine sends amux.sh, every property, what is never
  sent, and how to turn it off.

## The system

- [Architecture](ARCHITECTURE.md) — the problem, the vocabulary, the mental model, the processes, the agent
  directory, startup, remote hosts and the cloud, and the crate map with its dependency policy.
- [The agent process](AGENT_PROCESS.md) — lifecycle, control socket, provider children, facts and hooks, the pty
  socket and raw attach, the tool server, exit and self-destruct.
- [Provider crates](PROVIDER_CRATES.md) — each provider's protocol, host and client layers, who uses each, and
  the checks that hold them to the recordings.
- [Interpreters](INTERPRETERS.md) — how terminal Claude, headless Claude and Codex output becomes chat items:
  the step rule, checkpoints, redaction, provider crates, recordings and the coverage test.
- [Journal and store](JOURNAL_AND_STORE.md) — journal segments, commit and revisions, durability, ingest, the
  store schema, own and replica rows, retention and disk-full.
- [The wire](WIRE.md) — the amux.v1 service: subscriptions, items and appends, inputs and the queue, blobs,
  inventory, and how the schema only grows.
- [Network protocol](PROTOCOL.md) — carriers, links, streams, channels, routing, pairing, discovery, the relay
  and the front door.
- [Attachments](ATTACHMENTS.md) — blobs and attachments: shape, bytes and metadata, lifetimes, deletion and the
  review document.
- [Agent tools](AGENT_TOOLS.md) — the seven tools agents call, messaging, families, delivery, completion and
  work across hosts.
- [Supervisor](SUPERVISOR.md) — `amux supervise`, login items, stopping, updates and channels, overlap and
  rollback, keep-awake.

## Clients

- [Client library](CLIENT.md) — the shared client crates: session state and driver, views as values, and the
  flows every client uses.
- [Chat vocabulary](CHAT_VOCABULARY.md) — the client contract: the places a chat has, its rows and asks, and
  what interpreters and views must cover.
- [Terminal client](TERMINAL.md) — screens, keys, the leader, raw attach and the CLI verbs.
- [iPhone app](IOS.md) — packages, the chat and fleet, driving the app, snapshots and journeys.
- [Embedding](EMBEDDED.md) — app-runtime, app-embedded, app-ffi, the generated Swift types and the seams.
- [iPhone copy](IOS_COPY.md) — the rules the app's words follow and the lint that holds them.
- [Cloud](CLOUD.md) — what the app and daemon ask of amux.sh.

## Verifying

- [Testing](TESTING.md) — boundaries, lanes, the suite catalogue and contracts, how to run, where fixtures and
  evidence live.
- [TestNet](TESTNET.md) — the multi-host harness, `testnet serve` and `testnet ctl`.
- [QA](QA.md) — adversarial testing by agents: running `just qa`, what a run leaves behind, and turning findings
  into tests.
- [Performance](PERFORMANCE.md) — the perf lane, the flood, budgets and baselines.
- [Debugging](DEBUGGING.md) — dumps, the facts ring and checkpoints, `amux dump`, the reports directory,
  redaction and replay.
- [Parameters](PARAMETERS.md) — every tunable value with its starting point, its basis and where it is set.

## Operations

- [Build](BUILD.md) — toolchain, recipes, committed generated code and warm worktrees.
- [CI](CI.md) — workflows, lanes, `just ci` and the Windows cross-check.
- [Release](RELEASE.md) — shipping the iPhone app and the daemon's release feed and channels.
- [Licensing](LICENSING.md) — how the repository is licensed.
- [Logo](BRAND.md) — the editable vector master, `just logo` exports and the visual review.
