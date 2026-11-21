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

The main window is laid out like the developer tools it sits beside: a
dark **frame** holding the system's window buttons, a title row and a rail,
and on it one rounded **panel** that holds the app's columns — the sidebar,
the chat and, when open, the tool window — with a hairline between each.
The panel's corners are inset from the window's and run concentric with
them; the columns inside it are joined, not separate cards, which keeps the
window quiet. It is designed as a desktop app — menus, shortcuts, hover,
drag and drop, the person's own Terminal, Finder and editor — not as the
phone's screens made larger.

The frame's geometry follows the system's window buttons, which are never
moved: everything on the title row is centred on their centre line, and
the panel starts under the title row and sits a few points in from the
window's right and bottom edges. One shape rule holds across the window:
circles are live things and people (the window buttons, counts, the
profile picture, send and stop); rounded squares are places and things (the
panel, repository tiles, rows, tabs, chips, cards).

- **The title row**, on the frame, holds the sidebar toggle and back and
  forward through the agents opened (⌘[ and ⌘]) after the window buttons.
  Over the chat it is the chat's toolbar, a path to where the open agent
  is: its repository's tile and name, the agent that started it (a click
  opens it) when it has a parent, and its own name as a menu to pin,
  settle and attach it and to open its relatives; then its branch, and its
  host when that is not this Mac. As the window narrows the repository's
  name goes first, then the host, then the branch, and the agent's name
  truncates last. At its end, one button with the editor's own icon opens
  the folder in the first editor installed (VS Code, Cursor, Zed, Sublime
  Text, Xcode), and its menu shows it in Finder or attaches it in Terminal
  (⇧⌘T), which runs `amux attach <agent> --in native` for the agent's own
  interface. Restart to Update shows as a blue pill when an update is
  ready. At the far right is the tool window's toggle (⌥⌘I), which carries
  the open agent's change counts while the tool window is closed. The
  window drags from the row's empty parts and the frame's.
- **The tool window's tabs** stand on the title row over the tool column
  while it is open: Review is the only tool today, with a + menu to open a
  tool (Review, or the agent in Terminal) and room for terminals and
  browsers as tabs later. The open tab is lifted on a faint fill with a
  hairline ring; a tab's × shows on the open tab and under the pointer.
- **The rail** down the frame's left edge switches between surfaces:
  Agents and Devices today, with room for more. Its buttons are 36-point
  rounded squares; the open surface's sits on the hover fill. Its foot
  holds Settings and the open profile's picture, whose menu signs in or out
  and switches profiles. A blue count on Agents says how many threads of
  work need the person. Hiding the sidebar leaves the rail.
- **The agents sidebar** lists threads of work and which of them need the
  person. Its bar is search (⌘F) with New Agent (⌘N) beside it; then a
  card for each family's head in the runtime's order (pinned, needing the
  person, the rest), with no section headings: the order says it, pinned
  cards carry a pin, and the ones that need the person say so. The agents
  a head started, and the subagents Claude or Codex run for it, are not
  listed: the head stands for them, its last line saying what one of them
  asks when it needs the person, its card lit while one of them is open,
  and they are reached from its chat. A card is three lines. The first is
  where it works — the repository's tile and name, a pin when pinned — and
  at its end either its state or its age: "Needs you" in blue with a
  raised hand, "Working 2m" in plain grey words (work going on is the
  normal state, so it carries no glyph), Starting,
  "Failed" in red, Offline when its host cannot be reached. The second is
  its name; an agent at rest recedes a step. The third is what it is doing,
  asking, or why it stopped, and at rest its branch and its host when that
  is not this Mac, with the provider's glyph at the end. Hovering a card
  swaps its age for pin and settle buttons; its menu pins, settles, renames
  and deletes. Delete asks in a sheet rather than a system dialog, because
  its words change once the agent's host has looked at the worktree: it
  says what goes in the phone's words (`DeleteWords`, from the runtime's
  delete warning), with a spinner while the host is checking. The agents
  at work are left out of the list, since work going on is not news until
  it stops: "2 working" beside New Agent, in the rows' own word, brings
  them back in their places, lit while they show and faintly while it
  hides the agent open in the centre. A pinned agent, one with a member
  waiting on the person, and anything a search finds stay in the list.
  The settled agents sit behind one row anchored at the foot, which swaps
  the sidebar to them as a denser list of one-line rows, its bar leading
  back. Search
  narrows the list through the runtime's filter, and the arrow keys move
  the selection.
