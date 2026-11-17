//! wt trees, through wt's library: made for a repository wt is configured
//! for (its own `.wt.toml` or a user-layer configuration, as wt itself
//! decides), held while an agent is awake in them, started and stopped
//! with their agents, and removed with their last one. A tree this host
//! made has the owner `amux:<host id>`; any other tree, a person's
//! included, is hand-made, and amux only holds it and runs its agents in
//! its environment.
//!
//! wt's state is files under its home that every copy of wt on the machine
//! meets through, so the claims here are file locks like a `wt shell`'s,
//! and a stop waits for every other holder, this host's own claim among
//! them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use daemon_core::workspaces::{Key, Kind, Plan, PlanRefusal, Planned, Removal, Resolved};
use uuid::Uuid;

use super::{Environment, Holder, Maker, branch_start, canonical_beyond, refused};

/// The wt trees of one host.
pub struct WtBackend {
    /// wt's home, or none for wt's own default.
    home: Option<PathBuf>,
    /// What this host's trees record as their owner.
    owner: String,
    /// amux's claim on each tree it holds, by the tree's root.
    holds: Mutex<HashMap<String, ::wt::Hold>>,
}

/// The owner a tree this host made records.
pub fn owner_of(host: Uuid) -> String {
    format!("amux:{host}")
}

impl WtBackend {
    pub fn new(host: Uuid, home: Option<PathBuf>) -> Self {
        Self {
            home,
            owner: owner_of(host),
            holds: Mutex::new(HashMap::new()),
        }
    }

    /// wt as it stands now: its settings are read afresh each time, so a
    /// change a person makes counts from the next call.
    fn open(&self) -> Result<::wt::Wt, ::wt::Error> {
        match &self.home {
            Some(home) => ::wt::Wt::with_home(home),
            None => ::wt::Wt::open(),
        }
    }

    /// The tree whose root is the workspace's id.
    fn tree(&self, workspace: &Key) -> Result<::wt::Tree, String> {
        let wt = self.open().map_err(|error| error.to_string())?;
        let tree = wt
            .find(None, Path::new(&workspace.id))
            .map_err(|error| error.to_string())?;
        if canonical(&tree.root) != workspace.id {
            return Err(format!("{} is no longer a tree of its own", workspace.id));
        }
        Ok(tree)
    }
}

fn canonical(path: &Path) -> String {
    crate::plain_paths::canonical(path)
        .unwrap_or_else(|_| path.to_owned())
        .to_string_lossy()
        .into_owned()
}

impl Maker for WtBackend {
    fn plan(&self, folder: &Path, name: &str) -> Result<Option<Planned>, PlanRefusal> {
        let wt = self.open().map_err(|error| {
            refused(wire::ErrorCode::FailedPrecondition, format!("wt: {error}"))
        })?;
        let planned = match wt.plan(folder, name) {
            Ok(Some(planned)) => planned,
            Ok(None) => return Ok(None),
            Err(::wt::Error::Taken(why)) => return Err(PlanRefusal::Taken(why)),
            Err(error) => {
                return Err(refused(
                    wire::ErrorCode::FailedPrecondition,
                    format!("wt: {error}"),
                ));
            }
        };
        let (base_branch, from) = branch_start(folder);
        Ok(Some(Planned {
            workspace: Key {
                kind: Kind::Wt,
                id: canonical_beyond(&planned.path)
                    .to_string_lossy()
                    .into_owned(),
            },
            plan: Plan {
                repository: folder.to_string_lossy().into_owned(),
                name: planned.name,
                branch: planned.branch,
                from,
            },
            base_branch,
        }))
    }

