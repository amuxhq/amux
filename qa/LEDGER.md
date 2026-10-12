# The ledger

Every QA run has one shared ledger: `ledger.jsonl` in the run folder. It is how testers working at the same time
avoid covering the same ground, pass on hunches, and credit whoever found a problem first. It is append-only and
written only through `testnet ctl ledger`; never edit the file by hand.

`ctl ledger` finds the run's ledger through the `AMUX_QA_RUN` environment variable and signs each entry with
`AMUX_QA_TESTER`. The launcher sets both. Outside a QA run there is no ledger, and `ctl ledger` says so.

## Using it

1. **Read it first.** `ctl ledger read` prints every entry, oldest first. See what others have claimed and found.
   `ctl ledger findings` lists the run's findings alone, each once, with who saw it again beneath it; your prompt
   starts with the list as it stood when you began.
2. **Claim ground nobody has covered.** Go where the ledger is thin. Say what you are taking, specifically enough
   that someone else can tell whether they would overlap:
   `ctl ledger claim pairing a process host and a phone over a flapping LAN`.
3. **Keep reading it as you go,** before each new line of attack and before you record a finding. If someone has
   already found a symptom you are seeing, the credit is theirs: move on, or, if your evidence adds something
   (another way in, a cheaper repro), record it as a duplicate (step 5).
4. **Record hunches as ideas.** Something that looked wrong but that you could not capture or have no time for:
   `ctl ledger idea the phone's working count seems one behind after a resume`. Anyone may pick it up.
5. **Record each finding** once its folder is filled in: `ctl capture NAME` while the problem is on screen, fill in
   `findings/NAME/finding.md` from the template ([`FINDING.md`](FINDING.md)), then `ctl ledger finding NAME`. It
   records the finding's folder, its title (the first `# ` heading) and its severity (the `- **Severity:**`
   line). It refuses, saying what is missing, a finding whose title is still the folder's name, whose lines or
   sections are unfilled (only "Suspected stage" may stay empty), which keeps a template comment, or whose
   severity is not exactly one of `data loss`, `broken flow`, `wrong state`, `visual` or `wording`. A folder is
   recorded once.

   Once the run has any finding, say which yours is. When one in `ctl ledger findings` shows the same symptom,
   name it: `ctl ledger finding NAME --duplicate-of t2/lost-row` (`TESTER/NAME`, as the list shows it). When none
   does, `ctl ledger finding NAME --new`. Without either, it is refused with the run's findings most like your
   title first. You judge what counts as the same symptom; a duplicate names the original, never another
   duplicate. A duplicate keeps its evidence for triage but is not new, and a run stops after a wave that finds
   nothing new.

   Only this run counts. A symptom a finding of an earlier run showed is new here: after a round of fixes, seeing it
   again is a regression worth reporting.
6. **Close with your sign-off** when everything left on your list is something you cannot do:
   `ctl ledger done covered X, Y and Z; verdict: not ready for the commuter, a prompt sent while the relay drops
   is lost; could not try two phones at once, I hold one simulator`. Say what you covered, your verdict for
   your persona (ready, or not ready and why), and what you could not do and why. Never what you would try next:
   anything you could still try is not done. Then end.

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
| `finding` | `finding`: the path of the finding's folder; `title`; `severity`, one of the five words; `duplicate_of`, only on a duplicate: the original finding as `TESTER/NAME`. |
| `done` | `summary`: what was covered, the verdict for the persona, and what could not be done and why. |

```json
{"time":"2026-10-11T09:12:03+00:00","tester":"t2","kind":"claim","text":"supervised updates mid-turn"}
{"time":"2026-10-11T09:40:17+00:00","tester":"t2","kind":"finding","finding":"/…/t2/findings/update-drops-turn","title":"An update mid-turn drops the turn's last line","severity":"data loss"}
{"time":"2026-10-11T10:05:44+00:00","tester":"t2","kind":"done","summary":"covered updates and rollback; verdict: not ready, an update mid-turn drops the turn's last line; could not test updates over the relay, the net has no relay"}
```

`ctl ledger read` names each finding `TESTER/NAME`; it lives in `<run>/TESTER/findings/NAME/`. Findings live in
each tester's own folder; the ledger only points at them. Triage gathers them from there (see
`TRIAGE.md`).
