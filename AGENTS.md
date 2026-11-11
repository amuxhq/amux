# Operating Rules

amux is unreleased and in active development. Never preserve backwards
compatibility — old local protocols, config keys, and public APIs may be
broken freely. Simplify decisions accordingly. Before 0.9.0 nothing is
versioned up: every version number (store schema, wire protocol, file and
report formats) stays 1, an incompatible change keeps it, and stale state is
cleaned up by hand. The protobuf schemas are the
exception: `amux.v1` is frozen from 0.9.0 and changes only as
`proto/amux/v1/README.md` allows (add; reserve a retired number,
never renumber), which `just proto-compat` checks. `amux.dump` is exempt.

- Build, test and lint through `just` (`just --list`); `wt` is for worktrees,
  sessions and this tree's daemon. Recipes carry timeouts; a firing timeout is
  a hang to diagnose.
- Before a change reaches main, run `just premerge` and the tests of the
  crates it touched. A failing golden is re-recorded in the same change,
  after reading what moved.
- The why of a change and its test evidence go in the commit message; search
  past decisions with `git log --grep`. A rejected alternative that should
  constrain future work goes in that component's page in `docs/`.
- Committed documentation lives in `docs/` (start at `docs/README.md`);
  `notes/` is gitignored working material. Graduate notes into `docs/`
  deliberately; never force-add them.
- `docs/guide/` is the user guide, which amux.sh publishes from main at
  amux.sh/docs. A change people will notice updates its page there in the
  same commit; write those pages for users, not maintainers.
- Agent debugging starts at `docs/DEBUGGING.md`; captured reports live in `<root>/reports` under the installation root.
- This file holds rules about how agents operate. Documentation about the
  source belongs in `docs/` and component READMEs, not here.

## Handoff

Large or consequential work ends with a handoff; use the `handoff` skill
from the start. Small self-contained fixes end with a passing test and a
clear commit message — no handoff.

Machinery: `~/source/skills` (authoritative rules in its `docs/SPEC.md`).
