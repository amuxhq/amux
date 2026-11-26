//! The login item on Windows: a value under the user's `Run` key,
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, which Windows
//! starts in the person's session at logon, with their environment, in
//! explorer's job (no kill-on-close, breakaway allowed), so in no job that
//! can end the agents.
//!
//! - **One value per root.** The default installation's is `amux`; any
//!   other root's is `amux-<8 hex of the root's hash>`, so a worktree's or
//!   a test's installation never touches the real one's, and two roots
//!   never overwrite each other.
//! - **It starts the launcher.** `"<bin>\amux-launch.exe" supervise
//!   [--config "<path>"]`: `amux-launch.exe` has no console, starts `amux
//!   supervise` with the same arguments and restarts it if it dies, as
//!   launchd's `KeepAlive` and systemd's `Restart=` do. A build with no
//!   launcher beside it names `amux.exe` instead, which runs the same
//!   supervisor in a console window and without the restart.
//! - **At most 260 characters.** Windows starts no longer `Run` command
//!   line, so a longer one is refused rather than written.
//!
//! Nothing is carried in the value but the command: the session's own
//! environment, PATH included, is the person's.

use std::path::{Path, PathBuf};
use std::{io, ptr};

use sha2::{Digest as _, Sha256};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};

use super::{StartAtLogin, TEST_KEY_PREFIX, TEST_RUN_KEY};

/// The key, under `HKCU`, whose values Windows starts at logon.
pub(super) const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// The default installation's value.
const DEFAULT_VALUE: &str = "amux";
/// The launcher shipped beside `amux.exe`.
pub(super) const LAUNCHER: &str = "amux-launch.exe";
/// The longest command line a `Run` value may hold.
pub(super) const MAX_COMMAND_LINE: usize = 260;

pub(super) fn items() -> io::Result<PathBuf> {
    let Some(key) = std::env::var_os(TEST_RUN_KEY) else {
        return Ok(PathBuf::from(RUN_KEY));
    };
    let key = key.to_string_lossy().into_owned();
    if !key.starts_with(TEST_KEY_PREFIX) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{TEST_RUN_KEY} must name a key under {TEST_KEY_PREFIX}…, not {key}"),
        ));
    }
    Ok(PathBuf::from(key))
}

pub(super) fn scratch_items(_home: &Path, key: &str) -> PathBuf {
    PathBuf::from(key)
}

/// The value is the registration, and a test's goes under its own key.
pub(super) fn registers() -> bool {
    true
}

/// Deletes a test's key and all it holds; any other key is left alone.
pub(super) fn forget_scratch(items: &Path) {
    let key = items.to_string_lossy();
    if key.starts_with(TEST_KEY_PREFIX) {
        let key = wide(&key);
        // SAFETY: `key` is NUL-terminated.
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr()) };
    }
}

/// The value this installation's item is: `amux` for the default root,
/// `amux-<8 hex>` of the root otherwise. Roots are compared as Windows
/// compares paths, whatever their case or separators.
pub(super) fn value_name(root: &Path, default_root: &Path) -> String {
    let key = |path: &Path| {
        path.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    let root = key(root);
    if root == key(default_root) {
        return DEFAULT_VALUE.to_owned();
    }
    let digest = Sha256::digest(root.as_bytes());
    let hex: String = digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{DEFAULT_VALUE}-{hex}")
}

fn name(login: &StartAtLogin) -> String {
    value_name(&login.root, &settings::InstallationConfig::default().root)
}

/// The command the value runs: the launcher beside `binary` when there is
/// one, else `binary`, then `supervise` and the config. Refused when
/// Windows would not start it.
pub(super) fn command_line(binary: &Path, config: Option<&Path>) -> io::Result<String> {
    let launcher = binary.with_file_name(LAUNCHER);
    let program = if launcher.is_file() {
        launcher
    } else {
        binary.to_owned()
    };
    let mut line = format!("\"{}\" supervise", program.display());
    if let Some(config) = config {
        line.push_str(&format!(" --config \"{}\"", config.display()));
    }
    let length = line.encode_utf16().count();
    if length > MAX_COMMAND_LINE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "the login item's command line would be {length} characters, and Windows \
                 starts at most {MAX_COMMAND_LINE} from the Run key: {line}. Move the config \
                 file or amux to a shorter path."
            ),
        ));
    }
    Ok(line)
}

pub(super) fn location(login: &StartAtLogin) -> String {
    format!(r"HKCU\{}, value {}", login.items.display(), name(login))
}

pub(super) fn contents(login: &StartAtLogin) -> Option<String> {
    read_value(&login.items.to_string_lossy(), &name(login))
        .ok()
        .flatten()
}

pub(super) fn is_on(login: &StartAtLogin) -> bool {
    contents(login).is_some()
}

pub(super) fn write(login: &StartAtLogin) -> io::Result<()> {
    let line = command_line(&login.binary, login.config.as_deref())?;
    set_value(&login.items.to_string_lossy(), &name(login), &line)
}

