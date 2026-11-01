"""Reaching the real amux.sh as a QA account, from this Mac.

The sign-in here is the app's sign-in: the same authorization-code exchange
with PKCE, the same client, redirect and scopes, driven against production.
Two recipes need it — one that only reports what an account may do, and one
that carries a purchase from a phone to the account service — so it lives in
one place rather than being copied and drifting.

Nothing that identifies an account ever reaches the output. Addresses are
masked, and the password, the authorization code and every token are read,
used and dropped.
"""

from pathlib import Path
from urllib import error, parse, request
import base64
import hashlib
import html
import http.cookiejar
import json
import os
import re
import secrets
import subprocess
import sys

BASE = "https://amux.sh"
CLIENT_ID = "mobile"
CALLBACK = "amux://callback"
SCOPES = ["openid", "profile", "email", "offline_access", "api"]
# Where an address lives when the environment does not carry one. Outside the
# tracked tree on purpose: this repository is public and the addresses name
# accounts with QA privileges.
ADDRESS_FILE = Path(".autopilot/qa-account.env")
KEYCHAIN_SERVICE = "amuxcloud-qa"

# What a message says it is. Each recipe sets this to its own name so a
# failure reads as coming from the command the person ran.
PROGRAM = "qa-cloud"


def fail(why: str) -> None:
    """Says what is missing and stops. A recipe that cannot sign in must never
    read as one that signed in and found nothing."""
    print(f"{PROGRAM}: {why}", file=sys.stderr)
    raise SystemExit(1)


def masked(address: str) -> str:
    """The address as it may be written down: enough to tell two QA accounts
    apart in a transcript, never enough to be one."""
    name, _, domain = address.partition("@")
    return f"{name[:1]}***@***{domain[-3:]}" if domain else "***"


def address_in_file(variable: str) -> str:
    """What ADDRESS_FILE sets for a variable, or the empty string.

    The file is a plain list of assignments an operator wrote; it is read
    rather than sourced, because sourcing an untracked file would run it."""
    if not ADDRESS_FILE.exists():
        return ""
    for line in ADDRESS_FILE.read_text().splitlines():
        line = line.strip()
        if line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        if key.strip() == variable:
            return value.strip().strip('"').strip("'")
    return ""


def address(variable: str) -> str:
    """The account a recipe signs in as, or the empty string when nothing
    names one.

    The environment wins, so one run can name another account without editing
    anything. There is no built-in address: an account a recipe reached by
    default would be an account committed to a public repository."""
    return os.environ.get(variable, "").strip() or address_in_file(variable)


def password(for_address: str) -> str:
    """Read from the login keychain at the moment it is needed, and never
    written anywhere."""
    if not keychain_holds(for_address):
        fail(
            f"the login keychain has no password for {masked(for_address)} "
            f"under service {KEYCHAIN_SERVICE}. Add it with: security "
            f'add-generic-password -s {KEYCHAIN_SERVICE} -a "<the address>" -w'
        )
    return read_keychain(for_address)


def read_keychain(for_address: str) -> str:
    found = subprocess.run(
        ["security", "find-generic-password", "-s", KEYCHAIN_SERVICE,
         "-a", for_address, "-w"],
        capture_output=True, text=True, timeout=60)
    return found.stdout.rstrip("\n") if found.returncode == 0 else ""


def keychain_holds(for_address: str) -> bool:
    """Whether there is a password to read, asked without reading one."""
    return bool(for_address) and subprocess.run(
        ["security", "find-generic-password", "-s", KEYCHAIN_SERVICE,
         "-a", for_address],
        capture_output=True, text=True, timeout=60).returncode == 0


def through(opener, prepared) -> tuple[int, str, str]:
    """One request, answered rather than raised.

    A refusal is an answer here: a recipe that reads a 401 or a 403 needs the
    status and the body, and an exception would lose both."""
    prepared.add_header("User-Agent", "amux-qa")
    try:
        answer = opener.open(prepared, timeout=60)
    except error.HTTPError as refused:
        answer = refused
    except error.URLError as unreachable:
        fail(f"{BASE} could not be reached: {unreachable.reason}")
    body = answer.read().decode("utf-8", "replace")
    return answer.status, answer.headers.get("Location", ""), body


