#!/usr/bin/env python3
"""The pinned simulators: which device a recipe drives, and how it is pinned.

Every capture and measurement names a *kind* of device, ``golden`` or
``small``, never a device. Which device a kind means is decided here, once:

- Inside a wt worktree the device is the one wt leased for this command,
  named in ``WT_LEASE_IPHONE`` or ``WT_LEASE_IPHONE_SMALL``. A recipe that
  reaches this module inside a worktree without a lease is refused, because
  driving a device another checkout may be using is exactly the corruption
  the lease exists to prevent.
- Outside a worktree, which is CI or a bare checkout, the device is the first
  of the same naming scheme, so the only difference between the two
  environments is whether anything coordinates.

wt calls the subcommands at the bottom to create, probe and prepare a device;
the recipes call ``ready`` to get the udid of the device their kind means.
"""

import json
import os
import plistlib
import subprocess
import sys

RUNTIME = "com.apple.CoreSimulator.SimRuntime.iOS-26-5"
BUNDLE_ID = "sh.amux.app"

# The golden device is the one all budgets and baselines are pinned to; the
# small one exists so the design is checked at the narrowest supported width.
KINDS = {
    "golden": {
        "device_type": "com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro",
        "lease": "WT_LEASE_IPHONE",
        "fallback": "amux-iphone-1",
    },
    "small": {
        "device_type": "com.apple.CoreSimulator.SimDeviceType.iPhone-SE-3rd-generation",
        "lease": "WT_LEASE_IPHONE_SMALL",
        "fallback": "amux-small-1",
    },
}
DEVICES = tuple(KINDS)

# The names the kinds went by before the lease existed. A branch that predates
# it still asks for these, and gets the device its kind means today.
LEGACY_NAMES = {"amux-golden": "golden", "amux-small": "small"}


def device_name(kind: str) -> str:
    """The device this kind means for this process."""
    if kind not in KINDS:
        raise SystemExit(f"no pinned simulator kind named {kind}; one of {', '.join(KINDS)}")
    entry = KINDS[kind]
    leased = os.environ.get(entry["lease"])
    if leased:
        return leased
    if os.environ.get("WT_TARGET"):
        pool = "iphone" if kind == "golden" else "iphone-small"
        raise SystemExit(
            f"no {kind} simulator is leased for this command: run it under "
            f"`scripts/with {pool} -- ...` so wt hands it a device of its own"
        )
    return entry["fallback"]


def run(*command: str, timeout: int = 120) -> str:
    return subprocess.run(
        command, check=True, text=True, capture_output=True, timeout=timeout,
    ).stdout.strip()


def settle(*command: str, attempts: int = 3) -> None:
    """Apply a device setting, asking again when the answer is slow.

    A device that has just booted answers slowly while it finishes coming up,
    and slower still when the runner is compiling the app beside it: a runner
    building the app has taken more than two minutes to apply an appearance.
    Applying a setting twice is harmless and an expired attempt says nothing
    about whether it took, so it is simply asked again.
    """
    for attempt in range(attempts):
        try:
            run(*command)
            return
        except subprocess.TimeoutExpired:
            if attempt + 1 == attempts:
                raise
            print(f"{' '.join(command[1:4])} took too long; asking again", flush=True)


def device_inventory(attempts: int = 3) -> dict:
    """Every simulator this machine could run.

    CoreSimulator serialises this query behind whatever else is talking to it,
    and a machine that is booting or shutting down a device can hold it far
    past the timeout. An expired read says nothing about the machine's
    devices, so ask again rather than create a second pinned simulator.
    """
    for attempt in range(attempts):
        try:
            return json.loads(
                run("xcrun", "simctl", "list", "devices", "available", "-j", timeout=180)
            )
        except subprocess.TimeoutExpired:
            if attempt + 1 == attempts:
                raise


def find(name: str) -> dict | None:
    """The named device on the pinned runtime, or None."""
    matching = [
        device for device in device_inventory()["devices"].get(RUNTIME, [])
        if device["name"] == name
    ]
    if len(matching) > 1:
        raise RuntimeError(f"Multiple {name} simulators on {RUNTIME}")
    return matching[0] if matching else None


def ensure(kind_or_name: str, device_type: str | None = None) -> str:
    """Return the udid of the device a kind means, creating it when absent.

    Given a kind, the device is the one ``device_name`` resolves and its type
    is the kind's. Given a device name, ``device_type`` says what to create.
    """
    kind = LEGACY_NAMES.get(kind_or_name, kind_or_name)
    if kind in KINDS:
        name, device_type = device_name(kind), KINDS[kind]["device_type"]
    else:
        name = kind_or_name
        if device_type is None:
            raise SystemExit(f"{name} is not a simulator kind; say what to create")
    device = find(name)
    if device:
        if device["deviceTypeIdentifier"] != device_type:
            raise RuntimeError(f"{name} must be a {device_type.rsplit('.', 1)[-1]}")
        return device["udid"]
    return run("xcrun", "simctl", "create", name, device_type, RUNTIME)


