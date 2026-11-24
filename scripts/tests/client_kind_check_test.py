import importlib.util
from pathlib import Path
import contextlib
import io
import shutil
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "client_kind_check", ROOT / "scripts" / "client-kind-check.py"
)
assert SPEC is not None and SPEC.loader is not None
kinds = importlib.util.module_from_spec(SPEC)
# Registered before it runs: its dataclasses look their module up by name.
sys.modules[SPEC.name] = kinds
SPEC.loader.exec_module(kinds)

FLEET = "crates/ui-view/src/fleet.rs"
HOME = "apps/apple/Packages/AmuxFeatures/Sources/AmuxFeatures/AgentsHome.swift"


class ClientKindCheckTest(unittest.TestCase):
    """The check over a copy of the client crates' sources, planted with each
    way of deciding by kind or provider name."""

    def setUp(self):
        self.room = tempfile.TemporaryDirectory()
        self.root = Path(self.room.name)
        for crate in kinds.CRATES:
            shutil.copytree(ROOT / crate / "src", self.root / crate / "src")
        built_ins = self.root / kinds.BUILT_INS
        built_ins.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(ROOT / kinds.BUILT_INS, built_ins)
        # The apps' Swift alone, in its folders.
        apps = ROOT / kinds.SWIFT
        for source in apps.rglob("*.swift"):
            if ".build" in source.relative_to(apps).parts:
                continue
            copy = self.root / source.relative_to(ROOT)
            copy.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(source, copy)

    def tearDown(self):
        self.room.cleanup()

    def check(self) -> tuple[int, str]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
            code = kinds.check(self.root)
        return code, out.getvalue()

    def plant(self, path: str, line: str, before_tests: bool = True):
        source = self.root / path
        text = source.read_text()
        at = text.index("\n#[cfg(test)]") if before_tests and "\n#[cfg(test)]" in text else len(text)
        source.write_text(text[:at] + f"\nfn planted() {{\n    {line}\n}}\n" + text[at:])

    def test_the_tree_passes(self):
        code, out = self.check()
        self.assertEqual(code, 0, out)

    def test_each_way_of_deciding_by_kind_fails(self):
        for line in [
            "let terminal = agent.kind() == wire::Kind::ClaudePty;",
            "match kind { Kind::Acp => {} _ => {} }",
            'let codex = provider == "codex";',
            'let opencode = provider == "opencode";',
            'let grok = matches!(provider, "grok");',
            "let claude = provider == wire::agents::CLAUDE;",
        ]:
            with self.subTest(line=line):
                source = self.root / FLEET
                clean = source.read_text()
                self.plant(FLEET, line)
                code, out = self.check()
                source.write_text(clean)
                self.assertEqual(code, 1, out)
                self.assertIn("in planted", out)

    def test_the_built_in_ids_follow_wire(self):
        self.assertEqual(kinds.built_in_ids(ROOT), ["antigravity", "opencode", "grok"])
        # A built-in added to the list is searched for from then on.
        source = self.root / kinds.BUILT_INS
        source.write_text(source.read_text().replace(
            'id: "grok",', 'id: "grok",\n    },\n    BuiltIn {\n        id: "newcomer",', 1))
        self.assertIn("newcomer", kinds.built_in_ids(self.root))
        self.plant(FLEET, 'let newcomer = provider == "newcomer";')
        code, out = self.check()
        self.assertEqual(code, 1, out)
        self.assertIn("in planted", out)

    def test_an_entry_covers_only_its_own_function(self):
        # Nothing in the fleet view is listed, so a kind named there fails.
        self.plant(FLEET, "let _ = Kind::Codex;")
        code, out = self.check()
        self.assertEqual(code, 1, out)
        self.assertIn(f"{FLEET}", out)

    def test_test_items_and_comments_are_not_searched(self):
        source = self.root / FLEET
        source.write_text(
            source.read_text()
            + "\n#[cfg(test)]\nfn fixture() {\n    let _ = Kind::Codex;\n}\n"
            + "\n// Kind::Codex is named in words only.\n"
        )
        code, out = self.check()
        self.assertEqual(code, 0, out)

    def test_an_entry_that_matches_nothing_fails(self):
        source = self.root / "crates/ui-state/src/session/record.rs"
        source.write_text(
            source.read_text().replace(
                "[wire::agents::CLAUDE, wire::agents::CODEX]", "OWN_PROVIDERS"
            )
        )
        code, out = self.check()
        self.assertEqual(code, 1, out)
        self.assertIn("stale entry: crates/ui-state/src/session/record.rs kept_provider", out)


    def plant_swift(self, path: str, line: str):
        source = self.root / path
        source.write_text(source.read_text() + f"\nfunc planted() {{\n    {line}\n}}\n")

    def test_each_way_of_deciding_by_kind_in_swift_fails(self):
        for line in [
            "let terminal = row.card.kind == .claudePty",
            "if case .acp = kind { return }",
            "switch kind { case .codex: break default: break }",
            'let codex = provider == "codex"',
            'let grok = row.card.provider == "grok"',
        ]:
            with self.subTest(line=line):
                source = self.root / HOME
                clean = source.read_text()
                self.plant_swift(HOME, line)
                code, out = self.check()
                source.write_text(clean)
                self.assertEqual(code, 1, out)
                self.assertIn(f"{HOME}:", out)
                self.assertIn("in planted", out)

    def test_swift_names_members_comments_values_and_fixtures_are_not_searched(self):
        self.plant_swift(HOME, "let tint = Palette.codex")
        self.plant_swift(HOME, "// .codex is named in words only")
        for fixture in [
            "apps/apple/Packages/AmuxFeatures/Tests/AmuxFeaturesTests/ChatWordsTests.swift",
            "apps/apple/Amux/Debug/ComponentCatalog.swift",
        ]:
            self.plant_swift(fixture, 'let codex = provider == "codex"')
        code, out = self.check()
        self.assertEqual(code, 0, out)

    def test_a_swift_entry_that_matches_nothing_fails(self):
        entry = kinds.Allowed(HOME, "close", kinds.KIND, "planted for the test")
        kinds.ALLOWED.append(entry)
        try:
            code, out = self.check()
        finally:
            kinds.ALLOWED.remove(entry)
        self.assertEqual(code, 1, out)
        self.assertIn(f"stale entry: {HOME} close", out)


if __name__ == "__main__":
    unittest.main()
