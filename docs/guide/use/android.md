---
title: Android app
description: Start agents, answer them, review their work and keep an eye on every computer from your Android phone.
sidebar:
  order: 3
---

The amux Android app is your fleet in your pocket. It shows every agent on every
computer you've paired. You can start new agents from it, answer their
questions, review what they changed and keep the conversation going. Your
agents run on your computers, never on the phone, so they keep working when
you put it away.

The app runs on Android phones with Android 14 or newer (portrait orientation).
You don't need an account to use it on your local network.

To pair it with a computer, see
[Connect your phone](../get-started/connect-your-phone.md).

## Pairing and reaching your computers

**On the same Wi-Fi network,** open amux on your phone and run `amux pair` on
your computer (or open the pairing step during setup). Your phone finds the
computer automatically and offers **Pair with laptop?**. Type the six-digit
code, check the computer's name and key fingerprint, and confirm.

**By QR code or link,** run `amux pair --qr` on your computer and scan the code
with your phone's camera. The `https://amux.sh/p` link opens amux ready to
confirm the pairing; the invitation stays in the link's fragment and is never
seen by a web server.

**Away from home,** sign in to your `amux.sh` account so your phone reaches
your computers through the relay (part of
[Pro](https://amux.sh/docs/account/plans-and-billing/)). If a computer you pair
with is already signed in, it can offer its account over your encrypted pairing
link so you can sign in with a tap instead of going through the browser again.
You can also reach paired computers directly over Tailscale without an account
or the relay.

## Agents

The **Agents** tab lists your agents: **Pinned** first, then **Needs you**,
then **Live**. **Settled** holds the agents you're done with. Each agent goes by
the name you gave it or, if you left the Name field empty, the title Claude or
Codex gives the conversation. The line under it says what the agent is doing or
what it's waiting for. Tap the search icon in the top bar to search by name,
title, folder or computer, and use the filter chips (**Needs you**, **Working**,
or a computer name) to narrow the list.

Hold an agent row to **Pin** or **Unpin** it, **Rename** it, **Settle** it (it
stops if it's running, and asks first if that would cut work short),
**Un-settle** it, or **Delete Agent**. Sending a message to a settled agent
starts it again.

Tap **New Agent** at the bottom right to start an agent. Pick the computer and
the folder (your repositories come first, then folders your agents used lately,
then ones you used with Claude Code or Codex, or type a path). Tap the pin
beside a folder to keep it among that computer's repositories, or tap it again
to remove it. Then pick the agent, its model, effort and permission. Turn on
**New worktree** to give the agent a worktree of its own, made from the folder's
repository, so it can work without touching your checkout. The computer says
which agents are signed in there, and the app tells you before you start one
that isn't.

## Chats

Tap an agent to open its chat.

- **Sending:** type a message, or tap the microphone to dictate one, and send
  it. While the agent is working, your message waits in a queue and goes in
  when the turn ends. You can send a queued message right away or withdraw it
  instead.
- **Interrupting:** while the agent works, the send button becomes a stop
  button that interrupts the turn. The agent stays ready for your next
  message.
- **Attachments:** tap **+** beside the message box to add a photo or a file
  (up to 64 MB). Images in the chat show a preview, and tapping an image or
  file opens or shares it.
- **Agent settings:** the model chip under the message box changes the
  agent's model, effort, permission or mode, wherever that agent allows it.
- **Reading back:** scroll up and a button above the message box takes you back
  to the newest message. It says **New activity** once something has arrived.
- **The overview:** tap the chat header to see the agent's tasks, its
  background jobs and the files it changed.
- **Subagents:** when Claude or Codex hands work to helpers of its own, the
  chat folds them into one line, such as **4 subagents** with how many are
  running, waiting on you or done. Tap it to list them, and tap one to open
  its chat. A helper's chat has no message box, since only the agent that
  started it can talk to it, but it has **Stop** while it runs. In the
  Agents list the helpers sit under the agent that started them, by their
  titles. See
  [Agents working together](agents-working-together.md#claudes-and-codexs-own-subagents).

If you lose the connection, the chat says so (for example
`Reconnecting · your draft is kept`) and holds on to what you typed until it
can send it.

## Answering

When an agent needs you, its question appears as a card at the bottom of the
chat. A permission shows the command or the diff, with choices worded as what
will happen, such as **Allow once** or **Always allow**. A plan offers to start
building or to send it back with a note. Questions come one at a time, and you
can always reply in your own words instead.

## Reviewing changes

Open an agent's changes from its chat to read every file it touched. Select a
line, or a range of lines, and comment on it with **Add to Review**. Your
comments go back into the composer as one review, ready to send to the agent.
The count on the chat covers the agent's whole folder; if the folder changed
since the agent last finished a turn, opening its changes brings the count up
to date.

## Computers

The **Computers** tab lists your paired computers and says how your phone
reaches each one:

- **On this network**: your phone and the computer are on the same network.
- **Through the relay** (**over the internet**): they're apart, and your
  account's relay connects them. The relay is part of
  [Pro](https://amux.sh/docs/account/plans-and-billing/).
- **Away**: the computer is only reachable through the relay, and your account
  is on the free plan.
- **Offline**: nothing reaches the computer right now. Its agents stay in your
  list as they last were, and catch up when it's back.

Computers found on your network that you haven't paired yet appear under
**Found** above the list. Tap a computer to see its details, including its
addresses and the fingerprint of its key. **Unpair This Computer** stops your
phone reaching it until you pair again.

## Accounts and subscriptions

Tap your account picture at the top right of the **Agents** list to switch
accounts, add another account, or open **Settings**. Sign-in opens `amux.sh` in
your browser; the app never asks for your password directly. Each account keeps
its own paired computers and agents. **Remove from This Phone** takes an account
off your phone and leaves it on `amux.sh`; adding it back means pairing its
computers again. **Delete Account** permanently deletes your `amux.sh` account
after you confirm your email address.

**Subscriptions:** in-app Google Play subscriptions are not enabled yet in this
release. Subscribe or manage your Pro plan on
[amux.sh](https://amux.sh/docs/account/plans-and-billing/), and your plan
carries over to your Android phone automatically.

## Notifications

Notifications are not yet delivered on Android in this release and are planned
for a future update.

## Settings, problem reports and diagnostics

**Settings** holds your accounts, **Appearance** (**Light**, **Dark** or
**System**), **Notifications**, **Privacy & Diagnostics** (**Share Usage
Counts**, **Automatic Reports**, **Report a Problem**, **Export Diagnostics**
and **Contact Support**), and **This Phone** (your phone's name, key
fingerprint and app version). See
[What amux sends](../understand/what-amux-sends.md).

If something looks wrong, take a screenshot while using amux. A banner offers
to open **Report a Problem** (dismiss it if you just wanted a normal
screenshot), or open **Report a Problem** from **Settings**:

- **Mark or hide areas:** drag boxes on the captured screenshot to point out
  what went wrong, or tap a box and choose **Hide** to black out sensitive text
  or details. Hidden boxes are burned solidly black into the image before it
  leaves your phone.
- **Add a note and send:** write a short note on the report or on any box and
  tap **Send Report**. Reports go to your `amux.sh` account, so you need to be
  signed in to send one.
- **Export Diagnostics:** saves and shares this phone's diagnostic bundle
  directly using Android's share sheet if you prefer to inspect or send it
  yourself. Diagnostic dumps never include your conversations.

See [Troubleshooting](troubleshooting.md) for common fixes.
