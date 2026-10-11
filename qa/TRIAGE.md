# Triage: from finding to test

This guide is for whoever turns a QA run's findings into fixes, a person or an agent working in the repository
with the source. Testers are black-box; you are not. Your job is to find which stage owns each problem, show it
failing in a test at that stage, and file it.

Nothing here is automated. A finding is like a bug report with a debug dump attached: read it, reason about it,
and reproduce it as cheaply as you can.

## 1. Gather and deduplicate

- `ledger.jsonl` in the run folder lists every finding by folder (`finding` entries), with title and severity. The
  folders are under each tester's own folder: `<run>/<tester>/findings/<name>/`.
- For each, read `finding.md`, look at `screens/` (every screenshot the tester took, plus a fresh shot of every
  open pane and the phone at capture time), and skim `session.jsonl`, which holds every `ctl` command the tester
  ran, with its reply, in order. `topology.json` is the net as it stood.
- Several testers often meet the same bug from different sides. Group findings by symptom and by the stage you
  suspect; keep the clearest repro and note the others under it. The first finder in the ledger gets the credit.

## 2. Find the stage

Every finding passes through the data chain: what the provider sent, the interpreter's steps, the journal and
the store, the client's state, its views, and the screen. The first layer that is wrong owns the bug.

- `dumps/<host>/` holds `amux dump` of every running host at capture time; `dumps/phone/` the phone's own dump,
  client parts included. Print one with `testnet ctl dump show BUNDLE [--agent A]` (or the `dump-show` binary
  from `crates/replay-support`): each agent's row, journal, store rows, facts and checkpoints as text.
- [Debugging](../docs/DEBUGGING.md) has the comparison table under "Narrowing a problem to one stage": facts
  against the replayed journal, journal against store, store against client state, rows against the screen.
- `providers/<agent>/` holds a real agent's recorded provider traffic, for findings about what amux wrote to the
  provider (a wrong key into Claude's menu, a wrong request to Codex).
- `logs/` holds the net's log and each process host's daemon and supervisor logs.
- Disagreement between views narrows it at once: if `ctl chat` (the host's record) is right and a client is
  wrong, the bug is client-side; if the host's record is already wrong, look below it.

## 3. Reproduce at that stage

Use whatever is cheapest that still shows the problem:

- a fake-provider script you write here, from the dump's facts (what the provider said, in order) and the real
  agent's recorded traffic in `providers/<agent>/`: testers use real agents only, so the scripted repro that
  takes the model out of it is built in triage;
- a topology and a few `testnet ctl` steps, from `session.jsonl`;
- a terminal set (`journeys/sets`) for a screen state;
- the dump's own facts, as an interpreter fixture.

## 4. Write the failing test where that stage's tests live

Show it failing before the fix.

| Stage | Test |
| --- | --- |
| Interpreter | An interpreter fixture from the facts (`crates/interpret/fixtures/<kind>/`), which also produces the view golden |
| Ingest or store | A store test (`crates/store/tests`) or a single-daemon test (`crates/node/tests`) |
| Client event order | A `ui-state` spec (`crates/ui-state/tests/spec`) |
| View | A `ui-view` test (`crates/ui-view/tests`) |
| Terminal drawing | A component or whole-frame golden (`crates/tui/tests`), with an explicit assertion beside the golden |
| Phone drawing | A component snapshot or whole-screen golden (see [the iPhone app](../docs/IOS.md)), with an explicit assertion |
| Network, replication, families, lifecycle | A many-host spec in `crates/testnet/tests`, with fake scripts, gates, fences and cuts in place of timing |
| Process, supervisor, updates | The same on process or supervised hosts, or `crates/amux/tests` |
| Only visible through a real client | A journey story (`journeys/manifest.json`) |

[Testing](../docs/TESTING.md) says how each boundary is chosen; [TestNet](../docs/TESTNET.md) how to write a
many-host spec, including process and supervised hosts.

## 5. File it

File the issue with the test named, and fix it on its own branch.

## Worked example: a card that clips a long command

**The finding.** A tester whose real agent ran a 140-character shell command reports, at an 80-column terminal, that the command's card cuts the command off mid-word with no ellipsis,
and the rest is unreachable. `screens/` holds the 80-column shot and a 160-column shot where it fits.

**The stage.** `ctl chat desk worker` in `session.jsonl` shows the whole command in the host's record, and
`dump show` shows the full command in the store row. So the data is right through the store; the question is
whether the view gives the terminal the whole text or the terminal drops it. The text beside the shot
(`.txt`) shows the cut, so it is not a font limit of the picture.

**The repro.** Build the same row in a `ui-view` test from the store slice the dump holds: the view carries the
full command. The view is right, so the drawing is wrong.

**The test.** A terminal component golden for the tool card at 80 columns with that command, plus an explicit
assertion that the drawn lines together contain the whole command or end in an ellipsis. It fails before the fix,
and the golden alone would not have said why.

## Worked example: a flapping link leaves the phone's row stale

**The finding.** A commuter-persona tester had the phone paired with `desk` and ran `ctl flap desk laptop --down
2s --up 5s` while an agent on `laptop` finished its turn. After `ctl flap stop`, `ctl fleet desk` shows the agent
idle, but the phone still shows it working a minute later, and the phone's own dump (`dumps/phone/`) shows the
client's state with the agent working.

**The stage.** The desk's record is right, so the network carried the change to the desk. The phone talks to the
desk; its client state never got the change, so the question is whether the desk sent it to the phone or the
phone dropped it. The phone dump's trace (`client/`) shows the subscription reconnected during a down period and
never received the phase change that landed while it was away.

**The repro.** A many-host spec in `crates/testnet/tests`: two linked hosts, a client subscription on one, a fake
agent on the other held at a gate, `sever_link` while the subscription reconnects, open the gate, `restore_link`,
then `fence` the near host at the turn-end row and observe the subscription. No timing: the gate and the fence
order the steps.

**The test.** The spec asserts the subscription ends at the agent's idle phase once the near host holds the
turn-end row. It fails before the fix; if the client's own reconnect handling turns out to be at fault instead, the
same events go into a `ui-state` spec.