- **The hosts sidebar** lists this Mac, then the machines the profile
  trusts, each as a card with its kind's tile, its name and how this Mac
  reaches it. Picking one opens its page in the centre: one line of its
  reach, platform and amux version, which providers are signed in, its
  addresses (labelled, each copied from a button on hover), what being away
  or offline means for its agents, and the threads of work on it, each
  opening its chat. A paired machine's page ends with Stop Trusting, which
  revokes it after a confirmation.
- **New Agent** is the empty centre, not a sheet: "What should we build
  in **amux**?", naming the chosen folder in a heavier weight so it reads
  as the folder rather than the product, and a composer for the first
  message, with the agent — the
  provider's mark, model and effort — inside it beside send, opening the
  picker.
  Under it, a strip of chips for the host (this Mac to begin with; an
  unreachable one cannot be picked), the folder (a popover listing the
  host's repositories, its recent folders, then where Claude Code and
  Codex ran, read from the host's entry and searched as typed; a pin on
  each row under the pointer keeps it among the host's repositories or
  lets it go; Add repository… on this Mac; and a typed path), a new worktree as a toggle of its own, and at the far end the
  permission. Folders that are repositories carry their tile. As the
  window narrows the chips give way to their icons, the folder last. The
  recent folders sit under the strip as one line of chips. Return starts
  the agent on the host the placeholder names, opens its chat, and sends
  the message as its first prompt once the chat is ready for it. A refusal
  is said under the composer in the host's words.
- **The chat** fills the centre down a 736-point reading column. The
  prompt is a bubble on the right; the answer is prose on the ground, not
  in a bubble. A finished turn reads answer first: the prompt, a "Worked
  for …" line over a rule summarising the steps, then the agent's final
  words, with the steps folded under the line; an error, and a failed step
  the turn never fixed, stay in view. While a turn runs, "Working for …"
  stands in that place and counts. The fold is the shared view's
  (`ToolRows::Turns`, docs/CLIENT.md), which the app turns on as it opens a
  chat; the app draws the places it is handed. Under the answer, when the
  turn changed files, a card lists them with their counts and Open in
  Review; then copy and the time it was said. Steps read quieter than the
  answer, on one grid of glyph, verb, subject and meta: the verb is
  secondary, the subject monospaced, and only the step at work is lit, with
  its time counting; the activity line under the feed shows only what no
  step says (thinking, compacting, waiting on subagents). Every fold opens
  with one trailing chevron that turns as it opens. Code blocks are set
  into the window, a shade darker, with their language and Copy on a quiet
  header; inline code is the monospaced face on a faint chip (a rounded
  pill would need a custom text renderer, which SwiftUI skips on selectable
  text, and copying from an answer matters more). The feed fades out under
  the toolbar as it scrolls. Under the feed stands one thing at a time, as
  on the phone: an open ask in the composer's place until it is answered,
  the chat's ending once it ended, or the composer with the queue above it.
- **The ask card** says what kind of ask it is in a quiet header (a shield
  for a permission, a question mark for a question), shows what is asked
  in full — a command with its folder before it, as a shell prompt does —
  and puts the provider's answers in one right-aligned row, as a Mac dialog
  orders them: the refusal first and on Escape, the expected answer last,
  solid blue and on Return, each with its key shown, and the provider's
  other answers in a menu split off the expected one. A question's options
  are picked from the card, with Reply Instead where the agent takes a
  reply, and a note for an answer that takes one.
