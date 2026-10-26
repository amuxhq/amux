"""Check how the QA launcher reads a tester's transcript into its summary."""

import importlib.util
import json
import os
from pathlib import Path
import sys
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

specification = importlib.util.spec_from_file_location("qa", SCRIPTS / "qa.py")
qa = importlib.util.module_from_spec(specification)
specification.loader.exec_module(qa)


def result(turns: int, cost: float, duration_ms: int) -> str:
    return json.dumps({"type": "result", "subtype": "success", "num_turns": turns,
                       "total_cost_usd": cost, "duration_ms": duration_ms})


class Machine(unittest.TestCase):
    """The process table and memory figures read on the machine running the
    test, macOS or Linux, whose tools differ."""

    def test_the_process_table_lists_this_process_with_its_parent_and_footprint(self):
        table = qa.processes()
        self.assertIn(os.getpid(), table)
        parent, footprint, command = table[os.getpid()]
        self.assertEqual(parent, os.getppid())
        self.assertGreater(footprint, 0)
        self.assertIn("python", command.lower())

    def test_the_machine_has_memory_in_use(self):
        self.assertGreater(qa.memory_in_use(), 0)


class Results(unittest.TestCase):
    def test_a_tester_woken_after_its_summary_counts_every_turn_and_its_session_alone(self):
        lines = [
            json.dumps({"type": "system"}),
            result(84, 3.5, 31 * 60_000),
            "wt: lease released",
            result(1, 0.02, 6_000),
        ]
        words, session_ms = qa.results(lines)
        self.assertEqual(words, "success, 85 turns, $3.52 over 2 wakes")
        self.assertEqual(session_ms, 31 * 60_000)

    def test_one_result_reads_as_it_did(self):
        words, session_ms = qa.results([result(12, 0.4, 90_000)])
        self.assertEqual(words, "success, 12 turns, $0.40")
        self.assertEqual(session_ms, 90_000)

    def test_no_result_leaves_the_wall_clock_to_say_how_long(self):
        self.assertEqual(qa.results(["not json"]), ("no result line", None))


if __name__ == "__main__":
    unittest.main()
