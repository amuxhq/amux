#!/usr/bin/env python3
"""Carry a real App Store sandbox purchase from a phone to the account
service, and read back what the account may then do.

An App Store sandbox transaction exists in exactly one place: a physical
iPhone signed into a sandbox Apple Account, running a development-signed
build. Apple's engineers say so plainly — sandbox sign-in is not supported on
the Simulator — and the StoreKit configuration this repository commits is
StoreKit Testing, whose transactions are signed by the local test certificate
and are not sandbox transactions. So this recipe never runs on a simulator and
never invents a transaction: when the phone, the signing or the account is not
here, it says which and stops.

Two ways to run it:

  --preflight            report what is present and missing on this Mac and
                         exit 0, touching neither StoreKit, the store nor
                         amux.sh.
  --transaction-file P   post a transaction already captured on a phone
                         through POST /api/purchases as the device QA
                         account, and read the two answers back.
  (neither)              build and install on the connected phone, ask the
                         person to buy, and watch amux.sh until the purchase
                         lands or a bound elapses.

The account is the device QA account and only that: the one
AMUX_QA_DEVICE_EMAIL names, in the environment or in the operator's untracked
file. There is no built-in address, and the end-to-end account the other
recipes use is refused. Nothing that identifies the account reaches the
output, an evidence file or any committed file.
"""

from pathlib import Path
import argparse
import json
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(1, str(Path(__file__).resolve().parents[1] / "apps/apple/Tools"))
import ios_project
import qa_cloud
from qa_cloud import BASE, fail, masked

qa_cloud.PROGRAM = "qa-sandbox-purchase"

ADDRESS_VARIABLE = "AMUX_QA_DEVICE_EMAIL"
# The end-to-end account. Named here so it can be refused, never used.
OTHER_ACCOUNT_VARIABLE = "AMUX_QA_EMAIL"
# The Team ID, written by whoever owns this Mac's signing identity.
# Untracked: a Team ID is not a secret but it is not this repository's either,
# and a committed one would build somebody else's app.
SIGNING_FILE = Path("apps/apple/Signing.local.xcconfig")
REQUIRED_SETTINGS = ("DEVELOPMENT_TEAM",)
# The app's identity, committed in apps/apple/project.yml because it is the same
# everywhere: this build is the next version of the listing already on the
# App Store, and the subscriptions belong to that listing. A local override
# would sign a different app and find no products; the test beside this
# recipe checks the two spellings still agree.
BUNDLE_ID = "sh.amux.app"
DERIVED_DATA = Path("target/ios/DeviceDerivedData")
PRODUCTS = ("amux_pro_monthly", "amux_pro_yearly")
# How long the watch waits for a purchase the person is making by hand.
DEFAULT_MINUTES = 10


class Check:
    """One line of the preflight: a fact, whether it holds, and what to do
    about it when it does not."""

    def __init__(self, what: str, held: bool, detail: str,
                 by_hand: bool = False) -> None:
        self.what = what
        self.held = held
        self.detail = detail
        # A fact no Mac can answer: only the person holding the phone knows
        # it. Reported as such rather than guessed at.
        self.by_hand = by_hand

    def line(self) -> str:
        mark = "present" if self.held else ("confirm" if self.by_hand else "MISSING")
        return f"  [{mark}] {self.what}: {self.detail}"


def device_address() -> tuple[str, Check]:
    """The device QA account, and the check that says where it came from."""
    found = qa_cloud.address(ADDRESS_VARIABLE)
    if not found:
        return "", Check(
            "the device QA account's address", False,
            f"nothing names one. Set {ADDRESS_VARIABLE} in the environment, "
            f"or add it to {qa_cloud.ADDRESS_FILE}, which today sets only "
            f"{OTHER_ACCOUNT_VARIABLE} — a different account this recipe "
            "never uses. No address is built in")
    if found.strip().lower() == qa_cloud.address(OTHER_ACCOUNT_VARIABLE).strip().lower():
        return "", Check(
            "the device QA account's address", False,
            f"{ADDRESS_VARIABLE} names the same account as "
            f"{OTHER_ACCOUNT_VARIABLE}. That account is the end-to-end one and "
            "is not a sandbox account; this recipe runs as the device account "
            "or not at all")
    return found, Check(
        "the device QA account's address", True,
        f"{masked(found)}, from {ADDRESS_VARIABLE}")


