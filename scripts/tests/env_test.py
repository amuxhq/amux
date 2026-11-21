"""What `just env` builds for a world."""

import importlib.util
from pathlib import Path
import unittest

SCRIPT = Path(__file__).resolve().parent.parent / "env.py"
spec = importlib.util.spec_from_file_location("env", SCRIPT)
env = importlib.util.module_from_spec(spec)
spec.loader.exec_module(env)


class BuildTest(unittest.TestCase):
    def test_a_world_with_a_real_host_builds_the_probes_its_agents_run_behind(self):
        world = env.ENVIRONMENTS / "acp.json"
        self.assertTrue(env.has_real_host(world))
        built = [" ".join(command) for command in env.build_commands(phone=True, real=True)]
        self.assertTrue(any("-p qualification" in command and "--bin=acp-probe" in command for command in built), built)

    def test_a_world_of_fakes_builds_no_probe(self):
        built = " ".join(" ".join(command) for command in env.build_commands(phone=False, real=False))
        self.assertNotIn("qualification", built)


if __name__ == "__main__":
    unittest.main()
