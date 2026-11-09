# Mac app

*For people building the Mac app: what it is today, how it is built and run,
and what it shares with the iPhone app.*

The Mac app is a client of the amux daemon on the Mac and nothing more. It
hosts no installation of its own: it installs amux when the Mac has none,
attaches to the daemon behind `<root>/amux.sock`, and from then on every
profile, sign-in and agent it shows is the daemon's. Quitting it, or deleting
it, leaves amux running.

The window shows the path every desktop client takes: install, attach, set
the Mac up, sign in, switch profiles, list a profile's agents. Apart from
setting up and Settings, its screens are a placeholder until the desktop gets
a design of its own.

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

Screens, navigation and layout are the desktop's own; nothing from
`AmuxFeatures` or `AmuxShell` is linked. They live in the `AmuxMacScreens`
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
and sidebars blank. The window is never active, so its traffic lights are grey
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

- The app's real screens beyond setting up and Settings, and its copy in the
  string catalogue.
