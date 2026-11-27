use super::*;

/// A path printed in UTF-8 reads back as itself everywhere.
#[test]
fn a_utf8_path_from_bytes_is_itself() {
    assert_eq!(
        from_bytes("src/main é.rs".as_bytes()),
        PathBuf::from("src/main é.rs")
    );
}

#[test]
fn a_program_name_carries_the_platform_suffix() {
    assert_eq!(
        exe_name("amux"),
        format!("amux{}", std::env::consts::EXE_SUFFIX)
    );
}

/// A variable set over an environment replaces it under any spelling the
/// OS takes for its name (Windows: `PATH` replaces `Path`), and nothing
/// else; on Unix `Path` is another variable.
#[test]
fn a_variable_set_replaces_it_under_any_spelling_the_os_takes_for_it() {
    let mut env = HashMap::from([
        ("Path".to_owned(), "host".to_owned()),
        ("Other".to_owned(), "1".to_owned()),
    ]);
    set_vars(&mut env, [("PATH".to_owned(), "own".to_owned())]);
    assert_eq!(env["PATH"], "own");
    assert_eq!(env["Other"], "1");
    assert_eq!(env.contains_key("Path"), !same_variable("Path", "PATH"));
    assert_eq!(env.len(), if same_variable("Path", "PATH") { 2 } else { 3 });
}

#[test]
fn the_home_folder_is_known() {
    let home = home_dir().expect("a home folder");
    assert!(home.is_absolute(), "{}", home.display());
}

#[test]
fn a_program_given_by_path_is_found_there_and_nowhere_else() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join(exe_name("tool"));
    std::fs::write(&program, b"").unwrap();
    crate::fs::make_executable(&program).unwrap();
    let found = find_program(program.as_os_str(), OsStr::new(""));
    assert_eq!(found, Some(program.clone()));
    assert_eq!(find_program(OsStr::new("tool"), OsStr::new("")), None);
    assert_eq!(
        find_program(OsStr::new("tool"), dir.path().as_os_str()),
        Some(program)
    );
}
