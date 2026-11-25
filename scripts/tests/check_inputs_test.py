"""How build/checks/inputs.py writes the checks' inputs, from any machine."""

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("check_inputs", ROOT / "build" / "checks" / "inputs.py")
assert SPEC is not None and SPEC.loader is not None
inputs = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(inputs)


class WriteTest(unittest.TestCase):
    def setUp(self):
        self.room = tempfile.TemporaryDirectory()
        self.addCleanup(self.room.cleanup)
        self.target = Path(self.room.name) / "tracked"

    def left(self) -> list[str]:
        return sorted(path.name for path in Path(self.room.name).iterdir())

    def test_a_write_leaves_only_the_file(self):
        inputs.write(self.target, b"one")
        inputs.write(self.target, b"two")

        self.assertEqual(self.target.read_bytes(), b"two")
        self.assertEqual(self.left(), ["tracked"])

    def test_a_sharing_violation_on_windows_is_waited_out(self):
        real = os.replace
        refusals = [PermissionError(13, "in use"), PermissionError(13, "in use")]

        def replace(source, target):
            if refusals:
                raise refusals.pop()
            real(source, target)

        with mock.patch.object(inputs.os, "replace", side_effect=replace), \
                mock.patch.object(inputs.os, "name", "nt"), \
                mock.patch.object(inputs.time, "sleep"):
            inputs.write(self.target, b"content")

        self.assertEqual(self.target.read_bytes(), b"content")
        self.assertEqual(self.left(), ["tracked"])

    def test_a_write_that_fails_leaves_nothing_behind(self):
        with mock.patch.object(inputs.os, "replace", side_effect=PermissionError(13, "denied")), \
                mock.patch.object(inputs.os, "name", "posix"):
            with self.assertRaises(PermissionError):
                inputs.write(self.target, b"content")

        self.assertEqual(self.left(), [])


if __name__ == "__main__":
    unittest.main()
