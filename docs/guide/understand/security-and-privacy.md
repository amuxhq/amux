---
title: Security and privacy
description: Why it's safe to let your devices into your computers. Pairing as the only way in, what a paired computer can do, what the relay can and can't see, and how to take trust back.
sidebar:
  order: 2
---

amux opens a door into your computers: another device can start agents there,
and agents run commands as you. This page explains who can use that door,
what the servers in the middle can see, and how to close it again.

In short:

- **Only computers you paired, in person, get in.** No account, server or
  token stands in for pairing.
- **Your chats stay on your computers.** Traffic between your devices is
  encrypted end to end, including through the relay.
- **You can take trust back at once,** on your own computer, without asking
  anyone.

## Pairing is the only way in

Until two computers are paired, neither can see or touch the other's agents,
whatever account they share. Pairing happens once, with both computers in
front of you. One shows a six-digit PIN or a QR code, and you enter or scan
it on the other. Over SSH there's no PIN: your SSH login to the other
computer is the proof.

The PIN never crosses the network. The two computers use it in a
password-authenticated key exchange that proves each is talking to the
computer in front of you, and someone listening learns nothing they can use.
A PIN works once and expires when the pairing window closes: five minutes
after `amux pair` opens one, or ten for the window setting up shows, which
then opens a new one with a new PIN. Five wrong guesses close the window
early: after that even the right PIN is refused, and pairing needs a new
window. No pairing window survives amux restarting.

What pairing produces is small and stays on your computers: each computer
remembers the other's public key. From then on, every connection between
them starts with both sides proving they hold the key the other remembers.
Nothing else grants that trust: not an account, and not a server.

## What a paired computer can do

A paired computer can do anything with your agents that you could through
amux. It can open chats, send prompts, and start, stop and delete agents in
any folder, with any permission settings. That means it can run commands as
you, so **pair only computers you control.**

amux itself gives a paired computer no way to change who your computer
trusts, pair on your behalf or stop your amux. An agent it starts can still
run any command you could.

Two more things to keep in mind:

- Agents load a folder's own Claude settings and hooks, so start agents only
  in folders whose code you'd be happy to run.
- Profiles keep amux's own state apart, but they don't sandbox your agents.
  Every agent runs as your user, with your user's access to the computer.

## What the relay can and can't see

When two of your computers can't reach each other directly, the amux relay
copies their encrypted traffic between them. A computer connects to the relay
while its profile is signed in.

The relay sees the encrypted traffic, and the details it needs to route it:
computer names and ids, which account each computer belongs to, when computers come and
go, and how much traffic passes and when. It can't:

- **read your chats.** The traffic is encrypted end to end between your
  computers.
- **pretend to be one of your computers.** It holds no key that any of your computers
  trusts, so it fails the check that starts every connection.
- **start agents or run commands.** Those need a connection that ends inside
  a computer you paired, which the relay can't form.

A relay that misbehaved could drop or delay your traffic. It would gain no
power to read your chats or operate your agents.

## Signing in a paired device

When you pair your phone with a signed-in computer, the computer can offer
the phone its account, so the phone doesn't have to sign in separately. This
is a convenience, and it's built so it can't become a way in:

- The offer travels only between devices that are already paired, over their
  encrypted link.
- The signed-in side has to choose to offer it, and the other side sees the
  account's email and the offering computer's name before accepting.
- What it hands over is the account, meaning a route through the relay. It
  never hands over trust in another computer.
- The sign-in ticket behind it works for that one device, once, for a minute,
  so a copy is worth nothing anywhere else.

## Taking trust back

**Unpairing** takes effect at once and needs no one's permission:

```sh
amux unpair <host>
```

Your computer forgets the other computer's key. Every connection with it closes at
once, later attempts fail, and its agents leave your fleet. On the computer you
unpaired, the other computer shows as offline, marked **unpaired this
computer**, and its agents are out of reach there until the two pair again.
Each profile keeps its own pairings, so unpairing in one leaves the others as
they were.

**Signing out** doesn't untrust anything. It only takes the relay away. Computers
this computer reaches directly or over SSH work as before. Computers that only the
relay reached are out of reach until you sign in again, and the fleet says
**this computer is signed out**. A chat on one of those computers keeps your draft
until then.

To sign out every device at once, see
[Your account](https://amux.sh/docs/account/your-account/#signing-out-your-devices).

## Why this is trustworthy

- **Trust lives on your computers.** Each profile's private key never leaves
  its computer, and the list of computers it trusts is a local file that no server
  holds or syncs.
- **One rule decides access.** Every connection starts with both computers
  proving their keys to each other. The route underneath, whether direct,
  SSH or the relay, never grants anything.
- **Some things are missing on purpose.** There's no trust passed along from
  a friend of a friend, no central authority that can vouch for a computer, and no
  token that stands in for pairing. Each of those absences rules out a whole
  kind of attack.
- **Taking trust back is local.** Unpairing takes effect on your computer the
  moment you do it.

## What leaves your computers

Your prompts, your agents' output, your code and your file names stay on your
computers, or travel encrypted between your own devices. What does reach
amux.sh:

- **Your account,** if you have one: your email, how you sign in, and your
  plan. See [Your account](https://amux.sh/docs/account/your-account/).
- **Usage counts,** which you can turn off. See
  [What amux sends](what-amux-sends.md).
- **Problem reports,** only when you send one, or automatically if you turned
  that on. One you send includes the screen you froze, chat text and all. See [What amux sends](what-amux-sends.md#problem-reports).

## The browser extension

The amux extension for Chrome and browsers built on it
([Browser extension](../use/browser.md)) reads a page only when
you ask it to: when you click its button, press its shortcut or use its menu
on that page. It has no access to any other site.

When you comment on a page, it keeps the address and title of the page, the
text of what you comment on, the phrase you select and a picture of any box
you drag, with your comments, in your browser until you send them or close the
tab. It also remembers which agent you last sent to from each site.

It sends nothing anywhere until you press Send, and then only to amux on the
same computer, which delivers it to the agent you chose, the same way a
message you type does. That agent's AI provider (Anthropic for Claude, OpenAI
for Codex) receives it as part of your prompt. The extension makes no network
requests of its own, sends nothing to amux.sh, and doesn't sell or share what
it handles with anyone.

For how the connection itself works, see the [protocol](../../PROTOCOL.md).
