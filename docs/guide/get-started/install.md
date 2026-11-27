---
title: Install
description: Install amux on your Mac, Windows PC or Linux machine, then let its first run set it up and pair your phone.
sidebar:
  order: 1
---

amux runs your Claude Code and Codex agents on your own devices, keeps them
running, and lets you follow and answer them from a terminal or your iPhone.
Install it on every device you want agents to work on.

## Before you start

- A Mac with Apple silicon, a PC with Windows 10 or 11, or Linux on x86-64.
- Claude Code or Codex, installed and signed in on that machine. amux runs
  the agents you already use, on your own subscription to them.

You don't need an amux account. Everything on this page works without one.

## Install amux

**On a Mac or Linux, from a terminal:**

```sh
curl -fsSL https://amux.sh/install | sh
```

The script downloads the current release, checks it against the release's
signed checksum, installs it as `~/.amux/bin/amux`, and adds that folder to
your `PATH`. Open a new terminal afterwards, or run the line it prints, so
your shell finds `amux`.

**On Windows, from PowerShell** (no administrator needed):

```powershell
irm https://amux.sh/install | iex
```

The script installs `amux.exe` in `%USERPROFILE%\.amux\bin`, beside
`amux-launch.exe`, the small program that starts amux when you sign in, and
adds that folder to your user `Path`. Open a new terminal afterwards so it
finds `amux`. If amux was already running, the script stops it first, and
tells you to run `amux server start` to start the new version. Windows on
Arm gets the x64 build, which Windows runs for you.

amux isn't signed yet, so Windows may warn about it the first time it runs.
If you see **Windows protected your PC**, choose **More info**, then **Run
anyway**. On a PC with Smart App Control turned on, Windows may block amux
outright until it is signed.

### Let devices nearby reach a Windows PC

Windows' firewall keeps your phone and your other computers on the same
network from reaching amux until it has a rule for it. Add one once:

```powershell
amux setup firewall
```

Windows asks for an administrator's approval, then the rule lasts across
updates. Without it, your PC still reaches your other devices, because it
dials them, but they can't dial it, and your phone doesn't find it nearby.
If Windows asks whether to allow amux on networks when it first starts, you
can allow it, or cancel and run `amux setup firewall` instead.
[Troubleshooting](../use/troubleshooting.md#devices-nearby-cant-reach-a-windows-device)
has more.

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
   phone with this device. Scan the QR code with your iPhone's camera, or
   enter the code in the amux app. If you don't have the app yet, the QR
   code takes you to the App Store. See
   [Connect your phone](connect-your-phone.md).
2. **Keep it connected.** **Start amux when you log in**, and keep the
   device awake while it's plugged in (on battery it stays awake only
   while an agent works). Both are ticked; **Space** changes one and
   **Enter** applies them. This is what lets your phone reach the device
   whenever you need it.
3. **Reach it through the relay.** Signing in to an amux account, so your phone
   reaches this device when you're away from this network. Your phone then
   connects through amux's relay, which is part of
   [Pro](https://amux.sh/docs/account/plans-and-billing/).
   Skip it if you only use amux at home.
4. **Repositories.** The folders Claude Code and Codex worked in lately, ticked.
   New agents offer these first. **+ Add a repository…** adds another.
5. **Help improve amux.** Whether to send a report automatically when
   something goes wrong (off unless you turn it on), and a line saying amux
   sends usage counts, which you can turn off in Settings. Neither ever
   includes your chats, code or file names. See
   [What amux sends](../understand/what-amux-sends.md).
6. **Try starting an agent from your phone.** Tap **+** in the app and pick a
   repository. When the agent starts on this device, amux opens on it.

That's it. Next, [start your first agent](first-agent.md).

## Leaving and coming back

Press **q** to leave the terminal app. Your agents keep running, and so does
amux, in the background. Run `amux` again at any time to pick up where you
left off.

To change any of the setup choices later, press **,** on the home screen for
Settings. `amux status` shows whether amux is
running, which version, whether it starts at login, which account each
profile is signed in to and, on Windows, whether the firewall lets devices
nearby reach it.

amux keeps itself up to date. See [Updates and channels](../use/updates.md).

## Uninstalling

```sh
amux uninstall
```

This stops the agents running on this device (it asks first), removes the
login item, stops amux, deletes what the install put in `~/.amux/bin` (and
the old builds updates kept there), and takes that folder off your `PATH`.
Your agents' history stays, in case you install again; add `--delete-data`
to remove that too.

On Windows a program can't be deleted while it runs, and `amux uninstall`
is one, so it leaves itself behind as `amux.exe.old.1` (or the next free
number) and says so. Delete that file once the command has finished. If amux
had stopped unexpectedly and was about to start again, `amux-launch.exe` may
be left the same way, until it notices amux is gone and exits.
