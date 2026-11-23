---
title: Connect your phone
description: Get amux for iPhone or Android, pair it with the device your agents run on, and reach your agents at home or away.
sidebar:
  order: 3
---

amux for [iPhone](https://apps.apple.com/us/app/amux/id6760197635) and
[Android](../use/android.md) shows the agents on your devices, lets you
answer them, and starts new ones. The agents run on your Mac or Linux
machines; the phone is where you follow and steer them.

## Pair your phone

Pairing is a one-time step that makes your phone and a device trust each
other. A paired phone can start agents on that device, which run as you, so
pair only your own devices.

**While setting up.** The **Connect your iPhone** screen shows a QR code and a
six-digit code. Either:

- **Scan the QR code** with your iPhone's camera. With the app installed, it
  opens the app ready to pair. Without it, it opens the App Store; install
  the app and scan again.
- **Enter the code in the app.** Open amux on your phone while it's on the
  same Wi-Fi as the device. The app finds the device and asks
  **Pair with laptop?** with the number pad already up. Type the six digits.

Either way, the phone shows the device's name and how it will reach it,
**Local** or **Relayed**, before it trusts anything. Once it's paired, the
device's agents appear in the app.

**Any time later**, on the device, run:

```sh
amux pair          # shows a six-digit code
amux pair --qr     # shows a QR code for the camera
```

or choose **Finish setting up** on the terminal's home screen.

The first time the app looks for devices, iOS asks whether amux may find
devices on your local network. Allow it: that's how the phone finds your
Mac or Linux machine. If you said no, turn on **Local Network** for amux in
the iPhone's Settings.

A code works once, for a few minutes. After five wrong tries it stops
working; open pairing again for a new one.

## At home and away

**On the same network,** your phone reaches your device directly. The
pairing screen shows the device as **Local**, and once it's paired the
connection reads **Local** too. This is free and needs no account.

**Away from home,** your phone reaches your device through amux's relay, and
the app shows the connection as **Relayed**. Pro relays connections when your
phone and device aren't on the same network, so this needs both signed in to
the same amux account, with [Pro](https://amux.sh/docs/account/plans-and-billing/).
Your agents' traffic is encrypted end to end between your phone and your
device; the relay passes along data it can't read. See [Security and privacy](../understand/security-and-privacy.md).

You only sign in once. When you sign in on the device during setting up,
**Also sign in** your phone is ticked, and the phone then asks
**Sign in as you@example.com?**. Tap **Sign In** and it's done, with no second
trip through the browser. It works the other way too: a phone that's signed
in offers its account to a device it pairs with.

**With Tailscale,** your phone can reach your device directly wherever you
are, without the relay. Turn Tailscale on on the phone and pair by scanning
the QR code: it carries the device's Tailscale address. See
[Tailscale](add-another-computer.md#tailscale).

## Next

- [The iPhone app](../use/iphone.md): everything the iPhone app does.
- [The Android app](../use/android.md): everything the Android app does.
- [Add another device](add-another-computer.md): your phone pairs with each
  device on its own.
