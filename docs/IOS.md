# The iPhone app

*For developers building, changing or testing the iPhone app.*

The iPhone app is native SwiftUI, iPhone only, with iOS 26.0 as its minimum.
It is a full amux installation in its own process: the daemon runs in the
app, with one profile per account, and the chats and fleet are read from
that in-process runtime through the same client service the terminal dials
over a socket. The phone runs no agents of its own. It pairs with the
machines that do, reaches them directly on the local network or through the
relay, and keeps replica rows of their agents in its own store so a chat
draws before any machine answers.

The Rust side of that, and how Swift reaches it, is on the
[embedded runtime page](EMBEDDED.md). This page covers the Swift app: how it
is put together, what it does with accounts, the network and the store, and
how it is built, driven and tested. Everything lives under `apps/apple/`, and
every command goes through `just ios <recipe>` (`just --list ios`).

## Packages

| Component | Owns |
| --- | --- |
| `AmuxCore` | The Swift side of the bridge (`Runtime`, `Profile`, `Chat`), the observable stores screens read (`RuntimeCoordinator`, `StoreBundle`, `ChatModel`, `FleetStore`, `HostsStore`, `AccountRegistry`), the account service and App Store adapters, local-network discovery, report assembly and signposts |
| `AmuxCore/AmuxValues` | Swift mirrors of every value the bridge carries, generated from the Rust definitions; never edited by hand |
| `AmuxCore/LaunchClock` | The one piece of C in the app: an image initialiser that marks the end of dynamic loading for the cold-start split |
| `AmuxDesign` | Light and dark tokens, bundled faces, type scaling, glass, motion and thumb-target geometry |
| `AmuxFeatures` | The screens, as functions of the state and actions they are handed, plus the registered UIKit leaves |
| `AmuxShell` | Tabs (Agents, Hosts, You), routes, deep links, web sign-in and speech dictation |
| `AmuxTestSupport` | The driving door's protocol, scripted account, App Store and chat stand-ins, and the view-state trace |
| `apps/apple/Amux` | The app target: entry point, `Composition` (which assembles every store and service), report capture, and under `Debug/` the door server and the component catalogue |

`AmuxCore` links the bridge as the binary target `AmuxApp`, an XCFramework
under `target/ios/` that the bridge recipes build (see
[building the bridge](EMBEDDED.md)).

`apps/apple/Tools/feature-lint.sh`, run by `just ios lint`, holds
`AmuxFeatures` to three rules: no UIKit outside a registered leaf, no
platform conditionals, and no spinners. A screen waits by showing what it
already has, marked as not yet confirmed, rather than covering it.

## From the runtime to a screen

The shell draws the tab somebody is looking at. A tab is built when it is
first reached for and kept from then on, so coming back finds it as it was
left; the home's rows, grouped in `RowGroup`, are built as they come near
the screen. Both are what a cold launch's first frame is made of, and
building all three tabs and every row for a launch that showed one tab and
ten rows was most of its time.

`Composition` builds one `RuntimeCoordinator` per process. The coordinator
starts the installation under Application Support (`amux/installation`),
reading the phone's own store before any network is dialled, so the first
frame after a launch is what the phone last held. It opens the fleet of the
profile on screen and feeds that profile's `StoreBundle`: the fleet, hosts,
pairing and New Agent stores one account's screens draw from.

The runtime wakes Swift at most once per main-thread turn, naming the fleet
or the chat that moved. The bundle takes that one's changes and reads again
only what changed. A chat is a `ChatModel` over an open `Chat`:

- It holds the chat's row keys, oldest first: the session's window. Keys
  never move, and the list changes only at its two edges: newer keys above
  the newest it holds, older ones below its oldest when the reader scrolls
  up, and while the reader follows, the oldest keys the window drops as it
  trims to its cap of 200 rows.
- `following` is told to the session whenever it changes: leaving the newest
  row, and returning to it by reaching the bottom, New activity, or a send.
  In history the session holds arrivals apart from the window, and New
  activity shows from the frame's `arrivalsHeld`; the return releases them,
  or reloads the newest rows when more arrived than the window holds.
- The list draws a bounded run of those keys, at most 240, because every
  change places each drawn row again. While the reader follows, the run is
  the newest rows, which the cap keeps under 240; in history the window
  grows by pages, and reaching either end of the run takes in 80 held rows
  beyond it without a fetch, letting as many go at the far end, still held.
  A page is asked for only when fewer than a page of held rows are left
  above the run, one per arrival at the top. New activity, a send and a
  Reset draw the newest run again.
- Each row is a `RowCell` read by key when it is first drawn. An update names
  the keys it changed, and only cells already drawn are read again, so one
  changed item redraws one cell.
