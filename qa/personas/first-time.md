# The first-time user

You are testing as someone who has just heard of amux and is setting it up for the first time on a laptop, a
desktop and a phone. They have used Claude Code or Codex before, but never amux. They read the screens, not the
docs, and they believe what the screens tell them.

## What they are trying to get done

- Get amux running on the desktop and start one Claude agent on a real task.
- Pair the laptop and the phone with the desktop, so they can see and answer that agent from either.
- Answer a question or a permission ask from the phone while away from the desk.
- Come back to the laptop later and pick up where they left off.

## Their machines and network

Build it as you go, the way they would meet it:

- `ctl host add desk --lan --bonjour --provider real` and `ctl host add laptop --lan --provider real`: two
  machines on one home network, at first not paired with each other.
- Pair them the way a person does rather than with `ctl link`: `ctl amux desk -- pair` opens pairing on the
  desk and prints its PIN and address; `echo PIN | ctl amux laptop -- pair ADDRESS` joins from the laptop.
- The phone: `ctl phone open`, then `ctl phone pair desk`, and use the app from its first screen.
- A good home network. Make it worse only to see how a newcomer reads the trouble, not as the main attack.

## What would make them unhappy

- Not knowing what to do next: an empty screen that looks broken, a button whose words do not say what it does,
  a PIN that has to be typed somewhere unexplained.
- Pairing that seems to work on one side and not the other, or a host listed twice.
- An agent made on the desk that the laptop or phone does not show, or shows in a different state.
- A question the agent asked that the phone never shows, or an answer from the phone the agent never gets.
- Wording that assumes they know amux's own terms.
- Anything that loses what they typed.

Pay particular attention to first screens, empty states and the first time each thing happens.
