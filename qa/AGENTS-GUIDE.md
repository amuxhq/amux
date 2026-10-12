# Talking to agents precisely

A finding is only worth something when the failure is amux's, not a confused
model's. Every phrasing below was tried against the real models (Claude Haiku
and Codex `gpt-5.6-luna`) and produced the behaviour named beside it. Use them
word for word, and change only the names, files and texts in them.

## Habits

- **Ask for exact, checkable output.** "Reply with only the word done." Then
  `ctl chat HOST AGENT` shows whether it said exactly that.
- **One instruction per prompt when timing matters.** A prompt that does two
  things makes it hard to say which one a screen shows.
- **Name the file and the command.** "Run exactly this shell command once,
  with your shell tool, and nothing else: touch decided.txt."
- **Choose the small model on every surface.** Haiku for Claude,
  `gpt-5.6-luna` for Codex:
  - `ctl spawn` on a real host uses them unless you pass `--model`;
  - the terminal client's new agent starts on them on a real host (its
    settings edge shows the model once the agent runs);
  - on the phone, pick them in the new agent screen's Model menu
    (`phone tap new-agent.model`, then `phone choose "Haiku 5.5"` or
    `phone choose "GPT-5.6-Luna"`). Left at "Host default", it starts on
    the small model too: a test net's providers fill in the small model
    whenever amux names none.
- **Choose the permission too.** Claude left at the host's default starts
  with the permission `default`, which asks before running commands. To
  see it ask, keep that (`ctl spawn` uses it for real Claude unless told
  otherwise; the Permission menu on the phone; Shift+Tab in the terminal
  client).
- **Waits on real agents take minutes.** `ctl spawn --wait`, `ctl send --wait`
  and `ctl wait` give a real agent up to four minutes.
- **See what is waiting on you** with `ctl chat HOST AGENT`: its phase, every
  item with what it decodes as, and under "waiting on you" each open ask.

## Claude

| Behaviour | How |
| --- | --- |
| A question with options and a preview of each | Prompt: "Use the AskUserQuestion tool to ask me which of two names to use for a new helper function, parse_line or read_line, with a short code preview of each. Ask nothing else." Headless and terminal Claude both ask one question with two options, each with a code preview. |
| Two questions in one ask | Prompt: "Use the AskUserQuestion tool once, asking me two questions in that one call: 'Which colour?' with the options Red and Blue, and 'Which size?' with the options Small and Large. Then reply with the single word done. Do nothing else." |
| A question with several picks | Prompt: "Use the AskUserQuestion tool once to ask me 'Which toppings?' with the options Cheese, Olives and Basil, letting me pick more than one. Then reply with exactly the toppings I picked. Do nothing else." Its reply shows which picks arrived. |
| A question answered in your own words | Ask any question above and answer with the free-text choice instead of an option ("Other" in Claude's own view), then prompt "Repeat my answer word for word." to see what arrived. |
| A permission ask from a shell command | Start it with the permission `default`. Prompt: "Run exactly this shell command once, with your shell tool, and nothing else: touch decided.txt. Then reply with the single word done." Terminal Claude may treat a file in its own folder as safe; a network command always asks: "... and nothing else: curl -sI https://example.com. Then reply with the single word done." |
| A turn held open for 30 seconds | Prompt: "Run exactly this shell command once, with your shell tool, and nothing else: python3 -c 'import time; time.sleep(30)'. Then reply with the single word done." With the permission `default` it asks first; approve it, and the turn stays working for the 30 seconds, time to cut, kill or flap. Claude refuses a bare `sleep 30` (its own rule against waiting), so do not ask for one. |
| Planning | Switch the agent into plan mode the way a person does: create it with the permission `plan` (`ctl spawn --permission plan`, the Permission menu on the phone), or cycle the permission with Shift+Tab in the terminal client. Then give it an ordinary task, such as "Change README.md's only line from CURRENT to UPDATED." in a folder holding that README. It presents its plan for approval itself. Never tell it which tool to plan with. Claude may report a different model while it plans. |

## Codex

| Behaviour | How |
| --- | --- |
| A command approval | Start it with the permission `read-only`. Prompt: "Run exactly this shell command once, with your shell tool, and nothing else: touch decided.txt. The sandbox is read-only, so request escalated permissions for it; I will approve. Then reply with the single word done." |
| A patch (file change) approval | Permission `read-only`. Prompt: "Create a file named notes.txt containing the single line hello, by editing files with your patch tool, not the shell. The sandbox is read-only, so the edit needs my approval; I will approve. Then reply with the single word done." |
| Planning | Create it in plan mode (`ctl spawn --kind codex --mode plan`; Shift+Tab steps Codex's mode in the terminal client), then give it an ordinary task as for Claude. It presents a plan to start or keep refining. |
| A question | Codex asks questions only while planning. In plan mode: "Use your tool for asking the user questions to ask me one question, 'Which colour?', with the options Red and Blue. Then reply with the single word done. Do nothing else." |
| A turn held open for 30 seconds | Prompt: "Run exactly this shell command once, with your shell tool, and nothing else: sleep 30. Then reply with the single word done." It runs at once and the turn stays working for the 30 seconds, time to cut, kill or flap. |
| A permission grant | Not reachable with the stock Codex: its tool for requesting extra sandbox permissions is switched off, and asked for one it asks for a command approval instead. |

## amux's own agent tools

Every agent carries amux's tool server (`reference/docs/AGENT_TOOLS.md`).

| Behaviour | Prompt |
| --- | --- |
| A message to another agent | "Use amux's send tool to send worker-2 the message 'hello from desk'. Then reply with only the word done." Works from Claude and Codex; `ctl chat HOST worker-2` shows the message arriving. |
| A child agent | "Use amux's spawn tool to start a codex child on host laptop whose prompt is: Reply with only the word pong. Then reply with only the word done." Name the kind (claude or codex), the host and the child's prompt. A child starts on the small model and, for Claude, asking, and ends when its task is done. |
| A status line | "Use amux's status tool to set your status to 'reviewing the README'. Then reply with only the word done." The fleet's "working on" then reads that text. |
| What it can see | "Use amux's agents tool and reply with only the names of the agents it lists, comma-separated." |

## What no real agent does

amux's tools never ask a person anything, and the agents carry no other tool server, so a tool server's form, a
link it asks to open, terminal Claude's tool-server dialog and Codex's permission grant do not come up in testing.
Leave them out of your ground.
