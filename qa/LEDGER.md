# The ledger

Every QA run has one shared ledger: `ledger.jsonl` in the run folder. It is how testers working at the same time
avoid covering the same ground, pass on hunches, and credit whoever found a problem first. It is append-only and
written only through `testnet ctl ledger`; never edit the file by hand.

`ctl ledger` finds the run's ledger through the `AMUX_QA_RUN` environment variable and signs each entry with
`AMUX_QA_TESTER`. The launcher sets both. Outside a QA run there is no ledger, and `ctl ledger` says so.

## Using it

1. **Read it first.** `ctl ledger read` prints every entry, oldest first. See what others have claimed and found.
2. **Claim ground nobody has covered.** Go where the ledger is thin. Say what you are taking, specifically enough
   that someone else can tell whether they would overlap:
   `ctl ledger claim pairing a process host and a phone over a flapping LAN`.
3. **Keep reading it as you go,** before each new line of attack. If someone has already found a symptom you are
   seeing, the credit is theirs: note in your finding that it reproduces, or move on.
4. **Record hunches as ideas.** Something that looked wrong but that you could not capture or have no time for:
   `ctl ledger idea the phone's working count seems one behind after a resume`. Anyone may pick it up.
5. **Record each finding** once its folder is filled in: `ctl capture NAME` while the problem is on screen, fill in
   `findings/NAME/finding.md`, then `ctl ledger finding NAME`. It records the finding's folder, its title (the
   first `# ` heading) and its severity (the `- **Severity:**` line), so fill those in first.
6. **Close with a summary** when your ground is exhausted:
   `ctl ledger done covered X, Y and Z; would next try W`. Then end.

## Entries

One JSON object per line. Every entry has:

| Field | Meaning |
| --- | --- |
| `time` | When it was written, RFC 3339 in UTC. |
| `tester` | Who wrote it (`AMUX_QA_TESTER`). |
| `kind` | `claim`, `idea`, `finding` or `done`. |

And by kind:

| Kind | Fields |
| --- | --- |
| `claim` | `text`: the ground taken. |
| `idea` | `text`: the hunch. |
| `finding` | `finding`: the path of the finding's folder; `title`; `severity`. |
| `done` | `summary`: what was covered and what to try next. |

```json
{"time":"2026-10-11T09:12:03+00:00","tester":"t2","kind":"claim","text":"supervised updates mid-turn"}
{"time":"2026-10-11T09:40:17+00:00","tester":"t2","kind":"finding","finding":"/…/t2/findings/update-drops-turn","title":"An update mid-turn drops the turn's last line","severity":"data loss"}
{"time":"2026-10-11T10:05:44+00:00","tester":"t2","kind":"done","summary":"covered updates and rollback; next: updates while the relay is degraded"}
```

Findings live in each tester's own folder; the ledger only points at them. Triage gathers them from there (see
`TRIAGE.md`).
