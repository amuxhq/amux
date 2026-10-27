"""Check what the sandbox-purchase recipe decides, without a phone or a store.

Everything below is about which account the recipe would use and what it says
is missing — the two things that have to be right before anybody plugs a phone
in. No test here signs in, builds, installs or buys.
"""

import contextlib
import importlib.util
import io
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

specification = importlib.util.spec_from_file_location(
    "qa_sandbox_purchase", SCRIPTS / "qa-sandbox-purchase.py")
recipe = importlib.util.module_from_spec(specification)
specification.loader.exec_module(recipe)


@contextlib.contextmanager
def checkout(**variables):
    """A temporary repository root and a clean environment, so a real
    .autopilot/qa-account.env on this Mac cannot decide a test."""
    with tempfile.TemporaryDirectory() as directory:
        was = os.getcwd()
        kept = {name: os.environ.pop(name, None)
                for name in (recipe.ADDRESS_VARIABLE,
                             recipe.OTHER_ACCOUNT_VARIABLE)}
        os.environ.update({k: v for k, v in variables.items() if v})
        os.chdir(directory)
        try:
            yield Path(directory)
        finally:
            os.chdir(was)
            for name in (recipe.ADDRESS_VARIABLE, recipe.OTHER_ACCOUNT_VARIABLE):
                os.environ.pop(name, None)
                if kept[name] is not None:
                    os.environ[name] = kept[name]


def address_file(root: Path, text: str) -> None:
    written = root / recipe.qa_cloud.ADDRESS_FILE
    written.parent.mkdir(parents=True, exist_ok=True)
    written.write_text(text)


class TheAccount(unittest.TestCase):
    def test_the_environment_names_the_device_account(self):
        with checkout(**{recipe.ADDRESS_VARIABLE: "someone@example.test"}):
            who, check = recipe.device_address()
            self.assertEqual(who, "someone@example.test")
            self.assertTrue(check.held)
            self.assertNotIn("someone@example.test", check.detail)

    def test_the_operators_file_names_it_when_the_environment_does_not(self):
        with checkout() as root:
            address_file(root, f"{recipe.ADDRESS_VARIABLE}=device@example.test\n")
            who, check = recipe.device_address()
            self.assertEqual(who, "device@example.test")
            self.assertTrue(check.held)

    def test_a_file_with_only_the_end_to_end_account_leaves_it_missing(self):
        with checkout() as root:
            address_file(root, f"{recipe.OTHER_ACCOUNT_VARIABLE}=other@example.test\n")
            who, check = recipe.device_address()
            self.assertEqual(who, "")
            self.assertFalse(check.held)
            # The report has to say where to put an address, or the person
            # reading it has to go find this file to learn what to do.
            self.assertIn(recipe.ADDRESS_VARIABLE, check.detail)
            self.assertIn(str(recipe.qa_cloud.ADDRESS_FILE), check.detail)

    def test_the_end_to_end_account_is_refused_even_when_named(self):
        with checkout(**{recipe.ADDRESS_VARIABLE: "shared@example.test",
                         recipe.OTHER_ACCOUNT_VARIABLE: "shared@example.test"}):
            who, check = recipe.device_address()
            self.assertEqual(who, "")
            self.assertFalse(check.held)
            self.assertIn(recipe.OTHER_ACCOUNT_VARIABLE, check.detail)

    def test_no_address_is_built_in(self):
        source = (SCRIPTS / "qa-sandbox-purchase.py").read_text()
        self.assertNotIn("@amux.sh", source)
        self.assertNotIn("@gmail", source)


class TheSigningFile(unittest.TestCase):
    def test_a_missing_file_says_what_to_write_and_where(self):
        with checkout():
            settings, check = recipe.signing_check()
            self.assertEqual(settings, {})
            self.assertFalse(check.held)
            self.assertIn(str(recipe.SIGNING_FILE), check.detail)
            self.assertIn("DEVELOPMENT_TEAM", check.detail)

    def test_a_file_without_the_team_id_names_it(self):
        with checkout() as root:
            written = root / recipe.SIGNING_FILE
            written.parent.mkdir(parents=True, exist_ok=True)
            written.write_text("// signing\nCODE_SIGN_STYLE = Automatic\n")
            settings, check = recipe.signing_check()
            self.assertEqual(settings["CODE_SIGN_STYLE"], "Automatic")
            self.assertFalse(check.held)
            self.assertIn("DEVELOPMENT_TEAM", check.detail)

    def test_the_bundle_id_comes_from_the_project_and_not_this_file(self):
        # The app's identity is committed, because this build is the next
        # version of the listing already on the App Store. A local file that
        # could change it would sign a different app, and its subscriptions
        # would not exist.
        spec = (SCRIPTS.parent / "apps/apple/project.yml").read_text()
        self.assertIn(f"PRODUCT_BUNDLE_IDENTIFIER: {recipe.BUNDLE_ID}", spec)
        self.assertNotIn("PRODUCT_BUNDLE_IDENTIFIER", recipe.REQUIRED_SETTINGS)

    def test_the_repository_ignores_the_signing_file(self):
        # Checked against this checkout's own rules, not a temporary one: the
        # file names a signing identity and must never become a commit.
        self.assertEqual(0, os.system(
            f"git -C {SCRIPTS.parent} check-ignore -q {recipe.SIGNING_FILE}"))


class ThePhone(unittest.TestCase):
    def test_a_paired_but_unreachable_phone_is_not_a_connected_one(self):
        self.assertFalse(recipe.connected({"connectionProperties": {
            "pairingState": "paired", "tunnelState": "disconnected"}}))

    def test_a_reachable_phone_is(self):
        self.assertTrue(recipe.connected({"connectionProperties": {
            "pairingState": "paired", "tunnelState": "connected"}}))


class TheReport(unittest.TestCase):
    def read(self, checks) -> tuple[bool, str]:
        page = io.StringIO()
        with contextlib.redirect_stdout(page):
            satisfied = recipe.report(checks)
        return satisfied, page.getvalue()

    def test_a_missing_fact_is_named_in_the_summary(self):
        satisfied, page = self.read([
            recipe.Check("a connected phone", False, "none here"),
            recipe.Check("the account's password", True, "in the keychain")])
        self.assertFalse(satisfied)
        self.assertIn("MISSING", page)
        self.assertIn("a connected phone", page.splitlines()[-1])

    def test_a_fact_only_a_person_can_answer_reads_as_confirm(self):
        _, page = self.read([recipe.sandbox_account_check("a@example.test", False)])
        self.assertIn("[confirm]", page)
        self.assertIn("--confirmed", page)
        self.assertNotIn("a@example.test", page)

    def test_everything_present_says_so(self):
        satisfied, page = self.read([recipe.Check("a phone", True, "here")])
        self.assertTrue(satisfied)
        self.assertIn("everything a sandbox purchase needs is here", page)


class TheTransactionFileMode(unittest.TestCase):
    def test_it_asks_for_no_phone_and_no_signing(self):
        # A transaction captured on a phone has already been bought; demanding
        # the phone again would refuse a run that can honestly succeed.
        # The keychain is this Mac's, and Linux has none to ask.
        with checkout(**{recipe.ADDRESS_VARIABLE: "device@example.test"}), \
                mock.patch.object(recipe.qa_cloud, "keychain_holds", lambda _: False):
            _, phone, settings, checks = recipe.preflight(
                confirmed=False, for_transaction_file=True)
            self.assertIsNone(phone)
            self.assertEqual(settings, {})
            self.assertEqual([check.what for check in checks],
                             ["the device QA account's address",
                              "the account's password"])


if __name__ == "__main__":
    unittest.main()
