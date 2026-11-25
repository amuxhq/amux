"""What build/toolchains/bootstrap-guard.py does with the prelude's Python
tools: runs them as the interpreter would, and fails a compile whose
failure-filter build status records that rustc failed. A stand-in for the
prelude's rustc_action.py writes the build status it is told to."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

GUARD = Path(__file__).resolve().parents[2] / "build/toolchains/bootstrap-guard.py"

# Writes {"status": STATUS} to the --failure-filter file, if it was given
# one, and exits 0, as rustc_action.py does when rustc reported an error.
RUSTC_ACTION = """
import argparse, json, sys
parser = argparse.ArgumentParser(fromfile_prefix_chars="@")
parser.add_argument("--failure-filter")
parser.add_argument("--status", type=int, default=0)
parser.add_argument("rest", nargs=argparse.REMAINDER)
args = parser.parse_args()
if args.failure_filter:
    with open(args.failure_filter, "w") as file:
        json.dump({"status": args.status, "files": []}, file)
print("ran", *args.rest, file=sys.stderr)
"""


class BootstrapGuardTest(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="bootstrap-guard-"))
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.dir)])
        self.status = self.dir / "build_status.json"

    def run_script(self, name: str, source: str, *args: str) -> subprocess.CompletedProcess:
        script = self.dir / name
        script.write_text(source)
        return subprocess.run([sys.executable, "-B", str(GUARD), str(script), *args], capture_output=True, text=True, timeout=60)

    def test_a_compile_whose_rustc_failed_fails(self):
        ran = self.run_script("rustc_action.py", RUSTC_ACTION, f"--failure-filter={self.status}", "--status", "1", "rustc")
        self.assertEqual(ran.returncode, 1, ran.stderr)
        self.assertIn("ran rustc", ran.stderr)
        self.assertIn("rustc exited with status 1", ran.stderr)

    def test_a_compile_whose_rustc_succeeded_succeeds(self):
        ran = self.run_script("rustc_action.py", RUSTC_ACTION, f"--failure-filter={self.status}", "rustc")
        self.assertEqual(ran.returncode, 0, ran.stderr)
        self.assertEqual(json.loads(self.status.read_text())["status"], 0)

    def test_the_build_status_is_found_in_an_argument_file(self):
        argfile = self.dir / "rustc.args"
        argfile.write_text(f"--failure-filter={self.status}\n--status\n101\nrustc\n")
        ran = self.run_script("rustc_action.py", RUSTC_ACTION, f"@{argfile}")
        self.assertEqual(ran.returncode, 1, ran.stderr)
        self.assertIn("status 101", ran.stderr)

    def test_a_compile_without_the_failure_filter_is_left_alone(self):
        ran = self.run_script("rustc_action.py", RUSTC_ACTION, "rustc")
        self.assertEqual(ran.returncode, 0, ran.stderr)

    def test_another_tool_runs_as_under_the_interpreter(self):
        (self.dir / "helper.py").write_text("NAME = 'helper'\n")
        source = "import sys, helper\nprint(helper.NAME, sys.argv[1:], __name__)\nsys.exit(3)\n"
        ran = self.run_script("tool.py", source, "a", "--failure-filter=x")
        self.assertEqual(ran.returncode, 3)
        self.assertEqual(ran.stdout.strip(), "helper ['a', '--failure-filter=x'] __main__")


if __name__ == "__main__":
    unittest.main()
