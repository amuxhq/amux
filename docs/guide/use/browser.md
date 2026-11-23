---
title: Browser extension
description: Comment on any web page, the way you review a diff, and send the comments to an agent.
sidebar:
  order: 4
---

The amux extension for Chrome lets you point at what you mean on a web page
instead of describing it. Click a heading, select a phrase or drag a box around
part of the page, write what you think, and send all your comments to an agent
together, the way you send a review of its changes.

It works on any page: the app an agent is building on your dev server, a site
it changed, or a design document it wrote. The comments don't assume what you
want done. You say that in the message you send with them, such as "fix
these" or "revise the doc".

The extension works in Chrome and in browsers built on it, such as Edge,
Brave, Arc and Vivaldi. It needs amux on the same device.

## Install

1. Install **amux** from the
   [Chrome Web Store](https://chromewebstore.google.com/detail/omnhphmiepghpgpbfgjebeaajfppkhie).
2. Make sure amux is running on this device. amux connects itself to your
   browsers when it starts, so if you installed the browser after amux,
   restart amux (`amux server stop`, then `amux server start`) or your
   device.
3. Pin the extension to the toolbar so its button is at hand.

## Comment on a page

Click the amux button, or press **⌘⇧.** (**Ctrl+Shift+.** on Windows and
Linux). The side panel opens and the page enters review mode. On a page where
you haven't used the button or the shortcut yet, the side panel can't start
review mode by itself; it asks you to use one of them. Clicks no longer
reach the page while review mode is on. Instead:

- **Click** a block, such as a paragraph, heading, button, image or card, to
  comment on all of it. An outline shows which block you're pointing at. If
  it's too small, **Wider** in the comment box moves to the block around it.
- **Select text** to comment on just that phrase.
- **Drag a box** over an empty area, or turn on **Box** in the toolbar at the
  bottom and drag over anything, to comment on a region. The agent gets a
  picture of the region along with your comment.

Write your comment and press **Add** (or ⌘↩). Each comment becomes a numbered
pin on the page. Click a pin to change or delete its comment.

Hold **Shift** to use the page normally for a moment. Press **Esc**, or click
**Done**, when you've finished.

## Send your comments

Your comments are listed in the side panel. Choose an agent under **To**, add a
message if you like, and press **Send**. You can send to any agent on any of
your devices, or start a new agent in one of your folders. When the page is
a file inside an agent's folder, that agent is chosen for you. Otherwise the
agent you last sent to from that site is.

The agent receives each comment with what it's attached to: the element's
text, the phrase you selected and, where the page's development tools say,
the source file and line that drew it. That way it can find the right code or
the right sentence in its document.

Your comments stay with the tab until you send them or close it. If you follow
a link and comment on the next page too, the agent gets each page's comments
with that page.

You can also right-click a selection, a link or a page and choose **Send to an
amux agent** to send it as text.

## Local files

To comment on a file opened from disk, such as a design document an agent
wrote, the browser needs your permission: open the extension's details in
`chrome://extensions` and turn on **Allow access to file URLs**. The side panel
offers to take you there the first time.

## What the extension can see

The extension can't read any page until you click its button, press its
shortcut or use its menu on that page. It has no access to your sites
otherwise. What you send goes to amux on this device and from there to the
agent you chose, the same way a message you type does. Nothing goes to amux's
servers.

## If it doesn't connect

- **"amux isn't set up for this browser yet"**: amux isn't installed, is older
  than the extension, or hasn't started since you installed this browser.
  Install or update amux, then start it.
- **"amux isn't running"**: start the amux app, or run `amux server start`.
- **The browser's own pages** (`chrome://` pages and the Web Store) can't be
  reviewed.
- **Chromium from a snap or a Flatpak on Linux** (Ubuntu's Chromium is a
  snap): the browser's sandbox keeps it from starting amux. Use a browser
  installed from its maker's package instead.
