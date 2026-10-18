# Parameters

*For operators and developers tuning amux.*

Every number the design leaves open is a named value with one owner. This page
lists each one: the value amux starts with, what that value rests on, and where
it is set. Most starting values are reasoned rather than measured; the basis
column says which. When a measurement settles one, change the value where it
is set and the basis here.

Only a few values are in the configuration file; the rest are build constants
or fields of `node::Launch`, the struct the daemon starts agents and runs its
background work with. The installation configuration is
`$XDG_CONFIG_HOME/amux/config.yaml` (by default `~/.config/amux/config.yaml`),
parsed as `settings::InstallationConfig` in
[`crates/settings/src/lib.rs`](../crates/settings/src/lib.rs). The keys this
page names are:

```yaml
retention:
  own_budget_mib: 2048      # own agents' rows, per profile
  replica_rows_mib: 256     # rows replicated from paired hosts, per runtime
  replica_blobs_mib: 512    # blob files fetched from paired hosts, per runtime
agent:
  grace_secs: 300           # how long an agent outlives its daemon
  drain_secs: 300           # how long an orphaned ask is held while draining
  facts_ring_mib: 4         # the interpreter's facts ring, both segments together
```

Agent settings are copied into each agent's `spec.<n>` at spawn and resume, so
a change affects the next incarnation, never a running agent. The phone's
embedded runtime reads no configuration file and runs with the defaults.

## The table

