# What amux sends

*For anyone running amux who wants to know what it tells amux.sh about how it is used, and how to turn that off.*

amux sends a small number of usage events so we can tell whether people
install it, get it working across their machines, come back to it, and run
into the free tier. They are counts and choices, never content: what you
type, what your agents say and the names of your machines, folders and
files stay on your machines.

It is on by default. To turn it off:

- **On a computer:** `amux config telemetry off`. A running daemon stops
  sending at once, and nothing it records while off is sent later.
  `amux config telemetry on` resumes it, or, if the daemon started with
  telemetry off, takes effect when it next starts. `amux config telemetry`
  says whether it is on and where it goes.
- **`DO_NOT_TRACK`:** a daemon started with `DO_NOT_TRACK` set to anything
  but empty or `0` sends nothing, whatever the setting says. It must be set
  where the daemon starts: a daemon started at login, or by a service
  manager, does not see a variable set in a shell profile, so on a desktop
  install the setting above is the switch that always reaches it.
- **On the iPhone:** turn off **Share Usage** under **You**, in **This phone**.

Development builds, test runs and test networks never send anything. Only
the published `amux` binary and the App Store app do.

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
a version, or a host id. No event has a field for free text.

| Event | Sent when | Properties |
|---|---|---|
| `installed` | amux starts on a machine for the first time. | — |
| `checked_in` | Once a day for each profile while amux runs. | `agents_running`, `agents_created_24h` and `turns_24h`: counts of this machine's agents running now, created in the last day and turns they finished since the last check-in, each by kind (`claude_pty`, `claude_sdk`, `codex`). `paired_hosts`: how many machines are paired. `hosts_by_route`: how many are reachable now `direct`, through the `relay` or over `ssh`. `relay_carrier`: `quic` or `tcp`, while connected to the relay. `signed_in`: whether the profile is signed in. `tier`: `free` or `pro`, while connected to the relay. |
| `updated` | amux starts on a newer version than last time. | `from_version`, `to_version` |
| `update_rolled_back` | amux starts on an older version than last time, because the newer one was put back. | `from_version`, `to_version` |
| `daemon_crashed` | amux starts and finds that its last run stopped without shutting down, with the machine still up. | `crashed_version`: the version that stopped. |
| `client_opened` | The terminal client starts, or the iPhone app comes to the front; at most once an hour. | `surface`: `terminal` or `phone`. |
| `agent_created` | A new agent is started. | `kind`; `on`: `this_host` or `paired_host`, with `remote_host` for a paired one; `by`: `person` or `agent` (an agent starting a helper). |
| `prompt_sent` | A prompt you sent is accepted. | `kind`; `on` and `remote_host` as above; `queued`: whether it waits behind a turn already running. |
| `ask_answered` | Your answer to an agent's question or request is accepted. | `kind`; `on` and `remote_host` as above; `ask`: `question`, `permission`, `plan`, `form`, `link` or `grant`. |
| `pairing_started` | This machine shows a pairing code, or enters one. | `role`: `offerer` (showed the code) or `joiner` (entered it); `method`: `pin`, `qr` or `ssh`. |
| `pairing_succeeded` | A pairing finished and each machine trusts the other. | `role`, `method`, and `remote_host`: the machine paired with. |
| `pairing_failed` | A pairing did not finish. | `role`, `method`, and `reason`: `wrong_secret`, `no_window`, `unreachable`, `timed_out`, `abandoned`, `self_pairing` or `other`. |
| `signed_in` | A profile signs in to an amux.sh account. | — |
| `sign_in_failed` | Signing a profile in did not work. | `reason`: `rejected`, `unreachable`, `account_elsewhere`, `profile_conflict` or `other`. |
| `signed_out` | A profile signs out. | — |
| `relay_refused` | The relay refused to carry traffic because the account is on the free tier; at most once an hour. | — |
| `paywall_viewed` | The iPhone app's subscription page opens. | `from`: the tab it was opened from, `agents`, `hosts` or `you`. |
| `purchase_started` | The iPhone app starts a purchase with the App Store. | `interval`: `monthly` or `yearly`. |

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

The daemon's own log on each machine names machines and gives reasons in
words; it leaves the machine only inside a report you send, redacted, and
has nothing to do with these events.

## For developers

The events are the `Event` type in
[`crates/analytics`](../crates/analytics/src/lib.rs): every field is an
enum, a number, a bool, a version or a host id, so the type is the list of
what can be sent, and a test fails when an event or a property is missing
from this page. Setting `AMUX_ANALYTICS_URL` to a base URL sends a
development build's events to `<base>/api/events` instead of nowhere, for
trying the uploader against a server of your own; `DO_NOT_TRACK` and the
setting still turn it off. The tunable values are in
[parameters](PARAMETERS.md).
