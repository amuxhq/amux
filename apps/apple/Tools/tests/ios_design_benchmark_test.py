"""Benchmark source rewrites must stay bounded and fail on changed anchors."""

import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "ios_design_benchmark", Path(__file__).resolve().parents[1] / "ios-design-benchmark.py")
benchmark = importlib.util.module_from_spec(spec)
spec.loader.exec_module(benchmark)


class DesignBenchmarkTests(unittest.TestCase):
    def test_source_anchor_must_be_unique(self):
        for text in ("absent", "target target"):
            with self.assertRaises(ValueError):
                benchmark.replace_once(text, "target", "replacement")
        self.assertEqual(benchmark.replace_once("one target", "target", "replacement"),
                         "one replacement")

    def test_round_keeps_shot_and_index_generation_and_declares_each_idea(self):
        original = ("prefix\n    static let variants: OLD\n"
                    "    static let groups: OLD\n"
                    "    static var screens: SHOTS_AND_INDEX\n")
        for count in (2, 4):
            rewritten = benchmark.catalog_round(original, count)
            self.assertTrue(rewritten.startswith("prefix\n"))
            self.assertTrue(rewritten.endswith("    static var screens: SHOTS_AND_INDEX\n"))
            self.assertNotIn("OLD", rewritten)
            self.assertEqual(rewritten.count("Variant(id:"), count)
            self.assertIn("size: nil, options: variants", rewritten)
            for i in range(count):
                self.assertIn(f'Variant(id: "{i}"', rewritten)

    def test_repeat_labels_describe_the_actual_changed_source_values(self):
        original = "    static let variants: OLD\n    static var screens: unchanged"
        rewritten = benchmark.catalog_round(original, 2, gutter_offset=1)
        self.assertIn("Gutter 15 pt", rewritten)
        self.assertIn("Gutter 19 pt", rewritten)