| Parameter | Owner | Starting value | Basis | Where it is set |
| --- | --- | --- | --- | --- |
| Journal segment size | agent | 1 MiB | Reasoned. Large enough that rotation, and the store flush that reclaiming a segment costs, is rare; small enough that reclaiming frees disk promptly. The flood workload's backlog ceiling assumes twenty agents each filling a segment a second. Not measured. | `DEFAULT_SEGMENT_BYTES` in [`crates/agent/src/host.rs`](../crates/agent/src/host.rs), used when the spec's `EffectiveConfig.journal_segment_bytes` is zero; the daemon sets that from `Launch.journal_segment_bytes` (zero by default, not a configuration key) |
| Terminal log segments kept | agent | 4 segments of 256 KiB | Reasoned: enough bytes to rebuild one full screen for a terminal that attaches late. Not measured. Only terminal Claude keeps a terminal log. | `PTY_SEGMENT_BYTES` and `PTY_SEGMENTS_KEPT` in [`crates/agent/src/host.rs`](../crates/agent/src/host.rs) |
| Grace after control-socket end of stream | agent, from spec | 5 minutes | Reasoned: minutes, enough to ride out a daemon update or crash restart, whose supervisor limits are 60 s to start and 30 s of backoff. | `agent.grace_secs` (`AgentSettings`), copied to `EffectiveConfig.grace_ms`; `DEFAULT_GRACE_MS` in [`crates/agent/src/host.rs`](../crates/agent/src/host.rs) when the spec leaves it zero |
| Drain deadline for an orphaned ask | agent, from spec | 5 minutes | Reasoned: the same order as the grace. | `agent.drain_secs`, copied to `EffectiveConfig.drain_ms`; the agent falls back to `DEFAULT_DRAIN_MS`, the same 5 minutes, only when the spec leaves it zero |
| Facts ring size and segments kept | agent, from spec | 4 MiB across 2 segments | Reasoned: enough recent provider history to replay into a dump without keeping a second transcript. Not measured. | `agent.facts_ring_mib`, copied to `EffectiveConfig.facts_ring_bytes`; `DEFAULT_RING_BYTES` in [`crates/agent/src/host.rs`](../crates/agent/src/host.rs); the segment count is `KEEP = 2` in [`crates/core-ring/src/lib.rs`](../crates/core-ring/src/lib.rs) (see note 1) |
| Fan-out ring capacity | daemon | 1,024 events per agent (four ingest batches); 1,024 for the inventory | Reasoned from the batch: a batch's records are published in one burst while the store lock is held, so a reader's slack is counted in batches. Four batches absorb a reader whose task woke late by design; a stalled one is closed with `Lagged` and re-tails. The flood workload's ingest-lag budget (p99 under 250 ms) is stated as one ring's worth of work; that metric has no recorded baseline yet. | `FANOUT_CAPACITY` (`Launch.fanout_capacity`) and `Launch.inventory_capacity` in [`crates/node/src/runtime.rs`](../crates/node/src/runtime.rs); not a configuration key |
| Push notification delay | daemon | 30 s | Reasoned: tens of seconds, long enough that an answer from the desktop removes the notification before it is sent. | `Launch.notify_delay_ms` in [`crates/node/src/runtime.rs`](../crates/node/src/runtime.rs) (see note 2) |
| Own retention trim chunk | daemon | 4 MiB | Reasoned: a few MiB from the largest live agent per round, so a small overrun costs a small trim. | `Launch.retention_chunk_bytes` in [`crates/node/src/runtime.rs`](../crates/node/src/runtime.rs) |
| Own retention budget per profile | daemon config | 2,048 MiB | A generous multiple of the largest transcripts seen so far. Not measured against typical transcript sizes. Counts row bytes (`store::item_bytes`), not blob files, which go with their agent's directory. | `retention.own_budget_mib` (`RetentionSettings`) → `Launch.own_budget_bytes` |
| Replica budget per runtime (rows) | runtime config | 256 MiB | A round starting bound. Not measured. | `retention.replica_rows_mib` → `Launch.replica_budget_bytes` |
| Replica blob budget per runtime (files) | runtime config | 512 MiB, least recently read first | Reasoned: twice the row budget, since images dominate what is fetched. Not measured. | `retention.replica_blobs_mib` → `Launch.replica_blob_budget_bytes`; eviction in `store::BlobLru` ([`crates/store/src/blobs.rs`](../crates/store/src/blobs.rs)) |
| Latency budget, input to its reflection on screen | whole design | A few ms locally | By construction: one control-socket hop for the verdict, one commit, one fan-out. No workload measures it end to end yet (see note 3). | No single constant; the path is `ProfileRuntime::relay` in [`crates/node/src/relay.rs`](../crates/node/src/relay.rs) and `ProfileRuntime::ingest` in [`crates/node/src/runtime.rs`](../crates/node/src/runtime.rs) |
| Update check interval | supervisor | 1 hour | Reasoned: a corrective release reaches every host within the hour. | `Params.check_interval` in [`crates/node/src/supervisor/mod.rs`](../crates/node/src/supervisor/mod.rs); build constant |
| Rollback threshold K | supervisor | 3 starts that never activate | A guess to be revised when failed starts in the field say otherwise. | `Params.rollback_after` |
| Start deadline (spawn to prepared) | supervisor | 60 s | Reasoned: covers a migration and the daemon's read-only look at every agent directory. Not measured. | `Params.start_deadline` |
| Stop deadline (signal to kill) | supervisor | 30 s | Reasoned: a clean shutdown flushes every store before it sets the clean flag. | `Params.stop_deadline` |
| Restart backoff after activation | supervisor | 1 s doubling to 30 s; reset after 60 s up | Reasoned. | `Params.backoff_first`, `backoff_max`, `backoff_reset` |
| Entitlement refresh timeout | cloud link | 30 s | Reasoned: the request is answered only by a live connection, and the phone asks right after a purchase; a link between connections must answer the phone rather than hold it. | `REFRESH_TIMEOUT` in [`crates/node/src/edge/cloud.rs`](../crates/node/src/edge/cloud.rs) |
| Manifest size limit | supervisor | 1 MiB | Reasoned: a manifest is a few entries of a few hundred bytes; a longer answer is not one and is refused before it is buffered, so a server without the release key cannot exhaust memory. | `MANIFEST_LIMIT` in [`crates/node/src/supervisor/mod.rs`](../crates/node/src/supervisor/mod.rs) |
| Artifact size limit | supervisor | 256 MiB | Reasoned: the binary is tens of megabytes; the signature is checked only once the download is whole, so without a bound a server without the key could fill the disk. | `ARTIFACT_LIMIT` in [`crates/node/src/supervisor/mod.rs`](../crates/node/src/supervisor/mod.rs) |
| Composer and tool-server retry over an update | client, tool server | Clients reconnect after 250 ms, doubling to 5 s. The tool server retries a call for 5 s, backing off from 50 ms to 500 ms. | Reasoned: a daemon update takes a few seconds. An input already in flight is never retried; it shows as not confirmed. | `RECONNECT_FIRST_MS` and `RECONNECT_MAX_MS` in [`crates/ui-runtime/src/lib.rs`](../crates/ui-runtime/src/lib.rs); `RETRY_WINDOW` in [`crates/agent/src/tools.rs`](../crates/agent/src/tools.rs) |
| Driver trace length | ui-runtime | 400 events, memory only | Reasoned: a few hundred events cover the transitions a dump needs. The trace keeps between half this and this many. | `TRACE_EVENTS` in [`crates/ui-runtime/src/trace.rs`](../crates/ui-runtime/src/trace.rs) |
| Tail count on open (N) | client | Terminal: 40 rows. Phone: 200 rows. Both at most K. | Terminal: about a screen, with older rows paged as needed. Phone: the whole tail its own runtime keeps, so a chat opens with scroll-back already held. Not measured. | `PAGE` in [`crates/tui/src/chat/layout.rs`](../crates/tui/src/chat/layout.rs); `DEFAULT_TAIL` in [`crates/app-runtime/src/lib.rs`](../crates/app-runtime/src/lib.rs), overridable by `StartConfig.tail`; the daemon caps any tail at K in `open_subscription` |
| Transcript window cap while following | client | Terminal: 200 rows. Phone: 200 rows, or the tail when it is longer. Never below the tail. | Terminal: while the reader follows, the layout pages only to keep 40 rows above the screen, so the window must hold a screen and that lookahead after trimming; rows draw at least one line and 160 lines is taller than a terminal in use. Phone: the tail it opens with; the list pages only at a top marker a following reader sees when the whole window fits on screen, far fewer rows than this. A flood measured on 2026-09-29 wrote about 40 rows a second into one chat; without a cap an open chat held every one. While following, a page never fetches past the cap. Not measured further. | `CAP` in [`crates/tui/src/chat/layout.rs`](../crates/tui/src/chat/layout.rs); `DEFAULT_CAP` in [`crates/app-runtime/src/lib.rs`](../crates/app-runtime/src/lib.rs); passed with the tail as the `Window` to `Fleet::open` in [`crates/ui-runtime/src/fleet.rs`](../crates/ui-runtime/src/fleet.rs) |
| Rows a session holds off screen | client | 16 rows | Reasoned: a fleet row's second line reads what the agent last said or the step it is running, which sit among the newest rows; every live agent holds this many, so it stays a few rows, and a chat opening it pages the rest of its tail from the local runtime. | `HELD_ROWS` in [`crates/ui-runtime/src/fleet.rs`](../crates/ui-runtime/src/fleet.rs) |
| Replica tail and catch-up cap (K) | runtime | 200 rows | Reasoned: about an hour of a busy agent and several screens of scroll-back. An origin answers a delta of at most K rows and a Reset beyond it. Not yet tied to a measured size per row. | `TAIL_ROWS` in [`crates/node/src/runtime.rs`](../crates/node/src/runtime.rs) → `Launch.tail_rows`; also the floor own retention never trims below |
| Source reconnect backoff | runtime | 1 s doubling to 30 s while the host is reachable | Reasoned. | `Launch.source_backoff_ms` and `Launch.source_backoff_max_ms` |
| Lookahead above the screen before paging | client | Terminal: 40 rows. Phone: none; it pages when the top of the list comes into view. | Terminal: one page of held rows. | `LOOKAHEAD` in [`crates/tui/src/chat/layout.rs`](../crates/tui/src/chat/layout.rs); `ChatModel.reachedTop` in [`ChatModel.swift`](../apps/apple/Packages/AmuxCore/Sources/AmuxCore/ChatModel.swift) |
| Page size at an open collapsed run, and its cap | client | The run's length, at least one page, capped at 1,000 rows | Reasoned: a long run arrives in a few pages. | `RUN_PAGE_CAP` in [`crates/tui/src/chat/layout.rs`](../crates/tui/src/chat/layout.rs); `ChatModel.largestPage` and `pageSize` in [`ChatModel.swift`](../apps/apple/Packages/AmuxCore/Sources/AmuxCore/ChatModel.swift); the runtime returns at most 500 items per `Fetch` (see note 4) |
| Loading hint delay | client | 300 ms | Reasoned: a chat that fills at once never flashes the hint. | `LOADING_HINT_MS` in [`crates/tui/src/chat/mod.rs`](../crates/tui/src/chat/mod.rs); `ChatModel.loadingHintDelay` in [`ChatModel.swift`](../apps/apple/Packages/AmuxCore/Sources/AmuxCore/ChatModel.swift) |
| Session retention after last view | client | Phone: 5 minutes. Terminal: none; a chat's session closes when the terminal returns to the fleet. | Reasoned: long enough that going back to a chat is instant, short enough that the sessions a phone holds follow what the person looks at. | `StoreBundle.sessionRetention` in [`StoreBundle.swift`](../apps/apple/Packages/AmuxCore/Sources/AmuxCore/StoreBundle.swift) |
| WAL checkpoint interval | daemon | SQLite's automatic checkpoint (every 1,000 pages) | SQLite's default; amux sets no checkpoint pragma. Not measured. | Not set. Full checkpoints with a drive flush are taken explicitly by `Sqlite::flush_to_drive` in [`crates/store/src/sqlite.rs`](../crates/store/src/sqlite.rs): at clean shutdown and before journal segments are reclaimed |
| Put and fetch unary limit | wire | 64 MiB per attachment; 65 MiB per RPC message; 16 MiB per link control message | Reasoned: an attachment at the limit plus its protobuf framing fits one message. | `ATTACHMENT_SIZE_LIMIT`, `CHANNEL_MESSAGE_SIZE_LIMIT` and `MESSAGE_SIZE_LIMIT` in [`crates/wire/src/lib.rs`](../crates/wire/src/lib.rs) (see note 5) |
| Channel compression | link | zstd level 3, a 128 KiB window, hash and chain tables of 2^15 and 2^14 | Measured on about 16 MB of replayed agent journals (801 agents of all three kinds), twenty agents interleaved to a channel: 5.5x, against 5.8x for zstd's default 2 MiB window; a 256 KiB window gains 4% for 128 KiB more at each end's compressor and as much at its decompressor. Every replayed agent on one channel compresses 4.5x at this window against 5.9x at the default, so a host carrying far more agents than twenty gives some ratio up. The smaller tables cost about 1% of the ratio. One channel end holds about 1.7 MB of zstd state and 160 KB of buffers (a unit test holds it under 2 MB). On the phone that is about 4 MB more streaming footprint with three hosts, accepted: the phone's baseline is 77.1 MB with compression, against 74.8 MB measured without it on the pinned Mac. | `LEVEL`, `WINDOW_LOG`, `HASH_LOG` and `CHAIN_LOG` in [`crates/node/src/transport/compress.rs`](../crates/node/src/transport/compress.rs) |

