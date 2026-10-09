# How amux works

*For people using amux who want to know what their devices, accounts and paired machines are doing, and why it is safe to open a door into their machines.*

amux runs AI coding agents on your computers and lets every device you own
reach them. An agent keeps working on the machine where you started it; your
laptop, another desktop or your phone can open its chat, answer its
questions, send it the next prompt, or start new agents there. This page
explains the model behind that: which devices do what, how accounts and
pairing fit together, what the relay can and cannot see, and what happens
to your agents when amux itself restarts.

## The pieces

| Word | What it is |
|---|---|
| **Host** | A machine that runs amux: a desktop, a server, and your phone too. Hosts that run agents also run the amux daemon, a background process. |
| **Agent** | One coding agent: Claude Code in a terminal (`claude_pty`), headless Claude Code (`claude_sdk`), or Codex (`codex`). Each runs in its own process on one host, in the project directory you chose. |
| **Chat** | An agent's conversation, drawn the same way on every device. |
| **Fleet** | Every agent you can see, on this host and on the hosts you paired with, grouped by host, children beneath their parent. |
| **Profile** | One identity for amux on a machine: its own key, its own paired hosts, its own agents. An installation holds one profile per account, plus any you create yourself. |
| **Pairing** | The one-time step that makes two hosts trust each other. |
| **Relay** | A server that carries encrypted traffic between your hosts when they cannot reach each other directly. |

Every device is an equal client. The terminal client (bare `amux`) and the
iPhone app show the same fleet and the same chats and can do the same
things; neither is a remote control for the other.

## Running amux on a computer

`amux server start` starts amux in the background and `amux server stop`
stops it. On a desktop install you rarely need either: `amux init` asks
once whether amux should start when you log in, and any `amux` command that
finds amux stopped starts it and waits for it.

There, amux runs under a small supervisor (`amux supervise`) that restarts
it if it ever crashes and installs releases as they come out. `amux update`
asks it to check for a release now, and `amux config channel stable` or
`amux config channel preview` chooses which releases it follows. On a
server where a service manager runs amux instead, the service manager
starts and updates it, and commands wait for it rather than starting it.

Common commands, all acting on the selected profile:

```sh
amux                                  # open the terminal client: the fleet and chats
amux ls                               # list the agents, children beneath their parent
amux create claude_pty --prompt "…"   # start an agent in the current directory
amux send <agent> fix the flaky test  # send it a prompt
amux attach <agent>                   # hand this terminal to the agent's own interface
amux stop <agent>                     # stop its process
amux resume <agent> [message]         # start an exited agent again
amux delete <agent>                   # delete it, its history and its children
```

`amux attach` works only for agents on this machine: the agent's own
terminal never crosses the network. On any other device, every agent is a
chat.

## Agents outlive amux

Each agent is its own process, and it keeps running whether or not the amux
daemon is. The agent writes everything it does to a journal in its own
directory on disk; the daemon reads that journal and serves it to your
devices.

So when the daemon restarts, crashes, is stopped with `amux server stop`, or
is replaced by an update, nothing happens to your agents. Each one carries
on with its turn and keeps writing its journal. When a daemon comes back it
finds every running agent, reads what they wrote while it was gone, and
your devices catch up. A chat you had open shows the agent's work as if the
daemon had never left.

An agent waits for a daemon for a while (five minutes by default, set by
`agent.grace_secs` in the installation config; a change applies to agents
started afterwards). If no daemon returns in
that time, the agent finishes the turn it is on, writes it to its journal
and exits. Nothing is lost: the next daemon reads the finished turn, and the
fleet shows the agent as **exited · while the daemon was away**, with the
chat marking where it lost its daemon. Type a message in its chat, or run
`amux resume`, and it starts again where it left off with that message as
its next prompt. If the agent was waiting on a question for you when its
daemon disappeared, it waits a while longer (`agent.drain_secs`) and then
exits as "orphaned while waiting for you"; resuming it asks the question
again.

