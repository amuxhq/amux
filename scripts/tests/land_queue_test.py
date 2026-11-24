"""What the landing queue and `just land` do, against repositories of their
own: an origin standing in for GitHub, the queue's inbox and landing tree,
and a developer's clone. The gate is a shell command, never `just premerge`."""

import contextlib
import datetime
import importlib.util
import io
import os
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(SCRIPTS))


def load(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


queue = load("land_queue")
client = load("land")

IDENTITY = {
    "GIT_AUTHOR_NAME": "Lander",
    "GIT_AUTHOR_EMAIL": "lander@example.com",
    "GIT_COMMITTER_NAME": "Lander",
    "GIT_COMMITTER_EMAIL": "lander@example.com",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1",
}


def git(cwd: Path, *args: str) -> str:
    return subprocess.run(["git", *args], cwd=cwd, check=True, text=True, capture_output=True).stdout.strip()


def commit(tree: Path, path: str, text: str, message: str) -> str:
    (tree / path).write_text(text)
    git(tree, "add", path)
    git(tree, "commit", "--quiet", "-m", message)
    return git(tree, "rev-parse", "HEAD")


class Repos(unittest.TestCase):
    def setUp(self):
        self.env = mock_env(IDENTITY)
        self.env.__enter__()
        self.addCleanup(self.env.__exit__, None, None, None)
        self.root = Path(tempfile.mkdtemp(prefix="land-queue-test-"))
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.root)])
        self.origin = self.root / "origin.git"
        self.inbox = self.root / "inbox.git"
        git(self.root, "init", "--quiet", "--bare", "-b", "main", str(self.origin))
        git(self.root, "init", "--quiet", "--bare", str(self.inbox))
        seed = self.root / "seed"
        git(self.root, "clone", "--quiet", str(self.origin), str(seed))
        git(seed, "switch", "--quiet", "-c", "main")
        commit(seed, "README", "amux\n", "Start")
        git(seed, "push", "--quiet", "origin", "main")
        self.tree = self.clone("tree")
        self.dev = self.clone("dev")
        git(self.dev, "switch", "--quiet", "-c", "feature")

    def clone(self, name: str) -> Path:
        path = self.root / name
        git(self.root, "clone", "--quiet", str(self.origin), str(path))
        return path

    def request(self, rid: str, message: str | None = "Add a feature") -> str:
        """What `just land` pushes: a tag naming the dev's HEAD."""
        head = git(self.dev, "rev-parse", "HEAD")
        with patched(client, CHECKOUT=self.dev):
            source = client.request_tag(head, rid, message) if message is not None else head
        git(self.dev, "push", "--quiet", str(self.inbox), f"{source}:refs/queue/{rid}")
        return head

    def land(self, rid: str, premerge: str = "true") -> str:
        def gate_for(log):
            return queue.logged_gate(log, None, premerge=("sh", "-c", premerge))

        with contextlib.redirect_stdout(io.StringIO()):
            return queue.land_request(self.inbox, self.tree, rid, gate_for)

    def result(self, rid: str) -> str:
        return git(self.inbox, "cat-file", "blob", f"refs/results/{rid}")

    def main(self) -> str:
        return git(self.origin, "rev-parse", "main")


