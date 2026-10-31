import importlib.util
from pathlib import Path
import sys
import unittest


ROOT = Path(__file__).resolve().parents[2]
# The catalogue imports its selection helper from beside itself.
sys.path.insert(0, str(ROOT / "scripts"))
import suites  # noqa: E402
SPEC = importlib.util.spec_from_file_location(
    "tests_catalog", ROOT / "scripts" / "tests-catalog.py"
)
assert SPEC is not None and SPEC.loader is not None
catalog = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(catalog)


class TestsCatalogTest(unittest.TestCase):
    def test_checked_in_catalog_is_complete(self):
        errors, cargo, swift = catalog.validate(catalog.load_catalog())

        self.assertEqual(errors, [])
        self.assertIn(("testnet", "test", "integration::spec_network"), cargo)
        self.assertIn(("node", "lib", "node"), cargo)
        self.assertIn("AmuxUITests", swift)

    def test_a_suite_recipe_runs_its_own_suite(self):
        selectors = [{"package": "testnet", "modules": ["spec_links"]}]
        errors = []
        catalog.validate_cargo_recipe("links", "just suite links", selectors, errors)
        catalog.validate_cargo_recipe("links", "just suite stories", selectors, errors)
        catalog.validate_cargo_recipe("links", "just test-tui", selectors, errors)

        self.assertEqual(errors, ["links: recipe runs suite 'stories', not its own"])

    def test_a_suite_selects_its_modules_and_targets_and_nothing_else(self):
        suite = {
            "cargo": [
                {"package": "amux", "kind": "bin", "targets": ["amux"]},
                {"package": "amux", "modules": ["process", "attach"]},
                {"package": "tui", "kind": "lib", "targets": ["tui"], "features": ["fixtures"]},
            ]
        }

        self.assertEqual(
            suites.selection(suite),
            [
                "--workspace", "--features", "tui/fixtures", "-E",
                "(package(amux) & kind(bin) & binary(/^(amux)$/))"
                " | (binary_id(amux::integration) & test(/^(attach|process)::/))"
                " | (package(tui) & kind(lib))",
            ],
        )

    def test_the_integration_binary_s_suites_are_its_modules_that_hold_tests(self):
        modules = suites.integration_modules(ROOT / "crates" / "testnet")

        self.assertIn("spec_links", modules)
        self.assertNotIn("support", modules)
        self.assertNotIn("stories", modules)

    def test_every_protocol_model_has_its_own_ci_job(self):
        suites = [
            {
                "name": "protocol-models",
                "cargo": [
                    {"package": "models", "kind": "lib", "targets": ["models"]},
                    {"package": "models", "modules": ["family", "presence"]},
                ],
            }
        ]
        workflow = (
            "jobs:\n"
            "  models:\n"
            "    strategy:\n"
            "      matrix:\n"
            "        model: [family, retired]\n"
            "  build:\n"
            "    runs-on: ubuntu-latest\n"
        )

        self.assertEqual(
            catalog.validate_models_matrix(suites, workflow),
            [
                "protocol-models model presence is not in the CI models job's matrix",
                "the CI models job's matrix names retired, which is no protocol-models model",
            ],
        )
        self.assertEqual(
            catalog.validate_models_matrix(suites, "jobs:\n  test:\n    runs-on: x\n"),
            [".github/workflows/ci.yml has no models job with a model matrix"],
        )

    def test_every_suite_of_testnet_stories_declares_its_modes(self):
        def errors_of(suite):
            errors = []
            catalog.validate_modes(suite["name"], suite, errors)
            return errors

        stories = {"time": "driven", "cargo": [{"package": "testnet", "modules": ["harness"]}]}
        self.assertEqual(errors_of({"name": "both", **stories, "modes": ["real", "simulated"]}), [])
        self.assertEqual(
            errors_of({"name": "silent", **stories}),
            ["silent: a suite of testnet stories must declare its modes (real, simulated)"],
        )
        self.assertEqual(
            errors_of({"name": "odd", **stories, "modes": ["real", "real", "imagined"]}),
            ["odd: unknown modes 'imagined'", "odd: modes lists a mode twice"],
        )
        self.assertEqual(
            errors_of({"name": "empty", **stories, "modes": []}),
            ["empty: modes must be a non-empty list"],
        )
        fixtures = {"time": "none", "cargo": [{"package": "testnet", "modules": ["journal_fixtures"]}]}
        self.assertEqual(errors_of({"name": "fixtures", **fixtures}), [])
        self.assertEqual(
            errors_of({"name": "store", "time": "driven", "cargo": [{"package": "store", "modules": ["x"]}], "modes": ["real"]}),
            ["store: modes belong to suites of testnet stories only"],
        )


if __name__ == "__main__":
    unittest.main()