An agent never stops on its own because of anything short of that. The Stop
button in a chat interrupts the current turn and leaves the agent ready for
the next prompt; only `amux stop`, `amux delete`, or the provider itself
exiting ends the process.

## Accounts and profiles

You can use amux with no account at all. Hosts on the same network find
each other, pair, and work together without any server involved. SSH works
the same way.

An account at amux.sh adds the relay, so your hosts reach each other when
they are apart. `amux login` signs in through your browser; `amux logout`
signs out.

An installation keeps one **profile** per account. A profile is a complete,
separate amux: its own private key, its own list of paired hosts, its own
agents and history. Personal and Work on the same Mac share nothing but the
machine. Pairing belongs to a profile, so two machines that both carry
Personal and Work pair once for each; trusting a machine in Personal grants
it nothing in Work.

- `amux login` signs in the profile you name with `--profile`. Without one,
  it picks the profile already signed in to that account, else the profile
  that holds nothing yet, else it creates a new profile named after the
  account. A profile that already has agents or paired hosts asks before it
  is signed in to an account.
- `amux logout` signs the profile out. Its key, its paired hosts and its
  agents stay; signing in to the same account later picks up where it left
  off, with no pairing again.
- `amux profiles` lists the profiles. `amux profile create`,
  `amux profile rename` and `amux profile delete` manage them. Deleting a
  profile destroys its key, its trust in other hosts, and its agents.
- `--profile <label or id>` chooses the profile for any command; without it,
  commands use the first profile.

Every profile stays connected while amux runs, whichever one you are looking
at. A failed sign-in in one leaves the others working. Profiles keep amux's
own state apart; they do not sandbox the agents, which run as your user
with your user's access to the machine.

On the phone, each account you sign in to is a profile of the phone's own
amux. Signing out keeps the account listed with Sign In beside it, and the
machines on your network stay reachable. Removing an account from the phone
deletes that profile: its key and the machines it paired. Your account at
amux.sh is untouched.

## Pairing: trust you grant once, in person

Pairing is how a host comes to trust another. Until two hosts are paired,
neither can see or touch the other's agents, whatever account they share.

On the host you want to reach, run:

```sh
amux pair          # shows a six-digit PIN
amux pair --qr     # shows a QR code for the phone
```

Pairing mode stays open for five minutes, or until Ctrl+C. Then, from the
other side:

- **Another computer:** `amux peers` lists hosts found nearby. Run
  `amux pair <name>` (or `amux pair <address:port>`) and type the PIN.
- **The phone:** scan the QR code with the camera, or choose the host in the
  app and type the PIN.
- **Over SSH:** `amux pair user@host` pairs with the amux on that machine
  through your SSH login, with no PIN; the SSH login is the proof.

The PIN never crosses the network. Both hosts run a password-authenticated
key exchange (SPAKE2) that proves each is talking to the machine in front of
you; someone listening learns nothing they can use. A PIN works once and
expires with the pairing window, and five wrong guesses close the window.

For an unattended demo, such as an app reviewer who will never see your
screen, `amux pair --demo --pin 246810 --for 30d` holds a PIN you choose
open for a fixed time, at most 90 days. It is the same exchange, but the PIN
is reusable: pairing does not use it up and wrong guesses do not close the
window. The command returns at once; the window stays open until it
expires, `amux pair --cancel` closes it, or amux restarts. Treat the PIN as
a shared secret for that time, and use it only on a machine you can give
away.

No pairing window survives amux restarting. If amux restarts while you are
pairing, run `amux pair` again: it opens a fresh window with a new PIN.

What pairing produces is small and local: each host remembers the other's
public key. From then on, every connection between them starts with both
sides proving they hold the key the other remembers. Nothing else, not an
account, not a server, grants that trust.

