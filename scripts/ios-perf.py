#!/usr/bin/env python3
"""Measure the iPhone app on this Mac's simulator: `just ios perf`.

The optimised `Measured` app, which keeps the driving door, runs against the
served performance network (`qualification::perf::phone`: three machines on
the phone's local network, forty agents, one conversation a thousand rows
long that streams on cue). Every packet between the phone and a machine
crosses a gate the run can delay, so reconciliation is measured over a
household network's latency as well as over none. Every number is taken by
the app itself and read back through the door; this script only arranges the
workload, keeps the samples, judges them against the budgets below and the
enrolled Mac's baseline, and writes the report.

docs/PERFORMANCE.md, "The phone", is the reference: the tables there are
checked against BUDGETS and MACHINES here by scripts/tests/ios_perf_test.py.

  --describe        say which machine this is and whether its baseline exists
  --only GROUP      one of cold, reconciliation, streaming, idle; repeatable
  --baseline        record this run's medians as the machine's baseline, after
                    every budget passes; needs a whole run
  --flat            draw every surface flat (the reduce-transparency setting) for
                    the whole run: a diagnostic, never a baseline, that says how
                    much of a number is the glass
"""

from __future__ import annotations

import argparse
import base64
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).parent))
import ios_simulators
from journeys.phone import PhoneJourney

ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / "target/ios/DerivedData/Build/Products/Measured-iphonesimulator/Amux.app"
TOPOLOGY_TOOL = ROOT / "target/debug/phone-topology"
OUTPUT = ROOT / "target/ios/perf"
BASELINES = ROOT / "perf/baselines/phone"
SIMULATOR = "golden"

# The Macs a run is allowed on, by the model `sysctl -n hw.model` prints.
# A machine not listed is refused; enrolling one is adding a reviewed row
# here and in docs/PERFORMANCE.md, then recording its baseline deliberately.
MACHINES = {"Mac14,6": "pinned-mac"}

SAMPLES = 5
GROUPS = ("cold", "reconciliation", "streaming", "idle")
HOSTS = ("desk", "laptop", "studio")
FLEET_AGENTS = 40
STREAM_AGENT = "stream"
# The relay account every machine signs in to, and the phone away from home.
ACCOUNT = "ada"
STREAM_SECONDS = 20.0
IDLE_SECONDS = 5.0
LATENCIES_MS = (0, 100)
# A machine admits ten QUIC handshakes a minute from one address (its
# guard against a flood), and every launch here is one handshake to each
# machine from the same address. Launches this far apart stay under it with
# the pairing launch counted; a person does not relaunch faster.
LAUNCH_SPACING_SECONDS = 10.0

# metric -> (unit, group, budget for the median, budget for the worst sample
# or None, drift tolerance in percent over the baseline median, or None for
# a metric held to its ceiling alone). A tolerance of 0 is a metric with no
# slack at all; the count metrics are exact. Hitch time rests at nothing
# between runs of the same build and shows a millisecond or two on others,
# so a share of its baseline would fail runs for noise: its budget is what
# it must meet.
# The cold first frame's budget is set from a profile of the launch on the
# pinned Mac: 300 ms loading images, of which 270 is the simulator's loader
# on the main thread (85 of it dyld_sim re-pointing the shared cache at the
# host, which a device does not do), 100 of UIKit building the scene, then
# SwiftUI building a navigation stack, a tab bar and a home for the first
# time; nothing of ours is among the heavy leaves. A device is budgeted
# apart, once measured.
BUDGETS: dict[str, tuple[str, str, float, float | None, int | None]] = {
    "cold first frame": ("ms", "cold", 650, 700, 15),
    # The store read is forty rows from SQLite and the node's own start;
    # the fleet render is SwiftUI building those rows once. Each is a
    # part of the first frame, and a part's budget cannot be looser than
    # what a person would feel in the whole.
    "cold store read": ("ms", "cold", 50, None, 15),
    "cold fleet render": ("ms", "cold", 100, None, 15),
    # On the same network the fleet should be current within a few
    # frames of the first one; a second is the slow-network figure.
    "reconciliation at 0 ms": ("ms", "reconciliation", 250, None, 15),
    "reconciliation at 100 ms": ("ms", "reconciliation", 1000, None, 15),
    # Away from home a phone reaches its machines through the relay, every
    # stream handshaking end to end inside; a person on mobile data accepts
    # about a second and a half to see a current fleet.
    "reconciliation through the relay at 100 ms": ("ms", "reconciliation", 1500, None, 15),
    "streaming hitch time": ("ms/s", "streaming", 5, None, None),
    "streaming main-thread CPU": ("%", "streaming", 60, None, 15),
    # What keeps the app resident when it is put away, rather than what
    # the system would allow it in front.
    "streaming footprint": ("MB", "streaming", 120, None, 10),
    "idle transcript commits": ("count", "idle", 0, 0, 0),
    "idle display ticks": ("count", "idle", 0, 0, 0),
}


