# Browser extension

*For developers changing the browser extension, the page-review overlay, or the
parts of amux they reach: the bridge, its registration and the page review
attachment.*

The extension lets a person comment on any web page, the way a diff review
comments on lines, and send the comments to an agent together. It is another
amux client: it talks only to the amux on its own machine, never dials another
host, and keeps nothing but its unsent comments.

The pieces:

- [`js/page-review`](../js/page-review/src/index.ts): the overlay. Hover
  snapping, the three gestures, the comment box and the pins. It knows nothing
  of amux; whoever loads it is its host.
- [`js/extension`](../js/extension/): the Chrome extension. A service worker,
  a content script that hosts the overlay in a tab, a script in the page's own
  world that reads source hints, and the side panel.
- [`crates/browser-core`](../crates/browser-core/src/lib.rs): the client
  crates the extension runs as WebAssembly.
- [`amux internal browser`](../crates/amux/src/browser.rs): the bridge a
  browser starts, and [its registration](../crates/amux/src/browser_host.rs).
- The `page` attachment kind in [`records.proto`](../proto/amux/v1/records.proto),
  formatted for the model by the [`attachments`](../crates/attachments/src/lib.rs)
  crate (see [Attachments](ATTACHMENTS.md)).

## What a person does

Clicking the toolbar button, or pressing ⌘⇧. (Ctrl+Shift+. elsewhere), opens
the side panel and starts review mode in the tab. While it is on, the page does
not see clicks:

- **Click a block** to comment on all of it. Hover outlines the block the
  pointer means: it snaps to a paragraph, heading, list item, button, image,
  card or table cell, never the innermost span. "Wider" in the comment box
  moves to the block around it.
- **Select text** to comment on that phrase within its block.
- **Drag a box** from anywhere that is not text, or with the toolbar's Box
  tool on, to comment on a region. The region is captured as a screenshot.

Holding Shift gives the page its pointer back. Escape closes the comment box,
then leaves review mode. Comments show as numbered pins on the page and as a
list in the side panel, which sends them: an agent picker (agents grouped by
profile, or a new agent in a folder), a message box and Send. The right-click
menu also sends a selection, a link or a page's address as text.

The picker's default: a `file://` page inside an agent's folder goes to that
agent, whichever profile it is in; otherwise the agent last chosen on that
site.

## The attachment

A page review is an attachment of its own kind (`PageReview`, arm 5 of
`Attachment.of`): the page's address and title, and its comments. Each comment
is anchored on a block, because every gesture lands on one: a CSS selector, its
visible text cut short, and where the page says it was drawn from, when it
says. A selected phrase adds the quote; a dragged box adds its screenshot as a
blob of the agent. Nothing says what the comments are for: the words sent with
them do, as with a diff review.

The screenshots are stored with the agent (`PutBlob`) when Send is pressed,
since a blob belongs to an agent and the agent is chosen then. A new agent is
created first, with no prompt, so its screenshots can be stored before the
review is sent to it; a retry after a failed send goes to that agent rather
than making another. Comments added while a send is under way stay for the
next one.

