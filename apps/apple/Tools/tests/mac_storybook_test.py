"""The Mac storybook's watcher: what it calls a change and how it reports a
failed build."""

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest


TOOLS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("mac_storybook", TOOLS / "mac-storybook.py")
storybook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(storybook)


class FirstError(unittest.TestCase):
    def test_names_the_compilers_first_complaint_relative_to_the_checkout(self) -> None:
        log = (
            "note: Building targets\n"
            f"{storybook.ROOT}/apps/One.swift:3:1: error: first\n"
            f"{storybook.ROOT}/apps/Two.swift:9:1: error: second\n"
        )
        self.assertEqual(storybook.first_error(log), "apps/One.swift:3:1: error: first")

    def test_points_at_the_log_when_no_line_is_an_error(self) -> None:
        self.assertIn("build.log", storybook.first_error("** BUILD FAILED **\n"))


class Newest(unittest.TestCase):
    def test_sees_a_file_inside_a_folder_and_ignores_package_build_output(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "Sources/View.swift"
            built = root / ".build/out.o"
            for file, stamp in [(source, 100), (built, 500)]:
                file.parent.mkdir(parents=True)
                file.write_text("x")
                os.utime(file, (stamp, stamp))
            self.assertEqual(storybook.newest([root]), 100)


if __name__ == "__main__":
    unittest.main()
