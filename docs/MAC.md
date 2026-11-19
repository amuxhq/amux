# Mac app

*For people building the Mac app: what it is today, how it is built and run,
and what it shares with the iPhone app.*

The Mac app is a client of the amux daemon on the Mac and nothing more. It
hosts no installation of its own: it installs amux when the Mac has none,
attaches to the daemon behind `<root>/amux.sock`, and from then on every
profile, sign-in and agent it shows is the daemon's. Quitting it, or deleting
it, leaves amux running.

The window shows the path every desktop client takes: install, attach, set
the Mac up, sign in, switch profiles, then the profile's agents and hosts.

## The main window

The main window is laid out like the developer tools it sits beside (Codex,
Claude, T3 Code): a flush, full-height sidebar with the window's buttons on
it, the open thing in the centre, and a pane on the right for an agent's
changes. It is designed as a desktop app — menus, shortcuts, hover, drag and
drop, the person's own Terminal, Finder and editor — not as the phone's
screens made larger.

- **The rail** down the left edge, a step darker than the sidebar, carries
  amux's mark and switches between surfaces: Agents and Hosts today, with
  room for more. Its foot holds Settings and the open profile's picture,
  whose menu signs in or out and switches profiles. A coral count on Agents
  says how many agents need the person.
- **The agents sidebar** opens with New Agent (⌘N) and Search (⌘F) as plain
  rows, then every agent in the runtime's sections and order (pinned,
  needing the person, the rest). A row is its repository's tile, then its
  name and age over where it works ("amux · build-box"). What it is doing
  is a badge on the tile: a blue arc while it works, a coral dot when it
  needs the person, a red dot when it failed, a cloud when its host cannot
  be reached. A row that needs the person, or failed, says why in place of
  where it works. Hovering a row swaps its age for pin and settle buttons;
  its menu pins, settles, renames and deletes. A family's members hang off a
  line under the head's tile, each saying what it is doing; the head's count
  folds them. A subagent Claude or Codex runs for its parent hangs there by
  its title too, but has no pin or Delete: it goes with its parent. Delete
  asks in a sheet rather than a system dialog, because its words change
  once the agent's host has looked at the worktree: it says what goes in the
  phone's words (`DeleteWords`, from the runtime's delete warning), with a
  spinner while the host is checking. The settled agents sit
  behind one row anchored at the foot, which swaps the sidebar to them and
  back. Search narrows the list through the runtime's filter, and the arrow
  keys move the selection.
- **The hosts sidebar** lists this Mac, then the machines the profile trusts,
  each saying how this Mac reaches it. Picking one opens its page in the
  centre: its platform, version, which providers are signed in, its
  addresses, what being away or offline means for its agents, and the
  agents on it, each opening its chat. A paired machine's page ends with
  Stop Trusting, which revokes it after a confirmation.
- **New Agent** is the empty centre, not a sheet: a heading naming the
  folder, a composer for the first message, and chips under it for the
  host (this Mac to begin with; an unreachable one cannot be picked), the
  folder (the host's recent folders and repositories, and a popover that
  searches as typed and takes a path), the agent with the model, effort and
  mode its host offers and a new worktree, and the permission. Return
  starts the agent on the host the placeholder names, opens its chat, and
  sends the message as its first prompt once the chat is ready for it. A
  refusal is said under the composer in the host's words.
- **The top bars** are the title bar's height, so the window's buttons centre
  in the sidebar's. The sidebar's bar holds the sidebar toggle and back and
  forward through the agents opened (⌘[ and ⌘]). The centre's names the open
  agent by its folder and name, with a menu to open it in Terminal, pin and
  settle it; then Open in Terminal (⇧⌘T), which runs
  `amux attach <agent> --in native` in Terminal for the agent's own
  interface; then the changes toggle (⌥⌘I). A hidden sidebar's buttons move
  into it.
