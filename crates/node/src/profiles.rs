//! `<data_dir>/profiles/<id>/`: the profiles the installation hosts, one
//! directory each, named by the profile's id, which is also its host id,
//! the one its paired machines know it by. Each is a complete amux with
//! its own key, store and agents. A `profile` file in the directory says
//! what people call it and when it was made. A profile is made under a
//! dotted name and renamed into place whole, so a directory named by an id
//! without a profile file is one a crash left half removed, and goes at the
//! next listing.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::install::{AGENTS, PROFILES, REMOVAL_PATIENCE, remove_or_leave, write_durably};

pub type ProfileId = Uuid;

/// The client socket in each profile's directory.
pub const PROFILE_SOCKET: &str = "sock";
/// The file in a profile's directory that says what it is called.
pub const PROFILE_FILE: &str = "profile";

/// One profile as its directory describes it. Unknown keys are ignored: a
/// later build may add one, and this build must still start on a file that
/// build wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileEntry {
    /// The directory's name; the file does not repeat it.
    #[serde(skip)]
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

/// Every profile the installation hosts, oldest first. A directory named
/// by an id with no profile file is one a crash left half removed, and is
/// removed here; one still being made has a dotted name and is not read.
pub fn list(data_dir: &Path) -> io::Result<Vec<ProfileEntry>> {
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
                // One something still holds goes at a later listing.
                remove_or_leave(&entry.path(), Duration::ZERO, "a half-removed profile");
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
    write_entry_in(&profile_dir(data_dir, entry.id), entry)
}

fn write_entry_in(dir: &Path, entry: &ProfileEntry) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(entry).map_err(io::Error::other)?;
    write_durably(&dir.join(PROFILE_FILE), &bytes)
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

/// Creates a profile under a fresh id, built under a dotted name no
/// listing reads and renamed into place with its profile file, so neither
/// a crash nor a listing meanwhile sees half a profile. The label is made
/// unique.
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
    // What a crash left of an earlier try under this id.
    let staging = data_dir.join(PROFILES).join(format!(".{id}"));
    if os::fs::remove_patiently(&staging, REMOVAL_PATIENCE)? == os::fs::Removed::Later {
        return Err(io::Error::new(
            io::ErrorKind::ResourceBusy,
            format!(
                "{} is still in use from an earlier try at this profile",
                staging.display()
            ),
        ));
    }
    // A root made here is private from the start, as are the folders that
    // inherit from it; one already there is left as it is.
    crate::install::private_dir(data_dir)?;
    os::fs::create_private_dir(&staging)?;
    os::fs::create_private_dir(&staging.join(AGENTS))?;
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
    write_entry_in(&staging, &entry)?;
    os::fs::rename_patiently(&staging, &dir)?;
    crate::install::sync_dir(&data_dir.join(PROFILES))?;
    Ok(entry)
}

/// Removes a profile: its profile file first, so a crash midway leaves a
/// directory the next start removes, never a listed profile half gone.
/// What something else still holds once the profile file is gone (a shell
/// working inside, a log a person has open) is left, with a warning, for
/// the next listing to remove: the profile is gone either way.
pub(crate) fn discard(data_dir: &Path, profile: ProfileId) -> io::Result<()> {
    let dir = profile_dir(data_dir, profile);
    if os::fs::remove_patiently(&dir.join(PROFILE_FILE), REMOVAL_PATIENCE)?
        == os::fs::Removed::Later
    {
        return Err(io::Error::new(
            io::ErrorKind::ResourceBusy,
            format!("{} is open elsewhere", dir.join(PROFILE_FILE).display()),
        ));
    }
    if let Ok(agents) = std::fs::read_dir(dir.join(crate::install::AGENTS)) {
        for agent in agents.flatten() {
            agent_dir::local_socket::forget_short_addresses(&agent.path());
        }
    }
    remove_or_leave(&dir, REMOVAL_PATIENCE, "a removed profile's folder");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file a later build wrote still opens, as 0.8.4 did not with an
    /// account file holding a key it had dropped.
    #[test]
    fn a_profile_file_with_a_key_this_build_does_not_know_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(PROFILE_FILE),
            r#"{"label": "main", "created_ms": 1, "added_later": true}"#,
        )
        .unwrap();
        assert_eq!(read_entry(dir.path()).unwrap().label, "main");
    }

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
    fn a_directory_without_a_profile_file_is_a_half_removed_profile_and_goes() {
        let data_dir = tempfile::tempdir().unwrap();
        let kept = create_labelled(data_dir.path(), "kept").unwrap();
        let half = profile_dir(data_dir.path(), Uuid::new_v4());
        std::fs::create_dir_all(half.join(AGENTS)).unwrap();
        assert_eq!(list(data_dir.path()).unwrap(), vec![kept]);
        assert!(!half.exists());
    }

    /// A profile whose folder something else holds open, as a log a person
    /// follows on Windows is held, is still discarded: its file goes, the
    /// listing no longer shows it, and its folder goes at a listing once
    /// nothing holds it.
    #[test]
    fn a_profile_something_holds_is_discarded_and_its_folder_goes_later() {
        let data_dir = tempfile::tempdir().unwrap();
        let kept = create_labelled(data_dir.path(), "kept").unwrap();
        let gone = create_labelled(data_dir.path(), "gone").unwrap();
        let log = profile_dir(data_dir.path(), gone.id)
            .join(AGENTS)
            .join("agent.log");
        std::fs::write(&log, b"x").unwrap();
        // Unix has no share modes: nothing holds the file there.
        let held = os::testing::faults::hold_exclusively(&log).ok();

        discard(data_dir.path(), gone.id).unwrap();
        assert_eq!(list(data_dir.path()).unwrap(), vec![kept.clone()]);
        drop(held);
        assert_eq!(list(data_dir.path()).unwrap(), vec![kept]);
        assert!(!profile_dir(data_dir.path(), gone.id).exists());
    }

    #[test]
    fn a_profile_being_made_is_not_listed_or_removed() {
        let data_dir = tempfile::tempdir().unwrap();
        let kept = create_labelled(data_dir.path(), "kept").unwrap();
        let making = data_dir
            .path()
            .join(PROFILES)
            .join(format!(".{}", Uuid::new_v4()));
        std::fs::create_dir_all(making.join(AGENTS)).unwrap();
        assert_eq!(list(data_dir.path()).unwrap(), vec![kept]);
        assert!(making.exists());
    }

    #[test]
    fn the_profile_file_names_no_id_its_directory_does() {
        let data_dir = tempfile::tempdir().unwrap();
        let made = create_labelled(data_dir.path(), "kept").unwrap();
        let file =
            std::fs::read_to_string(profile_dir(data_dir.path(), made.id).join(PROFILE_FILE))
                .unwrap();
        assert!(!file.contains(&made.id.to_string()), "{file}");
        assert_eq!(list(data_dir.path()).unwrap()[0].id, made.id);
    }
}
