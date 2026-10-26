//! `<data_dir>/profiles/<id>/`: the profiles the installation hosts, one
//! directory each, named by the profile's id, which is also its host id,
//! the one its paired machines know it by. Each is a complete amux with
//! its own key, store and agents. A `profile` file in the directory says
//! what people call it and when it was made; a directory without one is a
//! profile a crash left half-made, and goes at the next start.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::install::{AGENTS, PROFILES, private_dir, write_durably};

pub type ProfileId = Uuid;

/// The client socket in each profile's directory.
pub const PROFILE_SOCKET: &str = "sock";
/// The file in a profile's directory that says what it is called.
pub const PROFILE_FILE: &str = "profile";
/// Where builds before the profile file listed the profiles. Its
/// directories have no profile file, and their stores know the machine by
/// a host id apart from the profile's, so they are neither half-made nor
/// readable as they stand: such an installation is refused, not tidied.
const OLD_REGISTRY: &str = "registry";

/// One profile as its directory describes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileEntry {
    pub id: ProfileId,
    /// What people call it; unique within the installation.
    pub label: String,
    /// When it was made, as Unix milliseconds: profiles are listed oldest
    /// first.
    pub created_ms: i64,
}

/// Where one profile's files live.
pub fn profile_dir(data_dir: &Path, profile: ProfileId) -> PathBuf {
    data_dir.join(PROFILES).join(profile.to_string())
}

/// Every profile the installation hosts, oldest first. A directory under
/// `profiles/` with no profile file is one a crash left half-made, or half
/// removed, and is removed here.
pub fn list(data_dir: &Path) -> io::Result<Vec<ProfileEntry>> {
    if data_dir.join(OLD_REGISTRY).exists() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} was made by an older amux whose profiles this build cannot read; \
                 remove it to start afresh",
                data_dir.display()
            ),
        ));
    }
    let entries = match std::fs::read_dir(data_dir.join(PROFILES)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut profiles = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| Uuid::parse_str(name).ok())
        else {
            continue;
        };
        match read_entry(&entry.path()) {
            Ok(mut profile) => {
                profile.id = id;
                profiles.push(profile);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                std::fs::remove_dir_all(entry.path())?;
            }
            Err(error) => return Err(error),
        }
    }
    profiles.sort_by_key(|profile| (profile.created_ms, profile.id));
    Ok(profiles)
}

fn read_entry(dir: &Path) -> io::Result<ProfileEntry> {
    let bytes = std::fs::read(dir.join(PROFILE_FILE))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Writes what the profile is called, durably, over what it said before.
pub(crate) fn write_entry(data_dir: &Path, entry: &ProfileEntry) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(entry).map_err(io::Error::other)?;
    write_durably(&profile_dir(data_dir, entry.id).join(PROFILE_FILE), &bytes)
}

/// A label no profile has yet: `wanted`, or `wanted-2`, `wanted-3`…
fn free_label(profiles: &[ProfileEntry], wanted: &str) -> String {
    let taken = |label: &str| profiles.iter().any(|entry| entry.label == label);
    if !taken(wanted) {
        return wanted.to_owned();
    }
    (2..)
        .map(|n| format!("{wanted}-{n}"))
        .find(|label| !taken(label))
        .expect("some suffix is free")
}

/// Creates a profile under a fresh id: its directory, then its profile
/// file last, so a crash leaves no half-made profile listed. The label is
/// made unique.
pub fn create_profile(data_dir: &Path) -> io::Result<ProfileId> {
    create_labelled(data_dir, "default").map(|entry| entry.id)
}

pub(crate) fn create_labelled(data_dir: &Path, label: &str) -> io::Result<ProfileEntry> {
    create_as(data_dir, Uuid::new_v4(), label)
}

/// Creates a profile under `id`, for a harness that starts a machine under
/// an id another machine already has.
pub fn create_profile_as(data_dir: &Path, id: ProfileId) -> io::Result<ProfileId> {
    create_as(data_dir, id, "default").map(|entry| entry.id)
}

fn create_as(data_dir: &Path, id: ProfileId, label: &str) -> io::Result<ProfileEntry> {
    let profiles = list(data_dir)?;
    let dir = profile_dir(data_dir, id);
    private_dir(&dir.join(AGENTS))?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64);
    let entry = ProfileEntry {
        id,
        label: free_label(&profiles, label),
        // After every profile already made, even one made in the same
        // millisecond: listed oldest first, profiles are in the order they
        // were made, and the first, which commands use by default, never
        // falls to which random id sorts lower.
        created_ms: profiles
            .iter()
            .map(|entry| entry.created_ms.saturating_add(1))
            .fold(now_ms, i64::max),
    };
    write_entry(data_dir, &entry)?;
    Ok(entry)
}

/// Removes a profile: its profile file first, so a crash midway leaves a
/// directory the next start removes, never a listed profile half gone.
pub(crate) fn discard(data_dir: &Path, profile: ProfileId) -> io::Result<()> {
    let dir = profile_dir(data_dir, profile);
    match std::fs::remove_file(dir.join(PROFILE_FILE)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    match std::fs::remove_dir_all(dir) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_are_their_directories_oldest_first_and_a_discarded_one_is_gone() {
        let data_dir = tempfile::tempdir().unwrap();
        let kept = create_labelled(data_dir.path(), "kept").unwrap();
        let gone = create_labelled(data_dir.path(), "kept").unwrap();
        assert_eq!(gone.label, "kept-2", "labels are made unique");
        assert_eq!(
            list(data_dir.path())
                .unwrap()
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            [kept.id, gone.id]
        );

        assert!(
            gone.created_ms > kept.created_ms,
            "made in one millisecond or not"
        );

        discard(data_dir.path(), gone.id).unwrap();
        assert_eq!(list(data_dir.path()).unwrap(), vec![kept.clone()]);
        assert!(!profile_dir(data_dir.path(), gone.id).exists());
        // Discarding what is already gone is not an error.
        discard(data_dir.path(), gone.id).unwrap();
    }

    #[test]
    fn a_directory_without_a_profile_file_is_a_half_made_profile_and_goes() {
        let data_dir = tempfile::tempdir().unwrap();
        let kept = create_labelled(data_dir.path(), "kept").unwrap();
        let half = profile_dir(data_dir.path(), Uuid::new_v4());
        private_dir(&half.join(AGENTS)).unwrap();
        assert_eq!(list(data_dir.path()).unwrap(), vec![kept]);
        assert!(!half.exists());
    }

    #[test]
    fn an_installation_from_before_the_profile_file_is_refused_and_left_alone() {
        let data_dir = tempfile::tempdir().unwrap();
        let old = profile_dir(data_dir.path(), Uuid::new_v4());
        private_dir(&old.join(AGENTS)).unwrap();
        std::fs::write(data_dir.path().join(OLD_REGISTRY), b"{}").unwrap();
        let refused = list(data_dir.path()).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::InvalidData);
        assert!(old.exists(), "its profiles stay for the person to remove");
    }
}
