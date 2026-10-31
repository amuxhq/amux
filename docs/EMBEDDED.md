# The embedded runtime

*For developers working on the Rust side of the iPhone app, or building another rich client on the same layer.*

A rich client needs three things from Rust: the chats and fleet it draws, a
runtime to read them from, and a way to call both from its own language. The
app layer splits those into three crates:

| Crate | Owns |
| --- | --- |
| `app-runtime` | The chats and fleet over a local runtime: session and fleet drivers, views asked for by row key, changes gathered until the host's next main-thread turn, and the values a host passes in and gets back |
| `app-embedded` | The daemon hosted in the client's own process: one installation with one profile per account, each with its identity, trust, store of replica rows, pairing, direct links and relay link; and `Phone`, the driver that steps its cores on the client's queue and confines its transport to one thread |
| `app-ffi` | The C ABI over both: exported `amux_*` functions, JSON in and out, a wake descriptor and a step, tickets for acts, opaque handles, the `staticlib`, and the header `cbindgen` generates |

The iPhone app is the one client built on them. How the Swift side uses
them is on [the iPhone page](IOS.md).

## The dependency rule

Nothing in `app-runtime` reaches the daemon. It talks to its runtime only
through `client::Client`, the same trait the terminal uses, which has two
implementations: `GrpcClient` over a profile's local socket, and `InProcess`,
the profile's client service called directly. A desktop app that attaches to
a running daemon uses `app-runtime` over `GrpcClient` and needs no
`app-embedded`; the phone uses it over `InProcess`, which `app-embedded`
hands out per profile.

`scripts/check-dependency-policy.py` (`just dependency-policy`) holds every
local edge to a fixed list:

| Crate | May depend on |
| --- | --- |
| `app-runtime` | `client`, `model`, `ui-runtime`, `ui-state`, `ui-view`, `wire` |
| `app-embedded` | `app-runtime`, `client`, `node`, `wire` |
| `app-ffi` | `app-embedded`, `app-runtime`, `client`, `model`, `node`, `ui-view` |

`app-runtime` is also one of the UI crates, which may not reach `node`,
`store`, `interpret`, `agent`, `claude`, `codex` or `pty-host` directly or
through anything else, and no production crate may depend on test support.
`app-embedded` and `app-ffi` host the runtime in process and are the
deliberate exception to "no UI crate links the daemon".

What ships on a phone is checked from the other end too. `just ios
graph-check` walks `cargo tree -p app-ffi` for the iOS simulator target and
fails if `pty-host`, `codex`, `agent`, `testnet`, `claude-specs`,
`codex-specs` or `replay-support` appears, with and without the driving
tools. `just mobile-check` checks `node`, `client`, `ui-state`,
`ui-runtime`, `app-runtime` and `app-embedded` for the iOS device and
simulator targets.

## What a client reuses

- **Views.** Rows, ask cards, the overview, the composer, fleet cards and the
  review document are `ui-view` values, the same ones the terminal renders.
  A client draws them; it does not recompute grouping or phases. See
  [the chat vocabulary](CHAT_VOCABULARY.md) for what each one means.
- **Session and fleet drivers.** `ui-runtime`'s `Session` and `Fleet` hold the
  subscription, re-tail and reconnect, paging and acts. `AppRuntime` in
  `app-runtime` hosts them for one profile and adds what a host with its own
  main thread needs: a wake at most once per turn, and rows handed out by key.
- **The runtime.** `EmbeddedRuntime` in `app-embedded` is `node` started in
  process with no agent kinds, so it hosts no agents and serves replicas of
  the agents on the machines it pairs with.
- **The values.** Everything a host passes in or gets back is a plain Rust
  value serialized as JSON, with a generated mirror in the client's language.

How a chat is opened and kept current against the local runtime is on
[the client page](CLIENT.md).

## Chats and the fleet

`AppRuntime::open` opens a profile's fleet and resolves once it has caught up
with the local runtime. `open_chat` resolves once the agent's snapshot and
the rows this device already holds are applied, so the host's first read is
correct even with the agent's host away.

Rows are handed out by item key, which never moves. The host holds a list of
keys that changes only at its two edges: newer keys above the newest it
holds, a page of older ones below its oldest, and, while the reader follows
the newest row, the oldest keys dropped as the chat's window trims to its cap
(200 rows, `DEFAULT_CAP`, raised with a longer tail). The host says where its
reader is with `Chat::follow`; while the reader is in history the window
stays put, arrivals are held apart and the frame's `arrivals_held` says so.
It reads the whole list again only when a change batch says it was reloaded,
which a Reset does, and so does a return to the newest row after more arrived
than the window holds.