pub(super) fn remove(login: &StartAtLogin) -> io::Result<()> {
    delete_value(&login.items.to_string_lossy(), &name(login))
}

/// The value is all there is: uninstalling stops the supervisor itself,
/// which tells its launcher it stopped on purpose.
pub(super) fn remove_now(login: &StartAtLogin) -> io::Result<()> {
    remove(login)
}

/// The scheduled task builds before the `Run` value registered as the
/// login item: one per machine, whose definition each kept in the root of
/// the installation that registered it.
const EARLIER_TASK: &str = "amux";
const EARLIER_TASK_FILE: &str = "amux-task.xml";

/// Ends and deletes the scheduled task an earlier build registered for
/// this installation, which it marked by keeping the task's definition in
/// the root, and deletes the definition; says whether there was one.
/// Under a test's key the task is named after that key, so no test
/// reaches the person's own.
pub(super) fn remove_earlier(login: &StartAtLogin) -> io::Result<bool> {
    let file = login.root.join(EARLIER_TASK_FILE);
    if !file.is_file() {
        return Ok(false);
    }
    if login.register {
        let task = earlier_task(&login.items);
        for arguments in [&["/End", "/TN"][..], &["/Delete", "/F", "/TN"]] {
            // One already ended, or already deleted by hand, is fine.
            let _ = std::process::Command::new("schtasks")
                .args(arguments)
                .arg(&task)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    std::fs::remove_file(&file)?;
    Ok(true)
}

/// The earlier task's name: `amux`, or under a test's key the key's own
/// last part (`amux-test-…`).
fn earlier_task(items: &Path) -> String {
    if items == Path::new(RUN_KEY) {
        return EARLIER_TASK.to_owned();
    }
    items
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| items.to_string_lossy().replace('\\', "-"))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn check(status: u32) -> io::Result<()> {
    match status {
        0 => Ok(()),
        status => Err(io::Error::from_raw_os_error(status as i32)),
    }
}

/// The string value `name` under `HKCU\key`; `None` when either is
/// missing.
fn read_value(key: &str, name: &str) -> io::Result<Option<String>> {
    let key = wide(key);
    let name = wide(name);
    let mut size = 0u32;
    let mut buffer: Vec<u16> = Vec::new();
    loop {
        // SAFETY: both names are NUL-terminated; `buffer` holds `size`
        // bytes (none on the first, size-only call).
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                if buffer.is_empty() {
                    ptr::null_mut()
                } else {
                    buffer.as_mut_ptr().cast()
                },
                &mut size,
            )
        };
        match status {
            0 if !buffer.is_empty() || size == 0 => break,
            0 | ERROR_MORE_DATA => buffer = vec![0u16; (size as usize).div_ceil(2) + 1],
            ERROR_FILE_NOT_FOUND => return Ok(None),
            status => return check(status).map(|()| None),
        }
    }
    let length = (size as usize / 2).min(buffer.len());
    let mut text = &buffer[..length];
    while let [rest @ .., 0] = text {
        text = rest;
    }
    Ok(Some(String::from_utf16_lossy(text)))
}

/// Sets the string value `name` under `HKCU\key`, making the key when it
/// is missing (a test's; the `Run` key is always there).
fn set_value(key: &str, name: &str, data: &str) -> io::Result<()> {
    let key = wide(key);
    let mut handle: HKEY = ptr::null_mut();
    // SAFETY: `key` is NUL-terminated; `handle` is closed below.
    check(unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            ptr::null(),
            &mut handle,
            ptr::null_mut(),
        )
    })?;
    let name = wide(name);
    let data = wide(data);
    // SAFETY: `data` is `data.len() * 2` bytes, its NUL included.
    let status = unsafe {
        RegSetValueExW(
            handle,
            name.as_ptr(),
            0,
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    // SAFETY: opened above.
    unsafe { RegCloseKey(handle) };
    check(status)
}

/// Deletes the value `name` under `HKCU\key`; one already gone is fine.
fn delete_value(key: &str, name: &str) -> io::Result<()> {
    let key = wide(key);
    let name = wide(name);
    // SAFETY: both are NUL-terminated.
    match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) } {
        ERROR_FILE_NOT_FOUND => Ok(()),
        status => check(status),
    }
}

#[cfg(test)]
mod tests {
    use super::super::ScratchItems;
    use super::*;

    fn login(home: &Path, items: &Path, root: &str) -> StartAtLogin {
        StartAtLogin {
            binary: home.join("bin").join("amux.exe"),
            config: Some(home.join("config.toml")),
            path: String::new(),
            root: home.join(root),
            items: items.to_owned(),
            register: true,
        }
    }

