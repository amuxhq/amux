---
title: Updates and channels
description: How amux keeps itself up to date without interrupting your agents, and how to choose between stable and preview releases.
sidebar:
  order: 5
---

amux updates itself. About once an hour it checks for a new release, and when
there is one it downloads it, checks it, and restarts onto it. Your agents keep
running throughout: each agent is its own process, so it carries on with its
turn while amux restarts, and your devices catch up a moment later.

You don't need to do anything. If you want to know where things stand, run:

```sh
amux status
```

It says which version is running, how updates arrive, and which channel you're
on.

## Updating now

```sh
amux update
```

This installs the latest release on your channel straight away and waits for
it to start. If you're already up to date, it says so.

A terminal app you opened before an update keeps working. It shows
`restart to update` at the top of home until you quit and run `amux` again.

## Channels

There are two channels:

- **stable**, which everyone is on unless they choose otherwise.
- **preview**, which gets releases before stable does, for people who want
  them early.

```sh
amux config set channel preview   # get releases early
amux config set channel stable    # go back
```

The change applies at the next check, or at once with `amux update`. amux
never installs an older version, so going back to stable keeps the preview
release you have until stable passes it.

New releases can reach machines gradually, so a machine next to yours may
update a little before or after it.

## If an update doesn't start

If a new version fails to start three times in a row, amux puts the previous
version back by itself and carries on with it. It won't try that release again
on its own. `amux status` says when this has happened, and
`amux update --retry` tries the release again.

## Turning automatic updates off

To install releases only when you ask for them:

```sh
amux config set updates manual
```

amux then updates only when you run `amux update`. `amux config unset updates`
turns automatic updates back on.

On a machine where your own service manager runs amux, amux never updates
itself ([Running amux on a server](server.md#running-amux-under-your-own-service-manager)).

## How releases are checked

Every release is signed. Before installing anything, amux checks the release's
signature against a key built into the version you already have, then checks
the download's size and checksum against the signed release. A download that
doesn't match is never installed.

The very first install is the one step that can't be checked this way, because
there's no amux on the machine yet to do the checking. The install script
downloads amux from amux.sh over HTTPS, and checks the download's checksum
before installing it.

## The apps

The iPhone app updates through the App Store, like any other app.
