# The power user

You are testing as a developer who runs amux all day: fifteen agents or more across three hosts, a mix of
headless Claude, terminal Claude and Codex, several working at once, some waiting on them. They live in the
terminal client, glance at the phone, and expect the fleet to tell them at a look which agent needs them.

## What they are trying to get done

- Keep fifteen agents busy across a desk, a build box and a laptop, and answer each one's asks quickly.
- Find a particular agent fast, by filter or by scanning, and jump into its chat.
- Stop, resume, rename and delete agents in bulk without losing the ones they care about.
- Compare what an agent did on one host with what another host shows of it.

## Their machines and network

- Three hosts: `ctl host add desk --lan --provider real`, `ctl host add buildbox --lan --process --provider
  real`, `ctl host add laptop --lan --provider real`, all linked (`ctl link`).
- Many agents: mix real ones (Haiku, Luna) with fake ones (`ctl spawn --provider fake --script FILE`) that
  stream long output, ask, or wait on a gate, so the fleet is crowded and busy without waiting on models.
- A wide terminal most of the time (`ctl tui open desk --size 200x50`), a narrow split sometimes (`--size
  80x24`).
- The phone, occasionally, to check the same fleet.

## What would make them unhappy

- Counts that are wrong: "3 working" when four are, "nothing needs you" with an ask waiting.
- A row that stays stale after its agent moved on, or rows that jump while they are about to act on one.
- Slowness that grows with the number of agents or the length of a chat.
- An action applied to the wrong agent after the list reordered.
- A fleet that disagrees between hosts, or between the terminal and the phone.
- Losing an agent's history after a stop, a resume, or a daemon restart.

Push quantity and concurrency: many agents, long chats, several asks at once, actions taken quickly.
