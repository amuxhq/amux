//! Typed keys into an `experimental` map.
//!
//! An amux.v1 message an experiment extends carries `map<string, bytes>
//! experimental`, each entry an encoded `amux.experimental` message under
//! the version of the experiment that wrote it. An [`Entry`] names one such
//! entry with the message it holds, so every reader and writer of it goes
//! through one key instead of a version string and a decode of its own.

use std::collections::BTreeMap;
use std::marker::PhantomData;

/// An `experimental` map as the wire carries it: version to encoded
/// message.
pub type Map = BTreeMap<String, Vec<u8>>;

/// The entry an experiment's version keeps in an `experimental` map, holding
/// an `M`.
pub struct Entry<M> {
    version: &'static str,
    // A function pointer so the key is `Copy`, `Send` and `Sync` whatever
    // `M` is: it names the type and holds none.
    _message: PhantomData<fn() -> M>,
}

impl<M> Clone for Entry<M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M> Copy for Entry<M> {}

impl<M> std::fmt::Debug for Entry<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Entry").field(&self.version).finish()
    }
}

impl<M> Entry<M> {
    /// The entry kept under `version`, `<name>/<n>`.
    pub const fn new(version: &'static str) -> Self {
        Entry {
            version,
            _message: PhantomData,
        }
    }

    /// The version it is keyed by: `workspaces/1`.
    pub const fn version(&self) -> &'static str {
        self.version
    }
}

impl<M: prost::Message + Default> Entry<M> {
    /// The entry in `map`, decoded: none when the map holds none, or when
    /// it does not decode, which is logged. An experiment's entry is never
    /// worth failing over.
    pub fn get(&self, map: &Map) -> Option<M> {
        let bytes = map.get(self.version)?;
        match M::decode(bytes.as_slice()) {
            Ok(message) => Some(message),
            Err(error) => {
                tracing::warn!(version = self.version, %error, "an experimental entry does not decode");
                None
            }
        }
    }

    /// Sets the entry in `map` to `value`, leaving every other entry.
    pub fn put(&self, map: &mut Map, value: &M) {
        map.insert(self.version.to_owned(), value.encode_to_vec());
    }

    /// Removes the entry from `map`, leaving every other entry.
    pub fn clear(&self, map: &mut Map) {
        map.remove(self.version);
    }
}

#[cfg(test)]
mod tests {
    use prost::Message as _;

    use super::*;
    use crate::experimental::{AgentWorkspace, SUSPENSION, SuspensionSpec, WORKSPACE};

    /// A key reads and writes only its own version's entry, and an entry
    /// that does not decode reads as none.
    #[test]
    fn a_key_reads_and_writes_only_its_own_entry() {
        let later: Entry<AgentWorkspace> = Entry::new("workspaces/2");
        let workspace = AgentWorkspace {
            id: "/src/app".into(),
            made_by_amux: true,
            ..Default::default()
        };
        let mut map = Map::new();
        assert_eq!(WORKSPACE.get(&map), None);
        WORKSPACE.put(&mut map, &workspace);
        later.put(&mut map, &AgentWorkspace::default());
        SUSPENSION.put(&mut map, &SuspensionSpec { after_ms: 5 });
        assert_eq!(
            map.keys().collect::<Vec<_>>(),
            ["suspension/1", "workspaces/1", "workspaces/2"]
        );
        assert_eq!(map["workspaces/1"], workspace.encode_to_vec());
        assert_eq!(WORKSPACE.get(&map), Some(workspace));
        assert_eq!(SUSPENSION.get(&map), Some(SuspensionSpec { after_ms: 5 }));
        later.clear(&mut map);
        assert_eq!(
            map.keys().collect::<Vec<_>>(),
            ["suspension/1", "workspaces/1"]
        );
        map.insert(WORKSPACE.version().to_owned(), vec![0xff]);
        assert_eq!(WORKSPACE.get(&map), None);
        assert!(map.contains_key("workspaces/1"), "a read changes nothing");
        WORKSPACE.clear(&mut map);
        assert_eq!(map.keys().collect::<Vec<_>>(), ["suspension/1"]);
    }
}
