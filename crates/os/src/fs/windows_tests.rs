//! `os::fs` on Windows: what only Windows can show.

use std::os::windows::fs::OpenOptionsExt as _;
use std::time::{Duration, Instant};

use super::testing::{dacl_sddl, sddl_trustee};
use super::*;
use crate::fs::{Removed, canonical, plain, remove_patiently, write_durably};

/// A folder beside the test program, which with the build's target folder
/// outside the user profile (`C:\amux-cargo\…` here) is a root that does
/// not inherit `%LOCALAPPDATA%`'s owner-only ACL; removed when dropped.
struct OutsideProfile(PathBuf);

impl OutsideProfile {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let exe = std::env::current_exe().unwrap();
        let path = exe
            .parent()
            .unwrap()
            .join(format!("dacl-test-{}-{nanos}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for OutsideProfile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The DACL's flags (`P`, `AI`…) and its ACEs, from its SDDL.
fn parse(sddl: &str) -> (String, Vec<String>) {
    let dacl = sddl.strip_prefix("D:").unwrap_or_else(|| panic!("{sddl}"));
    let flags = dacl.split('(').next().unwrap().to_owned();
    let aces = dacl
        .split('(')
        .skip(1)
        .map(|ace| ace.trim_end_matches(')').to_owned())
        .collect();
    (flags, aces)
}

/// The ACEs of an owner-only DACL as SDDL reads them back: this user by
/// the name SDDL gives it, which for the built-in Administrator is `LA`,
/// not the SID the DACL was made from (CI run 45718964949).
fn owner_only(inheritance: &str) -> Vec<String> {
    let user = sddl_trustee(&current_user_sid().unwrap()).unwrap();
    [user.as_str(), "SY", "BA"]
        .iter()
        .map(|who| format!("A;{inheritance};FA;;;{who}"))
        .collect()
}

#[test]
fn a_private_folder_outside_the_profile_reads_back_owner_only_and_passes_it_on() {
    let root = OutsideProfile::new();
    let (_, inherited) = parse(&dacl_sddl(&root.0).unwrap());
    eprintln!(
        "the test root at {} inherits {inherited:?}",
        root.0.display()
    );

    let private = root.0.join("amux");
    crate::fs::create_private_dir(&private).unwrap();
    let (flags, aces) = parse(&dacl_sddl(&private).unwrap());
    assert!(flags.starts_with('P'), "protected: {flags}");
    assert_eq!(aces, owner_only("OICI"));

    let inside = private.join("device.key");
    fs::write(&inside, b"secret").unwrap();
    let (flags, aces) = parse(&dacl_sddl(&inside).unwrap());
    assert!(!flags.contains('P'), "inherits: {flags}");
    assert_eq!(aces, owner_only("ID"));

    let nested = private.join("agents");
    fs::create_dir(&nested).unwrap();
    let (_, aces) = parse(&dacl_sddl(&nested).unwrap());
    assert_eq!(aces, owner_only("OICIID"));
}

#[test]
fn making_an_existing_folder_private_upgrades_what_it_holds() {
    let root = OutsideProfile::new();
    let existing = root.0.join("old-root");
    fs::create_dir(&existing).unwrap();
    // A folder already there that anyone may use, which hands
    // that down, wherever the test program happens to be built.
    set_dacl(&existing, "D:P(A;OICI;FA;;;WD)");
    fs::create_dir(existing.join("profile")).unwrap();
    let account = existing.join("profile").join("account");
    fs::write(&account, b"x").unwrap();
    let (_, before) = parse(&dacl_sddl(&account).unwrap());
    assert_ne!(
        before,
        owner_only("ID"),
        "the test root must not be owner-only already"
    );

    crate::fs::create_private_dir(&existing).unwrap();
    let (flags, aces) = parse(&dacl_sddl(&existing).unwrap());
    assert!(flags.starts_with('P'), "{flags}");
    assert_eq!(aces, owner_only("OICI"));
    let (_, aces) = parse(&dacl_sddl(&account).unwrap());
    assert_eq!(aces, owner_only("ID"));
}

#[test]
fn a_durably_written_file_is_owner_only_wherever_it_is() {
    let root = OutsideProfile::new();
    let path = root.0.join("relay-token");
    write_durably(&path, b"token").unwrap();
    let (flags, aces) = parse(&dacl_sddl(&path).unwrap());
    assert!(flags.starts_with('P'), "{flags}");
    assert_eq!(aces, owner_only(""));
    crate::fs::make_private(&path).unwrap();
    assert_eq!(parse(&dacl_sddl(&path).unwrap()).1, owner_only(""));
}

#[test]
fn the_user_sid_is_a_sid() {
    let sid = current_user_sid().unwrap();
    assert!(sid.starts_with("S-1-"), "{sid}");
}

/// Opens `path` the way a scanner or `Get-Content` might: reading, sharing
/// reads and writes but not deletion.
fn hold_without_delete_sharing(path: &Path) -> File {
    let mut options = OpenOptions::new();
    options.read(true);
    options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    if path.is_dir() {
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    options.open(path).unwrap()
}

#[test]
fn a_durable_replace_against_a_holder_without_delete_sharing_retries_then_reports() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("generation");
    fs::write(&path, b"old").unwrap();

    let holder = hold_without_delete_sharing(&path);
    let started = Instant::now();
    let error = write_durably(&path, b"new").unwrap_err();
    assert!(is_held(&error), "{error}");
    assert!(
        started.elapsed() >= SHARING_PATIENCE,
        "{:?}",
        started.elapsed()
    );
    assert_eq!(fs::read(&path).unwrap(), b"old");
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        1,
        "the temporary file is gone"
    );
    drop(holder);

    let holder = hold_without_delete_sharing(&path);
    let started = Instant::now();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(holder);
        });
        write_durably(&path, b"new").unwrap();
    });
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert_eq!(fs::read(&path).unwrap(), b"new");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn a_durable_replace_goes_through_a_reader_that_shares_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, b"old").unwrap();
    let mut reader = File::open(&path).unwrap();
    let started = Instant::now();
    write_durably(&path, b"new").unwrap();
    assert!(
        started.elapsed() < SHARING_PATIENCE,
        "no waiting: POSIX semantics"
    );
    assert_eq!(fs::read(&path).unwrap(), b"new");
    let mut old = String::new();
    io::Read::read_to_string(&mut reader, &mut old).unwrap();
    assert_eq!(old, "old", "the reader keeps the file it opened");
}

