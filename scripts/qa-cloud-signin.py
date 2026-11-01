#!/usr/bin/env python3
"""Sign a QA account into the real amux.sh the way the phone does, then read
what that account may do.

This is evidence a person runs on this Mac, not a test. Nothing here is
mocked: it performs the same authorization-code sign-in with PKCE the app
performs, against the same client, and then asks the three questions the app
asks about an account — who it is, what it is entitled to, and whether the
relay will issue it a credential.

Nothing that identifies the account ever reaches the output. The address is
masked, and the password, the authorization code and every token are read,
used and dropped.
"""

from pathlib import Path
import json
import sys

sys.path.insert(0, str(Path(__file__).parent))
import qa_cloud
from qa_cloud import BASE, CLIENT_ID, fail, masked

qa_cloud.PROGRAM = "qa-cloud-signin"

ADDRESS_VARIABLE = "AMUX_QA_EMAIL"


def address() -> str:
    """The account to sign in as: the environment, then the operator's file."""
    found = qa_cloud.address(ADDRESS_VARIABLE)
    if not found:
        fail(
            f"no account to sign in as. Set {ADDRESS_VARIABLE} in the "
            f"environment, or create {qa_cloud.ADDRESS_FILE} setting "
            f"{ADDRESS_VARIABLE}. That file is outside the tracked tree and "
            "must stay there."
        )
    return found


def main() -> None:
    who = address()
    secret = qa_cloud.password(who)
    print(f"signing {masked(who)} in at {BASE} as client {CLIENT_ID}")

    # The cookie jar the sign-in fills is dropped here. Every question below
    # carries the access token and nothing else, which is the position the
    # phone is in: it has never loaded a page on amux.sh.
    issued = qa_cloud.signed_in(who, secret)
    del secret
    token = issued["access_token"]
    print("signed in: amux.sh issued an access token"
          + (" and a refresh token" if issued.get("refresh_token") else
             " and no refresh token"))

    print("asking: every question below carries the access token and no "
          "cookie, the way the phone asks")

    print(f"tier: the access token claims {qa_cloud.tier(token)}")

    status, body = qa_cloud.ask(f"{BASE}/connect/userinfo", token)
    if status != 200:
        fail(f"amux.sh would not say who this account is: {status}")
    who_it_is = json.loads(body)
    # Whether it is the same account, not which account. The identifier is as
    # good as the address for finding a person, so neither is printed.
    same = who_it_is.get("email", "").lower() == who.lower()
    print("who: amux.sh answers with "
          + ("the same address that signed in" if same
             else "a DIFFERENT address from the one that signed in"))

    entitled, said = qa_cloud.read_entitlement(token)
    print(f"entitlement: {said}")

    issued, connect_said = qa_cloud.connect(token)
    print(f"connect: {connect_said}")

    # Both gates read the same table through the same call, so they cannot
    # honestly differ. If they ever do, the phone is about to draw a paywall
    # for an account the relay lets in, or offer a home to one it refuses.
    if entitled != issued:
        fail("the entitlement read and the connect endpoint disagree about "
             "this account, which is the defect this recipe exists to catch")
    print("agreement: the entitlement read and the connect endpoint say the "
          "same thing about this account")


if __name__ == "__main__":
    main()
