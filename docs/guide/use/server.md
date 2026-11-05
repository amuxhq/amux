---
title: Running amux on a server
description: Put amux on an always-on machine, such as a Linux box or a Mac mini, so your agents keep working while your laptop sleeps.
sidebar:
  order: 4
---

amux runs just as well on a machine you never sit at: a Linux server, a cloud
VM, or a Mac mini in a cupboard. Your agents run there, and you start them,
talk to them and review their work from your laptop and your phone. Your laptop
can sleep, or leave the house, and the agents carry on.

## Set it up

On the server, install amux the same way as anywhere else
([Install](../get-started/install.md)):

```sh
curl -fsSL https://amux.sh/install | sh
```

Your agents run Claude Code and Codex on the server itself, so install
whichever you use there and sign in to it, with `claude` then `/login`, or
`codex login`. amux uses what's on that machine, the same as you would.

Then give the server a name your other devices will show, and tell it which
folders to offer when you start an agent there:

```sh
amux config set host_name build-box
amux config set repositories '["~/source/app", "~/source/api"]'
```

## Pair it

From your laptop, pair over SSH. If you can `ssh` to the server, this is all it
takes, with no code to type:

```sh
amux pair you@build-box
```

From then on your laptop reaches the server through SSH whenever it isn't on
the same network. For your phone, and other ways to reach a server, such as
the relay or Tailscale, see
[Add another computer](../get-started/add-another-computer.md).

## Keep it running

amux keeps running after you close your SSH session, but it won't come back by
itself after the server restarts. Ask it to start at login:

```sh
amux config set start_at_login true
```

On Linux this adds a systemd user service. A user service normally starts only
when you log in, so on a server you never log in to, let it start at boot as
well:

```sh
sudo loginctl enable-linger "$USER"
```

Lingering also stops systemd from ending your processes when you log out, on
systems set up to do that.

Agents find their tools through your `PATH`, and the service remembers the
`PATH` of the shell you turned it on from. Turn it on from a normal login
shell, where `claude`, `codex` and the tools your agents use are all found.

`amux status` tells you whether amux is running and whether it starts at
login.

### Sleep

On a Mac, amux keeps the machine awake while it's plugged in, as long as the
lid is open. Turn that off with `amux config set keep_awake false`. On Linux,
amux leaves power settings to you, so make sure the machine doesn't suspend on
its own.

## Running amux under your own service manager

If you'd rather run amux as a system service you manage yourself, stop it,
turn off starting at login if you turned it on, and tell it there's no
supervisor. Only the supervisor installs updates, so updates become manual
first:

```sh
amux server stop
amux config set start_at_login false
amux config set updates manual
amux config set supervisor false
```

Then have your service manager run `amux daemon`. With systemd, a unit along
these lines works:

```ini
[Unit]
Description=amux

[Service]
User=you
ExecStart=/home/you/.amux/bin/amux daemon
Environment=PATH=/home/you/.local/bin:/usr/local/bin:/usr/bin:/bin
Restart=on-failure
KillMode=process

[Install]
WantedBy=multi-user.target
```

Two lines matter:

- `KillMode=process` stops only amux when the service stops, and leaves your
  agents running. Without it, a restart takes your agents down with it.
- `Restart=on-failure`, not `always`. Otherwise `amux server stop` can never
  stop amux, because the service manager starts it straight back up.

Without a supervisor, amux doesn't update itself. To update, run the install
script again, which installs the latest stable release, and restart the
service ([Updates and channels](updates.md)).

## Stopping amux

`amux server stop` stops amux, and your agents keep running. Each one waits a
while for amux to come back, five minutes by default. If amux isn't back by
then, the agent finishes the turn it's on and exits, and you can resume it
later from where it stopped ([How amux works](../understand/how-amux-works.md)).