/// The fallback is for other volumes: this one takes POSIX semantics, so a
/// replace here never falls back.
#[test]
fn this_volume_takes_a_posix_replace_through_the_handle() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    fs::write(&target, b"old").unwrap();
    let temp = dir.path().join("temp");
    let file = create_temp(&temp).unwrap();
    let flags = FILE_RENAME_FLAG_POSIX_SEMANTICS | FILE_RENAME_FLAG_REPLACE_IF_EXISTS;
    set_rename_info(&file, &wide(&target).unwrap(), FileRenameInfoEx, flags).unwrap();
    drop(file);
    assert!(!temp.exists());
    assert_eq!(fs::read(&target).unwrap(), b"");
}

#[test]
fn patient_removal_of_a_held_file_or_folder_is_later() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("agent.log");
    fs::write(&file, b"x").unwrap();
    let tree = dir.path().join("agent");
    fs::create_dir_all(tree.join("worktree")).unwrap();
    let held_inside = tree.join("worktree").join("held");
    fs::write(&held_inside, b"x").unwrap();
    let cwd = dir.path().join("shell-cwd");
    fs::create_dir(&cwd).unwrap();

    let patience = Duration::from_millis(300);
    for (path, held) in [(&file, &file), (&tree, &held_inside), (&cwd, &cwd)] {
        let holder = hold_without_delete_sharing(held);
        let started = Instant::now();
        assert_eq!(
            remove_patiently(path, patience).unwrap(),
            Removed::Later,
            "{}",
            path.display()
        );
        assert!(started.elapsed() >= patience);
        assert!(held.exists());
        drop(holder);
        assert_eq!(remove_patiently(path, patience).unwrap(), Removed::Gone);
        assert!(!path.exists());
    }
}

/// Sets `path`'s DACL to the one `sddl` gives, protected from what its
/// folder would hand down.
fn set_dacl(path: &Path, sddl: &str) {
    let sddl: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let (mut present, mut defaulted, mut dacl) = (0, 0, ptr::null_mut());
    let path = wide(path).unwrap();
    // SAFETY: `sddl` and `path` are NUL-terminated; the ACL points into
    // `descriptor`, which is freed after the call that reads it.
    unsafe {
        assert!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            ) != 0
        );
        assert!(
            GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) != 0
        );
        let status = SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            dacl,
            ptr::null(),
        );
        LocalFree(descriptor);
        assert_eq!(status, 0, "the DACL is set");
    }
}