- **Subagents** a turn started through Claude's or Codex's own tool fold to
  one line, "4 subagents", saying what they come to ("2 running · 1 waiting
  on you · 1 done", the waiting count in amber); a click on it unfolds the
  calls. Each call names its subagent with where that agent stands, read
  from its fleet row rather than the call, and a click on the call, or its
  arrow, opens that subagent's chat. Unfolding and opening are separate
  targets. The subagent's chat has no composer, since the provider lets only
  the parent's model talk to it: a bar says who runs it and offers Stop
  (⌘.), which stops the job running it.
- **The composer** is one rounded card that grows with the draft from two
  lines; Return sends, Shift-Return starts a line, ⌘. stops the turn, and a
  long paste becomes an attachment. Along its foot: the paperclip at the
  left (dropping files on the composer attaches them too); at the right the
  model and effort, led by the provider's mark, opening the picker; the
  permission the agent works under (a shield in orange when it never
  asks); and the round send button — blue with a draft, and while the
  agent works with the draft empty, stop as a disc of the text's ink (white
  in dark), since stopping is ordinary rather than an alarm.
- **The picker** is one panel rather than a long menu, opening from the
  model chip: for a new agent the providers down its left, each saying
  when the chosen host has it signed out; the provider's models with what
  each is for, searchable once there are many; and the chosen model's
  efforts and the agent's modes on the right. A pick applies at once and
  the panel stays, so a model and its effort are chosen in one visit.
- **Review**, the tool window's first tool, shows the changes as the host
  freezes them, uncommitted or against the branch's base (a menu in its
  toolbar), a file at a time under sticky file headers, with their counts
  and reload. Long lines wrap rather than being cut, hanging under their
  own indent; an added or removed line takes its tint across the numbers,
  with a bar of its colour down the edge. A click picks a line to comment
  on and Shift-click takes in more, and the written comments go into the
  draft for the next message. In a window too narrow for all three columns
  the tool window gives way to its narrowest, then the sidebar steps aside
  while it is open. The window's least width fits the rail, a readable chat
  and the narrowest tool window.
- **The menu bar** carries what the window does: New Agent (⌘N); Show or
  Hide Sidebar (⌃⌘S) and Tools (⌥⌘I), Agents (⌘1) and Devices (⌘2) in View;
  Back and Forward in Go; Open in Terminal in Agent.

**Repository tiles** come from the runtime, the same on every client: the
repository's name is the last segment of its origin remote (else the
folder's name), its mark the first and last letters of that name, and its
tint one of eight picked by a hash of the remote. Checkouts of one
repository — worktrees, and the same repository on another machine — share
a tile because they share a remote; a fork has its own. Agents carry it on
their fleet card; folders a host lists for New Agent and setting up carry
it too, from the folder's checkout (`Folder.checkout`; outside Git, the
folder's path names it). The
Mac draws it as a small rounded square with the mark in the tint on a
wash of it.

**Colour** is neutral: true black for the panel and a step up for the
frame in dark, near-white columns on a light grey frame in light — no hue
in the ground, so colour is kept for meaning. What is lifted off the
ground (the composer, the ask, cards, menus) steps up in lightness; what
is set into it (code, a command) steps down. Text has four tiers of the
app's own (`Palette.text`, `textSecondary`, `textTertiary`,
`textQuaternary`), named directly where text is drawn. SwiftUI's
`.secondary` and `.tertiary` are not used: they are fractions of whatever
style is set above them, so under any single colour they turn into a
translucent version of it. `textTiers()` at each screen's root only keeps
system controls in step. The answer's prose is a touch softer than the
text tier. One hue carries attention: blue is the person's turn — send,
the expected answer, an agent that needs them (a question and a
permission told apart by their glyphs), the rail's count, focus, links,
the update. An agent at work is neutral, since work going on is not
news; selection is neutral. Red is failure only, orange a permission
that never asks, and green and red otherwise only in diffs. The tones
are softened from full saturation so they sit on near-black without
glowing. Repository tints avoid those, so identity
never reads as state. The palette is `Palette` in `Chrome.swift`.