## Notes

1. **Facts ring segments.** The ring always keeps two segments, each half of
   `facts_ring_mib`; the count is not a setting.

2. **Push delay.** The shipped daemon and the phone runtime install
   `NoopSender` as their push sender, so notification rows fall due after this
   delay and are deleted without a push being sent. `HttpSender` in
   [`crates/node/src/outbox.rs`](../crates/node/src/outbox.rs) is the sender
   that posts to the account service.

3. **Latency.** The daemon relays an input's verdict only after the step that
   explains it is committed and broadcast, so the budget is the agent's journal
   write, one commit and one fan-out. The performance lane
   ([`crates/qualification/src/perf/flood.rs`](../crates/qualification/src/perf/flood.rs))
   has budgets for fleet and chat catch-up, ingest lag and backlog drain, but
   the only ingest figure recorded in the reference baseline
   ([`perf/baselines/desktop/Mac14,6.json`](../perf/baselines/desktop/Mac14,6.json))
   is the median cost per committed frame, about 23 µs. See
   [performance](PERFORMANCE.md).

4. **Page caps.** `Fetch` returns at most `MAX_PAGE` (500) items whatever the
   limit, and a peer's catch-up may ask for at most 10,000 rows (`MAX_CAP`),
   both in [`crates/node/src/serve.rs`](../crates/node/src/serve.rs). A client
   asking for a 1,000-row page at a collapsed run gets 500 and asks again.

