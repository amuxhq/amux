#!/usr/bin/env python3
"""Run one Rust test binary's tests, each in a process of its own.

    test-runner.py [--threads N] [--slow S] [--terminate-after K] -- BINARY [FILTER...]

This is how Buck runs the workspace's tests (build/rust.bzl, `amux_test`),
and it holds them to what cargo-nextest holds them to under Cargo
(.config/nextest.toml):

- Every test runs in a process of its own, so no test shares a global, an
  environment variable or a once-built fixture with another. libtest alone
  would run them as threads of one process.
- A test is reported slow after S seconds and ended after K of those
  periods, with every process it started.
- A test that ends while a process it started still holds its output
  fails: that process outlives the test and competes with the next ones.
- Ignored tests are not run.

Each test also runs with AMUX_SOURCE_ROOT naming the checkout the test runs
in, where the source-tree crate finds the crate's files whatever working
directory a process the test starts is given, and with a HOME and TMPDIR of
its own, made empty for it and removed after. No test then reads the
machine's real home, which would be an input Buck can't see, and both are
short paths under /tmp: a test's daemon binds its sockets there, and a
Unix socket's path is at most 104 bytes on macOS, which a path inside the
build's sandbox can exceed.

The output is each test's result as it finishes, then every failure with
what it printed. The exit status is non-zero when any test failed.

Anything after the binary selects tests by name, and only those run: a
name containing FILTER, one starting with it when it starts with `^`
(`^system_journeys::`, a module), or exactly it after `--exact`:
`buck2 run //crates/amux:integration -- ^system_journeys::`. Two settings
come from the environment, for the recipes a person runs by hand:

- AMUX_TEST_LOCAL=1 (`just journey system`, `just live`) runs a test as a
  person running it would have it: with the real HOME and TMPDIR, so a live
  provider finds its sign-in, and with what it prints shown as it prints.
- AMUX_TEST_REPEAT=N (`just flaky`) runs the selected tests N times, one
  round after another, and stops at the round a test fails in.
"""

import argparse
import concurrent.futures
import errno
import os
import selectors
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time

LEAK_TIMEOUT = 1.0
WINDOWS = sys.platform == "win32"


def selected(name: str, filters: list[str]) -> bool:
    if not filters:
        return True
    if filters[0] == "--exact":
        return name in filters[1:]
    return any(name.startswith(f[1:]) if f.startswith("^") else f in name for f in filters)


def starting(start):
    """Start a process, again if the binary is busy. Linux refuses to run a
    file some process holds open for writing (ETXTBSY), and a test binary
    the worker has just written can be: a process another action forked
    while the file was open inherits the descriptor, until it execs. That
    lasts moments, so a few tries a moment apart get past it."""
    for attempt in range(20):
        try:
            return start()
        except OSError as error:
            if error.errno != errno.ETXTBSY or attempt == 19:
                raise
            time.sleep(0.05 * (attempt + 1))


def tests(binary: list[str], filters: list[str]) -> list[str]:
    def listing(*extra: str) -> list[str]:
        out = starting(lambda: subprocess.run(
            binary + ["--list", "--format", "terse", *extra],
            check=True,
            capture_output=True,
            text=True,
        )).stdout
        return [line[: -len(": test")] for line in out.splitlines() if line.endswith(": test")]

    ignored = set(listing("--ignored"))
    return [name for name in listing() if name not in ignored and selected(name, filters)]


