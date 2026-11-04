# Mac app

*For people building the Mac app: what it is today, how it is built and run,
and what it shares with the iPhone app.*

The Mac app is a client of the amux daemon on the Mac and nothing more. It
hosts no installation of its own: it installs amux when the Mac has none,
attaches to the daemon behind `<root>/amux.sock`, and from then on every
profile, sign-in and agent it shows is the daemon's. Quitting it, or deleting
it, leaves amux running.

Its screens are a placeholder until the desktop gets a design of its own. The
window today shows the path every desktop client takes: install, attach, sign
in, switch profiles, list a profile's agents.

## What it shares

Everything in Rust, and on the Swift side the `AmuxCore` package with its
generated values (`AmuxValues`), which build for macOS as well as iOS:

- `Runtime.install(_:)` makes sure amux is installed and answering. It returns
  at once when the daemon answers, starts an installed amux that is stopped,
  and otherwise installs the copy the app carries in `Contents/Helpers/amux`
  (or, without one, the stable channel's build, checked against the signed
  manifest) with a login item, and starts it.
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

Screens, navigation and layout are the desktop's own; nothing from
`AmuxFeatures` or `AmuxShell` is linked.

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
then with Developer ID by `codesign` under the hardened runtime. Two
artifacts come from that one app:

- `Amux-<version>.zip`, the app alone: the update. It is not notarized; the
  app downloads it itself, and nothing marks that download for Gatekeeper.
- `Amux-<version>.dmg`, the app carrying the stable channel's `amux` (checked
  against its signed manifest and signed as `sh.amux.cli`) in
  `Contents/Helpers`, beside a link to Applications. It is what a browser
  downloads, so it is notarized and stapled; `--no-notarize` skips that,
  and such a disk image needs "Open Anyway" once, so a stable deploy
  refuses it and only preview takes it.

The tag's message records both digests, and the deploy refuses a file on the
GitHub Release that is not the one this Mac built. A deploy signs the
channel's Mac feed over the zip with the release key and publishes it
through the operator's publish script as `mac-<channel>.json`; a stable
deploy publishes the disk image as `Amux.dmg` beside it, so
`https://amux.sh/releases/Amux.dmg` is always the current one. There is no
staged rollout for the Mac app.

## Not yet

- The app's real screens, its copy in the string catalogue, and tests.