class LandingTest(Repos):
    def test_a_request_lands_as_one_merge_saying_what_the_branch_does(self):
        before = self.main()
        commit(self.dev, "a", "1\n", "One")
        head = commit(self.dev, "b", "2\n", "Two")
        self.request("20261123T100000.000000Z-mac-feature")

        first = self.land("20261123T100000.000000Z-mac-feature")

        landed = self.main()
        self.assertEqual(first, f"LANDED {landed}")
        self.assertEqual(git(self.origin, "log", "-1", "--format=%P", landed).split(), [before, head])
        self.assertEqual(git(self.origin, "log", "-1", "--format=%B", landed), "Add a feature")
        self.assertEqual(self.result("20261123T100000.000000Z-mac-feature"), first)
        self.assertEqual(queue.pending(self.inbox), [])

    def test_the_merge_is_the_senders_whoever_the_queue_runs_as(self):
        commit(self.dev, "a", "1\n", "One")
        self.request("r1")

        with mock_env({"GIT_AUTHOR_NAME": "Queue", "GIT_AUTHOR_EMAIL": "queue@example.com",
                       "GIT_COMMITTER_NAME": "Queue", "GIT_COMMITTER_EMAIL": "queue@example.com"}):
            self.land("r1")

        self.assertEqual(git(self.origin, "log", "-1", "--format=%an <%ae>|%cn <%ce>", self.main()),
                         "Lander <lander@example.com>|Lander <lander@example.com>")

    def test_a_failing_gate_stops_with_the_end_of_its_log_and_main_unmoved(self):
        before = self.main()
        commit(self.dev, "a", "1\n", "One")
        self.request("r1")

        first = self.land("r1", premerge="echo the tests broke; exit 3")

        self.assertEqual(self.main(), before)
        self.assertTrue(first.startswith("STOPPED the gate failed"), first)
        self.assertIn("exited 3", first)
        self.assertIn("the tests broke", self.result("r1"))
        self.assertIn("the tests broke", git(self.inbox, "cat-file", "blob", "refs/logs/r1"))

    def test_a_branch_that_conflicts_with_main_stops_naming_the_file(self):
        commit(self.dev, "README", "mine\n", "Mine")
        self.request("r1")
        other = self.clone("other")
        commit(other, "README", "theirs\n", "Theirs")
        git(other, "push", "--quiet", "origin", "HEAD:main")
        before = self.main()

        first = self.land("r1")

        self.assertEqual(self.main(), before)
        self.assertEqual(first, "STOPPED the branch doesn't merge onto main: README")

    def test_a_refused_push_lands_again_on_top_of_whoever_landed_first(self):
        commit(self.dev, "a", "1\n", "One")
        self.request("r1")
        other = self.clone("other")
        mark = self.root / "raced"
        # The first gate run lands someone else's commit, so the push after
        # it is refused; the second finds main already moved.
        race = f"[ -e {mark} ] || {{ touch {mark}; cd {other} && git commit -q --allow-empty -m Raced && git push -q origin HEAD:main; }}"

        first = self.land("r1", premerge=race)

        landed = self.main()
        self.assertEqual(first, f"LANDED {landed}")
        raced = git(other, "rev-parse", "HEAD")
        self.assertEqual(git(self.origin, "rev-parse", f"{landed}^1"), raced)

    def test_a_push_refused_with_main_unmoved_stops_at_once_saying_why(self):
        commit(self.dev, "a", "1\n", "One")
        self.request("r1")
        before = self.main()
        # Standing in for a protected branch: the origin refuses every push.
        hook = self.origin / "hooks" / "pre-receive"
        hook.write_text("#!/bin/sh\necho main is protected >&2\nexit 1\n")
        hook.chmod(0o755)

        first = self.land("r1")

        self.assertTrue(first.startswith("STOPPED the push to main was refused"), first)
        self.assertIn("main is protected", self.result("r1"))
        self.assertEqual(self.main(), before)

    def test_a_request_without_a_message_is_refused(self):
        before = self.main()
        commit(self.dev, "a", "1\n", "One")
        self.request("r1", message=None)

        first = self.land("r1")

        self.assertEqual(self.main(), before)
        self.assertTrue(first.startswith("STOPPED the request carries no message"), first)

    def test_a_commit_already_on_main_is_refused(self):
        self.request("r1")

        self.assertEqual(self.land("r1"), f"STOPPED {self.main()[:12]} is already on main")

    def test_requests_are_taken_oldest_first(self):
        commit(self.dev, "a", "1\n", "One")
        self.request("20261123T100002.000000Z-mac-b")
        self.request("20261123T100001.000000Z-nova-a")

        self.assertEqual(queue.pending(self.inbox), ["20261123T100001.000000Z-nova-a", "20261123T100002.000000Z-mac-b"])

    def test_old_results_are_forgotten_with_their_request_and_log(self):
        commit(self.dev, "a", "1\n", "One")
        self.request("20261101T100000.000000Z-mac-old")
        self.land("20261101T100000.000000Z-mac-old")
        commit(self.dev, "b", "2\n", "Two")
        self.request("20261122T100000.000000Z-mac-new")
        self.land("20261122T100000.000000Z-mac-new")

        queue.prune(self.inbox, datetime.datetime(2026, 11, 23, tzinfo=datetime.timezone.utc))

        left = git(self.inbox, "for-each-ref", "--format=%(refname)").splitlines()
        self.assertEqual(
            sorted(left),
            [f"refs/{kind}/20261122T100000.000000Z-mac-new" for kind in ("logs", "queue", "results")],
        )


