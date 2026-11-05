---
title: Install
description: Install amux on your computer, then let its first run set it up and pair your phone.
sidebar:
  order: 1
---

amux runs your Claude Code and Codex agents on your own computers, keeps them
running, and lets you follow and answer them from a terminal or your iPhone.
Install it on every computer you want agents to work on.

## Before you start

- A Mac with Apple silicon, Linux on x86-64, or Windows on x86-64.
- Claude Code or Codex, installed and signed in on that computer. amux runs
  the agents you already use, on your own subscription to them.

You don't need an amux account. Everything on this page works without one.

## Install amux

**On a Mac or Linux, from a terminal:**

```sh
curl -fsSL https://amux.sh/install | sh
```

**On Windows, in PowerShell:**

```powershell
irm https://amux.sh/install | iex
```

The script downloads the current release, checks it against the release's
signed checksum, installs it as `~/.amux/bin/amux`, and adds that folder to
your `PATH`. Open a new terminal afterwards, or run the line it prints, so
your shell finds `amux`.

## Set it up

Run:

```sh
amux
```

The first time, amux starts itself in the background and walks you through
setting up, one screen at a time. Every screen can wait: **Esc** skips it for
now, and whatever you skip stays on the home screen as **Finish setting up**
until you do it or dismiss it.

1. **Connect your iPhone.** A QR code and a six-digit code that pair your
   phone with this computer. Scan the QR code with your iPhone's camera, or
   enter the code in the amux app. If you don't have the app yet, the QR
   code takes you to the App Store. See
   [Connect your phone](connect-your-phone.md).
2. **Keep it connected.** **Start amux when you log in**, and keep the
   computer awake while it's plugged in (on battery it stays awake only
   while an agent works). Both are ticked; **Space** changes one and
   **Enter** applies them. This is what lets your phone reach the computer
   whenever you need it.
3. **Reach it from anywhere.** Signing in to an amux account, so your phone
   reaches this computer when you're away from this network. This uses the
   amux relay, which is part of [Pro](https://amux.sh/docs/account/plans-and-billing/).
   Skip it if you only use amux at home.
4. **Repositories.** The folders Claude Code and Codex worked in lately, ticked.
   New agents offer these first. **+ Add a folder…** adds another.
5. **Help improve amux.** Whether to send a report automatically when
   something goes wrong (off unless you turn it on), and a line saying amux
   sends usage counts, which you can turn off in Settings. Neither ever
   includes your chats, code or file names. See
   [What amux sends](../understand/what-amux-sends.md).
6. **Try starting an agent from your phone.** Tap **+** in the app and pick a
   repository. When the agent starts on this computer, amux opens on it.

That's it. Next, [start your first agent](first-agent.md).

## Leaving and coming back

Press **q** to leave the terminal app. Your agents keep running, and so does
amux, in the background. Run `amux` again at any time to pick up where you
left off.

To change any of the setup choices later, press **,** on the home screen for
Settings. `amux status` shows whether amux is
running, which version, whether it starts at login, and which account each
profile is signed in to.

amux keeps itself up to date. See [Updates and channels](../use/updates.md).

## Uninstalling

```sh
amux uninstall
```

This stops the agents running on this computer (it asks first), removes the
login item, stops amux, and deletes `~/.amux/bin/amux` and the `PATH` lines
the install script added. Your agents' history stays, in case you install
again; add `--delete-data` to remove that too.
