# QA

*For whoever runs a QA pass on a release candidate, and for whoever triages what it finds.*

QA is adversarial exploration by agents. A run starts several Claude Code testers, each playing a person with a
goal (a persona), each with its own test network of amux hosts to build, break and drive. They use amux the way a
suspicious person would, across real daemons, the real terminal client, the real phone app and real Claude and
Codex agents, and they keep evidence of everything that would make a person unhappy. Their judgement decides what
is a finding; the documentation only labels it. What a run produces is a set of findings, each with enough
evidence for a developer to find the stage at fault and write a test there.

QA is by hand only. It spends real model time, needs the operator's logins and, for phone testers, simulators; it
never runs in CI, and no test depends on it. Where it sits among the other suites is on [Testing](TESTING.md).

## Running it

```sh
just qa                                   # one wave of the default size
just qa --testers 2 --phone 1             # two testers, one of them with a phone
just qa --persona commuter --persona families --waves 3
```

| Option | Meaning |
| --- | --- |
| `--testers N` | Testers per wave. |
| `--phone M` | How many of them also drive the phone app; at most the number of simulator leases (two). |
| `--persona P` | Personas to use, by name (the files in `qa/personas/`); repeat it. Otherwise each wave draws fresh ones. |
| `--waves W` | Stop after this many waves. |

Before the first wave, the launcher:

- builds everything once, debug, with fixed features: `amux`, `testnet`, the fake providers, the provider probes,
  `dump-show`, and the debug phone app when any tester has a phone. Every tester's net uses that one build, so
  nothing rebuilds mid-run, and the release build, which cannot be supervised, is never picked up;
- launches each fresh binary once, because macOS can stall a binary's first launch;
- checks that the operator's Claude and Codex logins can run a real agent. Testers create real agents on Claude
  Haiku and Codex `gpt-5.6-luna` with those logins.

Each wave starts its testers at once. A tester with a phone runs entirely inside its own simulator lease (the
`iphone` pool), held for the tester's whole life, so two phone testers never share a device. When a wave ends the
next starts with fresh personas; the run stops when a whole wave adds no new finding, when `--waves` is reached,
or on Ctrl-C, which stops every tester and shuts every net down. At the end the launcher prints a summary: the
testers, the findings by severity, peak memory per wave, and where the run folder is.

## What a run leaves behind

Everything lives under `~/.local/state/amux-qa/<run>/`:

| Path | Contents |
| --- | --- |
| `reference/docs/`, `reference/qa/` | A snapshot of `docs/` and `qa/` taken at the start: everything the testers may read. |
| `ledger.jsonl` | The run's shared ledger of claims, ideas, findings and closing summaries ([`qa/LEDGER.md`](../qa/LEDGER.md)). |
| `summary.txt` | The launcher's closing summary. |
| `bin/` | The `testnet` the testers run, first on their `PATH`. |
| `login-check/` | The session that checked the logins with one real agent of each provider. |
| `t1/`, `t2/`, … | One folder per tester. |

Each tester's folder holds:

| Path | Contents |
| --- | --- |
| `prompt.md` | The prompt it was started with: the base prompt, its persona, the attack moves, the guide to talking to agents, and the run's facts. |
| `settings.json` | Its permissions. |
| `transcript.jsonl` | Its own transcript, as Claude Code streams it. |
| `launcher.err` | Whatever the tester's process wrote to its error stream, such as a lease it waited for. |
| `session.jsonl` | Every `testnet ctl` command it ran: time, arguments, success, reply, and the files it wrote. |
| `shots/` | Every screenshot it took, terminal (`.png`, the exact text beside it as `.txt`, and the cells as `.frame.json`) and phone. |
| `findings/<name>/` | One folder per capture: `finding.md`, `topology.json`, `session.jsonl`, `dumps/<host>/` and `dumps/phone/`, `providers/<agent>/`, `screens/`, `logs/`, and `errors.txt` when some evidence could not be taken. |
| `providers/` | Every real agent's recorded provider traffic, by host and agent. |

## How testers are confined

A tester is `claude -p --model sonnet`, started in its own folder outside the repository, so the repository's
agent instructions never load into it. It loads no user settings, skills, plugins or tool servers of the
operator's, and has only the shell, file reading and file editing tools. Its permissions run in `dontAsk` mode, so
anything not allowed is refused at once:

- it may run `testnet` (all of `testnet ctl`) and read-only utilities (`ls`, `cat`, `head`, `tail`, `grep`, `jq`,
  `wc`, `diff`, `file`); `git` is denied;
- it may read anywhere in the run folder, `reference/` included;
- it may write and edit only inside its own folder: notes, fake scripts, findings.

Its prompt tells it to read only `reference/docs` and `reference/qa`, and never to look for or read source code,
tests or history. Testers judge what a person would expect, not what the code does.

The prompt is assembled from the committed `qa/` folder:

| File | Contents |
| --- | --- |
| `qa/PROMPT.md` | The base prompt: the stance, the ledger, hypotheses, attacks, comparing views, screenshots, evidence, repros, judgement and when to stop. |
| `qa/personas/*.md` | One persona per file: who the person is, their goal, their machines and network, what would make them unhappy. |
| `qa/ATTACKS.md` | Attack moves with `ctl` recipes, and the screenshot checklist. |
| `qa/AGENTS-GUIDE.md` | Wording that reliably makes Claude and Codex agents do each thing a tester needs. |
| `qa/FINDING.md` | The finding template a capture starts from. |
| `qa/LEDGER.md` | The ledger's format and how testers use it. |
| `qa/TRIAGE.md` | From finding to test, for the triager. |

The tool testers drive everything with is `testnet ctl`, described on [TestNet](TESTNET.md).

## From findings to tests

Triage is not automated. A person or an agent with the source gathers the findings the ledger lists, deduplicates
them, finds the stage that owns each with the dumps and [Debugging](DEBUGGING.md)'s comparison table, reproduces
it there as cheaply as possible, writes a failing test where that stage's tests live, and files the issue with the
test named. [`qa/TRIAGE.md`](../qa/TRIAGE.md) walks through it, with a worked example for a drawing bug and one
for a network bug.
