---
title: Other agents
description: Run Antigravity, OpenCode, Grok or an agent of your own beside Claude and Codex.
sidebar:
  order: 5
---

Besides Claude and Codex, amux runs Google's Antigravity, OpenCode and xAI's
Grok. It can also run any other agent that speaks the Agent Client Protocol
(ACP), the protocol editors such as Zed use to talk to coding agents.

## The agents amux knows

| Agent | Its id | What amux runs |
|---|---|---|
| Antigravity | `antigravity` | `agy acp` |
| OpenCode | `opencode` | `opencode acp` |
| Grok | `grok` | `grok agent stdio` |

A computer offers one of these once the agent is installed there, so that
amux finds its program on the `PATH` it starts agents with. amux never
installs an agent for you. Sign in to each agent its own way before you start
one, for example with `opencode auth login`. An agent started before you signed
in waits and says it isn't signed in. Whatever you send it waits too. Sign in,
then send it a message, and it starts and answers everything in order.

New Agent, in the terminal and in the Mac and iPhone apps, lists every agent
the chosen computer offers, by name, your own among them. An agent that isn't
signed in there says so, with what to do. Until you've had a conversation with
an agent on that computer, amux doesn't know its models yet: it starts on its
own model and settings. From the command line, give the agent's id to
`amux new`:

```sh
amux new opencode --prompt "Find where we parse the config and explain it"
```

`--model` and `--effort` pick from what the agent offers. `--permission`
picks one of the agent's own modes, such as `plan`. Leave them out and the
agent starts with its own settings. A model or effort you change in its chat
becomes what that agent starts with next time on that computer, kept under its
id (`amux config unset new_agent.opencode.model` forgets it); a mode never
sticks. These agents have no interface of their
own, so `amux attach` opens them in amux's chat.

## Add your own agent

To add an agent amux doesn't know, give it an entry in the config file,
`~/.config/amux/config.toml`. `amux config edit` opens it.

```toml
[agents.my-agent]
name = "My Agent"             # what amux calls it; its id otherwise
command = "my-agent"          # a program on the PATH, or a full path
args = ["--acp"]              # what makes it speak ACP on its input and output
env = { MY_AGENT_LOG = "1" }  # added to the environment it starts with
```

The key after `agents.` is the agent's id. You give that id to `amux new my-agent`.
An id is lowercase letters, digits, hyphens and underscores. `claude` and
`codex` are taken. An entry needs a `command`. The rest is optional.

An entry under one of the ids above changes only that agent's fields you
set. For example, this runs an OpenCode installed somewhere else:

```toml
[agents.opencode]
command = "/opt/opencode/bin/opencode"
```

A change applies to the next agent you start or resume. An agent that's
already running keeps going as it started.

If an entry can't be used, because it has no command, a value of the wrong
type or a taken id, amux skips that entry and carries on. The reason goes
in amux's log and is printed by `amux config edit`.

## What's different about these agents

An agent's modes, such as OpenCode's build and plan, stand where Claude's
permissions do. Shift+Tab steps through them, and in the terminal `ctrl+s`
then `p` opens the list. An agent without modes, such as Grok, has nothing
to step. When an agent asks before running a command or editing a file, you
get exactly the choices it offers.

A message you send while an agent works waits its turn, and you can withdraw
it until then. Only Antigravity takes a waiting message into the turn it's
running, so only Antigravity offers Send Now. When an agent asks you a
question with set answers, you pick one, or several where the agent allows.

Some agents can't reopen a conversation in a new process. Such an agent
never suspends. When its process ends, for example because the computer
restarts, the agent has ended for good, and amux won't resume it. Start a
new one instead.
