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

/// What the testing check calls private is what this module makes so, and
/// a folder made plainly, or a file private only by where it is, is not.
#[test]
fn the_privacy_check_tells_a_private_folder_from_a_plain_one() {
    let dir = tempfile::tempdir().unwrap();
    let private = dir.path().join("private");
    create_private_dir(&private).unwrap();
    assert!(is_private(&private).unwrap());
    let plain = dir.path().join("plain");
    fs::create_dir(&plain).unwrap();
    assert!(!is_private(&plain).unwrap());
    make_private(&plain).unwrap();
    assert!(is_private(&plain).unwrap());
    let secret = dir.path().join("secret");
    drop(create_private(&secret, Create::New).unwrap());
    assert!(is_private(&secret).unwrap());
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

/// A deep path's long form is one a library calling Windows itself can
/// open (verbatim there); a shallow one is left plain, as it is elsewhere.
#[test]
fn a_deep_path_has_a_long_form_and_a_shallow_one_stays_plain() {
    let dir = tempfile::tempdir().unwrap();
    let shallow = canonical(dir.path()).unwrap().join("store.sqlite");
    assert_eq!(long_form(&shallow), shallow);
    let deep = shallow
        .with_file_name("deep".repeat(70))
        .join("store.sqlite");
    let long = long_form(&deep);
    assert_eq!(
        long.to_string_lossy().starts_with(r"\\?\"),
        cfg!(windows),
        "{}",
        long.display()
    );
    assert!(long.ends_with(Path::new(&"deep".repeat(70)).join("store.sqlite")));
}

/// A path's short form names the same file. On Windows each long name
/// becomes its 8.3 one, unless the volume keeps none, and a path past
/// `MAX_PATH` comes back plain once its short form is under it; a missing
/// path has no short form there. Elsewhere the path is as it was.
#[test]
fn a_short_path_names_the_same_file_in_8_3_names_where_there_are_some() {
    let dir = tempfile::tempdir().unwrap();
    let names = ["a folder with a long name", "another long-named folder"];
    let top = canonical(dir.path()).unwrap();
    let mut deep = top.clone();
    for (i, name) in names.iter().cycle().take(10).enumerate() {
        deep.push(format!("{name} {i}"));
    }
    fs::create_dir_all(&deep).unwrap();
    let file = deep.join("a config file with a long name.toml");
    fs::write(&file, b"").unwrap();
    assert!(file.as_os_str().len() > 260);

    let short = short_path(&file).unwrap();
    assert_eq!(FileId::of(&short).unwrap(), FileId::of(&file).unwrap());
    let text = short.to_string_lossy();
    // Only Windows has short names; elsewhere the path is as it was.
    assert!(cfg!(windows) || short == file, "{text}");
    if !short.ends_with(file.strip_prefix(&top).unwrap()) {
        // The volume keeps short names: every long one went.
        assert!(text.len() < 260 && !text.starts_with(r"\\?\"), "{text}");
        let tail = short.components().rev().take(11);
        for name in tail {
            let name = name.as_os_str().to_string_lossy();
            assert!(name.len() <= 12 && name.contains('~'), "{name} in {text}");
        }
    }
    // Windows has no short form of a missing path.
    assert_eq!(short_path(&deep.join("missing")).is_err(), cfg!(windows));
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
    write_durably(&root.join("inside"), b"x").unwrap();
    make_private(&root).unwrap();
    create_private_dir(&root).unwrap();
    assert!(crate::testing::is_private(&root).unwrap());
    assert!(crate::testing::is_private(&root.join("inside")).unwrap());
}

/// Two names of one file share its identity; another file has its own.
#[test]
fn a_hard_link_shares_its_files_identity() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one");
    fs::write(&one, "one").unwrap();
    let linked = dir.path().join("linked");
    fs::hard_link(&one, &linked).unwrap();
    let other = dir.path().join("other");
    fs::write(&other, "one").unwrap();
    assert_eq!(FileId::of(&one).unwrap(), FileId::of(&linked).unwrap());
    assert_ne!(FileId::of(&one).unwrap(), FileId::of(&other).unwrap());
    assert_eq!(
        FileId::of(&dir.path().join("missing")).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

/// A new file has no execute bit; one made executable has it where files
/// have one, which Windows' don't.
#[test]
fn the_execute_bit_is_the_one_make_executable_sets() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("tool");
    fs::write(&file, "tool").unwrap();
    assert!(!has_execute_bit(&fs::metadata(&file).unwrap()));
    make_executable(&file).unwrap();
    assert_eq!(
        has_execute_bit(&fs::metadata(&file).unwrap()),
        !cfg!(windows)
    );
}

/// A copy of a built program can be signed again, and still runs as a
/// program; only macOS changes it.
#[test]
fn a_copied_program_is_signed_ad_hoc() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join(crate::paths::exe_name("copy"));
    fs::copy(std::env::current_exe().unwrap(), &program).unwrap();
    make_executable(&program).unwrap();
    let before = fs::read(&program).unwrap();
    sign_ad_hoc(&program).unwrap();
    assert!(is_executable(&program, &fs::metadata(&program).unwrap()));
    if std::env::consts::OS != "macos" {
        assert!(fs::read(&program).unwrap() == before, "the program changed");
    }
}

/// A file no process quarantined comes out of a clearing as it was.
#[test]
fn a_file_with_no_quarantine_is_cleared_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("download");
    fs::write(&file, "bytes").unwrap();
    clear_quarantine(&file).unwrap();
    clear_quarantine(&file).unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "bytes");
}

/// A file's and a folder's modification time are set alike.
#[test]
fn set_modified_sets_a_files_and_a_folders_time() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    fs::write(&file, "file").unwrap();
    let folder = dir.path().join("folder");
    fs::create_dir(&folder).unwrap();
    let at = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for path in [&file, &folder] {
        set_modified(path, at).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().modified().unwrap(),
            at,
            "{path:?}"
        );
    }
}
