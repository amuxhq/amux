"""Check what the release recipe decides, without archiving anything.

Everything below is about the two numbers, the tag that records them and the
inputs the run refuses to proceed without — the decisions that are permanent
once a build reaches Apple. No test here builds, signs, exports or uploads.
"""

import contextlib
import datetime
import importlib.util
import io
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest
import unittest.mock

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

specification = importlib.util.spec_from_file_location(
    "release", SCRIPTS / "release.py")
recipe = importlib.util.module_from_spec(specification)
specification.loader.exec_module(recipe)

ROOT = SCRIPTS.parent


@contextlib.contextmanager
def checkout():
    """A temporary root, so this Mac's own signing file cannot decide a test."""
    with tempfile.TemporaryDirectory() as directory:
        was = os.getcwd()
        os.chdir(directory)
        try:
            yield Path(directory)
        finally:
            os.chdir(was)


@contextlib.contextmanager
def working_directory(where: Path):
    """Run inside `where`, whatever directory the tests were started from."""
    was = os.getcwd()
    os.chdir(where)
    try:
        yield where
    finally:
        os.chdir(was)


class TheBuildNumber(unittest.TestCase):
    def test_it_is_one_above_every_number_ever_issued(self):
        tags = ["ios-v1.0.32-b41", "ios-v1.0.33-b42", "ios-v1.0.31-b7"]
        self.assertEqual(43, recipe.next_build(tags, current=1))

    def test_a_repository_with_no_release_tag_refuses_to_guess_one(self):
        # App Store Connect already holds build numbers from this listing's
        # earlier Expo builds, and no tag here records them. Counting from the
        # project would offer 2 against an App Store sitting far above it.
        with self.assertRaises(recipe.Refusal) as refused:
            recipe.next_build([], current=1)
        self.assertIn("--build", str(refused.exception))
        self.assertIn("App Store Connect", str(refused.exception))

    def test_the_first_number_is_named_rather_than_derived(self):
        # The seed: read the highest number up there, pass it in. There is no
        # local ledger to check it against, which is the point.
        self.assertEqual(61, recipe.next_build([], current=1, override=61))

    def test_tags_that_are_not_the_apps_are_not_build_numbers(self):
        # Plain vX.Y.Z tags belong to the command-line release, which versions
        # separately. Reading one as a build number would move the app's
        # ledger by an unrelated act, so they leave the ledger empty.
        with self.assertRaises(recipe.Refusal):
            recipe.next_build(["v0.6.0", "v10.2.3"], current=1)

    def test_it_refuses_to_reuse_a_number(self):
        with self.assertRaises(recipe.Refusal) as refused:
            recipe.next_build(["ios-v1.0.32-b41"], current=1, override=41)
        self.assertIn("41", str(refused.exception))

    def test_it_refuses_to_go_backwards(self):
        # The reason the rule exists: App Store Connect keeps a build number
        # even for a build that was rejected or deleted, so a lowered number
        # is one that can never be used again.
        with self.assertRaises(recipe.Refusal):
            recipe.next_build(["ios-v1.0.32-b41"], current=1, override=12)

    def test_it_may_be_raised(self):
        # How the ledger is seeded above builds this repository never issued.
        self.assertEqual(900, recipe.next_build(["ios-v1.0.32-b41"],
                                                current=1, override=900))


class TheMarketingVersion(unittest.TestCase):
    def test_it_is_the_patch_after_the_highest_known_one(self):
        self.assertEqual("1.0.33", recipe.next_version(
            ["ios-v1.0.32-b41"], current="1.0.31"))

    def test_a_version_raised_in_the_project_is_honoured(self):
        self.assertEqual("1.1.1", recipe.next_version(
            ["ios-v1.0.32-b41"], current="1.1.0"))

    def test_it_refuses_a_version_below_the_highest_known_one(self):
        with self.assertRaises(recipe.Refusal) as refused:
            recipe.next_version(["ios-v1.0.32-b41"], current="1.0.31",
                                override="1.0.30")
        self.assertIn("1.0.30", str(refused.exception))

    def test_another_build_may_be_cut_under_the_version_already_here(self):
        # The App Store refuses a version that is not above the last version
        # it released, and refuses a reused build number. It does not refuse a
        # second build of a version nobody has released yet — which is exactly
        # what a first upload rejected by review needs.
        self.assertEqual("1.0.31", recipe.next_version(
            [], current="1.0.31", override="1.0.31"))
        self.assertEqual("1.0.32", recipe.next_version(
            ["ios-v1.0.32-b41"], current="1.0.31", override="1.0.32"))

    def test_a_minor_release_is_named_by_hand(self):
        self.assertEqual("1.1.0", recipe.next_version(
            ["ios-v1.0.32-b41"], current="1.0.31", override="1.1.0"))


