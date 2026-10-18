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


if __name__ == "__main__":
    unittest.main()