5. **Blob size.** An attachment is at most `ATTACHMENT_SIZE_LIMIT` (64 MiB,
   worded "64 MB") of its own bytes; the RPC message limit sits 1 MiB above
   it so a `PutBlobRequest` at the limit fits with its framing.

## Related constants

These are not in the design's parameter list, but sit beside the values above
and are tuned the same way:

| Value | Starting value | Where |
| --- | --- | --- |
| Frames committed per ingest transaction | 256 | `INGEST_BATCH` in [`crates/node/src/runtime.rs`](../crates/node/src/runtime.rs). A bound on how long a backlog drain holds the store lock: at the measured ingest cost of about 23 µs a frame, a full batch holds it for about 6 ms, the most a subscribe, an input or a spawn waits behind a draining agent. The fan-out ring is sized from the batch (four of them), never the batch from the ring. |
| Fully ingested journal segments kept for dumps | 2 | `KEPT_SEGMENTS`, same file |
| Retention sweep interval | 10 minutes | `Launch.retention_interval_ms` |
| Agent process start deadline (spawn to Hello) | 20 s | `Launch.start_deadline_ms` |
| Agent stop deadline before the process group is killed | 30 s | `Launch.stop_deadline_ms` |
| Wait for an interpreter's verdict | 30 s | `Launch.reply_patience_ms`; past it the input is reported lost and the client shows it not confirmed |
| One write on an agent's control socket | 5 s | `Launch.ctl_write_ms` |
| Deliveries outbox retry | 30 s | `Launch.delivery_retry_ms` |
| Push retry after a failure | 60 s | `Launch.push_retry_ms` |
| Wait on an origin for a page | 10 s | `FETCH_PATIENCE` in [`crates/node/src/sources.rs`](../crates/node/src/sources.rs) |
| Largest journal or control-socket frame | 64 MiB | `journal::MAX_FRAME_BYTES`; `agent_dir::MAX_FRAME_BYTES` |
| Fixed per-row allowance in retention's byte count | 48 bytes | `ITEM_OVERHEAD_BYTES` in [`crates/store/src/lib.rs`](../crates/store/src/lib.rs) |
| Analytics batch: wait, size and queue | Sent every 60 s, or at once at 50 events; at most 1,000 waiting, beyond which new events are dropped; at most 100 events in one request, the server's limit | `Params` in [`crates/analytics/src/upload.rs`](../crates/analytics/src/upload.rs) |
| Analytics retries | Two more tries, 5 s then 10 s apart, then the batch is dropped; 10 s per request; a shutdown waits at most 2 s and the phone 3 s as it leaves the screen | `Params` in [`crates/analytics/src/upload.rs`](../crates/analytics/src/upload.rs); `SHUTDOWN_FLUSH` in [`crates/node/src/telemetry.rs`](../crates/node/src/telemetry.rs); `BACKGROUND_FLUSH` in [`crates/app-embedded/src/lib.rs`](../crates/app-embedded/src/lib.rs) |
| Analytics check-in | Once a day per profile, never sooner than a minute after a start | `CHECK_IN_EVERY_MS` and `CHECK_IN_SETTLE_MS` in [`crates/node/src/telemetry.rs`](../crates/node/src/telemetry.rs) |
| Analytics repeats held back | `client_opened` and `relay_refused` at most once an hour per profile | `CLIENT_OPENED_EVERY` and `RELAY_REFUSED_EVERY` in [`crates/node/src/telemetry.rs`](../crates/node/src/telemetry.rs); the phone's in [`crates/app-embedded/src/lib.rs`](../crates/app-embedded/src/lib.rs) |

The analytics values are described in [what amux sends](ANALYTICS.md). What
the other values govern is described in [the journal and store](JOURNAL_AND_STORE.md),
[the wire](WIRE.md), [the agent process](AGENT_PROCESS.md) and
[the supervisor](SUPERVISOR.md).
