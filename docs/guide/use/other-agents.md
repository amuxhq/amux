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
one, for example with `opencode auth login`.

For now you start these agents from the command line. The apps can't start
them yet. Give the agent's id to `amux new`:

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

Some agents can't reopen a conversation in a new process. Such an agent
never suspends. When its process ends, for example because the computer
restarts, the agent has ended for good, and amux won't resume it. Start a
new one instead.
