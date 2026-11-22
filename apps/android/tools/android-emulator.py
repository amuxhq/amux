#!/usr/bin/env python3
"""Boot, pin, and manage the pinned Android emulator AVD (`amux-android-1`).

Ensures the pinned AVD is booted headless, waits for Android boot completion
and PackageManager readiness, disables system window/transition/animator
scales for deterministic settling, dismisses the keyguard, and prints a
machine-parseable summary line containing the adb serial (`emulator-5554`).
"""

from __future__ import annotations

import argparse
from pathlib import Path
import shutil
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).parent))
import android_sdk  # noqa: E402

PINNED_AVD = "amux-android-1"
PACKAGE_ID = "sh.amux.android"
DEFAULT_GPU = "auto"
DEFAULT_BOOT_TIMEOUT_SECONDS = 240


def find_tool(sdk_root: Path, relative: str, fallback_name: str) -> str:
    candidate = sdk_root / relative
    if candidate.is_file():
        return str(candidate)
    found = shutil.which(fallback_name)
    if found:
        return found
    raise RuntimeError(f"Android SDK tool {fallback_name!r} not found at {candidate} or on PATH")


def adb_path(sdk_root: Path) -> str:
    return find_tool(sdk_root, "platform-tools/adb", "adb")


def emulator_path(sdk_root: Path) -> str:
    return find_tool(sdk_root, "emulator/emulator", "emulator")


def parse_adb_devices(output: str) -> list[tuple[str, str]]:
    """Parse `adb devices` stdout into `(serial, state)` pairs."""
    devices: list[tuple[str, str]] = []
    for raw_line in output.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("List of devices") or line.startswith("*"):
            continue
        parts = line.split()
        if len(parts) >= 2:
            devices.append((parts[0], parts[1]))
    return devices


def parse_avd_name_output(output: str) -> str | None:
    """Extract the AVD name from `adb -s <serial> emu avd name` stdout."""
    for raw_line in output.splitlines():
        line = raw_line.strip()
        if not line or line == "OK":
            continue
        return line
    return None


def parse_emulator_serial_line(output: str, avd: str = PINNED_AVD) -> str | None:
    """Parse the serial from this script's summary line: `android (<avd>): <serial> (...)`."""
    prefix = f"android ({avd}): "
    for raw_line in output.splitlines():
        line = raw_line.strip()
        if line.startswith(prefix):
            rest = line[len(prefix):]
            serial = rest.split()[0] if rest.split() else ""
            if serial:
                return serial
    return None


def run_cmd(*command: str, timeout: int = 60, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        check=check,
        text=True,
        capture_output=True,
        timeout=timeout,
    )


def list_avds(emulator: str) -> list[str]:
    result = run_cmd(emulator, "-list-avds", timeout=30)
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


def avd_name_for_serial(adb: str, serial: str) -> str | None:
    result = run_cmd(adb, "-s", serial, "emu", "avd", "name", timeout=15, check=False)
    if result.returncode != 0:
        return None
    return parse_avd_name_output(result.stdout)


def find_running_avd(adb: str, avd: str) -> tuple[str, str] | None:
    """Return `(serial, state)` for a running emulator instance of `avd`, or None."""
    result = run_cmd(adb, "devices", timeout=30)
    for serial, state in parse_adb_devices(result.stdout):
        if not serial.startswith("emulator-"):
            continue
        if avd_name_for_serial(adb, serial) == avd:
            return (serial, state)
    return None


def emulator_launch_command(emulator: str, avd: str, *, gpu: str = DEFAULT_GPU) -> list[str]:
    return [
        emulator,
        "-avd",
        avd,
        "-no-window",
        "-no-audio",
        "-no-boot-anim",
        "-no-snapshot-save",
        "-gpu",
        gpu,
    ]


def wait_for_boot(adb: str, avd: str, timeout: int = DEFAULT_BOOT_TIMEOUT_SECONDS) -> str:
    """Wait for `avd` to appear online, complete Android boot, and bring up PackageManager."""
    deadline = time.monotonic() + timeout
    serial: str | None = None
    while time.monotonic() < deadline:
        running = find_running_avd(adb, avd)
        if running is not None:
            candidate, state = running
            if state == "device":
                serial = candidate
                break
        time.sleep(1.0)
    if serial is None:
        raise RuntimeError(f"emulator AVD {avd!r} did not come online within {timeout}s")

    while time.monotonic() < deadline:
        booted = run_cmd(
            adb, "-s", serial, "shell", "getprop", "sys.boot_completed",
            timeout=15, check=False,
        )
        if booted.returncode == 0 and booted.stdout.strip() == "1":
            pm = run_cmd(
                adb, "-s", serial, "shell", "pm", "path", "android",
                timeout=15, check=False,
            )
            if pm.returncode == 0 and "package:" in pm.stdout:
                return serial
        time.sleep(1.0)
    raise RuntimeError(f"emulator {serial} ({avd}) did not finish booting within {timeout}s")


