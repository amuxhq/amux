"""Check what an iPhone deploy changes in App Store Connect, against a fake
one: the build it picks, what it attaches and submits on stable, and what it
gives TestFlight on preview. Nothing here reaches Apple."""

import importlib.util
import json
from pathlib import Path
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
specification = importlib.util.spec_from_file_location("ios_deploy", SCRIPTS / "ios-deploy.py")
recipe = importlib.util.module_from_spec(specification)
specification.loader.exec_module(recipe)

API = "https://api.appstoreconnect.apple.com"


class Fake:
    """App Store Connect with one app, one processed build and the calls a
    deploy makes, keeping every write in order."""

    def __init__(self, versions=(), whats_new="Faster sign-in.", groups=(("Friends", False),),
                 beta_state="READY_FOR_BETA_SUBMISSION", phased=True, build_state="VALID",
                 open_submission=None):
        self.versions = [{"id": f"v{i}", "type": "appStoreVersions",
                          "attributes": {"versionString": number, "appStoreState": state}}
                         for i, (number, state) in enumerate(versions)]
        self.whats_new = whats_new
        self.groups = [{"id": f"g{i}", "type": "betaGroups",
                        "attributes": {"name": name, "isInternalGroup": internal}}
                       for i, (name, internal) in enumerate(groups)]
        self.beta_state = beta_state
        self.phased = phased
        self.build_state = build_state
        # (state, version ids it holds) of a submission not yet accepted.
        self.open_submission = open_submission
        self.writes = []

    def __call__(self, method, url, headers, body):
        path = url[len(API):]
        route = path.split("?")[0]
        data = json.loads(body) if body else None
        if method != "GET":
            self.writes.append((method, route, data))

        def answer(payload, status=200):
            return status, json.dumps(payload).encode()

        if method == "GET" and route == "/v1/apps":
            return answer({"data": [{"id": "app", "type": "apps"}]})
        if method == "GET" and route == "/v1/builds":
            assert "filter[version]=41" in path and "filter[preReleaseVersion.version]=1.0.34" in path, path
            return answer({"data": [{"id": "b41", "type": "builds",
                                     "attributes": {"processingState": self.build_state,
                                                    "expired": False}}]})
        if method == "GET" and route == "/v1/apps/app/appStoreVersions":
            assert "filter[versionString]=1.0.34" in path, path
            return answer({"data": self.versions})
        if method == "POST" and route == "/v1/appStoreVersions":
            return answer({"data": {"id": "vnew", "type": "appStoreVersions"}}, 201)
        if method == "GET" and route.endswith("/appStoreVersionLocalizations"):
            return answer({"data": [{"id": "loc", "type": "appStoreVersionLocalizations",
                                     "attributes": {"locale": "en-US", "whatsNew": self.whats_new}}]})
        if method == "GET" and route.endswith("/appStoreVersionPhasedRelease"):
            return answer({"data": {"id": "ph", "type": "appStoreVersionPhasedReleases"}
                           if self.phased else None})
        if method == "GET" and route == "/v1/reviewSubmissions":
            assert "filter[state]=READY_FOR_REVIEW,UNRESOLVED_ISSUES" in path, path
            if not self.open_submission:
                return answer({"data": []})
            state, _ = self.open_submission
            return answer({"data": [{"id": "open", "type": "reviewSubmissions",
                                     "attributes": {"state": state}}]})
        if method == "GET" and route == "/v1/reviewSubmissions/open/items":
            _, held = self.open_submission
            return answer({"data": [{"id": f"i{v}", "type": "reviewSubmissionItems", "relationships": {
                "appStoreVersion": {"data": {"type": "appStoreVersions", "id": v}}}} for v in held]})
        if method == "POST" and route == "/v1/reviewSubmissions":
            return answer({"data": {"id": "rs", "type": "reviewSubmissions"}}, 201)
        if method == "GET" and route == "/v1/apps/app/betaGroups":
            return answer({"data": self.groups})
        if method == "GET" and route == "/v1/builds/b41/buildBetaDetail":
            return answer({"data": {"attributes": {"externalBuildState": self.beta_state}}})
        if method == "GET" and route == "/v1/builds/b41/betaBuildLocalizations":
            return answer({"data": []})
        if method in ("PATCH", "POST", "DELETE"):
            return answer({"data": {"id": "x"}}, 200 if method == "PATCH" else 201)
        raise AssertionError(f"unexpected {method} {path}")


def run(fake, channel="stable", notes=None, group=None, dry_run=False):
    said = []
    recipe.deploy(recipe.Connect("t", send=fake), "1.0.34", 41, channel, notes, group,
                  dry_run, said.append)
    return said


class Builds(unittest.TestCase):
    def test_the_build_is_the_highest_the_tags_record_for_the_version(self):
        tags = ["ios-v1.0.33-b41", "ios-v1.0.34-b42", "ios-v1.0.34-b44", "v0.8.5"]
        self.assertEqual(recipe.tagged_build(tags, "1.0.34"), 44)
        with self.assertRaises(recipe.Refusal):
            recipe.tagged_build(tags, "1.0.35")

    def test_a_build_apple_has_not_processed_is_refused(self):
        with self.assertRaisesRegex(recipe.Refusal, "PROCESSING"):
            run(Fake(build_state="PROCESSING"))


