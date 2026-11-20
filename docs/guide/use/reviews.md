---
title: Reviewing changes
description: Read what an agent changed, comment on its lines, and send your comments back, round after round, from the terminal, the Mac or your iPhone.
sidebar:
  order: 3
---

Review an agent's work the way you'd review a pull request, without waiting
for one. Open its changes, comment on the lines that need it, and send your
comments into its chat. The agent gets each comment with the code it's on,
works on them, and the next time you look you see just what it changed since.

Your comments are kept on the computer the agent runs on, so a review you
start in the terminal is there on your phone, and the other way round.

## Switch reviews on

Reviewing is an [experiment](terminal.md#settings): switch on `reviews` on
the computer the agent runs on.

- **In the terminal app**, press `,` on home and turn on **reviews** under
  experiments.
- **In the Mac app**, open **Settings** and turn on **Reviews** under
  **Experiments**.
- **From the command line**, add `reviews` to the experiments you have on:
  `amux config set experiments.enabled '["reviews"]'`, listing any others
  you already use beside it.

amux restarts itself to apply the switch, a few seconds later; your agents
keep running. Your phone has no switch of its own: it reviews agents on any
computer that has reviews on. Where an agent's computer doesn't, its review
says reviews aren't offered there, and the agent's overview still lists the
files it changed.

As with every experiment, how reviews work may change in any release. If you
switch reviews off, the computer deletes its reviews as amux restarts to
apply that, unsent comments included; reviews you sent stay in the chats as
the text the agents read.

## Open a review

From the agent's chat:

- **Terminal:** press Ctrl+A then `r`, or click `[Diff]` in the header. A file
  in the overview opens the review at that file.
- **Mac:** open the tool window with the button at the far right of the title
  bar (⌥⌘I); **Review** is its first tab. The review chip in the message box
  opens it too.
- **iPhone:** tap the changes count at the top of the chat, **Review
  changes** in the overview, or the review chip in the message box.

The review shows everything on the agent's branch since it left its base
branch, committed or not, new files included. For an agent working on the
base branch itself, it's what isn't pushed yet, or, with nowhere to push,
what changed since you first opened the review. A subagent's changes are
reviewed from the agent that started it.

## Read the changes

A review is a snapshot of the agent's folder, taken when you open it, and it
holds still while you read. If the agent changes code meanwhile, the review
says so; refresh it to see the code as it is now.

Every changed file follows the next. Each change sits under the functions or
types it's in, such as `impl Store › fn open`, and the words an edited line
changed are marked. Lock files, generated and vendored files start folded;
open one if you want it. You can show more of a file
above or below a change, up to the whole function around it.

A file over 1 MB is listed but not shown. Files stored with Git LFS show as
their LFS pointers. If a merge or rebase was under way when the snapshot was
taken, the review says so, and conflicted files show their markers.

## Comment

Select a line, or several lines of one file, and comment on them, or comment
on a whole file. How you select is in each app's own page:
[terminal](terminal.md#reviewing-changes), [iPhone](iphone.md#reviewing-changes);
on the Mac, click a line and Shift-click or drag to take in more, then press
Return.

Comments are saved on the agent's computer as you write them. Until you send
them they're drafts: you can change their words or delete them, and they
show up on your other devices too.

## Send your comments

While you have drafts, the agent's message box shows a chip,
**Review · 3 comments**. Whatever you send next carries them, with your
message as the summary, such as "fix these" or "only the first one matters".
You can send the chip on its own. The chip's **✕** discards your drafts
instead, after asking.

The agent reads each comment with its file, its lines, the function they're
in, the code they're on and your words. It's also told which snapshot the
line numbers are from, and given a command that shows what has changed
since, so it doesn't read today's file at yesterday's line 42.

Your comments count as sent once the agent takes the message. If the agent
is busy and the message waits in its queue, withdrawing it from the queue
leaves the comments as drafts.

### Written on older code

If the agent changes code after you've started commenting, the chip says
**Written on older code**. Opening the review keeps your drafts on the code
you wrote them on, so nothing moves while you finish. You can:

- **Send them as they are.** That's fine: the agent knows which snapshot your
  line numbers are from and can see what changed since.
- **Refresh the review first.** Your drafts move to the code as it is now.
  Any whose lines the agent edited become comments on their whole file,
  keeping their words and the code you wrote them on, and the review says
  how many moved.

## Rounds and since your last review

Each review you send is a round. The next time you open the review it starts
on **Since your last review**: only what the agent changed since the code you
last reviewed. What arrived from the base branch in a merge or rebase isn't
shown, because it isn't the agent's change, but changes the agent undid are.
Switch to **All changes** to see everything again.

Your earlier comments come with you:

- On lines nobody touched, a comment sits where those lines are now.
- On lines that changed since, a comment is **outdated**: folded away, with
  the code it was written on.
- When code near a comment changed since you sent it, in the same function
  or within a few lines, the comment says so, so you can check it was dealt
  with.

There's no approving and no closing: a review is done when the work lands.
To start afresh, the review's menu (or `S` in the terminal) starts it over,
deleting every comment, and the next look is a first look. You can also
discard just your drafts. Both ask first.

## Look back at a round

A review you sent shows in the chat as a chip. Open it to see that round
again: its whole diff, from the base branch to the code you reviewed, every
file open, with each comment you sent on its lines.

If you edited a comment before the agent took the message, a later message
sent it, and this chip shows the text the agent read. You also see that text
when the review has been started over since, or when the computer can't be
reached and the round was never loaded. If the computer can no longer read
the round's code, for example after the branch was rebased and git cleaned up
the old commits, each comment shows with the code it was written on.

## Following the agent

You don't need to reopen the review to keep up. An open review notices when
the agent changes code, when its turn ends, and says so without moving
anything you're reading. Comments written on another device appear as
they're saved.

If the agent's computer goes to sleep or out of reach, what you already
loaded still reads. Commenting waits until it's back.

## What a review keeps

A review never adds anything to your repository: no commits, branches or
stashes, and your index is left alone. Each snapshot is kept with the agent,
in its own folder in amux's data on that computer. A snapshot holds untracked
files too, so anything sitting untracked in the agent's folder, a secret
included, is in it; it stays on that computer and is deleted with the agent,
when you start the review over, or when reviews are switched off. A review
has nothing to show in a folder that isn't a git repository.
