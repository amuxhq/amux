use std::fs;

use super::*;

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

/// std's lock and this one on one file exclude each other, both ways:
/// `supervisor.lock` and `installation.lock` are std locks that tests
/// probe with [`FileLock::held`].
#[test]
fn a_std_lock_and_a_file_lock_exclude_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("supervisor.lock");
    let std_lock = || {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .unwrap();
        match file.try_lock() {
            Ok(()) => Some(file),
            Err(fs::TryLockError::WouldBlock) => None,
            Err(fs::TryLockError::Error(error)) => panic!("{error}"),
        }
    };
    let held = std_lock().expect("free");
    assert!(FileLock::held(&path).unwrap());
    assert!(FileLock::try_acquire(&path).unwrap().is_none());
    drop(held);
    let lock = FileLock::try_acquire(&path).unwrap().expect("free");
    assert!(std_lock().is_none());
    drop(lock);
    assert!(std_lock().is_some());
}

/// A probe opens the file to write, as an NFS mount's exclusive lock
/// needs, and one it cannot open or lock is an error, never a free lock:
/// here the path is a folder.
#[test]
fn a_probe_that_cannot_lock_the_file_fails_rather_than_saying_free() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lock");
    fs::create_dir(&path).unwrap();
    assert!(FileLock::held(&path).is_err());
}

/// A lock file this user may only read is still probed, read-only.
#[test]
fn a_read_only_lock_file_is_still_probed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lock");
    let lock = FileLock::try_acquire(&path).unwrap().expect("free");
    let read_only = crate::testing::faults::read_only(&path).unwrap();
    assert!(FileLock::held(&path).unwrap());
    drop(lock);
    assert!(!FileLock::held(&path).unwrap());
    drop(read_only);
}
