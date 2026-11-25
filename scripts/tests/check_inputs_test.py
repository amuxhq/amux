"""What build/checks/inputs.py writes for the deny check, from any machine."""

import importlib.util
import json
import os
from pathlib import Path, PurePosixPath, PureWindowsPath
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("check_inputs", ROOT / "build" / "checks" / "inputs.py")
assert SPEC is not None and SPEC.loader is not None
inputs = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(inputs)


def metadata(root: str, home: str, sep: str, url_root: str) -> dict:
    """A small graph as `cargo metadata` gives it on one machine: a workspace
    crate with a path dependency, and a crate from crates.io."""

    def native(base: str, *parts: str) -> str:
        return sep.join([base, *parts])

    registry = native(home, "registry", "src", "index.crates.io-1949cf8c6b5b557f", "adler2-2.0.1")
    member = f"path+file://{url_root}/crates/a#0.1.0"
    return {
        "packages": [
            {
                "name": "a",
                "id": member,
                "source": None,
                "description": "matches \\d+ in a regex, not a path",
                "dependencies": [{"name": "b", "path": native(root, "crates", "b")}],
                "targets": [{"src_path": native(root, "crates", "a", "src", "lib.rs")}],
                "manifest_path": native(root, "crates", "a", "Cargo.toml"),
            },
            {
                "name": "adler2",
                "id": "registry+https://github.com/rust-lang/crates.io-index#adler2@2.0.1",
                "source": "registry+https://github.com/rust-lang/crates.io-index",
                "targets": [{"src_path": native(registry, "src", "lib.rs")}],
                "manifest_path": native(registry, "Cargo.toml"),
            },
        ],
        "workspace_members": [member],
        "resolve": {"nodes": [{"id": member, "deps": [{"pkg": member}]}], "root": None},
        "target_directory": native(root, "target"),
        "workspace_root": root,
    }


def written(graph: dict, workspace, home) -> str:
    return json.dumps(inputs.portable(graph, workspace, home), ensure_ascii=False, separators=(",", ":"))


class PortableTest(unittest.TestCase):
    def test_a_graph_from_windows_is_the_one_from_linux(self):
        linux = metadata("/home/dev/amux", "/home/dev/.cargo", "/", "/home/dev/amux")
        windows = metadata(r"C:\Users\dev\amux", r"C:\Users\dev\.cargo", "\\", "/C:/Users/dev/amux")

        from_linux = written(linux, PurePosixPath("/home/dev/amux"), PurePosixPath("/home/dev/.cargo"))
        from_windows = written(windows, PureWindowsPath(r"C:\Users\dev\amux"), PureWindowsPath(r"C:\Users\dev\.cargo"))

        self.assertEqual(from_windows, from_linux)
        self.assertNotIn("Users", from_windows)
        self.assertIn('"manifest_path":"@ROOT@/build/checks/inputs/deny/workspace/crates/a/Cargo.toml"', from_windows)
        self.assertIn('"id":"path+file://@ROOT@/build/checks/inputs/deny/workspace/crates/a#0.1.0"', from_windows)
        self.assertIn('"manifest_path":"/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/adler2-2.0.1/Cargo.toml"', from_windows)
        self.assertIn('"description":"matches \\\\d+ in a regex, not a path"', from_windows)

    def test_a_graph_from_linux_is_cargos_text_with_the_paths_replaced(self):
        graph = metadata("/home/dev/amux", "/home/dev/.cargo", "/", "/home/dev/amux")
        cargo = json.dumps(graph, separators=(",", ":"))
        replaced = cargo.replace("/home/dev/amux", "@ROOT@/build/checks/inputs/deny/workspace").replace("/home/dev/.cargo", "/cargo")

        self.assertEqual(written(graph, PurePosixPath("/home/dev/amux"), PurePosixPath("/home/dev/.cargo")), replaced)


class WriteTest(unittest.TestCase):
    def setUp(self):
        self.room = tempfile.TemporaryDirectory()
        self.addCleanup(self.room.cleanup)
        self.target = Path(self.room.name) / "metadata.json"

    def left(self) -> list[str]:
        return sorted(path.name for path in Path(self.room.name).iterdir())

    def test_a_write_leaves_only_the_file(self):
        inputs.write(self.target, b"one")
        inputs.write(self.target, b"two")

        self.assertEqual(self.target.read_bytes(), b"two")
        self.assertEqual(self.left(), ["metadata.json"])

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
        self.assertEqual(self.left(), ["metadata.json"])

    def test_a_write_that_fails_leaves_nothing_behind(self):
        with mock.patch.object(inputs.os, "replace", side_effect=PermissionError(13, "denied")), \
                mock.patch.object(inputs.os, "name", "posix"):
            with self.assertRaises(PermissionError):
                inputs.write(self.target, b"content")

        self.assertEqual(self.left(), [])


if __name__ == "__main__":
    unittest.main()
