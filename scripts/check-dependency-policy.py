#!/usr/bin/env python3
"""Enforce the local dependency edges that preserve workspace boundaries."""

import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
ALLOWED_LOCAL = {
    "wire": set(),
    # The plain values the views and the phone bridge share; the Swift
    # mirrors are generated from them, so they depend on nothing.
    "model": set(),
    "settings": set(),
    # The typed events a device may send and their uploader: a leaf, so the
    # daemon, the phone bridge and the CLI share one definition.
    "analytics": set(),
    "attachments": {"wire"},
    "journal": {"wire"},
    "store": {"wire"},
    "agent-dir": {"wire"},
    "redaction": set(),
    # Reads an agent folder's branch and change totals by running git; the
    # agent process and the daemon share it.
    "git-facts": set(),
    # The pure per-kind step, and the per-kind body redactor beside it.
    "interpret": {"attachments", "claude-protocol", "codex-protocol", "redaction", "wire"},
    # The agent process: its lock, journal and sockets, the interpreter,
    # and the provider hosts.
    "agent": {
        "agent-dir", "attachments", "claude", "claude-protocol", "codex", "codex-protocol",
        "git-facts", "interpret", "journal", "pty-host", "wire",
    },
    "claude": {"claude-protocol", "pty-host"},
    "codex": {"codex-protocol"},
    # Each provider's messages as types: pure, so the interpreter the phone
    # links can read them. PURE_EXTERNAL below bounds what they may pull in.
    "claude-protocol": set(),
    "codex-protocol": set(),
    "pty-host": set(),
    # The daemon reaches agent processes only through the directory contract,
    # never the agent crate, so the phone can host a runtime without a
    # provider in its graph.
    # interpret only for the dump path's per-kind redactor: the one stated
    # exception to "the daemon interprets nothing".
    "node": {"agent-dir", "analytics", "git-facts", "interpret", "journal", "release", "settings", "store", "version-stamp", "wire"},
    "amux": {"agent", "agent-dir", "analytics", "claude", "client", "node", "settings", "store", "tui", "wire"},
    # The version stamp a release tool can rewrite in a built binary, and the
    # release manifest the tool signs and the daemon verifies; the xtask
    # shares both without building the daemon.
    "version-stamp": set(),
    "release": set(),
    # The Swift mirrors of the view values are generated from their
    # definitions, so the generator reads them.
    "xtask": {"app-runtime", "model", "release", "ui-view", "version-stamp", "wire"},
    # The seam both clients call the local runtime through: the local socket
    # and the shared clock trait come from the agent directory contract.
    "client": {"agent-dir", "wire"},
    "ui-state": {"model", "wire"},
    "ui-view": {"attachments", "ui-state", "wire"},
    "ui-runtime": {"client", "ui-state", "ui-view", "wire"},
    # The phone's chats and fleet over the local runtime, with no node.
    "app-runtime": {"client", "model", "ui-runtime", "ui-state", "ui-view", "wire"},
    # The daemon's profile runtime hosted in the phone's process.
    "app-embedded": {"analytics", "app-runtime", "client", "node", "wire"},
    # The C ABI over both.
    "app-ffi": {"app-embedded", "app-runtime", "client", "model", "node", "ui-view"},
    # The terminal client composes the views over the drivers' state.
    "tui": {"attachments", "client", "ui-runtime", "ui-state", "ui-view", "wire"},
    # Replays a dump bundle's three pure stages: facts through the
    # interpreter (the journal for comparison), records through the session
    # model, state through the views; and provider recordings for the
    # capture tools.
    "replay-support": {
        "interpret", "journal", "redaction", "ui-state", "ui-view", "wire",
    },
}
# The UI library and the clients built on it reach the daemon only through
# the client seam: none of them may link, directly or through another crate,
# the daemon, its store, the interpreters or a provider. The phone bridge
# (app-ffi, app-embedded) hosts the runtime in process and is not one of them.
UI_CRATES = {"model", "client", "ui-state", "ui-view", "ui-runtime", "tui", "app-runtime"}
FORBIDDEN_FOR_UI = {
    "node", "store", "interpret", "agent", "claude", "claude-protocol", "codex", "codex-protocol",
    "pty-host",
}
# The protocol crates hold types and nothing else: no process, socket or
# async runtime code may come in through a dependency.
PURE_EXTERNAL = {
    "claude-protocol": {"serde", "serde_json"},
    "codex-protocol": {"serde", "serde_json"},
}
TEST_SUPPORT = {
    "patience",
    "testnet",
    "qualification",
    "claude-specs",
    "codex-specs",
    "provider-fakes",
    "fake-amux",
    "shot",
    "tui-set",
    "models",
}
SUPPORT_ALLOWED_LOCAL = {
    # The many-daemons harness: real daemons in process, real agents on the
    # fake providers, synthetic journals, and the production boundaries.
    # The one wait and its failure, shared by every test crate; the harness
    # builds its stream and cursor waits on it.
    "patience": set(),
    # The harness also signs release manifests with the test key for its
    # supervised hosts, and draws the terminal client's screens through shot.
    "testnet": {
        "agent-dir", "journal", "node", "patience", "provider-fakes", "release", "shot",
        "store", "tui", "wire",
    },
    # Qualification owns environment-dependent provider and performance
    # checks while reusing the network harness rather than shipping it.
    "qualification": {"node", "provider-fakes", "store", "testnet", "wire"},
    "claude-specs": {"claude", "claude-protocol", "pty-host", "redaction", "replay-support"},
    "codex-specs": {"codex", "codex-protocol", "redaction", "replay-support"},
    # The fake providers speak each protocol from its recordings, and check
    # what they compose against the protocol crates' types. They never use
    # the host crates, so a host bug cannot hide behind shared host code.
    "provider-fakes": {"claude-protocol", "codex-protocol", "pty-host"},
    # A stand-in amux binary that runs the real supervisor, so the
    # supervisor tests exec and roll back the shipped code.
    "fake-amux": {"agent-dir", "node"},
    # Renders the terminal client's named states to PNG for review.
    "shot": {"tui"},
    # Serves a declared world with `testnet serve` and runs the terminal
    # client on it, or draws it headlessly through `shot`.
    "tui-set": {"agent-dir", "client", "settings", "shot", "tui", "ui-runtime", "wire"},
    # The protocol's properties, explored over the real code: the store, the
    # journal and the message types today, and each state-machine core as it
    # lands.
    "models": {"journal", "store", "wire"},
}

