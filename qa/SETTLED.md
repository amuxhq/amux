# Settled behaviour

Everything below is how amux is meant to behave. It is decided, so do not
report it as a finding. Report amux when it does not behave this way, or when
the behaviour hurts a person in a way this list does not cover.

This describes amux once the current round of fixes has landed. Where the build
you are testing still behaves otherwise, that difference is a finding.

## Who sees which agents

- A phone sees the agents of the hosts it is paired with, and only those. An
  agent on a host linked to the phone's host, but not paired with the phone,
  is not listed on the phone. Pair the phone with that host to see its
  agents.
- A host's clients show its own agents and those of the hosts it trusts. Once
  two hosts unpair, neither lists the other's agents.

## A test net's daemon grace

- On a test net, agents outlive a stopped or killed daemon by 20 seconds. A
  real installation waits five minutes. Keep a host down for longer and every
  agent on it ends. That is expected, and so is what follows from it:
  - a child that finished meanwhile reads as having exited while its daemon
    was away;
  - a report to a parent that ended this way is dropped;
  - an ask that was open shows as dismissed and is not asked again.

## How amux presents an agent

- amux presents agents its own way, on purpose. It folds tool calls into
  short rows, uses its own words, layout, ask cards and keys, and does not
  copy Claude's or Codex's own interface. A difference in presentation from
  the agent's native view is not a finding. A difference in substance is:
  something missing, a different answer, a choice amux does not offer, a
  state that disagrees.
- An agent's prose is drawn as markdown, so `&amp;` shows as `&`. Single line
  breaks in its prose stay line breaks, on the phone as in the terminal.
- A file path in an edit ask is relative to the agent's folder when the file
  is inside it, and absolute when it is not.
- On the terminal client's home screen, a focused control carries the same
  `›` marker as a focused agent row. Home has no text field: its single
  letters are documented keys, so typing there presses them.
- A refused rename shows its reason on the key line, while the row still
  shows the name being edited.

## Asks and keys

- Asks are answered with amux's own keys (digits, Enter), in the terminal
  client and on the phone. Codex's own shortcuts, such as `y` and `p`, are not
  amux's keys.
- Esc on an ask moves the highlight to the way out (deny, cancel). It does not
  answer the ask.
- An ask that opens while someone is typing takes no key until half a second
  has passed with no key pressed. Every key restarts that half second, and
  the keys typed meanwhile go to the draft. A tester's key landing on an ask
  that opened mid-typing is covered in the agent guide.
- A plan offers three choices: start, start and switch the permission (to
  auto where Claude's menu offers auto mode, to accepting edits otherwise),
  or keep planning. Codex offers start or keep planning.

## Interrupt, stop and delete

- **Interrupt** ends the turn. The agent stays live, and its background work
  (shells, sub-agents) keeps running.
- **Stop** ends the agent and all of its work, now. It is then exited and can
  be resumed. When the agent is working, or has background jobs or
  sub-agents, the confirmation names what will be cut.
- **Delete** removes an exited agent for good. The clients offer Delete only
  on an exited agent: the phone's menu says Stop Agent for a live one, and the
  terminal's `[x]` stops a live agent and deletes an exited one, each with a
  confirmation. `amux delete` on a live agent stops it first.
- Stopping a parent stops all its descendants, on every host it can reach. A
  resumed parent gets no extra report about them.
- An agent's own `stop` tool really stops the agent it names. A message to a
  stopped child resumes it.
- An agent whose only remaining work is in the background reads as idle. The
  agent's row counts its background jobs.

## Families

- A child agent that has started a child of its own stays until that child
  has finished and reported, then reports up as usual.
- A child whose parent is gone finishes its task and exits. Its report is
  lost.

## Names

- An agent's name is unique on its host, counting exited agents. A spawn that
  asks for a taken name gets a suffix (`worker-2`), and the caller is told the
  name it got. A rename to a taken name is refused with the reason.
- A name used on two hosts is told apart as `name@host`. Every id form amux
  prints, full or short, is accepted wherever an agent is named.

## When a host cannot be reached

- Something that certainly never left (a prompt, an ask's answer, a stop) is
  held as pending, with Cancel ("sends when desk is back"), and sent once
  when the host is reachable. If the client closes first, a held prompt goes
  back to the draft (the phone keeps drafts); a held stop or answer is
  dropped.
- Something that may have landed says it "may not have arrived" and offers
  Resend or Discard. It is never resent on its own.
- Rename and delete fail at once, with the reason. They are never held.
- A daemon answers at once with a reason (unreachable, lapsed, refused). It
  never hangs for half a minute first.
- Paired hosts keep redialling each other's last working address while one
  is away, with backoff, whether or not discovery finds it.

## Slash commands and modes

- Claude handles its own slash commands: amux passes `/command args` through.
  Claude's built-in commands run with no model turn. Commands that exist only
  on Claude's own screen exist only in terminal Claude.
- `/model`, `/effort` and `/rename` are amux's own, on both clients. Bare,
  `/model` and `/effort` open amux's picker and `/rename` its rename; with a
  value, each sets it. There is no `/status`.
- For Codex, amux offers `/compact`, `/clear`, `/review`, `/init`, `/plan` and
  skills (`$name`). Any other slash command is refused in the composer.
- `/clear` starts the agent afresh on every kind, marked in the chat as a new
  conversation. amux offers no `/new`.
- A prompt typed while one of terminal Claude's own screens is open (a
  dialog, the transcript view) stays "sent", and the agent stays idle.
- A permission or mode changed in Claude's own view shows in amux at Claude's
  next record, not at once.

## The agent's own view switching conversation

- Codex's own view may switch to another conversation (`/new`). amux keeps
  sending to its own.
- When terminal Claude's own view resumes a different conversation, the chat
  says so ("Claude's own view is on another conversation") with Switch back.
  This is information, not an ask: the agent's phase does not change. Prompts
  are refused with that reason until its own conversation is resumed in
  Claude's view, or Switch back is used.

## Attachments and durations

- An attachment may be up to 64 MB. Both clients refuse a larger one before
  uploading it, in words naming the limit.
- A message one agent sends another can carry attachments.
- A command's duration counts from when it began running, after any
  approval it waited for.
