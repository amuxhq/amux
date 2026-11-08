---
title: Your first agent
description: Start a Claude Code or Codex agent, find it in your list, and answer it when it asks for you.
sidebar:
  order: 2
---

An agent is a Claude Code or Codex session that amux runs for you on one of
your computers. It keeps working when you close the terminal or put your
phone away, and you can pick it up again from any device you've paired.

## Start one

**From your phone.** Tap **+**, pick a repository, and say what the agent
should work on. The agent runs on your computer, not on the phone; the phone
is just where you're steering it from.

**From the terminal.** Run `amux` and press **n** (or choose
**+ New Agent**). Type what the agent should work on and press **Enter**: the
agent starts and its chat opens. **Ctrl+Enter** starts it and stays on the
home screen.

The bottom edge of the box shows what the agent will start with:

```text
+ Name │ Claude · Opus (high) · ask │ ~/source/amux │ laptop
```

That's its name, which agent with its model, effort and permission, the
folder, and the computer it runs on. Press **Ctrl+S** and then a letter to
change one:

| Key | Changes |
| --- | --- |
| **a** | the agent: Claude or Codex |
| **d** | the folder |
| **h** | the computer it runs on |
| **w** | a new worktree, so it works without touching your checkout |
| **m**, **e**, **p** | the model, the effort, the permission |
| **n** | the name, which is optional: leave it and the agent goes by the title Claude or Codex gives the conversation |

**From the command line.** In the folder you want it to work in:

```sh
amux new claude --prompt "find out why the login test is flaky"
amux new codex
```

## Find it

The home screen lists your agents in three sections:

- **Needs you:** agents waiting on an answer from you.
- **Live:** agents starting, working, or idle and ready for more.
- **Exited:** agents that have finished. It's folded until you open it.

Each row shows what the agent is called (the title of its conversation, or the name if you gave one) and its branch, which agent it is, its folder,
the computer it's on when that isn't this one, and how long it's been that
way. A filled **●** means it's working, and in blue it means it needs you.
The line underneath says what it's doing now or what it's asking.

Press **Enter** on a row to open its chat. The phone shows the same list on
its **Agents** tab.

## Answer it

When an agent wants your permission to run a command or edit a file, has a
plan ready, or has a question, it moves to **Needs you**. Open its chat and
the question takes the place of the message box, with numbered choices such
as `1. Yes`, `2. Yes, and always allow cargo test in this project` and `No`.
Press the number, or move to a choice and press **Enter**. **Esc** never
answers for you.

On the phone, the same question appears in the chat; tap your answer. An
agent waiting on you can be answered from whichever device is closest.

To send an agent more work, type in its chat and press **Enter**.

## Settle, resume and tidy up

On the home screen, **x** settles the highlighted agent: you're done with it,
so it stops if it's running and moves to **Settled**. It asks first if that
would cut work short. **x** on a settled agent deletes it, after asking. A
settled agent can be resumed later with `amux resume <name>`. `amux ls` lists your agents from the command line.

## Prefer Claude Code's own interface?

amux shows every agent in its own chat by default. If you'd rather work in
Claude Code's or Codex's own terminal interface, change **where you chat with
agents** in Settings (**,** on the home screen). New agents on this computer
then open in their own interface, and **a** on the home screen attaches to
one. The phone, and other computers, still show those agents in amux's chat.

## Next

- [Connect your phone](connect-your-phone.md), if you skipped it while
  setting up.
- [The terminal app](../use/terminal.md) for every screen and key.
- [Agents working together](../use/agents-working-together.md): agents that
  start subagents of their own, on this computer or another.
