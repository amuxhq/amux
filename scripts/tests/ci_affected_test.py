import importlib.util
from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("ci_affected", ROOT / "scripts" / "ci-affected.py")
assert SPEC is not None and SPEC.loader is not None
ci = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci)

# A small workspace: the models are built from a core; the bridge from a
# runtime that is built from the core too; the terminal client from neither.
CRATES = {
    "models": {"folder": "models", "built": {"daemon-core"}, "tests": {"provider-fakes"}},
    "daemon-core": {"folder": "daemon-core", "built": {"wire"}, "tests": {"testkit"}},
    "wire": {"folder": "wire", "built": set(), "tests": set()},
    "provider-fakes": {"folder": "provider-fakes", "built": set(), "tests": set()},
    "testkit": {"folder": "testkit", "built": set(), "tests": set()},
    "app-ffi": {"folder": "app-ffi", "built": {"app-bridge"}, "tests": set()},
    "app-bridge": {"folder": "app-bridge", "built": {"app-runtime"}, "tests": set()},
    "app-runtime": {"folder": "app-runtime", "built": {"wire"}, "tests": set()},
    "tui": {"folder": "tui", "built": {"wire"}, "tests": set()},
    "amux": {"folder": "amux", "built": {"tui", "daemon-core"}, "tests": set()},
    "testnet": {"folder": "testnet", "built": {"daemon-core"}, "tests": {"app-runtime"}},
    "ios-tools": {"folder": "ios-tools", "built": {"wire"}, "tests": set()},
    "browser-core": {"folder": "browser-core", "built": {"wire"}, "tests": set()},
    "named-crate": {"folder": "other-folder", "built": set(), "tests": set()},
}
SANITIZED = ["app-ffi", "tui"]


def groups(*files):
    result = ci.affected(list(files), CRATES, SANITIZED)
    return {name for name, runs in result.items() if runs}


class AffectedTest(unittest.TestCase):
    def test_documentation_alone_runs_nothing_but_the_policy_job(self):
        self.assertEqual(groups("docs/CI.md", "README.md", "images/readme/home.png"), set())

    def test_other_workflows_and_manual_scripts_run_only_the_policy_job(self):
        self.assertEqual(
            groups(
                ".github/workflows/ios.yml",
                ".github/workflows/nightly.yml",
                ".github/workflows/release.yml",
                "scripts/qa.py",
                "scripts/release.py",
                "scripts/tests/ci_affected_test.py",
            ),
            set(),
        )

    def test_a_crates_own_markdown_is_that_crates(self):
        self.assertIn("product", groups("crates/tui/README.md"))

    def test_a_documentation_file_a_test_reads_runs_that_crates_jobs(self):
        with_reader = dict(CRATES, analytics={"folder": "analytics", "built": set(), "tests": set()})
        result = ci.affected(["docs/guide/understand/what-amux-sends.md"], with_reader, SANITIZED)
        self.assertTrue(result["tests"])
        self.assertFalse(result["models"])

    def test_the_phone_app_alone_runs_only_ios_tools_and_leaves_the_rest_to_its_gate(self):
        self.assertEqual(
            groups("apps/apple/Amux/Sources/AmuxApp.swift", "docs/IOS.md"), {"ios_tools"}
        )

    def test_a_phone_fixture_read_by_a_rust_test_runs_that_crates_jobs(self):
        with_node = dict(
            CRATES,
            node={"folder": "node", "built": {"wire"}, "tests": set()},
            amux={"folder": "amux", "built": {"tui", "node"}, "tests": set()},
        )
        result = ci.affected(
            ["apps/apple/Packages/AmuxPush/Tests/AmuxPushTests/Fixtures/sealed-pushes.json"],
            with_node,
            SANITIZED,
        )
        self.assertTrue(result["tests"] and result["product"])
        self.assertFalse(result["ios_tools"])

    def test_a_crate_runs_the_jobs_built_from_it(self):
        self.assertEqual(groups("crates/tui/src/lib.rs"), {"tests", "product", "sanitizers"})
        self.assertEqual(groups("crates/app-runtime/src/lib.rs"), {"tests", "product", "bridge"})
        self.assertEqual(groups("crates/app-bridge/src/lib.rs"), {"tests", "product", "bridge"})
        self.assertEqual(
            groups("crates/app-ffi/src/lib.rs"), {"tests", "sanitizers", "bridge"}
        )
        self.assertEqual(groups("crates/ios-tools/src/lib.rs"), {"tests"})
        self.assertEqual(groups("crates/browser-core/src/lib.rs"), {"tests", "browser"})
        self.assertEqual(groups("crates/daemon-core/src/lib.rs"), {"tests", "product", "models"})
        self.assertEqual(
            groups("crates/wire/src/lib.rs"), {"tests", "product", "models", "bridge", "browser"}
        )

    def test_a_crates_folder_need_not_be_its_name(self):
        self.assertEqual(groups("crates/other-folder/src/lib.rs"), {"tests"})

    def test_a_crates_own_test_dependencies_count_and_those_beneath_it_do_not(self):
        self.assertIn("models", groups("crates/provider-fakes/src/lib.rs"))
        self.assertNotIn("models", groups("crates/testkit/src/lib.rs"))

    def test_anything_else_runs_everything(self):
        for file in ["Cargo.lock", "justfile", ".github/workflows/ci.yml", "scripts/sim.py",
                     "journeys/manifest.json", "proto/amux/v1/chat.proto", "qa/FINDING.md",
                     "crates/gone/src/lib.rs", "crates/README.md"]:
            # Anything else runs the full workspace tests, which already cover ios-tools.
            self.assertEqual(groups(file), set(ci.GROUPS) - {"ios_tools"}, file)

    def test_the_browser_extension_alone_runs_its_own_job(self):
        self.assertEqual(groups("js/extension/src/background/index.ts"), {"browser"})

    def test_two_commits_that_cannot_be_compared_are_not_guessed_at(self):
        self.assertIsNone(ci.changed_files("", "HEAD"))
        self.assertIsNone(ci.changed_files("0" * 40, "HEAD"))


class WorkspaceTest(unittest.TestCase):
    def test_the_groups_crates_exist(self):
        crates = ci.workspace()
        for name in ci.MODELS + ci.BRIDGE + ci.sanitized() + list(ci.READS.values()):
            self.assertIn(name, crates)

    def test_the_models_are_not_built_from_the_clients(self):
        built = ci.built_from(ci.MODELS, ci.workspace())
        self.assertIn("daemon-core", built)
        self.assertFalse({"tui", "app-ffi", "amux"} & built, built)

    def test_every_documentation_file_a_test_reads_is_listed(self):
        """A test reading a file under docs/ that READS does not name would be
        skipped by a change to that file."""
        read = set()
        for source in (ROOT / "crates").rglob("*.rs"):
            if "target" in source.parts:
                continue
            for found in re.findall(r'"(?:\.\./)+((?:docs|images|marketing)/[^"]+)"', source.read_text()):
                read.add(found)
        self.assertEqual(read - set(ci.READS), set())


if __name__ == "__main__":
    unittest.main()
