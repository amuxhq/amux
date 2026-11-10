---
title: How amux works
description: The model behind amux, in one page. Which devices do what, how accounts, profiles and pairing fit together, how your machines reach each other, and why your agents survive amux restarting.
sidebar:
  order: 1
---

amux runs AI coding agents on your computers and lets every device you own
reach them. An agent keeps working on the machine where you started it. Your
laptop, another desktop or your phone can open its chat, answer its
questions, send it the next prompt, or start new agents there.

## The pieces

| Word | What it is |
|---|---|
| **Host** | A machine that runs amux: a desktop, a server, and your phone too. |
| **Agent** | One coding agent, Claude Code or Codex. Each runs in its own process on one host, in the project folder you chose. |
| **Chat** | An agent's conversation, drawn the same way on every device. |
| **Fleet** | Every agent you can see, on this host and on the hosts you've paired with, grouped by host, with any subagents an agent started beneath it. |
| **Profile** | One identity for amux on a machine, with its own key, its own paired hosts and its own agents. |
| **Pairing** | The one-time step that makes two hosts trust each other. |
| **Relay** | A server that carries encrypted traffic between your hosts when they can't reach each other directly. |

Every device is an equal client. The terminal app and the iPhone app show
the same fleet and the same chats, and can do the same things. None of them is a remote control for another, and you can
[build your own](../build/build-a-client.md) that works the same way.

## amux on your computer

On a computer, amux runs in the background. That background process, the
daemon, is what runs your agents, talks to your other devices and serves
every app on the machine. You rarely start it yourself: opening `amux` starts
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
**stopped · while the daemon was away**, and sending it a message starts it
again where it left off, with that message as its next prompt. An agent that
was waiting on a question for you when amux disappeared asks the question
again when you resume it.

Short of that, an agent never stops on its own. The Stop button in a chat
interrupts the current turn and leaves the agent ready for the next prompt.
Only settling or deleting the agent, or the agent's own program exiting, ends
the process.

## Accounts and profiles

You can use amux with no account at all. Hosts on the same network find each
other, pair and work together without any server involved. Hosts you can
reach over SSH work the same way.

An [amux.sh account](https://amux.sh/docs/account/your-account/) adds the
relay, so your hosts can reach each other when they're apart:

- **Signed in, on the free plan:** your hosts can see whether each other is
  up, but the relay won't carry agent traffic or pairing between them. A host
  that only the relay can reach shows as away.
- **With [Pro](https://amux.sh/docs/account/plans-and-billing/):** the relay
  carries everything, so your phone reaches your desktop from anywhere.

Connections on your own network and over SSH never involve the account.

A **profile** is a complete, separate amux on one machine, with its own
private key, its own paired hosts, and its own agents and history. Most
people have one. If you keep a Personal and a Work account on the same Mac,
each gets its own profile, and the two share nothing but the machine.
Pairing belongs to a profile, so trusting a machine in Personal grants it
nothing in Work. Every profile stays connected while amux runs, whichever one
you're looking at.

Once one of your devices is signed in, it can sign in the devices it's paired
with. When you pair your phone with a signed-in computer, the computer offers
the phone its account, and the phone signs in with one tap. It works the
other way round too.

## Pairing

Pairing is how one host comes to trust another. Until two hosts are paired,
neither can see or touch the other's agents, whatever account they share.

You pair once, in person: one machine shows a six-digit PIN or a QR code, and
you enter or scan it on the other. Over SSH, your SSH login is the proof and
there's no PIN. From then on the two machines recognise each other by the
keys they exchanged, on any network.

- To pair your phone, see [Connect your phone](../get-started/connect-your-phone.md).
- To pair another computer, see [Add another computer](../get-started/add-another-computer.md).
- For why pairing is safe and what a paired machine can do, see
  [Security and privacy](security-and-privacy.md).

## How your hosts reach each other

Hosts on the same network find each other by themselves. Finding a host
doesn't trust it: a host you haven't paired with shows up only as something
you can pair with.

Paired hosts connect whichever way works, preferring the most direct:

| Route | When |
|---|---|
| **direct** | Both hosts are on the same network, or can reach each other's address, as on a Tailscale network. |
| **ssh** | You paired over SSH. The connection runs through `ssh` to the other machine. |
| **relay** | The hosts are apart, and both are signed in to the same account with Pro. |

The fleet shows each host's route beside it, or **offline** when nothing
reaches it. An agent on a host you can't reach stays in your fleet as it last
was. Its chat shows what you last saw, and fills in again when the host comes
back.

Whatever the route, the connection is encrypted between the two hosts and
checked against the keys they exchanged when they paired. The route only
carries the traffic. It never decides who is trusted.

## Going deeper

- [Security and privacy](security-and-privacy.md): what pairing protects,
  what the relay can see, and how to take trust back.
- [What amux sends](what-amux-sends.md): the usage counts amux sends, and how
  to turn them off.
- The [architecture](../../ARCHITECTURE.md) and the
  [protocol](../../PROTOCOL.md), for how it's built inside.
