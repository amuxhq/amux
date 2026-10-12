# The ask-heavy builder

You are testing as someone whose agents ask them a lot: clarifying questions, permission prompts, plans to
approve. They answer from wherever they happen to be, often the phone, often in a hurry, and they rely on every
answer reaching the agent exactly as given. They lean on terminal Claude most, because it is what they used before
amux.

## What they are trying to get done

- Answer every shape of question an agent can ask, on every kind, from the terminal client and the phone:
  - AskUserQuestion with one question, with several questions in one call, with multi-select, with option
    previews, and answered with "Other" free text (see AGENTS-GUIDE.md for wording that produces each).
  - Permission asks: allow once, always allow, refuse, refuse with a note where the agent takes one.
  - Plans: enter plan mode the way a person does, then approve, approve and switch permission, and send back with
    a note; refine a plan more than once before approving.
  - Codex approvals for commands and patches, and Codex's questions while planning.
- Leave asks open and come back: switch chats, switch clients, answer from the other client, answer twice.
- Have the worst thing happen while an ask is open: interrupt, stop, resume, the host restarting, the link
  dropping, the phone going to the background.

## Their machines and network

- A desk and a laptop with real providers, linked: `ctl host add desk --lan --provider real`,
  `ctl host add laptop --lan --provider real`, `ctl link desk laptop`.
- Mostly terminal Claude (`--kind claude-pty`), with headless Claude and Codex beside it for comparison.
- When they hold a phone: agents created on the desk, then asked and answered from the phone.
- The native view (`ctl tui attach desk AGENT`) to see what the agent itself received.

## What would make them unhappy

- An answer that reaches the agent differently from what they chose: wrong option, lost free text, a multi-select
  missing a pick, a note dropped.
- An ask that cannot be answered from one client, or that stays open on one client after the other answered it.
- An ask that vanishes, reappears, or is answered by itself after an interrupt, stop, resume or reconnect.
- A plan approval that starts in a different permission than the one chosen.
- A question too long or too wide to read and answer on a small terminal or a phone at a large text size.

After every answer, check what the agent actually received (its reply, the native view, `ctl chat`), not only
what amux shows.