class TheNumbersReachingTheProject(unittest.TestCase):
    """Writing both numbers is what a tag then promises the binary carries."""

    def spec(self, root: Path, text: str) -> Path:
        written = root / recipe.SPEC
        written.parent.mkdir(parents=True, exist_ok=True)
        written.write_text(text)
        return written

    @contextlib.contextmanager
    def project(self, text):
        """A checkout whose spec is `text`, with project generation stubbed.

        Generating the Xcode project is Xcode's business and needs the whole
        app; what is under test is the two substitutions."""
        with checkout() as root:
            written = self.spec(root, text)
            with unittest.mock.patch.object(recipe.ios_project, "generate"):
                yield written

    def test_both_numbers_are_substituted(self):
        with self.project('    settings:\n'
                          '      MARKETING_VERSION: "1.0.31"\n'
                          '      CURRENT_PROJECT_VERSION: "1"\n') as written:
            recipe.write_numbers("1.2.3", 44)
            self.assertIn('MARKETING_VERSION: "1.2.3"', written.read_text())
            self.assertIn('CURRENT_PROJECT_VERSION: "44"', written.read_text())
            self.assertEqual(("1.2.3", 44),
                             recipe.project_numbers(written.read_text()))

    def test_a_spec_neither_substitution_matches_is_refused(self):
        # The failure this rules out: a renamed or restructured setting leaves
        # the text untouched, the write succeeds, and the release tags numbers
        # the built app does not carry.
        text = "targets:\n  Amux:\n    type: application\n"
        with self.project(text) as written:
            with self.assertRaises(recipe.Refusal) as refused:
                recipe.write_numbers("1.2.3", 44)
            self.assertIn("MARKETING_VERSION", str(refused.exception))
            self.assertIn("CURRENT_PROJECT_VERSION", str(refused.exception))
            self.assertEqual(text, written.read_text())

    def test_half_a_spec_is_refused_too(self):
        with self.project('      MARKETING_VERSION: "1.0.31"\n') as written:
            with self.assertRaises(recipe.Refusal) as refused:
                recipe.write_numbers("1.2.3", 44)
            self.assertIn("CURRENT_PROJECT_VERSION", str(refused.exception))
            self.assertNotIn("1.2.3", written.read_text())

    def test_the_committed_spec_is_one_the_substitutions_match(self):
        with self.project((ROOT / recipe.SPEC).read_text()) as written:
            recipe.write_numbers("9.9.9", 999)
            self.assertEqual(("9.9.9", 999),
                             recipe.project_numbers(written.read_text()))


