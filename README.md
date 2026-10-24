<p align="center">
  <img src="images/brand/amux.svg" alt="amux" width="112">
</p>

<h1 align="center">amux</h1>

<p align="center">
  <strong>Your coding agents, on every machine you own.</strong>
</p>

<p align="center">
  amux brings every Claude Code and Codex agent on your computers into one place, in a
  beautiful terminal app and on your iPhone. See what each one is doing, answer the ones
  waiting on you, and let them hand work to each other, wherever you are.
</p>

```sh
curl -fsSL https://amux.sh/install | sh      # macOS, Linux
irm https://amux.sh/install | iex            # Windows PowerShell
```

<p align="center">
  <a href="https://apps.apple.com/us/app/amux/id6760197635"><img src="images/readme/app-store.svg" alt="Download on the App Store" height="44"></a>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="images/readme/hero-dark.png">
    <img src="images/readme/hero-light.png" alt="The amux terminal app and the amux iPhone app showing the same agents across three machines, with the phone approving an agent's request to run tests">
  </picture>
</p>

## Every agent, every machine, one list

Agents keep working on the machine you started them on: your desktop, a build server, your
laptop. amux shows all of them together, grouped by machine, with the agents that need you
at the top. Start a new agent on any of your machines, with any model, from the same place.

Agents carry on when amux restarts, updates or crashes, and every device catches up when it
comes back.

## Never leave an agent waiting

An agent that stops to ask for permission, present a plan or ask a question sits idle until
you answer. With amux every agent waiting on you is at the top of the list on your phone, and
you can approve the command, accept the plan or answer from wherever you are. The phone is a full
client, not a remote control: it shows the same agents and chats and can do everything the
terminal can.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="images/readme/phone-dark.png">
    <img src="images/readme/phone-light.png" alt="The iPhone app: every agent on three machines, and a Codex agent's request to run the sync tests waiting for Allow or Deny" width="720">
  </picture>
</p>

## A terminal app worth living in

amux's own terminal app is fast, easy to use, and built to be looked at all day. Claude and
Codex chats read the same way, the whole fleet is one key away, and it comes with the tools
you reach for between prompts:

- **Code review.** Read the agent's whole diff and comment on lines or files; the comments go
  back to the agent with your next message.
- **An overview** of what the agent has in flight: its tasks, background jobs, changed files
  and how close it is to its usage limits.
- **Attachments.** Paste a screenshot or a long log and the agent gets it whole.

It takes its colours from your terminal's theme. Prefer Claude's own interface? `amux attach`
hands your terminal straight to it.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/readme/review-dark.png">
  <img src="images/readme/review-light.png" alt="The review page: changed files on the left, the diff with a comment on the right">
</picture>

## Agents that work together

Every agent can see the others, message them and start subagents of its own, on any machine
you've paired. Claude can hand a task to Codex, and Codex to Claude: a Claude agent
redesigning checkout can start a Codex agent on your build server to write the payment tests
and another Claude to rewrite the error messages, and hears back from each one when it's done.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/readme/chat-dark.png">
  <img src="images/readme/chat-light.png" alt="A Claude agent's chat: it starts two subagents, then picks up the report its copy-writing subagent sent back">
</picture>

## Private by design

- **Your code stays on your machines.** Agents run where you started them; amux carries
  their chats between your devices.
- **End-to-end encrypted.** Every connection is encrypted between your own machines and
  checked against keys they exchanged when you paired them. The relay passes on traffic it
  cannot read.
- **Trust you grant in person.** Machines trust each other only once you pair them, with a
  PIN or QR code on the screen in front of you. No account or server can grant access, and
  unpairing takes effect at once.

[How amux works](docs/HOW_IT_WORKS.md) explains what each device, the account and the relay
can and cannot do.

## Get started

```sh
curl -fsSL https://amux.sh/install | sh   # install
amux                                      # open the terminal app
amux pair --qr                            # pair your phone: scan the code with the camera
amux pair desk                            # or pair another computer found nearby
```

## Free, with an optional paid tier

amux is free. Your machines find each other on the same network, or connect over SSH, with
no account at all. The paid tier adds the amux relay, so you can reach your agents from
anywhere, securely, when your machines are apart.

## Supported

| | |
|---|---|
| **Agents** | Claude Code (in amux's chat or its own terminal interface), Codex |
| **Computers** | macOS, Linux, Windows |
| **Phone** | iPhone |

## Learn more

- [How amux works](docs/HOW_IT_WORKS.md): devices, accounts, pairing and the relay
- [The terminal app](docs/TERMINAL.md): screens, keys and commands
- [All documentation](docs/README.md)

## License

amux is source-available under the [Functional Source License](LICENSE) (FSL-1.1-ALv2):
free to read, build, modify and use for anything except competing with us, and Apache-2.0
two years after each version is published. [docs/LICENSING.md](docs/LICENSING.md) explains
it.
