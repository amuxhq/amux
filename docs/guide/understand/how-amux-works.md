---
title: How amux works
description: The model behind amux, in one page. Which devices do what, how accounts, profiles and pairing fit together, how your devices reach each other, and why your agents survive amux restarting.
sidebar:
  order: 1
---

amux runs AI coding agents on your devices and lets everything you own
reach them: your other devices, your phone, a terminal. An agent keeps
working on the device where you started it. Your
laptop, another desktop or your phone can open its chat, answer its
questions, send it the next prompt, or start new agents there.

## The pieces

| Word | What it is |
|---|---|
| **Device** | Where your agents run: a desktop, a laptop or a server with amux on it. Your phone pairs with your devices to reach them. |
| **Agent** | One coding agent, Claude Code or Codex. Each runs in its own process on one device, in the project folder you chose. |
| **Chat** | An agent's conversation, drawn the same way in every app. |
| **Fleet** | Every agent you can see, on this device and on the devices you've paired with, grouped by device, with any subagents an agent started beneath it. |
| **Profile** | One identity for amux on a device, with its own key, its own paired devices and its own agents. |
| **Pairing** | The one-time step that makes two devices, or a phone and a device, trust each other. |
| **Relay** | amux's server that carries encrypted traffic between your devices when they can't reach each other directly. |

Every app is an equal client. The terminal app and the iPhone app show
the same fleet and the same chats, and can do the same things. None of them is a remote control for another, and you can
[build your own](../build/build-a-client.md) that works the same way.

## amux on your device

On a device, amux runs in the background. That background process, the
daemon, is what runs your agents, talks to your other devices and serves
every app on the device. You rarely start it yourself: opening `amux` starts
it if it isn't running, and setting up asks whether it should also start
when you log in.

On a desktop, a small supervisor looks after the daemon. It restarts amux if
it ever crashes, and installs new releases as they come out (see
[Updates and channels](../use/updates.md)). On a server, your service
manager can do that job instead (see [Running amux on a server](../use/server.md)).

## Your agents keep running

Each agent is its own process, and it keeps running whether or not the amux
daemon is. The agent writes everything it does to a journal on disk, and the
daemon reads that journal and serves it to your devices.

So when amux restarts, crashes, is stopped or updates itself, nothing happens
to your agents. Each one carries on with its turn. When amux comes back it
finds every running agent, reads what they did while it was gone, and your
devices catch up. A chat you had open shows the agent's work as if amux had
never left.

An agent waits for amux for a while, five minutes by default. If amux
doesn't come back in that time, the agent finishes the turn it's on, saves
it and exits. Nothing is lost. The fleet shows the agent as
**stopped · while amux was stopped**, and sending it a message starts it
again where it left off, with that message as its next prompt. An agent that
was waiting on a question for you when amux disappeared asks the question
again when you resume it.

An agent left idle with a long conversation is compacted before its
provider's prompt cache runs out, so the next message you send carries a
summary rather than the whole conversation read afresh. Claude in a
terminal does this itself; amux does it for headless Claude after about 55
minutes, when the conversation holds 200,000 tokens or more and Claude
keeps its cache for an hour, and for Codex after about 25 minutes, when the
thread holds 100,000 tokens or more. The chat marks it
**Compacted while idle**. Nothing is compacted once the cache has gone,
when it would save nothing, and a message you send meanwhile waits for it
to finish.

Short of that, an agent never stops on its own. The Stop button in a chat
interrupts the current turn and leaves the agent ready for the next prompt.
Only settling or deleting the agent, or the agent's own program exiting, ends
the process.

## Accounts and profiles

You can use amux with no account at all. Devices on the same network find each
other, pair and work together without any server involved. Devices you can
reach over SSH work the same way.

An [amux.sh account](https://amux.sh/docs/account/your-account/) adds the
relay, so your devices can reach each other when they're apart:

- **Signed in, on the free plan:** your devices can see whether each other is
  up, but the relay won't carry agent traffic or pairing between them. A device
  that only the relay can reach shows as away.
- **With [Pro](https://amux.sh/docs/account/plans-and-billing/):** Pro relays
  connections when your devices aren't on the same network, so your phone
  reaches your desktop wherever you are.

Connections on your own network and over SSH never involve the account.

A **profile** is a complete, separate amux on one device, with its own
private key, its own paired devices, and its own agents and history. Most
people have one. If you keep a Personal and a Work account on the same Mac,
each gets its own profile, and the two share nothing but the Mac.
Pairing belongs to a profile, so trusting a device in Personal grants it
nothing in Work. Every profile stays connected while amux runs, whichever one
you're looking at.

Once one of your devices is signed in, it can sign in whatever it's paired
with. When you pair your phone with a signed-in device, the device offers
the phone its account, and the phone signs in with one tap. It works the
other way round too.

## Pairing

Pairing is how one device comes to trust another. Until two devices are paired,
neither can see or touch the other's agents, whatever account they share.

You pair once, in person: one device shows a six-digit PIN or a QR code, and
you enter or scan it on the other. Over SSH, your SSH login is the proof and
there's no PIN. From then on the two recognise each other by the
keys they exchanged, on any network.

- To pair your phone, see [Connect your phone](../get-started/connect-your-phone.md).
- To pair another device, see [Add another device](../get-started/add-another-computer.md).
- For why pairing is safe and what a paired device can do, see
  [Security and privacy](security-and-privacy.md).

## How your devices reach each other

Devices on the same network find each other by themselves. Finding a device
doesn't trust it: a device you haven't paired with shows up only as something
you can pair with.

Paired devices connect whichever way works, preferring the most direct:

| Route | When |
|---|---|
| **Local** | Both are on the same network, and connect with nothing in between. |
| **Direct** | Nothing is in between, across networks: they reach each other without the relay, as on a Tailscale network or over a path amux opens between them. |
| **ssh** | You paired over SSH. The connection runs through `ssh` to the other device. |
| **Relayed** | They're apart, and both are signed in to the same account with Pro. The connection goes through amux's relay. |

The fleet shows each device's route beside it, or **offline** when nothing
reaches it. A device found on your network that you haven't paired yet reads
**Local** too. An agent on a device you can't reach stays in your fleet as it last
was. Its chat shows what you last saw, and fills in again when the device comes
back.

Whatever the route, the connection is encrypted between the two ends and
checked against the keys they exchanged when they paired. The route only
carries the traffic. It never decides who is trusted.

## Going deeper

- [Security and privacy](security-and-privacy.md): what pairing protects,
  what the relay can see, and how to unpair.
- [What amux sends](what-amux-sends.md): the usage counts amux sends, and how
  to turn them off.
- The [architecture](../../ARCHITECTURE.md) and the
  [protocol](../../PROTOCOL.md), for how it's built inside.
