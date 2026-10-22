import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
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
        self.assertIn(("testnet", "test", "spec_network"), cargo)
        self.assertIn(("node", "lib", "node"), cargo)
        self.assertIn("AmuxUITests", swift)

    def test_test_crate_recipe_cannot_name_an_unlisted_target(self):
        errors = []
        catalog.validate_cargo_recipe(
            "example",
            "just test-crate testnet -- --test absent",
            [
                {
                    "package": "testnet",
                    "targets": ["spec"],
                    "features": ["bundled"],
                }
            ],
            errors,
        )

        self.assertEqual(
            errors,
            ["example: recipe selects zero catalogued tests named 'absent'"],
        )

    def test_every_protocol_model_has_its_own_ci_job(self):
        suites = [
            {
                "name": "protocol-models",
                "cargo": [
                    {"package": "models", "kind": "lib", "targets": ["models"]},
                    {"package": "models", "targets": ["family", "presence"]},
                ],
            }
        ]
        workflow = (
            "jobs:\n"
            "  models:\n"
            "    strategy:\n"
            "      matrix:\n"
            "        target: [family, retired]\n"
            "  build:\n"
            "    runs-on: ubuntu-latest\n"
        )

        self.assertEqual(
            catalog.validate_models_matrix(suites, workflow),
            [
                "protocol-models test target presence is not in the CI models job's matrix",
                "the CI models job's matrix names retired, which is no protocol-models test target",
            ],
        )
        self.assertEqual(
            catalog.validate_models_matrix(suites, "jobs:\n  test:\n    runs-on: x\n"),
            [".github/workflows/ci.yml has no models job with a target matrix"],
        )

    def test_every_suite_of_testnet_stories_declares_its_modes(self):
        def errors_of(suite):
            errors = []
            catalog.validate_modes(suite["name"], suite, errors)
            return errors

        stories = {"time": "driven", "cargo": [{"package": "testnet", "targets": ["harness"]}]}
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
        fixtures = {"time": "none", "cargo": [{"package": "testnet", "targets": ["journal_fixtures"]}]}
        self.assertEqual(errors_of({"name": "fixtures", **fixtures}), [])
        self.assertEqual(
            errors_of({"name": "store", "time": "driven", "cargo": [{"package": "store", "targets": ["x"]}], "modes": ["real"]}),
            ["store: modes belong to suites of testnet stories only"],
        )


if __name__ == "__main__":
    unittest.main()
