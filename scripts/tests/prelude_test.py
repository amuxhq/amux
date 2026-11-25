"""What scripts/prelude.py reads from the prelude and writes into it: the
URL a `mvn:` coordinate names, every download a wrapper call declares, and a
digest table that reads back as it was written."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent


def load(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


prelude = load("prelude")

BUCK = '''third_party_jar(
    name = "guava",
    # @oss-disable[end= ]: internal_alias = "fbsource//third-party/java:guava",
    sha1 = "6ca5d1e7f3e2f9a1b0c4d8e7f6a5b4c3d2e1f0a9",
    url = "mvn:com.google.guava:guava:jar:33.2.0-jre",
    visibility = ["PUBLIC"],
)

buck_java_library(
    name = "uses-guava",
    deps = [":guava"],
)

third_party_aar(
    name = "sdk",
    sha1 = "0adb7cac2ab282100e2b8b6d66b997a69ed06adf",
    url = "mvn:https://maven.google.com:com.android.tools:sdklib:aar:31.7.3",
)

buck_prebuilt_artifact(
    name = "jar_builder_main.jar",
    # @oss-disable[end= ]: cas_digest = "73149775:23604183",
    oss_sha1 = "18f839ebb58babae4ae35900ef33cdf065d0cbc5",
    oss_url = "https://github.com/facebook/buck2/releases/download/androidToolchain%2F2025-04-03/jar_builder_main.jar",
)
'''


class MavenUrlTest(unittest.TestCase):
    def test_a_coordinate_without_a_repository_is_maven_centrals(self):
        self.assertEqual(
            prelude.maven_url("mvn:com.google.guava:guava:jar:33.2.0-jre"),
            "https://repo1.maven.org/maven2/com/google/guava/guava/33.2.0-jre/guava-33.2.0-jre.jar",
        )

    def test_a_classifier_and_a_repository_are_kept(self):
        self.assertEqual(
            prelude.maven_url("mvn:https://maven.google.com:com.android.tools:r8:jar:linux:8.7.18"),
            "https://maven.google.com/com/android/tools/r8/8.7.18/r8-8.7.18-linux.jar",
        )
        self.assertEqual(
            prelude.maven_url("mvn:org.jetbrains:annotations:src:13.0"),
            "https://repo1.maven.org/maven2/org/jetbrains/annotations/13.0/annotations-13.0-sources.jar",
        )

    def test_a_plain_url_is_itself(self):
        self.assertEqual(prelude.maven_url("https://example.com/a.jar"), "https://example.com/a.jar")


class DownloadsTest(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="prelude-"))
        (self.dir / "third-party").mkdir()

    def test_every_wrapper_call_gives_its_sha1_and_url(self):
        (self.dir / "third-party/BUCK").write_text(BUCK)
        self.assertEqual(
            prelude.downloads(self.dir),
            {
                "6ca5d1e7f3e2f9a1b0c4d8e7f6a5b4c3d2e1f0a9": "https://repo1.maven.org/maven2/com/google/guava/guava/33.2.0-jre/guava-33.2.0-jre.jar",
                "0adb7cac2ab282100e2b8b6d66b997a69ed06adf": "https://maven.google.com/com/android/tools/sdklib/31.7.3/sdklib-31.7.3.aar",
                "18f839ebb58babae4ae35900ef33cdf065d0cbc5": "https://github.com/facebook/buck2/releases/download/androidToolchain%2F2025-04-03/jar_builder_main.jar",
            },
        )

    def test_a_call_it_cannot_read_stops_it(self):
        (self.dir / "third-party/BUCK").write_text('third_party_jar(name = "x", sha1 = "y", url = "z")\n')
        with self.assertRaises(SystemExit):
            prelude.downloads(self.dir)


class DigestsTest(unittest.TestCase):
    def test_the_table_reads_back_as_written(self):
        digests = {"a" * 40: ("b" * 64, 1234), "c" * 40: None}
        path = Path(tempfile.mkdtemp(prefix="prelude-")) / "digests.bzl"
        path.write_text(prelude.render_digests(digests, {"a" * 40: "https://a", "c" * 40: "https://c"}))
        self.assertEqual(prelude.read_digests(path), digests)


if __name__ == "__main__":
    unittest.main()
