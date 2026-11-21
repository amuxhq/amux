# amux documentation

*For anyone arriving at amux: this is the map, one line per page.*

## The user guide

The pages under [`guide/`](guide/) are for people using amux, and amux.sh
publishes them as its documentation at <https://amux.sh/docs>. A change
people will notice updates the page that describes it.

- **Get started:** [Install](guide/get-started/install.md),
  [Your first agent](guide/get-started/first-agent.md),
  [Connect your phone](guide/get-started/connect-your-phone.md),
  [Add another computer](guide/get-started/add-another-computer.md).
- **Understand:** [How amux works](guide/understand/how-amux-works.md),
  [Security and privacy](guide/understand/security-and-privacy.md),
  [What amux sends](guide/understand/what-amux-sends.md).
- **Use:** [Terminal app](guide/use/terminal.md),
  [iPhone app](guide/use/iphone.md),
  [Reviewing changes](guide/use/reviews.md),
  [Agents working together](guide/use/agents-working-together.md),
  [Other agents](guide/use/other-agents.md),
  [Running amux on a server](guide/use/server.md),
  [Updates and channels](guide/use/updates.md),
  [Troubleshooting](guide/use/troubleshooting.md).
- **Build on amux:** [Build an amux client](guide/build/build-a-client.md),
  for anyone writing their own client: finding and installing amux,
  profiles, sign-in and the fleet, on amux's Rust crates or from the protocol.

Every guide page starts with a `title`, a `description` and a
`sidebar.order`, which the site uses for the page's heading, its search
summary and its place in the sidebar. Links between pages are relative
paths to the `.md` files, so they work both here and on the site.

## The system

- [Architecture](ARCHITECTURE.md) — the problem, the vocabulary, the mental model, the processes, the agent
  directory, startup, remote hosts and the cloud, and the crate map with its dependency policy.
- [The agent process](AGENT_PROCESS.md) — lifecycle, control socket, provider children, facts and hooks, the pty
  socket and raw attach, the tool server, exit and self-destruct.
- [Provider crates](PROVIDER_CRATES.md) — each provider's protocol, host and client layers, who uses each, and
  the checks that hold them to the recordings.
- [Interpreters](INTERPRETERS.md) — how terminal Claude, headless Claude, Codex and ACP agents' output becomes
  chat items: the step rule, checkpoints, redaction, provider crates, recordings and the coverage test.
- [ACP agents](ACP.md) — agents that speak the Agent Client Protocol: the layers, quirks by agent, resumability,
  sign-in, the host's catalogue, capabilities and descriptors, and how to add a built-in.
- [Journal and store](JOURNAL_AND_STORE.md) — journal segments, commit and revisions, durability, ingest, the
  store schema, own and replica rows, retention and disk-full.
- [The wire](WIRE.md) — the amux.v1 services and who answers each, how a call is addressed to a host and who it
  is for, subscriptions, items and appends, inputs and the queue, blobs and changes, inventory, and how the schema
  only grows.
- [Network protocol](PROTOCOL.md) — carriers, links, streams, channels, routing, pairing, discovery, the relay,
  the front door, and calls carried to another host.
- [Attachments](ATTACHMENTS.md) — blobs and attachments: shape, bytes and metadata, lifetimes, deletion and sent
  reviews.
- [Agent tools](AGENT_TOOLS.md) — the seven tools agents call, messaging, families, delivery, completion and
  work across hosts.
- [Workspaces](WORKSPACES.md) — where agents run: folders, git worktrees and wt trees, who made each, the
  row-first create, holding, starting and stopping, deletion and consent.
- [Reviews](REVIEWS.md) — reviewing an agent's changes round by round: the experimental service, snapshots kept
  out of the repository, pinned git, comparisons, comments and where they sit, rounds closing as the agent takes
  them, what the agent reads, and the page every client draws.
- [Supervisor](SUPERVISOR.md) — `amux supervise`, login items, stopping, updates and channels, overlap and
  rollback, keep-awake.

## Clients

- [Client library](CLIENT.md) — the shared client crates: session state and driver, views as values, and the
  flows every client uses.
- [Chat vocabulary](CHAT_VOCABULARY.md) — the client contract: the places a chat has, its rows and asks, and
  what interpreters and views must cover.
- [Syntax highlighting](HIGHLIGHTING.md) — one tree-sitter engine for every client and the host: languages and
  how one is picked, grammars built in the background, streaming blocks, definitions, sizes and what was left out.
- [Terminal client](TERMINAL.md) — screens, keys, the leader, raw attach and the CLI verbs.
- [iPhone app](IOS.md) — packages, the chat and fleet, driving the app, snapshots and journeys.
- [Mac app](MAC.md) — a client of the daemon: installing and attaching, sign-in, building and running.
- [Browser extension](BROWSER.md) — commenting on web pages for an agent: the overlay and its hosts, the native
  messaging bridge and its registration, the page review attachment, and WebAssembly as a convenience.
- [Embedding](EMBEDDED.md) — app-runtime, app-embedded, app-ffi, the generated Swift types and the seams.
- [iPhone copy](IOS_COPY.md) — the rules the app's words follow and the lint that holds them.
- [Cloud](CLOUD.md) — what the app and daemon ask of amux.sh.

## Verifying

- [Testing](TESTING.md) — boundaries, lanes, the suite catalogue, how to run, where fixtures and
  evidence live.
- [TestNet](TESTNET.md) — the multi-host harness, `testnet serve` and `testnet ctl`.
- [QA](QA.md) — adversarial testing by agents: running `just qa`, what a run leaves behind, and turning findings
  into tests.
- [Performance](PERFORMANCE.md) — the perf lane, the flood, budgets and baselines.
- [Debugging](DEBUGGING.md) — the one debug report and its two destinations, the facts ring and
  checkpoints, `amux dump`, the reports directory, records and replay.
- [Parameters](PARAMETERS.md) — every tunable value with its starting point, its basis and where it is set.

## Operations

- [Build](BUILD.md) — toolchain, recipes, committed generated code and warm worktrees.
- [CI](CI.md) — workflows, lanes, `just premerge`, `just ci` and the Windows cross-check.
- [Release](RELEASE.md) — shipping the iPhone app and the daemon's release feed and channels.
- [Licensing](LICENSING.md) — how the repository is licensed.
- [Logo](BRAND.md) — the editable vector master, `just logo` exports and the visual review.
