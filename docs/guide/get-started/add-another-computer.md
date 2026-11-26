---
title: Add another device
description: Pair your devices so each one sees and runs the other's agents, on the same network, by address, over SSH, through amux's relay, or over Tailscale.
sidebar:
  order: 4
---

Pair two devices and each one sees the other's agents, opens their chats,
and starts new agents there. A desktop and a laptop, a home machine and a
server: once they're paired, you work with the agents on both from either.

First [install amux](install.md) on the new device. Then pair the two.
Pairing is a one-time step, and a paired device can run agents on the
other as you, so pair only devices you control.

Your phone pairs with each device on its own: pairing two devices
doesn't pair your phone with the new one.

## On the same network

On the device you want to reach, open pairing:

```sh
amux pair
```

It shows a six-digit code and the command to run on the other device. On
the other device, `amux devices` lists the devices it can see, each with
the command that pairs it:

```text
desk   found nearby · amux pair desk
```

Run that command and type the code. That's it.

## By address

Some networks, such as many office and hotel ones, stop devices finding each
other. Pair by address instead. `amux pair` prints the address to use:

```text
On another device: run `amux pair desk` (or `amux pair 192.168.1.24:53122` if it is not found nearby) and enter the PIN.
```

Run the address version on the other device and type the code.

## Over SSH

If you can SSH into the other machine, you can pair through that login, with
no code to type:

```sh
amux pair you@server.example.com
```

amux must already be installed there, and SSH must log in without asking for
a password, for example with a key in your SSH agent. The two devices then
connect through SSH from then on. This is a good fit for a server you already
SSH into; see [Running amux on a server](../use/server.md).

From Windows, amux uses the `ssh` that comes with Windows. A key with a
passphrase works only once it's in the SSH agent, and Windows ships that
service turned off. In PowerShell as an administrator, turn it on once:

```powershell
Get-Service ssh-agent | Set-Service -StartupType Automatic
Start-Service ssh-agent
```

Then, as yourself, `ssh-add` your key. The machine you pair with must run
macOS or Linux: amux can't be reached over SSH on a Windows machine.

## Through amux's relay

With [Pro](https://amux.sh/docs/account/plans-and-billing/), devices signed
in to the same account reach each other through amux's relay, wherever they
are. `amux devices` lists your account's other devices:

```text
studio   on your account · amux pair studio
```

Run `amux pair studio`, and type the code that `amux pair` shows on the
other device. Without Pro, `amux devices` still lists them, and says Pro
pairs them.

A device that one of your paired devices can see shows up as
`through desk · amux pair studio`. Pair it the same way.

## Tailscale

If your devices are on the same [Tailscale](https://tailscale.com) network,
they can pair and connect directly, wherever they are, with no relay and no
Pro. Tailscale doesn't carry the messages amux uses to find devices nearby,
so pair by address, using the device's Tailscale address:

1. On the device you want to reach, run `amux pair`. Note the port at the
   end of the address it prints, `53122` in `192.168.1.24:53122`. The port
   stays the same from one pairing to the next.
2. Find that device's Tailscale address, with `tailscale ip -4` or in the
   Tailscale app. It starts with `100.`.
3. On the other device, run `amux pair` with the Tailscale address and that
   port, and type the code:

   ```sh
   amux pair 100.101.102.103:53122
   ```

Use the address itself: a Tailscale machine name doesn't work here.

Once paired, the two keep connecting directly over Tailscale.

**Your phone** can use Tailscale too. With Tailscale on on the phone, scan
the device's QR code (from setting up, or `amux pair --qr`): the code
carries the device's Tailscale address. Typing the six-digit code alone
won't work, because the phone only lists devices it finds on the local
network.

## Checking a connection

Press **h** on the terminal's home screen to see every paired device and
how it's reached: `local` on the same network, `direct` across networks
without the relay, `relayed` through amux's relay, or `ssh`, or why it
can't be reached right now, such as `away` or `offline`.

To unpair a device, run `amux unpair <name>`.

To understand what pairing and the relay can and can't do, see
[How amux works](../understand/how-amux-works.md).