class InterruptedRelease(unittest.TestCase):
    def test_step_failures_and_timeouts_explain_recovery_without_committing(self):
        names = ("archive", "export", "validate")
        for step in names:
            for rehearse in (False, True):
                for failure, reason in (
                    (subprocess.TimeoutExpired(["xcodebuild"], 1800), "timed out"),
                    (subprocess.CalledProcessError(1, ["xcodebuild"]), "failed"),
                ):
                    with self.subTest(step=step, rehearse=rehearse, reason=reason):
                        with tempfile.TemporaryDirectory() as directory, contextlib.ExitStack() as stack:
                            stack.enter_context(unittest.mock.patch.object(
                                sys, "argv", ["release.py"] + (["--rehearse"] if rehearse else [])))
                            for name, result in (
                                ("inputs", ([], {"clean": True})),
                                ("numbers", ("1.2.3", 44)),
                                ("notes", "Release notes"),
                            ):
                                stack.enter_context(unittest.mock.patch.object(
                                    recipe, name, return_value=result))
                            write = stack.enter_context(unittest.mock.patch.object(recipe, "write_numbers"))
                            commit = stack.enter_context(unittest.mock.patch.object(recipe, "commit_and_tag"))
                            steps = {name: stack.enter_context(unittest.mock.patch.object(recipe, name))
                                     for name in names}
                            steps["export"].return_value = Path(directory) / "amux.ipa"
                            steps[step].side_effect = failure
                            error = stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
                            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                            self.assertEqual(1, recipe.main())
                            self.assertIn(f"{step} {reason}", error.getvalue())
                            commit.assert_not_called()
                            if rehearse:
                                write.assert_not_called()
                                self.assertIn("the rehearsal wrote nothing", error.getvalue())
                            else:
                                write.assert_called_once_with("1.2.3", 44)
                                self.assertIn("nothing was committed and no tag was cut", error.getvalue())
                                self.assertIn("1.2.3 (44) is written into the tree", error.getvalue())
                                self.assertIn("docs/RELEASE.md says how to undo that", error.getvalue())
                            for later in names[names.index(step) + 1:]:
                                steps[later].assert_not_called()


class TheRehearsal(unittest.TestCase):
    """The claim that a rehearsal wrote nothing is measured, not asserted.

    A rehearsal regenerates the Xcode project and drives a full archive and
    export; either could leave a tracked file changed, and a promise nobody
    checked is exactly how that would go unnoticed."""

    def test_a_tree_in_the_state_it_started_in_has_changed_nothing(self):
        self.assertEqual([], recipe.tree_changes("", ""))
        self.assertEqual([], recipe.tree_changes(" M notes/scratch.md\n",
                                                 " M notes/scratch.md\n"))

    def test_a_file_the_run_wrote_is_named(self):
        self.assertEqual([" M apps/apple/project.yml"], recipe.tree_changes(
            "", " M apps/apple/project.yml\n"))
        self.assertEqual(["?? apps/apple/Amux/Generated.swift"], recipe.tree_changes(
            " M notes/scratch.md\n",
            " M notes/scratch.md\n?? apps/apple/Amux/Generated.swift\n"))

    def test_the_claim_and_the_refusal_both_come_from_that_comparison(self):
        source = (SCRIPTS / "release.py").read_text()
        self.assertIn("changed = tree_changes(facts[\"tree\"], tree())", source)
        self.assertIn("rehearsal changed the tree, which it must not", source)


class TheTag(unittest.TestCase):
    def test_it_carries_both_numbers(self):
        self.assertEqual("ios-v1.0.32-b41", recipe.tag_name("1.0.32", 41))


class TheProject(unittest.TestCase):
    def test_both_numbers_live_in_one_place(self):
        version, build = recipe.project_numbers((ROOT / recipe.SPEC).read_text())
        self.assertRegex(version, r"^\d+\.\d+\.\d+$")
        self.assertGreaterEqual(build, 1)

    def test_the_bundle_reads_them_from_the_build_settings(self):
        # What lets a rehearsal archive the would-be release without touching
        # the tree: both numbers can be passed to xcodebuild instead.
        plist = (ROOT / "apps/apple/Amux/Info.plist").read_text()
        self.assertIn("$(MARKETING_VERSION)", plist)
        self.assertIn("$(CURRENT_PROJECT_VERSION)", plist)