def credentials(who: str) -> Check:
    if not who:
        return Check("the account's password", False,
                     "no address to look one up for")
    if qa_cloud.keychain_holds(who):
        return Check("the account's password", True,
                     f"in this Mac's login keychain under service "
                     f"{qa_cloud.KEYCHAIN_SERVICE}")
    return Check(
        "the account's password", False,
        f"the login keychain has none for {masked(who)} under service "
        f"{qa_cloud.KEYCHAIN_SERVICE}. Add one with: security "
        f'add-generic-password -s {qa_cloud.KEYCHAIN_SERVICE} -a "$'
        f'{ADDRESS_VARIABLE}" -w')


def devices() -> list[dict]:
    """Every phone this Mac knows, as devicectl reports them."""
    listed = subprocess.run(
        ["xcrun", "devicectl", "list", "devices", "--quiet",
         "--json-output", "/dev/stdout"],
        capture_output=True, text=True, timeout=120)
    if listed.returncode != 0:
        return []
    try:
        return json.loads(listed.stdout)["result"]["devices"]
    except (KeyError, TypeError, ValueError):
        return []


def connected(phone: dict) -> bool:
    """Reachable now, not merely known.

    devicectl lists a phone it has ever been paired with whether or not it is
    on the desk, so pairing alone would let this recipe promise an install it
    cannot perform."""
    properties = phone.get("connectionProperties", {})
    return (properties.get("tunnelState") in ("connected", "available")
            and properties.get("pairingState") == "paired")


def phone_check() -> tuple[dict | None, Check]:
    known = devices()
    reachable = [phone for phone in known if connected(phone)]
    if reachable:
        model = reachable[0].get("deviceProperties", {}).get("name", "a phone")
        return reachable[0], Check("a connected phone", True,
                                   f"{model}, reachable over devicectl")
    if known:
        return None, Check(
            "a connected phone", False,
            f"{len(known)} phone(s) paired with this Mac but none reachable "
            "now — plug one in or bring it onto this network, then check "
            "xcrun devicectl list devices")
    return None, Check("a connected phone", False,
                       "xcrun devicectl knows no phone at all")


def signing_settings() -> dict[str, str]:
    """The assignments in the local signing file. Read rather than sourced:
    an untracked file this recipe executed would be a way to run anything."""
    if not SIGNING_FILE.exists():
        return {}
    settings = {}
    for line in SIGNING_FILE.read_text().splitlines():
        line = line.split("//")[0].strip()
        if "=" not in line or line.startswith("#"):
            continue
        key, _, value = line.partition("=")
        settings[key.strip()] = value.strip()
    return settings


def signing_check() -> tuple[dict[str, str], Check]:
    settings = signing_settings()
    if not settings:
        return {}, Check(
            "a Team ID and signing", False,
            f"{SIGNING_FILE} is not here. Write it with "
            f"{' and '.join(REQUIRED_SETTINGS)}; .gitignore already covers it, "
            "because this repository builds simulator-only and has no signing "
            "identity of its own")
    absent = [name for name in REQUIRED_SETTINGS if not settings.get(name)]
    if absent:
        return settings, Check("a Team ID and signing", False,
                               f"{SIGNING_FILE} does not set "
                               f"{' or '.join(absent)}")
    ignored = subprocess.run(["git", "check-ignore", "-q", str(SIGNING_FILE)],
                             capture_output=True, timeout=60).returncode == 0
    if not ignored:
        return settings, Check(
            "a Team ID and signing", False,
            f"{SIGNING_FILE} is not ignored by git. It names a signing "
            "identity and must never be committed")
    return settings, Check(
        "a Team ID and signing", True,
        f"{SIGNING_FILE} supplies the Team ID to build under")


