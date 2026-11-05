---
title: Add another computer
description: Pair your computers so each one sees and runs the other's agents, on the same network, by address, over SSH, through the relay, or over Tailscale.
sidebar:
  order: 4
---

Pair two computers and each one sees the other's agents, opens their chats,
and starts new agents there. A desktop and a laptop, a home machine and a
server: once they're paired, it's one list of agents across both.

First [install amux](install.md) on the new computer. Then pair the two.
Pairing is a one-time step, and a paired computer can run agents on the
other as you, so pair only machines you control.

Your phone pairs with each computer on its own: pairing two computers
doesn't pair your phone with the new one.

## On the same network

On the computer you want to reach, open pairing:

```sh
amux pair
```

It shows a six-digit code and the command to run on the other computer. On
the other computer, `amux hosts` lists the computers it can see, each with
the command that pairs it:

```text
desk   found nearby · amux pair desk
```

Run that command and type the code. That's it.

## By address

Some networks, such as many office and hotel ones, stop computers finding each
other. Pair by address instead. `amux pair` prints the address to use:

```text
On another computer: run `amux pair desk` (or `amux pair 192.168.1.24:53122` if it is not found nearby) and enter the PIN.
```

Run the address version on the other computer and type the code.

## Over SSH

If you can SSH into the other computer, you can pair through that login, with
no code to type:

```sh
amux pair you@server.example.com
```

amux must already be installed there, and SSH must log in without asking for
a password, for example with a key in your SSH agent. The two computers then
connect through SSH from then on. This is a good fit for a server you already
SSH into; see [Running amux on a server](../use/server.md). A Windows computer
can't be paired this way; pair it by code instead.

## Through the relay

With [Pro](https://amux.sh/docs/account/plans-and-billing/), computers signed
in to the same account reach each other from anywhere. `amux hosts` lists your
account's other computers:

```text
studio   on your account · amux pair studio
```

Run `amux pair studio`, and type the code that `amux pair` shows on the
other computer. Without Pro, `amux hosts` still lists them, and says Pro
pairs them.

A computer that one of your paired computers can see shows up as
`through desk · amux pair studio`. Pair it the same way.

## Tailscale

If your computers are on the same [Tailscale](https://tailscale.com) network,
they can pair and connect directly, wherever they are, with no relay and no
Pro. Tailscale doesn't carry the messages amux uses to find computers nearby,
so pair by address, using the computer's Tailscale address:

1. On the computer you want to reach, run `amux pair`. Note the port at the
   end of the address it prints, `53122` in `192.168.1.24:53122`. The port
   stays the same from one pairing to the next.
2. Find that computer's Tailscale address, with `tailscale ip -4` or in the
   Tailscale app. It starts with `100.`.
3. On the other computer, run `amux pair` with the Tailscale address and that
   port, and type the code:

   ```sh
   amux pair 100.101.102.103:53122
   ```

Use the address itself: a Tailscale machine name doesn't work here.

Once paired, the two keep connecting directly over Tailscale.

**Your phone** can use Tailscale too. With Tailscale on on the phone, scan
the computer's QR code (from setting up, or `amux pair --qr`): the code
carries the computer's Tailscale address. Typing the six-digit code alone
won't work, because the phone only lists computers it finds on the local
network.

## Checking a connection

Press **h** on the terminal's home screen to see every paired computer and
how it's reached: `direct` on the same network or over Tailscale, `ssh`, or
`relay`, or why it can't be reached right now.

To stop trusting a computer, run `amux unpair <name>`.

To understand what pairing and the relay can and can't do, see
[How amux works](../understand/how-amux-works.md).
