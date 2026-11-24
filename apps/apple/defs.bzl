"""The Apple apps' targets in their own terms (build/apple.bzl has the rules).

A Swift package target is a `package_library`, and its test target a
`package_tests`: the library compiled as SwiftPM would compile it, and the
tests as a bundle run on a leased simulator, or on this Mac for a Mac
package. Every target builds for the simulator's development configuration
unless a platform is named (`--target-platforms`), and the apps pick theirs
(apps/apple/BUCK).
"""

load("//apps/apple:version.bzl", "BUILD_NUMBER", "MARKETING_VERSION")
load("//build:apple.bzl", "apple_binary", "apple_bundle", "apple_xctest", "swift_library")

# The configurations the apps are built in (build/platforms/BUCK).
SIMULATOR = "root//build/platforms:ios-sim-app-shipping-phone-dev"
MAC = "root//build/platforms:macos-app-shipping-phone-dev"

def bridge(what):
    """`what`, a target of the Rust bridge, configured as the app it is
    linked into needs it: for that app's platform, in the bridge's mode."""
    return select({
        "prelude//os:macos": select({
            "DEFAULT": (what, "//build/platforms:macos-app-shipping-phone-dev"),
            "root//build/constraints:release": (what, "//build/platforms:macos-app-shipping-release"),
        }),
        "prelude//os:iphoneos": select({
            "DEFAULT": select({
                "DEFAULT": (what, "//build/platforms:ios-app-shipping-phone-dev"),
                "root//build/constraints:release": (what, "//build/platforms:ios-app-shipping-release"),
            }),
            "root//build/constraints:simulator": select({
                "DEFAULT": (what, "//build/platforms:ios-sim-app-shipping-phone-dev"),
                "root//build/constraints:release": (what, "//build/platforms:ios-sim-app-shipping-release"),
            }),
        }),
    })

def copied(base, name):
    """SwiftPM's `.copy(name)` of a target in `base`: every file under
    base/name, at its path from `base` in the resource bundle."""
    return {path.removeprefix(base + "/"): path for path in native.glob([base + "/" + name + "/**"])}

def package_library(name, package, srcs, deps = [], resources = {}, mac = False, **kwargs):
    swift_library(
        name = name,
        package = package,
        srcs = srcs,
        deps = deps,
        resources = resources,
        default_target_platform = MAC if mac else SIMULATOR,
        visibility = ["PUBLIC"],
        **kwargs
    )

def package_tests(name, package, srcs, deps, resources = {}, mac = False, test_srcs = [], test_deps = [], env = {}):
    """A package's test target: the module, the test bundle around it, and
    `name` the test that runs it."""
    platform = MAC if mac else SIMULATOR
    swift_library(
        name = name + "-module",
        module_name = name,
        package = package,
        srcs = srcs,
        deps = deps,
        resources = resources,
        xctest = True,
        default_target_platform = platform,
    )
    apple_binary(
        name = name + "-binary",
        product_name = name,
        kind = "bundle",
        deps = [":" + name + "-module"],
        xctest = True,
        default_target_platform = platform,
    )
    apple_bundle(
        name = name + "-bundle",
        product_name = name,
        extension = "xctest",
        identifier = "sh.amux." + name,
        binary = ":" + name + "-binary",
        info = {"CFBundlePackageType": "BNDL", "CFBundleExecutable": name, "CFBundleName": name, "CFBundleIdentifier": "sh.amux." + name, "CFBundleInfoDictionaryVersion": "6.0", "CFBundleDevelopmentRegion": "en"},
        default_target_platform = platform,
    )
    apple_xctest(
        name = name,
        bundle = ":" + name + "-bundle",
        srcs = test_srcs,
        deps = test_deps,
        env = env,
        labels = ["mac"] if mac else ["simulator"],
        default_target_platform = platform,
    )

def ios_versions():
    """The iPhone app's two numbers, as its Info.plist names them
    (apps/apple/version.bzl)."""
    return {
        "MARKETING_VERSION": read_root_config("apple", "marketing_version", MARKETING_VERSION),
        "CURRENT_PROJECT_VERSION": read_root_config("apple", "build_number", BUILD_NUMBER),
    }
