# Operating Rules

amux is released. From 0.9.0 on, builds of different versions meet; from
0.11.0 on, a build reads what 0.11.0 or a later release left on disk. Five
things are promises, and a change keeps them:

- **The protocol.** From 0.11.0 on, `amux.v1` only adds, as
  `proto/amux/v1/README.md` says (reserve a retired number; never renumber,
  retype or rename), which `just proto-compat` checks. `docs/WIRE.md` has
  the steps for a new field.
  `amux.dump` and `amux.experimental` are exempt.
- **The store.** It changes by appending a migration that only adds; a
  released migration is never edited. `just store-check` checks both, and
  `docs/JOURNAL_AND_STORE.md` has the steps.
- **A profile on disk.** What a release wrote, a later build opens: the
  `account` and `profile` files, and each agent's spec, journal and facts.
  A new field is one an older file can lack. The upgrade test
  (`crates/node/tests/upgrade.rs`) opens the profile 0.11.0 left, plus one
  from a later release only when that release writes something 0.11.0's
  profile can't hold.
- **The config file.** A key a release read keeps being read; a rename
  keeps the old name working. A key no build knows is ignored with a
  warning, so a config a release accepted never stops amux starting.
- **Commands amux runs on itself.** `amux supervise`, `amux daemon`,
  `amux relay`, the `--config` option, and `amux internal mcp`, `hook`,
  `ssh-pair`, `ssh-link` and `browser` keep their names and arguments, and
  `browser` its message framing. So does Windows' launcher:
  `amux-launch <args>` runs `amux <args>` from its own folder, and
  `amux supervise` honours `AMUX_LAUNCH_LINEAGE` (keeps the handle, hands it
  to its successor, writes `0` to it before a deliberate exit) and
  `AMUX_LAUNCH_RESTART` (a restart that finds a stop asked for exits
  without a daemon). A login item
  or service unit, a supervisor handing over to the release it installed, a
  running agent's provider, a browser's native messaging host and a paired
  host at another version call them long after the build that wrote the
  call. The command-line test in `crates/amux/src/main.rs` parses each one.
  Commands a person types change freely.

**Experiments are exempt.** An experiment is switched on per installation and
lives in `amux.experimental`, in the entries of `experimental` maps, in store
tables named `exp_*` and under `[experiments.<name>]` in the config. None of
that is promised: it may change or go in any release, and its data may be
dropped on upgrade. What switches experiments on (`experiments.enabled` and
the settings fields), and each `experimental` map itself, is promised like
everything else. An experiment must not keep anything it can't afford to lose
in those places; what must survive lives in a promised place or with the thing
it describes. The registry is `crates/settings/src/experiments.rs`.

Nothing else is promised. Crate APIs, internals and test formats change
freely, and nothing gets a shim for a shape no release shipped. A change that
cannot keep a promise is not made by breaking it: it is the next protocol
(`amux/2`, beside `amux/1`) or a new column or table.

- Build, test and lint through `just` (`just --list`); `wt` is for worktrees
  and this tree's daemon. Recipes carry timeouts; a firing timeout is a hang
  to diagnose.
- Before landing, run `just premerge`: every check and every
  test, through Buck2's cache, so only what the change affects runs. A
  failing golden is re-recorded in the same change (`just goldens`), after
  reading what moved.
- Land a branch on main with `just land "What the branch does"`. It merges
  your HEAD with `--no-ff` onto a fresh main in a worktree of its own, runs
  the checks again on the merge, pushes, and prints `LANDED <sha>`; your
  tree is never touched. If it stops, fix the branch (`just merge-main`
  brings main in) and land again. Never
  force-push. Without push access, open a pull request instead.
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
