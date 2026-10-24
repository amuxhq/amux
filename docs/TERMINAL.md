# The terminal client

*For people using amux from a terminal, and developers changing `crates/tui` or the `amux` command line.*

The terminal client is the fleet and the chat in a full-screen terminal interface. Running `amux` with no verb
opens it. It is a library ([`crates/tui`](../crates/tui/src/lib.rs)) that the `amux` binary
([`crates/amux`](../crates/amux/src/main.rs)) calls, never a second executable, and it is built on the shared
client library described in [CLIENT.md](CLIENT.md): it reads the local runtime only through the session and fleet
drivers, composes the view values (fleet rows, chat rows, asks, composer tokens) in its own layout, and owns only
the overview pane, the scroll anchor, opened rows, drafts and focus. Nothing it holds outlives the process except
the overview's layout, kept per terminal in `<root>/tui/layout.json`.

The same binary carries the command-line verbs, listed [at the end of this page](#command-line-verbs).

## Starting it

```sh
amux                        # the terminal client on the first profile
amux --profile work         # on a profile named by label or id
amux --config path/to.yaml  # with another installation config (or AMUX_CONFIG)
```

`amux` connects to the selected profile's local socket with the gRPC client, opens home, and draws. If the daemon
is not running, start it with `amux server start` (see [SUPERVISOR.md](SUPERVISOR.md)).

The client redraws only when something changed: a key, a change from the fleet or the open chat, or a finished
task. It ticks on a clock only while something on screen moves with time: every 250 ms while the open agent is
working (the running turn's elapsed time) or its chat is still empty, and at the end of the quit guard or a
notice. See [the two draw loops](CLIENT.md#the-two-draw-loops).

## Screens

Every screen has a top line, its content and a line of keys. A margin of two columns runs down each side, and
every edge sits on it: tinted blocks (your messages, the highlighted row on home), the composer's box, the header,
the key line and the arrows of sections and runs. Text inside a block, the agent's text, labels and marks sit
two columns further in. One blank line sits above the top line and one below it. A block's half line of padding
is drawn only while its words are on screen, so a block scrolled half off leaves no band behind.

Ink goes by importance, in four steps mixed from the terminal's own text and ground: brightest for what the eye
should land on (names, the key in a legend), normal for what you act on, grey for what you read, faint for what
you look up (ages, folders, hosts, counts). Colour marks meaning only: the attention ink (the terminal's blue) for
what needs you, red for failure, the terminal's cyan for code and paths inside the agent's text, and green and red
for lines added and removed wherever you read a diff. Anything that looks like a control can be clicked, and every
key in a key line too.

### Home

The client opens on home, and every chat returns to it. The top line is `amux` and the overall state at the
right: hosts it does not reach now, how many agents are working or starting and how many need you (agents, not
families, folded or not), a daemon running another build (`amux 0.8.0 running · restart to update`) and, when
this machine's hosts are out of reach because it is signed out, `signed out · amux login`. `/` turns the top line into a filter over names, folders and hosts; Enter keeps it, Esc
clears it, and it looks into folded sections too. It matches names, branches, folders and hosts.

Below it, `+ New Agent`, then the agents in sections drawn alike (a fold marker, the label, a count, a faint rule):
**Needs you**, **Live** (starting, working and idle) and **Exited**, folded until opened. Within a section agents are
listed by when each last changed state (a turn starting or ending, an ask opening or answered), newest first, so a
row never moves while an agent streams. The sections, their order and each row's second line come from the shared
fleet view every client draws. A family sits in its loudest member's section;
`→`, `←` or Space shows and hides its members, indented under their parent.

Each row is the agent's mark (`○` idle, starting, exited or on a host that is away; `●` working; `●` in the
attention ink when it needs you), its name, its branch when it works in a repository, then faint which agent it is
(`Claude`, `Claude (terminal)` or `Codex`), its folder from `~` and its host when that is not this machine, and at
the right how long it has been in its state. Short of room, the folder is cut from the left down to its last name,
then the host goes, then the agent. The second line says what the agent's state calls for: what it asks (`wants to
run cargo test -p auth`), the step it is running, what it last said, or why it cannot go on (`signed out of
Codex`, `usage limit reached`); why it exited (`finished`, a crash in red); that its host is away. A starting
agent has no second line. A folded family whose member needs you says that member's name and what it asks. The
highlighted row is tinted, with half a line of the tint above and below; under the pointer or the
keys alike, and it shows `[x]` in place of its age to stop the agent, or delete it once it has exited, after asking.
A terminal that gives no background to tint marks it with `›` instead. `+ New Agent` and the section headings
highlight the same way, tinted or marked, so a keyboard user always sees where Enter lands.

On a row, `r` renames it in place (Enter saves, Esc cancels), `s` stops it and `x` deletes it, each asking first
in a small window (Enter confirms, Esc cancels). A daemon that has not answered yet shows `connecting` at the
right of the top line.

### A new agent

`n`, `+ New Agent`, or the leader then `n` from a chat start a new agent. What opens depends on where the person
chats with their agents, the installation's `ui.chat_in`:

- **In amux** (`amux`, the default): the composer, first. Type what the new agent should work on and Enter starts
  it and opens its chat (Ctrl+Enter starts it and stays home). The composer's bottom edge is the settings:
  `+ Name │ Claude · Opus (high) · ask │ ~/source/amux │ laptop`, that is the name (named automatically
  unless given one), the agent with its model, effort and permission (and Codex's mode when it is not the default),
  the folder and the host. `ctrl+s` then a letter opens one in a small panel rising from its place on the edge: `n`
  name, `m` model, `e` effort, `p` permission, `d` folder, `h` host, `a` agent (Claude or Codex); clicking an item
  does the same. The models, efforts, permissions and modes are what the chosen host says that agent offers there,
  asked of the host before any agent runs on it; until it answers a model can be typed by name. Another model starts
  at its own default effort, and a permission it does not take gives way to the normal one. A provider the host
  says is signed out reads `Codex (not signed in)`. Long lists filter as you type; the folder's panel lists that
  host's recent folders and takes a typed path; hosts that are away cannot be picked. Shift+Tab steps Codex's mode,
  or Claude's permissions that still ask, as in a chat. Claude runs headless here, Codex on its app server.
- **In each agent's own terminal** (`terminal`): a small window over home with the name, the agent (Claude or
  Codex, each marked `not signed in` where the chosen host says so), the folder and the host, and no prompt or model,
  which the agent's own terminal sets. Enter moves from row to row and starts the agent from the last; ←/→ change
  a choice. Started on this machine, the terminal is
  handed straight to the new agent (see [raw attach](#raw-attach)); started on another machine its chat opens
  instead, and says why.

A new agent always shows the values it will start with: each agent's model, effort and permission come from the
installation's `new_agent.claude` and `new_agent.codex`, and the leader then `n` from a chat starts from that
chat's agent, folder and host instead.

### Hosts

`h`, or the leader then `h` from a chat, opens the hosts window over home: every trusted host whatever its
presence, with `this machine`, how it is reached (`direct`, `relay`) or why not (`offline · not signed in`, `offline ·
this machine is signed out`, `offline · no longer trusts this machine`, `away` and the free plan's words when the relay
refused the host for the plan). Hosts that local-network discovery sees and that are
not yet trusted are listed faint, with the command that pairs them (`found nearby · amux pair desk`); a faint line
says to pair another with `amux pair`. Esc, `q` or `h` closes it. Pairing, unpairing and signing in happen from the
command line ([`amux pair`](#command-line-verbs) and friends); [HOW_IT_WORKS.md](HOW_IT_WORKS.md) explains hosts,
pairing and the relay.

### The chat

Enter on a row opens its chat. From top to bottom:

- **The header**: the agent's name, a faint `│`, then faintly the folder it works in (`~/source/amux`) and, when it
  runs on another machine, `· <host>`. At the right, split by the same faint `│`: how much of its context is used
  (`41K / 200K`, in the warning ink near the limit), `[Diff +42 −7]`, which opens the review page, and `[Home]`.
  The change totals come from the agent's row, as of its last turn end, for the comparison the person chose: the
  uncommitted changes (`[Diff +42 −7]`), or everything since the branch left its base, uncommitted work included
  (`[Diff vs main +120 −30]`). The leader then `c` switches between the two; the choice is the terminal's, kept
  with its layout, and the branch comparison needs a base branch to count against. The
  header says where the chat stands only when that is a problem: `exited` with its cause, `catching up` before the
  first CaughtUp, `reconnecting`, `refreshing` while a rebuilt history is on its way, or why the host is away (not
  current, this machine signed out, or the host no longer trusts this machine).
- **The family line**, for an agent with a parent or children: the parent, the number of subagents and how many
  others in the whole family need you, grandchildren and the parent's side included.
- **The feed**, read one turn at a time, laid out upward from the newest or from wherever the reader scrolled:
  - your message is a tinted block with the time at its right; pasted text shows in place, cut after eight lines
    with `… N more lines [Show All]`;
  - the message that started the turn owning the feed's top line stays pinned under the header, cut to one line,
    as you scroll through a long answer; as the next turn's message nears the top the pinned one fades, then that
    message takes the pin. Clicking it scrolls to the message;
  - everything the agent wrote is in one reading ink, at most 100 columns a line; code and paths wear the
    terminal's cyan, and code blocks are highlighted in its palette. A single line break in its text stays a line
    break, as Claude's and Codex's own interfaces draw it; a blank line parts paragraphs. Thinking is never drawn;
  - each run of tool steps between two pieces of its text folds to one faint line of what the steps did
    (`▸ 3 commands · 2 edits · 4 reads`). Clicking it, or the leader then `o`, opens it to one line per step,
    verb first (`Ran just lint · exit 1 · 22s`, `Edited session.rs · +0 −1`); clicking a step opens its detail: a
    command's last lines of output, an edit's patch, a call's result;
  - a failure the turn ended without fixing stays in view in red where it ran, above its folded run's line; one
    fixed later in the turn folds away with the rest;
  - while the agent is at a run, its newest three steps show as they happen, the current one bright, and the
    running turn ends in `Working · 33s`, which becomes a faint `Worked 6m` when the turn ends;
  - a plan is drawn as the agent's text under a landmark that folds like a run (`▾ Plan · Move the
    journal`), and once decided folds to `▸ Plan · Move the journal · approved` or `· sent back` with the note
    under it;
  - session boundaries (`started`, `resumed`, `exited · crashed`) read like home's headings: faint words and a
    hairline.
- **The queue**, just above the composer: prompts waiting behind the running turn, one line each, faint on your
  message's surface, saying how each waits (`queued`, `queued from relay` from another agent, `sending into this
  turn`, `waiting for desk…`, `may not have arrived`).
- **The row above the composer**, when there is something to say: the task in progress and `3 of 4 tasks`, how
  many tool servers need signing in (`2 tool servers need sign-in`, in the warning ink) or failed to start (in
  red), and `ctrl+o` for the overview. The tool servers are counted when the chat opens and again only when
  another one fails; a send or opening the overview puts them away, and the overview keeps each by name.
- **The composer**, boxed, with `Model (effort) · mode` on its bottom edge in the words a person reads (`Opus
  (high) · accept edits`). The model is the one running: Claude's `Default (recommended)` reads as the model it
  stands for (`Opus 5`). The mode is left out while it is the agent's normal one, which asks before acting
  (Claude's and Codex's `default`); otherwise it reads `auto`, `accept edits`, `plan`, `bypass permissions`
  (Claude) or `read only`, `full access`, `plan` (Codex), and `custom` for a Codex pair outside its presets. Its
  top edge says what stands in the way of sending, most pressing first: a prompt the
  agent refused (`not sent: its host is shutting down`, in red), the host away (`desk is away`), an exited agent (`Enter
  resumes`), a usage limit reached (`5-hour limit reached · resets 23:24`, in the warning ink; sending stays
  open). One blank line below it, the key line: `shift+tab permission · ctrl+a more` at rest
  (`shift+tab mode` for an agent with modes), `enter queue · ctrl+x
  interrupt` added while the agent works, `enter resume` once it has exited, `enter holds it · it sends when the
  host is back` while the host is away (or once caught up, once amux is back, once this machine signs in). What everyone knows (Enter sends, pasting attaches) is not said.

The composer takes a draft at any time. Enter sends while the chat is current and the agent is live; while the
agent works it queues the prompt behind the running turn; when the agent has exited it resumes it with the draft as
its first prompt (an empty Enter just resumes). A prompt to an idle agent shows at once in the feed, and one to a
busy agent at once in the queue. A prompt sent while the chat is not current (the host away, the chat catching up,
this machine's daemon reconnecting) is held: it shows pending in the queue, `sends when desk is back`, with
`[Cancel]`, and goes by itself under its own id once the host can be reached. A draft left in a
chat for home or another chat waits in its composer until the chat opens again; one cleared before leaving stays
cleared, even before this machine's daemon has forgotten its copy, and quitting waits until it has. A prompt the agent
refuses, an exited agent's included, comes back into the composer with the reason, worded from the typed reason
the view hands over, the same as the phone's; one whose connection dropped before a reply waits in the
queue as `may not have arrived`, to resend or discard, and nothing is resent by itself. A prompt held while its
host is away is in the draft this machine's daemon keeps from the moment it is held; one held for a chat that is not
open goes back to that chat's draft if its resend is refused or its agent exits first, and comes back into the
composer when the chat opens.

Scrolled back, a control floats at the bottom centre of the feed, `↓ Jump to Bottom  ctrl+end`, or `↓ 3 new` when
rows arrived meanwhile; clicking it or Ctrl+End returns to the newest. Because the client reads the mouse, the
terminal's own text selection takes its modifier: Shift+drag, or Option+drag in iTerm2 and Ghostty. A mouse-wheel
event scrolls one line, so a trackpad's stream of small events moves smoothly; events arriving close together in
one direction step up to two and then three lines (`tui::wheel`, shared by the chat, the review page and home).

An empty chat says it is loading after 300 ms. Scrolling up keeps the reader's place while rows arrive below; the
client asks for older rows by itself when fewer than a page (40 rows) is held above the screen.

**Attachments.** Ctrl+V reads the clipboard: an image or a copied file is stored with the agent and inserted at the
cursor as a chip in the person's own words (`[screenshot.png · 240 KB]`); text is pasted. A paste of eight lines or
more, or of 1000 characters or more, becomes a pasted-text chip (`[pasted-1 · 23 lines]`). A chip keeps a space
after itself; one Backspace removes a chip with its attachment. [ATTACHMENTS.md](ATTACHMENTS.md) covers what happens
to attachments after that.

### Asks

When the agent waits on you, the ask takes over the composer's box, its edge in the attention ink; the draft waits
behind it and comes back after. For half a second after an ask is first drawn, keys still go to the draft (Enter
does nothing), so a digit or Enter typed as it appears never answers what was not read. The plan or the call it is
about stays in the feed above. A typed answer or note shows its own line breaks, its last four lines at most.

- **A permission** (`● Wants to run`, `Wants to edit`, `Wants to create`, `Wants to use`): the command, the file
  with its diff and the line of its first change (relative to the agent's folder when it is inside it, and cut
  from the front when too long, so the file's name stays), or the tool and its arguments, then numbered choices worded as what happens (`1. Yes`, `2. Yes,
  and always allow cargo test in this project`, `3. Yes, and don't ask again this session`) and the way out last
  (`No`). Where the agent takes a reason with a refusal, Claude's, Tab on `No` adds a note that goes back with it;
  Codex's refusal carries no reason, so its `No` offers no note. A long diff or command is cut,
  with `[Full Diff]` or `[Show All]` (or `f`) opening the whole of it inside the box.
- **A plan** (`● Plan ready`): `Yes, start building`, `Yes, and switch to <permission>` where the agent
  offers it (Claude's moves to auto mode where Claude offers it, to accept edits otherwise; Codex offers no switch), and `No, keep planning`, which takes a note.
- **Questions**: one at a time; several get a row of tabs (`‹ Layout   Entry   Review ›`), answered ones ticked,
  and end on a review that lists each answer and sends them all. Options read `label · recommended ·
  description`; several picks are `[ ]` boxes under `Select all that apply`; options with previews show the
  highlighted one's preview beside them. `Something else · enter to type` opens a field for an answer of your own.
  Secret answers type as dots. `Skip` leaves a question unanswered and Tab adds a note to it, each where the agent
  takes one; `Reply instead` sends your own words in place of the answers. Terminal Claude takes no note, and a
  skip only when it asks several questions: Claude's own form has a review screen only then, and submits a lone
  question as soon as it is answered.
- **A tool server's form** (`● linear needs details`): its message, then each field as a step in the server's
  order (a choice, Yes / No, boxes, or a typed value), starting from the server's defaults with Yes / No on No,
  required ones marked, and a review with Submit; `Decline` is the way out. A value the form cannot take says
  why beside it (`needs a whole number`, `needs at least 1`), and the review names the first field that keeps the
  form back.
- **A tool server's link** (`● grafana sent a link`): the server's own message and the address, `1. Open the
  link` (the box stays), `2. Done`, and `Decline`. The provider says nothing of what a link is for, so the
  record after it (`Opened grafana's link`) carries the message's first line.
- **Wider access** (Codex): what it wants and why, `Allow for this turn`, `Allow for this session`, `No`.
- **What this client cannot answer** (a dialog only Claude's own terminal shows): the reason, and `Open Claude's
  terminal` where that terminal can be attached here.
- **Sign-in**: when the agent's account needs signing in again, the box says so with the provider's message and
  where to do it (`Run claude and sign in with /login on desk`); nothing here can sign in, so it offers no choice.

After an answer the box shows one sending line; if the connection dropped before a reply, `r` resends and `d`
discards. Answered, an ask becomes a step in the feed: `Answered 2 questions` with each question and `→ the
answer`, `Sent the form to linear` with the fields it carried, `Signed in to grafana`, `Granted … · for this
turn`.

### The overview

`ctrl+o` opens the overview beside the chat (over it on a narrow terminal): what the agent has in flight, in
sections like home's, each foldable: its tasks (`✓` done, `●` current, `○` to do); each background job still
running, its command with how long it has run (a subagent or monitor the provider runs itself shows its
description after `subagent` or `monitor`), which opens the step that started it; the changed files of the
comparison the header counts, root files first and then each folder with its files and their lines added and
removed, each opening the review page at that file; its tool servers that need signing in or failed; and, near a
usage limit, every usage window with its name (`5-hour limit`, `Weekly limit`, `Fable weekly limit`), how full it
is, its state and when it resets, and Codex's credits. The changed files are asked for without a patch while the
overview shows, again when the row's totals move or the comparison changes. It stays open across chats until
closed, and remembers its folds per agent.

### The review page

The leader then `r`, or `[Diff]` in the header, asks the agent's host for the diff the header counts and opens it as a
full-screen review page, frozen as it was when the page opened. The header names the agent and the folder, with
`review · working tree at 3f2a1c9 │ 42 files · +109 −74` at the right. The changed files are listed on the left,
grouped by directory with each file's lines added and removed; beside them one stream of every file in that order,
each under its own landmark, with one column of line numbers (a removed line by its old number, every other by its
new one, as on the phone), the lines left out between two hunks drawn as a faint dashed rule, and its added and
removed lines tinted.
The file the stream is in is highlighted in the list; picking a file in the list takes the stream to it. Tab moves
the keys between the list and the stream, and `/` filters the list and the stream with it. Below 100 columns the
list shows only while it has the keys.

You move line by line, leave comments on lines or on whole files, and go back to the chat; the comments ride in the
draft as one review chip, which the page keeps in step as comments are saved and deleted. The leader then `r` again
with a review already in the draft returns to that page rather than reading a fresh diff. The page opens only while
the chat is current, and says so when the working tree has no changes. [ATTACHMENTS.md](ATTACHMENTS.md) describes
the review attachment and how the agent receives it.

### Reporting a problem

`b` on home, or the leader then `b` in a chat, freezes the screen and opens a report over it: drag with the mouse
over anything that looks wrong to mark it, write a note on each mark and one overall, and Enter writes the report
into the installation's `reports/` directory and says where; Esc leaves without writing. [DEBUGGING.md](DEBUGGING.md#a-report-from-the-terminal)
lists what the report holds.

### Settings

The terminal client has no settings screen; everything it offers comes from the agent's catalogue. Shift+Tab
moves the agent to its next mode where it offers modes (Codex: default and plan), and otherwise to its next
permission that still asks before acting (terminal Claude: Claude's own cycle key). `ctrl+s` then `m`, `e` or `p`
changes a running agent's model, effort or permission where it lets a client change them, and otherwise says how
(terminal Claude's model and effort change by typing its own `/model` or `/effort`, its permission only by
cycling); each model in the list carries the agent's own line on it, which says what an alias such as Claude's
Default stands for. The composer's edge names the permission and the mode when they are not the agent's normal
ones, and Codex settings that match no named permission read custom; the phone's model chip says the same. The
phone's settings sheet offers every model, effort, permission and mode the agent offers.

The terminal's appearance, its leader key and how new agents start come from the installation config file
(`~/.config/amux/config.yaml` unless `--config` or `AMUX_CONFIG` names another), read by
[`crates/settings`](../crates/settings/src/lib.rs):

```yaml
keybinds:
  leader: ctrl+a      # ctrl+<a-z>
ui:
  theme: terminal     # terminal, dark, light, or a path to a YAML theme file
  color: auto         # auto, truecolor or ansi
  chat_in: amux       # amux, or terminal: chat in each agent's own terminal
new_agent:
  claude: { model: opus, effort: high, permission: default }
  codex: { model: gpt-6.1-sol, effort: medium, permission: default }
```

`theme: terminal` asks the terminal for its own colours at startup and derives the palette from them, falling back
to the shipped dark palette when the terminal does not answer. A theme file's path is resolved beside the config
file; its `tokens:` may name any semantic token directly (`attention: "#5f87d7"`). `color: auto` reads `COLORTERM`
and `TERM` to choose truecolor or ANSI, and `NO_COLOR` turns colour off.

## Keys

Keys come in three layers. **amux** keys are about amux itself (home, a new agent, hosts, reporting, leaving): on
home they are bare letters, and in a chat the leader then a letter, mostly the same one (`<leader> s` goes home,
while `s` on home stops an agent). **This chat** keys act on the open chat (review, attach, focus rows), also under
the leader. **Local** keys belong to whatever has focus (an ask, a panel, the overview, home's list, a window) and
never leak out of it. Control chords are kept for speed and habit only: Ctrl+X stops the turn, Shift+Tab steps the mode or permission, Ctrl+C
clears the field, Ctrl+O opens the overview, Ctrl+S opens the settings letters. Esc backs out one level and never
stops the agent or answers an ask; clearing a field and stopping the agent are never the same key.

`?` on home, or the leader then `?`, shows every key; j/k or the wheel scroll it and any other key closes it.

### Everywhere

| Key | Action |
|---|---|
| Ctrl+C | Clears the field that has the keys, if it holds text (Ctrl+Y brings it back). On nothing, press it twice within three seconds to quit. |
| Ctrl+A, then a key | A leader chord; see [the leader key](#the-leader-key). |

### Home

| Key | Action |
|---|---|
| ↑ ↓ or `k` `j` | Move the highlight |
| Home End or `g` `G` | First or last |
| Enter | Open the chat; on a section heading, fold or unfold it |
| `a` or Ctrl+Enter | Attach to the agent's own terminal (agents on this machine only) |
| `/` | Filter by name, folder or host |
| → ← or Space | Show or hide a family's agents |
| `n` | A new agent |
| `r` | Rename, in place |
| `s`, `x` | Stop, or delete, asking first |
| `h` | Hosts |
| `b` | Report a problem |
| `d`, `q` | Leave for the shell; agents keep running |
| `?` | Every key |

### Chat

| Key | Action |
|---|---|
| Enter | Send; queue while the agent works; resume an exited agent |
| Shift+Enter, or Ctrl+J in any terminal | New line |
| Ctrl+V | Attach an image or file from the clipboard |
| ↑ in an empty composer | Into the queue |
| Ctrl+X | Stop the turn; the agent stays live and idle, and the draft is untouched |
| Shift+Tab | The agent's next mode, or where it has none its next permission |
| Ctrl+S then `m`, `e` or `p` | The agent's model, effort or permission, where it can change from here |
| Ctrl+O | The overview |
| Page Up, Page Down, mouse wheel | Scroll |
| Ctrl+Home, Ctrl+End | The oldest held row; follow the newest |
| Esc | Back out one level: a panel, a field, the overview's keys, focus |

The draft editor takes readline keys: Ctrl+B and Ctrl+F move by character, Ctrl+Left and Ctrl+Right by word,
Ctrl+E and End to the line's end, Home to its start, Ctrl+P and Ctrl+N (or ↑ and ↓) between lines; Ctrl+W, Ctrl+U
and Ctrl+K kill the word before, the line before and the line after the cursor; Ctrl+D deletes forward; Ctrl+Y
yanks the last kill back.

In the queue, ↑ and ↓ move and the highlighted prompt shows its controls; Enter does the first and Backspace the
second: on a queued prompt, send it now into the running turn, or withdraw it back into the composer; on one that
may not have arrived, resend or discard it. Esc or typing returns to the composer.

### Asks

| Key | Action |
|---|---|
| ↑ ↓, `j` `k`, `1`–`9` | Move, or pick at once by number |
| Enter | Choose; on a text field, answer and move on |
| Space | Tick a box where several can be picked |
| ← →, Tab, Shift+Tab | The next or previous question or field, moving on without answering |
| Tab on a question | Add a note to it, where the agent takes notes; ← → still move between questions |
| Tab on a refusal | Add a note to it |
| Esc | To the way out (`No`, `Decline`); in an open field, clear and close it |
| `f` | Open or close the whole diff or command inside the box |
| Ctrl+X | Stop the turn, from anywhere |

### Review page

| Key | Action |
|---|---|
| `j` `k`, ↓ ↑ | Next or previous line |
| Page Down, Space, Page Up | A screen at a time |
| `g` `G`, Home End | First or last line |
| `J` `K` | Next or previous hunk |
| `]` `[` | Next or previous file |
| `c` | Comment on the line (on a file's landmark, the whole file) |
| Enter | Edit the newest comment here |
| `d` | Delete the newest comment here |
| Tab | Move the keys between the file list and the stream |
| `/` | Filter the files |
| `q`, Esc | Back out: the filter, the list, then the page |

While writing a comment, Enter saves it, Ctrl+J adds a line, and Esc cancels.

## The leader key

The leader is a control key, Ctrl+A unless `keybinds.leader` names another `ctrl+<letter>`. Pressed on its own it
waits for one more key. In a chat, after a pause of 400 ms the next keys rise in a panel from the key line's
`ctrl+a more`, in two groups, amux then this chat; a chord typed quickly never draws it, and each key in it can be
clicked.

| Chord | Action |
|---|---|
| `<leader> s` | Home |
| `<leader> n` | A new agent, starting from this chat's agent, folder and host |
| `<leader> h` | Hosts |
| `<leader> b` | Report a problem |
| `<leader> d` | Leave for the shell; agents keep running |
| `<leader> ?` | Every key |
| `<leader> r` | Review the working tree; comments go in the draft |
| `<leader> a` | Attach to the agent's own terminal, when it is on this machine |
| `<leader> k`, `<leader> j` | Focus an older or newer row |
| `<leader> o` | Open or close the focused row or run |
| `<leader> y` | Copy the focused row (through the terminal's OSC 52 clipboard) |

During raw attach the leader is read out of the bytes you type: `<leader> d` detaches to the shell, `<leader> s`
returns home over the attached agent, and a leader followed by any other key goes to the agent exactly as typed.
The leader is recognised both as its control byte and in the form terminals send when an agent has switched on the
kitty keyboard protocol.

## Raw attach

Raw attach hands the terminal to an agent's own interface: terminal Claude's own screen, or a Codex terminal view of
the agent's thread. It is reached with `amux attach <agent>`, with `a` or Ctrl+Enter on home, with the leader then
`a` in a chat, by starting an agent for someone who chats in each agent's own terminal, or from an ask this client
cannot answer.

Terminal bytes never travel over the wire. The client finds the agent's directory by convention,
`<profile directory>/agents/<agent id>/`, and connects straight to its `pty.sock`; the daemon is not involved.
[AGENT_PROCESS.md](AGENT_PROCESS.md) describes the socket, including how terminal Claude's screen is replayed to a
late attacher and how each Codex attach gets its own view of the thread. Three refusals come before any socket is
tried:

- **An agent on another host.** Its terminal is on another machine. `amux attach` says so and suggests opening its
  chat with `amux` and Enter; home shows the same notice.
- **A stopped agent.** It has no process and so no terminal; the refusal says it is stopped and how to resume it.
- **Headless Claude** (`claude_sdk`). It has no terminal; the refusal points at its chat.

While attached, typing and resizes go to the agent. How it ends:

| Ending | `amux attach` | From the terminal client |
|---|---|---|
| `<leader> d` | Prints `[detached from <name>]` and returns to the shell | Leaves the client for the shell |
| `<leader> s` | Opens home over the attached agent | Returns home |
| The agent ends the connection | Prints `[<name>: <why>]` or `[<name> ended]` and returns to the shell | Leaves the client for the shell |

Only `<leader> s` comes back home. Leaving for home keeps the connection and a model of the agent's screen, so
attaching to the same agent again repaints it where it was rather than starting over; picking another agent opens
a second connection. The agent is drawn on the terminal's alternate screen, so leaving gives the shell its own
screen back rather than the agent's last frame. The terminal is restored whichever way attach ends, including the
modes the agent's interface switched on.

## Command-line verbs

Every verb takes the global `--config <path>` (or `AMUX_CONFIG`) and `--profile <label or id>`. An agent is named
by its name, `name@host` where agents on two hosts share a name, its id, or the short id `amux ls` prints.

| Verb | What it does |
|---|---|
| `amux` | Opens the terminal client. |
| `amux ls` (`list`) | Lists the agents with children beneath their parent: name, short id, kind and state. |
| `amux create <kind>` (`new`) | Starts an agent: `claude_pty` (also `claude`), `claude_sdk` or `codex`, with `--name`, `--cwd` (the current directory otherwise), `--model`, `--prompt` for its first prompt, and extra arguments for Claude after `--`. |
| `amux attach <agent>` | Hands this terminal to the agent's own interface; see [raw attach](#raw-attach). |
| `amux send <agent> <text…>` | Sends a prompt and prints whether the agent took it, and if not, why in a sentence. |
| `amux stop <agent>` | Stops the agent now, and every descendant follows on its own host: its turn is cancelled the provider's own way (a question waiting on you is dismissed) and it exits, to be resumed later. |
| `amux resume <agent> [text…]` | Starts an exited agent again, with an optional first prompt. |
| `amux rename <agent> <name>` | Renames an agent. |
| `amux delete <agent>` (`rm`) | Deletes an agent and its history; its children follow on their own hosts, a host that is away once it is back. |
| `amux dump [agents…]` | Writes a debug bundle for the named agents, or all of them, with an optional `--reason`, and prints its path. See [DEBUGGING.md](DEBUGGING.md). |
| `amux pair [target]` | Pairs with another host found nearby by name, an address, or `user@host` over SSH. With no target this host opens pairing mode and shows a PIN, or a QR code for the phone with `--qr` (`--print-link` also prints its link); `--link` pairs from an `amux://pair` link; `--cancel` closes pairing mode. `--demo --pin <digits> --for <duration>` holds a PIN you choose open for an unattended demo (`30d`, `12h`, `45m`; at most 90 days) and returns at once. |
| `amux peers` | Lists the paired hosts and the hosts found nearby. |
| `amux unpair <peer>` | Stops trusting a paired host, asking first unless `--force`. |
| `amux login` | Signs the profile in to an amux account so its hosts reach each other through the relay (`--cloud-url` for another account service). |
| `amux logout` | Signs the profile out; its agents and paired hosts stay. |
| `amux profiles` | Lists the installation's profiles. |
| `amux profile list\|create [label]\|rename <profile> <name>\|delete <profile>` | Manages the installation's profiles. |
| `amux init` | Sets up this install, including whether amux starts at login (`--login-item yes\|no`, asked when omitted). |
| `amux update` | Asks the supervisor to install the channel's release now, even one that was rolled back here. |
| `amux config channel stable\|preview` | Chooses which releases the supervisor follows. |
| `amux config telemetry [on\|off]` | Turns the usage events amux sends on or off; with no value, says whether they are on and where they go. See [what amux sends](ANALYTICS.md). |
| `amux server start` | Starts amux detached from the terminal: the supervisor where the install has one, the daemon otherwise. With `--cloud` it runs the cloud relay in the foreground instead. |
| `amux server stop` | Stops amux cleanly, the supervisor first where one runs; agents keep running. |
| `amux supervise` | Runs the daemon under a supervisor that restarts it and, under `updates: auto`, installs releases. See [SUPERVISOR.md](SUPERVISOR.md). |

Hidden subcommands exist for amux's own processes rather than for people: `daemon` (the daemon in the
foreground), `agent <dir>` (the agent process), `mcp <dir>` (the tool server an agent's provider launches),
`hooks claude` (terminal Claude's hook command), and `pair-recv` and `relay` (the far end of SSH pairing and a
peer's link over SSH).

## Tests and evidence

| What | Where | Run |
|---|---|---|
| Behaviour: keys, effects, layout, paging, asks, the queue, home, the review page | `crates/tui/src/tests.rs` | `just test-tui` |
| Component goldens in light and dark: every row kind as the feed draws it, each ask in its box, the queue, the composer's states, the review page | `crates/tui/tests/golden.rs`, `crates/tui/tests/golden/` | `just test-tui`; rewrite with `UPDATE_GOLDENS=1 just test-tui` |
| Whole frames from served hosts: home at several widths and with every standing, the hosts window, chats mid-call, an ask this client cannot answer, and a chat through a rewind | `crates/tui/tests/frames.rs` | `just test-tui` |
| PNG renderings of the components | [`crates/shot`](../crates/shot/README.md) | `just shot -- render vocabulary --out DIR` |
| The storybook: every named whole screen draws at every size in both palettes | `crates/shot/src/stories.rs` | `just test-crate shot` |
| Raw attach, the CLI and the binary against real daemons | `crates/amux/tests/` | `just test-crate amux` |
| Journeys through the real terminal client: deciding asks and plans, the queue, sending, the composer's states, starting and managing agents, attachments and review, reaching hosts, recovering, reporting a problem | `journeys/`, `scripts/terminal-journey.py` | `just journey terminal <name>` |

[TESTING.md](TESTING.md) places these in the suite catalogue.

## Storybook

The storybook is this client's named whole screens, each drawn from data by the real views: home empty, busy
and filtered, the hosts window, both new-agent composers, chats at work, finished, failed and interrupted,
every kind of ask, the overview and the review page. Chats replay the recorded provider fixtures, so a screen
shows what a real agent said. Stories live in `crates/shot/src/stories.rs`, in the order a person meets them,
and a test draws every one at every size in both palettes.

```sh
just storybook                         # step through them in this terminal
just storybook --story ask.question    # open on one
just storybook capture --out target/stories [--only chat.]
```

In the storybook, ← → step, ↑ ↓ jump to the first story of the previous or next group, and `/` finds a story
by name or by what it shows. `t` cycles the palette: this terminal's own colours, as the client draws by
default, then the shipped dark and light. `s` cycles the size: this terminal's, then 80x24, 110x34 and 60x24,
framed in the middle. It opens where it was left, and it rebuilds itself when a source file of `tui`,
`ui-view`, `ui-state` or `shot` changes, coming back on the same story; a failed build says so on the bottom
line and leaves its log in `target/storybook/build.log`.

`capture` draws each story at 80x24, 110x34 and 60x24 in dark and light, as text and PNG, with an `index.md`
naming each story and what it shows. A live world of real agents to drive the client against is an
[environment](TESTNET.md#environments).
