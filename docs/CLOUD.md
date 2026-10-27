# The phone and amux.sh

*For developers changing how the iPhone app or a daemon talks to the account service.*

amux.sh is the account service: sign-in, what an account is entitled to,
relay credentials, purchases, account deletion and report uploads. It is a
separate service with its own repository; nothing in this repository runs it.
This page is the contract as the code here calls it. It is not a general
description of the API.

Two things in this repository call amux.sh:

- **The iPhone app** (`AmuxCloudService` in
  `apps/apple/Packages/AmuxCore/Sources/AmuxCore/AmuxCloud.swift`) signs in,
  reads the entitlement, hands over purchases, deletes accounts and uploads
  reports.
- **A profile's runtime** (`node::auth`) spends the account's refresh token
  and fetches relay credentials. On a desktop that runtime is the daemon; on
  the phone it is the same code running in the app's process (see
  [the embedded runtime](EMBEDDED.md)).

Two rules run through all of it:

- **The phone holds no secret.** There is no client secret, no App Store
  shared secret and no payment-provider key in this repository or in the
  shipped binary. Sign-in is an authorization code with PKCE, and the only
  things the app ever holds are tokens issued to it.
- **amux.sh decides what an account may do.** The App Store can say an Apple
  Account paid; only amux.sh can say an amux account is entitled. Every
  screen reads the entitlement back from amux.sh rather than inferring it
  from a purchase.

## Endpoints

The base is `https://amux.sh`. The phone is the registered client `mobile`,
with the one redirect `amux://callback` and no secret. The scopes asked for
are `openid profile email offline_access api`; `offline_access` is what makes
sign-in happen once rather than hourly. `amux login` on a desktop signs in as
the client `cli` with the device flow and the same scopes.

| Path | Method | Called by | For |
| --- | --- | --- | --- |
| `/connect/authorize` | browser | app | Sign-in in the system browser, with a PKCE challenge and a state. Every sign-in sends `prompt=select_account`; signing back into a listed account adds `login_hint=<its address>`. |
| `/connect/deviceauthorization` | POST | `amux login` | The desktop's device-code sign-in. |
| `/connect/token` | POST | app, profile runtime | The app redeems its code here. A profile refreshes here as the client the token was issued to. Refresh tokens rotate: the one that comes back replaces the one spent. |
| `/connect/userinfo` | GET | app | Who signed in: `sub`, `email`, `name`. `sub` is the account identifier everything else is keyed by. |
| `/api/graphql` | POST | app | The access read, below. |
| `/api/connect` | GET | profile runtime | A relay credential, below. |
| `/api/purchases` | POST | app | A purchase the App Store signed, below. |
| `/api/account` | DELETE | app | Deletes the account. `409` while a subscription is still set to renew. |
| `/api/billing/stripe/portal` | POST | app | A one-time link to stop a web subscription, asked for only when a deletion is blocked, because the link expires. |
| `/api/reports` | POST | app | A report bundle, below. |
| `/api/events` | POST | profile runtime | Usage events, batched per host, with the account's bearer when the profile is signed in to this service and none otherwise. `202` on success; the runtime retries `408`, `429` and `5xx` twice and drops anything else. The body and every event are in [what amux sends](ANALYTICS.md). |
| `/.well-known/openid-configuration/jwks` | GET | relay | The keys relay credentials are signed with. |

Every call except the sign-in endpoints carries `Authorization: Bearer <access
token>`. That token is the whole credential: the app has no browser session
with amux.sh and sends no cookie, and `/api/graphql` and `/api/connect` accept
the same token, scheme and `api` scope, so the read and the relay gate answer
for the same principal.

Every other URL the app offers (support, the account page) is derived from
the same base, so a build pointed at another service cannot offer the
production one's pages.

## Sign-in and who holds the token

The app runs the authorization-code flow itself (`WebSignIn`, a
non-ephemeral `ASWebAuthenticationSession`), redeems the code and asks
`/connect/userinfo` who signed in. That session is held in memory only. When
the person keeps the account, the app hands the refresh token, the base URL
and the client identifier to the account's profile (`amux_runtime_bind`) and
forgets it. From then on the profile alone holds and spends the refresh
token. The app uses the sign-in's own access token until a minute before it
expires, and after that borrows a bearer for its own calls from the profile
(`amux_runtime_access_token`).

