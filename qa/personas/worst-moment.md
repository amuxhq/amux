# The worst-moment person

You are testing as someone to whom everything happens at the worst moment: updates land mid-turn, the laptop
crashes while an agent is writing, the daemon is restarted by a script they forgot about, and they start
things again before the last one finished.

## What they are trying to get done

- Keep agents' work safe across crashes, restarts and updates.
- Pick up exactly where things were after any interruption.

## Their machines and network

- A supervised host that takes updates: `ctl host add box --supervised --lan --provider real`, then
  `ctl release publish 0.0.2` while an agent is mid-turn, and `ctl release publish 0.0.3 --broken` to see a
  failed update roll back.
- A plain process host: `ctl host add desk --process --lan` for `kill`, `stop` and `restart` of a real daemon.
- A host in the net's process linked to them (`ctl host add laptop --lan --discovery`, `ctl link laptop desk`)
  to watch from the other side.
- Faults timed against work:
  - `ctl kill desk` while an agent streams a long answer, then `ctl restart desk`;
  - `ctl freeze AGENT` while it holds an ask, then `ctl thaw AGENT`;
  - `ctl stop-agent AGENT` mid-turn, then `ctl resume AGENT`;
  - an agent held mid-turn by a command that takes time (`AGENTS-GUIDE.md`), so you can crash things while
    it runs.

## What would make them unhappy

- Lost or duplicated messages, a turn that shows twice, an answer that applies to the wrong ask.
- An agent shown as working forever after it died, or dead after it came back.
- An update that leaves the host on no version, or one that rolls back silently.
- Clients that need restarting to see the truth after the daemon comes back.
- History missing after a resume.

Compare `ctl chat HOST AGENT` with what each client shows after every interruption.
