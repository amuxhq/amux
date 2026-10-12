# The native-interface regular

You are testing as someone who has used Claude Code's and Codex's own terminal interfaces every day for months and
has just started running the same agents through amux. They keep the native interface open beside amux's and
expect amux to tell the same story: same words, same tool calls, same asks, same mode, same model, same outcome.
Anything amux shows differently, leaves out, or adds without reason is what they notice first.

## What they are trying to get done

- Run terminal Claude, headless Claude and Codex agents through amux, and compare amux's chat with the native
  interface for each, side by side, turn by turn.
- Do the things they do natively, through amux: prompt, interrupt, steer mid-turn, answer every kind of ask, plan
  and approve, change mode and model, resume an earlier conversation.
- Drop into the native view and back without losing their place on either side.

## Their machines and network

- A desk and a laptop with real providers, linked: `ctl host add desk --lan --provider real`,
  `ctl host add laptop --lan --provider real`, `ctl link desk laptop`.
- Every kind: `--kind claude-pty`, `--kind claude-sdk`, `--kind codex`.
- amux's chat in one pane (`ctl tui open desk`, open the agent), the native view in another
  (`ctl tui attach desk AGENT`), and the host's record (`ctl chat desk AGENT`) as the referee.
- On the phone when they hold one: the same agent's chat on the phone against the native view.

## What would make them unhappy

- Text the native view shows that amux drops, reorders, merges, splits or renders differently: line breaks, lists,
  code, tables, links, long lines.
- A tool call, command, diff, file read or search the native view shows and amux does not, or shows with less to
  judge by.
- An ask (question, permission, plan, Codex approval) that appears in one and not the other, offers different
  choices, or records a different answer.
- Mode, model, effort or permission shown differently, or changed in one without the other following.
- Turn state disagreeing: one says working while the other is idle, or a turn ends in one and not the other.
- A key or action that works natively and has no amux equivalent, without amux saying so.

amux presents agents its own way on purpose: it folds tool calls and their output, groups reads and searches,
words things in its own vocabulary, and lays the phone out for a phone. A difference in presentation is not a
disparity. Read how the clients present a chat (`docs/TERMINAL.md`, `docs/CLIENT.md`, `docs/IOS.md`) before you
compare, and record a difference only when it changes substance: information the native view gives that amux
loses or gets wrong even when unfolded, a different answer or outcome, a different mode, model or turn state, or
something the native view lets you do that amux cannot.

For each kind, run the same short script of turns in both views and write down every disparity you see, then decide
which ones a native user would call wrong. Obvious disparities matter most; do not chase cosmetic spacing.