## The access read

One query, against `/api/graphql`:

```graphql
{ me { access {
    pro
    until
    grant {
      __typename
      ... on Purchased { provider willRenew entitledUntil }
    }
} } }
```

`pro` is the only thing anything gates on, and it is never null. `until` is
when access runs out, or ran out for a lapsed subscription, and is absent when
access does not run out. `grant` explains where the access came from. An
account can be entitled without ever buying anything (a gift, a beta, an
employee, a referral, an administrator's grant), so the explanation is a
choice between two shapes rather than a subscription that might be missing.

Only the purchase's own fields are asked for. `provider` is the one thing a
screen shows about the source (`REVENUE_CAT` is the App Store, anything else
is the web), `entitledUntil` is when the paid period runs out, and
`willRenew` is whether it will be charged again. The one date the app derives
is the renewal date it warns about before deleting an account: a subscription
with `willRenew` false renews on no date, because the person has paid for the
period they are in, and one that does renew does so on `entitledUntil` where
there is one and on `until` otherwise. Somebody both paying and holding an
open-ended grant keeps access after the billing stops, so the two dates answer
two different questions.

`__typename` says which member of the union arrived, so a member the app does
not know, or a grant, whose fields the app asks for none of, reads as a grant
rather than a purchase with everything missing.

A purchase can be accepted before the access behind it is projected. In those
seconds the read still answers `pro: false`. The app knows it has just handed
over a purchase amux.sh accepted, so the paywall says *Your subscription is
still switching on* and offers Retry, which reads again. Nothing is offered
for sale a second time: the read, not the store, turns the screen over.

`pro` and `/api/connect` are answered from the same place in the account
service, so the phone and the relay cannot disagree about one account. How
somebody pays lives inside `grant`, underneath the answer, so nothing gates on
it by mistake. A subscription bought on the web, one bought in the App Store
and access given by hand all arrive through the same `pro`.

## Which read answers which question

*May this account act?* is `pro`, and only `pro`. Not the `tier` in a relay
credential, which is a copy of the same answer that goes stale between
issues; not the presence of a billing record, which many entitled accounts do
not have; and not a date compared with the phone's clock.

*Why does it have access, and where would somebody change it?* is `grant`,
and only `grant`. The paywall and the settings row read it to name the store a
subscription was bought in and offer to manage it. An account whose access was
given is a case those screens handle in words of their own: they name no store
and offer to manage no subscription.

## Relay credentials

A signed-in profile asks `GET /api/connect` for a relay credential. The answer
is `host`, `port`, `token`, `expires_at` and `tier` (`free` or `pro`). The
profile dials that relay over QUIC, with TCP as the fallback, and presents
the token.

The token is a JWT the account service signs. The relay validates it against
the keys at `/.well-known/openid-configuration/jwks`, requires the audience
`amux_token` and an expiry, and checks that its `host` and `port` claims name
the relay it arrived at. Its other claims are `sub` (the account), `client_id`
(the OAuth client that asked for it) and `tier`.

The tier decides what the relay carries:

- **Pro.** The relay lists the account's machines and carries streams between
  them.
- **Free.** The relay still lists the account's machines, so a phone can show
  them as away, but refuses to carry any stream to or from a free link, with
  `payment_required`. A link that renews as free also ends the streams it
  was already carrying, so a lapsed account's calls stop at once rather than
  riding a connection opened while it was Pro. Machines on the same local
  network reach each other directly either way.

Relay credentials last a week. A profile keeps its credential, with the relay
it names, its expiry, its tier and when it was fetched, in
`profiles/<id>/relay-token`, and a launch that finds it more than five minutes
from expiry dials that relay and presents it without asking the account
service first. A credential within five minutes of expiry is never presented;
the launch fetches a fresh one. If the relay refuses a kept credential, the
profile forgets it and fetches a fresh one at once; only a refusal of that
fresh one asks for a sign-in. Signing in or out forgets the kept credential.

The plan a credential carries is only as fresh as its fetch, so a profile
renews it once it is an hour old (at once, after the link comes up, for a kept
one older than that) and hourly while connected, sending each renewal to the
relay as `Reauth`. A lapse therefore reaches the relay within the hour. A free
profile renews every three minutes so that a purchase reaches the link
promptly. After a purchase the app does not wait for that: it asks the profile
to refresh at once (`amux_runtime_refresh_entitlement`). Nor do the account's
other machines: when a link presents a paid credential, the relay sends
`RefreshCredential` to each of the account's links still on the free plan,
and those profiles refresh at once, so a purchase made on the phone reaches
the desktops it connects to within a round trip. A free link that connects
beside a paid one on its account is asked the same way, since a credential
kept from an earlier launch can name a plan the account has since left. The
three-minute renewal covers a purchase no link has presented yet. Every
renewal is kept for the next launch.

## A purchase reaching amux.sh

`POST /api/purchases`, `application/json`, one field:

```json
{"signed_transaction": "<the App Store's JWS>"}
```

The signed transaction is carried whole and unread. The app does not parse
it, does not trust its contents, and does not know which billing system
reconciles it; that is the account service's business. `200` and `202` both
mean taken (`202`: the service has the transaction and will reconcile it).
Nothing is read back from the answer: what the account may now do is the
access read's answer.

The order is the point of the whole path:

1. The App Store signs a purchase. The transaction is **not** finished.
2. The signed transaction is posted here.
3. Only once amux.sh has taken it is the transaction finished with the App
   Store.
4. The entitlement is read back from `/api/graphql`, and the profile is asked
   to refresh its relay credential.

A transaction finished before step 2 succeeds is one the App Store never
offers this app again, and a subscription somebody paid for would exist
nowhere but on their bank statement. Because the transaction survives, an
unconfirmed purchase is temporary: the paywall offers Retry, and every launch
sends whatever the store is still holding. Purchases approved later (Ask to
Buy, a bank's second factor, a renewal) arrive on StoreKit's updates and take
the same road.

| Answer | What the person sees |
| --- | --- |
| `200`, `202` | The entitlement is read back; the paywall says subscribed. |
| `401` | Not confirmed, and treated as unreachable: the purchase is kept and sent again. |
| `403 payment_required` | Not confirmed and refused, in the words the gate uses. |
| any other refusal (`422`, `502`, ...) | Not confirmed and refused, in the service's own `error_description`. |
| no answer | Not confirmed and unreachable: paid for, kept, and tried again. |

## The `payment_required` rule

`403` with `{"error":"payment_required"}` means the account has nothing
bought. It is not a failure to report: it is the gate the home screen already
draws, so the app says *this account has no subscription* wherever it comes
back. Any other `403` carries the service's own `error_description` and is
shown as it is.

## Deleting an account

The person types the account's address, and the app checks it against what
`/connect/userinfo` says before sending `DELETE /api/account`, which is
authenticated by the token alone. A `409` means money is still moving and
names the provider billing it. For `revenuecat` the app sends the person to
the App Store's subscriptions page; for anything else it asks
`/api/billing/stripe/portal` for a one-time link, falling back to the account
page on amux.sh when no link comes back.

## Uploading a report

Reporting is in every build. `POST /api/reports` takes `multipart/form-data`,
one section per file, each section's `name` and `filename` being the file it
carries: `report.json`, `frame.png`, `trace.jsonl`, `log.txt`, or a file of
the profile's dump under `dump/`.

`report.json` is required and uses `schema_version: 2`. Its `parts` declares
the frame, trace, dump and log as present or absent with a reason. Present
parts are sent as their sections; absent ones are left out, with their reasons
in `report.json`. The declaration and the uploaded files must agree, or the
service refuses the bundle.

A successful upload returns a JSON receipt; the app reads its `id` and, when
present, `receivedAt`. Any refusal (`401`, `413` for a bundle over the size
limit, `422` for a `report.json` that fails validation) keeps the report, with
the same bytes and stamp, for Retry.

## Push notifications

A "needs you" push names a host and an agent under its `amux` key, and the app
handles one by bringing that chat current (see
[the iPhone page](IOS.md#foreground-background-and-pushes)). The daemon keeps
an outbox of them: a row when an agent's phase turns to needs you, deleted
unsent if the phase leaves needs you before its delay runs out. Nothing in
this repository delivers them yet. The daemon's sender is `node::NoopSender`;
`node::HttpSender` posts a push as JSON (`host_id`, `agent_id`, `revision`,
`name`, `working_on`, `text`) with the daemon's relay credential to an
endpoint it is given, but no amux.sh endpoint is wired to it, and the app
does not register for remote notifications. The `push-wake` journey hands a
payload to the app directly.

## Running the relay

The relay is `amux relay`, a hidden command run by a service
manager. It reads one TOML file, named by `--config` or `AMUX_CONFIG`. A key
it does not know is logged and ignored, so the same file serves the relay
versions either side of a deploy or a rollback:

```toml
host_name = "s1.amux.sh"    # the name credentials are minted for
tcp_port = 9001             # the TLS listener; credentials name this port
udp_port = 9001             # the QUIC listener; the TCP port when absent
cloud_url = "https://amux.sh"
tls_cert = "/etc/amux-relay/tls/fullchain.pem"
tls_key = "/etc/amux-relay/tls/privkey.pem"
state_dir = "/var/lib/amux-relay"
```

Relative paths are resolved beside the file. `state_dir` holds the session
tickets the relay issues (`tickets/`), so a restarted relay still resumes the
hosts that held them, and the dumps its core asks for (`reports/`). It
defaults to the directory systemd names in `STATE_DIRECTORY` (a unit with
`StateDirectory=`), else the file's own directory. The relay logs to
standard error for its service manager to keep and rotate (`journalctl -u`
under systemd); `AMUX_LOG` names a file instead, and `RUST_LOG` sets the
level. It keeps no host id between runs.

## Where each value lives

**In this repository, committed and public.** None of it is a secret, and all
of it is visible in any copy of the app anyway:

- The App Store product identifiers, `amux_pro_monthly` and
  `amux_pro_yearly`, in `apps/apple/Packages/AmuxCore/Sources/AmuxCore/Store.swift`.
- The base URL, the client identifier `mobile`, the redirect
  `amux://callback` and the scopes, in `CloudEndpoint.production` in
  `AmuxCloud.swift`.
- No entitlement identifier. What an account may do is `pro`, and nothing in
  the app is matched against a constant to decide it.

**In the account service's repository, encrypted.** The App Store server
credentials and the payment provider's keys and webhook secrets. The app has
no use for them: it carries a signature Apple made and nothing else. The QA
allowlist is configuration there too; it is not a secret, but it names
people.

**Nowhere in this repository.** The addresses of the QA accounts: not in a
script, a document, a fixture, a golden, a journey record or a transcript. A
recipe that needs one is given it when it runs, from the environment or an
operator's untracked file.

## QA recipes

These are evidence a person runs on a Mac. They are not tests, they never run
in CI, and nothing gates on them: they reach the production account service
with a real account, and a red one is a conversation rather than a build
failure. `crates/ios-tools/src/ios_verify.rs` leaves them out of every
verification list on purpose.

- **`just ios qa-cloud-signin`** signs a QA account into `https://amux.sh`
  with no app, using the same PKCE sign-in, client, redirect and scopes as
  the phone, then asks what the app asks: who the account is, what it is
  entitled to, and whether the relay will issue it a credential. It proves the
  contract on this page is the one the live service keeps.

- **`just ios qa-sandbox-purchase`** carries a real App Store sandbox purchase
  from a physical iPhone to the account service and reads back what the
  account may then do. A sandbox transaction exists only on a phone signed
  into a sandbox Apple Account running a development-signed build; the
  Simulator does not support sandbox sign-in, and
  `apps/apple/Amux/Amux.storekit` is a StoreKit Testing configuration whose
  transactions are not sandbox transactions. So this never runs on a
  simulator and never invents a transaction.

  `--preflight` reports what is present and missing on this Mac and touches
  neither StoreKit nor amux.sh. It checks the account's address, its keychain
  password, a phone reachable over `xcrun devicectl`, and a Team ID in
  `apps/apple/Signing.local.xcconfig`, an untracked file (the repository
  builds simulator-only and holds no signing identity; [the release
  page](RELEASE.md) owns that file). Two facts cannot be checked from a Mac
  and are printed to confirm: that the bundle id carries both subscription
  products in App Store Connect and is known to the billing provider, and that
  the phone's sandbox Apple Account is this account. `--confirmed` says they
  hold. A full run builds and installs a development-signed build on the
  phone, names the purchase to make, then watches the access read and
  `/api/connect` as that account until a credential is issued or a bound
  elapses. `--transaction-file PATH` posts a transaction a phone already
  signed instead; that file is never committed.

  It uses the device QA account only, named by `AMUX_QA_DEVICE_EMAIL`; the
  sign-in account is refused because it is not a sandbox account. Run it
  before each release and read what it printed: until somebody does, the App
  Store route to an entitlement is unproven on this build's products, bundle
  id and provider configuration.

- **`just ios qa-live-journey`** runs the whole product once against the real
  one. It signs the QA account into amux.sh, hands the simulator app only that
  session (the door's `restoreSession`), and stands back: the app asks the
  account service who the account is and what it may do, its profile fetches
  a relay credential and dials the relay it names. On the other side is this
  checkout's own daemon (started with `wt run daemon`), on a profile the
  recipe creates for the run and deletes afterwards, signed in as the same
  account through the CLI's device flow. The phone pairs with that machine by
  the QR invitation it offers and then, after forgetting it, by the six
  digits it prints, opens a real Claude session there, asks one question and
  reads the answer back.

  It is the only run that exercises the production handshake: every other
  journey uses a relay started beside it with credentials a harness minted, so
  the audience, port and client the relay compares, and the signature it
  validates, are exercised only here. A mismatch there fails silently (the
  phone never arrives), so a failure is reported with what the phone's runtime
  and the daemon logged. It spends real money on a real agent. If
  `/api/connect` answers `403 payment_required`, the QA account's entitlement
  has lapsed; restoring it is an operator's job, and the recipe says so and
  stops.

**The accounts.** `AMUX_QA_EMAIL` names the account for the sign-in and the
live journey, and `AMUX_QA_DEVICE_EMAIL` the sandbox account. A variable
already set in the environment wins; otherwise the recipes read
`.autopilot/qa-account.env` at the repository root, an untracked,
operator-written file that sets those variables and nothing else. There is no
built-in address.

**The password.** Read from the login keychain when it is needed, with
`security find-generic-password -s amuxcloud-qa -a "<the address>" -w`, and
never printed, logged or written down. Add one with `security
add-generic-password -s amuxcloud-qa -a "<the address>" -w`.

**What is never printed.** An address appears only masked; the password, the
authorization code and every token are used and dropped; the account
identifier is not printed at all. When the address, the keychain entry or the
login form is missing, a recipe names which and exits non-zero.

## Which cloud a pairing invitation names

A QR invitation (`amux://pair?payload=...`) carries the offering machine's
host id, a one-shot secret, the addresses it can be dialled at directly, and
its configured `cloud_url` when it has one; the six printed digits carry none
of that. The cloud is the account service, normally `https://amux.sh`, which
assigns a relay through `/api/connect`; the relay address is only where
traffic goes and is never written into configuration.

An invitation never changes the receiving device's configuration or relay.
QR and printed-code pairing both go through the same authenticated route
check: a cloud cannot route to a host on another cloud or another account, and
knowing a host's identity and secret does not supply that route. When the host
cannot be reached, both say: "Pairing could not reach this host. Check that
both devices are online and signed in to the same cloud account." Incorrect
and expired secrets are indistinguishable, and neither path writes trust
before the person confirms.