def local_edges(package: dict[str, object]) -> set[str]:
    return {
        dependency["name"]
        for dependency in package["dependencies"]
        if dependency.get("path") is not None and dependency["kind"] != "dev"
    }


def main() -> int:
    metadata = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"],
            cwd=ROOT,
            text=True,
        )
    )
    members = set(metadata["workspace_members"])
    packages = {
        package["name"]: package
        for package in metadata["packages"]
        if package["id"] in members
    }
    failures = []

    for name, allowed in (ALLOWED_LOCAL | SUPPORT_ALLOWED_LOCAL).items():
        package = packages.get(name)
        if package is None:
            failures.append(f"missing required workspace package {name}")
        elif (actual := local_edges(package)) != allowed:
            failures.append(
                f"{name}: local dependencies are {sorted(actual)}, expected {sorted(allowed)}"
            )

    for name, allowed in PURE_EXTERNAL.items():
        package = packages.get(name)
        if package is None:
            failures.append(f"missing required workspace package {name}")
            continue
        normal = {
            dependency["name"]
            for dependency in package["dependencies"]
            if dependency["kind"] is None
        }
        if extra := normal - allowed:
            failures.append(f"{name}: a protocol crate depends on {sorted(extra)}")

    for name, package in packages.items():
        if name not in TEST_SUPPORT and (edges := local_edges(package) & TEST_SUPPORT):
            failures.append(f"{name}: production edges reach test support: {sorted(edges)}")

    edges = {name: local_edges(package) for name, package in packages.items()}
    for name in sorted(UI_CRATES & edges.keys()):
        reached, frontier = set(), [name]
        while frontier:
            for edge in edges.get(frontier.pop(), set()):
                if edge not in reached:
                    reached.add(edge)
                    frontier.append(edge)
        if forbidden := reached & FORBIDDEN_FOR_UI:
            failures.append(f"{name}: a UI crate reaches {sorted(forbidden)}")

    unlisted = packages.keys() - (ALLOWED_LOCAL | SUPPORT_ALLOWED_LOCAL).keys()
    for name in sorted(unlisted):
        failures.append(f"{name}: a workspace package with no entry in the policy")

    if failures:
        print("dependency policy failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print("dependency policy passed for production and support dependency edges")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