    /// The default root's value is `amux`; every other root's is its own,
    /// whatever case or separators name it.
    #[test]
    fn each_root_has_a_value_of_its_own() {
        let default = Path::new(r"C:\Users\someone\AppData\Local\amux");
        assert_eq!(value_name(default, default), "amux");
        assert_eq!(
            value_name(Path::new("c:/users/someone/appdata/local/amux/"), default),
            "amux"
        );
        let worktree = value_name(Path::new(r"C:\src\amux\.wt\data"), default);
        assert!(
            worktree.len() == "amux-".len() + 8
                && worktree.starts_with("amux-")
                && worktree[5..].chars().all(|c| c.is_ascii_hexdigit()),
            "{worktree}"
        );
        assert_eq!(
            value_name(Path::new(r"c:\SRC\amux\.wt\data"), default),
            worktree
        );
        assert_ne!(
            value_name(Path::new(r"C:\src\amux\.other\data"), default),
            worktree
        );
    }

    /// The value starts the launcher when one is beside amux, and amux
    /// itself when none is, with `supervise` and the config.
    #[test]
    fn the_value_starts_the_launcher_beside_amux() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let binary = bin.join("amux.exe");
        std::fs::write(&binary, b"").unwrap();
        let config = home.path().join("my config.toml");
        assert_eq!(
            command_line(&binary, Some(&config)).unwrap(),
            format!(
                "\"{}\" supervise --config \"{}\"",
                binary.display(),
                config.display()
            )
        );
        std::fs::write(bin.join(LAUNCHER), b"").unwrap();
        assert_eq!(
            command_line(&binary, None).unwrap(),
            format!("\"{}\" supervise", bin.join(LAUNCHER).display())
        );
    }

    /// A command line Windows would not start is refused, saying why, and
    /// nothing is written.
    #[test]
    fn a_command_line_over_260_characters_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let scratch = ScratchItems::new(home.path());
        let mut login = login(home.path(), scratch.items(), "data");
        login.config = Some(home.path().join("c".repeat(MAX_COMMAND_LINE)));
        let error = login.set(true).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("at most 260"), "{error}");
        assert!(!login.is_on());

        // Exactly 260 is started.
        let fixed = format!("\"{}\" supervise --config \"\"", login.binary.display()).len();
        login.config = Some(PathBuf::from("c".repeat(MAX_COMMAND_LINE - fixed)));
        assert_eq!(
            command_line(&login.binary, login.config.as_deref())
                .unwrap()
                .len(),
            MAX_COMMAND_LINE
        );
    }

    /// Uninstalling removes the scheduled task an earlier build registered
    /// for this root, which it marked with the task's definition in the
    /// root, and only then: the real `amux` task is a test's never.
    #[test]
    fn the_earlier_task_goes_only_where_its_definition_marks_the_root() {
        let home = tempfile::tempdir().unwrap();
        let scratch = ScratchItems::new(home.path());
        let login = login(home.path(), scratch.items(), "data");
        std::fs::create_dir_all(&login.root).unwrap();
        assert!(!login.remove_earlier().unwrap(), "no definition, no task");

        std::fs::write(login.root.join(EARLIER_TASK_FILE), b"\xff\xfe").unwrap();
        assert!(login.remove_earlier().unwrap());
        assert!(!login.root.join(EARLIER_TASK_FILE).exists());
        assert!(!login.remove_earlier().unwrap());

        assert_eq!(earlier_task(Path::new(RUN_KEY)), "amux");
        let task = earlier_task(scratch.items());
        assert!(task.starts_with("amux-test-"), "{task}");
    }

    /// Two roots' values sit side by side under the key, each set, read
    /// and removed alone, and the real `Run` key never sees them.
    #[test]
    fn a_per_root_run_value_is_set_read_and_removed() {
        let home = tempfile::tempdir().unwrap();
        let scratch = ScratchItems::new(home.path());
        let items = scratch.items().to_string_lossy().into_owned();
        assert!(items.starts_with(TEST_KEY_PREFIX), "{items}");
        let one = login(home.path(), scratch.items(), "one");
        let two = login(home.path(), scratch.items(), "two");
        assert_ne!(name(&one), name(&two));

        one.set(true).unwrap();
        two.set(true).unwrap();
        let line = command_line(&one.binary, one.config.as_deref()).unwrap();
        assert_eq!(read_value(&items, &name(&one)).unwrap(), Some(line.clone()));
        assert_eq!(read_value(&items, &name(&two)).unwrap(), Some(line));
        assert!(one.location().ends_with(&format!("value {}", name(&one))));
        for login in [&one, &two] {
            assert_eq!(read_value(RUN_KEY, &name(login)).unwrap(), None);
        }

        one.set(false).unwrap();
        assert_eq!(read_value(&items, &name(&one)).unwrap(), None);
        assert!(!one.is_on() && two.is_on());
        assert!(two.remove_now().unwrap());
        assert!(!two.is_on());

        drop(scratch);
        assert!(!two.is_on(), "the scratch key is gone");
        assert_eq!(read_value(&items, &name(&two)).unwrap(), None);
    }
}