class TheSigningFile(unittest.TestCase):
    def test_a_missing_file_leaves_no_team(self):
        with checkout():
            self.assertEqual("", recipe.team())

    def test_the_one_assignment_is_read(self):
        with checkout() as root:
            written = root / recipe.SIGNING_FILE
            written.parent.mkdir(parents=True, exist_ok=True)
            written.write_text("// this Mac's team\nDEVELOPMENT_TEAM = ABCDE12345\n")
            self.assertEqual("ABCDE12345", recipe.team())

    def test_a_file_setting_something_else_is_not_a_team(self):
        with checkout() as root:
            written = root / recipe.SIGNING_FILE
            written.parent.mkdir(parents=True, exist_ok=True)
            written.write_text("CODE_SIGN_STYLE = Automatic\n")
            self.assertEqual("", recipe.team())

    def test_without_it_the_run_says_which_piece_is_missing(self):
        # The keychain and the installed profiles are this Mac's, and Linux
        # has neither to ask.
        with checkout(), \
                unittest.mock.patch.object(recipe, "keychain", lambda _: ""), \
                unittest.mock.patch.object(recipe, "installed_profiles", lambda: []):
            checks, facts = recipe.inputs()
            team = [check for check in checks if check.what == "a Team ID"][0]
            self.assertFalse(team.held)
            self.assertIn(str(recipe.SIGNING_FILE), team.detail)
            self.assertIn(recipe.TEAM_SETTING, team.detail)
            self.assertEqual("", facts["team"])

    def test_the_repository_ignores_it(self):
        self.assertEqual(0, os.system(
            f"git -C {ROOT} check-ignore -q {recipe.SIGNING_FILE}"))


class TheExportOptions(unittest.TestCase):
    def test_the_committed_file_names_no_team(self):
        options = plistlib.loads((ROOT / recipe.EXPORT_OPTIONS).read_bytes())
        self.assertNotIn("teamID", options)

    def test_it_exports_rather_than_uploads(self):
        options = plistlib.loads((ROOT / recipe.EXPORT_OPTIONS).read_bytes())
        self.assertEqual("export", options["destination"])
        # Xcode would otherwise be free to rewrite the number that was tagged.
        self.assertFalse(options["manageAppVersionAndBuildNumber"])

    def test_the_team_is_inserted_at_run_time(self):
        with checkout() as root:
            (root / "apps/apple").mkdir(parents=True)
            (root / recipe.EXPORT_OPTIONS).write_bytes(
                (ROOT / recipe.EXPORT_OPTIONS).read_bytes())
            written = recipe.export_options("ABCDE12345")
            self.assertEqual(
                "ABCDE12345", plistlib.loads(written.read_bytes())["teamID"])


    def test_it_signs_by_hand_because_cloud_signing_cannot(self):
        # Xcode's cloud signing refuses to fetch a profile for this account
        # from a script, so the export names what it signs with.
        options = plistlib.loads((ROOT / recipe.EXPORT_OPTIONS).read_bytes())
        self.assertEqual("manual", options["signingStyle"])
        self.assertEqual("sh.amux.app",
                         next(iter(options["provisioningProfiles"])))

    def test_the_named_certificate_carries_no_account_identifier(self):
        # A full identity name ends in "(<Team ID>)" and this file is
        # committed, so the type is named and teamID picks the identity.
        options = plistlib.loads((ROOT / recipe.EXPORT_OPTIONS).read_bytes())
        self.assertEqual(recipe.DISTRIBUTION, options["signingCertificate"])
        self.assertNotIn("(", options["signingCertificate"])


class TheDistributionCertificate(unittest.TestCase):
    def test_it_is_the_one_belonging_to_this_team(self):
        # Two accounts' certificates can sit in one keychain and only the
        # Team ID in the parentheses tells them apart.
        found = recipe.distribution_identity([
            "Apple Development: Jordan Webster (AAAAAAAAAA)",
            "Apple Distribution: Someone Else (BBBBBBBBBB)",
            "Apple Distribution: Jordan Webster (CCCCCCCCCC)",
        ], "CCCCCCCCCC")
        self.assertEqual("Apple Distribution: Jordan Webster (CCCCCCCCCC)", found)

    def test_a_development_identity_is_not_one(self):
        self.assertEqual("", recipe.distribution_identity(
            ["Apple Development: Jordan Webster (CCCCCCCCCC)"], "CCCCCCCCCC"))

    def test_an_empty_keychain_leaves_no_certificate(self):
        self.assertEqual("", recipe.distribution_identity([], "CCCCCCCCCC"))