class Refusal(Exception):
    """A run that must not happen, and why."""


def machine() -> tuple[str, str]:
    """This Mac's model and its enrolled name, or a refusal."""
    if platform.system() != "Darwin":
        raise Refusal("the phone is measured on a Mac's simulator")
    model = subprocess.run(
        ["sysctl", "-n", "hw.model"], check=True, capture_output=True, text=True, timeout=10
    ).stdout.strip()
    if model not in MACHINES:
        raise Refusal(
            f"this Mac is {model}, which is not enrolled; the enrolled machines are "
            + ", ".join(f"{name} ({m})" for m, name in MACHINES.items())
        )
    return model, MACHINES[model]


def median(values: list[float]) -> float:
    return statistics.median(values)


class Run:
    """One measured run: the served network, the phone, and the samples."""

    def __init__(self, journey: PhoneJourney, only: list[str] | None, flat: bool = False):
        self.journey = journey
        self.only = only
        # The app is measured as it ships, glass and all, unless a flat run
        # was asked for as a diagnostic.
        self.flat = flat
        journey.flat = flat
        self.samples: dict[str, list[float]] = {name: [] for name in BUDGETS}
        self.notes: list[str] = []
        self.agent_ids = {item["name"]: item["id"] for item in journey.ready["agents"]}
        self.gates: dict[str, str] = {}
        self.relay_gate: str | None = None
        self.last_launch = time.monotonic()

    def takes(self, group: str) -> bool:
        return not self.only or group in self.only

    # --- the network -------------------------------------------------------

    def gate_the_machines(self) -> None:
        """A gate in front of every machine's LAN listener, dialled in the
        machine's place, so the run can put latency between the phone and
        all of them."""
        for host in HOSTS:
            reply = self.journey.request({"LanGate": {"host": host}})
            assert isinstance(reply, dict)
            self.gates[host] = reply["addr"]

    def latency(self, ms: int) -> None:
        for host in HOSTS:
            self.journey.request({"LanFaults": {"host": host, "delay_ms": ms, "loss_percent": 0}})
        self.notes.append(f"latency {ms} ms each way on every gate")

    def gate_the_relay(self) -> None:
        """A gate in front of the served relay's QUIC carrier, dialled in the
        relay's place, so the run can put latency between the phone and the
        relay the way mobile data does."""
        reply = self.journey.request({"RelayGate": {}})
        assert isinstance(reply, dict)
        self.relay_gate = reply["addr"]

    def relay_latency(self, ms: int) -> None:
        self.journey.request({"RelayFaults": {"delay_ms": ms, "loss_percent": 0}})
        self.notes.append(f"latency {ms} ms each way on the relay's gate")

    def leave_home(self) -> None:
        """The phone away from home: signed in to the machines' account at
        the relay, with every direct dial lost, as the machines' home
        addresses are from mobile data."""
        self.journey.app({
            "kind": "connect", "relay": self.journey.ready["cloud_url"],
            "token": f"refresh-{ACCOUNT}", "user": ACCOUNT,
        })
        for host in HOSTS:
            self.journey.request({"LanFaults": {"host": host, "delay_ms": 0, "loss_percent": 100}})
        self.notes.append(f"signed in as {ACCOUNT} at the relay; every direct dial lost")

    def come_home(self) -> None:
        for host in HOSTS:
            self.journey.request({"LanFaults": {"host": host, "delay_ms": 0, "loss_percent": 0}})
        self.relay_latency(0)

    def pair_through_the_gates(self) -> None:
        """Pairs with every machine by the link it prints, with the link's
        addresses rewritten to the machine's gate, so the pairing and every
        link after it cross the gate."""
        for host in HOSTS:
            pairing, link = self.journey.pairing_link(host)
            self.journey.pair(gated_link(link, self.gates[host]))
            pairing.wait(timeout=30)
        for name in [f"agent-{at:02}" for at in range(FLEET_AGENTS)] + [STREAM_AGENT]:
            self.journey.app({"kind": "awaitAgent", "agent": name, "seconds": 120}, timeout=130)
        self.journey.app({"kind": "awaitReconciled", "seconds": 60})
        self.await_reconciled(60)

    # --- the app's marks ---------------------------------------------------

    def marks(self) -> dict[str, float]:
        """The first time each signpost was marked, in ms since the process
        started."""
        reply = self.journey.app({"kind": "signposts"})
        first: dict[str, float] = {}
        for mark in reply["marks"]:
            first.setdefault(mark["signpost"], mark["sinceProcessStart"] * 1000)
        return first

    def await_reconciled(self, seconds: float) -> dict[str, float]:
        deadline = time.monotonic() + seconds
        while True:
            marks = self.marks()
            if "reconciled" in marks:
                return marks
            if time.monotonic() > deadline:
                raise RuntimeError(f"the app never reconciled with its hosts; it marked {sorted(marks)}")
            time.sleep(0.1)

    # --- cold launches -----------------------------------------------------

    def cold_launches(self, latency_ms: int) -> None:
        """Five launches of the installed app with its state on disk and the
        machines up, the app terminated between them. Each launch marks its
        own first frame, store read and reconciliation; this reads them back."""
        self.latency(latency_ms)
        record_cold = latency_ms == 0 and self.takes("cold")
        record_reconciliation = self.takes("reconciliation")
        for attempt in range(SAMPLES):
            marks = self.marked_launch(attempt, f"at {latency_ms} ms")
            store_read = (marks["nodeStarted"] - marks["storeReadBegan"]) + (
                marks["storeReadEnded"] - marks["fleetOpenBegan"]
            )
            if record_cold:
                self.samples["cold first frame"].append(marks["firstCachedFrame"])
                self.samples["cold store read"].append(store_read)
                self.samples["cold fleet render"].append(marks["firstCachedFrame"] - marks["storeReadEnded"])
            if record_reconciliation:
                self.samples[f"reconciliation at {latency_ms} ms"].append(
                    marks["reconciled"] - marks["storeReadEnded"]
                )

    def relay_launches(self, latency_ms: int) -> None:
        """Five launches of the phone away from home, the relay's gate
        holding each packet `latency_ms`: the fleet is current only once
        every machine has been reached through the relay."""
        self.relay_latency(latency_ms)
        for attempt in range(SAMPLES):
            marks = self.marked_launch(attempt, f"through the relay at {latency_ms} ms", relay_quic=self.relay_gate)
            self.samples[f"reconciliation through the relay at {latency_ms} ms"].append(
                marks["reconciled"] - marks["storeReadEnded"]
            )

    def marked_launch(self, attempt: int, named: str, relay_quic: str | None = None) -> dict[str, float]:
        """One launch, terminated and spaced from the last, read back to its
        reconciliation, with every mark a cold launch needs."""
        self.journey.quit()
        self.space_launches()
        self.journey.relaunch(geometry=False, relay_quic=relay_quic)
        marks = self.await_reconciled(120)
        for needed in ("firstCachedFrame", "storeReadBegan", "nodeStarted", "fleetOpenBegan", "storeReadEnded"):
            if needed not in marks:
                raise RuntimeError(f"launch {attempt + 1} {named} never marked {needed}; it marked {sorted(marks)}")
        self.notes.append(
            f"launch {attempt + 1} {named}: images {marks.get('imagesLoaded', 0):.0f}, "
            f"entered {marks.get('appEntered', 0):.0f}, built {marks.get('compositionBuilt', 0):.0f}, "
            f"shell {marks.get('shellPresented', 0):.0f}, node {marks['storeReadBegan']:.0f}"
            f"-{marks['nodeStarted']:.0f}, fleet {marks['fleetOpenBegan']:.0f}-{marks['storeReadEnded']:.0f}, "
            f"first frame {marks['firstCachedFrame']:.0f}, reconciled {marks['reconciled']:.0f} ms"
        )
        print(self.notes[-1], flush=True)
        return marks

    def space_launches(self) -> None:
        since = time.monotonic() - self.last_launch
        if since < LAUNCH_SPACING_SECONDS:
            time.sleep(LAUNCH_SPACING_SECONDS - since)
        self.last_launch = time.monotonic()

    # --- the conversation on screen ---------------------------------------

    def open_the_conversation(self) -> None:
        if self.flat:
            self.notes.append("every surface drawn flat (reduce transparency)")
        agent = self.agent_ids[STREAM_AGENT]
        # Frames only while tapping: the app is measured as it ships.
        self.journey.geometry(True)
        self.journey.tab("Agents")
        self.journey.wait_for(f"home.row.{agent}")
        self.journey.tap(f"home.row.{agent}")
        self.journey.wait_for("chat.field")
        self.journey.geometry(False)
        deadline = time.monotonic() + 120
        while self.newest_order() is None:
            if time.monotonic() > deadline:
                raise RuntimeError("the conversation on screen never showed its rows")
            time.sleep(0.2)
        self.journey.app({"kind": "settle"})

    def newest_order(self) -> int | None:
        """The order of the newest row the chat on screen holds, or nothing
        while it holds none."""
        reading = self.journey.app({"kind": "conversation", "agent": STREAM_AGENT})["conversation"]
        orders = [row["order"] for row in reading["rows"]]
        return max(orders) if orders else None

    def measure(self, seconds: float) -> dict:
        return self.journey.app({"kind": "measure", "seconds": seconds}, timeout=seconds + 60)["measurement"]

    def idle(self) -> None:
        """Nothing arriving, nothing drawn: after a settle, the chat takes no
        rows and the app asks the display for nothing."""
        for attempt in range(SAMPLES):
            time.sleep(2.0)
            measured = self.measure(IDLE_SECONDS)
            self.samples["idle transcript commits"].append(measured["transcriptCommits"])
            self.samples["idle display ticks"].append(measured["idleTicks"])
            self.notes.append(
                f"idle {attempt + 1}: {measured['transcriptCommits']} commits, "
                f"{measured['idleTicks']} ticks over {IDLE_SECONDS:.0f} s"
            )
            print(self.notes[-1], flush=True)

    def streaming(self) -> None:
        """Fifty rows a second for twenty seconds into the conversation on
        screen, resting at its tail so every row is laid out as it lands;
        the display, the main thread and the footprint watched meanwhile."""
        for attempt in range(SAMPLES):
            before = self.newest_order() or 0
            self.journey.request({"OpenGate": {"name": f"stream-{attempt}"}})
            measured = self.measure(STREAM_SECONDS)
            # The burst outlasts the watch by a little; let it end so the
            # next sample starts from rest, then check the rows arrived.
            time.sleep(2.0)
            arrived = (self.newest_order() or 0) - before
            if arrived < 900:
                raise RuntimeError(
                    f"stream sample {attempt + 1}: only {arrived} rows reached the phone; "
                    "the number would be about an idle screen"
                )
            self.samples["streaming hitch time"].append(measured["hitchMsPerS"])
            self.samples["streaming main-thread CPU"].append(measured["mainThreadCpuPercent"])
            self.samples["streaming footprint"].append(measured["footprintMB"])
            self.notes.append(
                f"stream {attempt + 1}: {arrived} rows, {measured['frames']} frames, "
                f"hitch {measured['hitchMsPerS']:.2f} ms/s, CPU {measured['mainThreadCpuPercent']:.1f}%, "
                f"footprint {measured['footprintMB']:.1f} MB"
            )
            print(self.notes[-1], flush=True)

    # --- the whole run -----------------------------------------------------

    def measure_everything(self) -> None:
        self.gate_the_machines()
        self.journey.launch()
        self.pair_through_the_gates()
        # A launch the cold launches are compared against: the same install,
        # the fleet already in its store. Every reconciled launch below reads
        # the remembered fleet first, which is what a cold first frame shows.
        if self.takes("cold") or self.takes("reconciliation"):
            for latency in LATENCIES_MS:
                if latency != 0 and not self.takes("reconciliation"):
                    continue
                self.cold_launches(latency)
            self.latency(0)
        if self.takes("reconciliation"):
            self.gate_the_relay()
            self.leave_home()
            self.relay_launches(LATENCIES_MS[-1])
            self.come_home()
        if self.takes("idle") or self.takes("streaming"):
            self.journey.quit()
            self.space_launches()
            self.journey.relaunch(geometry=False)
            self.await_reconciled(120)
            self.open_the_conversation()
            if self.takes("idle"):
                self.idle()
            if self.takes("streaming"):
                self.streaming()