- **The chat** fills the centre down a reading width. A finished turn reads
  answer first: the prompt, a "Worked for …" line summarising the steps,
  then the agent's final words, with the steps folded under the line; an
  error, and a failed step the turn never fixed, stay in view. The fold is
  the shared view's (`ToolRows::Turns`, docs/CLIENT.md), which the app turns
  on as it opens a chat; the app draws the places it is handed. Under the answer sit copy and, when the turn changed
  files, their count, which opens the changes. The turn still running shows
  its steps as they happen, on one grid of glyph, verb, subject and meta.
  Code blocks name their language and copy on hover. Under the feed stands
  one thing at a time, as on the phone: an open ask in the composer's place
  until it is answered, the chat's ending once it ended, or the composer
  with the queue above it. The ask card shows what is asked in full with the
  provider's answers as buttons, the expected one drawn solid and on Return;
  a question's options to pick, and Reply Instead where the agent takes a
  reply; and a note for an answer that takes one.
- **Subagents** a turn started through Claude's or Codex's own tool fold to
  one line, "4 subagents", saying what they come to ("2 running · 1 waiting
  on you · 1 done", the waiting count in coral); a click on it unfolds the
  calls. Each call names its subagent with where that agent stands, read
  from its fleet row rather than the call, and a click on the call, or its
  arrow, opens that subagent's chat. Unfolding and opening are separate
  targets. The subagent's chat has no composer, since the provider lets only
  the parent's model talk to it: a bar says who runs it and offers Stop
  (⌘.), which stops the job running it.
- **The composer** grows with the draft; Return sends, Shift-Return starts a
  line, ⌘. stops the turn, and a long paste becomes an attachment. Under it:
  + attaches files (dropping them on it does too), the permission the agent
  works under as a menu (orange when it never asks), and the model and
  effort as a menu of what the agent offers, each changing the agent as
  picked.
- **The floating card** at the chat's top right, while the changes pane is
  closed, names the host, folder (its full path on hover) and branch; the
  changes, opening the pane; the agent's parent and children as Lineage,
  each opening its chat; and Open in Terminal, and for an agent on this Mac,
  Show in Finder and Open in the first editor installed (VS Code, Cursor,
  Zed, Sublime Text, Xcode). It floats in the margin when the margin holds
  it, moves the chat left to make room while the chat stays readable, and
  otherwise folds to a pill of the change counts. A subagent its provider
  runs has no folder or terminal of its own, so its card shows neither the
  changes nor the Terminal, Finder and editor actions, and the title bar
  hides Open in Terminal and the changes toggle.
- **The changes pane** on the right shows the changes as the host freezes
  them, against the last commit or the branch's base (a menu in its bar),
  a file at a time, with their counts, reload and close. A click picks a
  line to comment on and Shift-click takes in more, and the written comments
  go into the draft for the next message. In a window too narrow for all
  three columns the pane gives way to its narrowest, then the sidebar steps
  aside while it is open.
- **The menu bar** carries what the window does: New Agent (⌘N); Show or
  Hide Sidebar (⌃⌘S) and Changes (⌥⌘I), Agents (⌘1) and Hosts (⌘2) in View;
  Back and Forward in Go; Open in Terminal in Agent.

**Repository tiles** come from the runtime's fleet card, the same on every
client: the repository's name is the last segment of its origin remote (else
the folder's name), the tile shows that name's first and last letter, and its
tint is one of eight picked by a hash of the remote. Checkouts of one
repository — worktrees, and the same repository on another machine — share a
tile because they share a remote; a fork has its own.

**Colour** follows one rule, taken from the app icon: a cool blue-black
ground in three steps (rail, sidebar, content), blue for amux itself
(selection, the action to take, work under way), coral for whatever needs the
person and nothing else, red for failure only, amber for a permission that
never asks, and green and red otherwise only in diffs. Tile tints avoid all of
those, so identity never reads as state. The palette is `Palette` in
`Chrome.swift`.

**Sizes** come from `Tokens.swift`: every text size is a step of `TextSize`
(`Font.ui` and `Font.mono`; monospaced text takes the half steps, since it
reads larger at the same size), every corner radius a `Radius`, and every
shadow one of two `Elevation`s. The page gutter and a round control's size
are in `Metrics`. Change how a kind of text or surface reads there, not where
it is drawn. The chat's composer and the New Agent page share one frame,
`ComposerShell`, with `RoundButton` and `ChipLabel` under the field.

The icons are [Lucide](https://lucide.dev)'s, under the ISC licence, bundled
as SVG in `AmuxMacScreens/Icons` with that licence beside them and drawn as
template images, so they take the text colour. Add one by copying its SVG
there with the stroke set to `#000` and its width to 1.75.

The window is one `Place`: the surface, the agent or host open, whether the
sidebar shows the settled agents, whether the changes pane is open, and the
agents opened before and after the one in view. The storybook
puts a story anywhere by handing one in.

The screens read a chat through `ChatSurface` and a review through
`ReviewSurface`, protocols the app's `MacChat` and `MacReview` answer by
reading the shared `ChatModel` and `ReviewModel` directly, so a view that
reads one row redraws when that row moves. New Agent reads the shared
`NewAgentStore` through `NewAgentSurface` in the same way, with what leaves
the Mac — listing a host's directories, asking what it offers, starting the
agent — sent through the `StoreBundle`. The storybook answers each of them
with one that holds still.

## Setting up

A profile whose setting up has steps still needed opens on them, in the order
the daemon keeps and the terminal shows, at the first one not done; a launch
part way through resumes there. Every step a screen shows is answered `shown`,
and Escape or Later skips it.

| Screen | What it does |
| --- | --- |
| Connect your iPhone | The Mac's name, with Rename to change it at once; a QR code the app draws for the window's link, with "Scan with your iPhone's camera to pair." and "Or enter 418 207 in the amux app."; then "Your iPhone is pairing…" as a phone joins, and a lockout with a new code after wrong PINs. The window is renewed before it ends unless a phone is joining, and closed when the screen leaves or the app quits. |
| Keep it connected | "Paired with <phone> ✓" once a phone paired, then start at login and keep awake, both ticked, applied on Continue. |
| Reach it from anywhere | Sign In runs `WebSignIn`. "Also sign in <phone>" is ticked when the phone just paired is signed out; after the sign-in the Mac offers it the account and opens the account service's offer page. When a paired machine offers its account instead, the screen asks "Sign in as <email> (from <machine>)?". |
| Repositories | The repositories Claude Code and Codex ran in lately, ticked, and Add a Folder. |
| Help improve amux | "Send a report automatically when something goes wrong", unticked, with "Never your chats, code, or file names." under it, then one line: "amux sends usage counts to help improve it, never your code or chats. You can turn this off in Settings." |
| Try starting an agent from your phone | Waits for an agent a phone starts here, then lands on the agent list with it selected. |

Steps left skipped show as "Finish Setting Up (n)" above the agent list,
which walks them again or, dismissed, stops offering them.

## Settings

Settings (⌘,) holds the Mac's name, start at login (on an install with a
supervisor), keep awake, the repository list, usage counts, automatic reports
and the account. Every change is applied at once through `UpdateSettings`,
so `amux config` and the terminal see it as soon as it is made.

## What it shares

Everything in Rust, and on the Swift side the `AmuxCore` package with its
generated values (`AmuxValues`), which build for macOS as well as iOS:

- `Runtime.install(_:)` makes sure amux is installed and answering. It returns
  at once when the daemon answers, starts an installed amux that is stopped,
  and otherwise downloads the stable channel's build, checks it against the
  signed manifest, and starts it. The app carries no amux of its own: a Mac
  app has no step that runs on install, so its first launch is that step,
  and the build it fetches is the one the supervisor would move a carried
  copy to anyway. It adds no login item: whether amux starts at login is the
  `start_at_login` setting, which the person chooses.
- `Runtime.attach(_:profilesMoved:)` answers the same `Runtime` the phone's
  `Runtime.start` does, over the daemon's sockets. Calls only an installation
  hosted in the app can answer (offering pairing from this device, its roster,
  a bearer) answer that they are the phone's.
- `WebSignIn` runs amux.sh's sign-in in `ASWebAuthenticationSession`, presented
  over the app's window. On the Mac the session opens the person's default
  browser when it supports authentication sessions, and Safari's window
  otherwise. `CloudEndpoint.desktop` names the account service's `desktop`
  client and its `sh.amux.desktop://callback` redirect; the session catches the
  redirect itself, so nothing registers the scheme. The refresh token goes to
  the daemon with `Runtime.bind`, and the app keeps nothing.

- `Runtime.checkForUpdate(_:)` looks for a newer version of the app itself
  ([Updates](#updates)).
- The setting-up calls: the installation's settings, repository suggestions, a
  profile's steps and their answers, a pairing window and its status, and a
  web sign-in link (`crates/app-ffi/README.md`). An attached runtime names
  itself `mac` to the daemon, which counts the app opening and each step as
  the Mac's.

- Code colouring: chat code blocks and edit cards take their runs from the
  shared view through the bridge, as the phone's do (`CodeHighlighting` in
  `AmuxWords`), drawn in the palette's `syntax` colours. The app puts the
  bridge's into the window's environment; the storybook, which links no
  bridge, draws code plain.

Screens, navigation and layout are the desktop's own; nothing from
`AmuxFeatures` or `AmuxShell` is linked. The words are shared: rows and chats
say what the phone says through `AmuxWords`, and the app's model draws from
the same `StoreBundle` (fleet, hosts, chats, starting an agent) the phone
does. They live in the `AmuxMacScreens`
package, drawn from values and reporting through actions; the app's models in
`Mac/Sources` hold the state and fill those values in.

## Building and running

```sh
just mac build   # the bridge's Mac slice, the checkout's amux, the app
just mac run     # build, start this checkout's daemon, run the app on it
```

`just mac rust` adds the `macos-arm64` slice to the `AmuxApp.xcframework` the
Swift packages link (`apps/apple/Tools/ios-rust.py --mac`). The iPhone app's
force-loaded driving library never carries it, and a later iPhone build that
rebuilds the bridge drops it again, which the next Mac build puts back.

The project is generated from `apps/apple/Mac/project.yml` into
`apps/apple/Mac/AmuxMac.xcodeproj`, apart from the iPhone app's, so neither
app's recipes build or check the other. The app is built for Apple silicon,
macOS 26, with the hardened runtime and without the sandbox: it reaches amux's
sockets under `~/.amux` and installs amux into `~/.amux/bin`. Development
builds are signed ad hoc and carry the checkout's debug `amux`; release
builds carry none ([Releases](#releases)).

```sh
just mac test    # the app's tests, with no window on screen
```

`just mac test` compiles the app's sources without its entry point: one test
walks setting up against a scratch daemon of its own (reading the pairing QR
code back as the daemon's link) and checks it writes no login item, and one
holds the storybook to its list.

`AMUX_CONFIG` names the installation the app attaches to, as it does for every
amux command, so in a worktree `just mac run` attaches to that tree's daemon.
With it set the app installs and starts nothing: such an installation runs its
own build, and the installed amux started on its root, or a login item
rewritten for it, would take the person's real installation's place.
`AMUX_CLOUD` points its sign-in at another account service. The app logs what
it installed, attached and listed under the `sh.amux.desktop` subsystem:

```sh
log show --predicate 'subsystem == "sh.amux.desktop"' --last 5m
```

## Storybook

The storybook is the app's whole screens by name, each drawn from values with
inert actions. It links `AmuxMacScreens` and the generated values and not the
Rust bridge, so a change builds in seconds and it cannot reach anyone's amux.

```sh
just mac storybook                         # the viewer, rebuilding on save
just mac storybook --story fleet.busy      # open on one
just mac storybook capture --out DIR [--only fleet.]
```

The viewer is two windows: the stage, styled as the app's own window so its
title bar, toolbar and glass are the real ones, and a panel that picks the
story, appearance and window size. ⌘[ and ⌘] step through the stories, ⌘T
cycles the appearance and ⌘S the size; click another app to see a window that
is not active. It opens where it was left. Saving a screen rebuilds it and
reopens the same story behind whatever you are working in; a failed build
leaves the last one up, says why on the panel and keeps its log in
`target/mac/storybook/build.log`. Quitting either end stops the other.

`capture` draws every story at each window size in dark and light as PNG
files, with an `index.md`, without putting anything on the desktop, so an
agent can look at a screen while somebody works at the Mac. Each story's
window is real but sits past the edge of every display, and the window server
photographs it. Drawing a view into a bitmap would not do: that leaves glass
and sidebars blank. A sheet's story is drawn alone, without a window around
it, at one size. The window is never active, so its traffic lights are grey
and its selection muted. A capture fails when a story's window comes out empty.

The stories are `Packages/AmuxMacScreens/Sources/AmuxMacStories/Stories.swift`.
The list of stories is `tests/stories.json`, shared with the terminal and the
phone: a story drawn on the Mac names `mac` among its clients, and a test fails
when the catalogue and the list differ. No picture is compared while the
screens are still being designed.

## Updates

The app updates itself, apart from amux, which its supervisor updates. At
launch and every hour it reads its own feed, `mac-<channel>.json` beside
amux's `<channel>.json` under the installation's `releases_url`, for the
installation's channel. The feed is amux's signed manifest format with one
target, `mac-app`, naming the zipped app; the check is the installer's
(`installer::update`), so the manifest's signature and the zip's size and
hash are checked by the same code that checks an amux download. It looks
before installing or attaching to amux, so an app that cannot reach the
amux it finds can still fetch the version that can.

A newer version is downloaded and unpacked into
`~/Library/Application Support/sh.amux.desktop/Updates/<version>/Amux.app`,
its `ready` file holding the zip's digest, so a version staged from
another feed is fetched again rather than swapped in, and the window shows a
quiet "Restart to Complete Update". Quitting the app
any way puts the staged app in place of the running one
(`FileManager.replaceItemAt`); the button quits and opens the new one once
this one has exited. An app that cannot replace its own bundle (run from its
disk image, or moved by Gatekeeper's App Translocation) does not look.

A build trusts the keys compiled into its bridge. A release build trusts the
release keys; a development build trusts none, so it finds no update, and a
test of the updater builds the bridge with `AMUX_RELEASE_PUBLIC_KEYS` naming
the test key and serves a feed on loopback.

## Releases

```sh
just mac release 0.0.2 --rehearse   # build and sign both artifacts; write, tag and upload nothing
just mac release 0.0.2              # the same, then commit, tag mac-v0.0.2, push, GitHub Release
just mac deploy 0.0.2               # sign the stable Mac feed and publish it, with Amux.dmg
just mac deploy 0.0.2 --channel preview
```

The app versions on its own: `MARKETING_VERSION` in `project.yml`, which is
also its build number, and tags `mac-v<version>`. A release builds on the
release Mac, since the signature is made with its Developer ID certificate:
the bridge's Mac slice under the `release` profile without the driving tools
(`apps/apple/Tools/ios-package.py --mac`), built to trust the release keys
the Release workflow names, then the Release app, signed ad hoc by Xcode and
then with Developer ID by `codesign` under the hardened runtime. That one
signed app is packed two ways:

- `Amux-<version>.zip`: the update, which an installed app downloads
  itself. Nothing marks that download for Gatekeeper.
- `Amux-<version>.dmg`: the install, the app beside a link to Applications
  to drag it onto. It is what a browser downloads, so it is notarized and
  stapled, which covers the app inside and so the zip's copy too.
  `--no-notarize` skips that, and such a disk image needs "Open Anyway"
  once, so a stable deploy refuses it and only preview takes it.

The tag's message records both digests, and the deploy refuses a file on the
GitHub Release that is not the one this Mac built. A deploy signs the
channel's Mac feed over the zip with the release key and publishes it
through the operator's publish script as `mac-<channel>.json`; a stable
deploy publishes the disk image as `Amux.dmg` beside it, so
`https://amux.sh/releases/Amux.dmg` is always the current one. There is no
staged rollout for the Mac app.

## Not yet

- Pairing another phone or host after setting up, and a host's fingerprint
  on its page.
- The app's copy in the string catalogue.