The host is told the fleet or a chat moved whenever it did, and at most once
until it takes that one's changes. However many updates land in between, the
host takes every changed key together on its next turn (`Coalescer`, in
`app-runtime/src/coalesce.rs`), so it reconfigures only the rows that
changed.

## The loop

The client owns the loop; Rust never calls into it. `Phone` in
`app-embedded` starts the installation with its profiles' daemon cores held
for stepping (`node::Cores`) rather than on threads of their own, and runs
links, channels, gRPC and the client streams on one confined tokio thread.
That thread posts what it decodes onto the cores' input queues, and every
input posted, like every view that moved and every act that ended, makes a
wake descriptor readable. The client watches the descriptor from one serial
queue and calls `step` there, which reads the descriptor empty and steps
every input waiting for every core, a batch per core at a time, on that
queue. So a core steps one thing at a time on the client's queue and
nowhere else, while its timers and jobs still run on the transport thread
and come back as inputs.

A call that waits for the cores to do something (starting, stopping, a
profile or chat opening) runs its work on the transport thread and steps the
cores on the calling thread meanwhile, so it never waits on a queue the
calling thread holds. Through the C ABI a step answers JSON with what moved
in each open fleet and chat as a delta, the profile list when it moved, and
the results of acts, each under the ticket its act returned when it started
and after the deltas of what it moved.

## Profiles

`EmbeddedRuntime` is one installation with one profile per account, exactly
as a desktop daemon is. Its registry calls create, delete, bind and sign
out profiles by id, and every per-profile call names the profile it is for.

- **Bind** takes a refresh token a sign-in produced, with the account
  service's address and the OAuth client it was issued to. From then on the
  profile holds and spends that token, fetches relay credentials and keeps
  its relay link up. The client never talks to the account service on the
  profile's behalf; it borrows a bearer (`access_token`) for its own calls.
- **Source policy** (`set_source_policy`) says whether a profile keeps a
  source open for every agent it lists or only for the chats that ask.
- **Discovered** hands the whole set of machines the platform found on the
  local network to every profile. On iOS only the system may browse, so
  `EdgeOverrides::discovery` defaults to none.

Pairing (`begin_pair`, `confirm_pair`, `abandon_pair`, `unpair`), the roster
of trusted machines and account state are per-profile calls too; creating,
renaming, stopping and deleting agents on a paired machine go through the
profile's `AppRuntime`. `refresh_entitlement` asks the account
service what the account buys now, so a purchase lifts the relay's tier
without waiting for the next credential renewal.

## Seams

Nothing test-shaped is compiled into a product crate by default. Where a
harness needs to reach in, the crate exposes an ordinary, always-compiled
injection point:

- `EmbeddedRuntime::start_with` takes `EdgeOverrides` (where the relay link
  dials, local-network discovery, where direct links listen) and a clock.
  `start`, which the phone uses, passes the defaults and the system clock.
- `app-ffi`'s `debug-tools` feature, never a default, is what a driving build
  links. It lets `StartConfig.lan_bind` keep direct links on loopback, lets
  `StartConfig.relay_tcp` dial a served test relay's plaintext carrier, and
  enables `amux_runtime_offer_pairing`, which puts a profile into pairing mode
  so tests can pair two profiles. A shipping build ignores the two fields and
  refuses the call; `amux_version` reports `+debug-tools` when the feature is
  on, and the app's measured runs check it.

The machines on the other side of every test are the `testnet` crate: real
daemons, one relay and a stand-in account service, driven in process by Rust
specs or out of process by `testnet serve`. See [the testnet page](TESTNET.md).

## Generated Swift types

Every value the bridge carries has a Swift mirror in
`apps/apple/Packages/AmuxCore/Sources/AmuxValues/Values.swift`, generated
from the Rust definitions:

```sh
cargo run -p ios-tools -- swift-types          # regenerate
cargo run -p ios-tools -- swift-types --check  # fail when the committed file differs
```

`crates/ios-tools/src/swift_types.rs` lists the root types, from `model`,
`ui-view` and `app-runtime::values`; everything they hold comes along. The
schemas come from `schemars`. A struct becomes a Swift struct with the Rust
field names as coding keys; a string-only enum becomes a raw-value enum; any
other enum is serde's externally tagged form with its `Codable` written out;
a variant holding an optional value writes `null` for none, so the variant is
still named. A schema shape outside those fails the generation rather than producing a
mirror that decodes wrongly. Byte strings, such as agent and input ids, are
JSON arrays of numbers.

Two tests in that file keep the mirrors honest: the committed file must match
what the generator produces, and every public struct and enum in `model`,
`ui-view` and `app-runtime::values` must have a mirror, apart from a short
list that never crosses (the answer body a choice sends, and borrowed option
types).