def gated_link(link: str, gate: str) -> str:
    """The pairing link with the gate's address in place of the machine's."""
    prefix, encoded = link.split("=", 1) if "=" in link else ("", link)
    padded = encoded + "=" * (-len(encoded) % 4)
    payload = json.loads(base64.urlsafe_b64decode(padded))
    payload["addrs"] = [gate]
    rewritten = base64.urlsafe_b64encode(json.dumps(payload, separators=(",", ":")).encode()).decode().rstrip("=")
    return f"{prefix}={rewritten}" if prefix else rewritten


# --- judging ---------------------------------------------------------------


def judge(samples: dict[str, list[float]], baseline: dict[str, float] | None) -> dict:
    """Every measured metric against its budgets and the baseline."""
    results = []
    for name, (unit, group, budget, worst_budget, tolerance) in BUDGETS.items():
        values = samples[name]
        if not values:
            continue
        med = median(values)
        worst = max(values)
        notes = []
        passed = True
        if med > budget:
            passed = False
            notes.append(f"median {med:.1f} {unit} over the budget of {budget:g}")
        if worst_budget is not None and worst > worst_budget:
            passed = False
            notes.append(f"worst {worst:.1f} {unit} over the worst-sample budget of {worst_budget:g}")
        recorded = baseline.get(name) if baseline else None
        drift = None
        if recorded is not None and tolerance is not None:
            allowed = recorded * (1 + tolerance / 100)
            drift = (med - recorded) / recorded * 100 if recorded else (0.0 if med == recorded else float("inf"))
            if med > allowed and med > recorded:
                passed = False
                notes.append(
                    f"median {med:.1f} {unit} is {drift:+.1f}% over the baseline of {recorded:.1f}, "
                    f"past the {tolerance}% tolerance"
                )
        results.append({
            "metric": name,
            "unit": unit,
            "group": group,
            "samples": values,
            "median": med,
            "worst": worst,
            "budget": budget,
            "worst_budget": worst_budget,
            "tolerance_percent": tolerance,
            "baseline": recorded,
            "drift_percent": drift,
            "passed": passed,
            "note": "; ".join(notes),
        })
    return {"passed": all(result["passed"] for result in results), "results": results}


