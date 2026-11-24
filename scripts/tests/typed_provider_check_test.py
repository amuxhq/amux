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
    "typed_provider_check", ROOT / "scripts" / "typed-provider-check.py"
)
assert SPEC is not None and SPEC.loader is not None
typed = importlib.util.module_from_spec(SPEC)
# Registered before it runs: its dataclasses look their module up by name.
sys.modules[SPEC.name] = typed
SPEC.loader.exec_module(typed)

FACTS = "crates/interpret/src/claude_pty/facts.rs"


class TypedProviderCheckTest(unittest.TestCase):
    """The check over a copy of the checked sources, planted with each way of
    going around the types."""

    # Copied with copyfile, not copy: the tests rewrite the copies, and a
    # Buck2 check's inputs, whose permissions copy would keep, are read-only.
    def setUp(self):
        self.room = tempfile.TemporaryDirectory()
        self.root = Path(self.room.name)
        for source in typed.SOURCES:
            target = self.root / source
            target.parent.mkdir(parents=True, exist_ok=True)
            if (ROOT / source).is_dir():
                shutil.copytree(ROOT / source, target, copy_function=shutil.copyfile)
            else:
                shutil.copyfile(ROOT / source, target)

    def tearDown(self):
        self.room.cleanup()

    def check(self) -> tuple[int, str]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
            code = typed.check(self.root)
        return code, out.getvalue()

    def plant(self, path: str, line: str):
        source = self.root / path
        text = source.read_text()
        at = text.index("\n#[cfg(test)]") if "\n#[cfg(test)]" in text else len(text)
        source.write_text(text[:at] + f"\nfn planted() {{\n    {line}\n}}\n" + text[at:])

    def test_the_tree_passes(self):
        self.assertEqual(self.check()[0], 0)

    def test_each_way_around_the_types_fails(self):
        for line in [
            'let line = serde_json::json!({"type": "user"});',
            'let kind = row.get("type");',
            'let kind = row["type"].as_str();',
            'let kind = row.pointer("/message/id");',
            'let schema = json_as_written(payload, &["request", "requested_schema"]);',
            "let fields = row.as_object();",
            "let row = serde_json::from_slice::<Value>(&bytes);",
            "let row: Value = serde_json::from_slice(&bytes).unwrap();",
        ]:
            with self.subTest(line=line):
                source = self.root / FACTS
                clean = source.read_text()
                self.plant(FACTS, line)
                code, out = self.check()
                source.write_text(clean)
                self.assertEqual(code, 1, out)
                self.assertIn("in planted", out)

    def test_an_exemption_covers_only_its_own_function(self):
        self.plant(FACTS, "let input = serde_json::from_str::<Value>(&tool.input);")
        code, out = self.check()
        self.assertEqual(code, 1, out)
        self.assertIn("in planted", out)

    def test_test_modules_are_not_searched(self):
        source = self.root / FACTS
        source.write_text(source.read_text() + '\nfn after_tests() { let _ = serde_json::json!({}); }\n')
        self.assertEqual(self.check()[0], 0)

    def test_an_exemption_that_matches_nothing_fails(self):
        source = self.root / "crates/interpret/src/codex/facts.rs"
        source.write_text(
            source.read_text().replace(
                "Some(Value::Object(Default::default()))", "Some(Value::from(serde_json::Map::new()))"
            )
        )
        code, out = self.check()
        self.assertEqual(code, 1, out)
        self.assertIn("stale exemption: crates/interpret/src/codex/facts.rs no_content", out)


if __name__ == "__main__":
    unittest.main()
