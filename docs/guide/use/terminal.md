---
title: Terminal app
description: Run, watch and talk to every agent on every machine from one terminal, and the amux commands worth knowing.
sidebar:
  order: 1
---

Run `amux` with nothing after it and the terminal app opens: every agent on
every machine you've paired, and a chat with any of them. If amux isn't
running yet, the app starts it first.

```sh
amux                     # open the terminal app
amux --profile work      # open it on another profile
```

Press `?` at any time to see every key.

## Home

Home is your fleet. `+ New Agent` sits at the top, and the agents are below it
in four sections:

- **Pinned**: agents you pinned, kept at the top.
- **Needs you**: agents waiting on you, to approve a command, answer a
  question or decide on a plan.
- **Live**: agents starting, working, or idle and ready for the next prompt,
  and agents that have exited but you haven't settled yet.
- **Settled**: agents you're done with. This section starts folded.

Each row shows the agent's name, its branch, which agent it is (Claude or
Codex), its folder and, when it isn't this machine, its host. The line under
the name says what's going on: the question it's asking, the step it's
running, what it last said, or why it can't carry on. A filled `●` means it's
working, and a blue `●` means it needs you. Agents are ordered by when they
last changed state, so a row doesn't jump around while its agent works.

An agent that started subagents of its own is shown with them indented beneath
it ([Agents working together](agents-working-together.md)). Press `→`, `←` or
Space to show or hide them.

On home:

| Key | What it does |
|---|---|
| ↑ ↓ or `k` `j` | Move |
| Enter | Open the agent's chat |
| `n` | Start a new agent |
| `/` | Filter by name, branch, folder or host |
| `r` | Rename the agent. Its new name replaces any title. |
| `x` | Settle the agent, asking first if that would cut work short; on a settled agent, delete it, asking first |
| `p` | Pin or unpin the agent |
| `h` | Your hosts, and how each one is reached |
| `,` | Settings |
| `b` | Report a problem |
| `q` | Leave for the shell. Your agents keep running. |

## Starting an agent

Press `n`. Type what the agent should work on, and press Enter to start it and
open its chat, or Ctrl+Enter to start it and stay on home.

The bottom edge of the box shows what the agent will start with: its name,
which agent and model, the folder and the host. Press `ctrl+s` and then a
letter to change one:

| Key | Changes |
|---|---|
| `a` | Claude or Codex |
| `m`, `e`, `p` | The model, the reasoning effort, and the permission |
| `d` | The folder. Your repositories come first, then folders your agents used lately, or type a path. |
| `h` | The host, to start the agent on another of your machines |
| `w` | A new worktree of the folder's repository, so the agent works without touching your checkout |
| `n` | The name, which is optional. Leave it and the agent goes by the title Claude or Codex gives the conversation. |

The models and permissions on offer are whatever that agent offers on the host
you picked. If Claude or Codex isn't signed in there, the picker says so.

A new agent starts on the model, effort and permission Claude or Codex is
already set to on that host, so amux and Claude's own terminal app agree until
you choose otherwise. When you pick a model or effort in amux, for a new agent
or in a chat, later agents on that host start on it too. Permissions never stick: a
permission you pick applies to that agent only. An agent you use in Claude's
own terminal app always follows Claude's settings.

You can also start an agent from the shell, in the current folder:

```sh
amux new claude --prompt "Find out why the login test is flaky"
amux new codex --cwd ~/source/app --name reviewer
```

## Chatting

Press Enter on an agent to open its chat. The header shows the agent's name,
its folder and host, how much of its context it has used, and
`[Diff +42 −7]`: the lines it has changed.

- **Enter** sends your message. While the agent is working, it waits in a
  queue just above the box and goes in when the current turn ends. Press ↑ in
  an empty box to send a queued message straight away, or take it back.
- **Ctrl+X** interrupts the turn. The agent stays ready for your next message.
- **Shift+Tab** steps through the agent's modes or permissions, such as
  Claude's plan mode or accept edits. When a permission lets the agent act
  without asking you first, the box gets a heavy yellow border.
- **Ctrl+V** attaches an image or file from the clipboard, in a new agent's
  first message too. A long paste becomes a single chip rather than filling
  the box.
- **Ctrl+O** opens the overview beside the chat: the agent's tasks, its
  background jobs, the files it changed, and how close it is to its usage
  limits.
- **Shift+Enter** or **Ctrl+J** starts a new line.
- **Drag** over text to select it. It's copied when you let go, and the keys
  it offers appear under the box: **R** quotes it in your reply and **Esc**
  lets it go. Any other key lets it go and types as usual.

