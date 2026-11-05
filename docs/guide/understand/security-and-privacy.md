---
title: Security and privacy
description: Why it's safe to let your devices into your machines. Pairing as the only way in, what a paired machine can do, what the relay can and can't see, and how to take trust back.
sidebar:
  order: 2
---

amux opens a door into your machines: another device can start agents there,
and agents run commands as you. This page explains who can use that door,
what the servers in the middle can see, and how to close it again.

In short:

- **Only machines you paired, in person, get in.** No account, server or
  token stands in for pairing.
- **Your chats stay on your machines.** Traffic between your devices is
  encrypted end to end, including through the relay.
- **You can take trust back at once,** on your own machine, without asking
  anyone.

## Pairing is the only way in

Until two hosts are paired, neither can see or touch the other's agents,
whatever account they share. Pairing happens once, with both machines in
front of you. One shows a six-digit PIN or a QR code, and you enter or scan
it on the other. Over SSH there's no PIN: your SSH login to the other
machine is the proof.

The PIN never crosses the network. The two hosts use it in a
password-authenticated key exchange that proves each is talking to the
machine in front of you, and someone listening learns nothing they can use.
A PIN works once and expires when the pairing window closes: five minutes
after `amux pair` opens one, or ten for the window setting up shows, which
then opens a new one with a new PIN. Five wrong guesses close the window
early: after that even the right PIN is refused, and pairing needs a new
window. No pairing window survives amux restarting.

What pairing produces is small and stays on your machines: each host
remembers the other's public key. From then on, every connection between
them starts with both sides proving they hold the key the other remembers.
Nothing else grants that trust: not an account, and not a server.

## What a paired machine can do

A paired machine can do anything with your agents that you could through
amux. It can open chats, send prompts, and start, stop and delete agents in
any folder, with any permission settings. That means it can run commands as
you, so **pair only machines you control.**

amux itself gives a paired machine no way to change who your machine
trusts, pair on your behalf or stop your amux. An agent it starts can still
run any command you could.

Two more things to keep in mind:

- Agents load a folder's own Claude settings and hooks, so start agents only
  in folders whose code you'd be happy to run.
- Profiles keep amux's own state apart, but they don't sandbox your agents.
  Every agent runs as your user, with your user's access to the machine.

## What the relay can and can't see

When two of your hosts can't reach each other directly, the amux relay
copies their encrypted traffic between them. A host connects to the relay
while its profile is signed in.

The relay sees the encrypted traffic, and the details it needs to route it:
host names and ids, which account each host belongs to, when hosts come and
go, and how much traffic passes and when. It can't:

- **read your chats.** The traffic is encrypted end to end between your
  hosts.
- **pretend to be one of your hosts.** It holds no key that any of your hosts
  trusts, so it fails the check that starts every connection.
- **start agents or run commands.** Those need a connection that ends inside
  a host you paired, which the relay can't form.

A relay that misbehaved could drop or delay your traffic. It would gain no
power to read your chats or operate your agents.

## Signing in a paired device

When you pair your phone with a signed-in computer, the computer can offer
the phone its account, so the phone doesn't have to sign in separately. This
is a convenience, and it's built so it can't become a way in:

- The offer travels only between devices that are already paired, over their
  encrypted link.
- The signed-in side has to choose to offer it, and the other side sees the
  account's email and the offering machine's name before accepting.
- What it hands over is the account, meaning a route through the relay. It
  never hands over trust in another machine.
- The sign-in ticket behind it works for that one device, once, for a minute,
  so a copy is worth nothing anywhere else.

## Taking trust back

**Unpairing** takes effect at once and needs no one's permission:

```sh
amux unpair <host>
```

Your machine forgets the other host's key. Every connection with it closes at
once, later attempts fail, and its agents leave your fleet. On the host you
unpaired, the other machine shows as offline, marked **no longer trusts this
machine**, and its agents are out of reach there until the two pair again.
Each profile keeps its own pairings, so unpairing in one leaves the others as
they were.

**Signing out** doesn't untrust anything. It only takes the relay away. Hosts
this machine reaches directly or over SSH work as before. Hosts that only the
relay reached are out of reach until you sign in again, and the fleet says
**this machine is signed out**. A chat on one of those hosts keeps your draft
until then.

To sign out every device at once, see
[Your account](https://amux.sh/docs/account/your-account/#signing-out-your-devices).

## Why this is trustworthy

- **Trust lives on your machines.** Each profile's private key never leaves
  its machine, and the list of hosts it trusts is a local file that no server
  holds or syncs.
- **One rule decides access.** Every connection starts with both hosts
  proving their keys to each other. The route underneath, whether direct,
  SSH or the relay, never grants anything.
- **Some things are missing on purpose.** There's no trust passed along from
  a friend of a friend, no central authority that can vouch for a host, and no
  token that stands in for pairing. Each of those absences rules out a whole
  kind of attack.
- **Taking trust back is local.** Unpairing takes effect on your machine the
  moment you do it.

## What leaves your machines

Your prompts, your agents' output, your code and your file names stay on your
machines, or travel encrypted between your own devices. What does reach
amux.sh:

- **Your account,** if you have one: your email, how you sign in, and your
  plan. See [Your account](https://amux.sh/docs/account/your-account/).
- **Usage counts,** which you can turn off. See
  [What amux sends](what-amux-sends.md).
- **Problem reports,** only when you send one, or automatically if you turned
  that on. One you send includes the screen you froze, chat text and all. See [What amux sends](what-amux-sends.md#problem-reports).

For how the connection itself works, see the [protocol](../../PROTOCOL.md).
