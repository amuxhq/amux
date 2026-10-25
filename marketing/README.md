# The README's pictures and the launch film's takes

The images in `images/readme/` are taken from a running amux, not drawn. A
scene of scripted agents runs on a test network of three machines, the
terminal client and the iPhone app are photographed against it, and the
pictures are composed from those shots. Re-run it whenever the clients'
look changes.

## The scene

Ada's small team builds Tally, an invoicing app (`repo/`). Each machine
has a clone: `~/src/tally` on `studio`, the desktop the terminal client
runs on; `~/tally` on `build`, a build server; `~/code/tally` on `laptop`.

| Agent | Kind | Machine | Where it rests |
|---|---|---|---|
| `checkout-redesign` | Claude | studio | Working, after starting two subagents |
| `payment-tests` | Codex | build | Working; started by `checkout-redesign` |
| `copy-pass` | Claude | studio | Finished, its report delivered to `checkout-redesign` |
| `flaky-sync-test` | Codex | build | Asking to run `cargo test -p sync --release` |
| `db-migration` | Claude | laptop | Waiting on its plan |
| `api-docs` | Claude | laptop | Idle |
| `deps-bump` | Codex | build | Idle |

Every agent plays a script from `scripts/`, run by the fake Claude and Codex
providers, so the scene is the same on every start. The subagents are started
by `checkout-redesign` through amux's own `spawn` tool and play their
machine's script. The scripts pause where a model would think, so the fleet's
ages read as a morning's work; the scene takes about five minutes to come to
rest.

The clones live in your home folder (`~/src/tally`, `~/tally` and
`~/code/tally`), so both clients write them as a person's. Each carries a mark
in its `.git`: the scene refuses to start over a folder there that isn't one
of its clones, and `just scene down` removes only its own.

## Taking the pictures

```sh
just ios build                                          # once, for the phone
wt exec amux/<tree> -- scripts/with iphone -- just scene shots
just compose
```

`just scene shots` starts the scene afresh, waits for every agent to come to
rest, and photographs the terminal client (fleet, a chat and the review
page, dark and light) and, under the simulator lease, the phone (fleet and
the waiting request). Without a lease it photographs only the terminal. The
shots land in `~/.local/state/amux-marketing/<tree>/launch/images`.

It also photographs checkout-redesign's chat in three other terminals'
colours, from `palettes/`: Gruvbox Dark, Solarized Light and Catppuccin Mocha.
The client takes its palette from each, as it would from yours.

`just compose` redraws the terminal frames at twice the renderer's size, puts
them in windows and the phone screens in a device outline, and writes the
README's light and dark images. The three themes and the agents' own
interfaces are one image each, for both of GitHub's pages.

## Claude Code's and Codex's own interfaces

```sh
just scene native
```

Only the real programs can draw their own interfaces, so `just scene native`
runs them, under your own Claude and Codex logins: real Claude Code in its
terminal and real Codex, on studio, in a clone of Tally at
`/Users/Shared/tally`. Each is asked a question Tally's files answer whole, by
reading. When both are done, it opens their screens with `amux attach` from
the terminal client, as a person would, and photographs them. Claude asks
once whether to trust the folder; the scene says yes, which Claude remembers
in your own settings. The answers differ from run to run, so the shots are
kept until it runs again, and `just compose` uses whichever it finds.

Codex runs with a scratch home, so it writes its folder in full: the clone is
in `/Users/Shared` so that it isn't a folder under your account's name.

## Filming the takes

```sh
wt exec amux/<tree> -- scripts/with iphone -- just scene film
```

`just scene film` films the takes the launch film is cut from, all of them
real UI. It films three terminal panes with `ctl tui record`:

- the fleet, from the start of the scene;
- checkout-redesign's chat, as the agent starts its subagents and hears back;
- the review page, with a comment typed onto a line a key at a time.

It also uses the simulator's screen recording to film the phone opening
flaky-sync-test's request and allowing it. The fleet is filmed at the same
moment, so the film can show the terminal keeping up.

`takes/takes.json` puts every take and every scripted action on the scene's
one clock. The film itself lives in its own repository, `amux-film`.

`just scene up` starts the scene to look at
(`testnet ctl tui watch PANE` from its folder), and `just scene down` stops it
and removes its clones.
