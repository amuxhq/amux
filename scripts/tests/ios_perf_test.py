"""The phone performance runner's contracts: the budgets and machines the
performance page states are the ones the runner enforces, a pairing link is
rewritten to a gate and nothing else, and the judge holds a sample to its
budget, its worst-sample budget and its baseline."""

import importlib.util
import json
import base64
from pathlib import Path
import re
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
SPEC = importlib.util.spec_from_file_location("ios_perf", ROOT / "scripts" / "ios-perf.py")
assert SPEC is not None and SPEC.loader is not None
perf = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = perf
SPEC.loader.exec_module(perf)

PAGE = ROOT / "docs/PERFORMANCE.md"


def phone_section() -> str:
    text = PAGE.read_text()
    return text[text.index("## The phone"):]


def table_rows(section: str, header: str) -> list[list[str]]:
    """The body rows of the first table whose header row starts with `header`."""
    lines = section.splitlines()
    for at, line in enumerate(lines):
        if line.startswith(header):
            rows = []
            for row in lines[at + 2:]:
                if not row.startswith("|"):
                    break
                rows.append([cell.strip() for cell in row.strip("|").split("|")])
            return rows
    raise AssertionError(f"no table starting {header!r} under The phone")


def number(cell: str) -> float | None:
    found = re.search(r"[\d,]+(?:\.\d+)?", cell)
    return float(found.group(0).replace(",", "")) if found else None


class ThePageStatesTheRunner(unittest.TestCase):
    def test_the_budget_table_matches_the_runners_budgets(self):
        rows = table_rows(phone_section(), "| Metric | Group |")
        stated = {}
        for metric, group, _what, budget, worst, tolerance in rows:
            name = metric.strip("`")
            budget_number = number(budget)
            unit = budget.replace(f"{budget_number:,.0f}", "").replace(str(int(budget_number)), "").strip()
            drift = None if "ceiling" in tolerance else int(number(tolerance) or 0)
            stated[name] = (unit, group, budget_number, number(worst), drift)
        self.assertEqual(stated, perf.BUDGETS)

    def test_the_machine_table_matches_the_runners_machines(self):
        rows = table_rows(phone_section(), "| Machine | Model |")
        stated = {model.strip("`"): name.strip("`") for name, model in rows}
        self.assertEqual(stated, perf.MACHINES)


class ALinkIsRewrittenToItsGate(unittest.TestCase):
    def test_only_the_addresses_change(self):
        payload = {"host_id": "h", "secret": [1, 2], "addrs": ["127.0.0.1:5000"], "cloud_url": "https://c"}
        encoded = base64.urlsafe_b64encode(json.dumps(payload).encode()).decode().rstrip("=")
        link = perf.gated_link(f"amux://pair?payload={encoded}", "127.0.0.1:6000")
        prefix, rewritten = link.split("=", 1)
        self.assertEqual(prefix, "amux://pair?payload")
        decoded = json.loads(base64.urlsafe_b64decode(rewritten + "=" * (-len(rewritten) % 4)))
        self.assertEqual(decoded, {**payload, "addrs": ["127.0.0.1:6000"]})


class TheJudge(unittest.TestCase):
    def samples(self, **given):
        samples = {name: [] for name in perf.BUDGETS}
        samples.update(given)
        return samples

    def test_a_median_over_budget_fails_and_an_unmeasured_metric_is_not_reported(self):
        verdict = perf.judge(self.samples(**{"cold first frame": [400, 450, 520, 430, 410]}), None)
        self.assertTrue(verdict["passed"])
        self.assertEqual([result["metric"] for result in verdict["results"]], ["cold first frame"])
        verdict = perf.judge(self.samples(**{"cold first frame": [400, 610, 520, 530, 950]}), None)
        self.assertFalse(verdict["passed"])
        self.assertIn("worst", verdict["results"][0]["note"])

    def test_a_ceiling_only_metric_ignores_a_baseline_it_rests_well_above(self):
        baseline = {"streaming hitch time": 0.0}
        verdict = perf.judge(self.samples(**{"streaming hitch time": [1.7, 1.7, 1.7, 1.7, 1.7]}), baseline)
        self.assertTrue(verdict["passed"])
        self.assertIsNone(verdict["results"][0]["drift_percent"])
        self.assertIn("ceiling only", perf.report(verdict, "pinned-mac", "Mac14,6", 1.0, []))
        self.assertIn("ceiling only", perf.summary_line(verdict["results"][0]))

    def test_drift_past_the_tolerance_fails_inside_the_budget(self):
        baseline = {"cold first frame": 300.0}
        inside = perf.judge(self.samples(**{"cold first frame": [340, 340, 340, 340, 340]}), baseline)
        self.assertTrue(inside["passed"])
        past = perf.judge(self.samples(**{"cold first frame": [350, 350, 350, 350, 350]}), baseline)
        self.assertFalse(past["passed"])
        self.assertIn("tolerance", past["results"][0]["note"])

    def test_a_count_metric_has_no_slack(self):
        verdict = perf.judge(self.samples(**{"idle display ticks": [0, 0, 1, 0, 0]}), None)
        self.assertFalse(verdict["passed"])


if __name__ == "__main__":
    unittest.main()
