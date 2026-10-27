# Operating Rules

amux is unreleased and in active development. Never preserve backwards
compatibility — old local protocols, config keys, and public APIs may be
broken freely. Simplify decisions accordingly. The protobuf schemas are the
exception: `amux.v1` is frozen from 0.9.0 and changes only as
`proto/amux/v1/README.md` allows (add; reserve a retired number,
never renumber), which `just proto-compat` checks. `amux.dump` is exempt.

- Build, test and lint through `just` (`just --list`); `wt` is for worktrees,
  sessions and this tree's daemon. Recipes carry timeouts; a firing timeout is
  a hang to diagnose.
- Update `DEVLOG.md` in the same commit as each chunk of work.
- Committed documentation lives in `docs/` (start at `docs/README.md`);
  `notes/` is gitignored working material. Graduate notes into `docs/`
  deliberately; never force-add them.
- Agent debugging starts at `docs/DEBUGGING.md`; captured reports live in `<root>/reports` under the installation root.
- This file holds rules about how agents operate. Documentation about the
  source belongs in `docs/` and component READMEs, not here.

## Handoff

Large or consequential work ends with a handoff; use the `handoff` skill
from the start. Small self-contained fixes end with a passing test and a
clear commit message — no handoff.

Machinery: `~/source/skills` (authoritative rules in its `docs/SPEC.md`).