class ClientTest(Repos):
    def test_just_land_waits_for_the_queue_and_prints_what_landed(self):
        commit(self.dev, "a", "1\n", "One")
        served = threading.Thread(target=self.serve_one)
        served.start()
        out, err = io.StringIO(), io.StringIO()
        with patched(client, CHECKOUT=self.dev, POLL_SECONDS=0.05), contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            status = client.land_through(str(self.inbox), git(self.dev, "rev-parse", "HEAD"), "Add a feature", "feature")
        served.join(timeout=30)

        self.assertEqual(status, 0, err.getvalue())
        self.assertIn(f"LANDED {self.main()}", out.getvalue())

    def test_just_land_reports_what_stopped_the_queue(self):
        commit(self.dev, "a", "1\n", "One")
        served = threading.Thread(target=self.serve_one, kwargs={"premerge": "echo nope; exit 1"})
        served.start()
        err = io.StringIO()
        with patched(client, CHECKOUT=self.dev, POLL_SECONDS=0.05), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
            status = client.land_through(str(self.inbox), git(self.dev, "rev-parse", "HEAD"), "Add a feature", "feature")
        served.join(timeout=30)

        self.assertEqual(status, 1)
        self.assertIn("land: stopped: the gate failed", err.getvalue())
        self.assertIn("nope", err.getvalue())

    def test_a_request_already_answered_is_not_counted_ahead(self):
        commit(self.dev, "a", "1\n", "One")
        served = threading.Thread(target=self.serve_one)
        served.start()
        with patched(client, CHECKOUT=self.dev, POLL_SECONDS=0.05), contextlib.redirect_stdout(io.StringIO()):
            client.land_through(str(self.inbox), git(self.dev, "rev-parse", "HEAD"), "Add a feature", "feature")
        served.join(timeout=30)

        with patched(client, CHECKOUT=self.dev):
            self.assertEqual(client.ahead_of(str(self.inbox), client.request_id("later"), client.inbox_env()), 0)

    def serve_one(self, premerge: str = "true"):
        for _ in range(600):
            waiting = queue.pending(self.inbox)
            if waiting:
                self.land(waiting[0], premerge=premerge)
                return
            threading.Event().wait(0.05)

    def test_request_ids_sort_by_when_they_were_made(self):
        first = client.request_id("feature/one")
        second = client.request_id("feature/one")
        self.assertLess(first, second)
        self.assertNotIn("/", first)


class CheckTest(Repos):
    def test_check_passes_for_a_bare_inbox_and_a_clone_and_fails_otherwise(self):
        def run(inbox: Path, tree: Path) -> int:
            args = queue.argparse.Namespace(inbox=inbox, tree=tree, nix=False)
            with contextlib.redirect_stderr(io.StringIO()):
                return queue.check(args)

        self.assertEqual(run(self.inbox, self.tree), 0)
        self.assertEqual(run(self.tree, self.tree), 1)
        self.assertEqual(run(self.inbox, self.root / "missing"), 1)


