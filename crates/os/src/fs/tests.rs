use std::fs;

use super::*;

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_durable_write_replaces_the_file_and_leaves_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("generation");
    write_durably(&path, b"one").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"one");
    write_durably(&path, b"two").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"two");
    assert_eq!(names(dir.path()), ["generation"]);
}

#[test]
fn a_durable_write_into_a_missing_folder_fails_and_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing").join("file");
    assert_eq!(
        write_durably(&path, b"x").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert!(names(dir.path()).is_empty());
}

#[test]
fn a_durable_creation_refuses_an_existing_file_and_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spec.1");
    create_new_durably(&path, b"first").unwrap();
    let error = create_new_durably(&path, b"second").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{error}");
    assert_eq!(fs::read(&path).unwrap(), b"first");
    assert_eq!(names(dir.path()), ["spec.1"]);
}

#[test]
fn of_many_durable_creations_of_one_file_exactly_one_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("device.key");
    let wins: usize = std::thread::scope(|scope| {
        let writers: Vec<_> = (0..8u8)
            .map(|n| {
                let path = &path;
                scope.spawn(move || create_new_durably(path, &[n]).is_ok())
            })
            .collect();
        writers
            .into_iter()
            .map(|writer| usize::from(writer.join().unwrap()))
            .sum()
    });
    assert_eq!(wins, 1);
    assert_eq!(names(dir.path()), ["device.key"]);
}

#[test]
fn a_durable_write_needs_a_file_name() {
    assert_eq!(
        write_durably(Path::new("/"), b"x").unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn a_folder_syncs() {
    let dir = tempfile::tempdir().unwrap();
    sync_dir(dir.path()).unwrap();
}

#[test]
fn a_patient_rename_replaces_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let from = dir.path().join("from");
    let to = dir.path().join("to");
    fs::write(&from, b"new").unwrap();
    fs::write(&to, b"old").unwrap();
    rename_patiently(&from, &to).unwrap();
    assert_eq!(fs::read(&to).unwrap(), b"new");
    assert_eq!(names(dir.path()), ["to"]);
}

#[test]
fn patient_removal_removes_a_file_a_tree_a_read_only_file_and_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    fs::write(&file, b"x").unwrap();
    let tree = dir.path().join("tree");
    fs::create_dir_all(tree.join("a").join("b")).unwrap();
    fs::write(tree.join("a").join("b").join("c"), b"x").unwrap();
    let read_only = dir.path().join("object");
    fs::write(&read_only, b"x").unwrap();
    let mut permissions = fs::metadata(&read_only).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&read_only, permissions).unwrap();

    let patience = Duration::from_millis(200);
    for path in [&file, &tree, &read_only] {
        assert_eq!(
            remove_patiently(path, patience).unwrap(),
            Removed::Gone,
            "{}",
            path.display()
        );
        assert!(!path.exists());
    }
    assert_eq!(remove_patiently(&file, patience).unwrap(), Removed::Gone);
}

#[test]
fn canonical_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let inner = dir.path().join("shop");
    fs::create_dir(&inner).unwrap();
    let canonical_once = canonical(&inner).unwrap();
    assert!(canonical_once.is_absolute());
    assert!(
        !canonical_once.to_string_lossy().starts_with(r"\\?\"),
        "{}",
        canonical_once.display()
    );
    assert_eq!(canonical(&canonical_once).unwrap(), canonical_once);
    assert_eq!(plain(canonical_once.clone()), canonical_once);
    let through_dot = inner.join(".").join("..").join("shop");
    assert_eq!(canonical(&through_dot).unwrap(), canonical_once);
    assert!(canonical_once.is_dir());
}

#[test]
fn an_executable_is_executable_and_a_plain_file_on_unix_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join(crate::paths::exe_name("tool"));
    fs::write(&program, b"").unwrap();
    make_executable(&program).unwrap();
    assert!(is_executable(&program, &fs::metadata(&program).unwrap()));
    assert!(!is_executable(
        dir.path(),
        &fs::metadata(dir.path()).unwrap()
    ));
    let text = dir.path().join("notes.txt");
    fs::write(&text, b"").unwrap();
    assert!(!is_executable(&text, &fs::metadata(&text).unwrap()));
}

#[test]
fn a_private_folder_is_created_with_its_parents_and_an_existing_one_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("a").join("root");
    create_private_dir(&root).unwrap();
    assert!(root.is_dir());
    fs::write(root.join("kept"), b"x").unwrap();
    create_private_dir(&root).unwrap();
    assert_eq!(names(&root), ["kept"]);
}

/// What each way of creating a private file does with a file already
/// there: New refuses it, Truncate empties it, Append writes after it.
#[test]
fn a_private_file_is_created_truncated_or_appended_to() {
    use std::io::{Read as _, Seek as _, Write as _};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret");
    let mut file = create_private(&path, Create::New).unwrap();
    file.write_all(b"one").unwrap();
    drop(file);
    assert_eq!(
        create_private(&path, Create::New).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(&path).unwrap(), b"one");

    let mut file = create_private(&path, Create::Append).unwrap();
    file.write_all(b"two").unwrap();
    file.rewind().unwrap();
    let mut read = String::new();
    file.read_to_string(&mut read).unwrap();
    assert_eq!(read, "onetwo", "appended, and readable");
    drop(file);

    let mut file = create_private(&path, Create::Truncate).unwrap();
    file.write_all(b"three").unwrap();
    drop(file);
    assert_eq!(fs::read(&path).unwrap(), b"three");
    assert_eq!(names(dir.path()), ["secret"]);
}

#[test]
fn an_atomic_replace_replaces_and_leaves_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    replace_atomically(&path, b"one").unwrap();
    replace_atomically(&path, b"two").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"two");
    assert_eq!(names(dir.path()), ["ctl.sock"]);
    let missing = dir.path().join("missing").join("file");
    assert!(replace_atomically(&missing, b"x").is_err());
    assert_eq!(names(dir.path()), ["ctl.sock"]);
    assert!(left_stages().is_empty(), "nothing is left to remove later");
}

#[test]
fn the_temporary_folder_can_hold_a_root() {
    let dir = tempfile::tempdir().unwrap();
    let check = volume_supports_root(&dir.path().join("not").join("yet")).unwrap();
    assert_eq!(check, VolumeCheck::Suitable, "{check}");
}

#[test]
fn a_device_name_or_a_trailing_dot_or_space_is_reserved_on_every_os() {
    for name in [
        "con", "NUL", "aux", "com1", "lpt9", "prn.txt", "name.", "name ",
    ] {
        assert!(reserved_name(name), "{name:?}");
    }
    for name in ["console", "com10", "fix-login", "a.con", "nul-checker"] {
        assert!(!reserved_name(name), "{name:?}");
    }
}

#[test]
fn making_a_private_folder_private_again_keeps_it_private() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    create_private_dir(&root).unwrap();
    fs::write(root.join("inside"), b"x").unwrap();
    make_private(&root).unwrap();
    create_private_dir(&root).unwrap();
    assert!(crate::testing::private::owner_only(&root).unwrap());
    assert!(crate::testing::private::owner_only(&root.join("inside")).unwrap());
}