The model reads the review as an `<amux-attachment kind="page">` element
([Attachments](ATTACHMENTS.md#the-element)), and each screenshot as a native
image after it, for the providers that take one.

**Source hints.** React's development builds, Vue and Svelte keep which file
drew an element where only the page's own scripts can read it, so
[`src/page`](../js/extension/src/page/index.ts) runs in the page's world and
answers the content script's question by an event. The attributes development
tools add (`data-inspector-relative-path`, `data-source` and the like) are read
directly. A page that says nothing gets an empty source; the selector and text
remain.

## How it connects

Chrome starts a native messaging host by a manifest file and exchanges JSON
with it over stdio, each message a 4-byte length in native byte order. The
host is `amux internal browser`, a byte forwarder: the extension names a target
(`installation` for the front door, or a profile's id in hex), a gRPC method
and the encoded request; the bridge makes the call on that socket and streams
the response messages back. It decodes nothing and has no per-method code. A
browser refuses a message over 1 MB to an extension, so a response message
larger than 512 KiB goes in parts the extension joins. Every call carries the
surface header `browser`, which the daemon counts in its analytics.

**The bridge is a promise.** An installed host manifest runs it long after the
build that wrote the manifest, for an extension at another version: its name,
its arguments (a browser passes the extension's origin, and on Windows a
window handle) and its framing only ever add. The command-line test in
`crates/amux/src/main.rs` parses it. Everything the extension and amux say
inside the framing is `amux.v1`, which only adds too.

**Registration.** The supervisor writes, when it starts, a host manifest into
the `NativeMessagingHosts` folder of each Chromium browser installed (Chrome,
its Beta and Canary, Chromium, Edge, Brave, Arc, Vivaldi), naming a small
script beside the installed binary that runs `amux internal browser`. It does
so only when it is the installed `~/.amux/bin/amux`, so a test or a
development tree never points a person's browsers at itself; it rewrites a
file only when its bytes would change. `amux uninstall` removes the manifests
that name this installation's script. A manifest allows only the extension's
ID, which its public key (`key` in the manifest) fixes, in the store and
unpacked alike.

**Profiles** work as in the terminal and the Mac app: the profiles from the
front door, each profile's fleet from its own inventory stream, and the
picker grouped by profile. The fleets are kept only while a side panel is
open; with none open the streams end, and the native port closes once no call
is open, since an open port keeps both the service worker and the bridge
process running.

**Permissions.** The extension asks for no access to sites. Its button, its
shortcut and its menu grant it the current tab at that moment (`activeTab`),
and only then does it inject the overlay. A click inside the side panel grants
nothing, so the panel's "Review this page" works only on a page already
granted; elsewhere the panel says to use the button or the shortcut. After a
reload, pins come back at the next click. Local files need the browser's "Allow access to file URLs"
switch for the extension, which it cannot turn on itself; the panel says so
and opens the extension's settings.

**State.** A tab's comments may span pages, as a person follows links while
reviewing: each comment keeps the page it was made on, the page shows only its
own as pins, and Send sends one page review per page. Each tab's unsent
comments live in the browser's session storage, and their screenshots in
IndexedDB, because the service worker is stopped whenever it is idle and
session storage has too little room for images. Both go when the tab closes or
the review is sent.

## The overlay and its hosts

The overlay is one bundle with a small interface ([`types.ts`](../js/page-review/src/types.ts)):
a host starts and stops it and hands it the comments to draw as pins; it tells
its host when a comment is added, edited or deleted, asks it to capture a
region before a box's comment opens, and asks where an element was drawn from.
Each host ships the overlay inside its own build, so the interface is never a
promise between versions. The host owns everything outside the page: the
screenshot, where pending comments are kept, and where they go.

The Chrome extension is one host. The Mac and iPhone apps are meant to be
others, hosting the same overlay in their own web views (`WKWebView` cannot
load a Chrome extension, and needs none: an app owns its view, injects the
overlay as a user script and hears it through a message handler), with the
review going into the chat's composer as a diff review does. `npm run build`
in `js/page-review` builds the one file such a host injects. Gestures are
built on pointer events, so a touch mode can be added without a rewrite.

## WebAssembly is optional

The extension compiles the pure client crates (`wire`, `ui-state`, `ui-view`,
`attachments`, `preview`) to WebAssembly because they happen to be pure, so the
fleet model, the rows the picker draws and every `amux.v1` encoding are the
same code the other clients run, and ship with the extension at its own
version. Staying compatible with WebAssembly is **not** a constraint on those
crates. If a change to a shared crate breaks the WebAssembly build, the change
wins, and the extension does in TypeScript what it lost: decoding the fleet for
the picker and encoding its few requests. Glue for WebAssembly lives only in
`browser-core`, whose one export takes and answers JSON; shared crates take no
WebAssembly-specific shims or `cfg`s. The WebAssembly build runs in the
browser's own recipes and CI job, never in `just premerge`.

## Building and testing

```sh
just browser              # browser-core as WebAssembly, then js/extension/dist
just browser --dev        # js/extension/dist-dev, the copy a development tree loads
just browser-test         # types, unit tests, and the end-to-end test
just browser-dev-host     # let dist-dev reach this tree's amux
```

To try a change, run `just browser --dev`, load `js/extension/dist-dev`
unpacked from `chrome://extensions`, and run `just browser-dev-host` with
`AMUX_CONFIG` naming this tree's installation (`wt exec <tree> --` sets it).
The development copy is named "amux (dev)" and starts the
`sh.amux.browser.dev` host, which runs this tree's `target/debug/amux`, so it
never reaches an installed amux; the installed extension's host is left alone.
`just browser-dev-host remove` takes it out.

The end-to-end test ([`review.spec.ts`](../js/extension/test/e2e/review.spec.ts))
starts a daemon with the fake providers, loads the extension in Playwright's
Chromium with a host manifest in a temporary profile (Chromium reads native
messaging hosts from its user data folder), comments on a heading, a phrase and
a box, sends them from the side panel, and reads what the fake provider was
given. Playwright cannot press a toolbar button, which is what grants the tab,
so the copy under test is given access to every site instead; the side panel
is opened as a page with `?tab=` naming the tab it shows. `AMUX_E2E_KEEP=1`
keeps screenshots of the overlay, the comment box and the panel, and what the
provider read, in `js/test-results`.

The JavaScript is one npm workspace under `js/`, as the crates are one Cargo
workspace: one lockfile, one base `tsconfig`, Vitest and Playwright. It needs
Node 24 or later, which runs the TypeScript build scripts directly.

## Rejected alternatives

- **A localhost WebSocket on the daemon** instead of native messaging: an open
  port and an authentication scheme to get right, for nothing native messaging
  lacks.
- **A JSON API of the extension's own between it and the bridge**: the
  extension updates from the store on its own schedule, so that API would be a
  second protocol across versions without `amux.v1`'s rules or checks.
- **Running the Rust client runtime inside the bridge**, as the phone runs it:
  the view values would then cross versions between the extension and the
  installed amux, making them a promise they deliberately are not.
- **Image and text attachments** instead of a kind of its own: clients would
  draw loose images and pasted text instead of a review, and each comment's
  pairing with its anchor would survive only as prose.
- **Extending the `review` kind**: it is built around a diff, and an older
  build would take a page review for a review and format it broken instead of
  refusing it.
- **Separate anchor kinds** for a phrase, a block and a box: every gesture
  lands on a block, so one element with an optional quote and image covers all
  three.
- **A screenshot on every comment**: most comments need a precise anchor, not a
  picture; the box gesture is the screenshot when one matters.
- **Access to every site**: an install-time warning and a slower store review,
  to save one click after a reload.
- **Registering the host with the login item**: a person who does not start
  amux at login would have no extension; and any supervisor writing it would
  let a test's or a development tree's point the browsers at itself.

## Later

- Lending a tab to an agent: the agent's tools screenshot, read and click in
  one tab a person hands it, until the tab closes. Never browser-wide: the
  browser holds the person's sessions to everything, a different trust
  boundary from paired hosts.
- Reloading or badging a tab when its agent finishes a turn.
- The Mac app's browser pane and the phone's document view, hosting the same
  overlay.
- Firefox, Safari and Windows (whose hosts are registered in the registry).
