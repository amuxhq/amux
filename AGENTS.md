# Operating Rules

amux is released. From 0.9.0 on, builds of different versions meet, and a
build reads what an earlier one left on disk. Three things are promises, and
a change keeps them:

- **The protocol.** `amux.v1` only adds, as `proto/amux/v1/README.md` says
  (reserve a retired number; never renumber, retype or rename), which
  `just proto-compat` checks. `docs/WIRE.md` has the steps for a new field.
  `amux.dump` is exempt.
- **The store.** It changes by appending a migration that only adds; a
  released migration is never edited. `just store-check` checks both, and
  `docs/JOURNAL_AND_STORE.md` has the steps.
- **A profile on disk.** What a release wrote, a later build opens: the
  `account` and `profile` files, and each agent's spec, journal and facts.
  A new field is one an older file can lack. The upgrade test
  (`crates/node/tests/upgrade.rs`) opens a profile every release left.

Nothing else is promised. Crate APIs, internals and test formats change
freely, and nothing gets a shim for a shape no release shipped. A change that
cannot keep a promise is not made by breaking it: it is the next protocol
(`amux/2`, beside `amux/1`) or a new column or table.

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