class TrimTest(unittest.TestCase):
    """The tree's buck-out is trimmed through its Buck2 daemon: a fake
    scripts/buck2 here records what it was asked."""

    def setUp(self):
        self.tree = Path(tempfile.mkdtemp(prefix="land-queue-trim-"))
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.tree)])
        self.asked = self.tree / "asked"
        (self.tree / "scripts").mkdir()
        self.buck2(0)

    def buck2(self, status: int):
        fake = self.tree / "scripts" / "buck2"
        fake.write_text(f'#!/bin/sh\necho "$WRAPPED $*" >> {self.asked}\nexit {status}\n')
        fake.chmod(0o755)

    def trim(self) -> str:
        def wrap(argv, tree):
            return ["env", f"WRAPPED=in {tree.name}", *argv]

        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            queue.trim(self.tree, wrap)
        return out.getvalue()

    def test_stale_outputs_go_through_the_daemon_in_the_merges_toolchain(self):
        (self.tree / "buck-out").mkdir()

        said = self.trim()

        self.assertEqual(self.asked.read_text(), f"in {self.tree.name} clean --stale {queue.TRIM_AGE}\n")
        self.assertIn("trimmed buck-out", said)

    def test_a_tree_that_never_built_is_left_alone(self):
        self.assertEqual(self.trim(), "")
        self.assertFalse(self.asked.exists())

    def test_a_failing_trim_is_said_and_the_queue_goes_on(self):
        (self.tree / "buck-out").mkdir()
        self.buck2(3)

        self.assertIn("trimming buck-out failed (exited 3)", self.trim())


class FreshDaemonTest(unittest.TestCase):
    """A Mac landing restarts its tree's Buck2 daemon once it is old: a fake
    scripts/buck2 here says when it started and records a kill."""

    NOW = datetime.datetime(2026, 11, 24, 12, 0, tzinfo=datetime.timezone.utc)

    def setUp(self):
        self.tree = Path(tempfile.mkdtemp(prefix="land-daemon-"))
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.tree)])
        self.asked = self.tree / "asked"
        (self.tree / "scripts").mkdir()

    def daemon(self, status: str):
        fake = self.tree / "scripts" / "buck2"
        fake.write_text(f"#!/bin/sh\necho \"$*\" >> {self.asked}\n[ \"$1\" = status ] && cat <<'EOF'\n{status}\nEOF\nexit 0\n")
        fake.chmod(0o755)

    def started(self, hours_ago: float):
        at = self.NOW - datetime.timedelta(hours=hours_ago)
        self.daemon(f'{{"start_time": "{at:%Y-%m-%dT%H:%M:%SZ}", "uptime": "?"}}')

    def fresh(self) -> list[str]:
        with contextlib.redirect_stdout(io.StringIO()):
            client.fresh_daemon(self.tree, self.NOW)
        return self.asked.read_text().split("\n")[:-1]

    def test_a_daemon_up_past_the_limit_is_restarted(self):
        self.started(client.DAEMON_HOURS + 0.5)
        self.assertEqual(self.fresh(), ["status", "kill"])

    def test_a_younger_daemon_stays_warm(self):
        self.started(client.DAEMON_HOURS - 0.5)
        self.assertEqual(self.fresh(), ["status"])

    def test_no_daemon_running_is_left_alone(self):
        self.daemon("")
        self.assertEqual(self.fresh(), ["status"])


@contextlib.contextmanager
def patched(module, **values):
    saved = {name: getattr(module, name) for name in values}
    for name, value in values.items():
        setattr(module, name, value)
    try:
        yield
    finally:
        for name, value in saved.items():
            setattr(module, name, value)


@contextlib.contextmanager
def mock_env(values):
    saved = {name: os.environ.get(name) for name in values}
    os.environ.update(values)
    try:
        yield
    finally:
        for name, value in saved.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value


if __name__ == "__main__":
    unittest.main()
