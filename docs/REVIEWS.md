# Reviews

*For developers changing how a person reviews an agent's changes: the reviews experiment's service on the agent's host, its snapshots, comments and rounds, what the agent is sent, and the review page every client draws.*

A person reviews an agent's changes the way a pull request is reviewed, but
round by round on a working tree that keeps moving: open the review, comment
on lines, send the comments into the agent's chat, let the agent work, and
look again at what changed since. The review lives on the agent's host, so
every client shows the same comments, and a client already holding what it
fetched can read it with the host asleep.

It is the reviews experiment, `reviews`, at version `reviews/1`. Its
service, entries, tables and files keep none of amux's promises
([experiments](WIRE.md#experiments)); its service is the first experimental
service ([experimental services](WIRE.md#experimental-services)). With the
experiment off at an agent's host there is no review page: every client
opens it on a banner saying reviews are not offered there, and the overview
still lists the changed files from `ReviewService.GetChanges`, which is
`amux.v1` and always on ([changes](WIRE.md#changes)).

A review never touches the person's repository. Its snapshots are commits of
the working tree kept in a git object store of the agent's own, outside the
repository, and every git call it makes runs with one pinned configuration
([below](#git-pinned)).

## Where it lives

| Concern | Where |
| --- | --- |
| The service and its messages (`Reviews1`: `Open`, `GetFiles`, `Edit`; the row and attachment entries) | [`crates/wire/proto/amux/experimental/reviews.proto`](../crates/wire/proto/amux/experimental/reviews.proto) |
| The experiment's name and version | [`crates/settings/src/experiments.rs`](../crates/settings/src/experiments.rs) |
| Mounting the service and who may call it | [`crates/node/src/api/host/reviews.rs`](../crates/node/src/api/host/reviews.rs) |
| `Open` and `Edit`, the base, the summary | [`crates/node/src/reviews/mod.rs`](../crates/node/src/reviews/mod.rs) |
| `GetFiles`: hunks, colouring, scopes, outlines, more context, snippets | [`crates/node/src/reviews/files.rs`](../crates/node/src/reviews/files.rs) |
| Where comments sit on a later snapshot | [`crates/node/src/reviews/place.rs`](../crates/node/src/reviews/place.rs) |
| Removing the snapshot stores with the experiment off | [`crates/node/src/reviews/cleanup.rs`](../crates/node/src/reviews/cleanup.rs) |
| Snapshots, the snapshot store, comparisons, the pinned git configuration | [`crates/git-facts/src/snapshot.rs`](../crates/git-facts/src/snapshot.rs), [`objects.rs`](../crates/git-facts/src/objects.rs), [`compare.rs`](../crates/git-facts/src/compare.rs), `CONFIG` and `DIFF_OPTIONS` in [`lib.rs`](../crates/git-facts/src/lib.rs) |
| The review's writes, its entry on the agent's row, rounds closing at ingest, the turn-end check | [`crates/daemon-core/src/reviews.rs`](../crates/daemon-core/src/reviews.rs) |
| The tables (`exp_reviews_1_*`) | [`crates/store/src/reviews.rs`](../crates/store/src/reviews.rs) ([experiment tables](JOURNAL_AND_STORE.md#experiment-tables)) |
| The text the agent reads, and the sent review's attachment | [`crates/attachments/src/sent_review.rs`](../crates/attachments/src/sent_review.rs) ([sent reviews](ATTACHMENTS.md#sent-reviews)) |
| The calls, addressed to the agent's host | `AgentReviews` in [`crates/client/src/reviews.rs`](../crates/client/src/reviews.rs), from `Agent::reviews()` |
| Fetching, caching and following the row | `ui_runtime::review::Review` in [`crates/ui-runtime/src/review.rs`](../crates/ui-runtime/src/review.rs) ([the client](CLIENT.md#the-drivers)) |
| The page as rows, changed words, the chip | `review_page`, `anchor` and `review_chip` in [`crates/ui-view/src/review_page.rs`](../crates/ui-view/src/review_page.rs), [`changed_words.rs`](../crates/ui-view/src/changed_words.rs) |
| The apps' page | `app_runtime::ReviewSession` ([`crates/app-runtime/src/review.rs`](../crates/app-runtime/src/review.rs)), the bridge's `amux_review_*` ([`crates/app-ffi/src/review.rs`](../crates/app-ffi/src/review.rs), [embedding](EMBEDDED.md#the-c-abi-in-outline)), Swift's `ReviewSession` in `AmuxCore` |
| Drawing it | the terminal's [`crates/tui/src/chat/review.rs`](../crates/tui/src/chat/review.rs) ([terminal](TERMINAL.md#the-review-page)), the Mac's `ReviewPagePane` ([Mac](MAC.md#the-main-window)), the iPhone's `ReviewScreen` and `ReviewRows` ([selecting lines](IOS.md#selecting-a-range-in-a-diff-is-a-uikit-hold)) |
| Caps and their reasons | [parameters](PARAMETERS.md) |

## What a review is

Each of a host's own agents has at most one review, a row keyed by a UUID
the host makes when the review begins. It holds the base branch it compares
with,
the commit HEAD was at when it began, and the repository's common git
directory; the snapshots it keeps, each with the base commit its all changes
run from and every other base it has been shown against; the rounds sent,
each with the snapshot and base commit its text was written on and the
input that closed it; and the comments. Two agents
sharing a folder each have a review of their own of the same tree. A
review's rows go with its agent, in the store's `remove_agent`.

The loop, as the host answers it:

1. **A person opens it** (`Open`). The first look begins the review, picks
   its base (below) and takes a snapshot. A later look takes a new snapshot
   when the tree has moved since the latest one and no drafts wait. With
   drafts waiting the review stays on their snapshot and says the tree
   moved, so a person's unsent comments never jump under them; `refresh`
   takes the snapshot anyway and moves the drafts onto it. The answer is a
   `ReviewSummary`: the files changed, the rounds, every comment placed on
   the snapshot, whether a merge was under way, and what the agent's
   command needs (the store's folder, the repository's top folder and the
   snapshot's index). No file bodies: those come with `GetFiles`, so the
   page draws before the slow work.
2. **A client reads it without changing it** (`Open` with `read_only`): it
   never begins a review, takes a snapshot or picks a base, and waits for
   no other call. Following the agent's row and building a message both
   read this way, so a send never moves the review it is sending.
3. **The page fetches file bodies** (`GetFiles`) for one comparison the
   review handed out, in batches ([below](#file-bodies)).
4. **The person comments** (`Edit`): put a comment by its id, delete one,
   discard every draft, or start over, which deletes the review and its
   snapshot store so the next look is a first look. An edit is all of its
   changes or none.
5. **The person sends.** The client builds a sent review from the drafts
   and sends it with the person's message; the round closes when the agent
   takes it ([rounds](#rounds)).
6. **The next look** opens on what changed since the last round.

One call at a time works on an agent's review (a per-agent lock in
`reviews/mod.rs`): two opens at once take one snapshot, and an edit never
lands between a refresh reading the drafts and moving them. A read-only open
takes no lock and reads the store once.

**The base** is found only when a person opens the review, by the rule
`GetChanges` uses: the base branch the `Open` names, else the one the
agent's worktree was made from, else the repository's default branch, and
all changes run from where the branch left it. Where that rule would fall
back to HEAD (on the base branch with no upstream, or with no base branch
found) the review runs from the commit it recorded when it began, so the
agent's later commits stay in all changes; before the first commit, from the
empty tree (`base_for_review` in `compare.rs`). The base found is recorded
with the snapshot it is shown against, and every other read uses what the
snapshot recorded, so a base branch deleted or rewritten since never stops a
review being read or edited.

## Snapshots

A snapshot is a commit of the agent's whole working tree, untracked files
included, `.gitignore` respected, with HEAD as its parent (none before the
first commit), a fixed identity (`amux`) and the message `amux review
snapshot` (`snapshot::take`):

1. The person's index is copied to a temporary file that keeps its
   modification time: git trusts an entry's cached stat data by comparing
   it with the index file's own time, and a copy stamped now would hide an
   edit made in the same second as the last index write. A split index is
   made whole in the copy, through a copy of its shared index in a
   repository of the scratch's own: git reading an index linked to the
   shared one in `.git` refreshes that file's time.
2. The paths that differ from that index, and the untracked ones, are listed
   and measured before anything is stored. A file over 1 MiB is left out and
   named (`omitted`, listed as large), unless it is in conflict or Git LFS
   tracks it; more than 10,000 paths refuse the snapshot.
3. The rest are added to the copy with the snapshot store as git's only
   object directory, its tree written, and the tree committed with the
   repository's objects readable as an alternate, since only the commit
   names something (its parent) that lives in the repository.
4. A file that disappears between being listed and being read fails the
   add; the snapshot is tried up to three times, 50 ms apart, then refused
   naming the file.

The store is `<agent dir>/review/objects`. Writes see only the store because
git, asked to write an object it can already read, "freshens" it by touching
the file or the whole pack that holds it, and with the repository as an
alternate most snapshots would touch the repository's packs. The cost is that
the first snapshot stores every tree of the working tree again; later ones
share them.

In a partial clone git fetches any object it looks for and lacks, and with
the store as its object directory every object of the repository looks
lacking: a link whose times changed is compared by its blob, and dropping
asks after kept commits the store may not hold. So every call whose object
directory is the store, or the throwaway folder `moved` writes into, runs
with lazy fetching off (`GIT_NO_LAZY_FETCH`, honoured from git 2.44): an
object it does not see is missing, as in a full clone, and nothing goes over
the network, waits on credentials or leaves a pack in the store; a git older than 2.44 ignores the setting
and still fetches, into the store. Reads, with
the repository as git's object directory, are left to fetch as the person's
own `git diff` would, as `GetChanges` does: in a partial clone a comparison
needing a blob the clone never fetched fetches it into the repository.

Edge cases, each with a test in
[`crates/git-facts/tests/snapshot.rs`](../crates/git-facts/tests/snapshot.rs):
a folder outside a repository has no review; a snapshot from a subfolder is
of the whole tree; a dirty submodule reads as a change of its commit, its
inner changes unseen; an embedded repository with no commit is left out,
since git can record one only by its commit; a link is stored as a link; a
snapshot taken mid-merge or mid-rebase (or cherry-pick or revert) holds the
conflict markers as content and says a merge was under way; a file LFS
tracks is stored as the pointer LFS would commit for it, worked out in
process from the file's SHA-256 and size, and LFS's filters never run; in
a partial clone, taking, asking whether the tree moved and dropping fetch
nothing. A sparse index is untested.

**Has the tree moved?** `snapshot::moved` answers whether the working tree
differs from a snapshot without storing anything: untracked files are marked
in a copy of the index without being stored, and the one object marking
writes (the empty blob) goes to a folder of its own that is then removed. A
path the snapshot left out for its size counts as moved only once a snapshot
would take it (it shrank, went or no longer differs). The daemon core asks
this at every turn end of an agent whose checkout holds a review, as a job
off its thread, and writes the answer onto the review's row entry.

**Kept snapshots.** After a snapshot is taken the review keeps the
snapshots a round or a comment's anchor names; the latest; the one before
it, which a page opened before the latest was taken still shows; and, for
each sent review waiting in the agent's queue (as the agent's latest
session snapshot lists the queue), its own snapshot, which its round will
be recorded on and its text tells the agent to diff against, and every
snapshot the anchors of the comments it sends name (`Review::kept`). Those
anchors can be older than the send's own snapshot, as when a draft written
on a page still showing an older snapshot is sent from the latest summary,
and each comment goes back to its anchor when the agent takes the send. The
one before the latest also covers a send in the moment before the queue
lists it: only a second refresh in that moment could drop its own snapshot,
though one refresh then could drop an older one its comments are anchored
on. The others are forgotten in the same write and their objects dropped from the store (`snapshot::drop`), with their indexes. An old
round still depends on the repository holding the commits beneath it: a
rebase followed by garbage collection can remove them, and the page then
shows that round's comments with the code stored with them.

**The snapshot's index.** For each snapshot it keeps, the host makes a git
index of it, `<agent dir>/review/indexes/<commit>`, written under another
name and renamed into place, which the agent's diff command reads
([what the agent reads](#what-the-agent-reads)). A read-only open makes one
too and takes no lock, so one rule keeps it apart from the calls that do:
the index is written in `review/`, beside `indexes/` rather than in it,
which dropping the unkept sweeps of every name that is not a kept
snapshot's; and only taking a snapshot makes the `review` folder, so a read
answering after the review was started over makes no index and leaves the
folder gone.

**Lifetime.** The `review` folder goes with the agent's directory, when the
review is started over, and when the daemon next opens the profile with the
experiment off, never following a link in its place
([experiment tables](JOURNAL_AND_STORE.md#experiment-tables)). No dump
includes it, and the core's record of a review write keeps no comment words,
snippet or path.

## Git, pinned

Every git call in `git-facts` runs with one configuration over the
person's (`CONFIG`), and every diff whose output is parsed with one set of
options (`DIFF_OPTIONS`):

| Pinned | Why |
| --- | --- |
| `core.fsmonitor=false`, `core.splitIndex=false`, `gc.auto=0`, `maintenance.auto=false` | Each writes into `.git`: the monitor's socket and folder, shared index files, housekeeping |
| LFS's `process`, `clean` and `smudge` filters emptied, `filter.lfs.required=false` | A filter writes the file's content into `.git/lfs`, and where LFS is configured as required but not installed it fails every read of a file whose cached times are stale |
| `diff.noprefix=false`, `diff.mnemonicPrefix=false`, `diff.relative=false`, `--no-relative`, colour off | The parser reads git's plain format: `a/` and `b/` prefixes, paths from the top folder |
| `--no-ext-diff`, `--no-textconv` | A person's external diff or text conversion would replace the lines a comment is anchored on |
| `--submodule=short` | `diff.submodule=log` writes a summary with no `diff --git` header, and `diff` the submodule's own files |
| `--inter-hunk-context=0`, `--diff-algorithm=myers`, `--indent-heuristic`, `-O/dev/null` | Hunks, their boundaries and the order of files decide line numbers, placements and what since-last keeps; a person's algorithm or order file would move them |
| `core.hooksPath` set to `/dev/null` (on Windows, a folder in the user's own temporary directory that is never made) | Git runs a repository's `post-index-change` hook whenever it writes an index, the copies a snapshot and a moved check write included; a person's hooks are for their own work. A folder that is never made in a temporary directory every user shares, as `/tmp` is on Linux, would let another user put a hook there and have it run as the person |
| `diff.autoRefreshIndex=true` | A file whose times alone changed reads as unchanged, as it does by default, whatever the person set; with optional locks off, git writes no refresh into the person's index |

The one call that runs with the person's configuration alone is checking out
a new worktree, which is the person's own work: their filters must run so its
files are their content rather than LFS pointers, and their hooks as on any
checkout. No call reads the
person's index either: one that names no index of its own is given an empty
one (`no_index`), since git reads an index even to diff two commits, for
the attributes it looks up. A test runs every call under a hostile
configuration, the index split for real, and checks the answers are the
same and `.git` is byte for byte unchanged
(`the_persons_git_configuration_changes_no_answer_and_nothing_in_git`).

## Comparisons

Every comparison is from one commit to a snapshot:

| View | From | To |
| --- | --- | --- |
| All changes (`REVIEW_VIEW_ALL`) | the base commit the snapshot was shown against | the snapshot |
| Since your last review (`REVIEW_VIEW_SINCE_LAST`) | the last round's snapshot | the snapshot |
| A past round's whole diff | the round's base commit | the round's snapshot |

**Since your last review** is limited to what is part of the agent's change
(`compare::since_last`). After a merge or rebase from a moved base, what
arrived from the base differs from the last round but is no part of the
change, so it goes. A hunk stays when one of its changes sits on the lines
of a change the agent's work holds now, or of one it held at the last round,
so the agent undoing its own change (a line put back, a file made and then
deleted) shows though all changes no longer lists it. A file the agent only
renamed changed no line, so no hunk of it is the agent's: an edit the base
brought to it later goes. A file with nothing to compare since the round (a
binary file, a rename alone) stays whole, and one renamed since the round
whose hunks all go stays as the rename alone. The summary's
`since_last` lists exactly those files, from the same comparison as their
hunks, and is empty before the first round and when the last round's
snapshot can no longer be read.

**A file the snapshot left out** for its size is listed as large, changed
when the base's tree holds a file at its path and made otherwise: the tree
says so, and the file's bytes, however many, are never read for it
(`compare::files_at`).

**Each file's language** is named by the rule every client uses
([highlighting](HIGHLIGHTING.md)): its path, else its `#!` line, read from
the snapshot. A summary is made on every open, edit and list, so the `#!`
lines of all the files their paths name nothing for are read in one
`cat-file --batch` (`compare::heads`), and what they named is kept by
snapshot and path, which name the same bytes for good: the next summary of
that snapshot runs no git for it (`languages` in `reviews/mod.rs`).

**A request names its whole comparison**: the snapshot, the commit it is
compared from, the view, and for since-last the base each side was shown
against. The host checks that it handed that comparison out (the snapshot is
one it keeps, and the commit a base it has been shown against or a round's
snapshot) and answers `INVALID_ARGUMENT` otherwise, `NOT_FOUND` for a
snapshot it no longer keeps, and `DATA_LOSS` for one whose commits can no
longer be read. So a request answers the same whatever the
review's base becomes later, and a client keys what it fetched by the
request. With `list`, `GetFiles` also answers the comparison's file list,
which is how a past round's whole diff is listed, since no summary lists it.

### File bodies

`GetFiles` answers, for each file asked for that fits, its hunks with three
lines of context, each line coloured, each hunk's **scope** (the chain of
definitions its changed lines sit in), the file's **outline** (the
definitions the change touches) and any **more context** asked for
([`files.rs`](../crates/node/src/reviews/files.rs)):

- Lines are coloured from each side's whole file, never from the hunk alone,
  by the [`highlight`](HIGHLIGHTING.md) crate. That work, and finding each
  file's definitions, is the slow part: it runs on a blocking pool two files
  at a time, so a large change leaves the host's other cores to its agents,
  and is kept by object id (a review's snapshots share most of their files)
  in a 64 MiB cache across every review the process serves. A file too large
  to show is never coloured. Measured in release, in process
  (`what_highlighting_costs_the_host`, an ignored test): a 50-file Rust
  change takes 156 ms the first time and 125 ms cached, the rest being git's
  diff and object reads; one answer touching every other grammar, 17 files,
  takes 618 ms, most of it building those grammars for the first time.
- A hunk's scope comes from its changed lines, not its context lines, which
  often reach into the definition before: its added lines, or for a hunk
  that only removes, its removed lines. The outline merges the definitions
  the added lines touch on the snapshot with those the removed lines touch
  on the compared commit, so a deleted function still shows. Each
  definition says which of the two commits its lines are numbered in
  (`ReviewSide`), so "more context up to the definition" reads the right
  lines.
- More context is read from the side of the comparison asked for
  (`context_side`: the snapshot, or for lines on a hunk's removed side the
  commit it is compared against, where a renamed file has its old path), at
  most 2,000 lines per file in one answer. A request names a side, never a
  commit, so nothing is read from outside the comparison the host checked;
  one asking for context without a side is `INVALID_ARGUMENT` before any git
  call.
- One answer holds at most 64 files and 8 MiB encoded, the first file
  always; the rest are left out for the client to ask for again. A client
  asks 16 at a time, those on screen first.

## Comments and where they sit

A comment is anchored on **the commit that holds its lines**, a path, and a
line range, zero to zero for the whole file. A new or context line is held by
the snapshot; a removed line by the commit the view compared against. The
path is the file's in that commit, so a removed line of a file renamed since
names its old path (`anchor` takes it from the file's head), and the host
places the comment in the file as the snapshot names it. "Old
side" and "new side" are not used, because the two views have different old
sides. A comment put on lines of a commit the review no longer shows is
refused, for the client to open the review again.

When a comment is put on new lines, the host stores with it its **snippet**
(the commented lines and three above, in diff form, at most 60 lines of at
most 256 bytes each) and its **scope**. A change of words on the same lines
keeps them. They are what the comment is shown with once its lines are gone,
and what the agent reads.

**Placing** a comment on a later snapshot is one diff from its anchor's
commit to that snapshot, renames followed, as GitHub places review comments
([`place.rs`](../crates/node/src/reviews/place.rs)):

| What happened to its lines | A draft | A sent comment |
| --- | --- | --- |
| Nothing touched them | on their new range | on their new range |
| Edited or deleted | on its whole file, keeping its words and snippet | **outdated**: folded, shown with its snippet |
| Removed lines, on a commit a view of the review still compares against | on those lines, on that view's removed side (`compared`) | the same |

A placement names the file the comment sits in as the snapshot names it,
following a rename. A sent comment is marked **changed near** when a line of
its enclosing definition, or with none a line within three of it, changed
since the round it was sent in. A refresh moves drafts by the same rule and
the summary says how many moved.

Each put raises the comment's **revision**. A sent comment can still be
edited or deleted; the chat keeps what was sent.

## Rounds

A send carries the drafts as a **sent review**: a `text` attachment named
`review` that the client builds from what a read-only open answered
(`attachments::sent_review::attachment`), with an entry,
`Attachment.experimental["reviews/1"]`, a `SentReview` naming the review,
the snapshot the text's lines are on and the base commit it was shown
against, the round the client expects, and exactly the comments it sends,
each with the revision and anchor the text was written from
([sent reviews](ATTACHMENTS.md#sent-reviews)). The client builds it
because a host never rewrites a message: what the agent reads is what the
person sent, and only the sender knows which comments it chose.

**A round closes when the agent takes the message**, not when it is sent:
when the item the prompt or message became is ingested from the agent's
journal, in the store transaction that commits the item and moves the
journal's cursor past it (`rounds_delivered`, called from the daemon core's
ingest). Of the comments the entry names, those still drafts at the
revision named are marked sent, in a round numbered as it arrives and
recorded on the snapshot and base commit the entry names, with the id of
the input the agent took; each keeps the revision it was sent at
(`sent_revision`) and goes back to the anchor the text names, which a
refresh after the text was built moved without raising its revision. So
the round is the code the sender saw, whatever was refreshed before the
agent took it, and since-last runs from it. Two sends in one input close
one round. So:

- a prompt waiting in the queue has closed nothing, and one withdrawn from
  the queue leaves its comments drafts;
- a draft another client wrote meanwhile, or one edited after the text was
  built, stays a draft, and a chip removed from the composer sends nothing;
- a send whose comments are all already sent or edited closes no round, and
  two clients sending at once never claim one number;
- a crash between the item and the round is impossible: both land or
  neither, and ingest reads the journal again.

A client finds the round a sent review closed by the input it was sent in:
the chat's chip names the id of the item that holds it, and the round is
the one recorded with that input (`ui_runtime::review::round_of`). Not by
its comments: two sends queued at once can carry the same comment, the
first closing a round with it and the second another without it. A message
whose comments were all sent or edited before the agent took it closed
none, and opens the text the agent read, as does one whose round the host
no longer holds, or whose comments have all been deleted.

### What the agent reads

The attachment's text (`sent_review::text`), escaped like any text
attachment ([the element](ATTACHMENTS.md#the-element)), holds for each
comment its path from the repository's top, its lines, the definitions it
sits in, the code it is on as a fenced `diff` and then the person's words;
and last, that the line numbers are a snapshot's, with a command that shows
what changed since:

```sh
GIT_INDEX_FILE=<index> GIT_ALTERNATE_OBJECT_DIRECTORIES=<objects> git --no-optional-locks \
  -c diff.autoRefreshIndex=false -C <root> diff <snapshot> \
  && GIT_INDEX_FILE=<index> git --no-optional-locks -C <root> ls-files --others --exclude-standard
```

The snapshot's own index stands in for the repository's, so a file that was
untracked when the snapshot was taken is compared like any other, and the
files made since are listed after the diff. The command writes nothing, to
the repository or anywhere else, the snapshot's index included: a plain
`git diff` writes the file times it finds back into the index it read, which
`--no-optional-locks` alone does not stop. A comment on lines the change
removed names them by the commit they were removed from ("lines 4–5 of
commit 9c1e…"), and so does one whose lines are no longer on the snapshot.
The agent is told the code, not only a line number, because the easy mistake
is to read today's file at "line 42".

## The row entry

The review tells every client what it holds through the agent's row:
`Agent.experimental["reviews/1"]`, a `ReviewRowEntry` with the number of
drafts, a counter, whether the tree has moved since the latest snapshot, and
the review's id. Every write to the review is one store transaction, one rise
of the counter and one row published; the turn-end check changes only
`moved`, and leaves the counter. A review started over has a new id and
counts afresh, so a client compares id and counter together (`RowMark`):

- a new id or counter: the client reads the review again, read-only;
- only `moved`: the page says the tree changed under its snapshot, and
  nothing on it changes.

The composer's chip is the entry's draft count, and says the drafts were
written on older code while `moved` is set. No client polls.

## Who may call

`Reviews1` is a host service answered by the agent's own host:

- only while that host runs the experiment, fixed when the daemon starts;
  otherwise it is not mounted and answers `UNIMPLEMENTED`, which a client
  reads as "not offered there";
- only for a person: it is never mounted on an agent's tools socket, and an
  agent's call carried from a paired host is refused `PERMISSION_DENIED`,
  even for its own children, since a review is a person's and an agent reads
  what it is sent;
- only for the host's own agents: a copy of another host's agent is
  `NOT_FOUND`, naming that host;
- never for a provider's subagent, which works in its parent's folder, so
  its changes are in its parent's review, and has no directory of its own
  to keep one in: `UNIMPLEMENTED`, as for a review not offered. No client
  asks for one: the terminal answers its review key in a subagent's chat
  with a notice, and the apps show a subagent no changes to review.

Any node carries a call to it unread, so the phone, whose runtime runs no
experiment, reaches a desk's review. The other statuses a client acts on:
`FAILED_PRECONDITION` from `Open` when the agent's folder is gone or is not a
repository (and from `Edit` or `GetFiles` before a review has been opened);
`DATA_LOSS` from `Open` or `GetFiles` when a commit the review needs can no
longer be read, which starting over clears (a client shows a past round's
comments with the code stored with them instead); `INTERNAL` when git itself
fails; `RESOURCE_EXHAUSTED` for every cap on what a review
holds (500 comments, 16 KiB of words in one, 4 MiB of comments together,
checked before anything is written) and for a tree with too many paths;
`ABORTED` when files keep vanishing under a snapshot, or the review began or
was started over meanwhile, each worth trying again.

## Clients

The page is composed once, in the view layer, and drawn by the terminal, the
Mac and the iPhone exactly as the rows are placed:

- **`ui_runtime::review::Review`** makes the calls and keeps what they
  fetched. Every read names commits, whose content never changes, so it is
  kept by request in a process-wide cache (32 MiB of encoded answers) and,
  apart from that, by the open page for as long as it shows it, so eviction
  never blanks a page. A review already loaded reads with its host asleep;
  only edits need the host. **The page never changes under its reader**: a
  summary read again because the review changed is taken only while it
  describes the snapshot the page shows; one that moved on leaves the page
  where it is, marked stale, until the person refreshes or opens it again.
- **`ui_view::review_page`** turns the summary, the fetched bodies, the
  extra lines and the person's own choices (the view, folded files, notices)
  into a header, banners and rows: file heads with their flags and outline,
  hunk heads with their scope, lines with their colouring and changed words,
  gaps that load more lines, and comments where they are placed. It lists
  the files still to fetch, which a client asks for in batches. Generated
  files, lock files, vendored code and large changes start folded, except on
  a past round's page, which opens every file. `anchor` turns a selection of
  rows into the commit and lines a comment is on. **Changed words** within
  an edited line are worked out here (`changed_words`): each removed line is
  paired with the added line most like it, at least half alike by bytes of
  shared words, and the words that differ are marked, by an in-house
  longest common subsequence over words, since every client builds the view
  layer.
- **The apps** hold an `app_runtime::ReviewSession` per open page, reached
  through the bridge's `amux_review_*` calls, and Swift's `ReviewSession`
  draws the page it is handed and asks for what it says it still needs.
- **What each client draws** is on its own page: the terminal's keys and
  layout ([terminal](TERMINAL.md#the-review-page)), the Mac's tool window
  ([Mac](MAC.md#the-main-window)), and the iPhone's line selection
  ([iPhone](IOS.md#selecting-a-range-in-a-diff-is-a-uikit-hold)). The
  terminal draws a file's outline under its head; the apps do not yet.

## Rejected alternatives

**Snapshot commits kept by refs in the repository.** A ref holding a
branch's tip makes amux's own test for "this branch has no commits of its
own" count the branch as merged and delete it with its workspace; refs are
shared by every store using the repository, so one store's cleanup deletes
another's; untracked files, secrets among them, enter the repository, and
`push --mirror` sends them; and refs strand when the experiment is switched
off. A store in the agent's directory has none of these, and its cleanup is
removing a folder.

**Writing snapshot objects into the repository's object directory**, or
writing with the repository as an alternate. Either touches the person's
`.git`: the first adds objects, the second freshens packs it already holds.

**Storing nothing and detecting staleness.** It gives no rounds, no way to
reopen an earlier round, and no proven way to move comments.

**Storing each changed file's new side as a blob.** It reimplements what git
already stores and diffs.

**Keeping the review in the tree's git** (worktree-private refs, comments in
a committed file). It loses live updates through the streams clients already
hold, retention with the agent, and isolation from the person's repository.

**Finding a comment's new place by searching for its text.** Line mapping
through git's diff is the proven method, and the one GitHub uses.

**Leaving git's diff to the person's configuration.** Line numbers, hunk
boundaries and file order would differ from one machine to the next, and
with them where comments sit and what since-last keeps; several options
also write into `.git`.

**Closing a round when the host accepts the send.** A prompt waiting in the
queue can be withdrawn, which would leave its comments marked sent and never
read. Closing it in a write of its own after ingest left a window where the
journal's cursor had moved past the item and the round was still open, which
a crash made permanent.

**Drafts held on the client.** The host would have to move them on every
refresh; snippets and placements live where the snapshots are, and a draft
written on the phone is there on the desk.

**A `Check` call** to ask whether the review changed. The entry on the row
every client already receives answers it without polling.

**Ending a review automatically** when the tree equals the base branch's
tip. It would end a live review on the base branch as soon as the agent
commits, and miss squash merges and rebases once anything else is on the
base. A review ends when the person starts it over or the agent goes.

**An approval state, viewed marks on files, or a review per workspace.**
Done is the work landing; viewed marks, if they come back, can live on the
client keyed by review, path and content; and a review belongs to one agent.

## Tests

| What | Where | Run |
| --- | --- | --- |
| Snapshots: nothing in `.git` changes taking, asking or dropping; nothing fetched in a partial clone; same-second edits; untracked secrets only in the store; submodules, links, embedded repositories, large files, too many paths, merges and rebases, LFS without LFS; kept snapshots stay readable; every call under a hostile git configuration | [`crates/git-facts/tests/snapshot.rs`](../crates/git-facts/tests/snapshot.rs), [`compare.rs`](../crates/git-facts/tests/compare.rs) | `just test-crate git-facts` |
| `Open` and `Edit` in memory: first and later looks, drafts holding the page, refresh moving drafts, placements (moved, outdated, file, compared, renamed, changed near), discarding and starting over, two agents in one folder, the base on a branch with no upstream, since-last after a merge, kept snapshots, two opens at once, the row entry and its counter, read-only opens, a rewritten base branch, the agent's command truthful and writing nothing, rounds closing exactly the named drafts when the agent takes the prompt, on the snapshot the text names through refreshes while it waits, the experiment switched off, and a dump holding nothing of a review | [`crates/node/tests/reviews.rs`](../crates/node/tests/reviews.rs) | `just test-crate node` |
| `GetFiles`: colouring from whole files, scopes and outlines on either side, more context, since-last limits, caps, the object-id cache, snippets | [`crates/node/tests/review_files.rs`](../crates/node/tests/review_files.rs) | `just test-crate node` |
| A person on a paired host reviewing through the carrier, the copy following the entry, deletion removing rows, folder and copy; agents, tools sockets, copies and hosts without the experiment refused | `spec_reviews` in [`crates/testnet/tests/`](../crates/testnet/tests/) | `just suite many-daemon-protocol-spec` |
| The page as rows, compared comments, a removed-only hunk, the trailing gap; changed words | [`crates/ui-view/tests/reviews.rs`](../crates/ui-view/tests/reviews.rs) and its golden, `changed_words.rs` | `just test-crate ui-view` |
| Fetching, caching, following the row, rounds found by the input that closed them | [`crates/ui-runtime/tests/review.rs`](../crates/ui-runtime/tests/review.rs) | `just test-crate ui-runtime` |
| The sent review's text and entry, and body escaping | [`crates/attachments/tests/attachments.rs`](../crates/attachments/tests/attachments.rs) | `just test-crate attachments` |
| The terminal's page and keys; the `attachment-or-review` journey on the terminal and the phone | `crates/tui/src/tests.rs`, [`journeys/`](../journeys/manifest.json) | `just test-tui`, `just journey terminal attachment-or-review`, `just ios journey attachment-or-review` |
| A real Claude and Codex reading a sent review: each quotes the code exactly and runs the command, Codex inside its read-only sandbox | the `review_attachment` scenario | `just live claude_sdk review_attachment`, `just live codex review_attachment` |