class Browser:
    """A cookie jar and a redirect that stops rather than following.

    This is the sign-in and the login form it drives, and nothing else. The API asks go through `ask`, which
    has no jar, because the phone has no jar.

    The sign-in ends at `amux://callback`, which no HTTP client can follow, so
    every redirect is read here and the hand-off is recognised by its scheme —
    exactly what the app's own web session does."""

    def __init__(self) -> None:
        self.jar = http.cookiejar.CookieJar()
        self.opener = request.build_opener(
            request.HTTPCookieProcessor(self.jar), NoRedirect())

    def get(self, url: str) -> tuple[int, str, str]:
        return self.send(request.Request(url, method="GET"))

    def post(self, url: str, fields: dict[str, str]) -> tuple[int, str, str]:
        body = parse.urlencode(fields).encode()
        return self.send(request.Request(
            url, data=body, method="POST",
            headers={"Content-Type": "application/x-www-form-urlencoded"}))

    def send(self, prepared) -> tuple[int, str, str]:
        return through(self.opener, prepared)


class NoRedirect(request.HTTPRedirectHandler):
    def redirect_request(self, *arguments):
        return None


# Every API ask goes through this, and it has no cookie jar on purpose.
#
# The phone has never loaded a page on amux.sh. It holds an access token and
# nothing else, and that is the whole question these recipes ask: does the
# token work. Asking through the browser's opener would send the sign-in
# cookie alongside the token, and amux.sh would answer the cookie — which is
# how /api/graphql went to production reading `me: null` for every bearer
# while this recipe reported it green.
API = request.build_opener(NoRedirect())


def verifier_and_challenge() -> tuple[str, str]:
    verifier = base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("=")
    digest = hashlib.sha256(verifier.encode()).digest()
    return verifier, base64.urlsafe_b64encode(digest).decode().rstrip("=")


def authorize_url(challenge: str, state: str) -> str:
    return f"{BASE}/connect/authorize?" + parse.urlencode({
        "client_id": CLIENT_ID,
        "response_type": "code",
        "redirect_uri": CALLBACK,
        "scope": " ".join(SCOPES),
        "code_challenge": challenge,
        "code_challenge_method": "S256",
        "state": state,
    })


def form_fields(page: str) -> dict[str, str]:
    """Every hidden input on the login form, the antiforgery token among
    them. Read off the page rather than guessed, because a token a recipe
    invented would be refused."""
    fields = {}
    for tag in re.findall(r"<input[^>]*>", page, re.IGNORECASE):
        if 'type="hidden"' not in tag.lower():
            continue
        name = re.search(r'name="([^"]+)"', tag)
        value = re.search(r'value="([^"]*)"', tag)
        if name:
            # Unescaped, because the page is HTML: the return URL comes back
            # with its ampersands as entities, and posting it as written would
            # hand amux.sh a query with one parameter in it.
            fields[name.group(1)] = html.unescape(value.group(1)) if value else ""
    return fields


def sign_in(browser: Browser, who: str, secret: str, challenge: str,
            state: str) -> str:
    """Drives the browser hand-off to its end and answers the code."""
    location = authorize_url(challenge, state)
    seen = 0
    while seen < 12:
        seen += 1
        if location.startswith(CALLBACK.split(":")[0] + ":"):
            returned = parse.parse_qs(parse.urlparse(location).query)
            if "error" in returned:
                fail(f"amux.sh refused the sign-in: {returned['error'][0]}")
            if returned.get("state", [""])[0] != state:
                fail("the callback answered a different sign-in request")
            code = returned.get("code", [""])[0]
            if not code:
                fail("the callback carried no authorization code")
            return code
        status, redirect, body = browser.get(location)
        if status in (301, 302, 303, 307, 308) and redirect:
            location = parse.urljoin(location, redirect)
            continue
        if status != 200:
            fail(f"amux.sh answered {status} where a sign-in page was expected")
        if "Input.Password" not in body:
            # Either the login form has changed shape, or the hand-off went
            # somewhere else entirely — amux.sh's own error page, most often.
            # The page it stopped on is named, because that is the one fact
            # that says which of the two happened.
            fail(
                "the sign-in could not be driven to its end: "
                f"{parse.urlparse(location).path} is not a page with a "
                "password field on it. amux.sh may have changed its login "
                "form, or refused the request that page was reached through."
            )
        fields = form_fields(body)
        fields["Input.Email"] = who
        fields["Input.Password"] = secret
        fields["Input.RememberMe"] = "false"
        status, redirect, body = browser.post(location, fields)
        if status == 200:
            fail(
                "amux.sh would not sign that account in. The address and the "
                "keychain password are what to check; neither is printed."
            )
        if not redirect:
            fail(f"the sign-in form answered {status} and went nowhere")
        location = parse.urljoin(location, redirect)
    fail("the sign-in went round in circles and never reached the callback")


