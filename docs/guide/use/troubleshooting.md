---
title: Troubleshooting
description: What to check when amux isn't running, a device is out of reach, or an agent can't carry on, and how to send a report.
sidebar:
  order: 8
---

## Start with amux status

```sh
amux status
```

It answers most questions in a few lines:

- whether amux is running, and which version
- how it updates, and whether a release was rolled back
- whether it starts at login
- each account on this device, and whether the relay is connected
- whether usage counts are on, and where the config file is

## amux isn't running

Any `amux` command starts amux if it's stopped, and so does opening the
terminal app. To start it on its own:

```sh
amux server start
```

If it still doesn't start, the logs say why. They're in
`~/.local/share/amux/`: `daemon.log` for amux itself, and `supervisor.log` for
the small process that starts it and keeps it up to date.

Your agents don't depend on amux running. If amux was away for long enough
that an agent gave up waiting, the agent shows as **stopped** with a note that
amux was away. Send it a message, or run `amux resume <agent>`, and it picks
up where it left off.

## A device is out of reach

Your fleet and the Devices tab on your phone say why a device can't be reached:

| What it says | What to do |
|---|---|
| **away** | The device is only reachable through amux's relay, and your account is on the free plan: relayed connections need Pro. Get on the same network as the device, or [subscribe to Pro](https://amux.sh/docs/account/plans-and-billing/). |
| **offline** | Nothing reaches it right now. Check that the device is on and amux is running there (`amux status`). If it's on another network, both ends need to be signed in to the same account, with Pro. |
| **this device is signed out** | Sign in again with `amux login`, or from Settings in the app. |
| **unpaired this device** | The other device unpaired this one. Pair them again ([Add another device](../get-started/add-another-computer.md)). |

An agent on a device that's out of reach stays in your fleet as it last was.
Its chat fills in again when the device is back.

## The phone doesn't find my device

- Check that the phone and the device are on the same network. Guest
  networks, and some office networks, keep devices from seeing each other.
- Check that the app may use the local network: in the iPhone's **Settings**
  app, go to **Privacy & Security** → **Local Network** and turn on amux.
- Open amux on the device and start pairing there, with `amux pair`. The
  phone offers to pair only with a device that's waiting for it.

[Connect your phone](../get-started/connect-your-phone.md) has the other ways
to pair, including when the phone and device are on different networks.

## An agent can't carry on

The line under an agent's name, and the box where you type to it or the chat
just above it, say what's in the way:

- **Claude or Codex needs you to sign in.** amux can't sign in for you. On the
  device the agent runs on, run `claude` and sign in with `/login`, or run
  `codex login`. Then send the agent a message: you don't need to stop it, and
  sending is how it tries again. The notice goes once Claude or Codex answers.
  Claude can take up to half a minute to notice you signed in, so if the
  first message still fails, send another. Codex keeps the API key or ChatGPT
  account it started with: if you signed in with a different one, run
  `amux settle <agent>` and then `amux resume <agent>`, which starts it again
  on the same conversation.
- **Can't reach Claude.** The request got no answer, usually because the
  device the agent runs on has lost its connection. Claude keeps trying by
  itself, and the chat counts down to the next attempt.
- **A usage limit is reached.** The agent's provider has a limit on how much
  you can use it. The agent's row and its chat say when the limit resets, and
  you can still type your next message in the meantime.
- **The agent can't find a command,** such as `npm` or `cargo`, though it works
  in your own terminal. amux started at login remembers the `PATH` of the shell
  it was turned on from. Turn it off and on again from a shell where the
  command works:

  ```sh
  amux config set start_at_login false
  amux config set start_at_login true
  ```

  The new `PATH` applies from your next login, or straight away after
  `amux server stop` and `amux server start`.
- **The relay refuses this version.** `amux status` says so. Run
  `amux update`.

## A device needs updating

A newer amux can ask an older device for something it doesn't know. Two
messages mean that, and neither loses your words:

| What it says | What to do |
|---|---|
| **that device needs updating to do this** | Update amux on that device. |
| **not sent: one of its attachments is of a kind this device can't read** | Update amux on the agent's device and send it again. Your words are back in the composer. |

[Updates and channels](updates.md) says how.

## An update didn't happen

`amux status` says if the latest release failed to start on this device and
amux went back to the version before. `amux update --retry` tries it again.
[Updates and channels](updates.md) has more.

## Sending a report

If something looks wrong, send a report while it's still on screen. A report
holds the screen you froze, with your marks and note, what amux's own parts
were doing, and its own logs. Apart from what's on that screen, it never
holds your messages, your code, or your file and folder names
([What amux sends](../understand/what-amux-sends.md#problem-reports)).

- **In the terminal app**, press `b` on home, or Ctrl+A then `b` in a chat. The
  screen freezes: drag over anything that looks wrong, add a note, and press
  Enter.
- **On iPhone**, take a screenshot and the app offers to report it, or use
  **Report a Problem** under **Help** in Settings.
- **On Android**, take a screenshot and a banner offers to open **Report a
  Problem** (you can drag boxes to mark what's wrong or tap **Hide** to black
  out sensitive areas before sending), or use **Report a Problem** under
  **Help** in Settings.
- **From the shell**, run `amux dump --reason "what looks wrong" --send`.

Reports go to your amux account, so you need to be signed in to send one. A copy
stays in `~/.local/share/amux/reports/`. Without `--send`, `amux dump` only
saves the report there, so you can look at it, or share it yourself. On the
phone, **Export Diagnostics** does the same.

You can also turn on automatic reports, which amux sends when one of its own
checks finds something wrong. They're off unless you turn them on, in Settings
or with `amux config set auto_reports true`.

## Starting over

`amux uninstall` stops this device's agents, after asking, and removes amux.
Add `--delete-data` to remove its data and settings too, including your
pairings and your agents' history.

If you're stuck, [contact support](https://amux.sh/support).
