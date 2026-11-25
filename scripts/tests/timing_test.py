"""The summary of where a Buck2 command's time went (build/checks/timing.py):
which kind each action is, which commands count as having run and where,
and that the two places are reported apart. The details are shaped as
`buck2 log show` prints them."""

import contextlib
import importlib.util
import io
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


def load(path: Path):
    spec = importlib.util.spec_from_file_location(path.stem, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


timing = load(ROOT / "build/checks/timing.py")


def details(command: dict, queue_us=None, exec_us=3_000_000) -> dict:
    return {
        "command_kind": {"command": command},
        "metadata": {
            "start_time": [1795609396, 724124000],
            "queue_duration_us": queue_us,
            "input_materialization_duration_us": 800_000,
            "execution_time_us": exec_us,
        },
    }


class KindTest(unittest.TestCase):
    def test_each_rustc_emit_has_its_kind(self):
        for identifier, kind in {
            "rlib [pic]": "compile",
            "cdylib [pic]": "compile",
            "link [pic]": "link",
            "metadata [pic]": "metadata",
            "check": "metadata",
            "diag": "metadata",
            "proc-macro": "other",
        }.items():
            self.assertEqual(timing.kind_of("rustc", identifier), kind, identifier)

    def test_clippy_is_its_own_kind(self):
        self.assertEqual(timing.kind_of("clippy", ""), "clippy")

    def test_anything_else_is_other(self):
        self.assertEqual(timing.kind_of("c_compile", "ring.c (pic)"), "other")


class RanTest(unittest.TestCase):
    def test_a_cache_hit_did_not_run(self):
        self.assertIsNone(timing.ran(details({"RemoteCommand": {"cache_hit": True}})))

    def test_a_command_on_the_worker_ran_remotely(self):
        ran = timing.ran(details({"RemoteCommand": {"cache_hit": False}}, queue_us=2_000_000))
        self.assertEqual((ran["where"], ran["queue"], ran["exec"]), ("remote", 2.0, 3.0))

    def test_a_command_in_buck2_itself_ran_locally_and_queued_for_nothing(self):
        for kind in ("OmittedLocalCommand", "LocalCommand", "WorkerCommand"):
            ran = timing.ran(details({kind: {"action_digest": "ab:1"}}))
            self.assertEqual((ran["where"], ran["queue"], ran["exec"]), ("local", 0.0, 3.0), kind)

    def test_an_action_with_no_command_did_not_run(self):
        self.assertIsNone(timing.ran(details({})))


class SummaryTest(unittest.TestCase):
    def summary(self, runs) -> str:
        out = io.StringIO()
        with (
            mock.patch.object(timing, "collect", return_value=runs),
            mock.patch.object(timing, "critical_path", return_value=[]),
            mock.patch.object(timing.sys, "argv", ["timing.py", __file__]),
            contextlib.redirect_stdout(out),
        ):
            timing.main()
        return out.getvalue()

    def test_remote_and_local_are_counted_apart(self):
        remote = timing.ran(details({"RemoteCommand": {"cache_hit": False}}, exec_us=5_000_000))
        local = timing.ran(details({"OmittedLocalCommand": {}}, exec_us=7_000_000))
        summary = self.summary([("compile", "a:a", remote), ("compile", "b:b", local), ("clippy", "c:c", local)])
        lines = [line.strip() for line in summary.splitlines()]
        self.assertIn("compile  remote    1 ran,      5 s in all; longest a:a 5 s", lines)
        self.assertIn("compile  local     1 ran,      7 s in all; longest b:b 7 s", lines)
        self.assertTrue(any(line.split()[:2] == ["clippy", "local"] for line in lines))
        self.assertTrue(any(line.split()[0] == "remote" and "queued" in line for line in lines))
        self.assertTrue(any(line.split()[0] == "local" and "queued" not in line for line in lines))


if __name__ == "__main__":
    unittest.main()
