"""Keep the unit recipe's selection honest and its scheme complete."""

import importlib.util
from pathlib import Path
import re
import sys
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]
ROOT = SCRIPTS.parents[2]
sys.path.insert(0, str(SCRIPTS))
specification = importlib.util.spec_from_file_location("ios_unit", SCRIPTS / "ios-unit.py")
recipe = importlib.util.module_from_spec(specification)
specification.loader.exec_module(recipe)


class UnitRecipeTests(unittest.TestCase):
    def test_default_includes_the_app_hosted_and_package_tests_but_not_the_pictures(self):
        with patch.object(recipe, "suites", return_value=["AmuxAppTests", "CoreTests"]):
            self.assertEqual(
                recipe.selected(["-quiet"]),
                ["-quiet", "-only-testing:AmuxAppTests", "-only-testing:CoreTests"],
            )
        self.assertNotIn("AmuxComponentSnapshotTests", recipe.suites())

    def test_a_named_selection_is_kept_as_given(self):
        named = ["-only-testing:AmuxAppTests/CloudSessionStoreTests",
                 "-only-testing:CoreTests/CloudTests", "-quiet"]
        with patch.object(recipe, "suites", return_value=["AmuxAppTests", "CoreTests"]):
            self.assertEqual(recipe.selected(named), named)

    def test_an_unknown_target_is_refused_by_name(self):
        with patch.object(recipe, "suites", return_value=["AmuxAppTests"]), \
                self.assertRaisesRegex(SystemExit, "NoSuchTests"):
            recipe.selected(["-only-testing:NoSuchTests/Case"])

    def test_skip_build_runs_the_built_bundles_without_generating(self):
        with patch.object(recipe.ios_project, "generate") as generate, \
                patch.object(recipe.ios_simulators, "ready", return_value="simulator"), \
                patch.object(recipe.subprocess, "run") as run:
            recipe.main(["--skip-build"])
        generate.assert_not_called()
        invocation = run.call_args.args[0]
        self.assertEqual(invocation[:2], ["xcodebuild", "test-without-building"])
        self.assertNotIn("--skip-build", invocation)
        self.assertIn("-only-testing:AmuxAppTests", invocation)

    def test_the_scheme_holds_every_suite(self):
        """A package suite the scheme leaves out could never be selected."""
        project = (ROOT / "apps/apple/project.yml").read_text()
        scheme = project.split(f"\n  {recipe.SCHEME}:\n", 1)[1]
        targets = scheme.split("    test:\n", 1)[1].split("\n\n", 1)[0]
        listed = set(re.findall(r"- (?:package: \w+/)?(\w+)", targets))
        with patch.object(recipe, "PACKAGES", ROOT / "apps/apple/Packages"):
            self.assertLessEqual(set(recipe.suites()), listed)