class TheProvisioningProfile(unittest.TestCase):
    def profile(self, name, days=0, hours=0):
        # plistlib decodes a plist date to a naive datetime holding UTC, so a
        # fixture is built the same way a real profile reads.
        now = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None)
        return {"Name": name,
                "ExpirationDate": now + datetime.timedelta(days=days,
                                                           hours=hours)}

    def test_the_export_names_one_per_bundle_id(self):
        with checkout() as root:
            (root / "apps/apple").mkdir(parents=True)
            (root / recipe.EXPORT_OPTIONS).write_bytes(
                (ROOT / recipe.EXPORT_OPTIONS).read_bytes())
            self.assertEqual({"sh.amux.app": "amux App Store"},
                             recipe.wanted_profiles())

    def test_the_one_with_that_name_is_chosen(self):
        profiles = [self.profile("something else", 30),
                    self.profile("amux App Store", 30)]
        self.assertEqual("amux App Store",
                         recipe.usable(profiles, "amux App Store")["Name"])

    def test_an_expired_profile_counts_as_absent(self):
        # Exporting with an expired profile fails the same way exporting
        # without one does, so the run should name the same missing piece.
        profiles = [self.profile("amux App Store", -1)]
        self.assertEqual({}, recipe.usable(profiles, "amux App Store"))

    def test_the_last_hours_of_a_profile_are_read_in_utc(self):
        # The failure this rules out: comparing a UTC expiry with local time
        # reads a profile as good for as many hours as this Mac is behind
        # UTC after it has actually expired, and the export then fails in
        # Xcode instead of in the preflight that exists to catch it.
        self.assertEqual("amux App Store", recipe.usable(
            [self.profile("amux App Store", hours=2)], "amux App Store")["Name"])
        self.assertEqual({}, recipe.usable(
            [self.profile("amux App Store", hours=-2)], "amux App Store"))

    def test_a_profile_with_no_expiry_is_not_trusted(self):
        self.assertEqual({}, recipe.usable([{"Name": "amux App Store"}],
                                           "amux App Store"))


class TheLastStep(unittest.TestCase):
    def test_the_run_ends_by_asking_apple(self):
        # Validation is the recipe's last step rather than something run by
        # hand afterwards, so that one command is the whole release.
        source = (SCRIPTS / "release.py").read_text()
        self.assertIn("--validate-app", source)
        self.assertIn("validate(exported, facts)", source)

    def test_validation_is_authenticated_by_the_key_and_nothing_else(self):
        # No Apple Account, no password, no 2FA: the two identifiers come
        # from the keychain and altool finds the .p8 itself.
        source = (SCRIPTS / "release.py").read_text()
        self.assertIn('"--api-key", facts["key"]', source)
        self.assertIn('"--api-issuer", facts["issuer"]', source)


