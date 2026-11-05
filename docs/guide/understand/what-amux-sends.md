---
title: What amux sends
description: The usage counts amux sends to amux.sh, every event and every field, what it never sends, and how to turn it off.
sidebar:
  order: 3
---

amux sends a small number of usage events, so we can tell whether people
install it, get it working across their machines, come back to it, and run
into the limits of the free plan. They are counts and choices, never content.
What you type, what your agents say, and the names of your machines, folders
and files stay on your machines.

This page lists everything that's sent, field by field.

## Turning it off

Usage counts are on by default, with nothing to accept first. Setting up a
computer says so in one line, on its **Help improve amux** step. To turn them
off:

- **On a computer:** turn off **Send usage counts** in the terminal
  app's Settings, or run `amux config set telemetry false`. amux
  stops sending at once, and nothing it records while off is sent later.
  `amux status` says whether it's on.
- **On the iPhone:** turn off **Share Usage** under **You**, in
  **This phone**. The app's App Store privacy label lists what it collects.
- **With `DO_NOT_TRACK`:** amux started with `DO_NOT_TRACK` set to anything
  but empty or `0` sends nothing, whatever the setting says. It has to be set
  where amux starts, though. amux started at login, or by a service manager,
  doesn't see a variable set in your shell profile, so on a desktop the
  setting above is the switch that always works.

Only the published `amux` and the App Store app send anything. Builds from
source and test runs never do.

## Where it goes

Each machine posts batches of events to `https://amux.sh/api/events` (or to
the amux.sh a profile is signed in to). amux.sh checks every event against
the list on this page, drops anything else, works out the country from the
request's address, and passes the events to [PostHog](https://posthog.com)
in the EU, where we read them. amux.sh never passes on or stores the full
address; PostHog receives at most the first three parts of an IPv4 address
or the first three groups of an IPv6 one, uses it to find the country and
then discards it.

A profile that is signed in to an amux.sh account sends the account's token
with its events, so its events are joined to that account. A profile that
never signed in sends no token and is known only by its random host id.

Events wait in memory and go once a minute, or sooner once 50 have
gathered. A batch that cannot be sent is tried twice more and then dropped.
Nothing is written to disk to be sent later.

## The ids

| Id | What it is |
|---|---|
| `installation_id` | A random id made the first time amux starts on a machine (or the app on a phone) and kept in the installation's directory. It counts installs. |
| `host_id` | The random id of one profile on that machine, the same one its paired machines know it by. Every event is filed under it. |
| `remote_host` | The host id of *another* machine: the one a prompt, an answer or a new agent went to, or the one this machine just paired with. |

`remote_host` is how we tell that two machines belong to the same person
without an account: when your laptop sends a prompt to your desktop, the
laptop's event names the desktop's host id. That is a map of which of your
machines talk to each other, by random ids only. It never includes a
machine's name or address.

A reinstall makes new ids. Deleting the installation's directory, or the
app, forgets them.

## On every request

| Field | What it is |
|---|---|
| `installation_id`, `host_id` | As above. |
| `platform` | `macOS`, `Linux`, `Windows` or `ios`. |
| `os_version` | The operating system's version: the product version on macOS and iOS, the kernel release on Linux; none on Windows. |
| `arch` | The processor architecture, such as `aarch64`. |
| `version` | amux's version. |
| `channel` | The release channel the machine follows: `stable` or `preview`. |

## The events

Every value below is one of the listed words, a number, `true` or `false`,
a version, a host id, or the name of one of amux's own checks. No event has
a field for free text.

