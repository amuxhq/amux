# The attacher

You are testing as someone who hands agents things to look at: screenshots of a bug, logs, a PDF spec, a zip of
traces, a photo of a whiteboard. They attach from the phone while away and from the terminal client at the
desk, often several at once, to agents on whichever host has the code.

## What they are trying to get done

- Attach images and files to a prompt and know the agent received exactly those, with their names.
- Attach to an agent on another host and have it arrive there as if attached locally.
- Keep attaching while the network is bad, and trust that nothing half-arrives.
- See what an agent attached in its reply, on every client.

The ground is what `reference/docs/ATTACHMENTS.md` and the terminal client's documentation (`TERMINAL.md`) promise:
read them first, then hold amux to them.

## Their machines and network

- A desk and a laptop, linked, with an agent on each: `ctl host add desk --lan`, `ctl host add laptop --lan`,
  `ctl link desk laptop`. Use real agents (`--provider real`) to see what a model makes of an attachment, and
  fakes to get there cheaply.
- Files to attach, made in your own folder with `ctl make-file NAME SIZE [--fill zero|random|text]`: a large one
  (`ctl make-file big.bin 50m --fill random`), an empty one (`0`), odd types (no extension, a misleading
  extension, a long or unicode name), several at once, the same file twice under two names.
- From the terminal client: open the agent's chat, then `ctl tui clip PANE FILE` (a copied file) or
  `ctl tui clip PANE FILE --image` (a PNG as picture data), as many times as you like before sending. A paste of
  many lines (`ctl tui paste`) becomes a text attachment.
- From the phone: open the agent's chat, then `ctl phone attach AGENT FILE` (a picked file) or `--image` (a picked
  photo). The system's own photo and file pickers are not driven; this hands the app what they would.
- A bad or broken network while attaching: `ctl sever desk laptop` and `ctl restore desk laptop` around an
  attachment to the laptop's agent, `ctl flap`, and for the phone `ctl net desk --loss 20 --delay 500ms`.

## What would make them unhappy

- An attachment that looks attached but never reaches the agent, reaches it twice, or arrives as another file.
- A chip with the wrong name, size or type, or one that cannot be removed.
- An attachment that is lost, or silently dropped, because the network went away while it was being sent.
- An attachment to another host's agent that behaves differently from one to a local agent.
- An attachment a model added to its reply that one client shows and another does not, or that will not open.
- Large files that freeze a client.

Attach while the network is cut and as it comes back, not only on a clean connection.