- The whole list is read again only when a change batch says it was reloaded,
  which is what a Reset's swap and the reload of a head that moved on do: the
  rows on screen stay until the rebuilt transcript replaces them.
- It keeps the head ask's unsent answers and a review in progress for as
  long as the chat is open, so leaving the page loses none of them. The
  draft and its attachments last longer: `DraftShelf` keeps each one on the
  phone, per account and agent, as it changes, so a draft outlives its
  chat's session and the app being closed. Drafts never leave the device,
  and removing an account from the phone forgets its drafts.
- An attachment is checked against the stated limit, 64 MB of content
  (`wire::ATTACHMENT_SIZE_LIMIT`, copied as `ChatModel.attachmentLimit`),
  before anything is read or uploaded, and one over it is refused in words
  naming the file and the limit. One that fails to store is named with the
  reason in the chat's notice.
- A send while the chat is not current (catching up, reconnecting, or its
  host out of reach) is handed to the client library rather than refused:
  Send stays enabled, the field says when the message goes, and the library
  holds the prompt, answer or stop and sends it, with the same id, once the
  host can be reached. A held prompt is drawn pending with "held · sends
  when desk is back" and a Cancel that returns its words to the draft; a held
  answer shows on the ask card with Cancel; a held stop shows under the
  chat's header and on the agent's home row, and the chat's menu offers
  Cancel Stop.
- A draft starting with `/` goes to the library's client commands first:
  what the library runs itself (opening settings, renaming, choosing a
  model) happens on the phone, a word the agent does not take is refused in
  words naming what it does take, and anything else is sent to the agent as
  typed.
