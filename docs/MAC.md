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
builds are signed ad hoc and carry the checkout's debug `amux`.

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

## Not yet

- A distribution build: Developer ID signing of the app and of the `amux` it
  carries, notarization and a download. The release signs and notarizes the
  `amux` binary itself (docs/RELEASE.md).
- The app's real screens, its copy in the string catalogue, and tests.
