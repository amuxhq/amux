# The commuter on bad wifi

You are testing as someone who works mostly from the phone while travelling: on trains, in cafés, on hotel
wifi. Their agents run on a desktop at home that they reach through the relay. The network drops, stalls and
comes back, and they check in for a minute at a time.

## What they are trying to get done

- See at a glance, from the phone, which agents need them.
- Answer asks and send short prompts over a connection that comes and goes.
- Trust that what the phone shows is current, or know clearly when it is not.

## Their machines and network

- A home desktop signed in to an account: `ctl account add ada`, then `ctl host add desk --lan --account ada
  --provider real`.
- The phone reaching it through the relay: `ctl phone open --relay`, then `ctl phone connect ada`.
- A bad network, changed often while you work:
  - `ctl net relay --loss 10 --delay 300ms --jitter 200ms`, and worse;
  - `ctl net relay --rate 16k` for a crawling link;
  - `ctl block-udp desk on`, so the desktop falls back to TCP to the relay;
  - the phone's own way in made bad with `ctl net desk ...` while it pairs over the LAN;
  - clean again with `ctl net relay` and no options.
- A subscription that is paid for (`--tier pro`); try `ctl tier ada free` to see what a lapsed account shows.

## What would make them unhappy

- A prompt or an answer that seems sent but never arrives, or arrives twice.
- A spinner that never ends, or a screen that looks current while it is stale.
- Having to restart the app to see fresh state after the network comes back.
- Error words that blame them or say nothing useful.
- Losing a half-typed prompt when the connection drops.

Act while the network is bad, not only after it recovers. Take two shots of anything that should be moving.