Text between `**` or `*` shows bold or italic as you type, and a `` ` `` pair
shows as code; the marks stay so you can edit them. A quote collapses once
you move out of it if it's long. Click the count to open it again, or press
Backspace just below it to remove the whole quote. Once sent, your message
renders as markdown, like the agent's replies.

The agent's tool steps fold into one line between its replies, such as
`▸ 3 commands · 2 edits · 4 reads`. Click it to see each step, and click a
step to see its output or its diff.

Your unsent message belongs to the chat, not to the terminal. Close the
terminal, or pick the chat up on your phone, and it's still there.

### The leader key

In a chat, most of amux's own keys come after the leader, **Ctrl+A**. Press it
and pause, and the keys it offers appear above the key line.

| Keys | What they do |
|---|---|
| Ctrl+A, then `s` | Back to home |
| Ctrl+A, then `n` | A new agent in the same folder, on the same host |
| Ctrl+A, then `r` | Review the agent's changes |
| Ctrl+A, then `a` | Switch to the agent's own interface, for an agent on this machine |
| Ctrl+A, then `d` | Leave for the shell |
| Ctrl+A, then `?` | Every key |

If Ctrl+A clashes with something you use, choose another `ctrl+<letter>`:

```sh
amux config set keybinds.leader ctrl+b
```

## Answering an agent

When an agent needs you, its question takes over the message box. Choices are
numbered and worded as what will happen, such as `1. Yes`,
`2. Yes, and always allow cargo test in this project` and `3. No`. Press a
number, or move to a choice and press Enter. Where the agent takes a reason
with a refusal, Tab on `No` adds a note.

For half a second after a question appears, your keys still go to what you
were typing, so a key you'd already started pressing can't answer something
you haven't read.

A plan offers to start building, to start building and switch to a mode that
asks less, or to keep planning with a note. Questions with several parts come
one at a time and end on a review of your answers.

## Reviewing changes

Press Ctrl+A then `r`, or click `[Diff]` in the header, to open the review: the
files the agent changed on the left, and every change on the right.

| Key | What it does |
|---|---|
| `j` `k` | Next or previous line |
| `]` `[` | Next or previous file |
| `c` | Comment on the line, or on the whole file from its heading |
| `q` | Back to the chat |

Your comments go into the message box as one review, ready to send to the
agent with anything else you want to say.

The header can count either your uncommitted changes, or everything since the
branch left its base. Ctrl+A then `c` switches between the two.

## Chatting in each agent's own interface

If you'd rather use Claude Code's or Codex's own screen, tell amux:

```sh
amux config set ui.chat_in native
```

New agents then start in their own interface, and opening one hands your
terminal to it. Inside it, Ctrl+A then `d` leaves for the shell, and Ctrl+A then
`s` brings home up over it. Everything else you type goes to the agent.

An agent's own interface only opens on the machine it runs on. Agents on your
other machines, and agents started from your phone, open in amux's chat
wherever you are.

## Settings

Press `,` on home for settings:

- this machine's name, as your other devices see it
- whether amux starts when you log in
- whether amux keeps this machine awake while it's plugged in
- your repositories, the folders offered first when you start an agent
- where you chat with agents
- whether selecting text copies it; turned off, press **C** to copy
- usage counts and automatic reports ([What amux sends](../understand/what-amux-sends.md))
- your account: who's signed in, and on which plan

These settings belong to the machine, so your phone and every other terminal
see a change at once. Copying when you select is the exception: it applies to
every terminal on this machine and doesn't affect your phone.

A few more live only in the config file, `~/.config/amux/config.toml`.
`amux config edit` opens it.

```toml
[ui]
theme = "terminal"    # terminal (your terminal's own colours), dark or light
```

To go back to following Claude or Codex after picking in amux, forget the
remembered value: `amux config unset new_agent.claude.model`, and likewise
`new_agent.claude.effort`, `new_agent.codex.model` and `new_agent.codex.effort`.

## Commands

Everything the app does is also a command. Name an agent by its name, by
`name@host` when agents on two hosts share a name, or by the short id
`amux ls` prints.

| Command | What it does |
|---|---|
| `amux ls` | List your agents, subagents beneath the agent that started them |
| `amux new claude` or `amux new codex` | Start an agent in this folder |
| `amux send <agent> <message>` | Send an agent a message |
| `amux attach <agent>` | Open an agent's chat in this terminal |
| `amux settle <agent>` | You're done with an agent: stops it if it's running, and settles its subagents. It can be resumed later. |
| `amux resume <agent> [message]` | Start an exited or settled agent again |
| `amux rename <agent> <name>` | Rename an agent |
| `amux rm <agent>` | Delete an agent, its history and its subagents |
| `amux pair [target]` | Pair with another machine, or open this one for pairing ([Add another computer](../get-started/add-another-computer.md)) |
| `amux hosts` | List your paired machines, and ones you could pair |
| `amux unpair <host>` | Stop trusting a machine |
| `amux login`, `amux logout` | Sign this machine in or out of your amux account |
| `amux status` | Whether amux is running, its version, and each account |
| `amux update` | Install the latest release now ([Updates and channels](updates.md)) |
| `amux config get`, `set`, `unset`, `edit` | Read and change settings |
| `amux server start`, `amux server stop` | Start or stop amux. Stopping it leaves your agents running. |
| `amux dump` | Save a report for troubleshooting ([Troubleshooting](troubleshooting.md)) |
| `amux uninstall` | Remove amux from this machine |

`amux --help`, or `--help` after any command, lists the rest.