def store_check(confirmed: bool) -> Check:
    """A fact this Mac cannot check. App Store Connect and the billing
    provider are asked with credentials nothing here holds, so the person
    running the recipe says whether it holds and the recipe says so plainly."""
    return Check(
        "both products in App Store Connect and known to the billing provider",
        confirmed,
        f"{BUNDLE_ID} must carry {' and '.join(PRODUCTS)} as subscriptions in "
        "App Store Connect, and the billing provider must know that bundle "
        "id, or the purchase signs and nothing recognises it. Not checkable "
        "from this Mac"
        + ("; confirmed by --confirmed" if confirmed else
           "; pass --confirmed once it is true"),
        by_hand=not confirmed)


def sandbox_account_check(who: str, confirmed: bool) -> Check:
    """Also not checkable from here: iOS does not tell a Mac which sandbox
    Apple Account a phone is signed into."""
    return Check(
        "the sandbox Apple Account on the phone", confirmed,
        f"Settings › Developer › Sandbox Apple Account must be "
        f"{masked(who) if who else 'the device QA account'}, and the phone "
        "must not be signed into it in the App Store proper. Not checkable "
        "from this Mac"
        + ("; confirmed by --confirmed" if confirmed else
           "; pass --confirmed once it is true"),
        by_hand=not confirmed)


def preflight(confirmed: bool, for_transaction_file: bool
              ) -> tuple[str, dict | None, dict[str, str], list[Check]]:
    """Everything the run needs, asked without doing any of it."""
    who, address_check = device_address()
    checks = [address_check, credentials(who)]
    phone, settings = None, {}
    if for_transaction_file:
        # A transaction captured earlier needs no phone and no build: the
        # purchase already happened, and what is left is the post and the two
        # reads. Asking for signing here would refuse a run that can succeed.
        return who, phone, settings, checks
    phone, found = phone_check()
    settings, signing = signing_check()
    checks += [found, signing, store_check(confirmed),
               sandbox_account_check(who, confirmed)]
    return who, phone, settings, checks


def report(checks: list[Check]) -> bool:
    print(f"qa-sandbox-purchase preflight on this Mac, against the App Store "
          f"sandbox and {BASE}:")
    for check in checks:
        print(check.line())
    missing = [check for check in checks if not check.held]
    if missing:
        print(f"{len(missing)} of {len(checks)} not satisfied: "
              + "; ".join(check.what for check in missing))
    else:
        print("everything a sandbox purchase needs is here")
    return not missing


def build_and_install(phone: dict, settings: dict[str, str]) -> None:
    """A development-signed build on the phone. Never a simulator: a simulator
    has no sandbox to buy in."""
    identifier = phone["identifier"]
    ios_project.generate()
    print("building a development-signed build for the phone", flush=True)
    subprocess.run([
        "xcodebuild", "build",
        "-project", "apps/apple/Amux.xcodeproj",
        "-scheme", "Amux",
        "-configuration", "Debug",
        "-destination", f"id={identifier}",
        "-derivedDataPath", str(DERIVED_DATA),
        "-allowProvisioningUpdates",
        "-quiet",
        "CODE_SIGNING_ALLOWED=YES",
        "CODE_SIGN_STYLE=Automatic",
        f"DEVELOPMENT_TEAM={settings['DEVELOPMENT_TEAM']}",
    ], check=True, timeout=2400)
    application = DERIVED_DATA / "Build/Products/Debug-iphoneos/Amux.app"
    if not application.is_dir():
        fail(f"{application} was not produced")
    print("installing it on the phone", flush=True)
    subprocess.run(
        ["xcrun", "devicectl", "device", "install", "app",
         "--device", identifier, str(application)],
        check=True, timeout=900)


