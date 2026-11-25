//! Each filesystem fault a test injects happens, and is undone, on the OS
//! the test runs on.

use std::io::ErrorKind;

use super::{self as faults, Deny, Linked};

/// Whether a permission fault can happen to this process; says why not.
fn enforced() -> bool {
    match faults::unenforced() {
        Some(why) => {
            eprintln!("skipped: {why}");
            false
        }
        None => true,
    }
}

#[test]
fn a_directory_denied_writes_refuses_a_new_file_until_the_guard_drops() {
    if !enforced() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("journal");
    std::fs::create_dir(&journal).unwrap();
    std::fs::write(journal.join("0"), "kept").unwrap();

    let denied = faults::deny(&journal, Deny::Write).unwrap();
    let error = std::fs::write(journal.join("1"), "refused").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::PermissionDenied, "{error}");
    assert_eq!(
        std::fs::read_to_string(journal.join("0")).unwrap(),
        "kept",
        "what is there still reads"
    );

    denied.undo().unwrap();
    std::fs::write(journal.join("1"), "written").unwrap();
}

#[test]
fn a_file_denied_reads_cannot_be_read_but_can_be_looked_at() {
    if !enforced() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("locked.txt");
    std::fs::write(&file, "secret").unwrap();

    let denied = faults::deny(&file, Deny::Read).unwrap();
    let error = std::fs::read(&file).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::PermissionDenied, "{error}");
    assert_eq!(std::fs::metadata(&file).unwrap().len(), 6);

    drop(denied);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "secret");
}

#[test]
fn a_file_denied_everything_opens_for_nothing() {
    if !enforced() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("root-owned.txt");
    std::fs::write(&file, "theirs").unwrap();

    let denied = faults::deny(&file, Deny::All).unwrap();
    let read = std::fs::File::open(&file).unwrap_err();
    assert_eq!(read.kind(), ErrorKind::PermissionDenied, "{read}");
    let write = std::fs::OpenOptions::new()
        .write(true)
        .open(&file)
        .unwrap_err();
    assert_eq!(write.kind(), ErrorKind::PermissionDenied, "{write}");

    drop(denied);
    std::fs::write(&file, "ours").unwrap();
}

#[test]
fn a_read_only_file_refuses_a_write_until_the_guard_drops() {
    if !enforced() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("spec.1");
    std::fs::write(&file, "first").unwrap();

    let read_only = faults::read_only(&file).unwrap();
    let error = std::fs::write(&file, "second").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::PermissionDenied, "{error}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "first");

    drop(read_only);
    std::fs::write(&file, "second").unwrap();
}

#[test]
fn a_file_held_exclusively_fails_every_other_open_with_a_sharing_violation() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("journal.0");
    std::fs::write(&file, "frames").unwrap();

    let held = match faults::hold_exclusively(&file) {
        Ok(held) => held,
        Err(error) if error.kind() == ErrorKind::Unsupported => {
            eprintln!("skipped: {error}");
            return;
        }
        Err(error) => panic!("holding the file: {error}"),
    };
    let read = std::fs::read(&file).unwrap_err();
    assert!(faults::is_sharing_violation(&read), "{read}");
    let renamed = std::fs::rename(&file, dir.path().join("moved")).unwrap_err();
    assert!(faults::is_sharing_violation(&renamed), "{renamed}");

    drop(held);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "frames");
}

#[test]
fn a_directory_link_reaches_its_target_and_goes_without_it() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("kept.txt"), "kept\n").unwrap();
    let link = dir.path().join("review");

    faults::link_dir(&elsewhere, &link).unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link is a link, not a directory of its own"
    );
    assert_eq!(
        std::fs::read_to_string(link.join("kept.txt")).unwrap(),
        "kept\n"
    );

    faults::remove_link(&link).unwrap();
    assert!(std::fs::symlink_metadata(&link).is_err());
    assert_eq!(
        std::fs::read_to_string(elsewhere.join("kept.txt")).unwrap(),
        "kept\n",
        "removing the link leaves what it names"
    );
}

#[test]
fn a_symbolic_link_is_made_or_skipped_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.txt");
    std::fs::write(&target, "through the link").unwrap();
    let link = dir.path().join("link.txt");

    match faults::symlink(&target, &link).unwrap() {
        Linked::Made => {
            assert_eq!(std::fs::read_link(&link).unwrap(), target);
            assert_eq!(std::fs::read_to_string(&link).unwrap(), "through the link");
            faults::remove_link(&link).unwrap();
            assert!(target.exists());
        }
        Linked::Skipped(why) => {
            assert!(!why.is_empty());
            assert!(std::fs::symlink_metadata(&link).is_err(), "nothing is left");
            eprintln!("skipped: {why}");
        }
    }
}
