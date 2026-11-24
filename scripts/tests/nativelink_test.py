"""What the Mac's NativeLink start script (scripts/nativelink.py) does to the
local cache layer as it picks a configuration: it runs here against a cache
directory of its own, a stand-in for the NativeLink binary that records the
configuration it was given, and a listener standing in for the remote."""

import importlib.util
import os
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent


def load(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


nativelink = load("nativelink")


@unittest.skipUnless(sys.platform == "darwin", "the start script is the Mac's")
class StartScriptTest(unittest.TestCase):
    def setUp(self):
        self.cache = Path(tempfile.mkdtemp(prefix="nativelink-start-"))
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.cache)])
        self.ran = self.cache / "ran"
        fake = self.cache / "nativelink"
        fake.write_text(f'#!/bin/sh\necho "$1" > "{self.ran}"\n')
        fake.chmod(0o755)
        for name, value in {
            "CACHE": self.cache,
            "CONFIG": self.cache / "config.json5",
            "CONFIG_LOCAL": self.cache / "config-local.json5",
            "SERVING": self.cache / "serving",
            "UNSHARED_SINCE": self.cache / "unshared-since",
            "binary": lambda: str(fake),
        }.items():
            original = getattr(nativelink, name)
            setattr(nativelink, name, value)
            self.addCleanup(setattr, nativelink, name, original)
        self.blobs = self.cache / "cas" / "d2"
        self.blobs.mkdir(parents=True)
        self.remote = socket.socket()
        self.remote.bind(("127.0.0.1", 0))
        self.remote.listen()
        self.addCleanup(lambda: self.remote.close())

    def start(self, remote: dict | None = None) -> str:
        """Runs the start script; says which configuration it started with."""
        script = self.cache / "start"
        script.write_text(nativelink.start_script(remote or {}))
        subprocess.run(["/bin/sh", str(script)], check=True, timeout=30)
        started = Path(self.ran.read_text().strip())
        self.assertEqual(nativelink.SERVING.read_text().strip(), "remote" if started == nativelink.CONFIG else "local")
        return started.name

    def reachable(self) -> dict:
        return {"remote": f"grpc://127.0.0.1:{self.remote.getsockname()[1]}", "workers": "127.0.0.1:50062"}

    def unreachable(self) -> dict:
        port = self.remote.getsockname()[1]
        self.remote.close()
        return {"remote": f"grpc://127.0.0.1:{port}", "workers": "127.0.0.1:50062"}

    def blob(self, name: str, age: float) -> Path:
        path = self.blobs / name
        path.write_text(name)
        when = time.time() - age
        os.utime(path, (when, when))
        return path

    def test_what_the_local_layer_gained_without_the_remote_goes_when_it_is_back(self):
        before = self.blob("before", age=3600)
        self.assertEqual(self.start(self.unreachable()), "config-local.json5")
        os.utime(nativelink.UNSHARED_SINCE, (time.time() - 60, time.time() - 60))
        during = self.blob("during", age=0)
        self.remote = socket.socket()
        self.remote.bind(("127.0.0.1", 0))
        self.remote.listen()
        self.assertEqual(self.start(self.reachable()), "config.json5")
        self.assertTrue(before.exists())
        self.assertFalse(during.exists())
        self.assertFalse(nativelink.UNSHARED_SINCE.exists())

    def test_starting_without_the_remote_again_keeps_when_that_began(self):
        self.start({})
        began = nativelink.UNSHARED_SINCE.stat().st_mtime
        os.utime(nativelink.UNSHARED_SINCE, (began - 60, began - 60))
        self.start({})
        self.assertEqual(nativelink.UNSHARED_SINCE.stat().st_mtime, began - 60)

    def test_a_layer_that_always_had_the_remote_is_kept(self):
        kept = self.blob("kept", age=0)
        self.assertEqual(self.start(self.reachable()), "config.json5")
        self.assertTrue(kept.exists())
        self.assertFalse(nativelink.UNSHARED_SINCE.exists())


class ConfigTest(unittest.TestCase):
    def test_existence_checks_count_the_local_layer_and_uploads_reach_the_remote(self):
        stores = nativelink.remote_stores("grpc://remote:50051")
        cas = stores[stores.index('name: "CAS",'):]
        cas = cas[: cas.index("\n    },")]
        self.assertIn('fast: { ref_store: { name: "SHARED_CAS" } }', cas)
        self.assertIn('fast_direction: "update"', cas)
        self.assertIn('slow: { ref_store: { name: "LOCAL_CAS" } }', cas)
        self.assertIn('slow_direction: "read_only"', cas)
        template = nativelink.TEMPLATE.read_text()
        self.assertIn('cas: [{ instance_name: "main", cas_store: "CAS" }]', template)
        self.assertIn('bytestream: [{ instance_name: "main", cas_store: "SHARED_CAS" }]', template)


if __name__ == "__main__":
    unittest.main()
