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
- Whatever a test started ends with it. On Unix the test leads a session
  of its own, and its process group is killed; a process that left the
  group escapes. On Windows the test starts suspended, joins a Job Object
  of its own (kill on close, no breakaway) and only then runs, so every
  process it starts, and every one those start, is born inside the job,
  orphans too, and none can leave it; the job is ended with the test, and
  if the runner dies its handle closes and the job ends anyway. A start
  that asks to break away is refused (`os::process::spawn_background`
  then starts amux's agents inside the job, as under test they should).
- Ignored tests are not run.

Each test also runs with AMUX_TEST_RUNNER=1, which tells test support that
starts amux processes it runs here (on Windows it refuses to run without
it: run by hand, those processes open windows on the desktop), with
AMUX_SOURCE_ROOT naming the checkout the test runs in, where the source-tree crate finds the crate's files whatever working
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
# os::testing::TEST_RUNNER_ENV: set for every test this runs.
TEST_RUNNER_ENV = "AMUX_TEST_RUNNER"
WINDOWS = sys.platform == "win32"
# How long an ended job's processes have to be gone.
JOB_END_PATIENCE = 10.0


class Job:
    """A Windows Job Object of the runner's own: every process in it is
    ended when the job is ended or its last handle closes, and none may
    leave it (no BREAKAWAY_OK, so a start asking for
    CREATE_BREAKAWAY_FROM_JOB is refused with ERROR_ACCESS_DENIED). The
    handle isn't inheritable, so no test holds its own job open.

    Python's standard library has no job API; this is kernel32's, through
    ctypes."""

    KILL_ON_JOB_CLOSE = 0x2000
    EXTENDED_LIMIT_INFORMATION = 9
    BASIC_ACCOUNTING_INFORMATION = 1
    PROCESS_TERMINATE = 0x0001
    PROCESS_SET_QUOTA = 0x0100

    def __init__(self) -> None:
        api = win32()
        handle = api.kernel32.CreateJobObjectW(None, None)
        if not handle:
            raise api.error()
        self.handle = handle
        info = api.ExtendedLimits()
        info.BasicLimitInformation.LimitFlags = Job.KILL_ON_JOB_CLOSE
        if not api.kernel32.SetInformationJobObject(
            handle, Job.EXTENDED_LIMIT_INFORMATION, api.ctypes.byref(info), api.ctypes.sizeof(info)
        ):
            error = api.error()
            self.close()
            raise error

    def assign(self, pid: int) -> None:
        api = win32()
        process = api.kernel32.OpenProcess(Job.PROCESS_SET_QUOTA | Job.PROCESS_TERMINATE, False, pid)
        if not process:
            raise api.error()
        try:
            if not api.kernel32.AssignProcessToJobObject(self.handle, process):
                raise api.error()
        finally:
            api.kernel32.CloseHandle(process)

    def active(self) -> int:
        """How many processes the job still holds."""
        api = win32()
        accounting = api.Accounting()
        if not api.kernel32.QueryInformationJobObject(
            self.handle,
            Job.BASIC_ACCOUNTING_INFORMATION,
            api.ctypes.byref(accounting),
            api.ctypes.sizeof(accounting),
            None,
        ):
            raise api.error()
        return accounting.ActiveProcesses

    def end(self) -> None:
        """Ends every process in the job and waits, briefly, for them to be
        gone, so the test's scratch directory can be removed after."""
        win32().kernel32.TerminateJobObject(self.handle, 1)
        deadline = time.monotonic() + JOB_END_PATIENCE
        try:
            while self.active() and time.monotonic() < deadline:
                time.sleep(0.02)
        except OSError:
            pass

    def close(self) -> None:
        if self.handle:
            win32().kernel32.CloseHandle(self.handle)
            self.handle = None


_WIN32 = None


def win32():
    """kernel32 and ntdll as Job and resume call them, declared once."""
    global _WIN32
    if _WIN32 is not None:
        return _WIN32
    import ctypes
    from ctypes import wintypes

    class IoCounters(ctypes.Structure):
        _fields_ = [
            (name, ctypes.c_ulonglong)
            for name in (
                "ReadOperationCount",
                "WriteOperationCount",
                "OtherOperationCount",
                "ReadTransferCount",
                "WriteTransferCount",
                "OtherTransferCount",
            )
        ]

    class BasicLimits(ctypes.Structure):
        _fields_ = [
            ("PerProcessUserTimeLimit", ctypes.c_longlong),
            ("PerJobUserTimeLimit", ctypes.c_longlong),
            ("LimitFlags", wintypes.DWORD),
            ("MinimumWorkingSetSize", ctypes.c_size_t),
            ("MaximumWorkingSetSize", ctypes.c_size_t),
            ("ActiveProcessLimit", wintypes.DWORD),
            ("Affinity", ctypes.c_size_t),
            ("PriorityClass", wintypes.DWORD),
            ("SchedulingClass", wintypes.DWORD),
        ]

    class ExtendedLimits(ctypes.Structure):
        _fields_ = [
            ("BasicLimitInformation", BasicLimits),
            ("IoInfo", IoCounters),
            ("ProcessMemoryLimit", ctypes.c_size_t),
            ("JobMemoryLimit", ctypes.c_size_t),
            ("PeakProcessMemoryUsed", ctypes.c_size_t),
            ("PeakJobMemoryUsed", ctypes.c_size_t),
        ]

    class Accounting(ctypes.Structure):
        _fields_ = [
            ("TotalUserTime", ctypes.c_longlong),
            ("TotalKernelTime", ctypes.c_longlong),
            ("ThisPeriodTotalUserTime", ctypes.c_longlong),
            ("ThisPeriodTotalKernelTime", ctypes.c_longlong),
            ("TotalPageFaultCount", wintypes.DWORD),
            ("TotalProcesses", wintypes.DWORD),
            ("ActiveProcesses", wintypes.DWORD),
            ("TotalTerminatedProcesses", wintypes.DWORD),
        ]

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    ntdll = ctypes.WinDLL("ntdll")
    # Handles are pointers: undeclared, ctypes would cut them to 32 bits.
    for name, restype, argtypes in (
        ("CreateJobObjectW", wintypes.HANDLE, [wintypes.LPVOID, wintypes.LPCWSTR]),
        ("SetInformationJobObject", wintypes.BOOL, [wintypes.HANDLE, ctypes.c_int, wintypes.LPVOID, wintypes.DWORD]),
        (
            "QueryInformationJobObject",
            wintypes.BOOL,
            [wintypes.HANDLE, ctypes.c_int, wintypes.LPVOID, wintypes.DWORD, wintypes.LPDWORD],
        ),
        ("AssignProcessToJobObject", wintypes.BOOL, [wintypes.HANDLE, wintypes.HANDLE]),
        ("TerminateJobObject", wintypes.BOOL, [wintypes.HANDLE, wintypes.UINT]),
        ("OpenProcess", wintypes.HANDLE, [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]),
        ("CloseHandle", wintypes.BOOL, [wintypes.HANDLE]),
    ):
        function = getattr(kernel32, name)
        function.restype, function.argtypes = restype, argtypes
    ntdll.NtResumeProcess.restype = ctypes.c_long
    ntdll.NtResumeProcess.argtypes = [wintypes.HANDLE]

    class Api:
        pass

    api = Api()
    api.ctypes = ctypes
    api.kernel32 = kernel32
    api.ntdll = ntdll
    api.ExtendedLimits = ExtendedLimits
    api.Accounting = Accounting
    api.error = lambda: ctypes.WinError(ctypes.get_last_error())
    _WIN32 = api
    return api


def resume(pid: int) -> None:
    """Lets a process started suspended run."""
    api = win32()
    process_suspend_resume = 0x0800
    process = api.kernel32.OpenProcess(process_suspend_resume, False, pid)
    if not process:
        raise api.error()
    try:
        status = api.ntdll.NtResumeProcess(process)
        if status < 0:
            raise OSError(f"NtResumeProcess: NTSTATUS {status & 0xFFFFFFFF:#010x}")
    finally:
        api.kernel32.CloseHandle(process)


def start_contained(argv: list[str], **popen) -> tuple[subprocess.Popen, Job | None]:
    """Starts a test with everything it will start contained (see the
    module's docstring): on Unix in a session of its own, on Windows in a
    job of its own, joined while the test is suspended, before it runs at
    all. Where the job can't be made, the test runs without one, as before
    there was one, and the runner says so."""
    if not WINDOWS:
        return subprocess.Popen(argv, start_new_session=True, **popen), None
    create_suspended = 0x00000004
    child = subprocess.Popen(argv, creationflags=create_suspended, **popen)
    job = None
    try:
        job = Job()
        job.assign(child.pid)
    except OSError as error:
        if job is not None:
            job.close()
            job = None
        print(f"test-runner: {argv[0]} runs in no job of its own ({error})", file=sys.stderr, flush=True)
    try:
        resume(child.pid)
    except OSError:
        child.kill()
        child.wait()
        if job is not None:
            job.close()
        raise
    return child, job


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
    env[TEST_RUNNER_ENV] = "1"
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
    child, job = starting(lambda: start_contained(
        binary + ["--exact", name, "--nocapture"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
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
    end_tree(child, job)
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


def end_tree(child: subprocess.Popen, job: Job | None) -> None:
    """Ends the test and whatever it left running."""
    if job is not None:
        # Every process the test started, orphans too, and every one
        # those started: none could leave the job.
        job.end()
        job.close()
        return
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