/// An ACL that refuses a deletion is reported, not waited on as if
/// something held the path: a sweep would put it off for ever.
#[test]
fn patient_removal_reports_a_deletion_an_acl_refuses() {
    // A deletion asks with backup intent, which an enabled SeRestore grants
    // past any DACL: on GitHub's runner both deletions went through.
    let disabled = crate::testing::faults::hold_to_acls().unwrap();
    eprintln!("disabled to be held to the DACLs: {disabled:?}");
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("kept");
    let tree = folder.join("agent");
    fs::create_dir_all(&tree).unwrap();
    let file = folder.join("agent.log");
    fs::write(&file, b"x").unwrap();
    // A deletion needs DELETE on the path or FILE_DELETE_CHILD on its
    // folder: both are refused, to everyone.
    let (allowed, no_delete) = ("D:P(A;;FA;;;WD)", "D:P(D;;0x10000;;;WD)(A;;FA;;;WD)");
    set_dacl(&file, no_delete);
    set_dacl(&tree, no_delete);
    set_dacl(&folder, "D:P(D;;0x40;;;WD)(A;OICI;FA;;;WD)");
    let patience = Duration::from_millis(300);
    let removed: Vec<_> = [&file, &tree]
        .into_iter()
        .map(|path| {
            let started = Instant::now();
            let removed = remove_patiently(path, patience);
            (path, started.elapsed(), removed)
        })
        .collect();
    set_dacl(&folder, "D:P(A;OICI;FA;;;WD)");
    set_dacl(&file, allowed);
    set_dacl(&tree, allowed);
    for (path, took, removed) in removed {
        let error = removed.expect_err("a refused deletion is an error, not Later");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
        assert!(
            took < patience,
            "{} was not waited on: {took:?}",
            path.display()
        );
        assert!(path.exists());
        assert_eq!(remove_patiently(path, patience).unwrap(), Removed::Gone);
    }
}

/// `node`'s test from before the move, with the share now plain too, as
/// [`canonical`] makes it.
#[test]
fn a_drive_path_loses_its_verbatim_prefix_and_so_does_a_share() {
    assert_eq!(
        plain(PathBuf::from(r"\\?\C:\Users\me\shop")),
        PathBuf::from(r"C:\Users\me\shop")
    );
    assert_eq!(
        plain(PathBuf::from(r"\\?\UNC\server\share\shop")),
        PathBuf::from(r"\\server\share\shop")
    );
    let device = PathBuf::from(r"\\?\C:\x\nul");
    assert_eq!(plain(device.clone()), device);
}

#[test]
fn a_canonical_path_stays_verbatim_where_the_plain_form_means_another_file() {
    let dir = tempfile::tempdir().unwrap();
    let verbatim_dir = dir.path().canonicalize().unwrap();
    assert!(verbatim_dir.to_string_lossy().starts_with(r"\\?\"));
    for name in ["trail.", "trail ", "con"] {
        let odd = verbatim_dir.join(name);
        fs::create_dir(&odd).unwrap();
        let canonical = canonical(&odd).unwrap();
        assert!(
            canonical.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            canonical.display()
        );
        assert!(canonical.is_dir());
        fs::remove_dir(&odd).unwrap();
    }
}

#[test]
fn a_long_path_is_made_private_and_written_durably() {
    let dir = tempfile::tempdir().unwrap();
    let mut deep = dir.path().to_owned();
    while deep.as_os_str().len() < 300 {
        deep.push("a-folder-with-a-long-name");
    }
    crate::fs::create_private_dir(&deep).unwrap();
    let path = deep.join("file");
    write_durably(&path, b"deep").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"deep");
    crate::fs::sync_dir(&deep).unwrap();
}

/// A junction (which needs no privilege) goes as the link itself: its
/// target and what the target holds stay.
#[test]
fn patient_removal_of_a_junction_removes_the_link_alone() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("worktree");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("kept"), b"x").unwrap();
    let link = dir.path().join("link");
    let made = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&link)
        .arg(&target)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    assert_eq!(
        remove_patiently(&link, Duration::from_millis(300)).unwrap(),
        Removed::Gone
    );
    assert!(fs::symlink_metadata(&link).is_err(), "the junction is gone");
    assert!(target.join("kept").exists(), "its target is untouched");
}