def redeem(browser: Browser, code: str, verifier: str) -> dict:
    status, _, body = browser.post(f"{BASE}/connect/token", {
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": CALLBACK,
        "client_id": CLIENT_ID,
        "code_verifier": verifier,
    })
    if status != 200:
        fail(f"the authorization code was not redeemed: amux.sh answered {status}")
    return json.loads(body)


def signed_in(who: str, secret: str) -> dict:
    """One sign-in, from the authorize request to the redeemed tokens."""
    browser = Browser()
    verifier, challenge = verifier_and_challenge()
    state = secrets.token_urlsafe(24)
    code = sign_in(browser, who, secret, challenge, state)
    return redeem(browser, code, verifier)


def ask(url: str, token: str, method: str = "GET",
        body: str | None = None) -> tuple[int, str]:
    """An API request carrying the access token and nothing else."""
    prepared = request.Request(
        url, data=body.encode() if body else None, method=method,
        headers={"Authorization": f"Bearer {token}"}
        | ({"Content-Type": "application/json"} if body else {}))
    return through(API, prepared)[::2]


def tier(token: str) -> str:
    """What the access token itself says this account may do.

    amux.sh recomputes this claim from the account's entitlements every time a
    token is issued. The entitlement read and the connect endpoint now answer
    from that same table too, so all three agreeing is what this recipe
    expects; a disagreement is a defect rather than a fact about the
    account."""
    parts = token.split(".")
    if len(parts) < 2:
        return "unreadable"
    payload = parts[1] + "=" * (-len(parts[1]) % 4)
    try:
        return json.loads(base64.urlsafe_b64decode(payload)).get("tier") or "absent"
    except (ValueError, AttributeError):
        return "unreadable"


ENTITLEMENT_QUERY = (
    '{"query":"{ me { access { pro until grant { __typename '
    '... on Purchased { status provider willRenew entitledUntil } '
    '... on Granted { reason } } } } }"}')

GRANTS = {
    "COMPLIMENTARY": "given as a gift",
    "EMPLOYEE": "given because they work there",
    "BETA": "given for the beta",
    "REFERRAL": "given through a referral",
    "ADMIN": "given by an administrator",
}


def entitlement(answer: str) -> str:
    """The same read the app makes, said in a sentence.

    One question, asked of the entitlements table: may this account act. What
    paid for it, if anything did, is nested inside the answer as the
    explanation rather than offered as a second thing to gate on."""
    try:
        access = json.loads(answer)["data"]["me"]["access"]
        pro = access["pro"]
    except (KeyError, TypeError, ValueError):
        return "amux.sh answered in a shape this recipe does not know"
    grant = access.get("grant") or {}
    kind = grant.get("__typename")
    if kind == "Purchased":
        where = "the App Store" if grant.get("provider") == "REVENUE_CAT" else "the web"
        renewal = "renewing" if grant.get("willRenew") else "not renewing"
        source = (f"bought on {where}, {renewal} (status "
                  f"{grant.get('status')}, paid up to "
                  f"{grant.get('entitledUntil')})")
    elif kind == "Granted":
        source = GRANTS.get(grant.get("reason"), "given rather than bought")
    else:
        source = "from nothing this account has ever been given or has bought"
    if not pro:
        return f"not entitled: access {source} is over"
    ends = access.get("until")
    return (f"entitled {'until ' + ends if ends else 'with no end date'}, "
            f"{source}")


def read_entitlement(token: str) -> tuple[bool, str]:
    """Whether this account may act, and the answer in words."""
    status, body = ask(f"{BASE}/api/graphql", token, "POST", ENTITLEMENT_QUERY)
    if status != 200:
        fail(f"the entitlement read answered {status}")
    try:
        pro = bool(json.loads(body)["data"]["me"]["access"]["pro"])
    except (KeyError, TypeError, ValueError):
        pro = False
    return pro, entitlement(body)


def connect(token: str) -> tuple[bool, str]:
    """Whether the relay will issue this account a credential, and the answer
    in words. A refusal that is the subscription gate is not a failure: it is
    the second gate the phone draws."""
    status, body = ask(f"{BASE}/api/connect", token)
    if status == 200:
        issued = json.loads(body)
        return True, (f"a relay credential was issued for "
                      f"{issued.get('host')}:{issued.get('port')}, expiring "
                      f"{issued.get('expires_at')}")
    if status == 403 and "payment_required" in body:
        return False, ("refused with payment_required — this account may not "
                       "act, so the phone would show the second gate")
    fail(f"the connect endpoint answered {status}, which is neither a "
         f"credential nor the subscription gate")