A paired host can operate your agents in that profile: open their chats,
send prompts, start, stop and delete them. It cannot change who your machine
trusts, pair on your behalf, or stop your amux.

## Reaching a host

Hosts find each other on the local network by themselves. Finding a host
does not trust it: an unpaired host shows up only as something you can pair
with.

Paired hosts connect whichever way works, preferring the most direct:

| Route | When |
|---|---|
| **direct** | Both hosts are on the same network. |
| **ssh** | You paired over SSH; the connection runs through `ssh` to the other machine. |
| **relay** | The hosts are apart and both are signed in to the account, with a subscription. |

The fleet names each host's route beside it, or **offline** when nothing
reaches it. An agent on a host you cannot reach stays in your fleet as it
last was; its chat shows what you last saw and fills in again when the host
comes back.

Whatever the route, the connection is encrypted between the two hosts and
authenticated against the keys they exchanged when they paired. The route is
plumbing; it never decides who is trusted.

## What the relay can see and do

When two of your hosts cannot reach each other directly, the amux relay
copies their encrypted traffic between them. A host connects to the relay
while its profile is signed in.

What an account buys:

- **Signed in, no subscription:** your hosts see each other's presence
  through the relay, and you can tell a host that is up from one that is
  off, but the relay refuses to carry agent traffic or pairing between them,
  so a host only the relay reaches shows as away.
  Their agents are reachable on your network or over SSH.
- **With a subscription:** the relay carries agent traffic and pairing too,
  so your phone reaches your desktop from anywhere.

Direct connections on your network and SSH never consult the account.

The relay sees encrypted traffic and the metadata it needs to route it:
host names and ids, which account a host belongs to, when hosts come and
go, and how much traffic passes and when. It cannot:

- **read your chats** — the traffic is encrypted end to end between your
  hosts;
- **pretend to be one of your hosts** — it holds no key any of your hosts
  trusts, so it fails the check that starts every connection;
- **start agents or run commands** — those need a connection that ends
  inside a host you paired, which the relay cannot form.

A relay that misbehaved could drop or delay traffic. It would gain no power
to read chats or operate agents.

## Unpairing and signing out

**Unpairing** is immediate and needs no one's permission:

```sh
amux unpair <host>     # asks first; --force skips the question
```

Your machine forgets the other host's key. Every connection with it closes
at once, later attempts fail, and its agents leave your fleet. Other profiles keep
their own decisions.

On the host you unpaired, the fleet says so: the other host reads as
**offline**, the hosts list adds **no longer trusts this machine**, a banner offers
`amux pair <host>` to pair again, and on the phone a chat on that host says
"*host* no longer trusts this phone. Pair again to see this chat." Its
agents are out of reach there until the two pair again.

**Signing out** does not untrust anything; it only takes the relay away.
While this machine is signed out:

- hosts it reaches directly or over SSH work as before;
- hosts only the relay reached are out of reach, and the fleet says why in
  terms of this machine: **this machine is signed out**, with a banner
  suggesting `amux login` (on the phone, "This phone is signed out");
- a chat on such a host keeps your draft and waits until you sign in again.

When another host is the one signed out, the hosts list says **not signed
in** beside it wherever the relay was the way to it.

## Why this is trustworthy

- **Trust lives on your machines.** Each profile's private key never leaves
  the machine, and the list of hosts it trusts is a local file that no
  server holds or syncs.
- **One rule decides access.** Every connection starts with both hosts
  proving their keys to each other. The route underneath, direct, SSH or
  relay, never grants anything.
- **It is missing things on purpose.** No trust passed along ("a friend of
  a friend"), no central authority that can vouch for a host, no tokens
  that stand in for pairing. Each absence is a class of attack that cannot
  happen.
- **Revocation is local.** Unpairing takes effect on your machine the moment
  you do it.

For how the connection itself works, see the [protocol](PROTOCOL.md). For
how the pieces fit together inside, see the [architecture](ARCHITECTURE.md).
