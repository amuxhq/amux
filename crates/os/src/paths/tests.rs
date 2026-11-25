use super::*;

#[test]
fn a_program_name_carries_the_platform_suffix() {
    assert_eq!(
        exe_name("amux"),
        format!("amux{}", std::env::consts::EXE_SUFFIX)
    );
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