**Sizes** come from `Tokens.swift`: every text size is a step of `TextSize`
(`Font.ui` and `Font.mono`; the system face takes whole points only, and
monospaced text takes the half steps too, since it reads larger at the same
size), every corner radius a `Radius`, and every shadow one of two
`Elevation`s. The frame's geometry (the rail, the gap, the bar height,
where the title row's buttons start), the columns' widths, the reading
column and a round control's size are in `Metrics`. Change how a kind of
text or surface reads there, not where it is drawn. The chat's composer
and the New Agent page share one frame, `ComposerShell`, with `RoundButton`
and `ChipLabel` along its foot.

The icons are the system's SF Symbols, drawn in the text's weight so a
glyph sits with the words beside it. Screens name an icon by the app's
own name (`Icon("git-branch")`), and `Icons.symbols` in `Chrome.swift`
maps each to its symbol, so swapping a symbol is one line. An icon beside
words is set a little low (`Icon`'s `optical`), halfway from the
capitals' middle to the lowercase middle, because centring puts it on
the capitals and it reads high beside lowercase words. An icon alone in
a button is centred. The providers' marks are the only drawings, in
`AmuxMacScreens/Icons` with their licences in `NOTICE`: Claude's, in its
terracotta where the provider is being chosen or named (the picker and
the model chip) and in the text's colour elsewhere, and OpenAI's for
Codex. A mark is found by the provider's id (`provider-<id>.svg`), and an
agent with none is named in its mark's place. The built-in agents that
speak ACP have no mark here: none of Antigravity's, OpenCode's or Grok's
owners grants one to a client like this (`NOTICE` says why), and an agent a
person added gets its name, never a mark made up for it. "Open in Terminal"
is offered only for an agent with a terminal interface of its own, as its
card or its chat's capabilities say.

The window is one `Place`: the surface, the agent or host open, whether the
sidebar shows the settled agents, whether the tool window is open, and the
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
and Escape or Later skips it. Each step is one centred column with its
glyph on a tile, its choices in a left-aligned box, and its action in a bar
at the foot.

| Screen | What it does |
| --- | --- |
| Connect your iPhone | The Mac's name, with Rename to change it in place; a QR code the app draws for the window's link, with "Scan with your iPhone’s camera to pair." and "Or enter 418 207 in the amux app." (the PIN large, monospaced and selectable); then "Your iPhone is pairing…" as a phone joins, the code dimmed under a spinner, and a lockout with a new code after wrong PINs. The window is renewed before it ends unless a phone is joining, and closed when the screen leaves or the app quits. |
| Keep it connected | "Paired with <phone> ✓" once a phone paired, then start at login and keep awake, both ticked, applied on Continue. |
| Reach it from anywhere | Sign In runs `WebSignIn`. "Sign in on <phone> too" is ticked when the phone just paired is signed out; after the sign-in the Mac offers it the account and opens the account service's offer page. When a paired machine offers its account instead, the screen asks "Use <email>?" ("<machine> is signed in with this account…"), with Use Another Account to turn it down for the browser sign-in. |
| Where should agents work? | The repositories Claude Code and Codex ran in lately, ticked, and Add a Folder. |
| Help improve amux | "Send a report automatically when something goes wrong", unticked, with "Never your chats, code, or file names." under it, then one line: "Usage counts are sent to help improve amux, never your code or chats. You can turn them off in Settings." |
| Try starting an agent from your phone | Waits for an agent a phone starts here, then lands on the agent list with it selected. |

Steps left skipped show as "Finish Setting Up (n)" above the agent list,
which walks them again or, dismissed, stops offering them.

## Settings

Settings (⌘,) holds the Mac's name, start at login (on an install with a
supervisor), keep awake, the repository list, usage counts, automatic reports
and the account. Every change is applied at once through `UpdateSettings`,
so `amux config` and the terminal see it as soon as it is made. It is drawn
on the main window's grouped cards, as the host page is, rather than as a
system form: each setting is a title over a quiet note with its control at
the right, and a repository is removed from a button on its line under the
pointer.

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