def run_one(
    binary: list[str], name: str, slow: float, terminate_after: int, local: bool
) -> tuple[str, str, float, bytes]:
    start = time.monotonic()
    scratch = None
    env = dict(os.environ)
    if not local:
        # Resolved: macOS's /tmp is a link to /private/tmp, and a path a test
        # compares against a canonical one must be canonical already.
        # Windows has no /tmp; its temporary folder is short enough.
        scratch = tempfile.mkdtemp(prefix="t-", dir=tempfile.gettempdir() if WINDOWS else os.path.realpath("/tmp"))
        for sub in ("home", "tmp"):
            os.mkdir(os.path.join(scratch, sub))
        env.update(HOME=os.path.join(scratch, "home"), TMPDIR=os.path.join(scratch, "tmp"))
        if WINDOWS:
            # Where Windows programs look for the same two.
            env.update(USERPROFILE=env["HOME"], TEMP=env["TMPDIR"], TMP=env["TMPDIR"])
    child = starting(lambda: subprocess.Popen(
        binary + ["--exact", name, "--nocapture"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        start_new_session=not WINDOWS,
        env=env,
    ))
    output = bytearray()
    chunks = Chunks(child.stdout)
    deadline = start + slow * terminate_after
    reported_slow = False
    exited_at = None
    status = "pass"
    while True:
        now = time.monotonic()
        if child.poll() is not None and exited_at is None:
            exited_at = now
        if exited_at is not None and now - exited_at > LEAK_TIMEOUT:
            status = "leak"
            break
        if exited_at is None and now > deadline:
            status = "timeout"
            break
        if not reported_slow and now - start > slow:
            reported_slow = True
            print(f"SLOW  {name} ({slow:.0f}s)", flush=True)
        chunk = chunks.next(timeout=0.1)
        if chunk == b"":
            break
        if chunk:
            output += chunk
            if local:
                sys.stdout.buffer.write(chunk)
                sys.stdout.flush()
    chunks.close()
    end_tree(child)
    child.wait()
    # On Windows the reader thread may still be blocked reading a pipe that
    # an escaped process holds, and closing a handle under a blocked read
    # can wait for it; the handle goes with the runner.
    if not WINDOWS:
        child.stdout.close()
    if scratch:
        shutil.rmtree(scratch, ignore_errors=True)
    if status == "pass" and child.returncode != 0:
        status = "fail"
    return name, status, time.monotonic() - start, bytes(output)


class Chunks:
    """What a test writes, a chunk at a time, waiting at most a timeout for
    the next: None when nothing came, b"" once the pipe has closed, which
    is when every process holding it has ended. A selector does this on
    Unix; Windows can't select on a pipe, so a thread reads it there."""

    def __init__(self, pipe):
        self.pipe = pipe
        if WINDOWS:
            self.queue = []
            self.ready = threading.Condition()
            threading.Thread(target=self._read, daemon=True).start()
        else:
            self.selector = selectors.DefaultSelector()
            self.selector.register(pipe, selectors.EVENT_READ)

    def _read(self) -> None:
        while True:
            chunk = self.pipe.raw.read(65536) or b""
            with self.ready:
                self.queue.append(chunk)
                self.ready.notify()
            if not chunk:
                return

    def next(self, timeout: float) -> bytes | None:
        if not WINDOWS:
            if not self.selector.select(timeout=timeout):
                return None
            return os.read(self.pipe.fileno(), 65536)
        with self.ready:
            if not self.queue:
                self.ready.wait(timeout)
            return self.queue.pop(0) if self.queue else None

    def close(self) -> None:
        if not WINDOWS:
            self.selector.close()


def end_tree(child: subprocess.Popen) -> None:
    """Ends the test and whatever it left running."""
    if WINDOWS:
        # Every process the test started, by the parent each was started
        # from; a process whose parent has already ended is out of reach,
        # as one that left its group is on Unix.
        subprocess.run(["taskkill", "/T", "/F", "/PID", str(child.pid)], capture_output=True)
        return
    # With nothing left but the test itself, exited and not yet reaped,
    # macOS answers EPERM, not ESRCH.
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--slow", type=float, default=60)
    parser.add_argument("--terminate-after", type=int, default=3)
    parser.add_argument("binary", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.binary[1:] if args.binary[:1] == ["--"] else args.binary
    binary, filters = command[:1], command[1:]
    local = os.environ.get("AMUX_TEST_LOCAL") == "1"
    os.environ.setdefault("AMUX_SOURCE_ROOT", os.getcwd())
    # Buck names the artifacts a test is given (TESTNET_BINARIES,
    # CARGO_BIN_EXE_<name>) relative to the checkout, the runner's working
    # directory; a process a test starts elsewhere inherits them, so they
    # are made absolute here, once.
    for key, value in os.environ.items():
        if value.startswith("buck-out/"):
            os.environ[key] = os.path.abspath(value)

    names = tests(binary, filters)
    repeat = int(os.environ.get("AMUX_TEST_REPEAT", "1"))
    failures = []
    ran = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, args.threads)) as pool:
        for round in range(1, repeat + 1):
            if repeat > 1:
                print(f"round {round} of {repeat}", flush=True)
            running = [pool.submit(run_one, binary, name, args.slow, args.terminate_after, local) for name in names]
            for done in concurrent.futures.as_completed(running):
                name, status, seconds, output = done.result()
                ran += 1
                print(f"{status.upper():5} {name} ({seconds:.2f}s)", flush=True)
                if status != "pass":
                    failures.append((name, status, output))
            if failures:
                break

    for name, status, output in sorted(failures):
        why = {
            "fail": "failed",
            "timeout": f"ran past {args.slow * args.terminate_after:.0f}s and was ended",
            "leak": "ended while a process it started still held its output",
        }[status]
        print(f"\n---- {name}: {why} ----", flush=True)
        if not local:
            sys.stdout.buffer.write(output)
            sys.stdout.flush()
    print(f"\n{ran - len(failures)} passed, {len(failures)} failed", flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
