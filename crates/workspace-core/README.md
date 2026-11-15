# workspace-core

The workspace lifecycle as one function of its state and an input. A
workspace is where an agent runs: a plain folder, a git worktree this host
made, or a wt tree. `Workspaces::step` takes what happened to a workspace's
members (one was created in a new workspace or added to an existing one, is
about to launch, stopped being awake, was deleted with or without consent to
discard its changes) and what each job it asked for came back with. It says
which jobs to run: make the workspace, take or release amux's claim on it,
run its start hook, stop it once nothing else holds it, or delete it. It
also says when a member may launch. The backends that run the jobs (git and
wt) and the resolving of a folder to its workspace belong to the driver.

Per workspace it holds only what amux decides: the kind and id, whether this
host made it, its members and which are awake, whether amux's claim is held,
whether a start job or a stop-when-unheld call is outstanding, whether a
delete gave consent to discard, whether it is being made or deleted, and its
problem (the newest failed hook, claim or environment). Whether a tree is
started is the backend's business and is not cached here.

The rules it enforces:

- a member launches only while amux's claim is held;
- at most one stop-when-unheld call is outstanding per workspace, and none
  while this host's start job for it runs;
- only workspaces this host made are started, stopped or deleted, but every
  wt tree with an awake member is claimed;
- a workspace is deleted once, after its last member is, and with its
  changes discarded only once some member's delete gave consent; removal
  never waits, so a refused delete means the workspace is kept and is no
  longer this host's.

Outputs are performed in order. A claim is registered with the backend
before the next output runs, and a release is done at once, so a stop or
delete issued after a release does not see amux's own hold.

A restarted daemon starts a fresh core. It adds each member as it resolves
the member's folder, the running ones first so their claims are retaken,
and then sweeps the workspaces this host owns on disk: each one without a
member is deleted, keeping its changes. A stop that was waiting when the old
daemon went is not re-armed.

The core does no IO and reads no clock, and it has no step budget of its
own: the daemon core composes it, runs it inside its own step and checks
`Workspaces::check_invariants` at the end of that step. The model
`WorkspaceLifecycle` in `crates/models` explores it over a world of backends,
shells, pins and restarts.
