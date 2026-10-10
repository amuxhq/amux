#!/usr/bin/env python3
"""Fail when the tree still carries a mechanism amux no longer has.

Each row names something amux removed and lists the names it went by. The
check searches everything git tracks or would track (code, config, protos,
recipes, scripts, workflows and docs) for those names and fails on any hit,
grouped by row, so a survivor cannot come back quietly. DEVLOG.md is history
and is not searched, nor are the checks that name removed things to hold them
gone. It also fails when the workspace
members list and the crates directory disagree, so a dead crate cannot linger
outside the build.

Patterns are POSIX extended regular expressions, as `git grep -E` reads them.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
SELF = Path(__file__).resolve().relative_to(ROOT).as_posix()


@dataclass(frozen=True)
class Row:
    name: str
    goes: str
    patterns: list[str]


@dataclass(frozen=True)
class Exemption:
    pattern: str
    path: str
    why: str


ROWS = [
    Row(
        "agent hosting",
        "provider adapters and session loops inside the daemon, multiplex buffers, byte fan-out, the daemon-side summarizer",
        [
            r"agent-runtime|agent_runtime",
            r"AgentRuntimeFactory|LocalAgentHost",
            r"MultiplexByteBuffer|MultiplexByteReader|MultiplexStructuredBuffer|MultiplexStructuredReader",
            r"BroadcastBuffer|BroadcastReader|BufferPolicy",
            r"StructuredLogSource",
            r"SummarizerHandle|SummarizerPublication|SummaryCut|summarizer_protocol",
            r"select_summary",
            r"[Ss]ummarizer",
            r"daemon_summarizer|test-daemon-summarizer",
            r"daemon_protocol|test-daemon-protocol",
            r"daemon_sdk|test-daemon-sdk",
        ],
    ),
    Row(
        "suspend and restore",
        "kill-and-relaunch suspend, suspended state files, consumed seals, the suspend RPCs and verbs",
        [
            r"SuspendAll|ResumeAll|suspend_all|resume_all",
            r"SuspendReason|SuspendReport|ResumeReport|AgentResumeStatus|AgentResumeResult|ProfileSuspendResult|ProfileResumeResult",
            r"SuspendedServerState|SuspendedAgent|SuspendedLocalAgentNameSource|save_suspended|load_suspended|remove_suspended",
            r"suspended[.]yaml|consumed-seals[.]yaml",
            r"ConsumedSeals|consume_seals|invalidate_seals|SealedAt",
            r"ClaudeSuspendRecord|CodexSuspendRecord",
            r"suspend_for_update_if_running|thaw_update",
            r"ServerCommands::(Suspend|Resume)",
            r"SuspendRestart|suspend_restart_agents",
            r"LINK_CLOSE_REASON_SUSPENDING|LinkCloseReason::Suspending|ShutdownReason::Suspending",
        ],
    ),
    Row(
        "sequence numbers",
        "sequence epochs, replay facts, gap and reset detection, expected_seq on input",
        [
            r"expected_seq",
            r"SequenceNumberMismatch|SEQUENCE_NUMBER_MISMATCH",
            r"[Ss]equence epoch|sequence_epoch",
            r"SequencedReplayQuery|ByteReplayQuery|structured_replay_has_gap|structured_replay_facts",
            r"ReplayOutcome",
            r"missing_after",
        ],
    ),
    Row(
        "wire",
        "session subscriptions and replay outcomes, terminal bytes over the network, summary inventory events, the old AgentRef and the duplicate client requests, agent kinds and Claude drivers, the test agent, artifact references, diff responses, Debug",
        [
            r"ReplayFacts",
            r"ReplayQuery",
            r"ReplayComplete",
            r"SubscribeSession",
            r"SessionOpened|SessionOutput|SessionClosed",
            r"TerminalV1Args|ClaudePtyTranscriptV1Args|ClaudeSdkV1Args|CodexSdkV1Args",
            r"ProtocolNotExposed|AgentProtocol([^A-Za-z_]|$)",
            r"StructuredRow",
            r"SessionControl",
            # AgentRef is back on purpose: it names any agent by host and id,
            # a message's recipient and an agent's parent alike, not the old
            # duplicate reference.
            r"Client(CreateAgent|RenameAgent|DeleteAgent|SendMessage|SetAgentStatus|SubscribeSession|SendInput|PutArtifact|GetArtifact|Diff|ListRepositories)Request",
            r"ListHostsRequest|ListHostsResponse|ListAgentsRequest|ListAgentsResponse|rpc ListHosts|rpc ListAgents",
            r"SubscribeHosts|SubscribeAgentsRequest|SubscribeAgentsResponse|rpc SubscribeAgents|SubscribeAgentEvents",
            r"AgentSummary|AgentProgressEvent|SummaryTodoProgress|SummaryContextMeter|SummaryAttention|ContextMeterSource",
            r"inventory_revision",
            r"ClaudeDriver",
            r"ClaudeKind|CodexKind([^A-Za-z_]|$)|message AgentKind",
            r"TestAgentKind|TestEchoV1|TestAgentCreateConfig|test_agent[.]proto",
            r"crates/test-agent|-p test-agent|debug/test-agent|test_agent_bin|new test-agent",
            r"ArtifactRef([^A-Za-z_]|$)",
            r"ArtifactKind([^A-Za-z_]|$)",
            # DiffFile is back on purpose: Diff answers with one per changed
            # file, so the name alone no longer marks the old response.
            r"DiffResponse|BaseIdentity|PathBlob([^A-Za-z_]|$)",
            r"DebugProfile|DebugInstallation|DebugFormat|DebugRequest|DebugResponse|rpc Debug[(]",
            r"ClaudePtyTranscriptV1Input|ClaudeSdkV1Input|CodexSdkV1Input|ClaudeCyclePermissionMode|CodexSdkV1Command",
        ],
    ),
    Row(
        "remote terminal",
        "terminal bytes over the wire and remote raw attach",
        [
            r"TerminalV1Input|TerminalV1Output|TerminalV1ReplayQuery|message TerminalSize",
            r"SessionArgs::TerminalV1",
            r"subscribe_raw|attach_subscribed|attach_new_codex_terminal|attach_opens_chat",
            r"remote_attach_reports_agent_exit|a_remote_terminal_agent_opens_chat",
        ],
    ),
    Row(
        "client store",
        "the write-through commit protocol, the store worker and the recorder snapshots",
        [
            r"ExpectedHead",
            r"CommitOutcome|CommitResult",
            r"AttemptId",
            r"HeadState|MutationOracle",
            r"StoreWorker|store_worker",
            r"StoreMsg|StoreOp([^A-Za-z_]|$)",
            r"STORE_READ_SPAN|amux[.]store[.]read",
            r"RecorderSnapshot|replay_msgs|write_recorder_snapshot|msgs[.]jsonl",
            r"recorder_checkpoint|write_model_checkpoint",
        ],
    ),
    Row(
        "folds",
        "the fold crate, per-client feed entries, the phone deriving its own transcript rows",
        [
            r"(^|[^A-Za-z_:])fold::",
            r"crates/fold|-p fold([^A-Za-z_-]|$)|test-fold",
            r"ProviderFold",
            r"FeedEntry",
            r"transcriptRows[(]|TranscriptRows[.]swift|TranscriptRowsTests",
            r"ui_state::restored|restored::(claude|claude_sdk|codex)",
        ],
    ),
    Row(
        "bridge",
        "stored feeds and the slot machine",
        [
            r"StoredFeed|StoredRow",
            r"Slot::(Entry|Boundary)",
            r"FeedEntryDto",
        ],
    ),
    Row(
        "artifacts",
        "the artifacts crate, its index and pins, the ephemeral lifetime, the client artifact cache",
        [
            r"artifacts::|crates/artifacts|-p artifacts([^A-Za-z_-]|$)",
            r"EPHEMERAL_TTL",
            r"artifact_cache_mib|artifact_cache_dir",
            r"(^|[^A-Za-z_])INDEX_FILE|recover_artifacts",
            r"sweep_loaded|artifact_sweeper",
            r"PutArtifact|GetArtifact|put_artifact|get_artifact",
            r"agent_attach",
            r"ArtifactCorrupt",
            r"repeated string pin",
            r"amux[.]attachments",
            r"AttachmentIndex",
        ],
    ),
    Row(
        "agent-side RPCs",
        "SetAgentStatus, artifact puts by agents, parent envelopes, agent connectors",
        [
            r"SetAgentStatus|set_agent_status",
            r"put_artifact_by_agent",
            r"parent_envelope",
            r"ClientConnector|ConfigConnector",
        ],
    ),
    Row(
        "input rejections",
        "rejection items and the handed-to-the-process reply",
        [
            r"amux[.](claude[.]|claude_sdk[.])?input_result",
            r"input_result_row",
            r"message SendInputResponse [{][}]",
        ],
    ),
    Row(
        "agent messaging",
        "daemon-side message carriers, the readiness wait and rollback, recipient rows",
        [
            r"AMUX_AGENT_ID",
            r"amux[.]codex_message|amux[.]claude_sdk[.]message",
            r"DeliveryTarget|DeliveryLiveness|DeliveryError|deliver_envelope",
            r"MESSAGING_SOCKET_MIN_VERSION",
            r"CreateAgentRollback",
            r"codex_message_row|recipient row|writes_recipient_row",
        ],
    ),
    Row(
        "updates",
        "update marker files, UpdateStatus plumbing, update-required",
        [
            r"MarkerFileReporter",
            r"UpdateStatus",
            r"UpdateMarkerFiles|StatusReporters",
            r"update-available|update-required|update-dismissed",
            r"UpdateRequired|UPDATE_REQUIRED|update_required",
            r"SuspendReason::Update|SUSPEND_REASON_UPDATE",
        ],
    ),
    Row(
        "hooks",
        "hook routing through the daemon and hook environment forwarding",
        [
            r"HandleHook",
            r"HookEnvironment",
            r"handle_hook|send_hook_event",
            r"sync_messaging|MESSAGING_ENV_KEYS",
        ],
    ),
    Row(
        "codex",
        "the shared Codex app-server, its two planes and thread attachment",
        [
            r"DaemonMode|try_managed_daemon_start|app-server-control",
            r"CodexAttached|CodexRawPtyTarget|CodexRawPtyLease|CodexRawPtyPlan",
            r"CODEX_RAW_THREAD_NOT_READY",
            r"enum Plane|Plane::(Terminal|Structured)",
        ],
    ),
    Row(
        "debug",
        "row-ring replay and door recordings",
        [
            r"tui::replay|Replay::load|frame_diff",
            r"ios-replay|DoorRecording",
            r"replay_report([^s]|$)",
        ],
    ),
    Row(
        "terminal stand-ins",
        "the terminal's stand-in module and its gates, option lists written in clients, the terminal's tidying of model ids",
        [
            r"crates/tui/src/pending|(crate|super|tui)::pending::",
            r"attaches_elsewhere|offers_worktree|declines_questions|skips_questions|models_before_start|home_summary|home_order|diff_counts|wants_diff_stat",
            r"Held back outside",
            r"CLAUDE_EFFORTS|CODEX_EFFORTS|CODEX_MODES|CLAUDE_MODES|CODEX_PRESETS|CLAUDE_STOPS_ASKING",
        ],
    ),
    Row(
        "context strip",
        "the composer's strip, replaced by the overview",
        [
            r"pub struct Strip([^A-Za-z_]|$)|(^|[^A-Za-z_])in_strip|CONTEXT_STRIP_PERCENT",
        ],
    ),
    Row(
        "stretches",
        "the stretch query, a second way to fold tool steps",
        [
            r"stretch_at|stretch_steps|StretchCache|StretchCounts|struct Stretch([^A-Za-z_]|$)",
        ],
    ),
    Row(
        "plan recognition",
        "reading a plan-file write as the plan, and the approve and send-back answers",
        [
            r"is_plan_file|plan_file_row|waiting_plan",
            r"PlanApprove|PlanSendBack",
        ],
    ),
    Row(
        "provider messages by hand",
        "the Claude SDK module and wire messages the typed provider facts replaced",
        [
            r"claude::sdk",
            r"(^|[^A-Za-z_])SetApproval([^A-Za-z_]|$)",
            r"BackgroundProcesses|UsageLimits|message UsageWindow",
        ],
    ),
    Row(
        "the phone's own views",
        "the phone's outbox, session strip, fleet rows and run folding, and the row fields the shared views replaced",
        [
            # working_on lives on as the agent's own status line on the wire
            # and in its tools; only the phone's copy of it on the strip went.
            r"OutboxRow|OutboxState|outbox_rows|outboxRows",
            r"(amux_)?session_strip|sessionStrip",
            r"(^|[^A-Za-z_])(amux_)?fleet_rows",
            r"RunInfo|CollapseRuns|run_expanded",
            # What went is the inventory Agent's field and the phone's copy
            # of it. The store's agents column of the same name stays: it is
            # the newest snapshot's time, which retention orders own agents by.
            r"lastActivityMs",
            r"last_activity_ms *= *[0-9]",
            r"workingOn",
        ],
    ),
    Row(
        "the answer envelope",
        "the untyped answer body and its kind string, Codex's separate approval input, and the daemon-only arms (rename, dump) in a person's input",
        [
            r"AnswerInput",
            r"message Approve([^A-Za-z_]|$)",
            r"RenameThread",
            r"DumpInput",
        ],
    ),
    Row(
        "the remote key vocabulary",
        "named terminal keys a client could send; the permission-mode cycle is the one keystroke left",
        [
            # The agent's own keymap names keys too (claude::pty::keymap);
            # only the wire's vocabulary went.
            r"wire::KeyName|enum KeyName",
            r"KEY_NAME_",
            r"message Key([^A-Za-z_]|$)",
        ],
    ),
    Row(
        "loose create and resume requests",
        "a create's separate host id and host name, its config oneof beside a separate kind, and a whole input as the first prompt of a create or resume",
        [
            r"create_agent_request::Config",
            r"host_name: (Some|None)",
            r"NotAPrompt",
            r"PeerRef([^A-Za-z_]|$)",
            r"peer_ref::",
        ],
    ),
    Row(
        "requests the client and peer services shared",
        "a client's subscribe cursor and the dump answer that packed a peer's bundle beside a local path",
        [
            r"(^|[^_a-z])subscribe_request::",
            r"DumpResponse",
        ],
    ),
    Row(
        "the catalogue directory",
        "an agent's catalogues/ directory beside its blobs, the daemon's re-encoding to name or verify a catalogue, its separate replica copy, and GetCatalogue's agent form",
        [
            r"catalogues/",
            r"CATALOGUES([^A-Za-z_]|$)",
            r"write_catalogue",
            r"keep_replica_catalogue",
            r"catalogue_hash\(",
            r"WriteCatalogue",
            r"get_catalogue_request::",
            r"HostProvider",
        ],
    ),
    Row(
        "row fields nothing read",
        "fields written and carried but read by nothing",
        [
            r"GetBlobResponse.*blob",
            r"journal_offset",
            r"WorkingOn([^A-Za-z_]|$)",
            r"updated_at_ms",
            r"(row|agent|held)\.producer_version",
            r"hello_received",
            r"spec\.provider_version",
            r"config\.hooks",
            r"repeated string hooks",
            r"map<string, string> env",
            r"snapshot_revision",
        ],
    ),
    Row(
        "the in-memory store",
        "a second store implementation kept only for the store's own tests, and the row-primitive traits it shared with SQLite",
        [
            r"InMemory([^A-Za-z_]|$)",
            r"trait Tables([^A-Za-z_]|$)",
            r"trait Backend([^A-Za-z_]|$)",
            r"both_implementations_answer",
            r"SqlTables",
            r"NotificationBody",
        ],
    ),
    Row(
        "link reasons nothing sent",
        "link-close reasons and a relay state no producer has sent since the daemon rebuild",
        [
            r"LINK_CLOSE_REASON_UPDATING",
            r"LINK_CLOSE_REASON_RESTARTING",
            r"LinkCloseReason::(Updating|Restarting)",
            r"OBSERVED_STARTUP_FAILED",
            r"Observed::StartupFailed",
            r"RelayLink::Failed",
            r"The relay cannot be reached",
        ],
    ),
    Row(
        "terminal task items",
        "a terminal Claude task-notification item no interpreter emitted, and headless Claude's status compared as a word",
        [
            r"message TaskNotification",
            r"TaskNotification task",
            r"super::TaskNotification",
            r"Pty::Task\(",
            r"== \"compacting\"",
        ],
    ),
    Row(
        "refusal words",
        "a refusal carried as a word every producer spelled and every client parsed, with an open-ended arm for words it did not know",
        [
            r"refusal::ALL",
            r"closed_ask\"",
            r"not_queued\"",
            r"host_unreachable\"",
            r"RefusalReason::Other",
        ],
    ),
    Row(
        "kind and provider words",
        "kinds and providers named by strings each side mapped to and from its enum, and host features as strings",
        [
            r"kind_tag\(",
            r"kind_from_tag\(",
            r"kind_from_name\(",
            r"Provider::from_name",
            r"FEATURE_CLOUD_RELAY",
            r"amux\.cloud_relay",
            r"repeated string features",
        ],
    ),
    Row(
        "provider words for scopes and verdicts",
        "Claude's scope destinations and Codex's reviewer status carried only as the provider's word, and the scope choice's own copy of the grant",
        [
            r"string saved_to",
            r"saved_to: ::prost",
            r"ScopeChoice[^\n]*label",
            r"string decision = 1",
        ],
    ),
    Row(
        "the exit-cause words",
        "exit causes and removal reasons carried as the daemon's sentences, which only the daemon could write and clients matched word for word",
        [
            r"([^_]|^)CAUSE_(STOPPED|ABORTED|KILLED|EXITED|FINISHED|UNSTARTED|NO_DIRECTORY)",
            r"CAUSE_EXITED_AWAY",
            r"REMOVED_BY_RETENTION",
            r"([^_]|^)NO_LONGER_LISTED([^A-Za-z_]|$)",
            r"([^_A-Z]|^)NOT_TRUSTED([^A-Za-z_]|$)",
            r"optional string exit_cause",
        ],
    ),
    Row(
        "the front door's replay ids",
        "operation ids on front-door calls, minted fresh per call and read by nothing once the replay ledger behind them was gone",
        [
            r"operation_id",
            r"operationId",
            r"ProfileOperation([^A-Za-z_]|$)",
        ],
    ),
    Row(
        "the pairing wrappers",
        "pairing requests wrapped in a profile envelope, and demo pairing as a second TTL beside a mode",
        [
            r"ProfileStartPairingRequest",
            r"ProfileBeginPairRequest",
            r"ProfilePendingPairRequest",
            r"ProfileTrustSshPeerRequest",
            r"DemoPairing([^A-Za-z_]|$)",
            r"ProfilePairingStatusRequest",
            r"ProfileGetPeerRequest",
            r"ProfileUnpairRequest",
            r"start_pairing_request::Mode",
        ],
    ),
    Row(
        "the front door's empty requests and one-field answers",
        "request messages that carried nothing, answers that wrapped one message, the watch's sequence numbers, and the pairing route's own enum",
        [
            r"ListProfilesRequest",
            r"WatchProfilesRe(quest|sponse)",
            r"GetInfoRequest",
            r"InstallationShutdownRequest",
            r"DeleteProfileResponse",
            r"CancelPairingResponse",
            r"GetPeerResponse",
            r"([^A-Za-z]|^)UnpairResponse",
            r"([^A-Za-z]|^)ShutdownResponse",
            r"StartPairingResponse",
            r"PendingPairResponse",
            r"GetPairingStatusResponse",
            r"front_door_path",
            r"PeerVia",
            r"PEER_VIA_",
        ],
    ),
    Row(
        "docs",
        "the chapter on agent messaging and remote sessions",
        [
            r"Chapter 7: Agent messaging and remote sessions",
        ],
    ),
    Row(
        "names the protocol renamed",
        "per-provider and per-use copies of one vocabulary, and timestamp suffixes that said Unix twice",
        [
            r"AgentParent([^A-Za-z_]|$)",
            r"HostVia([^A-Za-z_]|$)",
            r"HOST_VIA_",
            r"ClaudeLimit([^A-Za-z_]|$)",
            r"CodexLimit([^A-Za-z_]|$)",
            r"CLAUDE_LIMIT_|CODEX_LIMIT_",
            r"_unix_ms",
        ],
    ),
    Row(
        "the opaque create config",
        "a create's kind beside its settings as bytes decoded by kind, the refusal when they did not decode, and the forwarding helper that passed the bytes on",
        [
            r"forwarded_create",
            r"config_must_match_kind",
            r"unknown_config_field_survives_forwarding",
            r"bytes config = [0-9]",
        ],
    ),
    Row(
        "the agent's farewell frame",
        "the exit cause sent as a last control frame, with the wait for a daemon to take it; the cause travels in the journal",
        [
            r"FAREWELL_PATIENCE",
            r"Of::Exiting",
            r"message Exiting",
            r"Exiting exiting = ",
        ],
    ),
    Row(
        "the wire's exit words",
        "exit causes worded once in the wire crate for every client; each client words the enum itself",
        [
            r"wire::exit_words",
        ],
    ),
    Row(
        "ask open and close times",
        "times on an ask's close, a permission decision, a plan's decision and a Codex ask that nothing read",
        [
            r"AskTimes",
            r"an_answered_ask_records_when_it_opened_and_closed",
            r"optional int64 (opened|closed|asked|decided)_at_ms",
        ],
    ),
    Row(
        "the interrupt-only tools socket",
        "a check that refused any input but an interrupt from an agent to its child",
        [
            r"tools_socket_input_other_than_interrupt_is_refused",
            r"the one input an agent sends is an interrupt",
        ],
    ),
    Row(
        "the tools socket's refusal of a person's calls",
        "a check that refused renaming, stopping, resuming, deleting, blobs, diffs, repository listings and dumps from an agent",
        [
            r"people_only",
            r"manages_no_other_agent",
        ],
    ),
]

# Files that name removed things on purpose, to hold them gone.
HOLDERS = [SELF]

# A hit that names a removed thing on purpose.
EXEMPTIONS = [
    Exemption(
        r"write_catalogue",
        "apps/apple/Tools/strings_lint_test.py",
        "these are the app's Xcode string catalogues, not an agent's",
    ),
    Exemption(
        r"artifact_cache_mib|artifact_cache_dir",
        "crates/settings/src/lib.rs",
        "the retired-key table names the old key so an old config fails with the reason",
    ),
]


def grep(pattern: str) -> list[str]:
    result = subprocess.run(
        ["git", "grep", "--untracked", "-I", "-n", "-E", "-e", pattern, "--",
         ".", ":!DEVLOG.md", *(f":!{path}" for path in HOLDERS)],
        cwd=ROOT, capture_output=True, text=True,
    )
    if result.returncode not in (0, 1):
        raise SystemExit(f"git grep failed on {pattern!r}: {result.stderr.strip()}")
    exempt = {e.path for e in EXEMPTIONS if e.pattern == pattern}
    return [line for line in result.stdout.splitlines()
            if line.split(":", 1)[0] not in exempt]


def workspace_drift() -> list[str]:
    members = set(tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["members"])
    crates = {f"crates/{p.name}" for p in (ROOT / "crates").iterdir()
              if (p / "Cargo.toml").exists()}
    problems = [f"{m} is a workspace member but has no Cargo.toml" for m in sorted(members - crates)]
    problems += [f"{c} is not a workspace member" for c in sorted(crates - members)]
    return problems


def main() -> int:
    failed = 0
    for row in ROWS:
        hits = [(pattern, grep(pattern)) for pattern in row.patterns]
        hits = [(pattern, lines) for pattern, lines in hits if lines]
        if not hits:
            print(f"ok    {row.name} ({len(row.patterns)} patterns)")
            continue
        count = sum(len(lines) for _, lines in hits)
        failed += count
        print(f"FAIL  {row.name}: {count} hits. Gone: {row.goes}.")
        for pattern, lines in hits:
            print(f"      /{pattern}/")
            for line in lines:
                print(f"        {line[:200]}")
    drift = workspace_drift()
    if drift:
        failed += len(drift)
        print("FAIL  workspace members")
        for problem in drift:
            print(f"        {problem}")
    else:
        print("ok    workspace members name only live crates")
    for exemption in EXEMPTIONS:
        print(f"note  {exemption.path} may name /{exemption.pattern}/: {exemption.why}")
    if failed:
        print(f"deletion ledger: {failed} survivors")
        return 1
    print("deletion ledger: nothing survives")
    return 0


if __name__ == "__main__":
    sys.exit(main())
