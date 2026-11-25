import importlib.util
from pathlib import Path
import sys
import tempfile
import textwrap
import threading
import time
import unittest
import warnings


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("test_runner", ROOT / "build" / "test-runner.py")
assert SPEC is not None and SPEC.loader is not None
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)

WINDOWS = sys.platform == "win32"

# A stand-in for a Rust test binary, run as `fake.py DIR --exact NAME
# --nocapture`, as the runner runs one test. Each test is a branch below.
FAKE = textwrap.dedent(
    """
    import os, subprocess, sys, time
    room, name = sys.argv[1], sys.argv[3]

    def wait_for(path):
        while not os.path.exists(path):
            time.sleep(0.02)

    if name == "passes":
        sys.exit(0)
    if name == "fails":
        sys.exit(1)
    if name == "in-the-runner":
        sys.exit(0 if os.environ.get("AMUX_TEST_RUNNER") == "1" else 1)
    if name == "orphan":
        # A child that starts a grandchild and exits at once, so the
        # grandchild's parent is gone: an orphan, which no walk of the
        # test's process tree reaches. It holds none of the test's output.
        spawn = (
            "import subprocess, sys; "
            "g = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(120)'], "
            "stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL); "
            "open(sys.argv[1], 'w').write(str(g.pid))"
        )
        subprocess.run([sys.executable, "-c", spawn, os.path.join(room, "pid.tmp")], check=True)
        os.replace(os.path.join(room, "pid.tmp"), os.path.join(room, "pid"))
        # The test ends only once the case has pinned the orphan.
        wait_for(os.path.join(room, "pinned"))
        sys.exit(0)
    if name == "breakaway":
        CREATE_BREAKAWAY_FROM_JOB = 0x01000000
        try:
            escaped = subprocess.Popen(
                [sys.executable, "-c", "pass"],
                creationflags=CREATE_BREAKAWAY_FROM_JOB,
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
        except PermissionError:
            print("breakaway refused")
            sys.exit(0)
        escaped.wait()
        print("broke away")
        sys.exit(1)
    sys.exit(f"no such fake test: {name}")
    """
)


class Pinned:
    """A process opened by pid, so its pid can't be reused while the case
    looks at it: Windows keeps a process's id while a handle to it is open."""

    SYNCHRONIZE = 0x00100000
    PROCESS_TERMINATE = 0x0001
    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000

    def __init__(self, pid: int):
        import ctypes
        from ctypes import wintypes

        self.ctypes = ctypes
        self.kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        self.kernel32.OpenProcess.restype = wintypes.HANDLE
        self.kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.kernel32.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        self.kernel32.WaitForSingleObject.restype = wintypes.DWORD
        self.kernel32.GetExitCodeProcess.argtypes = [wintypes.HANDLE, wintypes.LPDWORD]
        self.kernel32.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
        self.kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        self.handle = self.kernel32.OpenProcess(
            self.SYNCHRONIZE | self.PROCESS_TERMINATE | self.PROCESS_QUERY_LIMITED_INFORMATION, False, pid
        )
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())

    def ended_within(self, seconds: float) -> bool:
        return self.kernel32.WaitForSingleObject(self.handle, int(seconds * 1000)) == 0

    def exit_code(self) -> int:
        code = self.ctypes.c_ulong()
        self.kernel32.GetExitCodeProcess(self.handle, self.ctypes.byref(code))
        return code.value

    def close(self) -> None:
        # Never leave the orphan behind, whatever the case found.
        self.kernel32.TerminateProcess(self.handle, 9)
        self.kernel32.CloseHandle(self.handle)


class TestRunnerTest(unittest.TestCase):
    """run_one, the runner's run of one test, over a fake test binary."""

    def setUp(self):
        # On Windows the runner leaves each test's output pipe to close
        # with the runner (see run_one).
        warnings.simplefilter("ignore", ResourceWarning)
        self.room = tempfile.TemporaryDirectory()
        self.dir = Path(self.room.name)
        fake = self.dir / "fake.py"
        fake.write_text(FAKE)
        self.binary = [sys.executable, str(fake), str(self.dir)]

    def tearDown(self):
        self.room.cleanup()

    def run_test(self, name: str) -> tuple[str, bytes]:
        _, status, _, output = runner.run_one(self.binary, name, slow=60, terminate_after=1, local=False)
        return status, output

    def test_a_test_passes_or_fails_by_its_exit_status(self):
        self.assertEqual(self.run_test("passes")[0], "pass")
        self.assertEqual(self.run_test("fails")[0], "fail")

    def test_a_test_is_told_it_runs_in_the_runner(self):
        # What os::testing::require_test_runner looks for.
        self.assertEqual(self.run_test("in-the-runner")[0], "pass")

    @unittest.skipUnless(WINDOWS, "the job is how Windows contains a test")
    def test_an_orphan_a_test_leaves_dies_when_the_test_ends(self):
        ran = {}
        thread = threading.Thread(target=lambda: ran.update(result=self.run_test("orphan")))
        thread.start()
        pid_file = self.dir / "pid"
        deadline = time.monotonic() + 30
        while not pid_file.exists():
            self.assertLess(time.monotonic(), deadline, "the fake test never started its orphan")
            time.sleep(0.02)
        orphan = Pinned(int(pid_file.read_text()))
        try:
            (self.dir / "pinned").write_text("")
            thread.join(60)
            self.assertFalse(thread.is_alive(), "the runner never finished the test")
            self.assertEqual(ran["result"][0], "pass", ran["result"][1].decode(errors="replace"))
            self.assertTrue(orphan.ended_within(5), "the orphan outlived its test")
            # Ended by the runner (TerminateJobObject's exit code), not by
            # finishing its sleep.
            self.assertEqual(orphan.exit_code(), 1)
        finally:
            orphan.close()

    @unittest.skipUnless(WINDOWS, "the job is how Windows contains a test")
    def test_a_test_cannot_break_away_from_its_job(self):
        status, output = self.run_test("breakaway")
        self.assertIn(b"breakaway refused", output)
        self.assertEqual(status, "pass", output.decode(errors="replace"))


if __name__ == "__main__":
    unittest.main()