def summary_line(result: dict) -> str:
    """One metric as the run prints it: the median and worst against the
    budget, the baseline and drift where there is one, and the verdict."""
    against = ""
    if result["baseline"] is not None:
        drift = (
            f"{result['drift_percent']:+.1f}%" if result["drift_percent"] is not None else "ceiling only"
        )
        against = f", baseline {result['baseline']:.1f} ({drift})"
    verdict = "" if result["passed"] else f" — FAILED: {result['note']}"
    return (
        f"{result['metric']}: median {result['median']:.1f} {result['unit']}, worst {result['worst']:.1f}, "
        f"budget {result['budget']:g}{against}{verdict}"
    )


def report(verdict: dict, machine_name: str, model: str, minutes: float, notes: list[str]) -> str:
    lines = [
        "# The phone measured",
        "",
        f"Machine `{machine_name}` ({model}), the `Measured` app on the `{SIMULATOR}` simulator, "
        f"{SAMPLES} samples per metric; the run took {minutes:.1f} minutes.",
        "",
        "The simulator reports 60 Hz and composites through the Mac's display: hitch time is display-link "
        "accounting, a proxy for a device's hitch metric.",
        "",
        "| Metric | Median | Worst | Budget | Baseline | Drift | Verdict |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ]
    for result in verdict["results"]:
        unit = result["unit"]
        budget = f"{result['budget']:g} {unit}"
        if result["worst_budget"] is not None:
            budget += f" (worst {result['worst_budget']:g})"
        baseline = f"{result['baseline']:.1f}" if result["baseline"] is not None else "none"
        if result["tolerance_percent"] is None:
            drift = "ceiling only"
        elif result["drift_percent"] is not None:
            drift = f"{result['drift_percent']:+.1f}%"
        else:
            drift = "unavailable"
        lines.append(
            f"| {result['metric']} | {result['median']:.1f} {unit} | {result['worst']:.1f} {unit} | {budget} | "
            f"{baseline} | {drift} | {'PASS' if result['passed'] else 'FAIL: ' + result['note']} |"
        )
    lines += ["", "## Samples", ""] + [f"- {note}" for note in notes] + [""]
    return "\n".join(lines)


def describe() -> None:
    model, name = machine()
    baseline = BASELINES / f"{name}.json"
    print(f"this Mac is {model}, enrolled as {name}")
    print(f"baseline {baseline}: {'present' if baseline.is_file() else 'missing'}")
    print(f"app: {APP} ({'built' if APP.is_dir() else 'not built'})")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--describe", action="store_true")
    parser.add_argument("--only", choices=GROUPS, action="append")
    parser.add_argument("--baseline", action="store_true")
    parser.add_argument("--flat", action="store_true")
    arguments = parser.parse_args()
    try:
        if arguments.describe:
            describe()
            return 0
        if arguments.baseline and arguments.only:
            raise Refusal("a baseline is a whole run; --baseline takes no --only")
        if arguments.baseline and arguments.flat:
            raise Refusal("a flat run is a diagnostic; --baseline records the app as it ships")
        model, name = machine()
        if not APP.is_dir():
            raise Refusal(f"{APP} is not built; `scripts/ios-build.py --configuration Measured` builds it")
        if not TOPOLOGY_TOOL.is_file():
            raise Refusal(f"{TOPOLOGY_TOOL} is not built; `just ios tools` builds it")
    except Refusal as refusal:
        print(f"ios-perf: {refusal}", file=sys.stderr)
        return 2

    if OUTPUT.exists():
        shutil.rmtree(OUTPUT)
    OUTPUT.mkdir(parents=True)
    topology = OUTPUT / "topology.json"
    subprocess.run([str(TOPOLOGY_TOOL), str(topology)], check=True, timeout=60, capture_output=True)
    baseline_file = BASELINES / f"{name}.json"
    baseline = json.loads(baseline_file.read_text())["medians"] if baseline_file.is_file() else None
    if baseline is None:
        print(f"no baseline for {name}: judged on budgets alone", flush=True)

    began = time.monotonic()
    udid = ios_simulators.ready(SIMULATOR)
    journey = PhoneJourney({"id": "perf"}, topology, udid, output=OUTPUT / "journey", app=APP)
    run = Run(journey, arguments.only, flat=arguments.flat)
    try:
        run.measure_everything()
        journey.finish([])
    except BaseException as error:
        journey.fail(error)
        # The served machines' own logs, which the driver's close discards,
        # are what says why a launch never reconciled.
        try:
            shutil.copytree(
                journey.scratch, OUTPUT / "served",
                ignore=shutil.ignore_patterns("sock", "*.sock", "*.sqlite*"), dirs_exist_ok=True)
        except shutil.Error:
            pass
        journey.close()
        raise
    journey.close()
    minutes = (time.monotonic() - began) / 60

    verdict = judge(run.samples, baseline)
    (OUTPUT / "samples.json").write_text(json.dumps(run.samples, indent=2) + "\n")
    (OUTPUT / "verdict.json").write_text(json.dumps(verdict, indent=2) + "\n")
    text = report(verdict, name, model, minutes, run.notes)
    (OUTPUT / "report.md").write_text(text)
    for result in verdict["results"]:
        print(summary_line(result), flush=True)
    print(f"the run took {minutes:.1f} minutes; {OUTPUT / 'report.md'}", flush=True)
    if not verdict["passed"]:
        print("ios-perf: over budget", file=sys.stderr)
        return 1
    if arguments.baseline:
        BASELINES.mkdir(parents=True, exist_ok=True)
        recorded = {
            "schema_version": 1,
            "machine_model": model,
            "configuration": "Measured",
            "simulator": ios_simulators.device_name(SIMULATOR),
            "medians": {result["metric"]: result["median"] for result in verdict["results"]},
        }
        baseline_file.write_text(json.dumps(recorded, indent=2) + "\n")
        print(f"recorded {baseline_file}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