Some view values hold wire types. Those protobuf types are generated with
serde and schema derives so they can be mirrored too: `VIEW_VALUES` in
`crates/xtask/src/main.rs` names them, and `just protobuf` (`xtask codegen`)
regenerates the committed wire code with those derives. Adding a wire type to
a view value means adding it there.

## The C ABI in outline

Everything crosses as JSON; every returned string is the caller's to free with
`amux_string_free`, and every `AmuxBytes` with `amux_bytes_free`. A null
return means the call failed, and a panic is caught at the boundary and reads
as a failure. Reads return at once. An act that waits returns a ticket at
once, and its JSON result comes back from a later `amux_runtime_step` under
that ticket; nothing in the library calls into the client.

| Family | Handle | What it covers |
| --- | --- | --- |
| `amux_runtime_*` | `AmuxRuntime` | The installation: start and stop, the wake descriptor and the step, lifecycle inputs (foreground, background, suspending, network changed), the profile list, the registry (create, delete, bind, sign out), source policy, discovered machines, bearers, entitlement refresh, which profiles trust a host, product analytics (the setting and the app's own events) |
| `amux_profile_*` | `AmuxProfile` | One profile open on screen: pairing, the roster, account state, creating agents and listing directories, renaming, stopping and deleting agents, the dump |
| `amux_fleet_*` | `AmuxProfile` | That profile's fleet: rows, cards, family headers, hosts, and taking its changes |
| `amux_session_*` | `AmuxChat` | One open chat: keys and rows by key, the ask card, overview, settings and frame, taking changes; sending, answering, withdrawing, interrupting, resuming, paging, blobs and the review |

The lifecycle nests: start the runtime, open a profile, open chats on it;
close every chat before its profile and every profile before stopping the
runtime.

```c
AmuxRuntime *rt = amux_runtime_start(config_json, &error);
/* watch amux_runtime_wake_fd(rt); call amux_runtime_step(rt) when readable */
AmuxProfile *p = amux_profile_open(rt, profile_id, &error);
AmuxChat *chat = amux_session_open(p, agent_key_json, 0, &error);
/* ... */
amux_session_close(chat);
amux_profile_close(p);
amux_runtime_stop(rt);
```

Every call, its arguments and its threading are in
[the bridge contract](../crates/app-ffi/README.md).

## Building the bridge

The bridge is `app-ffi` built as `libamux_app.a`, with the header
`amux_app.h`, packaged into an XCFramework whose module is `AmuxApp`.
`apps/apple/Tools/ios_bridge.py` holds the names and layout both bridge recipes share.

```sh
just ios rust      # the simulator slice a development build links
just ios package   # every shipping slice, and the linkage check
```

- `just ios rust` builds the simulator slice with `debug-tools` under the
  `phone-dev` profile (`release` without LTO, so a Rust edit rebuilds
  quickly), into `target/ios/AmuxAppDebugTools.xcframework`, which
  the Debug and Measured app configurations force-load. It also stages that
  slice as `target/ios/AmuxApp.xcframework`, so the Swift packages' unit
  tests link current Rust. When no Rust input changed since its last run it
  runs no cargo at all, so a Swift-only edit pays nothing here. `just ios
  build` and `just ios unit` depend on it.
- `just ios package` builds the simulator and device slices without
  `debug-tools` under the `release` profile (fat LTO, one codegen unit,
  `panic = "abort"`, optimised for speed; the Release app links with
  `-no_exported_symbols` so the linker can dead-strip the bridge), assembles
  `target/ios/AmuxApp.xcframework`, and links it from Swift on the pinned
  simulator to prove it loads and carries SQLite statically. Release and the
  shipping recipes depend on it.

Each target triple has its own Cargo target directory under
`target/ios/rust-cargo/`, so simulator and device builds never invalidate each
other, and `SDKROOT` is left unset for the same reason. Each run writes the
slice sizes to `target/ios/size.txt`.

## Tests

| Command | What it proves |
| --- | --- |
| `just test-crate app-runtime` | The host is woken once however many updates land and hears them as one delta; a host's keys moved only by deltas follow the window through every seeded schedule of the client model, and through a trim to the cap; new rows arrive above the newest held and older ones below the oldest, each read when drawn; collapsed runs and the chat frame read as the views say; a settings pick reaches the agent |
| `just test-crate app-embedded` | A phone pairs by PIN and reads a machine's agents from its own rows; relinks when its browser finds the machine again; each account is a profile and only the resumed one holds a relay link; a signed-in phone reaches its hosts over the relay; a dump carries the runtime's log, redacted |
| `just test-crate app-ffi` | Through the C functions, with the phone's state held only from openings and deltas: pairing by PIN, the fleet, a chat's rows by key, a permission answered by position and a question by picks, paging (handed over before its answer) and a dump; and nothing closed is handed over while its acts are in flight |
