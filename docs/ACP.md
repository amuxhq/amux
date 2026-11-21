# ACP agents

*For developers changing how amux runs agents that speak the Agent Client Protocol, or adding one it knows by
name.*

Besides Claude Code and Codex, amux runs any agent program that speaks the
[Agent Client Protocol](https://agentclientprotocol.com) (ACP): JSON-RPC over the program's stdin and stdout, the
protocol editors such as Zed use to drive coding agents. Antigravity (`agy acp`), OpenCode (`opencode acp`) and Grok
(`grok agent stdio`) are built in; a person adds any other under `[agents.<id>]` in the config
([the user guide](guide/use/other-agents.md)). This page ties the pieces together; the interpreter's rules in
detail are in [Interpreters](INTERPRETERS.md#acp-agents), and how each row reaches a client is in
[Chat vocabulary](CHAT_VOCABULARY.md#acp-agents).

## The problem it solves

Every bespoke integration is written against one program's own protocol, and there are dozens of agent programs.
ACP lets one integration serve all of them, including ones amux has never heard of. The goals that shaped it:

- A person picks an agent, never a protocol. The word ACP appears only where a person adds their own agent and
  needs to know it must speak it.
- One interpreter, one wire envelope and one set of chat rows serve every ACP agent. Where agents differ, the
  difference comes from what the agent says at `initialize`, plus a short table of known ways for the built-ins.
- An ACP agent is an amux agent on every client, with the chat, asks, settings and lifecycle Claude and Codex
  have wherever the agent supports them, and visibly without them where it does not.
- Clients decide what to offer from typed capabilities and a descriptor the host sends, never by matching the
  agent's kind or name.

## The three layers

As for Claude and Codex ([Provider crates](PROVIDER_CRATES.md)), ACP is a protocol crate, a pure interpreter and a
host module, plus the registry that says which programs a host offers.

| Layer | Where | Holds |
|---|---|---|
| Protocol | [`acp-protocol`](../crates/acp-protocol/src/lib.rs) | Every ACP v1 message in both directions as types, with a tolerant decoder for production, a strict one for the recording checks, and encoders. A test generates every message from ACP's JSON schema, vendored in the crate's `schema/`, and holds the types to it. |
| Interpreter | [`interpret::acp`](../crates/interpret/src/acp/mod.rs) | What each message means and what to send: items, asks, the queue, settings, the turn, reopening a conversation, sign-in. No clock, no IO. |
| Host | [`agent::acp`](../crates/agent/src/acp.rs), with the start and handshake in [`agent::provider`](../crates/agent/src/provider.rs) | Starting the program over stdio on every platform, the handshake's requests, prompts with their attachments as content blocks, amux's tool server given as an MCP entry. |
| Registry | [`wire::agents`](../crates/wire/src/agents.rs) (the built-ins), [`settings::agents`](../crates/settings/src/agents.rs) (`[agents.<id>]`), [`node::agents`](../crates/node/src/agents.rs) (merging them and finding each command) | Which programs a host can run, by id, and which it offers: those whose command is found. |

On the wire an ACP agent is one kind, `KIND_ACP`, with its own envelope (`AcpItem`, `AcpSnapshot`, `AcpInput`,
`AcpAsk`, `AcpAnswer` in [`acp.proto`](../proto/amux/v1/acp.proto)). Which agent it is travels as a plain string, the
provider id (`antigravity`, `grok`, `my-agent`), in `AcpCreateConfig.agent`, the spec's `provider` and every
message that names a provider. Claude and Codex have provider ids too (`claude`, `codex`), so no client keeps a
closed list of providers.

amux offers an agent no file-system and no terminal client capabilities, so the agent does its own I/O. It does
offer elicitation forms and links, which become asks, and Grok's own question request
(`_x.ai/ask_user_question`), which becomes a question ask.

What an agent presents in its reply becomes an attachment at its place in the reply: an image, audio, an embedded
file, and a local file it links, which the agent process reads and stores
([Attachments](ATTACHMENTS.md#bytes-and-metadata)). A call to amux's `attach` tool carries the attachment it made, as
Claude's does. What a tool call holds (an image a tool read, a file's contents) is what the tool saw, so it is never
an attachment, and a tool call's images stay hidden.

## Quirks by agent id

The interpreter assumes only what ACP says. Where a built-in agent does something the protocol leaves open,
`interpret::acp::quirks` returns a `Quirks` for that id, and each quirk names the recording that shows it, under
`crates/interpret/fixtures/acp/live/`. An agent a person adds gets the default, so it is driven by what it
advertises alone. The table of quirks is in [Interpreters](INTERPRETERS.md#acp-agents); what each built-in did in
every recorded scenario is the evidence behind it.

The rule for a new quirk: first check whether the behaviour can be read from what the agent advertises, or handled
generically without harm to other agents (a setting change's answer is authoritative; a thought level is found by
its option's category, not its id; `session/set_mode` is called only where the agent offers modes). Only what
cannot goes in the table, with its recording.

## Resumability

Whether an agent can continue a conversation in a new process is decided by what its own `initialize` says, not
by its id: `session/resume` where it offers it, else `session/load` (whose replay of the conversation is swallowed
until the load's answer), else nothing. The answer is kept on the agent's snapshot (`AcpSnapshot.reopen`) and in
its capabilities (`resumable`), from each process's own `initialize`, since it can change with the agent's version.

An agent that offers neither cannot be resumed at all, and amux never pretends it can:

- It never suspends, because the interpreter's `holds` reports its open session.
- When its process ends for any reason (it exits, it is killed, the machine restarts), the agent has ended for
  good. Its next process stops after `initialize`, opens no session, and exits saying the agent cannot reopen
  its conversation in a new process (`interpret::acp::UNRESUMABLE`).
- No client offers a resume: the composer reads `Ended · <agent> can't reopen this conversation` and takes no text.

Starting a fresh conversation in its place is never passed off as a resume, and a refused resume or load ends the
incarnation rather than falling back to a new session, so an agent's session never changes under it.

## Sign-in

amux never signs a person in to an agent. What it knows of sign-in comes from the agent:

- **At `initialize`**, where a built-in shows it (Grok lists the `cached_token` auth method only while signed in).
- **From a refused session**: a session refused `AUTH_REQUIRED` does not end the agent. It waits, idle and signed
  out, holding every prompt in its queue, and each prompt that arrives asks again (`AcpReopen`) with the very
  request that was refused, so a refused resume is asked again as a resume. Once the session opens, the queue
  drains in order.

The words a person sees for how to sign in, carried as the descriptor's `sign_in_hint`, are the agent's own where
an auth method's description names a command to run ("Run `opencode auth login` in the terminal"). Where none does
(Antigravity's "Log in with your Google account", Grok's "Cached token from ~/.grok/auth.json"), a built-in's hint in
the registry says what to run instead, each checked against the program's own help: "Run `grok login`.", and
"Run agy and sign in when it asks.", since `agy` has no sign-in command. An agent a person added keeps its first
description. The host takes the
sign-in its agents meet as its word on that agent and republishes its entry when a conversation disagrees with
what the host last said, so New Agent marks the agent not signed in everywhere.

## The host's catalogue

A host tells clients what each agent offers (models, efforts, modes, commands) before anyone starts one, so New
Agent can offer pickers. For an ACP agent, asking must leave nothing behind in the agent's own history:

- The catalogue helper (`amux internal catalogue <id>`, in [`agent::catalogue`](../crates/agent/src/catalogue.rs))
  runs the agent's command, sends `initialize` and stops. It learns the version, resumability, the prompt
  capabilities and the sign-in methods, plus whatever a built-in lists without a session (Grok's models and
  commands in `initialize`'s `_meta`).
- Anything else comes from the agent's most recent conversation on that host: each conversation publishes its
  catalogue, and [`node::offers`](../crates/node/src/offers.rs) keeps the newest (`Learned`). Before any
  conversation, New Agent offers no models for an agent that lists none without a session, and the agent starts on
  its own defaults.
- A person's model and effort picks are remembered per agent id (`new_agent.<id>`), as for Claude and Codex; a mode
  pick never is, since the default comes from the agent's own settings.

## Capabilities and descriptors

A client never matches on kind or provider to decide what to offer. Each host entry lists every provider it offers
as a `ProviderOnHost` with:

- a `ProviderDescriptor`: the name people see (the built-in's, or the `name` of a person's entry, else its id),
  the kind a new agent starts as, the sign-in hint and the version;
- `AgentCapabilities`: whether it stops one background job, switches back to an earlier conversation, has a
  terminal of its own to attach to, is resumable (and so suspendable), takes a queued prompt into a running turn,
  takes images, takes files, and passes slash commands it does not list.

For Claude and Codex the capabilities are the kind's ([`wire::capabilities`](../crates/wire/src/capabilities.rs)).
For an ACP agent they come from its `initialize` and its quirks (`AgentCapabilities::acp`): send-now only for an
agent known to fold a prompt into its running turn (Antigravity), images from its prompt capabilities, files always
(to an agent that takes no embedded contents, a file goes named by its path for it to read), resumable from its
session capabilities, and never a terminal or a job stop. A running agent's own snapshot carries its capabilities
too, which is what a client reads for that agent. `just client-kind-check` fails a client crate that decides by
kind outside the decode boundary.

Analytics name a built-in by its id and every agent a person added as `custom`, since an id is the person's own
words.

## Adding a built-in agent

1. **Record it.** Add the agent to the live lane's list in
   [`crates/qualification/tests/acp/mod.rs`](../crates/qualification/tests/acp/mod.rs) and record every scenario
   under the owner's sign-in: `just live acp <agent> all`. Write what it did in each scenario beside the others'
   and look for what it does that the protocol leaves open.
2. **Name it.** Add a `BuiltIn` to `wire::agents::BUILT_INS`: its id, the name people see, the command and the
   arguments that make it serve ACP on its stdio, taken from what the agent's makers publish for ACP clients. A
   built-in is offered only where its command is found, and amux never installs it.
3. **Read it.** Run its recordings through the interpreter with goldens (`crates/interpret/fixtures/acp/`, prefixed
   by the agent), add a quirk only for what step one showed that nothing advertised tells, and keep the test that
   replays every recording with nothing unrecognized passing.
4. **Count it.** Give it an analytics kind of its own in [`analytics`](../crates/analytics/src/event.rs) and map
   its id in [`node::telemetry`](../crates/node/src/telemetry.rs); otherwise it counts as `custom`.
5. **Show it.** Add it to the table in [the user guide](guide/use/other-agents.md). The Apple apps draw a
   built-in's own glyph only where its licence allows bundling it, else its name (see
   [the Mac app](MAC.md)).

## Testing

| What | Where |
|---|---|
| The protocol against the schema and the recordings | `acp-protocol`'s tests ([Provider crates](PROVIDER_CRATES.md#who-checks-what)) |
| The interpreter, recording by recording, and the rules no recording shows | `crates/interpret/tests/acp.rs`, `crates/interpret/fixtures/acp/` |
| A scripted agent built only from recorded shapes, with flags for an agent that cannot reopen a session, one signed out until a file exists, the three mid-turn behaviours and more | `fake-acp` in [`provider-fakes`](../crates/provider-fakes/src/acp.rs) |
| The agent process hosting it | `crates/agent/tests`, against `fake-acp` |
| Across hosts: created and driven from a paired host, its offers read there | `crates/testnet/tests/spec_acp.rs` |
| The terminal app end to end: create from config, permissions, interrupt, queue, an agent that ends for good, sign-in | the `acp-*` terminal journeys ([Testing](TESTING.md#journeys)), on `journeys/topologies/terminal-acp.json`, whose host adds its agents in its own config |
| The real agents, scenario by scenario | `just live acp <agent> <scenario>` ([Testing](TESTING.md#lanes), qualification lane) |

## Decisions and what was rejected

| Decision | Rejected, and why |
|---|---|
| One kind, `KIND_ACP`, with one envelope; the agent is a string id | A kind per agent reusing ACP's types: the same switch in every client N times, and an agent a person adds could never have a kind, since proto enums are fixed at build time. This is not merging two providers' envelopes: it is one protocol. |
| The provider is a plain string id for every agent, Claude and Codex included | Keeping the provider enum and adding values: it cannot name an agent a person adds. |
| Clients read capabilities and a descriptor | Branching on kind in clients: new kinds fell through to "Claude" or "Codex" silently. |
| An agent that cannot reopen a conversation never suspends and its exit is final | Starting a fresh conversation and calling it a resume: that is still presenting a different conversation as the same one. |
| amux holds the queue for every agent; send-now only where the agent is known to steer | Sending concurrent `session/prompt`s: ACP does not define it, and the three built-ins do three different things with it. |
| The host's catalogue comes from `initialize`, a built-in's session-less sources, then the last conversation | Opening a throwaway session to ask: it leaves a stray conversation in the agent's own history, and none of the built-ins offers a way to delete one. |
| A hand-typed protocol crate checked against the vendored schema | The official `agent-client-protocol` crate: barred by the dependency policy, and not tolerant in the way amux decodes. |
| No file-system or terminal client capabilities | Running the agent's files and terminals through amux: more surface for no gain to a person now. |
| Built-ins curated in code, commands taken from the official ACP registry; a person's own in config | Installing agents from the registry at runtime: it downloads code, and its Antigravity entry runs a separate bundled binary rather than the `agy` people have. |
| Config `[agents.<id>]` with `name`, `command`, `args`, `env` | `[acp.<id>]`: it puts the protocol in front of people. |
| What an agent presents in its reply is an attachment; what a tool saw is not | Attaching what tool calls carry: a read file or a screenshot a tool took is the agent's input, not something it shows a person, and every device would fill with them. |
| A titled choice in an agent's form (`oneOf` of constants) is offered by its titles, and the agent is answered with the constant | Leaving it to the client's form: it read such a field as typed text, and Antigravity took a typed title for no answer. |
