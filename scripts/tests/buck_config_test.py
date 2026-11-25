"""When a Mac's tree calls the build host out of reach (scripts/buck-config.py):
only after it misses twice in a row, and never while `just premerge` has
decided for it. A listener stands in for the build host's NativeLink, and a
directory of the test's own for the answer the trees share."""

import importlib.util
import os
import socket
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPTS = Path(__file__).resolve().parent.parent


def load(name: str):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


buck_config = load("buck-config")


class AnswersTest(unittest.TestCase):
    def setUp(self):
        cache = tempfile.TemporaryDirectory(prefix="buck-config-")
        self.addCleanup(cache.cleanup)
        patch = mock.patch.object(buck_config, "REACHABLE", Path(cache.name) / "reachable")
        patch.start()
        self.addCleanup(patch.stop)
        # The pause between the two tries, not waited for here.
        self.sleeps = []
        patch = mock.patch.object(buck_config.time, "sleep", self.sleeps.append)
        patch.start()
        self.addCleanup(patch.stop)

    def listener(self) -> str:
        remote = socket.socket()
        remote.bind(("127.0.0.1", 0))
        remote.listen()
        self.addCleanup(remote.close)
        return f"grpc://127.0.0.1:{remote.getsockname()[1]}"

    def closed_port(self) -> str:
        probe = socket.socket()
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
        probe.close()
        return f"grpc://127.0.0.1:{port}"

    def test_a_remote_that_takes_the_connection_answers(self):
        self.assertTrue(buck_config.answers(self.listener()))
        self.assertEqual(self.sleeps, [])
        self.assertEqual(buck_config.REACHABLE.read_text().split()[1], "yes")

    def test_one_lost_connection_is_not_out_of_reach(self):
        real = socket.create_connection
        tries = []

        def drops_the_first(address, timeout):
            tries.append(address)
            if len(tries) == 1:
                raise TimeoutError("timed out")
            return real(address, timeout=timeout)

        with mock.patch.object(buck_config.socket, "create_connection", drops_the_first):
            self.assertTrue(buck_config.answers(self.listener()))
        self.assertEqual(len(tries), 2)
        self.assertEqual(self.sleeps, [1])
        self.assertEqual(buck_config.REACHABLE.read_text().split()[1], "yes")

    def test_two_misses_in_a_row_are(self):
        self.assertFalse(buck_config.answers(self.closed_port()))
        self.assertEqual(self.sleeps, [1])
        self.assertEqual(buck_config.REACHABLE.read_text().split()[1], "no")

    def test_the_answer_is_remembered_rather_than_asked_again(self):
        url = self.closed_port()
        self.assertFalse(buck_config.answers(url))
        with mock.patch.object(buck_config, "connects", side_effect=AssertionError("asked again")):
            self.assertFalse(buck_config.answers(url))


class PinnedModeTest(unittest.TestCase):
    def test_a_decided_mode_is_used_without_asking(self):
        with (
            mock.patch.dict(os.environ, {"AMUX_BUCK_MODE": "remote"}),
            mock.patch.object(buck_config, "answers", side_effect=AssertionError("asked the remote")),
            mock.patch.object(buck_config, "remote", side_effect=AssertionError("read the settings")),
        ):
            self.assertEqual(buck_config.mode(), "remote")
            self.assertTrue(buck_config.linux(buck_config.mode()))

    def test_an_offline_decision_holds_too(self):
        with mock.patch.dict(os.environ, {"AMUX_BUCK_MODE": "local"}):
            self.assertEqual(buck_config.mode(), "local")
            self.assertFalse(buck_config.linux(buck_config.mode()))


if __name__ == "__main__":
    unittest.main()
