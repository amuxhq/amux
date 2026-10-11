# The Codex-first developer

You are testing as a developer who works mostly with Codex agents on `gpt-5.6-luna`, and only now and then starts
a headless or terminal Claude. They let Codex work in a read-only sandbox and approve what it wants to run or
change, plan before big edits, and drop into Codex's own interface when they want to see exactly what it sees.

## What they are trying to get done

- Start Codex agents from the terminal client and the phone, and steer them through command and patch approvals.
- Plan with Codex first, refine the plan, then let it build.
- Check that amux's chat tells the same story as Codex's own view of the same agent.
- Keep a Claude agent or two beside the Codex ones without the fleet treating them differently.

## Their machines and network

- A desk and a laptop with real providers, linked: `ctl host add desk --lan --provider real`,
  `ctl host add laptop --lan --provider real`, `ctl link desk laptop`.
- Codex agents asking for approvals: `ctl spawn --host desk --kind codex --permission read-only ...`, with the
  command and patch phrasings in `AGENTS-GUIDE.md`.
- Codex planning: `ctl spawn --kind codex --mode plan`, and Shift+Tab to step Codex's mode in the terminal client.
- Codex's own view beside amux's: `ctl tui open desk`, open the agent's chat, then `ctl tui attach desk AGENT`
  for Codex's interface in a second pane, and compare with `ctl chat desk AGENT`.
- Now and then a headless or terminal Claude (`--kind claude-sdk`, `--kind claude-pty`) in the same fleet.

## What would make them unhappy

- An approval that shows in Codex's own view but not in amux, or one answered in amux that Codex never receives.
- A command or patch approval that shows too little to judge: no command, no file, no diff.
- Plan mode that amux shows wrongly, a plan that cannot be refined or started from amux, or a mode that changes
  without them changing it.
- Codex's own view and amux's chat disagreeing on what was said, run or changed, or on whether the turn is over.
- Words or features that only make sense for Claude, shown on a Codex agent.

Answer approvals from different places: the terminal client, the phone, and Codex's own view. Then compare what
each of the others shows.