# What the pinned region is, as a `defaults read` would print it back. The
# 12-hour clock is pinned explicitly rather than left to the region: a device
# created on a Mac set to 24-hour time inherits that setting and would
# photograph 09:41 where every baseline reads 9:41.
REGION = {
    "AppleLanguages": ("-array", ["en-US"], "en-US"),
    "AppleLocale": ("-string", ["en_US"], "en_US"),
    "AppleICUForce24HourTime": ("-bool", ["false"], "0"),
}


def spawn(udid: str, *command: str, attempts: int = 3) -> str:
    """Run a command inside the device, asking again when the device is slow.

    A device that `bootstatus` has just called booted can still hold a
    spawned process past the timeout while it finishes coming up; a runner
    creating its second device has taken over two minutes to answer
    `defaults read`. An expired attempt says nothing about the answer, so it
    is asked again rather than failing the pin.
    """
    for attempt in range(attempts):
        try:
            return run("xcrun", "simctl", "spawn", udid, *command)
        except subprocess.TimeoutExpired:
            if attempt + 1 == attempts:
                raise
            print(f"{udid}: {' '.join(command[:2])} took too long; asking again")
    raise AssertionError("unreachable")


def read_default(udid: str, key: str) -> str | None:
    """What the device says the key is, or None when it has never been set."""
    try:
        printed = spawn(udid, "defaults", "read", ".GlobalPreferences", key)
    except subprocess.CalledProcessError:
        return None
    # An array prints over several lines wrapped in parentheses; every value
    # this module writes is a single one, so the punctuation is noise.
    return printed.strip().strip("()").strip().strip('"')


def write_region(udid: str) -> bool:
    """Pin language, region and clock; say whether anything actually moved."""
    changed = False
    for key, (kind, arguments, settled) in REGION.items():
        if read_default(udid, key) == settled:
            continue
        spawn(udid, "defaults", "write", ".GlobalPreferences", key, kind, *arguments)
        changed = True
    return changed


def apps_listed(launchctl: str) -> list[str]:
    """The bundle identifiers among launchd's job labels."""
    found = []
    for line in launchctl.splitlines():
        label = line.split("\t")[-1]
        if label.startswith("UIKitApplication:"):
            found.append(label[len("UIKitApplication:"):].split("[", 1)[0])
    return found


def running_apps(udid: str) -> list[str]:
    """The bundle identifiers of every app process the device has running."""
    return apps_listed(spawn(udid, "launchctl", "list"))


def quit_apps(udid: str) -> None:
    """Terminate every app on the device so the next launch starts from Home.

    An app launched while another app is in front carries that app's name in
    its status bar, as a way back to it. A developer's simulator can have
    anything in front from ordinary use; a capture taken then differs from the
    same capture on a runner by exactly that breadcrumb.
    """
    for bundle in running_apps(udid):
        if bundle in SYSTEM_PROCESSES or bundle.startswith("com.apple.chrono."):
            continue
        # Best effort: a process that will not go quietly is not worth a
        # failed run, since nothing a runner has in front leaves a breadcrumb.
        try:
            subprocess.run(
                ["xcrun", "simctl", "terminate", udid, bundle],
                capture_output=True, timeout=20,
            )
        except subprocess.TimeoutExpired:
            print(f"{udid}: {bundle} did not terminate in 20 seconds; carrying on")


# Launched by SpringBoard for its own use, never in front, and one of them
# (Spotlight) does not answer a terminate on a GitHub runner at all.
SYSTEM_PROCESSES = {"com.apple.Spotlight", "com.apple.family"}


SIMULATOR_APP = "com.apple.iphonesimulator"


def disconnect_hardware_keyboard(udid: str) -> bool:
    """Pin Simulator.app's hardware keyboard off for the device.

    With the Mac's keyboard connected the software keyboard never rises, so a
    screen whose field takes focus is captured without it; a headless device,
    which is what a runner has, always raises it. Simulator.app reads this
    when it attaches a window, so a change takes effect the next time it is
    opened. Goes through `defaults` rather than the file so the preference
    daemon's copy is the one that changes. Answers whether anything moved.
    """
    exported = subprocess.run(
        ["defaults", "export", SIMULATOR_APP, "-"],
        check=True, capture_output=True, timeout=60,
    ).stdout
    preferences = plistlib.loads(exported) if exported.strip() else {}
    device = preferences.setdefault("DevicePreferences", {}).setdefault(udid, {})
    if device.get("ConnectHardwareKeyboard") is False:
        return False
    device["ConnectHardwareKeyboard"] = False
    subprocess.run(
        ["defaults", "import", SIMULATOR_APP, "-"],
        check=True, input=plistlib.dumps(preferences), capture_output=True, timeout=60,
    )
    return True


