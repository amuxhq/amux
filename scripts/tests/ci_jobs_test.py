"""Which gated CI jobs a change runs (scripts/ci_jobs.py over build/ci-jobs.toml):
the globs, the precedence of the tables, the two files read rather than
matched, and that every file the checkout tracks is classified."""

import importlib.util
import subprocess
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
ROOT = SCRIPTS.parent


def load(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


ci_jobs = load("ci_jobs")

TABLE = """
[everyone]
paths = ["Cargo.lock", "build/*.bzl", "!build/apple.bzl"]

[covered]
paths = ["docs/**", "crates/**", ".github/workflows/*"]

[jobs.phone]
name = "Phone"
workflow_job = "phone"
paths = ["apps/apple/**", "!apps/apple/Mac/**", "build/apple.bzl", "crates/app-ffi/**"]
contains_in = ["crates/**"]
contains = ['cfg(target_os = "ios")']

[jobs.desk]
name = "Desk"
workflow_job = "desk"
paths = ["apps/apple/**"]
"""

WORKFLOW = """name: CI
on: push
env:
  A: "1"
jobs:
  changes:
    runs-on: ubuntu-latest
    steps:
      - run: echo
  phone:
    runs-on: macos-26
    steps: &shared
      - run: echo one
      - run: echo two
  desk:
    runs-on: macos-26
    steps: *shared
  other:
    runs-on: ubuntu-latest
    steps:
      - run: echo
"""


class Fake:
    """Contents at the base and head commits, from dictionaries."""

    def __init__(self, base: dict | None = None, head: dict | None = None):
        self.base, self.head = "base", "head"
        self.files = {"base": base or {}, "head": head or {}}

    def prefetch(self, paths):
        pass

    def read(self, rev, path):
        return self.files[rev].get(path)

    def both(self, path):
        return [t for t in (self.read("base", path), self.read("head", path)) if t is not None]


def runs(files, contents=None, table=TABLE):
    decided = ci_jobs.decide(ci_jobs.load_table(table), files, contents or Fake())
    return {job for job, (run, _) in decided.items() if run}


class GlobTest(unittest.TestCase):
    def test_a_star_stays_in_its_segment_and_two_cross_them(self):
        self.assertTrue(ci_jobs.Globs(["build/*.bzl"]).match("build/rust.bzl"))
        self.assertFalse(ci_jobs.Globs(["build/*.bzl"]).match("build/toolchains/rust.bzl"))
        self.assertTrue(ci_jobs.Globs(["apps/**"]).match("apps/apple/Amux/A.swift"))
        self.assertTrue(ci_jobs.Globs(["**/*.md"]).match("README.md"))
        self.assertTrue(ci_jobs.Globs(["**/*.md"]).match("crates/node/README.md"))

    def test_a_bang_takes_back_what_came_before_it(self):
        globs = ci_jobs.Globs(["apps/apple/**", "!apps/apple/Mac/**"])
        self.assertTrue(globs.match("apps/apple/Amux/A.swift"))
        self.assertFalse(globs.match("apps/apple/Mac/M.swift"))


class DecideTest(unittest.TestCase):
    def test_a_covered_file_runs_nothing_and_a_docs_change_skips_every_job(self):
        self.assertEqual(runs(["docs/CI.md", "crates/node/src/lib.rs"]), set())

    def test_a_territory_wins_over_covered(self):
        self.assertEqual(runs(["crates/app-ffi/src/lib.rs"]), {"phone"})

    def test_an_exclusion_keeps_the_other_app_alone(self):
        self.assertEqual(runs(["apps/apple/Mac/M.swift"]), {"desk"})
        self.assertEqual(runs(["apps/apple/Amux/A.swift"]), {"phone", "desk"})

    def test_everyone_runs_every_job_and_its_exclusion_lands_in_a_territory(self):
        self.assertEqual(runs(["build/rust.bzl"]), {"phone", "desk"})
        self.assertEqual(runs(["build/apple.bzl"]), {"phone"})

    def test_a_file_no_table_names_runs_every_job(self):
        decided = ci_jobs.decide(ci_jobs.load_table(TABLE), ["somewhere/new.txt"], Fake())
        self.assertTrue(all(run for run, _ in decided.values()))
        self.assertIn("somewhere/new.txt", decided["phone"][1])

    def test_contents_before_or_after_the_change_run_a_job(self):
        ios = '#[cfg(target_os = "ios")]\nfn f() {}\n'
        self.assertEqual(runs(["crates/node/src/a.rs"], Fake(head={"crates/node/src/a.rs": ios})), {"phone"})
        self.assertEqual(runs(["crates/node/src/a.rs"], Fake(base={"crates/node/src/a.rs": ios})), {"phone"})
        self.assertEqual(runs(["crates/node/src/a.rs"], Fake(head={"crates/node/src/a.rs": "fn f() {}"})), set())

    def test_contents_count_only_under_contains_in(self):
        ios = '#[cfg(target_os = "ios")]'
        self.assertEqual(runs(["docs/IOS.md"], Fake(head={"docs/IOS.md": ios})), set())

    def test_the_reason_names_the_file(self):
        decided = ci_jobs.decide(ci_jobs.load_table(TABLE), ["docs/a.md", "apps/apple/Amux/A.swift"], Fake())
        self.assertTrue(decided["phone"][0])
        self.assertIn("apps/apple/Amux/A.swift", decided["phone"][1])
        self.assertFalse(ci_jobs.decide(ci_jobs.load_table(TABLE), ["docs/a.md"], Fake())["phone"][0])


LOCK = """version = 4

[[package]]
name = "node"
version = "0.1.0"
dependencies = ["serde"]

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"""


class LockTest(unittest.TestCase):
    def test_a_workspace_crates_entry_alone_is_covered(self):
        bumped = LOCK.replace('version = "0.1.0"', 'version = "0.2.0"')
        self.assertEqual(runs(["Cargo.lock"], Fake({"Cargo.lock": LOCK}, {"Cargo.lock": bumped})), set())
        edge = LOCK.replace('dependencies = ["serde"]', 'dependencies = ["serde", "wire"]')
        self.assertEqual(runs(["Cargo.lock"], Fake({"Cargo.lock": LOCK}, {"Cargo.lock": edge})), set())

    def test_a_third_party_change_runs_every_job(self):
        bumped = LOCK.replace('version = "1.0.0"', 'version = "1.0.1"')
        self.assertEqual(runs(["Cargo.lock"], Fake({"Cargo.lock": LOCK}, {"Cargo.lock": bumped})), {"phone", "desk"})

    def test_a_lock_that_does_not_parse_runs_every_job(self):
        self.assertEqual(runs(["Cargo.lock"], Fake({"Cargo.lock": LOCK}, {"Cargo.lock": "[[["})), {"phone", "desk"})


class WorkflowTest(unittest.TestCase):
    def changed(self, new):
        path = ".github/workflows/ci.yml"
        return runs([path], Fake({path: WORKFLOW}, {path: new}))

    def test_a_line_in_a_jobs_block_runs_that_job(self):
        self.assertEqual(self.changed(WORKFLOW.replace("    steps: *shared", "    timeout-minutes: 5\n    steps: *shared")), {"desk"})

    def test_a_shared_anchor_runs_every_job_that_uses_it(self):
        self.assertEqual(self.changed(WORKFLOW.replace("echo two", "echo three")), {"phone", "desk"})

    def test_another_jobs_block_runs_no_gated_job(self):
        self.assertEqual(self.changed(WORKFLOW.replace("  other:\n    runs-on: ubuntu-latest", "  other:\n    runs-on: ubuntu-24.04")), set())

    def test_a_line_outside_the_jobs_runs_every_job(self):
        self.assertEqual(self.changed(WORKFLOW.replace('A: "1"', 'A: "2"')), {"phone", "desk"})

    def test_a_job_added_at_the_end_runs_no_gated_job(self):
        self.assertEqual(self.changed(WORKFLOW + "  more:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo\n"), set())


class InCITest(unittest.TestCase):
    """What the `changes` job writes: one line per gated job, and every job
    on when the run was started by hand or anything went wrong."""

    def decide(self, event, passes=None):
        import contextlib, io, os, tempfile, unittest.mock
        with tempfile.TemporaryDirectory() as directory, contextlib.ExitStack() as stack:
            output = Path(directory) / "output"
            stack.enter_context(unittest.mock.patch.dict(os.environ, {"GITHUB_EVENT_NAME": event, "GITHUB_OUTPUT": str(output), "GITHUB_REPOSITORY": "o/r"}))
            stack.enter_context(unittest.mock.patch.object(ci_jobs, "last_passes", side_effect=passes or RuntimeError("GitHub is down")))
            stack.enter_context(unittest.mock.patch.object(ci_jobs, "git", return_value=subprocess.CompletedProcess([], 0, "abc\n", "")))
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            self.assertEqual(0, ci_jobs.decide_in_ci())
            return dict(line.split("=") for line in output.read_text().split())

    def test_a_run_by_hand_runs_every_job(self):
        self.assertEqual(set(self.decide("workflow_dispatch").values()), {"true"})

    def test_a_decision_that_fails_runs_every_job(self):
        written = self.decide("push")
        self.assertEqual(set(written.values()), {"true"})
        self.assertEqual(set(written), {"iphone", "mac_app", "macos_tests", "macos_journeys", "windows_tests"})


class TheRealTableTest(unittest.TestCase):
    """build/ci-jobs.toml itself."""

    @classmethod
    def setUpClass(cls):
        cls.table = ci_jobs.load_table()

    def tracked(self) -> list[str]:
        # What build/checks/inputs.py writes for the repository checks, or,
        # run in a checkout before it has, git's own list.
        listed = ROOT / "build/checks/inputs/tracked"
        if listed.exists():
            return [line for line in listed.read_text().splitlines() if line]
        return subprocess.run(["git", "ls-files"], cwd=ROOT, capture_output=True, text=True, check=True).stdout.splitlines()

    def test_every_tracked_file_is_classified(self):
        files = self.tracked()
        self.assertGreater(len(files), 1000)
        unclassified = [path for path in files if self.table.kind(path) == "unclassified"]
        self.assertEqual(unclassified, [], "name these in build/ci-jobs.toml: everyone, covered or a job's paths")

    def test_no_list_names_a_glob_twice(self):
        # A repeated glob is a copy-and-paste slip no other check notices.
        lists = {"everyone": self.table.everyone, "covered": self.table.covered}
        lists |= {f"jobs.{job.key}.paths": job.paths for job in self.table.jobs}
        for name, globs in lists.items():
            repeated = sorted({p for p in globs.patterns if globs.patterns.count(p) > 1})
            self.assertEqual(repeated, [], f"build/ci-jobs.toml [{name}] lists these more than once")

    def test_every_gated_job_has_a_job_in_the_workflow(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        owners, _ = ci_jobs.workflow_owners(workflow)
        keys = {job for job in owners.values() if job}
        for job in self.table.jobs:
            self.assertIn(job.workflow_job, keys)
            self.assertIn(f"name: {job.name}\n", workflow)
            # The job's `if` reads the answer the `changes` job passes on.
            self.assertIn(f"{job.output}: ${{{{ steps.decide.outputs.{job.output} }}}}", workflow)
            self.assertIn(f"needs.changes.outputs.{job.output} != 'false'", workflow)

    def test_the_phone_runs_for_its_app_and_not_for_the_node(self):
        decided = ci_jobs.decide(self.table, ["apps/apple/Amux/Sources/App.swift"], Fake())
        self.assertEqual({j for j, (r, _) in decided.items() if r}, {"iphone"})
        decided = ci_jobs.decide(self.table, ["crates/node/src/agents.rs"], Fake(head={"crates/node/src/agents.rs": "fn f() {}"}))
        self.assertEqual({j for j, (r, _) in decided.items() if r}, set())

    def test_mobile_code_android_also_compiles_does_not_run_the_phone(self):
        mobile = '#[cfg(any(target_os = "ios", target_os = "android"))]'
        decided = ci_jobs.decide(self.table, ["crates/node/src/awake.rs"], Fake(head={"crates/node/src/awake.rs": mobile}))
        self.assertFalse(decided["iphone"][0])

    def test_windows_code_runs_the_windows_tests(self):
        code = "#[cfg(not(unix))]\nfn f() {}"
        decided = ci_jobs.decide(self.table, ["crates/pty-host/src/lib.rs"], Fake(head={"crates/pty-host/src/lib.rs": code}))
        self.assertEqual({j for j, (r, _) in decided.items() if r}, {"windows-tests"})

    def selected(self, path: str) -> set[str]:
        # The file holds no `cfg` of its own: its module's is on the parent's
        # declaration, so only its path can select a job.
        decided = ci_jobs.decide(self.table, [path], Fake(head={path: "pub fn f() {}"}))
        return {j for j, (r, _) in decided.items() if r}

    def test_a_module_for_windows_alone_runs_the_windows_tests(self):
        for path in [
            "crates/os/src/fs/windows.rs",
            "crates/os/src/fs/windows_tests.rs",
            "crates/os/src/process/windows/mod.rs",
            "crates/os/src/process/windows/wait.rs",
            "crates/os/src/testing/desktop/windows.rs",
            "crates/agent-dir/src/local_socket/windows.rs",
            "crates/node/src/platform/windows.rs",
            "crates/node/src/supervisor/login/windows.rs",
            "crates/node/src/supervisor/keep_awake/windows.rs",
            "crates/tui/src/platform/windows.rs",
            "crates/provider-fakes/src/platform/windows.rs",
            "crates/qualification/src/platform/windows.rs",
            "crates/amux/tests/windows_install.rs",
        ]:
            with self.subTest(path):
                self.assertEqual(self.selected(path), {"windows-tests"})

    def test_what_only_windows_ships_or_runs_runs_the_windows_tests(self):
        for path in [
            "crates/launch/src/main.rs",
            "crates/launch/build.rs",
            "crates/exe-resource/src/lib.rs",
            "scripts/install/install.ps1",
        ]:
            with self.subTest(path):
                self.assertEqual(self.selected(path), {"windows-tests"})

    def test_a_module_for_macos_alone_runs_the_macos_tests_and_journeys(self):
        for path in [
            "crates/node/src/platform/macos.rs",
            "crates/node/src/supervisor/login/macos.rs",
            "crates/node/src/supervisor/keep_awake/macos.rs",
            "crates/qualification/src/platform/unix/macos.rs",
        ]:
            with self.subTest(path):
                self.assertEqual(self.selected(path), {"macos-tests", "macos-journeys"})

    def test_a_module_for_linux_alone_is_the_linux_gates(self):
        for path in [
            "crates/node/src/platform/linux.rs",
            "crates/node/src/supervisor/login/linux.rs",
            "crates/qualification/src/platform/unix/linux.rs",
        ]:
            with self.subTest(path):
                self.assertEqual(self.selected(path), set())


if __name__ == "__main__":
    unittest.main()
