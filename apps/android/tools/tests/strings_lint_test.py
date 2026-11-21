"""Unit tests for apps/android/tools/strings_lint.py and strings-lint.sh."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

TOOLS_DIR = Path(__file__).resolve().parents[1]
REPO_ROOT = Path(__file__).resolve().parents[4]
if str(TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(TOOLS_DIR))

import strings_lint as lint  # noqa: E402


class StringsLintTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.strings_path = Path("apps/android/app/src/main/res/values/strings.xml")
        self.write(
            self.strings_path,
            '<?xml version="1.0" encoding="utf-8"?>\n'
            "<resources>\n"
            '    <string name="app_name">amux</string>\n'
            '    <string name="reconnecting">Reconnecting · your draft is kept</string>\n'
            '    <string name="loading">Catching up…</string>\n'
            '    <string name="possessive">Host’s fingerprint</string>\n'
            "</resources>\n",
        )
        self.write(lint.EXEMPTIONS, json.dumps({"version": 1, "files": {}}))

    def write(self, rel_path: Path, text: str) -> None:
        target = self.root / rel_path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def test_clean_strings_and_empty_exemptions_pass(self):
        complaints, _ = lint.check(self.root)
        self.assertEqual(complaints, [])

    def test_shell_script_passes_on_clean_tree(self):
        result = subprocess.run(
            ["sh", str(REPO_ROOT / "apps/android/tools/strings-lint.sh"), "--root", str(self.root)],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)

    def test_mechanical_copy_violations_fail(self):
        cases = [
            ("exclamation", "Paired!"),
            ("straight_double_quote", 'Say \\"hello\\"'),
            ("straight_apostrophe", "Host\\'s state"),
            ("three_dots", "Reconnecting..."),
            ("em_dash", "Offline — retry"),
            ("semicolon", "Failed; retry"),
        ]
        for key, bad_value in cases:
            with self.subTest(key=key):
                self.write(
                    self.strings_path,
                    '<?xml version="1.0" encoding="utf-8"?>\n'
                    "<resources>\n"
                    f'    <string name="{key}">{bad_value}</string>\n'
                    "</resources>\n",
                )
                complaints, _ = lint.check(self.root)
                self.assertTrue(complaints, f"expected failure for {key}: {bad_value}")

    def test_duplicate_string_key_fails(self):
        self.write(
            self.strings_path,
            '<?xml version="1.0" encoding="utf-8"?>\n'
            "<resources>\n"
            '    <string name="dup">One</string>\n'
            '    <string name="dup">Two</string>\n'
            "</resources>\n",
        )
        complaints, _ = lint.check(self.root)
        self.assertTrue(any("duplicate string key" in c for c in complaints))

    def test_string_array_and_plurals_duplicate_and_empty_items_fail(self):
        self.write(
            self.strings_path,
            '<?xml version="1.0" encoding="utf-8"?>\n'
            "<resources>\n"
            '    <string-array name="choices">\n'
            "        <item>One</item>\n"
            "        <item>   </item>\n"
            "    </string-array>\n"
            '    <string-array name="choices">\n'
            "        <item>Two</item>\n"
            "    </string-array>\n"
            '    <plurals name="agents">\n'
            '        <item quantity="one">One agent</item>\n'
            '        <item quantity="other"></item>\n'
            "    </plurals>\n"
            '    <plurals name="agents">\n'
            '        <item quantity="other">Many agents</item>\n'
            "    </plurals>\n"
            "</resources>\n",
        )
        complaints, _ = lint.check(self.root)
        self.assertTrue(any("duplicate string-array key 'choices'" in c for c in complaints), complaints)
        self.assertTrue(any("string-array 'choices'[1] is empty" in c for c in complaints), complaints)
        self.assertTrue(any("duplicate plurals key 'agents'" in c for c in complaints), complaints)
        self.assertTrue(any("plurals 'agents'[1] is empty" in c for c in complaints), complaints)

    def test_feature_kotlin_hardcoded_ui_literals_fail_unless_exempted(self):
        feature_kt = Path("apps/android/features/src/main/kotlin/sh/amux/features/Screen.kt")
        self.write(
            feature_kt,
            "package sh.amux.features\n"
            "fun Screen(label: String) {\n"
            "    // Text(\"ignored in comment\")\n"
            "    /* contentDescription = \"ignored in block comment\" */\n"
            "    Text(text = label)\n"
            '    Text("Hardcoded title")\n'
            '    Icon(contentDescription = "Hardcoded icon")\n'
            "}\n",
        )
        complaints, _ = lint.check(self.root)
        self.assertEqual(len(complaints), 2, complaints)
        self.assertTrue(any('Hardcoded title' in c for c in complaints))
        self.assertTrue(any('Hardcoded icon' in c for c in complaints))

        # Recording both literals in noncopy.json satisfies the lint.
        self.write(
            lint.EXEMPTIONS,
            json.dumps(
                {
                    "version": 1,
                    "files": {
                        feature_kt.as_posix(): [
                            {
                                "literal": '"Hardcoded title"',
                                "context": 'Text("Hardcoded title")',
                                "reason": "Test literal exemption.",
                                "count": 1,
                            },
                            {
                                "literal": '"Hardcoded icon"',
                                "context": 'Icon(contentDescription = "Hardcoded icon")',
                                "reason": "Test contentDescription exemption.",
                                "count": 1,
                            },
                        ]
                    },
                }
            ),
        )
        complaints, _ = lint.check(self.root)
        self.assertEqual(complaints, [])

    def test_xml_exemption_allows_reviewed_entry_and_detects_staleness(self):
        line = '<string name="verbatim">echo "ok"</string>'
        self.write(
            self.strings_path,
            '<?xml version="1.0" encoding="utf-8"?>\n'
            "<resources>\n"
            f"    {line}\n"
            "</resources>\n",
        )
        self.write(
            lint.EXEMPTIONS,
            json.dumps(
                {
                    "version": 1,
                    "files": {
                        self.strings_path.as_posix(): [
                            {
                                "literal": 'echo "ok"',
                                "context": line,
                                "reason": "Verbatim command example.",
                                "count": 1,
                            }
                        ]
                    },
                }
            ),
        )
        self.assertEqual(lint.check(self.root)[0], [])

        # Removing the string makes the exemption stale.
        self.write(
            self.strings_path,
            '<?xml version="1.0" encoding="utf-8"?>\n'
            "<resources>\n"
            '    <string name="app_name">amux</string>\n'
            "</resources>\n",
        )
        complaints, _ = lint.check(self.root)
        self.assertTrue(any("stale" in c for c in complaints))

    def test_noncopy_schema_errors_fail(self):
        # Unsupported version
        self.write(lint.EXEMPTIONS, json.dumps({"version": 2, "files": {}}))
        with self.assertRaisesRegex(ValueError, "version"):
            lint.check(self.root)

        # Missing referenced file
        self.write(
            lint.EXEMPTIONS,
            json.dumps(
                {
                    "version": 1,
                    "files": {
                        "apps/android/app/src/main/kotlin/Missing.kt": [
                            {"literal": '"x"', "context": 'val a = "x"', "reason": "Wire key", "count": 1}
                        ]
                    },
                }
            ),
        )
        with self.assertRaisesRegex(ValueError, "missing source"):
            lint.check(self.root)

        # Missing reason
        kt_file = Path("apps/android/app/src/main/kotlin/Wire.kt")
        self.write(kt_file, 'val key = "wire"\n')
        self.write(
            lint.EXEMPTIONS,
            json.dumps(
                {
                    "version": 1,
                    "files": {
                        kt_file.as_posix(): [
                            {"literal": '"wire"', "context": 'val key = "wire"', "reason": "", "count": 1}
                        ]
                    },
                }
            ),
        )
        with self.assertRaisesRegex(ValueError, "no reason"):
            lint.check(self.root)

        # Invalid count
        self.write(
            lint.EXEMPTIONS,
            json.dumps(
                {
                    "version": 1,
                    "files": {
                        kt_file.as_posix(): [
                            {"literal": '"wire"', "context": 'val key = "wire"', "reason": "Wire key", "count": 0}
                        ]
                    },
                }
            ),
        )
        with self.assertRaisesRegex(ValueError, "positive integer"):
            lint.check(self.root)

        # Duplicate JSON key
        self.write(lint.EXEMPTIONS, '{"version": 1, "files": {}, "files": {}}')
        with self.assertRaisesRegex(ValueError, "duplicate key"):
            lint.check(self.root)

    def test_scans_all_values_strings_xml_variants_and_cross_file_duplicates(self):
        chat_rows_xml = Path("apps/android/features/src/main/res/values/strings_chat_rows.xml")
        chat_asks_xml = Path("apps/android/features/src/main/res/values/strings_chat_asks.xml")
        localized_xml = Path("apps/android/features/src/main/res/values-fr/strings_chat_rows.xml")
        non_values_xml = Path("apps/android/features/src/main/res/xml/strings_ignored.xml")

        self.write(
            chat_rows_xml,
            '<?xml version="1.0" encoding="utf-8"?>\n'
            "<resources>\n"
            '    <string name="row_ok">Working…</string>\n'
            '    <string name="row_bad">Done!</string>\n'
            "</resources>\n",
        )
        self.write(
            chat_asks_xml,
            '<?xml version="1.0" encoding="utf-8"?>\n'
            "<resources>\n"
            '    <string name="row_ok">Duplicate across sibling file</string>\n'
            "</resources>\n",
        )
        self.write(
            localized_xml,
            '<?xml version="1.0" encoding="utf-8"?>\n'
            "<resources>\n"
            '    <string name="row_ok">En cours…</string>\n'
            "</resources>\n",
        )
        self.write(
            non_values_xml,
            '<?xml version="1.0" encoding="utf-8"?>\n'
            "<resources>\n"
            '    <string name="ignored">Not in values!</string>\n'
            "</resources>\n",
        )

        discovered = [p.relative_to(self.root).as_posix() for p in lint.strings_xml_files(self.root)]
        self.assertIn(chat_rows_xml.as_posix(), discovered)
        self.assertIn(chat_asks_xml.as_posix(), discovered)
        self.assertIn(localized_xml.as_posix(), discovered)
        self.assertNotIn(non_values_xml.as_posix(), discovered)

        complaints, _ = lint.check(self.root)
        self.assertTrue(any("strings_chat_rows.xml" in c and "exclamation mark" in c for c in complaints), complaints)
        self.assertTrue(any("strings_chat_rows.xml" in c and "duplicate string key 'row_ok'" in c for c in complaints), complaints)
        self.assertFalse(any("strings_ignored.xml" in c for c in complaints), complaints)
        self.assertFalse(any("values-fr" in c for c in complaints), complaints)


if __name__ == "__main__":
    unittest.main()
