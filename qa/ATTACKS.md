# Attack moves

These are openers, not limits. Each is a way to put amux somewhere its makers may not have looked. Use them to
start, then follow what you see: the best findings come from edge cases nobody listed.

After every move, compare the views: the terminal client (`ctl tui screen`), the phone (`ctl phone query`), the
host's own record (`ctl chat HOST AGENT`, `ctl fleet HOST`), and the provider's own view where there is one
(`ctl tui attach PANE AGENT`). Any two that differ is a finding, even when neither looks wrong alone.

Every agent is a real Claude or Codex. To break things at an exact moment, hold an agent mid-turn: ask it to run
a command that takes time (`AGENTS-GUIDE.md` has the wording for each), and while it runs cut the link, kill the
daemon, flap the network or stop the agent. Network faults (`ctl sever`, `ctl flap`, `ctl net`) give you the rest
of the timing.

## Interrupt it halfway

- Stop a turn mid-stream: Ctrl+X in the terminal (`ctl tui keys PANE C-x`), the stop control on the phone,
  `ctl stop-agent AGENT` from outside.
- Stop the agent while an ask is open, then `ctl resume AGENT`. Is the ask still there, gone, or answered?
- Close the terminal client mid-turn (`ctl tui close PANE`) and open it again (`ctl tui open HOST`).
- Background the phone app's work by closing and reopening it (`ctl phone open` again).

## Do it twice

- Send the same prompt twice quickly (`ctl tui type`, Enter, the same again before the first turn ends).
- Answer the same ask from the terminal and the phone.
- Create two agents with the same name, in the same folder, on the same host.
- Pair the same two hosts twice; unpair twice.
- `ctl release publish` the same version twice.

## From two clients at once

- Open two panes on the same host (`ctl tui open desk`, `ctl tui open desk`) and one on another host, plus the
  phone. Act in one; watch the others.
- Type in one client while another sends a prompt to the same agent.
- Rename or delete an agent from one client while another has its chat open.

## While the network drops or the daemon dies

- `ctl sever A B` mid-turn, then `ctl restore A B`. Does the far host catch up, and do its clients?
- `ctl flap A B --down 2s --up 5s` for a minute while working, then `ctl flap stop`.
- `ctl net HOST --loss 20 --delay 400ms --jitter 300ms --reorder 10`, or `--rate 8k`; `ctl net relay ...` for
  the relay; `ctl net HOST` with no options makes it clean again.
- `ctl block-udp HOST on`: the host falls back to TCP to the relay.
- `ctl kill HOST` mid-turn (its agents keep running), then `ctl restart HOST`. `ctl stop HOST` for a clean stop.
- `ctl freeze AGENT` while it streams or holds an ask; the agent answers nothing until `ctl thaw AGENT`.

## Right after a restart

- Act in the first seconds after `ctl restart HOST`: send, answer, open a chat, create an agent.
- Restart one host of a pair and watch the other's fleet. A link to a crashed process host can take up to two
  minutes to come back; what does each client say meanwhile?

## Empty, huge and strange input

- An empty prompt; only spaces; only newlines.
- A huge prompt (`ctl make-file big.txt 2m --fill text`, then `ctl tui paste PANE --file big.txt`), a very long
  single line, thousands of short lines.
- Unicode: accents, right-to-left text, combining marks, emoji, CJK, zero-width characters, a name made of them.
- Long names for agents, hosts and folders; names with spaces, slashes, quotes.
- Ask an agent for a very long answer, a single 300-character line, or to run a very long shell command, to
  see wrapping and clipping at 80 columns.

## Cancel, then retry

- Start creating an agent and back out with Escape; start again.
- Cancel pairing halfway and pair again.
- Answer an ask, interrupt before the agent goes on, and answer again.
- Stop an agent, resume it, stop it again before it settles.

## Steps out of order

- Answer an ask after the agent was stopped.
- Send to an agent on a host you just unlinked (`ctl unlink A B`).
- Use the phone before pairing finishes; open a chat whose agent was just deleted elsewhere.
- Change a subscription tier (`ctl tier ACCOUNT free`) while a relayed turn is in flight.

## Updates at the worst moment

- On a supervised host, `ctl release publish 0.0.2` while an agent is mid-turn; `ctl release publish 0.0.3
  --broken` and watch the rollback. Does every client show the host the whole time, and on which version?
- `ctl kill HOST` on a supervised host: its supervisor starts the daemon again on its own.

## Looks: sizes, themes, text sizes

- Terminal: `ctl tui resize PANE 80x24`, then `200x50`; open panes with `--theme light` and `--theme dark`.
- Phone: `ctl phone text-size accessibility3` (and `xSmall`), `ctl phone appearance dark` and `light`.
- Resize while something streams or an ask is open.

# The screenshot checklist

Look at every screenshot, and describe it in writing before you act again. For each, check:

- **Clipped or overlapping text:** words cut off, lines running into each other or off the edge.
- **Misalignment:** columns that do not line up, ragged edges, a badge off its row.
- **A wrong colour in one theme:** text that disappears on its background in light or dark, a highlight that
  only works in one.
- **Stale state:** a phase, count or preview that has not caught up with what the host records.
- **A stuck spinner:** anything still "working" when the host says it is idle. Take two shots a few seconds apart
  of anything that should be animating, to tell moving from stuck.
- **Wrong counts or tenses:** "1 agents", "5s ago" that never changes, "working" for something finished.
- **Confusing words:** jargon, a button whose words do not say what it does, an error that blames the person.
- **An empty state that looks broken:** a blank screen with no word of why or what to do.

Check every screen you rely on at a narrow (80 columns) and a wide terminal, in both themes, and on the phone at
a large text size.

`ctl tui shot` has known limits: background colour a program paints by clearing to the end of a line is not
kept, and its font has no CJK or emoji glyphs, so those draw as boxes. A box in a shot is not a finding on its
own: check those characters in the text beside the shot (the `.txt` file) or in `ctl tui screen`, which are
exact.