def pin(adb: str, serial: str) -> None:
    """Disable animations, keep the display awake, and dismiss the keyguard."""
    commands = [
        ("shell", "settings", "put", "global", "window_animation_scale", "0"),
        ("shell", "settings", "put", "global", "transition_animation_scale", "0"),
        ("shell", "settings", "put", "global", "animator_duration_scale", "0"),
        ("shell", "svc", "power", "stayon", "true"),
        ("shell", "wm", "dismiss-keyguard"),
    ]
    for args in commands:
        run_cmd(adb, "-s", serial, *args, timeout=30)


def ensure_booted(
    avd: str = PINNED_AVD,
    *,
    gpu: str = DEFAULT_GPU,
    timeout: int = DEFAULT_BOOT_TIMEOUT_SECONDS,
    log_dir: Path | None = None,
) -> str:
    sdk_root = android_sdk.require_sdk()
    adb = adb_path(sdk_root)
    emulator = emulator_path(sdk_root)

    avds = list_avds(emulator)
    if avd not in avds:
        available = ", ".join(avds) if avds else "(none)"
        raise RuntimeError(f"AVD {avd!r} is not installed; available AVDs: {available}")

    # Ensure adb server is running before probing devices.
    run_cmd(adb, "start-server", timeout=30)

    running = find_running_avd(adb, avd)
    if running is None:
        logs = log_dir if log_dir is not None else Path("target/android")
        logs.mkdir(parents=True, exist_ok=True)
        log_path = logs / f"emulator-{avd}.log"
        cmd = emulator_launch_command(emulator, avd, gpu=gpu)
        with log_path.open("wb") as log_file:
            subprocess.Popen(
                cmd,
                stdin=subprocess.DEVNULL,
                stdout=log_file,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )

    serial = wait_for_boot(adb, avd, timeout=timeout)
    pin(adb, serial)
    return serial


def shutdown(avd: str = PINNED_AVD, *, timeout: int = 60) -> bool:
    sdk_root = android_sdk.require_sdk()
    adb = adb_path(sdk_root)
    running = find_running_avd(adb, avd)
    if running is None:
        return False
    serial, _ = running
    run_cmd(adb, "-s", serial, "emu", "kill", timeout=30, check=False)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if find_running_avd(adb, avd) is None:
            return True
        time.sleep(0.5)
    raise RuntimeError(f"emulator {serial} ({avd}) did not shut down within {timeout}s")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--avd",
        default=PINNED_AVD,
        help=f"AVD name to boot or manage (default: {PINNED_AVD})",
    )
    parser.add_argument(
        "--gpu",
        default=DEFAULT_GPU,
        help=f"Emulator GPU mode (default: {DEFAULT_GPU})",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=DEFAULT_BOOT_TIMEOUT_SECONDS,
        help=f"Boot timeout in seconds (default: {DEFAULT_BOOT_TIMEOUT_SECONDS})",
    )
    parser.add_argument(
        "--shutdown",
        action="store_true",
        help="Shut down the running pinned AVD if booted",
    )
    parser.add_argument(
        "--serial",
        action="store_true",
        help="Print only the adb serial of the running AVD (exits 1 if not running)",
    )
    args = parser.parse_args(argv)

    if args.shutdown:
        stopped = shutdown(args.avd)
        if stopped:
            print(f"android ({args.avd}): shut down", flush=True)
        else:
            print(f"android ({args.avd}): already stopped", flush=True)
        return 0

    if args.serial:
        sdk_root = android_sdk.require_sdk()
        running = find_running_avd(adb_path(sdk_root), args.avd)
        if running is None:
            return 1
        print(running[0], flush=True)
        return 0

    serial = ensure_booted(args.avd, gpu=args.gpu, timeout=args.timeout)
    print(f"android ({args.avd}): {serial} (booted, animations=0)", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
