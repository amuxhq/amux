---
title: Agents working together
description: Let your agents message each other and start subagents of their own, Claude or Codex, on any of your machines.
sidebar:
  order: 3
---

Every agent amux runs, Claude or Codex, can see the rest of your fleet and work
with it. An agent can message another agent, start subagents of its own on any of
your paired machines, and hear back when they're done. You don't set anything
up. Ask for it in plain words.

> Start a Codex agent on studio to review this branch, and tell me what it
> finds.

> Ask the agent working on the API what the new endpoint returns, then update
> the client to match.

> Split the migration into three parts and give each one to its own subagent.
> Bring their changes together when they're all done.

## What an agent can do

amux gives every agent a small set of tools:

- **See the fleet**: every agent on your machines, what each is working on,
  and whether it's busy or needs you.
- **See your hosts**: the machines you've paired, and which are online.
- **Message another agent**: by name, on any of your machines. The message
  arrives labelled with who sent it, so the agent knows it isn't from you, and
  it can reply the same way.
- **Start a subagent**: a new Claude or Codex agent with a task, on this machine
  or another one you've paired.
- **Settle its own subagents**, and only its own.
- **Say what it's working on**, so you and the other agents can find the
  right one to ask.
- **Show you a file it made**, such as a screenshot or a report, as an image or
  a file in its reply.

An agent only sees machines you've paired yourself. It can't pair a machine,
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

To start a subagent on another machine, the agent needs a folder on that
machine. If it doesn't know one, it asks you.

Claude subagents always run in amux's chat, even if you usually chat in Claude
Code's own interface, so you can read them from any device.

## Settling and deleting

Settling or deleting an agent does the same to its subagents, and to their
subagents in turn, on every machine. A subagent on a machine that's offline
follows as soon as that machine is back.
