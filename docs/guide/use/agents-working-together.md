---
title: Agents working together
description: Let your agents message each other and start subagents of their own, with the model, permission and worktree each task needs, on any of your computers.
sidebar:
  order: 5
---

Every agent amux runs, Claude or Codex, can see the rest of your fleet and work
with it. An agent can message another agent, start subagents of its own on any of
your paired computers, and hear back when they're done. You don't set anything
up. Ask for it in plain words.

> Start a Codex agent on studio to review this branch, and tell me what it
> finds.

> Ask the agent working on the API what the new endpoint returns, then update
> the client to match.

> Split the migration into three parts and give each one to its own subagent.
> Bring their changes together when they're all done.

## What an agent can do

amux gives every agent a small set of tools:

- **See the fleet**: every agent on your computers, what each is working on,
  which model it runs, and whether it's busy or needs you.
- **See your computers**: the computers you've paired, which are online, and
  the agents, models and permissions each one offers.
- **Message another agent**: by name, on any of your computers. The message
  arrives labelled with who sent it, so the agent knows it isn't from you, and
  it can reply the same way.
- **Start a subagent**: a new Claude, Codex or other agent with a task, on this
  computer or another one you've paired, with the model and effort the task
  needs, in its own worktree if it asks for one.
- **Settle its own subagents**, and only its own.
- **Say what it's working on**, so you and the other agents can find the
  right one to ask.
- **Show you a file it made**, such as a screenshot or a report, as an image or
  a file in its reply.

An agent only sees computers you've paired yourself. It can't pair a computer,
and it can't reach anything you couldn't.

## Subagents

An agent started by another agent is its subagent. Subagents appear in your fleet
beneath the agent that started them, on your phone and in the terminal, and a
chat shows its agent's family above the messages. A subagent is an ordinary agent
in every other way. You can open its chat, read what it's doing, and message it
yourself.

When a subagent needs you, for example to approve a command, it asks in its own
chat and shows up under **Needs you** like any other agent.

A subagent does one job. When it finishes its task and has nothing left to do,
it exits, and its last message goes back to the agent that started it, which
carries on from there. If that agent sends it more work later, the subagent
starts again where it left off, with everything it knew. Only the agent that
started a subagent can wake it this way, though you can always resume it
yourself.

To start a subagent on another computer, the agent needs a folder on that
computer. If it doesn't know one, it asks you.

A subagent never gets more permission than the agent that started it. By
default it gets the same, and the agent can ask for less, such as read-only
for a review; asking for more is refused. Permission here is one scale for
every kind of agent, from read-only through asking you, to working on its
own, to full access, and amux picks the matching setting in Claude, Codex or
Antigravity. You can still change a subagent's permission yourself
afterwards. The model and effort an agent picks for a subagent apply to that
subagent only, and never change the defaults you've chosen.

An agent can also start a subagent from a copy of its own conversation, so
the subagent knows everything the agent knew so far. That works for a Claude
agent starting a Claude subagent, or Codex starting Codex, on the same
computer.

What an agent is working on is what it said it's working on, or else the
title of its conversation. It's never your prompt, since every agent can read
the fleet.

Claude subagents always run in amux's chat, even if you usually chat in Claude
Code's own interface, so you can read them from any device.

## Claude's and Codex's own subagents

Claude and Codex can also start helpers of their own inside a conversation,
such as Claude's Agent tool. amux shows each one as a subagent with its own
chat, under the agent that started it, so you can watch what it does and
answer its questions from any device. These are run by Claude or Codex, not by
amux: you can't message them or resume them, and they work in their parent's
folder. Where Claude or Codex allows it, you can stop one from its chat, and
the agent that started it carries on without it.

## Settling and deleting

Settling or deleting an agent does the same to its subagents, and to their
subagents in turn, on every computer. A subagent on a computer that's offline
follows as soon as that computer is back.

With the `workspaces` experiment on, deleting the last agent in a worktree
amux made removes the worktree too. Before you confirm, the terminal and the
iPhone and Mac apps ask the agent's computer what the delete takes with it,
and say they're checking until it answers. When the worktree goes, they say
so, with how many files have uncommitted changes and what becomes of its
branch: it keeps its commits, or, when it has none that another branch
doesn't hold, it goes too, so a branch you've merged elsewhere goes.
Confirming discards the changes they named. If the computer can't be asked,
they say so, and a worktree with changes stays. A
worktree whose changes weren't shown to you stays on disk, no longer amux's,
and so does one you delete from an older app or with `amux rm` without
`--discard-changes`. A worktree that other agents still use stays until the
last of them goes; its subagents count as going with it.