class TheOrderOfARelease(unittest.TestCase):
    """Nothing permanent is recorded until Apple has accepted the build.

    A commit and an annotated tag are the only things a run leaves that a
    `git checkout` cannot undo, so they come after the archive, the export and
    the validation. Every step is stubbed here: what is under test is the
    order they are called in and what a failure leaves behind."""

    def drive(self, breaks="", argv=("release.py",),
              real_numbers=False) -> tuple[int, list[str]]:
        """Run main() with every real step replaced by a recorder.

        `breaks` names a step that fails the way xcodebuild or altool does;
        `real_numbers` lets the run choose its own numbers from this
        checkout's spec and tags instead of being handed a pair."""
        called = []

        def step(name, result=None):
            def record(*arguments, **keywords):
                called.append(name)
                if name == breaks:
                    raise subprocess.CalledProcessError(65, [name])
                return result
            return record

        with tempfile.TemporaryDirectory() as directory:
            package = Path(directory) / "Amux.ipa"
            package.write_bytes(b"")
            facts = {"clean": True, "tree": "", "team": "ABCDE12345",
                     "key": "KEY", "issuer": "ISSUER",
                     "key_path": Path(directory) / "AuthKey_KEY.p8"}
            patches = {
                "inputs": lambda: ([recipe.Check("a clean tree", True, "clean")],
                                   facts),
                "notes": lambda *_: "amux 1.0.32",
                "tree": lambda: "",
                "write_numbers": step("write_numbers"),
                "archive": step("archive"),
                "export": step("export", package),
                "validate": step("validate"),
                "upload": step("upload"),
                "commit_and_tag": step("commit_and_tag"),
                "publish": step("publish"),
            }
            if not real_numbers:
                patches["numbers"] = lambda *_: ("1.0.32", 41)
            with contextlib.ExitStack() as stack:
                stack.enter_context(working_directory(ROOT))
                # The run reports itself to the console; a test suite is not
                # its reader, and its stderr would land inside unittest's own.
                stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
                for name, replacement in patches.items():
                    stack.enter_context(
                        unittest.mock.patch.object(recipe, name, replacement))
                stack.enter_context(
                    unittest.mock.patch.object(sys, "argv", list(argv)))
                return recipe.main(), called

    def test_the_tag_is_cut_after_apple_has_answered(self):
        # Upload sits between the two for a reason: a build Apple would refuse
        # never reaches it, and the tag records a build that actually arrived.
        code, called = self.drive()
        self.assertEqual(0, code)
        self.assertEqual(["write_numbers", "archive", "export", "validate",
                          "upload", "commit_and_tag", "publish"], called)

    def test_an_upload_failure_leaves_no_commit_and_no_tag(self):
        # The build number is spent either way, but the tree should not claim
        # a release that never arrived.
        code, called = self.drive(breaks="upload")
        self.assertEqual(1, code)
        self.assertNotIn("commit_and_tag", called)

    def test_no_upload_stops_where_every_run_used_to(self):
        code, called = self.drive(argv=("release.py", "--no-upload"))
        self.assertEqual(0, code)
        self.assertNotIn("upload", called)
        self.assertIn("commit_and_tag", called)

    def test_no_push_keeps_the_ledger_at_home(self):
        code, called = self.drive(argv=("release.py", "--no-push"))
        self.assertEqual(0, code)
        self.assertIn("commit_and_tag", called)
        self.assertNotIn("publish", called)

    def test_a_validation_failure_leaves_no_commit_and_no_tag(self):
        # The state this ordering exists for: the numbers are in the working
        # tree, where one git checkout undoes them, and nothing else happened.
        code, called = self.drive(breaks="validate")
        self.assertEqual(1, code)
        self.assertNotIn("commit_and_tag", called)

    def test_a_failed_archive_stops_before_the_export(self):
        code, called = self.drive(breaks="archive")
        self.assertEqual(1, code)
        self.assertEqual(["write_numbers", "archive"], called)

    def test_a_rehearsal_writes_nothing_and_records_nothing(self):
        code, called = self.drive(argv=("release.py", "--rehearse"))
        self.assertEqual(0, code)
        self.assertEqual(["archive", "export", "validate"], called)

    def test_a_rehearsal_runs_though_no_tag_records_a_build_number(self):
        # This checkout has issued no ios-v* tag, so a release refuses its
        # first build number. A rehearsal issues nothing and spends nothing,
        # so it stands in the project's own number and proves the signing
        # path anyway — the numbers here are the checkout's real ones.
        code, called = self.drive(argv=("release.py", "--rehearse"),
                                  real_numbers=True)
        self.assertEqual(0, code)
        # Stops at the validation, which is what keeps a rehearsal free to run
        # as often as anybody likes: an upload would spend a build number
        # permanently and show the build to the whole team.
        self.assertEqual(["archive", "export", "validate"], called)


class WhatItNeverDoes(unittest.TestCase):
    def test_the_recipe_never_submits_for_review(self):
        # Uploading and pushing are both part of a release now. Submitting is
        # not: that is a person's act in App Store Connect, with the
        # screenshots, the notes and the reviewers already decided. These are
        # the names the two ways of doing it go by.
        source = (SCRIPTS / "release.py").read_text()
        self.assertNotIn("--notarize-app", source)
        self.assertNotIn("appStoreVersionSubmissions", source)
        self.assertNotIn("betaAppReviewSubmissions", source)


if __name__ == "__main__":
    unittest.main()