| Event | Sent when | Properties |
|---|---|---|
| `installed` | amux starts on a machine for the first time. | — |
| `checked_in` | Once a day for each profile while amux runs. | `agents_running`, `agents_created_24h` and `turns_24h`: counts of this machine's agents running now, created in the last day and turns they finished since the last check-in, each by kind (`claude_pty`, `claude_sdk`, `codex`). `paired_hosts`: how many machines are paired. `hosts_by_route`: how many are reachable now `direct`, through the `relay` or over `ssh`. `relay_carrier`: `quic` or `tcp`, while connected to the relay. `signed_in`: whether the profile is signed in. `tier`: `free` or `pro`, while connected to the relay. |
| `updated` | amux starts on a newer version than last time. | `from_version`, `to_version` |
| `update_rolled_back` | amux starts on an older version than last time, because the newer one was put back. | `from_version`, `to_version` |
| `daemon_crashed` | amux starts and finds that its last run stopped without shutting down, with the machine still up. | `crashed_version`: the version that stopped. |
| `client_opened` | The terminal client starts, the Mac app opens, or the iPhone app comes to the front; at most once an hour. | `surface`: `terminal`, `mac` or `phone`. |
| `agent_created` | A new agent is started. | `kind`; `on`: `this_host` or `paired_host`, with `remote_host` for a paired one; `by`: `person` or `agent` (an agent starting a helper). |
| `prompt_sent` | A prompt you sent is accepted. | `kind`; `on` and `remote_host` as above; `queued`: whether it waits behind a turn already running. |
| `ask_answered` | Your answer to an agent's question or request is accepted. | `kind`; `on` and `remote_host` as above; `ask`: `question`, `permission`, `plan`, `form`, `link` or `grant`. |
| `pairing_started` | This machine shows a pairing code, or enters one. | `role`: `offerer` (showed the code) or `joiner` (entered it); `method`: `pin`, `qr` or `ssh` (a window showing both a PIN and a QR code counts as `pin`; the pairing's success or failure says which was used). |
| `pairing_succeeded` | A pairing finished and each machine trusts the other. | `role`, `method`, and `remote_host`: the machine paired with. |
| `pairing_failed` | A pairing did not finish. | `role`, `method`, and `reason`: `wrong_secret`, `no_window`, `unreachable`, `timed_out`, `abandoned`, `self_pairing` or `other`. |
| `signed_in` | A profile signs in to an amux.sh account. | `via`: `browser` (the account service's pages, from this device) or `paired_device` (it accepted a paired device's offer of its account). |
| `sign_in_failed` | Signing a profile in did not work. | `reason`: `rejected`, `unreachable`, `account_elsewhere`, `profile_conflict` or `other`. |
| `sign_in_offer` | A signed-in device offered a paired one its account, and the offer was shown, accepted or dismissed. | `role`: `offerer` (the signed-in side) or `receiver` (the side offered the account); `outcome`: `shown` (a client put it in front of its person), `accepted` or `dismissed`. |
| `signed_out` | A profile signs out. | — |
| `relay_refused` | The relay refused to carry traffic because the account is on the free tier; at most once an hour. | — |
| `paywall_viewed` | The iPhone app's subscription page opens. | `from`: the tab it was opened from, `agents`, `hosts` or `you`. |
| `purchase_started` | The iPhone app starts a purchase with the App Store. | `interval`: `monthly` or `yearly`. |
| `onboarding_step` | A step of the first-run setup is shown, confirmed or skipped. | `step`: `pair_phone`, `keep_reachable`, `sign_in`, `repositories`, `improve` or `first_agent`. `outcome`: `shown`, `done` or `skipped`. `surface`: `terminal`, `mac` or `phone`. |
| `repositories_chosen` | The repositories step is confirmed. | `kept`: how many folders were kept; `suggested`: how many were suggested. Numbers only, never the folders. |
| `check_failed` | One of amux's internal checks broke: a rule the daemon, an agent process, the relay or a connection's path choice holds after every step failed, a step took longer than allowed, or a chat's transcript failed its own check on a client. At most once per check every ten minutes. | `core`: `daemon`, `agent`, `relay`, `path` or `transcript`. `check`: which check, as the name amux's code gives it, such as `daemon.generation_fell`. `cause`: `invariant` or `over_budget`. `held_back`: how many more times the same check broke since the last one was sent. |

A prompt or answer sent to another machine's agent is counted once, by the
machine where you sent it; the machine that runs the agent does not count
it again. Prompts typed straight into an agent's own terminal never pass
through amux's client, so they show up only in `turns_24h`. SSH pairing is
counted once the daemon trusts the other machine; an SSH exchange that fails
before that is not counted.

## What is never sent

- What you type: prompts, answers, messages between agents, drafts.
- What agents produce: their output, tool calls, files, diffs and logs.
- Names: your name and email, machine names, profile labels, agent names.
- Paths: folders, repositories and file names.
- Secrets: pairing PINs and QR codes, keys, tokens other than the account
  token described above.
- Your IP address beyond what is needed to find a country, as described
  above.

## Problem reports

Problem reports are separate from usage counts. A report goes to amux.sh only
when you send one yourself, from the terminal app or the phone, or
automatically when amux catches one of its own bugs, if you turned on
automatic reports. Automatic reports are off unless you turn them on: setting
up asks, and you can change it later with **Send reports automatically** in
Settings, **Send Automatic Reports** on the phone, or
`amux config set auto_reports true`.

A report you send yourself includes the screen you froze to report it, as
you saw it, with your marks and your note. If a chat was on screen, its
visible text goes with the report, so look before you send.

Everything else in a report, and the whole of an automatic one, carries the
shape of what went wrong, not its content. Which steps ran, in what order,
how big things were and how long they took are kept. Every word of your
chats, your code, and the names of your machines, folders and files are
emptied out or masked before the report leaves your machine. amux's own logs
go with it, with paths, email addresses and machine and user names masked. A signed-out profile sends no reports. If you delete your
account, its reports are deleted with it.

The [debugging guide](../../DEBUGGING.md#sending-reports) describes a
report's contents in full.

A test in amux's source checks that this page names every event and every
field a device can send, so the list above can't fall behind the code.
