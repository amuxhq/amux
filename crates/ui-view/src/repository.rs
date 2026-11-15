//! The repository an agent works in, as every client marks it: its name,
//! two letters for its tile, and the tint the tile takes. Checkouts of one
//! repository share all three wherever they are — worktrees, and the same
//! repository on another machine — because they share a remote.

use schemars::JsonSchema;
use serde::Serialize;

/// How many tints a repository's tile can take. Each client draws them in
/// its own palette; the index is what they agree on.
pub const REPOSITORY_TINTS: u8 = 8;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct Repository {
    /// What its checkouts share: the remote, else the folder's path.
    pub key: String,
    /// What a person calls it: the remote's last segment, else the
    /// folder's name. "amux".
    pub name: String,
    /// The first and last letter or digit of the name, lower case: "ax".
    pub mark: String,
    /// Below [`REPOSITORY_TINTS`], the same for every checkout of it.
    pub tint: u8,
}

impl Repository {
    /// From the agent's remote, else its folder. None when neither names
    /// anything a tile can show.
    pub fn of(remote: Option<&str>, cwd: &str) -> Option<Self> {
        let key = match remote {
            Some(remote) if !remote.is_empty() => remote,
            _ => cwd.trim_end_matches(['/', '\\']),
        };
        let name = key.rsplit(['/', '\\']).next().unwrap_or(key).to_owned();
        let letters: Vec<char> = name.chars().filter(|c| c.is_alphanumeric()).collect();
        let mark: String = match letters.as_slice() {
            [] => return None,
            [only] => only.to_lowercase().collect(),
            [first, .., last] => first.to_lowercase().chain(last.to_lowercase()).collect(),
        };
        Some(Self {
            key: key.to_owned(),
            name,
            mark,
            tint: tint(key),
        })
    }
}

/// FNV-1a over the key, mixed with MurmurHash3's finaliser so the low bits
/// spread: stable across builds and platforms, unlike the standard hasher.
fn tint(key: &str) -> u8 {
    let mut hash = key.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x85eb_ca6b);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(0xc2b2_ae35);
    hash ^= hash >> 16;
    (hash % u32::from(REPOSITORY_TINTS)) as u8
}

#[cfg(test)]
mod tests {
    use super::Repository;

    #[test]
    fn a_remote_names_the_repository_wherever_it_is_checked_out() {
        let main = Repository::of(Some("github.com/amuxhq/amux"), "/Users/ada/src/amux").unwrap();
        let worktree = Repository::of(
            Some("github.com/amuxhq/amux"),
            "/Users/ada/.wt/trees/amux/macgui",
        )
        .unwrap();
        assert_eq!(main, worktree);
        assert_eq!(main.name, "amux");
        assert_eq!(main.mark, "ax");
    }

    #[test]
    fn without_a_remote_the_folder_names_it() {
        let local = Repository::of(None, "/Users/ada/notes/").unwrap();
        assert_eq!((local.name.as_str(), local.mark.as_str()), ("notes", "ns"));
        assert_eq!(Repository::of(None, "/x").unwrap().mark, "x");
        assert_eq!(Repository::of(None, "/").map(|r| r.mark), None);
        assert_eq!(Repository::of(None, "C:\\src\\Amux-2").unwrap().mark, "a2");
    }

    #[test]
    fn the_tint_is_fixed_for_a_key() {
        // Clients store nothing about tints: a change here recolours every
        // repository everywhere, so the values are pinned.
        let tints: Vec<u8> = ["amux", "website", "notes", "skills", "amuxcloud", "wt"]
            .iter()
            .map(|name| super::tint(&format!("github.com/amuxhq/{name}")))
            .collect();
        assert_eq!(tints, [5, 7, 6, 3, 0, 1]);
    }
}
