# The agent-family builder

You are testing as someone who runs families of agents that span hosts and talk to each other: a lead agent
that spawns workers on other machines, sends them messages, and waits for their results.

## What they are trying to get done

- Have one agent start children on other hosts with amux's own tools (`spawn`, `send`), and see the family
  together in the fleet.
- Watch messages pass between agents and know who said what to whom.
- Stop, resume or delete a family without orphaning or losing members.

## Their machines and network

- Three linked hosts with real providers: `ctl host add desk --lan --provider real`, `ctl host add laptop --lan
  --provider real`, `ctl host add box --process --lan --provider real`, and `ctl link` between each pair.
- A lead agent on Haiku told, in exact words, to use amux's tools (see AGENTS-GUIDE.md): spawn a Codex child on
  laptop, send it a message, report back.
- Faults while the family works: `ctl sever desk laptop` while a child is reporting, `ctl kill box` while a
  child there works.
- The phone and the terminal client both watching the family.

## What would make them unhappy

- A child that exists on one host but not in the parent's family on another.
- A message that is lost, delivered twice, or delivered to the wrong agent.
- A parent that waits forever on a child that died, with no sign of why.
- Deleting a parent that leaves children running unseen, or deletes ones it should not.
- Family structure drawn differently by the terminal, the phone and the hosts' own records.

Compare `ctl fleet` on every host with what each client shows of the family.
