"""The phone plugged into this Mac, as the recipes that build for one find it.

A driven build for a phone is signed under the Team ID in the local signing
file and installed over the phone's USB link. The link is a point-to-point
IPv6 network of its own between the Mac and the phone, which is how the Mac
reaches anything the app listens on without either side listening on Wi-Fi.
"""

from dataclasses import dataclass
import json
from pathlib import Path
import subprocess
import tempfile

SIGNING_FILE = Path("apps/apple/Signing.local.xcconfig")
TEAM_SETTING = "DEVELOPMENT_TEAM"


@dataclass(frozen=True)
class Phone:
    name: str
    # What devicectl names the phone by.
    identifier: str
    # What xcodebuild and xctrace name it by.
    udid: str
    model: str
    os_version: str
    # The phone's own address on the USB link.
    address: str


def connected() -> Phone:
    """The one phone plugged in, paired and ready to be driven, or an error
    that says which of those it is not."""
    with tempfile.TemporaryDirectory() as scratch:
        listing = Path(scratch) / "devices.json"
        subprocess.run(
            ["xcrun", "devicectl", "list", "devices", "--quiet", "--json-output", str(listing)],
            check=True, capture_output=True, timeout=60)
        devices = json.loads(listing.read_text())["result"]["devices"]
    phones = [
        device for device in devices
        if device["hardwareProperties"].get("reality") == "physical"
        and device["hardwareProperties"].get("platform") == "iOS"
        and device["connectionProperties"].get("transportType") == "wired"
    ]
    if len(phones) != 1:
        raise SystemExit(
            f"expected one phone plugged in over USB, found {len(phones)}; "
            "`xcrun devicectl list devices` lists what this Mac sees")
    phone = phones[0]
    connection = phone["connectionProperties"]
    properties = phone["deviceProperties"]
    if connection.get("pairingState") != "paired":
        raise SystemExit(f"{properties['name']} is not paired with this Mac: unlock it and trust the Mac")
    if properties.get("developerModeStatus") != "enabled":
        raise SystemExit(
            f"{properties['name']} does not have Developer Mode on: "
            "Settings > Privacy & Security > Developer Mode, then restart it")
    if connection.get("tunnelState") != "connected" or not connection.get("tunnelIPAddress"):
        raise SystemExit(f"{properties['name']} has no USB link up yet: unlock it and try again")
    return Phone(
        name=properties["name"],
        identifier=phone["identifier"],
        udid=phone["hardwareProperties"]["udid"],
        model=phone["hardwareProperties"]["productType"],
        os_version=properties["osVersionNumber"],
        address=connection["tunnelIPAddress"],
    )


def development_team() -> str:
    """The Team ID a build for a phone is signed under. Read rather than
    passed to xcodebuild as an xcconfig, so the untracked file can set this
    and nothing else about the build."""
    if SIGNING_FILE.exists():
        for line in SIGNING_FILE.read_text().splitlines():
            key, _, value = line.split("//")[0].partition("=")
            if key.strip() == TEAM_SETTING and value.strip():
                return value.strip()
    raise SystemExit(
        f"{SIGNING_FILE} sets no {TEAM_SETTING}; write it with your Team ID "
        f"({TEAM_SETTING} = ABCDE12345) to build for a phone")
