---
title: Terminal app
description: Run, watch and talk to every agent on every device from one terminal, and the amux commands worth knowing.
sidebar:
  order: 1
---

Run `amux` with nothing after it and the terminal app opens: every agent on
every device you've paired, and a chat with any of them. If amux isn't
running yet, the app starts it first.

```sh
amux                     # open the terminal app
amux --profile work      # open it on another profile
```

Press `?` at any time to see every key.

On Windows the terminal app isn't supported: use the desktop app there. It
still opens, but Windows' console has no bracketed paste, so a pasted
message with several lines is sent at its first line break, and Ctrl+Enter
and Shift+Enter arrive as Enter. The commands at the end of this page work
on Windows as they do elsewhere, apart from `amux attach`, which opens the
same terminal chat.

## Home

Home is your fleet. `+ New Agent` sits at the top, and the agents are below it
in four sections:

- **Pinned**: agents you pinned, kept at the top.
- **Needs you**: agents waiting on you, to approve a command, answer a
  question or decide on a plan.
- **Live**: agents starting, working, or idle and ready for the next prompt,
  and agents that have stopped but you haven't settled yet.
- **Settled**: agents you're done with. This section starts folded.

Each row shows the agent's name, its branch, which agent it is (Claude or
Codex), its folder and, when it isn't this device, its device. The line under
the name says what's going on: the question it's asking, the step it's
running, what it last said, or why it can't carry on. A stopped or settled
agent keeps the last thing it said there, unless it failed, when the line
says why. A filled `●` means it's
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
| `/` | Filter by name, branch, folder or device |
| `r` | Rename the agent. Its new name replaces any title. |
| `x` | Settle the agent, asking first if that would cut work short or end a conversation that can't be resumed; on a settled agent, delete it, asking first |
| `p` | Pin or unpin the agent |
| `h` | Your devices, and how each one is reached: `direct`, `relayed` or `ssh`, or `away` or `offline` |
| `,` | Settings |
| `b` | Report a problem |
| `q` | Leave for the shell. Your agents keep running. |

## Starting an agent

Press `n`. Type what the agent should work on, and press Enter to start it and
open its chat, or Ctrl+Enter to start it and stay on home.

The bottom edge of the box shows what the agent will start with: its name,
which agent and model, the folder and the device. Press `ctrl+s` and then a
letter to change one:

| Key | Changes |
|---|---|
| `a` | The agent: Claude, Codex, or another agent set up on that device, such as OpenCode |
| `m`, `e`, `p` | The model, the reasoning effort, and the permission |
| `d` | The folder. Your repositories come first, then folders your agents used lately, or type a path. |
| `h` | The device, to start the agent on another of your devices |
| `w` | A new worktree of the folder's repository, so the agent works without touching your checkout |
| `n` | The name, which is optional. Leave it and the agent goes by the title the agent gives the conversation. |

The models and permissions on offer are whatever that agent offers on the device
you picked. If an agent isn't signed in there, the picker says so, with what to
do about it. Another agent starts in its own mode unless you pick one with
Shift+Tab. See [other agents](other-agents.md) to add one.

A new agent starts on the model, effort and permission Claude or Codex is
already set to on that device, so amux and Claude's own terminal app agree until
you choose otherwise. When you pick a model or effort in amux, for a new agent
or in a chat, later agents on that device start on it too. Permissions never stick: a
permission you pick applies to that agent only. An agent you use in Claude's
own terminal app always follows Claude's settings.

You can also start an agent from the shell, in the current folder:

```sh
amux new claude --prompt "Find out why the login test is flaky"
amux new codex --cwd ~/source/app --name reviewer
amux new claude --worktree --name fix-scroll
```

`--worktree` is the `w` key: the agent starts in a new worktree of the
folder's repository, on a new branch named after it, and amux says where the
worktree is.

## Chatting

Press Enter on an agent to open its chat. The header shows the agent's name,
its folder and device, how much of its context it has used, and
`[Diff +42 −7]`: the lines it has changed.

- **Enter** sends your message. While the agent is working, it waits in a
  queue just above the box and goes in when the current turn ends. Press ↑ in
  an empty box to send a queued message straight away, where the agent can
  take one mid-turn, or take it back.
- **Ctrl+X** interrupts the turn. The agent stays ready for your next message.
- **Shift+Tab** steps through the agent's modes or permissions, such as
  Claude's plan mode or accept edits. When a permission lets the agent act
  without asking you first, the box gets a heavy yellow border.
