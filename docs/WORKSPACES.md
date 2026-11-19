# Workspaces

*For developers changing where agents run: the folders, git worktrees and wt trees amux makes, holds, starts, stops and removes.*

A **workspace** is where an agent runs. Under the `workspaces/1` experiment
every agent is a member of exactly one, a workspace lasts while any member
does, agents run in its environment, and a workspace amux made stops when
nothing uses it and starts again when a member wakes. With the experiment
off, a new worktree is made before the agent's row and never removed, as
[the network protocol](PROTOCOL.md#calls-on-another-hosts-agents) describes, and
none of the rest runs. The experiment's shapes keep none of amux's promises
([the wire](WIRE.md#experiments)).

## Kinds

A workspace is named by its kind and an id. For every local kind the id is
the workspace's root, a canonical path on its host.

| Kind | What it is | Hooks, holds and environment |
| --- | --- | --- |
| `folder` | A plain folder, or a repository's own working tree | None. Never made, started, stopped or removed |
| `git` | A git worktree amux made without wt | None. Nothing but amux holds it, so claiming, starting and stopping do nothing |
| `wt` | A wt tree, made by amux or by hand | Start and stop hooks, holds wt counts beside amux's (a `wt shell`, another daemon, a `wt start` pin), and the tree's environment |

An agent's workspace is a function of its folder, which is fixed when the
agent is made, so nothing about it is stored for the host's own agents. The
daemon resolves the folder from what is on disk, in this order: a folder
inside a wt tree is in that tree; one inside a git worktree this host made
is in that worktree; one inside a repository is in a `folder` workspace at
the repository's working tree, so `~/src/app` and `~/src/app/lib` share one;
anything else is a `folder` workspace of its own. A folder that no longer
exists is a `folder` workspace at its path as written. Workspaces do not
nest: a worktree inside a repository is a workspace of its own. Families are
independent of workspaces: a child started in its parent's folder shares its
parent's workspace, and one started in a new workspace is a member of that.
A provider subagent (one Claude's Agent tool or a Codex child thread starts
inside its parent's process) is the exception to "every agent": it is in
its parent's folder but no member, since it has no process to launch and
goes with its parent, so it never holds, starts or removes a workspace.

`CreateAgentRequest.new_workspace` (`amux new --worktree`, the terminal's
`w` toggle) asks for a new one. It is a wt tree when wt is configured for the
repository, by wt's own answer (a user-layer configuration counts as well as
the repository's `.wt.toml`), and a git worktree otherwise. wt is Unix-only,
so Windows builds have no wt backend and make git worktrees.

## Who made it

Which host made a workspace is recorded with the workspace, on disk, so it
survives a store reset, a downgrade and the experiment being switched off
and on. It names the host because one installation runs several profiles,
each a host with its own store, and they share one worktrees folder, while
wt's trees belong to the whole machine.

| Kind | The record |
| --- | --- |
| `git` | `amux-owner` in the worktree's private git directory (`<repository>/.git/worktrees/<name>/amux-owner`), holding the host id. git removes it with the worktree, and it is never part of the working tree |
| `wt` | The tree's owner, `amux:<host id>`, which wt keeps and `wt ls` shows |

Only a workspace this host made is ever started, stopped or removed by it.
A wt tree a person made, and another profile's or installation's workspace,
are hand-made to this host: it holds a wt tree while one of its agents is
awake there, so `wt rm` refuses it as it would for a `wt exec`, and runs
agents in its environment, nothing more. Worktrees
made with the experiment off carry no marker and are never removed.

A git worktree whose marker cannot be written is undone at once, with the
branch its make created: unmarked, nothing would ever remove it, and its
branch would block the name. The wt backend removes or disowns a tree only
when its owner is this host's, since two profiles can plan the same tree for
agents of one name and the one that loses the make must not take the
winner's tree away.

## Creating

A create in a new workspace writes the agent's row first, so every client
lists the agent at once, however long wt's fetch and copying take:

1. **Plan, before any row.** The backend plans the workspace without the
   network: for wt, `Wt::plan` gives the label, path and branch its
   templates render, checked against local branches and folders; for git
   the path is `<installation>/worktrees/<repository>/<name>` on a branch
   named `<name>`. The name is the agent's. One whose branch or folder is
   taken gets the next free suffix, as a name another agent holds does, up
   to sixteen tries. A folder outside a repository, one with no branch
   checked out when the new workspace is a git worktree, or anything wt
   refuses fails the call with no agent made.
2. **Write the row** at the workspace's final folder, its entry saying
   `creating`, and answer the call with it.
3. **Make the workspace** in the background. wt fetches, clones, copies its
   inputs, then starts the create hook, which the agent does not wait for.
4. **Launch the process.**

A new git worktree branches from what the creating folder has checked out.
A new wt tree does too, unless that folder is on the repository's default
branch, when it starts from `origin`'s, freshly fetched; a repository without
`origin` starts from the folder's commit. A child spawned from its parent's
branch therefore shares its parent's history, and the parent merges the
child's branch straight back. Uncommitted changes in the creating folder are
never carried over.

While the workspace is made, messages wait in the agent's lane (without the
five seconds a live row's process otherwise has to connect), and so does a
stop or a delete. The agent's chat is caught up from the moment its row is
written, rather than at its process's Hello as for an agent launched at once:
nothing is in its journal yet, so a client takes a message straight away
instead of saying it is catching up for the seconds a fetch, a checkout and
a copy of build output take. The same holds for a new agent whose launch
waits for its workspace's claim. A workspace that cannot be made ends the agent as
`EXIT_CAUSE_COULD_NOT_START` with the reason, and a restarted daemon that
finds a row still `creating` ends it the same way. So the restart the daemon
makes to apply an experiment switch waits for every workspace job under way,
a make or a hook, for up to five minutes ([experiments](WIRE.md#experiments)),
while an update or `amux server stop` does not wait.

Every launch in a wt tree, start or resume, merges the tree's environment
into the provider's (`start_process` in
[`runtime.rs`](../crates/node/src/runtime.rs)): its identity and port
variables, its `[env]`, and `.wt/bin` ahead of the `PATH`. Reading it also
renders the tree's files again, so a launch after a merge that changed the
tree's configuration picks the change up. An environment that does not load
is left out and becomes the workspace's `problem`, so the agent that would
fix it still runs.

## Holding, starting and stopping

A member is **awake** while its process runs or is being launched; a
suspended agent has no process and is not. While a workspace has an awake
member the daemon holds one **claim** on it. For a wt tree the claim is a
shared hold wt counts beside the holds amux cannot see; for every other kind
it is amux's own count.

| Event | What amux does |
| --- | --- |
| A member is about to launch | Takes the claim if it holds none, and launches the member once it is held. A claim waits only behind a stop hook already running. For a wt tree this host made, taking the claim also asks for the start hook, which the launch does not wait for; wt runs it after a create hook still running |
| The last awake member stops being awake (it suspends, is settled, exits or is deleted) | Releases the claim and, for a wt tree this host made, makes one stop-when-unheld call: wt waits until nothing else holds the tree, does nothing if a person pinned it, and runs the stop hook |
| A member wakes while that call waits | The call cannot complete while the claim is held; a stop hook already running finishes first, then the claim is granted |
| The last member is deleted | The workspace is deleted, below |

At most one stop-when-unheld call is outstanding per workspace, and none is
issued while this host's start job for it runs: the stop waits for the start
and is then reconsidered, so it never overtakes the start it follows. amux
records nothing about whether a tree is started and runs the start hook each
time it takes the claim on a tree it made, so start hooks must be safe to
repeat.

A workspace stops as soon as nothing uses it, with no delay. An agent without
a parent has already been idle for the suspension delay before it sleeps, a
person who settles an agent is done with it, and a finished child's process
exits, so its workspace stops at once; a follow-up to it costs one stop and
one start, and waits for a stop hook already running. Stopping is not
removing: the folder and branch stay, so a parent can still merge a finished
child's commits.

A failed start counts as started and a failed stop as stopped; either
becomes the workspace's `problem`, shown on its members, and nothing retries
on its own. A claim that cannot be taken counts as held, so its members still
run. A person's hard `wt stop` stops a tree whatever holds it.

**A daemon that restarts** starts the workspace core afresh. It resolves each
own agent's folder (one job per folder, those with a running process first)
and adds each agent as its folder resolves, a launch asked meanwhile waiting
for it. Taking the claim for an adopted process runs the start hook again,
since the daemon before may have lost a start it had asked for. A stop that
was waiting when the old daemon went is not re-armed: a workspace already
without awake members stays as it is until a member next wakes and sleeps.
Then the daemon lists the workspaces it made on disk and deletes each one
without a member, without consent to discard (so a dirty one is kept and
disowned), which covers a crash between deleting a row and removing its
workspace.

## Deleting

When the last member of a workspace this host made is deleted, the host
removes it if it is clean or a delete agreed to lose its changes. Otherwise
it leaves it on disk and clears its owner, so it is nobody's, in amux and on
disk. A removal does not wait for holders: a tree a person holds with
`wt shell` is kept and disowned the same way.

- **The host decides what is clean** when it removes the workspace, never
  the totals on a row, which can be stale. The git backend runs `git status
  --porcelain --untracked-files=normal --ignore-submodules=none`, the flags
  wt's own check uses, so a repository's `status.showUntrackedFiles=no`
  cannot hide untracked files. Ignored files are not changes: build output
  goes with the worktree.
- **A branch goes with its workspace when it has no commits of its own,**
  meaning its tip is reachable from another ref (not counting its upstream
  or the stash). wt records no start point and the base moves, so
  reachability is the only test that works. A kept workspace keeps its
  branch.
- **A delete waits for this host's own stop-when-unheld call** to answer. A
  member deleted while awake ends its process first, which asks for the
  stop. The stop hook holds its tree exclusively while it runs, and a
  removal refused for that hold would disown a clean tree. The wt revision
  amux takes also makes a removal wait out a stop hook already running.

Consent is `DeleteAgentRequest.discard_workspace_changes`, a permanent
`amux.v1` field whose zero value keeps changes, as every build before it did;
`amux rm --discard-changes` sets it. A delete's consent covers its own agent
and the members that follow it by [the family rule](PROTOCOL.md#the-family-rule),
so it counts only when every other member left in the workspace descends
from the deleted agent; daemon-core finds them by walking the rows' parent
links. It then holds until the workspace goes or gains a member, so a parent
deleted with consent still discards when its child, sharing the folder,
follows without it. Retention, the family rule and older clients never
discard.

Clients warn before they consent. `ui-state`
([`fleet.rs`](../crates/ui-state/src/fleet.rs)) works out whether a delete
removes the row's workspace: its host made it, it is not a folder, and every
other member on that host is the agent's descendant. An agent with a process
keeps its row's git facts current, so the warning reads them; one without
(suspended or exited) cannot, so the client asks the agent's host for a
comparison against `HEAD`, through the same changed-files call the review
page uses, before it warns. A confirmation captures what it showed when it opened
and sends consent only when that warning named changes, so a fleet that
changed meanwhile, or a check that failed, never discards work the person
was not shown. The branch is named whenever the row has one, and what
becomes of it is always the host's answer, for an agent with a process too:
the same call's `Diff.own_commits` counts the commits on the checked-out
branch that no other ref holds (its upstream and the stash aside), the
predicate the removal applies, zero exactly when the branch goes. A row's
totals cannot say it (a line committed and changed again uncommitted gives
equal totals over a commit of its own), and a merge made anywhere else takes
the commits away without the agent's row changing. Until the host answers,
or when it cannot, the warning says the branch stays if anything was
committed to it, which is true either way.

## Where it lives

| Concern | Where |
| --- | --- |
| The lifecycle rules, as one pure step | [`workspace-core`](../crates/workspace-core/README.md) |
| Feeding it what happens to agents, holding launches until it allows them, resolving members at start, the entry on own rows | [`crates/daemon-core/src/workspaces.rs`](../crates/daemon-core/src/workspaces.rs) |
| The row-first create and its name suffixing | `begin_create` and `workspace_planned` in [`families.rs`](../crates/daemon-core/src/families.rs) |
| The backends, each a maker (plan, make, start, delete, list owned) and a holder (resolve, claim, release, stop when unheld, environment) | [`crates/node/src/workspaces/`](../crates/node/src/workspaces/mod.rs) |
| The model, over the real core | `WorkspaceLifecycle` in [`crates/models/src/workspaces.rs`](../crates/models/src/workspaces.rs) |
| The experiment's name and version | [`crates/settings/src/experiments.rs`](../crates/settings/src/experiments.rs) |
| What a row says of its workspace | `amux.experimental.AgentWorkspace` (kind, id, `made_by_amux`, `creating`, `problem`) in `Agent.experimental["workspaces/1"]` |
| Where that is kept | On the row, in the store's `experimental` column ([experiment tables](JOURNAL_AND_STORE.md#experiment-tables)). For own agents a cache of what the folder says, republished only when it changes; a replica's as its host wrote it, kept and passed on by every host whether it runs the experiment or not, so the phone warns of a delete's worktree though its node has no switch |
| Git worktrees on disk | `<installation>/worktrees/<repository>/<name>` |
| wt trees on disk | Where wt's configuration places them, under wt's home (`$WT_HOME`, else `~/.wt`), so `wt ls`, `wt shell` and `wt rm` work on them |

wt is a git dependency pinned to a revision until it publishes a release
with holds, owners and planning. Workspace ids are paths that name the
person's folders, so a report's redaction aliases them, and a plan's
repository, as paths ([debugging](DEBUGGING.md)).

Switching the experiment off loses nothing that matters: amux stops making
wt trees and stops removing workspaces with their last agent, the host
clears the entry from its own rows when it next opens the store, and the
workspaces it made keep their owner on disk for when it is on again.

## Rejected alternatives

**Recording who made a workspace in amux's own data,** a field in the
agent's spec or a store column. It duplicates what the disk already says, a
copy can disagree with the disk, and it is lost with a store reset or an
experiment switched off, after which amux would either leak workspaces
or remove ones it never made. A spec field would also be a promised on-disk
format from its first release.

**A workspace only for agents amux made a worktree for.** Every rule would
need a branch for agents in no workspace, and two agents in a main checkout
could not be grouped.

**Making the workspace before the row,** as the path without the experiment
does. Only the creating client would know an agent was coming, wt's fetch
can take tens of seconds, and a create forwarded to another host could
outrun the forwarding host's patience for an answer.

**A marker recording whether a tree is started.** It survives reboots and
crashes and goes stale, so a start would be skipped exactly when it was
needed. Start hooks are safe to repeat and run each time the claim is taken
instead. A restarted daemon in particular can have lost a start it asked
for (one still waiting behind the create hook, say), so trusting "already
started" would leave an agent running in a tree whose start hook never ran.

**Polling workspaces,** or waiting a while with no holders before stopping.
Every transition is caused by something amux or wt already observes: a
launch, a process ending, a delete, a hold going inside wt's
stop-when-unheld. A delay before stopping buys nothing either, since an agent
without a parent has already been idle for the suspension delay before it
sleeps. A "still wanted?" check when the wait for holders ends is not needed
for the same reason: it cannot end while the daemon holds its claim, and it
holds its claim whenever a member is awake.

**Waiting for the start hook before launching.** A hook can take minutes,
and the person sees the agent at once and expects it to work. A failed hook
must not keep the agent that could fix it from running; its failure is the
workspace's `problem` instead.

**Consent that sticks to the workspace for any later delete.** `amux rm
--discard-changes A` while an unrelated `B` still worked in the tree, then a
plain delete of `B` by a person, retention or the family rule, would have
discarded `B`'s work, which nobody agreed to. Consent covers only the
members its delete takes with it.

**Keeping the rows' workspace entries in an experiment table** of the
store's (`exp_workspaces_1_members`). A host with the experiment off dropped
them from the replicas it stored, so the phone, whose node runs no
experiment, never warned that a delete takes the worktree. The entry is
what a row says to everyone, so it rides on the row
([experiment tables](JOURNAL_AND_STORE.md#experiment-tables)).