- When terminal Claude's own view resumes another conversation, the chat
  says so above the composer ("Claude's own view switched to another
  conversation") with Switch Back, which asks the library to return that
  view to this chat.
- A multi-select question sends the options ticked and any words typed as
  "Something else" together; typing does not untick anything. A
  single-choice question sends one or the other.

A bundle keeps every chat a page shows open, plus chats viewed in the last
five minutes, plus a chat a push is bringing current.

## Accounts and profiles

App startup does not require an account. The phone is one installation of
the daemon with one profile per account, as a desktop is; the daemon's
profile registry creates, binds, signs out, pauses and deletes them.

- With nobody signed in, the installation holds one unbound profile and the
  app shows its fleet. The first sign-in binds that profile, keeping its key,
  pairings and store. A further account gets a profile of its own.
- Signing out keeps the profile tied to its account, with its local
  relationships; it stays on screen and still reaches machines on the local
  network, and signing back in binds the same profile.
- Only the profile on screen has its fleet open. Switching account opens the
  other profile's fleet without stopping the installation.
- Only the profile on screen keeps its relay link: every other bound profile
  is paused, which holds its relay link down and keeps its direct links and
  store.
- The profile on screen keeps a source open for every agent it lists while
  the app is in front of somebody. Every other profile, and the one on screen
  while the app is in the background, opens sources only for the chats that
  ask.

Remove from This Phone (an account's section on You, or the menu on any
account row) takes an account off the phone and leaves it on amux.sh. The
registry deletes the account's profile: its key, its trust store, the hosts
it paired and its store. The installation keeps at least one profile, so
removing the last account first creates a fresh unbound one. The hosts keep
their record of the old key until it is revoked there, and adding the
account back means pairing again. Deleting the account takes the same path.
When the account on screen is removed, it is replaced by one still signed
in, then any account, then the unbound profile.

`AccountRegistry` remembers only which account is on screen and, per account,
what it last listed and what the account service last said. Which accounts
exist, their addresses and whether they are signed in are the registry's to
say. A profile alone holds and spends its account's refresh token; the app
borrows a bearer from it for its own calls to amux.sh.

## Signing in

Sign-in is an authorization code with PKCE in the system browser
(`WebSignIn`, an `ASWebAuthenticationSession`). The app has no password field.
Every sign-in opens amux.sh with `prompt=select_account`, which lists the
accounts that browser has used and offers another; signing back into a
listed account also sends `login_hint` with its address. The browser session
is not ephemeral, because the chooser is made of what it remembers.

When signing back in returns a different account from the one asked for, the
sign-in page names both and adds nothing until the person continues as the
returned account or cancels. A sign-in's session stays in memory until the
account is kept: only then is its refresh token handed to the account's
profile (`amux_runtime_bind`), so an account that was turned down, or a page
somebody left, writes nothing down.

What the app asks of amux.sh, and how purchases reach it, is on
[the cloud page](CLOUD.md).

## The local network

`LocalDiscovery` in `AmuxCore/Discovery.swift` is the app's only network
browser. On iOS only the system may browse, so the Rust runtime does not:
while the app is active, `LocalDiscovery` browses `_amux._udp` with
`NWBrowser`, resolves what it finds, and hands the whole found set to every
profile. Discovery only supplies candidates and addresses; pairing and the
pinned handshake establish trust.

The app declares `_amux._udp` in `NSBonjourServices` and explains the
local-network permission as "amux finds hosts on your network so this phone
can pair and connect to them directly." When access is denied, the Hosts tab
says so, lists no machine and offers the Settings route.

Tapping a host on the Hosts tab opens its page: how the phone reaches it,
its platform, version, providers and addresses, the last dial error while it
is offline, and its key's fingerprint in full. The page and the Devices sheet
both offer Revoke, and both ask first, saying the phone stops reaching the
machine and only pairing again brings it back. Every route the shell can
push has a built page; `RoutePagesTests` opens each one.

## Foreground, background and pushes

`RootView` tells the coordinator whether the scene is in the background. In
front of somebody, the browser runs and the profile on screen lists every
agent's source. Put away, the browser stops and forgets what it found, and
only the chats a push opens keep a source. Coming back to the front starts
the browser again and lists every agent again.

The same two moments drive [product analytics](ANALYTICS.md), which the
runtime keeps and sends: coming to the front counts the app as opened (at
most once an hour), and going to the back sends what is waiting before the
system may suspend the app. The paywall opening and a purchase starting are
seen only by the app, which records them on the account on screen
(`RuntimeCoordinator.record`). **Share Usage** under **You** turns it off; the
choice is kept in the app's defaults and handed to the runtime at start and
on every change. Only the App Store build sends.

A "needs you" notification's payload names a host and an agent under its
`amux` key (`PushPayload`). When one wakes the app in the background,
`RuntimeCoordinator.warm` brings the account whose profile trusts that host
on screen (pausing the previous one's relay link before resuming its own),
opens that one chat, waits for it to be current, and returns. A tap on the
notification opens the chat. A push for a host no profile trusts is left
alone. In front of somebody the account on screen never changes under them;
a tap on a notification for another account's host brings that account
forward.

## The store

The store is the workspace's `store` crate, built with SQLite bundled rather
than the system library, on the device and in the simulator alike. SQLite
fixes therefore reach installed phones through an amux update, not an iOS
update. The packaging recipe's linkage check proves the linked app carries
SQLite rather than loading it, and `just test-store-ios` runs the store's own
suites on the pinned simulator.

A missing store is an ordinary empty cache and opens as an empty fleet. If
the installation cannot start or its store cannot be used, the whole shell is
replaced with `StoreFailureScreen`: the cause and one Relaunch action, with
no fleet or chat left usable behind it. Relaunch stops and starts the
installation again.

## Reporting a problem

Reporting is in every build, Release included. After a system screenshot,
or from Report a Problem, the app freezes its own frame and opens a report
over it; the person can mark rectangles and write a note. There is no shake
gesture. A report is a bundle (`ReportAssembly`): `report.json` at
`schema_version` 2 declares each part present or absent with a reason,
beside the frozen `frame.png`, the view-state `trace.jsonl`, the tail of the
installation's `runtime.log` as `log.txt`, and the profile's dump under
`dump/`. Only a build with the driving tools records the trace, so a
Release report declares it absent. The build stamps its checkout revision
into the app, and each report carries it as `git_sha`.

A report is sent to the account on screen; with nobody signed in, Send is
unavailable. A failed upload keeps the same bytes, creation time and stamp
for Retry. Captured reports are read with [the debugging guide](DEBUGGING.md).

## Building

Use an Apple silicon Mac with Xcode 26.6 (17F113), the iOS 26.5 simulator
runtime, XcodeGen 2.46 or newer, Rust with the `aarch64-apple-ios` and
`aarch64-apple-ios-sim` targets, and `just`.

```sh
just ios simulator     # create and boot the pinned simulators
just ios build         # bridge, Xcode project, debug app
just ios unit          # package and app-hosted unit suites
just ios lint          # feature rules and the copy inventory
```

`just ios build` builds the bridge first (`just ios rust`), generates
`apps/apple/Amux.xcodeproj` from `apps/apple/project.yml`, and builds the app
for the golden simulator into
`target/ios/DerivedData/Build/Products/Debug-iphonesimulator/Amux.app`. The
generated project is committed: change `project.yml` and commit both.

There are two configurations. `Debug` compiles the driving tools in
(`AMUX_DEBUG_TOOLS`) and links the bridge built with them. `Release` excludes
`Amux/Debug/` and `AmuxTestSupport` from the target and links the shipping
bridge. `just ios scope-audit` opens a
built Release bundle and refuses one that carries a debug surface or an
excluded platform; there is no Mac, Catalyst or iPad target.

Recipes name a kind of device, never a device. `golden` is an iPhone 17 Pro
on iOS 26.5 at 3×, the device every budget and baseline is pinned to;
`small` is the iPhone SE (3rd generation) on the same runtime, for the
narrowest supported width. `scripts/ios_simulators.py` decides which device a
kind means: inside a `wt` worktree it is the one leased for the command
(`scripts/with iphone -- ...`), so two checkouts never drive one device;
elsewhere, including CI, it is `amux-iphone-1` or `amux-small-1`. The
recipes pin en_US, a 12-hour clock, 9:41, a full battery and the requested
appearance. A simulator change means reviewed baseline changes, with the new
pin and its reason in the commit message.

## Driving a debug build

A build with the driving tools opens a door: newline-delimited JSON on
loopback, one request per line, defined in `AmuxTestSupport/Door.swift`. The
door drives the running app itself. It never opens a screen with invented
state behind it; every picture it takes is of the app. `query` returns what
is drawn, with accessibility identifiers, labels, values, frames and enabled
states; `tap`, `type`, `paste` and `scroll` act the way a finger does;
`pairByCode` and `pair` pair with a machine by the code or link it printed;
`appearance`, `dynamicType` and `assist` change how the app draws; `geometry`
says whether identified elements report their frames, which a tap needs and a
measurement leaves off; `settle` waits for the screen to stop moving;
`capture` photographs it. `connect`
signs the app into a served relay, and `cloud` and `store` script what the
account service and the App Store answer, for journeys about accounts and
purchases.

`xtask door` launches the installed app on a pinned simulator, speaks the
requests it is given and prints the answers:

```sh
just ios build && just ios tools
timeout 120 target/debug/xtask door --simulator golden \
  --install target/ios/DerivedData/Build/Products/Debug-iphonesimulator/Amux.app \
  '{"kind":"open","screen":"home"}' \
  '{"kind":"appearance","appearance":"dark"}' \
  '{"kind":"settle"}' '{"kind":"query"}' \
  '{"kind":"capture","path":"/tmp/amux-home.png"}'
```

`open` goes where a person would tap to: a tab (`home`, `hosts`, `you`), or
a page (`conversation`, `pin`, `new-agent`, ...) about a machine or agent the
runtime lists, named in the request's `fixture` field. A page about something
the runtime does not list is refused. On its own the app has no machines, so
anything beyond the tabs is reached against a served test network, which is
what the golden, journey and accessibility recipes set up.

## Testing

Use the cheapest test that can see the regression:

| Layer | What it holds | Recipe |
| --- | --- | --- |
| Unit suites | Model state, decisions and projections, without rendering | `just ios unit` |
| Component snapshots | One production view from typed inputs: its text, size and both appearances | `just ios component-snapshots` |
| Whole-screen goldens | Composition, safe areas and navigation on a few screens of the running app | `just ios goldens` |
| Journeys | Real taps and typing, routing, persistence and the machines on the other side | `just ios journey` |
| Accessibility audit | Every control on every drawn state has a VoiceOver name and a 44 pt target | `just ios accessibility` |

Journeys prove actions; a picture of a final state proves nothing about the
actions that produce it. How the phone's suites fit the rest of the testing
is on [the testing page](TESTING.md), and the served networks they run
against are on [the testnet page](TESTNET.md).

### Compared pictures are drawn flat

Every picture a check compares, component snapshot or whole-screen golden, is
drawn with the app's reduce-transparency flag on. Each frosted surface then
takes the flat branch of `Frosted` in `AmuxDesign/Glass.swift`: a raised fill
with a hairline rim, exactly what a person who turned on Reduce Transparency
sees. The app itself is unchanged and still draws Liquid Glass and material
on a phone.

Glass is not something an exact comparison can hold. The render server
finishes it after SwiftUI has drawn, out of the app's sight and on its own
schedule: small glass eases its shadow and filters toward the brightness
behind it after reports that arrive a third of a second later on one machine
and more than half a second on another, and larger glass completes in a
second pass the app cannot observe. A photograph taken at any fixed moment
shows one stage or another of that work. Drawn flat, the same screens repeat
pixel for pixel.

Glass is looked at by eye instead, in review captures nothing compares:

```sh
just ios goldens -- --review DIR
just ios component-snapshots -- --review DIR
just ios component-snapshots -- --review DIR composer.strip ask.plan
```

The golden run walks the same way with the flag off and writes every
manifest screen, light and dark, to `DIR/<screen>.<appearance>.png`. The
snapshot run draws with the flag off every selected example that wears a
frosted surface: one whose window, once it is ready, holds a Liquid Glass
layer or a backdrop layer SwiftUI draws itself (the material under a panel;
a list's scroll edge effect is UIKit's and does not count). It waits 2.5 s,
longer than the render server has been seen to take to finish glass, and
writes `DIR/<id>.<appearance>.png`. Neither reads or writes a baseline,
neither can fail on a picture, and neither runs in `just ci`, the gate or any
check.

The phone journeys are drawn flat too: the journey driver turns the flag on
at every launch, including the reopens a story makes, and their screens are
held to the same thresholds as the whole-screen goldens.

### What a compared picture may differ by

The component snapshots, the whole-screen goldens and the journeys' screens
are held to one allowance:

- any pixel may move by one 8-bit level per channel, which is the
  rasteriser's rounding;
- at most 64 pixels of a picture may move further, and none of them by more
  than 8 levels in any channel.

Drawn flat, a picture repeats to rounding nearly every time. Now and then
the rasteriser draws the anti-aliased edge of a rounded border a few levels
differently, in a picture that has stopped changing: eight pixels on a
card's four corners, off by up to four levels, failed a CI run. Noise like
that is small in area and in distance at once, and a change somebody made
is not: a mark gone missing moves its pixels far (the needs-you dot alone
is some 450 pixels), and a colour token that drifted moves thousands. Both
numbers are deliberately tight, since slack of 12 levels over 600 pixels
once hid a misplaced mask, and neither is a percentage, which would grow
with the picture.

The numbers live in two places that must agree: `golden::ALLOWANCE` in
`crates/xtask/src/golden.rs` for the screens, and `RoundingImageDiff.swift`
for the component snapshots. A match that spent some of the allowance says
so in the run's output (`AMUX_SNAPSHOT_STRAYS`, `AMUX_PICTURE_STRAYS`), so
the numbers can be judged from what runs actually see.

### Component snapshots

The debug-only `ComponentCatalog` in `Amux/Debug/` supplies named examples to
both Xcode's previews and the snapshot suite. Each builds production views
from typed values and a scripted chat with inert actions, on a fixed canvas,
traits, locale and time zone, and is drawn in one app-hosted test process
without navigating or photographing the simulator.

```sh
just ios component-snapshots
just ios component-snapshots composer.draft ask.plan
just ios component-snapshots --skip-build composer.draft
just ios component-snapshots --record composer.draft
just ios component-snapshots-perturb row.prose
```

The ordinary run builds current sources; `--skip-build` checks the last build
and never verifies a source edit. `--record` is the only way a baseline is
written: inspect both appearances before committing it, then run an ordinary
comparison. The perturbation recipe verifies the unchanged baseline, draws a
stripe over the component and requires a mismatch.

Comparisons use Point-Free's SnapshotTesting under the allowance every
compared phone picture shares (`RoundingImageDiff`; see
[What a compared picture may differ by](#what-a-compared-picture-may-differ-by)).
Each example is drawn flat (see above) in a
window of its own, on screen, and photographed once it reports ready and two
photographs a display frame apart agree. A few take one more change after
they report ready (an attachment chip replacing "Attaching", a focused
field's caret appearing), which has come up to 2.8 s later on a CI runner;
those are marked `settlesLate` in the catalogue and photographed only once
their photographs have stayed unchanged for a second. `--settle-all` waits
like that for every example and fails one that changed without the mark; the
nightly captures run and every `--record` use it, so a new example that
changes late is caught when its baseline is written. An
example whose window still holds glass or material when it is photographed
fails by name, so a surface that reaches glass without the flag is a plain
failure, never a flaky one. These in-process images say nothing about system
chrome, keyboards, scrolling or layering between screens; those belong to the
goldens and journeys.

### Whole-screen goldens

```sh
just ios goldens
just ios goldens -- --only origin-rewind
just ios goldens -- --update
just ios goldens-perturb
```

`apps/apple/Goldens/manifest.json` has two parts, each picture with a
sentence saying what it shows. `components` is every example the component
catalogue pins, and the snapshot suite checks that the two name the same
pictures. `screens` is the handful of whole screens the phone is held to:
pairing, hosts, the fleet, a chat with its strip, a question ask, the way out
of an ask the phone cannot answer, and a chat through its host's power loss
before and after the swap.

`scripts/ios-goldens.py` reaches every screen the way a person does. It
serves `journeys/topologies/phone-goldens.json`, installs the debug app
fresh with the reduce-transparency flag on, pairs with the desk by the code
it printed, and taps through to each screen while the served network makes
the desk's agents act. Each screen is compared in light and dark twice over:

- The display's pixels outside the masks, with `xtask golden diff`, under
  the allowance every compared phone picture shares (see
  [What a compared picture may differ by](#what-a-compared-picture-may-differ-by)).
- The door's element geometry, as `<screen>.elements.txt`, compared word for
  word and frame for frame. Tab pages covered by a pushed page are left out.

The manifest declares the pinned simulator and the system chrome it draws
over every app, and no pixel under that chrome is compared: the status bar's
clock and indicators, which SpringBoard can draw late in the previous
appearance's colour on a loaded machine, and the home indicator, which a
runner with both pinned devices booted can leave on screen. The difference
image washes every excluded rectangle blue so a reviewer sees what was not
compared.

`--only` photographs the named screens but still walks the whole way, so each
is reached in the same state. `--update` rewrites only the goldens that
differ; inspect both appearances before committing them. The perturbation
recipe reaches the fleet twice, once with the accent token moved and once with
only the needs-you dot taken away, and fails unless both come back different
with a difference image each time. Expected, actual and difference images
land in `target/ios/goldens/`. Never refresh a baseline to hide
nondeterminism.

### Journeys

`journeys/manifest.json` declares the stories every client is held to, each
with a claim, the clients it runs on and the served topology it runs against
(`phone_topology` for the phone where it differs). `just ios journey` runs
every story written for the phone; name stories to run some, or pass
`--native` for the ones only the phone runs:

```sh
just ios journey
just ios journey reach-host manage-agent
just ios journey -- --native
```

`scripts/ios-journey.py` starts the story's topology with `testnet serve`,
installs the debug app fresh on the leased simulator with the
reduce-transparency flag on, and acts only as a person would, through the
door: tap, type, paste, pair by the code or link a machine printed. It judges
by what the machines recorded (the served network's chat, inventory and
provider-input reads) and by screens compared with reviewed goldens under
`journeys/goldens/phone/<story>/`, pixels and element geometry both, at the
whole-screen goldens' thresholds. Results land in `target/journeys/phone/<story>/`;
`UPDATE_JOURNEY_GOLDENS=1` rewrites the goldens that differ.

The machines in a journey are real daemons with scripted providers, and the
relay is a real relay beside a stand-in account service that mints its
credentials. Account and purchase journeys script the account service and the
App Store through the door. None of this is production sign-in, billing or a
live provider; the by-hand recipes on [the cloud page](CLOUD.md) are.

### The accessibility audit

`just ios accessibility` reaches every page the goldens reach, the same way,
and at each one asks a running UI test (`AmuxUITests/AccessibilityAuditTests`)
to audit what is on screen: every control has a name VoiceOver can read and at
least a 44 pt target. It is a UI test because XCUITest is the only
accessibility client an app cannot be for itself. Links inside an agent's
prose are judged on their name only and listed in the record, which lands in
`target/ios/accessibility/`.

### Verification recipes

| Recipe | Runs |
| --- | --- |
| `just ios gate` | Lint, script tests, bridge graph check, bridge, simulator, one build of the app and every unit and component suite, then component snapshots, loopback smoke and unit suites. What CI runs on every push that touches the phone's paths ([CI](CI.md)). |
| `just ios captures` | Goldens and their perturbation, the store suite on the simulator, journeys and the accessibility audit. Nightly and on demand in CI. |
| `just ios shipping` | The shipping XCFramework and the Release scope audit. `just ios release` depends on both. |
| `just ios measured` | The phone measured on this Mac: `just ios perf`, on an enrolled Mac only ([Performance](PERFORMANCE.md)). |
| `just ios verify` | The workspace's format, lint, tests and specs, then all four of the above, stopping at the first failure. |

`just ios loopback-smoke` links the driving bridge from a bare Swift
executable, pairs it with a served machine by the link that machine prints,
and reads the machine and its agents back.

## Copy

[The copy standard](IOS_COPY.md) sets wording, case, terms and punctuation.
`just ios lint` checks every Swift literal in the app and its packages
against the English catalogue or an exact non-copy exemption. A copy change
includes its affected light and dark goldens.

## The app icon

The icon is the one published on the App Store, committed as
`apps/apple/Amux/Assets.xcassets/AppIcon.appiconset/AppIcon.png`. Nothing in
the build draws, scales or regenerates it: one 1024×1024 image is the whole
set, and the asset catalogue compiler derives every size. Replacing the icon
means replacing that file.

The App Store enforces three rules that a simulator build never shows:

- **No alpha channel.** An icon with one is rejected outright.
- **A 120×120 iPhone icon in the bundle.** The catalogue compiler writes
  `AppIcon60x60@2x.png`; an upload without it fails with code 90022.
- **A top-level `CFBundleIconName`.** `apps/apple/project.yml` declares it.
  The compiler writes its own copy nested inside `CFBundleIcons`, which is not
  where Apple looks; without the declared one an upload fails with code 90713.

`scripts/tests/icon_test.py` checks the sources, and `just ios scope-audit`
refuses a built bundle with no top-level `CFBundleIconName` or no
`AppIcon60x60@2x.png`.

## Checks only a phone can make

The simulator cannot stand in for these; they are done by hand on a physical
iPhone running a Release build before a release:

- Take a system screenshot with the thumbnail preview enabled, and again with
  the full-screen preview. Each time the app's Report prompt appears and opens
  the frozen frame, without a Share step or Photos permission. The report
  journey covers the notification and the app's side of it; it cannot post a
  real system screenshot.
- Dictation: with the caret inside a draft, tap Dictate, grant speech and
  microphone access, and speak. The words appear at the caret as spoken, the
  composer says it is listening, and Stop Dictation keeps the draft. Denied
  access offers Settings, and unavailable on-device recognition leaves the
  draft intact. The simulator covers the denied state only.
- VoiceOver navigation, Dynamic Type, Reduce Motion, Reduce Transparency and
  the system pickers, on supported phones.
- Timing on the oldest supported phone: cold start, reaching a paired host's
  fleet, scrolling a long chat while it streams on ProMotion and standard
  displays, thermal state and battery. The simulator's numbers for the same
  workloads come from `just ios perf` ([performance](PERFORMANCE.md)) and
  stand in for a phone's until somebody measures one.

## Registered UIKit leaves

`RegisteredLeaves` in `AmuxFeatures` names the only places the screens
package may contain UIKit. A leaf is one file under `Leaves/`, wrapped in one
representable, named there and justified by a written measurement or
argument; `feature-lint.sh` refuses UIKit anywhere else. A name with no file
behind it is a candidate: the question was asked and answered without a
leaf. There are four names and two leaves.

| Name | Status | Why |
| --- | --- | --- |
| `transcriptList` | Leaf, `Leaves/TranscriptList.swift` | A SwiftUI stack placed every drawn row again on every arrival, about 60 ms a frame under the stream; a lazy one could not hold the reader's place. Its heights are its own: UIKit's self-sizing cost most of a frame per arrival too |
| `tokenTextField` | Candidate | Attachments are chips beside the field, so the field is SwiftUI's own |
| `diffSelection` | Candidate | Line frames and a point lookup select a range without a second layout system |
| `menuButton` | Leaf, `Leaves/MenuButton.swift` | SwiftUI's `Menu` does not pass its name or identifier to the button VoiceOver reaches |

A leaf costs a file outside the platform-neutral package, a representable to
wrap it, a second layout system to keep in step with the design tokens, and a
screen that cannot be captured the way every other screen is. Each section
below says what would reopen its question.

### The transcript is a UIKit leaf hosting SwiftUI rows

The transcript is the longest list, the only one that grows while somebody
reads it, and the one people scroll for minutes. Its rows are SwiftUI: one
`ChatRowView` per row, drawn from the row's value, observing its own
`RowCell` so that a change to one row redraws one cell. What holds them is
a `UICollectionView` (`Leaves/TranscriptList.swift`), each cell a
`UIHostingConfiguration` around the same row view, with the app's own
environment (design, photographed, reduce flags, the driving door's
identification) carried into the cell because a hosted cell sits under the
window's traits rather than the screen's environment. The list's data is
every key `ChatModel` holds; nothing bounds what it draws, because it lays
out only the cells in view.

The rows' heights are the leaf's own, not UIKit's self-sizing. Its layout
stacks the rows by height, measures a row with one hidden hosting view
when it first comes within a screen of view (as if photographed, so a row
that parses its markdown on a background task is measured at its final
shape), and forgets a height only when the model says that row was read
again, the width changes or the environment does. The list reports no safe
area to its cells: it runs under the status bar, the header and the home
indicator, and a hosted cell passing under them took their insets as
padding and drew taller than it was placed, over the row below. UIKit's self-sizing was
tried first and measured out: it measures from estimates on demand, keeps
what it measured by index path, animates each result, and measures every
cell on screen again whenever one moves, which under the stream at the
window's cap (every arrival also drops the oldest row) cost most of a
frame per arrival; and three times a row was drawn in another row's frame
after rows landed above it. With heights known before a row is placed, an
arrival is one insert and one re-stack.

It replaced two SwiftUI designs the phone's performance suite measured
failing. A `LazyVStack` over every held row froze for over 90 s placing
the whole list when a reader jumped to the top of about 1,100 rows, and
forgot the heights it had measured when rows were inserted above, so no
correction could hold the reader's place. A plain `VStack` over a bounded
run of 240 rows kept the place, but placed all 240 rows again on every
arrival: under `just ios perf`'s stream (fifty rows a second into the chat
on screen) the main thread sat at 99.9%, about 60 ms of layout per frame,
eighteen frames a second and a hitch time of 650-720 ms/s against a
budget of 5, with the app's own code under 2% of the samples. The leaf
lays out the visible cells, keeps the reader's place itself, and the
suite's streaming numbers ([performance](PERFORMANCE.md), "The phone")
are its measurement.

What the leaf owns, and what stays in `ChatModel`:

- The reader's place. On every change to the sequence while the reader is
  in history (a page landing, a notice above the oldest row coming or
  going, the window dropping rows), and in every layout pass that measures
  rows above the screen, the row crossing the top of the view is put back
  at the same position on screen before anything is drawn, so nothing the
  reader is looking at moves. A reader following the newest row is kept at
  the bottom in every layout pass instead: rows arriving, rows measured,
  the keyboard, a notice or a card changing the space under the rows.
  `ChatFeedTests` holds rows
  still to the pixel under a landing page and under arrivals, holds the
  bottom while following, and shows a streamed chat exactly as the same
  rows open.
- Where the reader is. A drag away from the bottom stops following at
  once, so rows arriving meanwhile are held by the session and New
  activity shows; a drag or deceleration coming to rest within 24 pt of
  the bottom follows again. `ChatModel` is told each time that changes
  and tells the session. A cell among the first twenty coming on screen
  asks the model for the page before the oldest row.
- Going to the newest row. `ChatModel.toNewest` is bumped when somebody
  asks for it (a send, a resume, a reset, New activity), never on an
  arrival; the leaf scrolls there, animated unless the screen is being
  photographed or motion is reduced.
- What the door reads. Each hosted cell reports the elements it declared
  to the leaf, which moves their frames from the cell to the window and
  publishes the union in row order as the same preference the door reads
  from every screen. Only rows with a cell (on screen or prepared) report.

What would reopen it: SwiftUI's `List` or lazy stacks keeping visible
content still when rows are inserted above, at the streaming budget.

### The composer's field is SwiftUI

The composer keeps a draft's attachments as chips in a row above the field,
not as tokens inside its text. The field is SwiftUI's own
`TextField(_:text:axis:)`, and nothing in it needs a run of text to be one
object to the caret. `ChatModel` holds the draft and its attachments; the
chips are the same `AttachmentChip` view wherever an attachment is drawn.
A chip names the file cut in its middle, so the extension stays, and its
size apart, never cut; at an accessibility text size the name may take two
lines and the size goes under it, and the remove mark grows with the text
inside a target of at least 44 points, which the `composer-chips` golden
screen puts in front of the accessibility audit.
What it sends is a `Draft`: the words followed by the attachments, as
ordered parts, which is where a sent message draws them. The parts become
text with one placeholder per attachment only in Rust, so the phone cannot
send an attachment without its place.

What would reopen it: putting attachment tokens inline in the text. SwiftUI's
text editors bind a `String` or an `AttributedString`, and neither has an
attribute that makes a run atomic to the caret or renders an
`NSTextAttachment`, so inline tokens would need a `UITextView` leaf.

### Selecting a range in a diff is SwiftUI

The review page lets a finger draw across a run of lines to comment on them.
A leaf would be bought for the hit test: while a finger drags, the page has
to say which line it is over on every movement, and the worry is that SwiftUI
would either walk the whole patch per update or need a layout system of its
own. It does neither. Each line reports its frame in the list's named
coordinate space through a preference key as it lays out, into a dictionary
keyed by line; the drag looks its point up in that dictionary. Because the
stack is lazy, that is the screenful plus whatever the reader has scrolled
past, not the patch.

The gesture is a 0.3 s long press that hands over to a zero-distance drag. That
is the system's own way to start a selection and what tells the scroll view
to let go: a bare drag scrolls the page, and a tap has one end where a range
needs two. VoiceOver selects with an action on a line instead.

This is an argument, not a measurement: no performance workload covers the
review page. What would reopen it: a real patch on a real phone dropping
frames under a finger.

### Menus are presented by a UIKit button

Every control that opens a menu, the chat's More and the ask card's More, is
a `MenuButton`. SwiftUI's `Menu` presents through a UIKit button it lays over
its own label, and that button is what VoiceOver and XCUITest reach. The name
and identifier given to the `Menu` stay on SwiftUI's side and never reach it,
so the accessibility audit read those controls as buttons with nothing to
say. `accessibilityLabel`, `accessibilityRepresentation`, and a `Label` in
place of a bare image were each tried on the `Menu`, and none reached the
button.

The leaf keeps that button but owns it: a `UIButton` that shows its `UIMenu`
as its primary action, laid transparent over a label SwiftUI still draws and
hides from accessibility, carrying the control's name and identifier itself.
It gives under the thumb the way every other discrete control does. Its rows
are plain values (a title, a symbol, whether it destroys, an action), so a
screen still describes its menu from its state.

What would reopen it: a SwiftUI `Menu` whose accessibility modifiers reach the
button it presents through.
