# Journal fixtures

Journals the simulated net's in-process agents play. Each `<name>.frames` file
is what an agent process would have written to its journal: journal frames
(a varint length, then a `Step`) back to back, the initial snapshot first,
with empty steps left out as the journal's writer leaves them out.
`journals.toml` lists every file with the interpreter that made it, the
script it came from, what it reaches and what its end leaves on purpose.

## How each was made

- **Cut from recordings** (`claude_pty-*`, `claude_sdk-*`, `codex-*`): the
  interpreter's own recorded fixture named as `source`, which points at a
  provider recording in `crates/claude-specs` or `crates/codex-specs`, run
  through that kind's interpreter with `interpret::replay`, every step
  encoded with `journal::encode_frame`.
- **Generated** (`test-*`): an authored script in `generated/`, in the
  interpreter fixtures' vocabulary, run the same way through the test kind's
  interpreter. These reach shapes the recordings never show: an ask left
  open, a follow-up queued during a turn, a one-shot child's follow-up
  before its exit, a drained stop and a provider dying mid-turn.

## They compare, never regenerate

The files are fixed inputs: tests and simulated stories read them as they
are, and nothing rewrites them when an interpreter changes.
`tests/journal_fixtures.rs` checks that every listed file is whole frames,
obeys `interpret::check_invariants` for its kind and reaches what its
listing claims (`turn_end`, `ask`, `follow_up`, `exit`).

To add a journal, add its listing (and its script under `generated/` for a
generated one), then cut it:

    cargo test -p testnet --test journal_fixtures -- --ignored cut_missing_journals

The cutter writes only listed journals whose file does not exist yet. To
change a journal, delete its file on purpose and cut it again; the diff is
then a reviewed change, not a side effect.
