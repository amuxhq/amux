---
title: iPhone app
description: Start agents, answer them, review their work and keep an eye on every machine from your iPhone.
sidebar:
  order: 2
---

The amux iPhone app is your fleet in your pocket. It shows every agent on every
machine you've paired. You can start new agents from it, answer their
questions, review what they changed and keep the conversation going. Your
agents run on your computers, never on the phone, so they keep working when
you put it away.

The app is free on the [App Store](https://apps.apple.com/us/app/amux/id6760197635)
and needs iOS 26. You don't need an account to use it.

To pair it with a computer, see
[Connect your phone](../get-started/connect-your-phone.md).

## Agents

The Agents tab lists your agents: the ones you pinned, then the
ones that need you, then the rest, with **Settled** at the bottom for the ones
you're done with. Each agent goes by the name you gave it or, if you left the
optional Name field empty when you started it, the title Claude or Codex gives
the conversation. The line under it says what the agent is doing or what it's
waiting for. A settled agent keeps the last thing it said, unless it failed,
when the line says why. Pull down for search, by name, title, folder or host.

Swipe a row right to pin it, or left to settle it: it stops if it's running,
and asks first if that would cut work short. A settled agent can be un-settled
or deleted the same way, and sending it a message starts it again.

Tap **+** to start an agent. Pick the host and the folder (your repositories
come first, then folders your agents used lately, or type a path), then the
agent, its model, effort and permission. Turn on **New worktree** to give the
agent a worktree of its own, made from the folder's repository, so it can work
without touching your checkout. The host says which agents are signed in there,
and the app tells you before you start one that isn't.

## Chats

Tap an agent to open its chat.

- **Sending:** type a message, or tap the microphone to dictate one, and send
  it. While the agent is working, your message waits in a queue and goes in
  when the turn ends. You can send a queued message right away instead.
- **Interrupting:** while the agent works, the send button becomes a stop
  button that interrupts the turn. The agent stays ready for your next
  message.
- **Attachments:** tap **+** beside the message box to add a photo or a file.
  Images in the chat show a preview, and tapping an image or file opens it.
- **Agent settings:** the model chip under the message box changes the
  agent's model, effort, permission or mode, wherever that agent allows it.
- **Reading back:** scroll up and an arrow above the message box takes you back
  to the newest message. It says **New activity** once something has arrived.
- **The overview:** shows the agent's tasks, its background jobs and the
  files it changed.

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

## Hosts

The Hosts tab lists your paired machines, and says how the phone reaches each
one:

- **On this network** or **direct**: the phone and the machine are on the same
  network.
- **Through the relay**: they're apart, and your account's relay connects
  them. The relay is part of [Pro](https://amux.sh/docs/account/plans-and-billing/).
- **Away**: the machine is only reachable through the relay, and your account
  is on the free plan.
- **Offline**: nothing reaches the machine right now. Its agents stay in your
  list as they last were, and catch up when it's back.

Machines found on your network that you haven't paired yet appear as cards
above the list. Tap a host to see its details, including the fingerprint of
its key. **Revoke This Host** stops the phone reaching it until you pair again.

The first time the app looks for computers, iOS asks for permission to use
your local network. If you said no, turn it back on in the iPhone's
**Settings** app, under **Privacy & Security** → **Local Network**.

## Accounts

Tap the account picture at the top left of the Agents list to switch account,
add another account, or open Settings. Each account keeps its own paired
machines and agents. **Remove from This Phone** takes an account off the phone
and leaves it on amux.sh. Adding it back means pairing its machines again.

If one of your computers is already signed in, the phone offers to sign in as
the same account when you pair, so there's no need to go through the browser
again. [Your account](https://amux.sh/docs/account/your-account/) covers
signing in and the rest.

## Notifications

With [Pro](https://amux.sh/docs/account/plans-and-billing/), the app notifies
you when an agent needs you, wherever you are and whether or not the app is
open. Turn them on from the card on the agent list or from **Notifications** in
**Settings**. Your phone and your computer have to be signed in to the same
account.

A notification names the agent and says what it needs:

- It wants to do something: "Wants to run deploy --prod", or to edit a file or
  use a tool, and how many more requests are waiting.
- It has a question, or a plan for you to decide.
- It can't go on: it reached a usage limit, with the time it resets, it needs
  signing in on its host, or its turn failed.
- It stopped unexpectedly.

Tap one to open the chat. You won't get one for the chat you're reading.

Your computer encrypts each notification for your phone before it leaves, so
neither amux.sh nor Apple can read it. If your phone can't decrypt one, as just
after a restart until you first unlock it, it says only "An agent needs you",
and a tap opens your agents.

Signing out, or the account leaving Pro, stops that account's notifications. To
turn them all off, use the phone's own **Settings**.

## Settings and help

Settings holds your accounts and subscription, this phone's name and
fingerprint, **Share Usage** and **Send Automatic Reports**
([What amux sends](../understand/what-amux-sends.md)), and **Help**.

If something looks wrong, take a screenshot. The app offers to report it: you
can draw boxes around anything that's wrong, add a note, and send it. Help also
has **Report a Problem** and **Export Diagnostics**, which saves the same report
to share yourself. Reports go to your amux account, so you need to be signed in
to send one. See [Troubleshooting](troubleshooting.md).