class Stable(unittest.TestCase):
    def test_a_new_version_is_created_attached_and_submitted_without_a_phased_release(self):
        fake = Fake()
        said = run(fake, notes="Faster sign-in.")
        routes = [(method, route) for method, route, _ in fake.writes]
        self.assertEqual(routes, [
            ("POST", "/v1/appStoreVersions"),
            ("PATCH", "/v1/appStoreVersions/vnew"),
            ("PATCH", "/v1/appStoreVersions/vnew/relationships/build"),
            ("DELETE", "/v1/appStoreVersionPhasedReleases/ph"),
            ("PATCH", "/v1/appStoreVersionLocalizations/loc"),
            ("POST", "/v1/reviewSubmissions"),
            ("POST", "/v1/reviewSubmissionItems"),
            ("PATCH", "/v1/reviewSubmissions/rs"),
        ])
        attached = fake.writes[2][2]
        self.assertEqual(attached, {"data": {"type": "builds", "id": "b41"}})
        self.assertEqual(fake.writes[1][2]["data"]["attributes"], {"releaseType": "AFTER_APPROVAL"})
        self.assertEqual(fake.writes[-1][2]["data"]["attributes"], {"submitted": True})
        self.assertIn("submitted version 1.0.34 for review", said[-1])

    def test_a_new_version_without_notes_is_refused_before_it_is_made(self):
        fake = Fake()
        with self.assertRaisesRegex(recipe.Refusal, "--notes-file"):
            run(fake)
        self.assertEqual(fake.writes, [])

    def test_a_rejected_versions_submission_is_sent_again_without_adding_it_twice(self):
        fake = Fake(versions=(("1.0.34", "REJECTED"),), phased=False,
                    open_submission=("UNRESOLVED_ISSUES", ["v0"]))
        run(fake)
        routes = [(method, route) for method, route, _ in fake.writes]
        self.assertNotIn(("POST", "/v1/reviewSubmissions"), routes)
        self.assertNotIn(("POST", "/v1/reviewSubmissionItems"), routes)
        self.assertEqual(routes[-1], ("PATCH", "/v1/reviewSubmissions/open"))

    def test_an_unsent_submission_gets_the_version_added(self):
        fake = Fake(versions=(("1.0.34", "PREPARE_FOR_SUBMISSION"),), phased=False,
                    open_submission=("READY_FOR_REVIEW", []))
        run(fake)
        added = [data for method, route, data in fake.writes if route == "/v1/reviewSubmissionItems"]
        self.assertEqual(added[0]["data"]["relationships"]["reviewSubmission"]["data"]["id"], "open")

    def test_notes_become_whats_new(self):
        fake = Fake(versions=(("1.0.34", "PREPARE_FOR_SUBMISSION"),), phased=False)
        run(fake, notes="Pictures on every profile.")
        patched = [data for method, route, data in fake.writes
                   if route == "/v1/appStoreVersionLocalizations/loc"]
        self.assertEqual(patched[0]["data"]["attributes"], {"whatsNew": "Pictures on every profile."})

    def test_a_listing_without_whats_new_is_refused_before_anything_changes(self):
        fake = Fake(versions=(("1.0.34", "PREPARE_FOR_SUBMISSION"),), whats_new="")
        with self.assertRaisesRegex(recipe.Refusal, "What's New"):
            run(fake)
        self.assertEqual(fake.writes, [])

    def test_a_version_already_in_review_is_refused(self):
        fake = Fake(versions=(("1.0.34", "WAITING_FOR_REVIEW"),))
        with self.assertRaisesRegex(recipe.Refusal, "WAITING_FOR_REVIEW"):
            run(fake)
        self.assertEqual(fake.writes, [])

    def test_a_dry_run_changes_nothing(self):
        fake = Fake(versions=(("1.0.34", "PREPARE_FOR_SUBMISSION"),))
        said = run(fake, dry_run=True)
        self.assertEqual(fake.writes, [])
        self.assertIn("would attach the build", said[-1])


class Preview(unittest.TestCase):
    def test_the_build_goes_to_the_external_group_and_to_beta_review(self):
        fake = Fake(groups=(("Team", True), ("Friends", False)))
        run(fake, channel="preview", notes="Try the Mac app.")
        routes = [(method, route) for method, route, _ in fake.writes]
        self.assertEqual(routes, [
            ("POST", "/v1/betaBuildLocalizations"),
            ("POST", "/v1/betaGroups/g1/relationships/builds"),
            ("POST", "/v1/betaAppReviewSubmissions"),
        ])

    def test_a_build_beta_review_already_passed_is_not_submitted_again(self):
        fake = Fake(beta_state="BETA_APPROVED")
        run(fake, channel="preview")
        self.assertEqual([route for _, route, _ in fake.writes], ["/v1/betaGroups/g0/relationships/builds"])

    def test_several_external_groups_need_one_named(self):
        fake = Fake(groups=(("Friends", False), ("Press", False)))
        with self.assertRaisesRegex(recipe.Refusal, "--group"):
            run(fake, channel="preview")
        run(fake, channel="preview", group="Press")
        self.assertIn("/v1/betaGroups/g1/relationships/builds", [route for _, route, _ in fake.writes])


if __name__ == "__main__":
    unittest.main()