    fn make(&self, workspace: &Key, plan: &Plan) -> Result<(), String> {
        let made = (|| {
            let wt = self.open()?;
            // Planned again, since anything may have changed since the
            // plan, and wt makes a tree only from a plan of its own.
            let planned = wt
                .plan(Path::new(&plan.repository), &plan.name)?
                .ok_or_else(|| {
                    ::wt::Error::Conflict("wt is no longer configured for the repository".into())
                })?;
            if canonical_beyond(&planned.path).to_string_lossy() != workspace.id {
                return Err(::wt::Error::Conflict(format!(
                    "the tree would now be at {}",
                    planned.path.display()
                )));
            }
            let options = ::wt::NewTree {
                from: plan.from.clone(),
                fetch: true,
                fresh_branch: true,
                owner: Some(self.owner.clone()),
            };
            let created = wt.new_tree(&planned, options)?;
            for warning in &created.warnings {
                tracing::info!(id = %workspace.id, %warning, "making a wt tree");
            }
            let tree = created.tree;
            tree.copy_inputs()?;
            // The create hook builds on in the background; wt keeps a
            // start, a stop or a removal behind it.
            if let Some(mut child) = tree.spawn_create()? {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Ok(())
        })();
        made.map_err(|error: ::wt::Error| format!("making the wt tree: {error}"))
    }

    fn start(&self, workspace: &Key) -> Result<(), String> {
        let tree = self.tree(workspace)?;
        let ran = tree
            .run_hook(::wt::Hook::Start, ::wt::RunHook::default())
            .map_err(|error| error.to_string())?;
        ran.problem().map_or(Ok(()), Err)
    }

    fn delete(&self, workspace: &Key, discard: bool) -> Removal {
        let Ok(wt) = self.open() else {
            return Removal::Kept;
        };
        let tree = match wt.find(None, Path::new(&workspace.id)) {
            Ok(tree) if canonical(&tree.root) == workspace.id => tree,
            // Nothing of it is wt's: a make that never got as far.
            _ if !Path::new(&workspace.id).exists() => return Removal::Removed,
            _ => return Removal::Kept,
        };
        // Only a tree this host made is its to remove or disown. Another
        // profile's make of the same tree may have won, and the tree is
        // then the winner's: this host leaves it as it is.
        if tree.owner().as_deref() != Some(self.owner.as_str()) {
            return Removal::Kept;
        }
        let options = ::wt::Remove {
            ignore_dirty: discard,
            delete_empty_branch: true,
            ..::wt::Remove::default()
        };
        match wt.remove(&tree, options) {
            Ok(removed) => {
                for warning in &removed.warnings {
                    tracing::warn!(id = %workspace.id, %warning, "removing a wt tree");
                }
                Removal::Removed
            }
            // Someone holds it, or it has work nobody agreed to lose: it
            // stays, nobody's.
            Err(::wt::Error::Held(why) | ::wt::Error::Conflict(why)) => {
                tracing::info!(id = %workspace.id, %why, "keeping a wt tree, disowned");
                if let Err(error) = tree.disown() {
                    tracing::warn!(id = %workspace.id, %error, "disowning a wt tree failed");
                }
                Removal::Kept
            }
            Err(error) => {
                tracing::warn!(id = %workspace.id, %error, "removing a wt tree failed");
                Removal::Kept
            }
        }
    }

    fn owned(&self) -> Vec<Key> {
        let Ok(trees) = self.open().and_then(|wt| wt.list()) else {
            return Vec::new();
        };
        trees
            .into_iter()
            .filter(|tree| tree.owner().as_deref() == Some(self.owner.as_str()))
            .map(|tree| Key {
                kind: Kind::Wt,
                id: canonical(&tree.root),
            })
            .collect()
    }
}

impl Holder for WtBackend {
    fn resolve(&self, folder: &Path) -> Option<Resolved> {
        let tree = self.open().ok()?.find(None, folder).ok()?;
        Some(Resolved {
            workspace: Key {
                kind: Kind::Wt,
                id: canonical(&tree.root),
            },
            made_here: tree.owner().as_deref() == Some(self.owner.as_str()),
        })
    }

    fn try_claim(&self, workspace: &Key) -> Result<bool, String> {
        let tree = self.tree(workspace)?;
        match tree.try_hold().map_err(|error| error.to_string())? {
            Some(hold) => {
                self.holds
                    .lock()
                    .unwrap()
                    .insert(workspace.id.clone(), hold);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn claim(&self, workspace: &Key) -> Result<(), String> {
        let tree = self.tree(workspace)?;
        let hold = tree.hold().map_err(|error| error.to_string())?;
        self.holds
            .lock()
            .unwrap()
            .insert(workspace.id.clone(), hold);
        Ok(())
    }

    fn release(&self, workspace: &Key) {
        self.holds.lock().unwrap().remove(&workspace.id);
    }

    fn stop_when_unheld(&self, workspace: &Key) -> Result<(), String> {
        let tree = self.tree(workspace)?;
        match tree
            .stop_when_unheld(::wt::RunHook::default())
            .map_err(|error| error.to_string())?
        {
            ::wt::Stopped::Ran(ran) => ran.problem().map_or(Ok(()), Err),
            ::wt::Stopped::Pinned | ::wt::Stopped::Gone => Ok(()),
        }
    }

    fn environment(&self, workspace: &Key) -> Result<Option<Environment>, String> {
        let env = self
            .tree(workspace)?
            .env()
            .map_err(|error| error.to_string())?;
        Ok(Some(Environment {
            vars: env.vars,
            path_prefix: env.path_prefix,
        }))
    }
}