/// A directory symlink likewise, where this account may make one (the
/// privilege, or developer mode); skipped, with the reason, where it may not.
#[test]
fn patient_removal_of_a_directory_symlink_removes_the_link_alone() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("worktree");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("kept"), b"x").unwrap();
    let link = dir.path().join("link");
    if let Err(error) = std::os::windows::fs::symlink_dir(&target, &link) {
        eprintln!("skipped: this account may not make a directory symlink: {error}");
        return;
    }
    assert_eq!(
        remove_patiently(&link, Duration::from_millis(300)).unwrap(),
        Removed::Gone
    );
    assert!(fs::symlink_metadata(&link).is_err(), "the link is gone");
    assert!(target.join("kept").exists(), "its target is untouched");
}

/// A failed write's temporary file is gone even when a scanner opens it,
/// without delete sharing, the moment the writer closes it: the writer
/// deletes it through its own handle first.
#[test]
fn a_failed_writes_temporary_file_survives_no_scanner() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("generation");
    fs::write(&path, b"old").unwrap();
    let holder = hold_without_delete_sharing(&path);

    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let scanner = scope.spawn(|| {
            let mut held = Vec::new();
            while !done.load(Ordering::Relaxed) {
                for entry in fs::read_dir(dir.path()).unwrap().flatten() {
                    if entry.file_name().to_string_lossy().ends_with(".tmp")
                        && let Ok(file) = OpenOptions::new()
                            .read(true)
                            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                            .open(entry.path())
                    {
                        held.push(file);
                    }
                }
            }
            // Holds what it caught a little past the writer's cleanup.
            std::thread::sleep(Duration::from_millis(200));
            held.len()
        });
        let error = write_durably(&path, b"new").unwrap_err();
        assert!(is_held(&error), "{error}");
        done.store(true, Ordering::Relaxed);
        let caught = scanner.join().unwrap();
        eprintln!("the scanner caught {caught} temporary files open");
    });
    drop(holder);
    let left: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, ["generation"], "no temporary file is left");
    assert!(crate::fs::left_stages().is_empty());
}

/// A secret created outside any private folder, in one whose ACL lets
/// everyone read what it holds, is owner-only from its creation, where
/// `private_open_options` inherits the broad ACL. An existing file opened
/// by `create_private` is made owner-only too.
#[test]
fn a_private_file_is_owner_only_in_a_folder_everyone_can_read() {
    use crate::fs::{Create, create_private};

    let root = OutsideProfile::new();
    let broad = root.0.join("shared");
    fs::create_dir(&broad).unwrap();
    let granted = std::process::Command::new("icacls")
        .arg(&broad)
        .args(["/grant", "*S-1-1-0:(OI)(CI)R"])
        .output()
        .unwrap();
    assert!(granted.status.success(), "{granted:?}");

    let inherited = broad.join("inherited");
    private_open_options()
        .create_new(true)
        .open(&inherited)
        .unwrap();
    let (_, aces) = parse(&dacl_sddl(&inherited).unwrap());
    assert!(
        aces.iter().any(|ace| ace.ends_with(";WD")),
        "the old options inherit everyone's read: {aces:?}"
    );

    for how in [Create::New, Create::Truncate, Create::Append] {
        let path = broad.join(format!("secret-{how:?}"));
        drop(create_private(&path, how).unwrap());
        let (flags, aces) = parse(&dacl_sddl(&path).unwrap());
        assert!(flags.starts_with('P'), "{how:?}: protected: {flags}");
        assert_eq!(aces, owner_only(""), "{how:?}");
    }

    for how in [Create::Truncate, Create::Append] {
        let path = broad.join(format!("existing-{how:?}"));
        fs::write(&path, b"x").unwrap();
        drop(create_private(&path, how).unwrap());
        let (flags, aces) = parse(&dacl_sddl(&path).unwrap());
        assert!(flags.starts_with('P'), "{how:?}: protected: {flags}");
        assert_eq!(aces, owner_only(""), "{how:?}");
    }
}
