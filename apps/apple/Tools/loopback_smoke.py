#!/usr/bin/env python3
"""Pair a bare Swift executable on the iOS simulator with a served machine and read its fleet back."""

import json
import os
from pathlib import Path
import queue
import signal
import socket
import subprocess
import sys
import tempfile
import threading

from linkage_smoke import compile_swift, device_name, run, simulator

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts"))
import ios_bridge as bridge


TOPOLOGY = "journeys/topologies/phone-loopback.json"
MARKER = "runtime stopped"


def control(address: str, request: object) -> object:
    """One request to the served net's door, and what it answered."""
    host, port = address.rsplit(":", 1)
    with socket.create_connection((host, int(port)), timeout=60) as connection:
        connection.sendall((json.dumps(request) + "\n").encode())
        with connection.makefile("rb") as stream:
            reply = json.loads(stream.readline())
    if "ok" not in reply:
        raise RuntimeError(f"Runner refused {request}: {reply}")
    return reply["ok"]


def released(address: str) -> None:
    host, port = address.rsplit(":", 1)
    endpoint = (host, int(port))
    with socket.socket() as connection:
        connection.settimeout(2)
        if connection.connect_ex(endpoint) == 0:
            raise RuntimeError(f"Runner listener survived shutdown: {address}")
    with socket.socket() as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(endpoint)


def read_line(stream, what: str, timeout: float = 120) -> str:
    """One line from a child's output, or a named failure when none comes."""
    lines = queue.Queue()
    threading.Thread(target=lambda: lines.put(stream.readline()), daemon=True).start()
    try:
        line = lines.get(timeout=timeout)
    except queue.Empty as error:
        raise RuntimeError(f"{what} within {timeout:.0f} seconds") from error
    if not line:
        raise RuntimeError(f"{what}: the process ended first")
    return line


def read_ready(process: subprocess.Popen) -> dict:
    # The first start builds amux and the fake providers beside the runner.
    return json.loads(read_line(process.stdout, "Runner did not become ready", timeout=600))


def pairing_link(amux: Path, config: str, environment: dict) -> tuple[subprocess.Popen, str]:
    """The link a served machine prints when it opens pairing, the way a
    person asks for it: `amux pair --qr --print-link`."""
    process = subprocess.Popen(
        [str(amux), "--config", config, "pair", "--qr", "--print-link"],
        env=environment, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    while True:
        line = read_line(process.stdout, "the machine printed no pairing link", timeout=60)
        if line.startswith("Pairing link: "):
            return process, line.removeprefix("Pairing link: ").strip()


def validate_output(output: str, machine: str, agent: str) -> None:
    lines = output.splitlines()
    def one(prefix: str) -> object:
        found = [line.removeprefix(prefix) for line in lines if line.startswith(prefix)]
        if len(found) != 1:
            raise RuntimeError(f"Expected one {prefix} line: {output}")
        return json.loads(found[0])
    paired, host, agents = one("paired="), one("host="), one("agents=")
    if paired.get("name") != machine or host.get("name") != machine:
        raise RuntimeError(f"Paired with the wrong machine: {output}")
    if host.get("via") != "Direct":
        raise RuntimeError(f"{machine} was not reached over its direct link: {output}")
    if agent not in agents:
        raise RuntimeError(f"{agent} is missing from the fleet: {output}")
    if MARKER not in lines:
        raise RuntimeError(f"The runtime did not stop: {output}")


def round_trip(executable: Path, device: str) -> str:
    with tempfile.TemporaryDirectory(prefix="amux-lb-", dir="/tmp") as temporary:
        root = Path(temporary)
        environment = {key: value for key, value in os.environ.items()
                       if key not in ("AMUX_LOG", "AMUX_CONFIG")}
        environment |= {key: str(root) for key in ("TMPDIR", "TMP", "TEMP")}
        environment["TESTNET_BINARIES"] = str(bridge.TESTNET_BINARIES.resolve())
        runner = subprocess.Popen(
            [*bridge.TESTNET_SERVE, TOPOLOGY],
            env=environment, stdout=subprocess.PIPE, text=True)
        pairing = None
        try:
            ready = read_ready(runner)
            desk, = ready["hosts"]
            agent, = [agent["name"] for agent in ready["agents"]]
            pairing, link = pairing_link(Path("target/debug/amux").resolve(), desk["config"], environment)
            try:
                output = run("xcrun", "simctl", "spawn", device, str(executable), link, desk["name"], agent, timeout=90)
            except subprocess.CalledProcessError as error:
                raise RuntimeError(f"The Swift executable failed: {error.stderr}{error.stdout}") from error
            validate_output(output, desk["name"], agent)
            control(ready["control"], "Shutdown")
            if runner.wait(timeout=30) != 0:
                raise RuntimeError("Runner failed during shutdown")
            if runner.stdout.read():
                raise RuntimeError("Runner wrote unexpected stdout after readiness")
            released(ready["control"])
            return output + "\nRunner teardown verified: successful exit, control listener released\n"
        finally:
            for process in (pairing, runner):
                if process is not None and process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)
            runner.stdout.close()


def build_bridge(output: Path) -> None:
    """Builds and stages an independent copy of the app's driving bridge."""
    built = bridge.cargo_build(
        bridge.SIMULATOR_TRIPLE, profile=bridge.DRIVING_PROFILE,
        features=bridge.DRIVING_FEATURES,
        log=output / f"{bridge.SIMULATOR_TRIPLE}-build.jsonl")
    bridge.stage(built, output / bridge.SIMULATOR_TRIPLE)


def main() -> None:
    name = device_name()
    output = Path("target/ios/loopback").resolve()
    output.mkdir(parents=True, exist_ok=True)
    report = output.parent / "loopback-smoke.txt"
    report.unlink(missing_ok=True)
    subprocess.run([sys.executable, "-B", str(Path(__file__).with_name("test_loopback_smoke.py"))], check=True, timeout=15)
    # The same slice a development build links, staged on its own so this
    # smoke never depends on which framework the app last packaged.
    directory = output / bridge.SIMULATOR_TRIPLE
    build_bridge(output)
    executable = output / "app-ffi-loopback"
    compile_swift(directory, directory / "include", Path(__file__).with_name("LoopbackSmoke.swift"), executable)
    device, already_booted = simulator()
    try:
        if not already_booted:
            run("xcrun", "simctl", "boot", device)
        run("xcrun", "simctl", "bootstatus", device, "-b", timeout=180)
        text = f"{name}: iPhone 17 Pro, iOS 26.5 ({device})\n" + round_trip(executable, device)
    finally:
        if not already_booted:
            run("xcrun", "simctl", "shutdown", device)
    report.write_text(text)
    print(text, end="", flush=True)


if __name__ == "__main__":
    # Allow the recipe's timeout to unwind the runner and simulator ownership.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        main()
    except subprocess.CalledProcessError as error:
        if error.stdout:
            print(error.stdout, file=sys.stderr)
        if error.stderr:
            print(error.stderr, file=sys.stderr)
        raise
