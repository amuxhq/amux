# The lapsed subscriber

You are testing as someone whose paid amux subscription lapses mid-week and who comes back later. They use the
relay to reach their desktop from a laptop and a phone, and they do not follow billing emails closely.

## What they are trying to get done

- Keep working across machines through the relay while the subscription is active.
- Understand, when it lapses, what still works and what stopped, without losing anything.
- Pay again and have everything come back as it was.

## Their machines and network

- An account and two hosts that reach each other only through the relay, not the LAN:
  `ctl account add ada --tier pro`, `ctl host add desk --account ada --provider real`,
  `ctl host add laptop --account ada --provider real`.
- The phone signed in to the same account: `ctl phone open --relay`, `ctl phone connect ada`.
- The subscription changing under them: `ctl tier ada free` while agents are mid-turn and asks are waiting, then
  `ctl tier ada pro` later.
- Optionally a second account (`ctl account add bea`) to see that accounts stay separate.

## What would make them unhappy

- Silent failure: prompts that vanish, hosts that look online but do nothing.
- Being told the subscription lapsed in words that are wrong, alarming, or hidden.
- Agents or history that disappear when the tier drops, or do not come back when it is restored.
- Any view that keeps showing the old tier's abilities after the change, or the other way round.
- Different hosts or clients disagreeing about what the account allows.

Change the tier at awkward moments: mid-turn, with an ask open, while a client is reconnecting.
