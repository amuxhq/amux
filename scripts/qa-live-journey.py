#!/usr/bin/env python3
"""The whole product, once, against the real one: an entitled account, the
production relay, a daemon on this Mac and a real agent answering a question
asked from the phone.

Every other journey in this repository runs against a relay started beside it,
with credentials a harness minted. That proves the app; it cannot prove the
production handshake, because the thing being validated on the other side is a
signature nobody local can produce. This signs a QA account into
`https://amux.sh` exactly as the phone does, hands the app the session it got,
and then stands back: the app asks the account service what it may do, asks it
for a relay credential, and dials the relay that credential names. Nothing
here tells the app where to go.

The machine on the other side is this checkout's own daemon, on a profile made
for the run and destroyed after it, signed in through the CLI's device-code
flow with the same account. The agent is a real Claude session and the
exchange is one question with a one-word answer, because it spends real money.

    just ios qa-live-journey

Evidence a person runs by hand, never a test and never in CI. Nothing that
identifies the account reaches the output: the address is masked, the account
identifier and every token are replaced wherever they would otherwise appear,
and the password is read out of the login keychain at the moment it is needed
and dropped.
"""

from pathlib import Path
import json
import os
import pty
import re
import secrets
import subprocess
import sys
import tempfile
import threading
import time
import uuid

sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(1, str(Path(__file__).resolve().parents[1] / "apps/apple/Tools"))
import ios_simulators
import qa_cloud
from qa_cloud import BASE, fail, masked

SIMULATOR = "golden"
BUNDLE_ID = "sh.amux.app"
APPLICATION = Path("target/ios/DerivedData/Build/Products/Debug-iphonesimulator/Amux.app")

qa_cloud.PROGRAM = "qa-live-journey"

ADDRESS_VARIABLE = "AMUX_QA_EMAIL"
OUTPUT = Path("target/ios/qa-live-journey")
# One question with an answer that is not in the question, so a transcript
# holding the answer cannot be the phone reading back its own message.
QUESTION = "Answer with one word only, and no punctuation: what is the capital city of France?"
ANSWER = "Paris"