def watch(who: str, secret: str, minutes: int) -> None:
    """Ask amux.sh the two questions until the purchase has landed.

    The entitlement read and the relay's own gate are asked separately on
    purpose: they are answered from different places, and a purchase that
    moved one without the other is exactly the failure worth catching."""
    deadline = time.monotonic() + minutes * 60
    issued = qa_cloud.signed_in(who, secret)
    token = issued["access_token"]
    print(f"signed {masked(who)} in at {BASE}; "
          f"the access token claims {qa_cloud.tier(token)}", flush=True)
    while True:
        _, said = qa_cloud.read_entitlement(token)
        issued_credential, connect_said = qa_cloud.connect(token)
        print(f"entitlement: {said}", flush=True)
        print(f"connect: {connect_said}", flush=True)
        if issued_credential:
            print("the sandbox purchase reached the account service: the "
                  "relay now issues this account a credential where it "
                  "refused one before", flush=True)
            return
        if time.monotonic() >= deadline:
            fail(f"{minutes} minutes passed and the relay still refuses this "
                 "account a credential. The purchase either was not made, was "
                 "not posted, or was not recognised on the other side")
        time.sleep(20)


def post_transaction(who: str, secret: str, transaction: Path) -> None:
    """The other road to the same two answers: a transaction a phone already
    signed, handed to the account service by hand."""
    signed = transaction.read_text().strip()
    if not signed:
        fail(f"{transaction} is empty")
    if signed.count(".") < 2:
        fail(f"{transaction} does not look like a signed transaction: the "
             "App Store's JWS is three dot-separated parts")
    issued = qa_cloud.signed_in(who, secret)
    token = issued["access_token"]
    print(f"signed {masked(who)} in at {BASE}", flush=True)
    status, body = qa_cloud.ask(
        f"{BASE}/api/purchases", token, "POST",
        json.dumps({"signed_transaction": signed}))
    if status == 200:
        print("purchases: taken, and the account's subscription came back "
              "with it")
    elif status == 202:
        print("purchases: taken, but the billing provider has not turned it "
              "into a subscription yet")
    else:
        said = ""
        try:
            said = json.loads(body).get("error_description", "")
        except ValueError:
            pass
        fail(f"the account service refused the transaction with {status}"
             + (f": {said}" if said else ""))
    _, said = qa_cloud.read_entitlement(token)
    print(f"entitlement: {said}")
    _, connect_said = qa_cloud.connect(token)
    print(f"connect: {connect_said}")


def main() -> None:
    parser = argparse.ArgumentParser(
        prog="qa-sandbox-purchase", description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--preflight", action="store_true",
                        help="report what is here and stop")
    parser.add_argument("--confirmed", action="store_true",
                        help="the two facts only the person at the phone can "
                             "answer are true")
    parser.add_argument("--transaction-file", type=Path,
                        help="a signed transaction captured on a phone, "
                             "posted through the account service as this "
                             "account. Never committed")
    parser.add_argument("--minutes", type=int, default=DEFAULT_MINUTES,
                        help="how long to watch for the purchase to land")
    arguments = parser.parse_args()

    from_file = arguments.transaction_file is not None
    who, phone, settings, checks = preflight(arguments.confirmed, from_file)
    if from_file:
        checks.append(Check(
            "the captured transaction", arguments.transaction_file.is_file(),
            f"{arguments.transaction_file}"
            if arguments.transaction_file.is_file()
            else f"{arguments.transaction_file} is not a file"))
    satisfied = report(checks)

    if arguments.preflight:
        # Reporting is the whole job here: nothing is built, nothing is
        # bought, and amux.sh is not touched. An incomplete Mac is a true
        # answer, not a failure.
        return
    if not satisfied:
        raise SystemExit(1)

    secret = qa_cloud.password(who)
    if from_file:
        post_transaction(who, secret, arguments.transaction_file)
        return
    build_and_install(phone, settings)
    print()
    print(f"On the phone, open amux, sign in as {masked(who)} and buy one of "
          f"{' or '.join(PRODUCTS)} on the paywall.")
    print("The App Store will say [Environment: Sandbox]. If it does not, the "
          "phone is buying for real — stop.")
    print(f"Watching {BASE} for up to {arguments.minutes} minutes.", flush=True)
    print()
    watch(who, secret, arguments.minutes)


if __name__ == "__main__":
    main()
