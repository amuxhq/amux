"""Failure probes for the copy review boundary; run by strings-lint.sh."""

import json
from pathlib import Path
import tempfile
import unittest

import strings_lint as lint


class SwiftLiterals(unittest.TestCase):
    def test_comments_do_not_hide_following_copy(self):
        source = '/* "ignored" /* nested */ */ // "ignored too"\nText("Read Me")'
        self.assertEqual([item.key for item in lint.literals(source)], ["Read Me"])

    def test_interpolation_and_fallback_words_are_both_reviewed(self):
        source = r'Text("Hello \(name ?? "friend"), \(count) hosts")'
        self.assertCountEqual([item.key for item in lint.literals(source)],
                              ["friend", "Hello %@, %@ hosts"])

    def test_raw_multiline_and_unicode_are_not_escape_hatches(self):
        source = '''let words = #"""
    Pair with \\#(name ?? "host")?
    Second line
    """#
    let caption = "Hosts \\u{00B7} 3"
    '''
        self.assertCountEqual([item.key for item in lint.literals(source)],
                              ["host", "Pair with %@?\nSecond line", "Hosts · 3"])

    def test_escaped_quotes_and_nested_calls_do_not_end_interpolation(self):
        source = r'Text("A \(name.replacingOccurrences(of: "\"", with: "quote")) Z")'
        self.assertCountEqual([item.key for item in lint.literals(source)],
                              ['"', 'quote', 'A %@ Z'])

    def test_unterminated_input_is_refused(self):
        for source in ['Text("unfinished)', '/* unfinished', r'Text("\(name)']:
            with self.subTest(source=source), self.assertRaises(ValueError):
                lint.literals(source)


class InventoryBoundary(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.write_catalogue(lint.CATALOGUE, ["Pair", "Hello %@"])
        self.write_catalogue(lint.DEBUG_CATALOGUE, ["Report"])
        self.write(lint.EXEMPTIONS, json.dumps({"version": 1, "files": {}}))
        self.source = Path("apps/apple/Packages/AmuxFeatures/Sources/Feature.swift")
        self.write(self.source, 'Text("Pair")')
        self.uses = Path("apps/apple/Packages/AmuxFeatures/Sources/Uses.swift")
        self.write(self.uses, r'Text("Pair"); Text("Hello \(name)")')
        self.debug_uses = Path("apps/apple/Amux/Debug/Uses.swift")
        self.write(self.debug_uses, 'Text("Report")')

    def write(self, path, text):
        file = self.root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(text)

    def write_catalogue(self, path, keys):
        self.write(path, json.dumps({"sourceLanguage": "en", "version": "1.0", "strings": {
            key: {"comment": "Test copy", "localizations": {"en": {
                "stringUnit": {"state": "translated", "value": key}}}} for key in keys}}))

    def test_reviewed_copy_passes(self):
        self.assertEqual(lint.check(self.root)[0], [])

    def test_view_helper_new_package_and_debug_copy_fail_when_missing(self):
        cases = [
            (self.source, 'Text("Missing view copy")'),
            (Path("apps/apple/Packages/AmuxCore/Sources/Store.swift"),
             'var failure: String { "Missing helper copy" }'),
            (Path("apps/apple/Packages/Future/Sources/New.swift"), 'let label = #"Raw copy"#'),
            (Path("apps/apple/Amux/Debug/Report.swift"), 'Text("Missing debug copy")'),
            (Path("apps/apple/Packages/AmuxTestSupport/Sources/Report.swift"),
             'let label = """\nMultiline copy\n"""'),
            (self.source, r'Text("Hello \(name ?? "missing fallback")")'),
        ]
        for path, source in cases:
            with self.subTest(path=path, source=source):
                self.write(path, source)
                complaints, _ = lint.check(self.root)
                self.assertTrue(any("uncatalogued literal" in issue for issue in complaints))
                (self.root / path).unlink()

    def test_catalogued_copy_no_literal_uses_is_reported(self):
        (self.root / self.uses).unlink()
        self.write(self.source, 'Text("Pair")')
        self.assertEqual(lint.check(self.root)[0], [
            f"{lint.CATALOGUE}: no literal uses the catalogued copy 'Hello %@'"])
        # Debug copy is used only from a debug source.
        (self.root / self.debug_uses).unlink()
        self.write(self.source, 'Text("Pair"); Text("Hello \\(name)")')
        self.assertEqual(lint.check(self.root)[0], [
            f"{lint.DEBUG_CATALOGUE}: no literal uses the catalogued copy 'Report'"])

    def test_debug_catalogue_cannot_authorize_shipping_copy(self):
        self.write(self.source, 'Text("Report")')
        self.assertTrue(lint.check(self.root)[0])
        (self.root / self.source).unlink()
        self.write(Path("apps/apple/Amux/Debug/Report.swift"), 'Text("Report")')
        self.assertEqual(lint.check(self.root)[0], [])

    def test_exemption_is_bound_to_exact_source_and_file(self):
        source = 'let wireKey = "operation"'
        item = lint.literals(source)[0]
        self.write(lint.EXEMPTIONS, json.dumps({"version": 1, "files": {
            str(self.source): [{"literal": item.source, "context": item.context,
                                "count": 1, "reason": "Wire protocol key"}]}}))
        self.write(self.source, source)
        self.assertEqual(lint.check(self.root)[0], [])
        self.write(self.source, source + '\nText("operation")')
        self.assertTrue(lint.check(self.root)[0])
        self.write(self.source, source)
        self.write(Path("apps/apple/Packages/AmuxCore/Sources/Other.swift"), source)
        self.assertTrue(lint.check(self.root)[0])
        (self.root / "apps/apple/Packages/AmuxCore/Sources/Other.swift").unlink()
        self.write(self.source, 'Text("Pair")')
        self.assertTrue(any("stale" in issue for issue in lint.check(self.root)[0]))

    def test_missing_reason_or_english_value_is_refused(self):
        self.write(lint.EXEMPTIONS, json.dumps({"version": 1, "files": {
            str(self.source): [{"literal": '"wire"', "context": '"wire"', "count": 1}]}}))
        with self.assertRaisesRegex(ValueError, "no reason"):
            lint.check(self.root)
        self.write(lint.CATALOGUE, json.dumps({"sourceLanguage": "en", "version": "1.0",
                                              "strings": {"Pair": {"comment": "A button"}}}))
        with self.assertRaisesRegex(ValueError, "English value"):
            lint.check(self.root)

    def test_forbidden_copy_punctuation_is_refused(self):
        for key in ["Offline — retry", "Wait...", "Done!", "Failed; retry", "Host's state"]:
            with self.subTest(key=key):
                self.write_catalogue(lint.CATALOGUE, [key])
                with self.assertRaisesRegex(ValueError, "punctuation"):
                    lint.check(self.root)

    def test_duplicate_catalogue_keys_are_refused(self):
        self.write(lint.CATALOGUE, '{"strings": {}, "strings": {}}')
        with self.assertRaisesRegex(ValueError, "duplicate key"):
            lint.check(self.root)


if __name__ == "__main__":
    unittest.main()