def pin(udid: str) -> None:
    """Boot the device and fix everything a capture would otherwise vary on."""
    run("xcrun", "simctl", "bootstatus", udid, "-b", timeout=600)
    if disconnect_hardware_keyboard(udid):
        print(f"{udid}: hardware keyboard pinned off; reopen Simulator.app for it to apply")
    # Language and region are read by an app at launch, so they are set before
    # anything under test is installed rather than between screens.
    if write_region(udid):
        # SpringBoard reads the region once, when it starts, and it drew the
        # status bar before these values existed. Without a second boot the
        # very first capture on a newly created device carries the Mac's own
        # clock format rather than the pinned one, and a baseline photographed
        # then disagrees with every later run.
        run("xcrun", "simctl", "shutdown", udid, timeout=300)
        run("xcrun", "simctl", "bootstatus", udid, "-b", timeout=600)
    settle("xcrun", "simctl", "ui", udid, "appearance", "light")
    quit_apps(udid)
    settle(
        "xcrun", "simctl", "status_bar", udid, "override",
        "--time", "9:41",
        "--dataNetwork", "wifi", "--wifiMode", "active", "--wifiBars", "3",
        "--cellularMode", "active", "--cellularBars", "4",
        "--batteryState", "charged", "--batteryLevel", "100",
    )


def voice_over(udid: str, running: bool) -> None:
    """Turns the device's own screen reader on or off.

    VoiceOver is a system setting, so it can only be reached from out here:
    nothing inside an app can start a VoiceOver session, and a driver that
    wanted the app drawn and read the way a blind reader gets it has to change
    the device first. The preferences are what Settings writes; the two
    notifications are what makes every running process notice, because the
    accessibility state is cached per process and an app launched afterwards
    reads the cache rather than the file.

    Whether it took is not asked here. The app is the only thing that can
    answer that, and the journey asks it through the door.
    """
    for key in ("VoiceOverTouchEnabled", "ApplicationAccessibilityEnabled",
                "AccessibilityEnabled"):
        run("xcrun", "simctl", "spawn", udid, "defaults", "write",
            "com.apple.Accessibility", key, "-int", "1" if running else "0")
    for cache in ("com.apple.accessibility.cache.app.ax", "com.apple.accessibility.cache.ax"):
        run("xcrun", "simctl", "spawn", udid, "notifyutil", "-p", cache)


def clean(udid: str) -> None:
    """Remove what a previous use could leave behind for the next one.

    Device state bleeds between recipes even inside one checkout: the
    accessibility audit turns VoiceOver on, the unit suites type into fields,
    and every driving recipe leaves the app and its pairings installed. Each
    lease starts from none of that.
    """
    subprocess.run(["xcrun", "simctl", "uninstall", udid, BUNDLE_ID],
                   capture_output=True, timeout=300)
    try:
        voice_over(udid, False)
    except subprocess.CalledProcessError:
        pass


def ready(kind: str) -> str:
    """The udid of the device this kind means, present, booted and pinned."""
    udid = ensure(kind)
    pin(udid)
    return udid


def main() -> None:
    arguments = sys.argv[1:]
    # The subcommands wt's pool recipes call, each about one named device.
    if arguments[:1] == ["exists"]:
        raise SystemExit(0 if find(arguments[1]) else 1)
    if arguments[:1] == ["create"]:
        name, kind = arguments[1], arguments[2]
        udid = ensure(name, KINDS[kind]["device_type"])
        pin(udid)
        print(f"{name}: {udid} (iOS 26.5, booted, en_US, 9:41, light)", flush=True)
        return
    if arguments[:1] == ["acquire"]:
        udid = find(arguments[1])["udid"]
        pin(udid)
        clean(udid)
        return
    # Named kinds, or both: create and boot the devices they mean. Booting one
    # takes minutes, and the suites that need only the golden device should
    # not wait for the small one.
    wanted = arguments or list(KINDS)
    unknown = [kind for kind in wanted if kind not in KINDS]
    if unknown:
        raise SystemExit(f"no pinned simulator kind named {', '.join(unknown)}")
    for kind in wanted:
        udid = ready(kind)
        print(f"{kind} ({device_name(kind)}): {udid} (iOS 26.5, booted, en_US, 9:41, light)",
              flush=True)


if __name__ == "__main__":
    main()
