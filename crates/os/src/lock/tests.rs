use std::fs::{self, OpenOptions, TryLockError};

use super::*;

/// What a build before `FileLock` did: std's whole-file lock.
fn old_style_lock(path: &Path) -> Option<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    match file.try_lock() {
        Ok(()) => Some(file),
        Err(TryLockError::WouldBlock) => None,
        Err(TryLockError::Error(error)) => panic!("{error}"),
    }
}

#[test]
fn a_held_lock_keeps_its_data_readable_and_rewritable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("supervisor.lock");
    let mut lock = FileLock::try_acquire(&path).unwrap().expect("free");
    lock.write_data(b"4242").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "4242");
    lock.write_data(b"7").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "7");
    assert!(FileLock::held(&path).unwrap());
}

#[test]
fn a_lock_excludes_a_second_taker_until_it_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lock");
    assert!(!FileLock::held(&path).unwrap(), "no file, no lock");
    let lock = FileLock::try_acquire(&path).unwrap().expect("free");
    assert!(FileLock::try_acquire(&path).unwrap().is_none());
    assert!(FileLock::held(&path).unwrap());
    drop(lock);
    assert!(!FileLock::held(&path).unwrap());
    assert!(FileLock::try_acquire(&path).unwrap().is_some());
}

#[test]
fn taking_the_lock_keeps_what_the_file_holds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lock");
    fs::write(&path, b"1234").unwrap();
    let _lock = FileLock::try_acquire(&path).unwrap().expect("free");
    assert_eq!(fs::read_to_string(&path).unwrap(), "1234");
}

/// An older build holds the file with std's lock; this build, probing or
/// taking it, sees it held.
#[test]
fn an_old_style_holder_excludes_a_new_prober() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("installation.lock");
    let old = old_style_lock(&path).expect("free");
    assert!(FileLock::held(&path).unwrap());
    assert!(FileLock::try_acquire(&path).unwrap().is_none());
    old.unlock().unwrap();
    drop(old);
    assert!(!FileLock::held(&path).unwrap());
}

/// This build holds the file; an older build, probing with std's lock,
/// sees it held.
#[test]
fn a_new_holder_excludes_an_old_style_prober() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("installation.lock");
    let mut lock = FileLock::try_acquire(&path).unwrap().expect("free");
    lock.write_data(b"99").unwrap();
    assert!(old_style_lock(&path).is_none());
    drop(lock);
    assert!(old_style_lock(&path).is_some());
}