class Journal:
    """What the run did, on the way past, with every secret taken out.

    Written as it goes rather than at the end: a run that dies at the relay
    has to leave behind what it had reached, and the last line before the
    failure is usually the whole diagnosis."""

    def __init__(self, path: Path) -> None:
        self.path = path
        self.lines: list[str] = []
        self.secrets: list[str] = []

    def keep(self, secret: str | None) -> None:
        """Never print this again, wherever it turns up."""
        if secret and len(secret) > 6:
            self.secrets.append(secret)

    def scrub(self, text: str) -> str:
        for secret in self.secrets:
            text = text.replace(secret, "<redacted>")
        return text

    def say(self, line: str) -> None:
        line = self.scrub(line)
        self.lines.append(line)
        print(f"{qa_cloud.PROGRAM}: {line}", flush=True)
        self.write()

    def write(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.path.write_text("\n".join(self.lines) + "\n")

    def stop(self, why: str) -> None:
        self.say(f"FAILED {why}")
        fail(self.scrub(why))


def address() -> str:
    """The account to sign in as. There is no built-in one: an address this
    recipe reached by default would be an address committed to a public
    repository."""
    found = qa_cloud.address(ADDRESS_VARIABLE)
    if not found:
        fail(
            f"no account to sign in as. Set {ADDRESS_VARIABLE} in the "
            f"environment, or create {qa_cloud.ADDRESS_FILE} setting "
            f"{ADDRESS_VARIABLE}. That file is outside the tracked tree and "
            "must stay there."
        )
    return found


# MARK: - This checkout's daemon


def amux(*arguments: str, timeout: int = 120) -> subprocess.CompletedProcess:
    """The daemon this checkout runs, never a developer's. `AMUX_CONFIG` puts
    every profile, socket and key under `.wt/amux`, and `wt` puts this
    checkout's own build first on the path."""
    return subprocess.run(["amux", *arguments], capture_output=True, text=True,
                          timeout=timeout)


def daemon_is_up(journal: Journal) -> None:
    if amux("profiles").returncode != 0:
        journal.stop(
            "this checkout's daemon is not running. Start it with `wt run "
            "daemon`; it is a resource of this worktree and `wt remove` stops "
            "it again."
        )


def make_profile(journal: Journal, name: str) -> str:
    """Makes the profile this run uses and answers its identifier.

    Everything afterwards selects it by that identifier rather than by the
    name it was given: signing a profile in is what settles what it is called,
    and a run that went on addressing it by the name it started with would
    stop finding it the moment it succeeded."""
    made = amux("profile", "create", name)
    if made.returncode != 0:
        journal.stop(f"a profile for this run could not be made: {made.stderr.strip()}")
    found = re.search(
        r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", made.stdout)
    if not found:
        journal.stop("the daemon made a profile and did not say which one")
    journal.say(f"made a profile called {name} on this checkout's daemon, for this run alone")
    return found.group(0)


def remove_profile(journal: Journal, profile: str) -> None:
    amux("--profile", profile, "pair", "--cancel")
    gone = amux("--profile", profile, "profile", "delete", "--yes")
    journal.say(
        "removed this run's profile, with its keys, trust and credentials"
        if gone.returncode == 0 else
        "this run's profile could not be removed and is still on this daemon: "
        + journal.scrub(gone.stderr.strip()))


def sign_the_daemon_in(journal: Journal, browser: qa_cloud.Browser, profile: str) -> None:
    """Completes the CLI's device-code flow for a profile, as the same account.

    `amux login` prints a code and an address to type it at, then polls. What a
    person does between those two moments is done here, in the browser session
    that is already signed in: the code is read off the CLI's own output and
    approved on the account service's device page."""
    login = subprocess.Popen(
        ["amux", "--profile", profile, "login"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
    verification, code = "", ""
    deadline = time.time() + 120
    while time.time() < deadline and not (verification and code):
        line = login.stdout.readline()
        if not line:
            break
        found = re.search(r"https://\S+", line)
        if found and not verification:
            verification = found.group(0).split("?")[0]
        typed = re.search(r"enter code:\s*(\S+)", line)
        if typed:
            code = typed.group(1)
    if not (verification and code):
        login.kill()
        journal.stop("`amux login` never printed a device code to approve")
    approve(journal, browser, verification, code)
    # Drained rather than left in the pipe: what `amux login` prints once it
    # succeeds names the account it bound, and a full pipe would stop the
    # command that is about to be waited on.
    draining = threading.Thread(target=login.stdout.read, daemon=True)
    draining.start()
    try:
        login.wait(timeout=180)
    except subprocess.TimeoutExpired:
        login.kill()
        journal.stop("the device code was approved but `amux login` never finished")
    if login.returncode != 0:
        journal.stop(f"`amux login` refused the approved code (exit {login.returncode})")
    journal.say("the daemon's profile is signed in as the same account, through the "
                "device-code flow the CLI uses, approved in a browser session and not "
                "by a person")


def approve(journal: Journal, browser: qa_cloud.Browser, verification: str,
            code: str) -> None:
    """Presses Allow on the account service's device page."""
    page = f"{verification}?userCode={code}"
    status, _, body = browser.get(page)
    if status != 200 or "Authorize Device" not in body:
        journal.stop(f"the device page answered {status} rather than asking to authorize")
    fields = qa_cloud.form_fields(body) | {"button": "yes"}
    status, _, body = browser.post(page, fields)
    if status != 200 or "Device Authorized" not in body:
        journal.stop(f"the device page would not authorize this code: it answered {status}")


def profile_reaches_the_relay(journal: Journal, profile: str, seconds: int = 180) -> None:
    """Waits until the signed-in profile is connected, because a machine the
    relay is not holding is a machine the phone cannot be offered."""
    deadline = time.time() + seconds
    status = "nothing"
    while time.time() < deadline:
        status = profile_status(profile)
        # The daemon says how it is connected as well as that it is: a
        # connected profile reads `bound / connected (pro, quic)`, naming the
        # tier the relay granted and the carrier it settled on. Matching the
        # word rather than the end of the line keeps this from failing the
        # moment the daemon has more to say.
        connected = re.search(r"\bconnected\b", status)
        if connected and "not connected" not in status:
            journal.say(
                "the daemon's profile is connected to the production relay: "
                f"it reads {status!r}"
            )
            return
        if "subscription_required" in status or "authentication_required" in status:
            journal.stop(f"the relay turned this machine away: it reads {status!r}")
        time.sleep(2)
    journal.stop(
        f"the daemon's profile signed in but never reached the relay: it reads "
        f"{status!r} after {seconds}s")


def profile_status(profile: str) -> str:
    """One profile's state in the daemon's own words, such as `bound /
    connected`. Read out of the listing by identifier, because the label is the
    account's once it is signed in and the identifier never moves."""
    listed = amux("--profile", profile, "profiles")
    for line in listed.stdout.splitlines():
        if line.strip().startswith(profile):
            return re.split(r"\s{2,}", line.strip())[-1]
    return "not listed"


def machine_identity(profile: str) -> str:
    """Which machine this profile is, in the identifier the phone names it by.

    A code proves possession of one machine's offer and says nothing about
    which machine that is, so the phone is told the identity separately and
    finds the machine among the ones the relay is offering. No command prints
    a machine's own identity — every one of them prints somebody else's — so
    it is read from the profile's own state, which is where the daemon keeps
    it and what the machine puts in its own invitation."""
    root = Path(os.environ["AMUX_CONFIG"]).parent
    raw = (root / "profiles" / profile / "data" / "host_id").read_bytes()
    return str(uuid.UUID(bytes=raw))


def pairing_code(journal: Journal, profile: str) -> str:
    """The code the machine prints, held open for the run.

    The same offer the machine's own screen shows, kept reusable for a bounded
    while so a phone that arrives late still finds it; the daemon drops it when
    it expires, and the run cancels it on the way out either way."""
    digits = f"{secrets.randbelow(1_000_000):06d}"
    held = amux("--profile", profile, "pair", "--demo", "--pin", digits, "--for", "15m")
    if held.returncode != 0:
        journal.stop(f"the machine would not print a pairing code: {held.stderr.strip()}")
    journal.say("the machine is holding a pairing code open, the one its own screen "
                "would show")
    return digits


def pairing_invitation(journal: Journal, profile: str) -> tuple[str, subprocess.Popen]:
    """The invitation the machine shows, held open while the phone scans it.

    The same offer the machine's own screen puts on a QR code, printed as the
    link behind it because nothing here can point a camera at a screen. Unlike
    a code, an invitation names the machine and the account service it is on,
    so this is the only pairing route where the two ends have to agree about
    which cloud they are on — which is the whole reason it is run against the
    production one. The command holds the offer open for as long as it runs,
    so it is left running and stopped by the caller."""
    offered = subprocess.Popen(
        ["amux", "--profile", profile, "pair", "--qr", "--link"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
    deadline = time.time() + 60
    link = ""
    while time.time() < deadline and not link:
        line = offered.stdout.readline()
        if not line:
            break
        found = re.search(r"Pairing link: (\S+)", line)
        if found:
            link = found.group(1)
    if not link:
        offered.terminate()
        journal.stop("the machine would not show a pairing invitation")
    journal.say("the machine is showing the invitation its own screen shows, the one "
                "a phone scans")
    return link, offered


def stop_offering(offered: subprocess.Popen, profile: str) -> None:
    """Ends the invitation, so the machine can hold a different offer out."""
    offered.terminate()
    try:
        offered.wait(timeout=15)
    except subprocess.TimeoutExpired:
        offered.kill()
    amux("--profile", profile, "pair", "--cancel")


def start_the_agent(journal: Journal, profile: str) -> str:
    """Starts a real Claude session on that profile and answers its identifier.

    `amux new` opens the agent it creates, and refuses to run anywhere that is
    not a terminal, so it is given one. The client is let go once the daemon
    has the agent; what it was showing is the daemon's, and closing a terminal
    has never ended a session.

    The agent is created without a name, which is what makes its identifier
    readable off `amux ls` — the phone opens a conversation by identifier,
    and there is nowhere else on this Mac to read one."""
    before = agent_ids(profile)
    terminal, side = pty.openpty()
    opened = subprocess.Popen(
        # The driver the app's own New Agent creates. A terminal-driven Claude
        # is read through a keymap matched to the installed Claude version,
        # which is a second thing to be wrong about a run that is asking a
        # question about the relay.
        ["amux", "--profile", profile, "new", "claude", "--driver", "sdk"],
        stdin=side, stdout=side, stderr=side, start_new_session=True)
    os.close(side)
    deadline = time.time() + 180
    started = ""
    while time.time() < deadline and not started:
        new = agent_ids(profile) - before
        if new:
            started = sorted(new)[0]
            break
        time.sleep(2)
    os.close(terminal)
    opened.terminate()
    try:
        opened.wait(timeout=30)
    except subprocess.TimeoutExpired:
        opened.kill()
    if not started:
        journal.stop("the daemon never started a Claude session for this run")
    journal.say("a real Claude session is running on that machine, in this checkout")
    return started


def agent_ids(profile: str) -> set[str]:
    listed = amux("--profile", profile, "list")
    return set(re.findall(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
                          listed.stdout))


# MARK: - The phone


def speak(journal: Journal, udid: str, requests: list[dict], seconds: int) -> list[dict]:
    """Says a plan to the app on the pinned simulator and answers its replies.

    The plan carries the one thing this run must not write down — a refresh
    token — so it is written into a directory that goes away with the process
    rather than into the run's own output beside the evidence."""
    with tempfile.TemporaryDirectory() as scratch:
        plan = Path(scratch) / "requests.json"
        plan.write_text(json.dumps(requests))
        plan.chmod(0o600)
        spoken = subprocess.run([
            str(Path("target/debug/ios-tools").resolve()), "door",
            "--simulator", SIMULATOR,
            "--bundle-id", BUNDLE_ID,
            "--timeout", str(seconds),
            "--requests", str(plan),
            "--allow-errors",
        ], text=True, capture_output=True, timeout=seconds + 300)
    if spoken.returncode != 0:
        journal.stop(f"the app could not be driven: {journal.scrub(spoken.stderr.strip())}")
    return json.loads(spoken.stdout)


def container(udid: str) -> Path:
    return Path(ios_simulators.run(
        "xcrun", "simctl", "get_app_container", udid, BUNDLE_ID, "data"))


def install_fresh(udid: str) -> None:
    """The build under test on the simulator with nothing remembered and
    nobody trusted: uninstalling takes the app's container, and with it every
    account, cache and pairing an earlier run left."""
    subprocess.run(["xcrun", "simctl", "uninstall", udid, BUNDLE_ID], capture_output=True, timeout=120)
    ios_simulators.run("xcrun", "simctl", "install", udid, str(APPLICATION), timeout=300)


def logs(journal: Journal, udid: str, account: str) -> str:
    """What the two ends said, for a failure that would otherwise be a
    timeout.

    A handshake the relay refuses is refused silently as far as the screen is
    concerned: the phone simply never arrives. The runtime's own log holds what
    came back from the relay, and the daemon's holds the other side of the same
    minute, so both are read rather than guessed at."""
    written = []
    phone = container(udid) / "Library/Application Support/amux/installation/runtime.log"
    if phone.is_file():
        written.append("what the phone's runtime logged:\n"
                       + tail(phone))
    machine = Path(os.environ.get("AMUX_LOG", ".wt/amux/amux.log"))
    if machine.is_file():
        written.append("what this checkout's daemon logged:\n" + tail(machine))
    return journal.scrub("\n\n".join(written) or "neither end left a log to read")


def tail(path: Path, lines: int = 40) -> str:
    return "\n".join(path.read_text(errors="replace").splitlines()[-lines:])


def replies(answers: list[dict], kind: str) -> list[dict]:
    return [answer for answer in answers if answer.get("kind") == kind]


def refusal(answers: list[dict]) -> str:
    return "; ".join(answer.get("message", "") for answer in answers
                     if answer.get("kind") == "error")


# MARK: - What the phone is asked to do

# The session is imported once, in the first of the two visits below, and
# never again. A refresh token is spent the first time it is used and the
# account service answers with a replacement, so the token this run read out
# of the browser is dead the moment the phone has used it. A second visit that
# handed the same token over again would be refused — and would be asking the
# phone to do something no phone ever does: a person who opens the app again
# is opened on the session the app kept, not on one somebody re-imported.


def first_visit(account: str, refresh: str, invitation: str, agent: str) -> list[dict]:
    """The phone, from nothing remembered to a real agent's answer.

    Everything after the first line is the app's own production path. It is
    handed a session and nothing else: no relay, no credential and no opinion
    about what this account may do."""
    return [
        {"kind": "restoreSession", "account": account, "refresh": refresh},
        {"kind": "awaitReconciled", "seconds": 120},
        {"kind": "accounts"},
        # By the machine's own invitation, which names the account service
        # both ends are on. A phone that disagreed about that would refuse
        # this before it ever reached the machine.
        {"kind": "pair", "qr": invitation},
        # Read twice: once the moment the trust is written, so a phone that
        # never got a fleet says so, and once after the conversation is
        # ready, which is what a working run reports.
        {"kind": "bridge"},
        {"kind": "awaitAgent", "agent": agent, "seconds": 180},
        {"kind": "watch", "agent": agent},
        # Asked before the fleet is read, not after: a machine that has
        # just admitted this phone has not finished saying so, and a
        # conversation that will take a message is the phone's own word
        # for the moment the machine's account of itself has landed.
        {"kind": "awaitSendable", "agent": agent, "seconds": 240},
        {"kind": "bridge"},
        {"kind": "send", "agent": agent, "text": QUESTION},
        {"kind": "awaitReply", "agent": agent, "saying": ANSWER, "seconds": 300},
    ]


def second_visit(machine: str, code: str) -> list[dict]:
    """The same phone, closed and opened again, pairing the other way.

    Nothing signs it in: the app comes up on the session it saved for itself
    while the first visit was running, refreshes it against the account
    service and dials the relay that service names. Waiting for the connection
    is the first thing asked, because a launch that restores its own session
    is doing so while the driver is already talking to it.

    Then the machine is forgotten and trusted again by the six digits it
    prints, the route a phone that cannot see the screen takes. Second because
    a machine holds one offer at a time, and the invitation had to be the one
    a phone that trusted nobody arrived at."""
    return [
        {"kind": "awaitReconciled", "seconds": 180},
        {"kind": "accounts"},
        {"kind": "revoke", "host": machine},
        {"kind": "pairByCode", "host": machine, "pin": code},
        {"kind": "bridge"},
    ]


# MARK: - The run


def main() -> None:
    journal = Journal(OUTPUT / "qa-live-journey.txt")
    who = address()
    journal.keep(who)
    journal.keep(who.partition("@")[0])
    secret = qa_cloud.password(who)
    journal.say(f"signing {masked(who)} into {BASE}, the way the phone does")

    browser, issued = qa_cloud.signed_in(who, secret)
    del secret
    access, refresh = issued["access_token"], issued.get("refresh_token")
    journal.keep(access)
    journal.keep(refresh)
    if not refresh:
        journal.stop("amux.sh issued no refresh token, so there is no session to hand "
                     "the phone")

    status, body = qa_cloud.ask(f"{BASE}/connect/userinfo", access)
    if status != 200:
        journal.stop(f"amux.sh would not say who this account is: {status}")
    account = json.loads(body)["sub"]
    journal.keep(account)

    entitled, said = qa_cloud.read_entitlement(access)
    journal.say(f"the account service says this account is {said}")
    if not entitled:
        journal.stop(
            "this account may not act, so there is nothing to prove past the "
            "sign-in. The dedicated end-to-end account is entitled by hand "
            "through the QA coupon path and nothing renews it; an entitlement "
            "that has lapsed is for an operator to restore, never for this "
            "recipe to write."
        )
    issued_credential, connect_said = qa_cloud.connect(access)
    if not issued_credential:
        journal.stop(f"the relay refused this account a credential: {connect_said}")
    journal.say("the relay will issue this account a credential, so the phone has a "
                "relay to be sent to")

    daemon_is_up(journal)
    udid = ios_simulators.ready(SIMULATOR)

    named = f"qa-live-{uuid.uuid4().hex[:8]}"
    profile = make_profile(journal, named)
    offered: subprocess.Popen | None = None
    try:
        sign_the_daemon_in(journal, browser, profile)
        profile_reaches_the_relay(journal, profile)
        agent = start_the_agent(journal, profile)
        machine = machine_identity(profile)

        install_fresh(udid)
        journal.say("the app is installed on the pinned simulator with nothing "
                    "remembered and nobody trusted")

        # Held out last, because an invitation lasts five minutes and
        # installing an app takes some of them.
        invitation, offered = pairing_invitation(journal, profile)

        answers = speak(journal, udid, first_visit(account, refresh, invitation, agent),
                        seconds=900)

        complaint = refusal(answers)
        # What the phone got wrong rather than what stopped it. A disagreement
        # between what the account service says an account may do and what the
        # phone shows for it is the defect this run exists to catch, and it is
        # not a reason to stop reading: the rest of the journey is what says
        # whether the relay let this phone in regardless.
        wrong: list[str] = []

        signed_in = replies(answers, "accounts")
        if not signed_in:
            journal.say(logs(journal, udid, account))
            journal.stop(f"the phone never reached the relay: {complaint}")
        known = signed_in[0]["known"]["accounts"]
        reads = known[0]["entitlement"] if known else "nothing"
        journal.say(f"the phone signed in from that session and reads this account as "
                    f"{reads!r}")
        if reads == "None":
            wrong.append(
                "the phone reads this account as having no access while the "
                "account service says it is entitled and the relay issues it a "
                "credential — the paywall this phone would draw is one the "
                "person could not act on, which is the mistake the access gate "
                "was written to end")

        paired = replies(answers, "paired")
        if not paired:
            journal.say(logs(journal, udid, account))
            journal.stop(f"the phone never trusted the machine: {complaint}")
        journal.say(f"the phone authenticated {paired[0]['host']!r} over the production "
                    "relay by scanning that machine's own invitation, and wrote the "
                    "trust")

        bridge = replies(answers, "bridge")
        if not bridge:
            journal.say(logs(journal, udid, account))
            journal.stop(f"the phone never said where it had got to: {complaint}")
        state = bridge[-1]["bridge"]
        if not (state["reconciled"] and state["hosts"] and state["agents"]):
            journal.say(logs(journal, udid, account))
            journal.stop(f"no machine confirmed a fleet with anything in it: "
                         f"{state['hosts']} and {state['agents']}; {complaint}")
        journal.say(f"the fleet on the phone was confirmed by the machine over the "
                    f"production relay: {len(state['hosts'])} machine and "
                    f"{len(state['agents'])} agent, the connection reading "
                    f"{state['connection']!r}")

        sent = replies(answers, "sendAttempt")
        if not sent or not sent[0]["delivered"]:
            journal.say(f"what the phone had reached: {json.dumps(bridge[0]['bridge'])}")
            journal.say(logs(journal, udid, account))
            journal.stop(f"the message never left the phone: {complaint}")

        answered = replies(answers, "conversation")
        if not answered:
            journal.say(logs(journal, udid, account))
            journal.stop(f"the agent never answered on the phone: {complaint}")
        journal.say(
            f"the phone asked a real Claude session a question and read its answer "
            f"back: {len(answered[-1]['conversation']['entries'])} rows in the "
            f"conversation, one of them saying {ANSWER!r}")

        # The machine now holds the other kind of offer out, and the phone is
        # closed and opened again to take it — on its own saved session, as
        # `second_visit` says.
        stop_offering(offered, profile)
        code = pairing_code(journal, profile)
        again = speak(journal, udid, second_visit(machine, code), seconds=600)

        resumed = replies(again, "accounts")
        reopened = [known for reply in resumed
                    for known in reply["known"]["accounts"]
                    if known["id"] == account and known["signedIn"]]
        if not reopened:
            journal.say(logs(journal, udid, account))
            journal.stop("the phone was opened again and did not come back signed in "
                         f"from the session it had saved: {refusal(again)}")
        journal.say("the phone was closed and opened again, and reached the production "
                    "relay from the session it had saved for itself — nobody handed it "
                    "a token a second time")

        recoded = replies(again, "paired")
        if not recoded:
            journal.say(logs(journal, udid, account))
            journal.stop("the phone forgot the machine and could not trust it again by "
                         f"the code it printed: {refusal(again)}")
        journal.say(f"the phone then forgot {recoded[0]['host']!r} and trusted it again "
                    "over the production relay by the code that machine printed")

        journal.say(
            "what this proves: an account entitled on the web — a subscription "
            "bought there, or access granted there, whichever this account holds "
            "— reaches a machine and a real agent through the production account "
            "service and the production relay, on the credential that service "
            "minted and the address it named — by both of the ways a person "
            "pairs, the machine's invitation and the code it prints, and the "
            "second of them after a relaunch on the session the phone kept "
            "for itself. The App "
            "Store purchase route is a "
            "different one and is not claimed here; a sandbox purchase needs a "
            "phone in somebody's hand.")
        if wrong:
            journal.stop("; ".join(wrong))
        journal.say("passed")
    finally:
        if offered and offered.poll() is None:
            offered.terminate()
        remove_profile(journal, profile)


if __name__ == "__main__":
    main()
