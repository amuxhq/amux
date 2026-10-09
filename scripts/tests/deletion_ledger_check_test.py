import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "deletion_ledger_check", ROOT / "scripts" / "deletion-ledger-check.py"
)
assert SPEC is not None and SPEC.loader is not None
ledger = importlib.util.module_from_spec(SPEC)
# Registered before it runs: its dataclasses look their module up by name.
sys.modules[SPEC.name] = ledger
SPEC.loader.exec_module(ledger)


# Removed names, split so this file is not itself a hit.
SURVIVOR = "Suspend" "All"
RETIRED_KEY = "ui.artifact_" "cache_mib"


class DeletionLedgerCheckTest(unittest.TestCase):
    def setUp(self):
        self.room = tempfile.TemporaryDirectory()
        self.root = Path(self.room.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        (self.root / "crates" / "live").mkdir(parents=True)
        (self.root / "crates" / "live" / "Cargo.toml").write_text("[package]\n")
        (self.root / "Cargo.toml").write_text('[workspace]\nmembers = ["crates/live"]\n')
        (self.root / "README.md").write_text("Nothing removed is named here.\n")

    def tearDown(self):
        self.room.cleanup()

    def check(self) -> int:
        with mock.patch.object(ledger, "ROOT", self.root), \
                mock.patch("builtins.print"):
            return ledger.main()

    def test_a_clean_tree_passes(self):
        self.assertEqual(self.check(), 0)

    def test_a_survivor_anywhere_fails(self):
        # Untracked files count: a survivor must not pass because it was never
        # added to the index.
        (self.root / "docs").mkdir()
        (self.root / "docs" / "UPDATES.md").write_text(f"Run {SURVIVOR} before an update.\n")
        self.assertEqual(self.check(), 1)

    def test_devlog_history_is_not_searched(self):
        (self.root / "DEVLOG.md").write_text(f"Removed {SURVIVOR}.\n")
        self.assertEqual(self.check(), 0)

    def test_a_crate_outside_the_members_list_fails(self):
        (self.root / "crates" / "dead").mkdir()
        (self.root / "crates" / "dead" / "Cargo.toml").write_text("[package]\n")
        self.assertEqual(self.check(), 1)

    def test_the_exemption_covers_only_its_own_file(self):
        (self.root / "crates" / "settings" / "src").mkdir(parents=True)
        (self.root / "crates" / "settings" / "src" / "lib.rs").write_text(
            f'const RETIRED: &str = "{RETIRED_KEY}";\n')
        self.assertEqual(self.check(), 0)
        (self.root / "crates" / "live" / "lib.rs").write_text(
            f'const CACHE: &str = "{RETIRED_KEY}";\n')
        self.assertEqual(self.check(), 1)


if __name__ == "__main__":
    unittest.main()