- **Ctrl+V** attaches an image or file from the clipboard, in a new agent's
  first message too. An agent that can't take images or files says so at once. A long paste becomes a single chip rather than filling
  the box.
- **Ctrl+O** opens the overview beside the chat: the agent's tasks, its
  background jobs, the files it changed, and how close it is to its usage
  limits.
- **/** at the start of a message lists the agent's commands, such as
  `/compact` or `/review`, with what each does. Keep typing to narrow the
  list, move with ↑ and ↓, and press Enter to run one or Tab to fill it in
  and add to it. A command that needs something after its name, such as
  `/rename <name>`, is filled in for you to finish. Esc hides the list. In
  a Codex chat, **$** lists its skills the same way.
- **Shift+Enter** or **Ctrl+J** starts a new line.
- **Drag** over text to select it. It's copied when you let go, and the keys
  it offers appear under the box: **R** quotes it in your reply and **Esc**
  lets it go. Any other key lets it go and types as usual.

Text between `**` or `*` shows bold or italic as you type, and a `` ` `` pair
shows as code; the marks stay so you can edit them. A quote collapses once
you move out of it if it's long. Click the count to open it again, or press
Backspace just below it to remove the whole quote. Once sent, your message
renders as markdown, like the agent's replies.

Code blocks in the agent's replies, and the diff it asks permission to make,
are coloured as an editor colours them when they're in a common language:
Rust, Swift, TypeScript, JavaScript, Python, Go, C, C++, Java, shell, HTML,
CSS, JSON, YAML, TOML and Markdown. Code in any other language shows in one
colour. In a terminal limited to 16 colours, a diff's added and removed lines
stay in the diff's own green and red, so they still read as changes.

Attachments in the chat are underlined: click one to open it, or focus its
message with the leader and `j` or `k` and press the leader then `o`. Pictures,
PDFs and plain text open in their usual app, and anything else is shown in its
folder so you can decide what to do with it. In a terminal that can draw images,
such as Ghostty, kitty or iTerm2, an image also shows a thumbnail, including
inside tmux, where amux turns on `allow-passthrough` for its own pane.

The agent's tool steps fold into one line between its replies, such as
`▸ Read 4 files, edited 2 files, ran 3 commands`. Click it to see each step,
and click a step to see its output or its diff. While the agent works, you see
the step it's on now under a line of what it has done so far. When Claude says
what a command is for, the step reads as Claude's words, with the command
itself faint underneath.

### Subagents

When Claude or Codex hands part of the work to a subagent of its own, such as
Claude's Agent tool, the subagent gets its own chat. In the agent's chat the
call that started it is one line saying how the subagent is doing right now:
`● Running subagent Survey the redraw paths`, `waiting on you` in blue when it
has a question for you, or `Ran subagent … · 1m 35s` once it's done.
Subagents started together share one line, such as
`▸ 4 subagents · 2 running · 1 waiting on you · 1 done`. Click it, or focus it
and press Ctrl+A then `o`, to list each one.

Click a subagent's line, or focus it with Ctrl+A then `j` or `k` and press
Ctrl+A then Enter, to open its chat. There you can read everything it did and
answer its questions, which show up there and under **Needs you** on home.
You can't message a subagent: Claude or Codex talks to it for you, and the
box where you'd type says so. To stop it, press **Ctrl+X**, then Enter to
confirm. The agent that started it carries on without it. Claude's own
terminal app doesn't let amux stop a single subagent, so there it isn't
offered.

On home, subagents are listed under the agent that started them, by what they
were asked to do. They go with that agent: settling it settles them.

Your unsent message belongs to the chat, not to the terminal. Close the
terminal, or pick the chat up on your phone, and it's still there.

### The leader key

In a chat, most of amux's own keys come after the leader, **Ctrl+A**. Press it
and pause, and the keys it offers appear above the key line.

| Keys | What they do |
|---|---|
| Ctrl+A, then `s` | Back to home |
| Ctrl+A, then `n` | A new agent in the same folder, on the same device |
| Ctrl+A, then `r` | Review the agent's changes |
| Ctrl+A, then `a` | Switch to the agent's own interface, for an agent on this device |
| Ctrl+A, then `x` | Settle the agent and go home, asking first if that would cut work short or end a conversation that can't be resumed |
| Ctrl+A, then `o` | Open or close the focused line: a step, a run of steps, or a group of subagents |
| Ctrl+A, then Enter | Open the chat of the subagent the focused line started |
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
with a refusal, Tab on `No` adds a note. Agents other than Claude and Codex
offer their own choices, such as `Always allow`, and take a note on `No` or on
`Decline` as what to do instead.

For half a second after a question appears, your keys still go to what you
were typing, so a key you'd already started pressing can't answer something
you haven't read. In a subagent's chat, which has nothing to type in, those
keys do nothing.

A plan offers to implement it, to implement it and switch to a mode that
asks less, or to keep planning with a note. Questions with several parts come
one at a time and end on a review of your answers.

## Reviewing changes

Press Ctrl+A then `r`, or click `[Diff]` in the header, to open the review; a
file in the overview opens it at that file. Reviews need the reviews
experiment on the agent's device; [Reviewing changes](reviews.md) explains
how to switch it on, and how comments, rounds and **Since your last review**
work.

The review holds still while you read: if the agent changes code meanwhile,
the page says so and `r` refreshes it.

| Key | What it does |
|---|---|
| `j` `k` | Next or previous line |
| `J` `K` | Next or previous change |
| `]` `[` | Next or previous file |
| `c` or Enter | Comment on the line; `c` on a file's heading comments on the whole file |
| `v`, or Shift+↓ ↑ | Select several lines, then `c` to comment on all of them |
| Enter on a draft | Change its words; empty them to delete it |
| `d` | Delete the comment under the cursor |
| Enter on a file's heading, or `f` | Fold or open the file |
| Enter or `e` on hidden lines | Show more lines above or below |
| Tab | Switch between All changes and Since your last review |
| `r` | Refresh to the code as it is now |
| `D` | Discard your draft comments, asking first |
| `S` | Start the review over, asking first |
| `q` | Back to the chat |

While you have drafts, the message box shows `[review · 3 comments ✕]`:
whatever you send next carries them to the agent, with your message as the
summary. Click the ✕ to discard them instead. When the agent has changed
code since you wrote them, the chip adds `written on older code`
([what that means](reviews.md#written-on-older-code)).

A review you sent shows in the chat as `[review · 3 comments]`. Click it, or
focus its message and press Ctrl+A then `o`, to see that round again
([Look back at a round](reviews.md#look-back-at-a-round)); Enter on a file
folds it, and `q` goes back to the chat.

The header can count either your uncommitted changes, or everything since the
branch left its base. Ctrl+A then `c` switches between the two. For an agent
working on the base branch itself, everything on the branch is what isn't
pushed yet: its commits since the upstream, and its uncommitted changes. The
count is of the agent's whole folder, so agents sharing one checkout count
the same changes: it updates when any of them finishes a turn.

## Chatting in each agent's own interface

If you'd rather use Claude Code's or Codex's own screen, tell amux:

```sh
amux config set ui.chat_in native
```

New agents then start in their own interface, and opening one hands your
terminal to it. Inside it, Ctrl+A then `d` leaves for the shell, and Ctrl+A then
`s` brings home up over it. Everything else you type goes to the agent.

An agent's own interface only opens on the device it runs on. Agents on your
other devices, and agents started from your phone, open in amux's chat
wherever you are.

## Settings

Press `,` on home for settings:

- this device's name, as your phone and other devices see it
- whether amux starts when you log in
- whether amux keeps this device awake while it's plugged in
- your repositories, the folders offered first when you start an agent
- where you chat with agents
- whether selecting text copies it; turned off, press **C** to copy
- usage counts and automatic reports ([What amux sends](../understand/what-amux-sends.md))
- your account: who's signed in, and on which plan
- experiments: features still taking shape, which you can try before they
  settle. Each is on or off for the whole device. When you switch one, amux
  restarts itself to apply it, a few seconds later, or once it has finished
  making a worktree or running a worktree's hook; the switch says
  `applying…` until then. Your agents keep running, as they do whenever
  amux restarts. An experiment may change or go in any release, and what it
  keeps may be lost when amux updates.

  With `workspaces` on, a new worktree is a [wt](https://github.com/jordanwebster/wt)
  tree when the repository uses wt, and its agent runs in the tree's
  environment. The agent appears at once and starts when its worktree is
  ready. A name the repository's branches already have gets a suffix
  (`fix-2`) instead of failing. When you delete a worktree's last agent,
  amux removes the worktree. The confirmation lists its uncommitted changes,
  which confirming discards; otherwise a worktree with changes stays on disk
  ([Settling and deleting](agents-working-together.md#settling-and-deleting)).

  With `suspension` on, an agent you started yourself that has sat idle
  for 30 minutes closes its Claude or Codex to free the device. It still
  shows as idle, and starts again on its own when a message reaches it,
  including a report from an agent it started, or when you open its
  terminal. It doesn't suspend while it's working, waiting for agents it
  started, running something in the background, holding a wake-up it
  scheduled or a permission you gave it for the session (a change of
  permission mode, approving a plan included, doesn't count: it wakes up in
  the mode it was in), or while a
  terminal is open on it. A long conversation is first compacted while
  the provider still has it cached, so it isn't read again in full when you
  come back: with the 30-minute delay, a Codex agent compacts after 25
  minutes and still suspends at 30, and a headless Claude agent compacts
  after 55 minutes and suspends right after. After the device restarts, agents that were idle
  stay that way until something reaches them, and one that was working
  carries on where it left off; its chat marks where with "Resumed · after
  the device restarted".

  With `reviews` on, you can review an agent's changes, comment on its lines
  and send the comments back round by round
  ([Reviewing changes](reviews.md)).

These settings belong to the device, so your phone and every other terminal
see a change at once. Copying when you select is the exception: it applies to
every terminal on this device and doesn't affect your phone.

A few more live only in the config file, `~/.config/amux/config.toml`.
`amux config edit` opens it.

```toml
[ui]
theme = "terminal"    # terminal (your terminal's own colours), dark or light

[experiments]
enabled = ["workspaces"]   # the experiments switched on, by name

[experiments.suspension]
after = "1h"               # how long an agent sits idle before it suspends
```

Agents other than Claude and Codex, the ones amux knows and ones you add,
are set up in the same file under `[agents.<id>]`; see
[Other agents](other-agents.md).

Switching experiments with `amux config set experiments.enabled
'["workspaces"]'`, or by editing the file, restarts amux the same way.
`amux config` reads and changes an experiment's settings by the same names:
`amux config set experiments.suspension.after 1h`, then
`amux config get experiments.suspension.after`. A name amux doesn't have is
refused with the ones it does. While the experiment is on, a change to its
settings restarts amux the same way, and its switch says `applying…` until
then; a value it can't use is ignored with a warning, so it keeps its
default and nothing restarts.

To go back to following Claude or Codex after picking in amux, forget the
remembered value: `amux config unset new_agent.claude.model`, and likewise
`new_agent.claude.effort`, `new_agent.codex.model` and `new_agent.codex.effort`.
Another agent's picks are kept under its id, as `new_agent.<id>.model` and
`new_agent.<id>.effort`, for example `new_agent.opencode.model`.

## Commands

Everything the app does is also a command. Name an agent by its name, by
`name@host` when agents on two devices share a name, or by the short id
`amux ls` prints.

| Command | What it does |
|---|---|
| `amux ls` | List your agents, subagents beneath the agent that started them, each with the agent it runs (`claude`, `codex`, `opencode`, …) |
| `amux new claude`, `amux new codex`, or `amux new <agent>` for [another agent](other-agents.md) | Start an agent in this folder; `--worktree` starts it in a new worktree of the folder's repository |
| `amux send <agent> <message>` | Send an agent a message |
| `amux attach <agent>` | Open an agent's chat in this terminal |
| `amux settle <agent>` | You're done with an agent: stops it if it's running, and settles its subagents. It can be resumed later. |
| `amux resume <agent> [message]` | Start a stopped or settled agent again |
| `amux rename <agent> <name>` | Rename an agent |
| `amux rm <agent>` | Delete an agent, its history and its subagents; `--discard-changes` also removes a worktree that goes with it when it has uncommitted changes |
| `amux pair [target]` | Pair with another device, or open this one for pairing ([Add another device](../get-started/add-another-computer.md)) |
| `amux devices` | List what you've paired, and devices you could pair |
| `amux unpair <device>` | Unpair a device or phone |
| `amux login`, `amux logout` | Sign this device in or out of your amux account |
| `amux status` | Whether amux is running, its version, and each account |
| `amux update` | Install the latest release now ([Updates and channels](updates.md)) |
| `amux config get`, `set`, `unset`, `edit` | Read and change settings |
| `amux server start`, `amux server stop` | Start or stop amux. Stopping it leaves your agents running. |
| `amux dump` | Save a report for troubleshooting ([Troubleshooting](troubleshooting.md)) |
| `amux uninstall` | Remove amux from this device |

`amux --help`, or `--help` after any command, lists the rest.
