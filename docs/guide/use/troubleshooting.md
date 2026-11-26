---
title: Troubleshooting
description: What to check when amux isn't running, a device is out of reach, or an agent can't carry on, and how to send a report.
sidebar:
  order: 8
---

## Start with amux status

```sh
amux status
```

It answers most questions in a few lines:

- whether amux is running, and which version
- how it updates, and whether a release was rolled back
- whether it starts at login
- each account on this device, and whether the relay is connected
- whether usage counts are on, and where the config file is
- on Windows, whether the firewall lets devices nearby reach amux

## amux isn't running

Any `amux` command starts amux if it's stopped, and so does opening the
terminal app. To start it on its own:

```sh
amux server start
```

If it still doesn't start, the logs say why. They're in
`~/.local/share/amux/` (on Windows, `%LOCALAPPDATA%\amux\`): `daemon.log`
for amux itself, and `supervisor.log` for the small process that starts it
and keeps it up to date.

Your agents don't depend on amux running. If amux was away for long enough
that an agent gave up waiting, the agent shows as **stopped** with a note that
amux was away. Send it a message, or run `amux resume <agent>`, and it picks
up where it left off.

## A device is out of reach

Your fleet and the Devices tab on your phone say why a device can't be reached:

| What it says | What to do |
|---|---|
| **away** | The device is only reachable through amux's relay, and your account is on the free plan: relayed connections need Pro. Get on the same network as the device, or [subscribe to Pro](https://amux.sh/docs/account/plans-and-billing/). |
| **offline** | Nothing reaches it right now. Check that the device is on and amux is running there (`amux status`). If it's on another network, both ends need to be signed in to the same account, with Pro. |
| **this device is signed out** | Sign in again with `amux login`, or from Settings in the app. |
| **unpaired this device** | The other device unpaired this one. Pair them again ([Add another device](../get-started/add-another-computer.md)). |

An agent on a device that's out of reach stays in your fleet as it last was.
Its chat fills in again when the device is back.

## The phone doesn't find my device

- Check that the phone and the device are on the same network. Guest
  networks, and some office networks, keep devices from seeing each other.
- Check that the app may use the local network: in the iPhone's **Settings**
  app, go to **Privacy & Security** → **Local Network** and turn on amux.
- Open amux on the device and start pairing there, with `amux pair`. The
  phone offers to pair only with a device that's waiting for it.
- If the device runs Windows, check its firewall (next section).

[Connect your phone](../get-started/connect-your-phone.md) has the other ways
to pair, including when the phone and device are on different networks.

## Devices nearby can't reach a Windows device

Windows' firewall drops what other devices on your network send to a program
it has no rule for. Until amux has one, a Windows device still reaches your
other devices, because it dials out to them, but they can't dial it, and
neither your phone nor your other computers find it nearby. `amux status`
says which it is. To add the rule:

```sh
amux setup firewall
```

Windows asks once for an administrator's approval. The rule lets devices on
private and domain networks reach amux, never on public ones such as a café's,
and lasts across updates, because amux is always started from the same
place, `~\.amux\bin\amux.exe`.

If you dismissed Windows' own prompt to allow amux when it first appeared,
Windows added a rule that blocks it instead. `amux status` says so, and
`amux setup firewall` replaces that rule with its own. It removes nothing
else: if a rule you made blocks amux, or you changed the rule Windows
added, it names the rule and changes nothing, so you can change it yourself
in Windows Defender Firewall.

A rule that lets in only some addresses, such as one computer's, doesn't let
in the rest of your devices, so `amux setup firewall` adds its rule beside
it. A rule that blocks some addresses may be blocking devices on your
network, so amux counts it as a block.

On a work computer, your organisation's policy may set the firewall's
rules. If a rule of the policy blocks amux, or the policy ignores rules
made on the computer itself, `amux setup firewall` says so and changes
nothing: whoever manages the policy can add the rule.

## Pairing over SSH from Windows fails

amux pairs and links over SSH with the `ssh` on your `Path`, normally the
one that comes with Windows, and never stops to ask for a password or a
passphrase. A key with a passphrase works only once it's in Windows' SSH
agent, which Windows ships turned off. In PowerShell as an administrator,
turn it on once:

```powershell
Get-Service ssh-agent | Set-Service -StartupType Automatic
Start-Service ssh-agent
```

Then, as yourself, run `ssh-add` with your key, and check that
`ssh you@server` logs in without asking anything before you pair again. If
another `ssh` comes first on your `Path` (Git's, say), amux uses that one,
and it doesn't read Windows' SSH agent. The machine at the other end must
run macOS or Linux: amux can't be reached over SSH on a Windows machine.

## On Windows, amux finds different settings in some terminals

On Windows amux keeps its settings in `%APPDATA%\amux\config.toml` and its
data in `%LOCALAPPDATA%\amux\`. If `XDG_CONFIG_HOME` or `XDG_DATA_HOME` is
set, as some shells and tools for Unix programs do (Git Bash, MSYS2, a
profile script), amux uses those folders instead. Set in one terminal only,
they give amux run from that terminal a different config and data from the
amux Windows starts when you sign in: different agents, pairings and
settings, and a second amux running beside the first.

`amux status` names the config file it reads. To see whether either
variable is set, run this in PowerShell (in Git Bash, drop the `env:`):

```powershell
echo $env:XDG_CONFIG_HOME $env:XDG_DATA_HOME
```

Unset them in the shell that sets them. Or, to keep amux there on purpose,
set them for your whole account instead (in **Settings**, search for
**environment variables**, then **Edit environment variables for your
account**), and sign out and in again.

## On Windows, amux warns about the drive its data is on

amux keeps its data on a local drive formatted NTFS or ReFS, where Windows
can replace or remove a file something else has open and keep the files
yours alone. On a network drive, a USB stick, a RAM disk or a FAT or exFAT
drive (as a redirected or roaming profile may put `%LOCALAPPDATA%`), amux
still starts, but `daemon.log` says `the root's volume is a poor home for
amux's data` and why, and updates, cleanup and privacy may not work as
they should.

Point amux at a folder on a local NTFS drive, then restart it:

```powershell
amux config set root D:\amux
amux server stop
amux
```

amux starts afresh there: what it kept in the old folder, your pairings
and your agents' history, stays behind, so this is best done before you've
set much up.

## An agent can't carry on

The line under an agent's name, and the box where you type to it or the chat
just above it, say what's in the way:

- **Claude or Codex needs you to sign in.** amux can't sign in for you. On the
  device the agent runs on, run `claude` and sign in with `/login`, or run
  `codex login`. Then send the agent a message: you don't need to stop it, and
  sending is how it tries again. The notice goes once Claude or Codex answers.
  Claude can take up to half a minute to notice you signed in, so if the
  first message still fails, send another. Codex keeps the API key or ChatGPT
  account it started with: if you signed in with a different one, run
  `amux settle <agent>` and then `amux resume <agent>`, which starts it again
  on the same conversation.
- **Can't reach Claude.** The request got no answer, usually because the
  device the agent runs on has lost its connection. Claude keeps trying by
  itself, and the chat counts down to the next attempt.
- **A usage limit is reached.** The agent's provider has a limit on how much
  you can use it. The agent's row and its chat say when the limit resets, and
  you can still type your next message in the meantime.
- **The agent can't find a command,** such as `npm` or `cargo`, though it works
  in your own terminal. amux started at login remembers the `PATH` of the shell
  it was turned on from. Turn it off and on again from a shell where the
  command works:

  ```sh
  amux config set start_at_login false
  amux config set start_at_login true
  ```

  The new `PATH` applies from your next login, or straight away after
  `amux server stop` and `amux server start`.

  On macOS and Linux that is always the way. On Windows you often don't
  need to: when a program you install adds its folder to your user or
  system `Path` (as the Node.js and npm installers do), each agent started
  after that finds it, with no restart. An agent that was already running
  keeps the `PATH` it started with: run `amux settle <agent>` and then
  `amux resume <agent>` to start it again on the same conversation. A
  folder you added only in a terminal or a shell profile still needs the
  steps above. An agent whose `env` in your amux config sets its own `PATH`
  (see [Other agents](other-agents.md#add-your-own-agent)) uses that `PATH`
  as written, so add the folder there.
- **The relay refuses this version.** `amux status` says so. Run
  `amux update`.

## A device needs updating

A newer amux can ask an older device for something it doesn't know. Two
messages mean that, and neither loses your words:

| What it says | What to do |
|---|---|
| **that device needs updating to do this** | Update amux on that device. |
| **not sent: one of its attachments is of a kind this device can't read** | Update amux on the agent's device and send it again. Your words are back in the composer. |

[Updates and channels](updates.md) says how.

## An update didn't happen

`amux status` says if the latest release failed to start on this device and
amux went back to the version before. `amux update --retry` tries it again.
[Updates and channels](updates.md) has more.

## Sending a report

If something looks wrong, send a report while it's still on screen. A report
holds the screen you froze, with your marks and note, what amux's own parts
were doing, and its own logs. Apart from what's on that screen, it never
holds your messages, your code, or your file and folder names
([What amux sends](../understand/what-amux-sends.md#problem-reports)).

- **In the terminal app**, press `b` on home, or Ctrl+A then `b` in a chat. The
  screen freezes: drag over anything that looks wrong, add a note, and press
  Enter.
- **On iPhone**, take a screenshot and the app offers to report it, or use
  **Report a Problem** under **Help** in Settings.
- **On Android**, take a screenshot and a banner offers to open **Report a
  Problem** (you can drag boxes to mark what's wrong or tap **Hide** to black
  out sensitive areas before sending), or use **Report a Problem** under
  **Help** in Settings.
- **From the shell**, run `amux dump --reason "what looks wrong" --send`.

Reports go to your amux account, so you need to be signed in to send one. A copy
stays in `~/.local/share/amux/reports/` (on Windows,
`%LOCALAPPDATA%\amux\reports\`). Without `--send`, `amux dump` only
saves the report there, so you can look at it, or share it yourself. On the
phone, **Export Diagnostics** does the same.

You can also turn on automatic reports, which amux sends when one of its own
checks finds something wrong. They're off unless you turn them on, in Settings
or with `amux config set auto_reports true`.

## Starting over

`amux uninstall` stops this device's agents, after asking, and removes amux.
Add `--delete-data` to remove its data and settings too, including your
pairings and your agents' history.

If you're stuck, [contact support](https://amux.sh/support).
