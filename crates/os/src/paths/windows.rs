//! `os::paths` on Windows: the registry's `Path`, and `PATHEXT`.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::Foundation::ERROR_MORE_DATA;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_EXPAND_SZ, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ,
    RRF_RT_REG_SZ, RegGetValueW,
};

/// Where Windows keeps the system's environment.
const SYSTEM_ENVIRONMENT: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
/// Where Windows keeps the user's environment.
const USER_ENVIRONMENT: &str = "Environment";

/// What Windows tries when `PATHEXT` is unset.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

pub(super) fn current_path() -> OsString {
    merged_path(
        std::env::var_os("PATH").unwrap_or_default(),
        &[
            (HKEY_LOCAL_MACHINE, SYSTEM_ENVIRONMENT),
            (HKEY_CURRENT_USER, USER_ENVIRONMENT),
        ],
    )
}

/// `process`, as it is, followed by each entry of the `Path` value under
/// each of `sources` (a root key and a subkey) that it lacks.
fn merged_path(process: OsString, sources: &[(HKEY, &str)]) -> OsString {
    let mut seen: HashSet<String> = std::env::split_paths(&process)
        .map(|entry| entry_key(&entry))
        .collect();
    seen.remove("");
    let mut merged = process;
    for &(root, subkey) in sources {
        let Some(value) = registry_path(root, subkey) else {
            continue;
        };
        for entry in std::env::split_paths(&value) {
            let key = entry_key(&entry);
            if key.is_empty()
                || entry
                    .as_os_str()
                    .encode_wide()
                    .any(|c| c == u16::from(b'"'))
            {
                continue;
            }
            if !seen.insert(key) {
                continue;
            }
            if !merged.is_empty() {
                merged.push(";");
            }
            if entry
                .as_os_str()
                .encode_wide()
                .any(|c| c == u16::from(b';'))
            {
                merged.push("\"");
                merged.push(entry.as_os_str());
                merged.push("\"");
            } else {
                merged.push(entry.as_os_str());
            }
        }
    }
    merged
}

/// What makes two `PATH` entries the same folder: case and a trailing
/// separator aside.
fn entry_key(entry: &Path) -> String {
    entry
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_lowercase()
}

/// The `Path` value under `root\subkey`, its `%VARIABLES%` expanded when
/// it is a `REG_EXPAND_SZ`; `None` when there is none.
fn registry_path(root: HKEY, subkey: &str) -> Option<OsString> {
    let subkey: Vec<u16> = subkey.encode_utf16().chain([0]).collect();
    let value: Vec<u16> = "Path".encode_utf16().chain([0]).collect();
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    let mut kind = 0u32;
    let mut size = 0u32;
    let mut buffer: Vec<u16> = Vec::new();
    loop {
        // SAFETY: both names are NUL-terminated; `buffer` holds `size`
        // bytes (none on the first, size-only call).
        let status = unsafe {
            RegGetValueW(
                root,
                subkey.as_ptr(),
                value.as_ptr(),
                flags,
                &mut kind,
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
            _ => return None,
        }
    }
    let length = (size as usize / 2).min(buffer.len());
    let mut text = &buffer[..length];
    while let [rest @ .., 0] = text {
        text = rest;
    }
    let text = OsString::from_wide(text);
    Some(if kind == REG_EXPAND_SZ {
        expand(&text.to_string_lossy()).into()
    } else {
        text
    })
}

/// `text` with each `%NAME%` replaced by that variable of this process, as
/// `ExpandEnvironmentStrings` does: a name that isn't set is left as it is.
fn expand(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match (!name.is_empty()).then(|| std::env::var_os(name)).flatten() {
                    Some(value) => {
                        out.push_str(&value.to_string_lossy());
                        rest = &after[end + 1..];
                    }
                    None => {
                        // Left as written; the closing `%` may open the next.
                        out.push('%');
                        out.push_str(name);
                        rest = &after[end..];
                    }
                }
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

// Programs -------------------------------------------------------------------------

/// Windows names a variable whatever its case.
pub(super) fn same_variable(key: &str, name: &str) -> bool {
    key.eq_ignore_ascii_case(name)
}

/// Under `pathext` when given, else this process's `PATHEXT`.
pub(super) fn find_program(name: &OsStr, path: &OsStr, pathext: Option<&OsStr>) -> Option<PathBuf> {
    match pathext {
        Some(pathext) => find_program_with(name, path, pathext),
        None => find_program_with(name, path, &self::pathext()),
    }
}

fn pathext() -> OsString {
    std::env::var_os("PATHEXT").unwrap_or_else(|| DEFAULT_PATHEXT.into())
}

fn find_program_with(name: &OsStr, path: &OsStr, pathext: &OsStr) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    let extensions = extensions(pathext);
    let runnable = |candidate: &Path| std::fs::metadata(candidate).is_ok_and(|meta| meta.is_file());
    let wide: Vec<u16> = name.encode_wide().collect();
    let separated = wide
        .iter()
        .any(|&c| c == u16::from(b'/') || c == u16::from(b'\\'))
        || wide.get(1) == Some(&u16::from(b':'));
    if separated {
        return super::first_runnable(candidates(Path::new(name), &extensions), runnable);
    }
    super::first_runnable(
        super::folders(path).flat_map(|folder| candidates(&folder.join(name), &extensions)),
        runnable,
    )
}

/// The extensions `pathext` lists, in its order.
fn extensions(pathext: &OsStr) -> Vec<String> {
    pathext
        .to_string_lossy()
        .split(';')
        .map(str::trim)
        .filter(|extension| extension.starts_with('.') && extension.len() > 1)
        .map(str::to_owned)
        .collect()
}

/// Whether `extensions` lists the extension of `path`.
fn listed(path: &Path, extensions: &[String]) -> bool {
    path.extension().and_then(OsStr::to_str).is_some_and(|own| {
        extensions
            .iter()
            .any(|listed| listed[1..].eq_ignore_ascii_case(own))
    })
}

/// Whether Windows runs `path` by its extension, as `PATHEXT` lists.
pub(crate) fn runs_by_extension(path: &Path) -> bool {
    listed(path, &extensions(&pathext()))
}

/// The files `path` may be: itself when its extension is listed, then
/// itself with each listed extension added.
fn candidates(path: &Path, extensions: &[String]) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if listed(path, extensions) {
        candidates.push(path.to_owned());
    }
    for extension in extensions {
        // Lower case first, as files are named, so the path a program is
        // started by and shown as is the file's own spelling on a file
        // system that matches names whatever their case; then as listed,
        // for one that doesn't.
        let lower = extension.to_ascii_lowercase();
        let mut name = path.as_os_str().to_owned();
        name.push(&lower);
        candidates.push(PathBuf::from(name));
        if lower != *extension {
            let mut name = path.as_os_str().to_owned();
            name.push(extension);
            candidates.push(PathBuf::from(name));
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use std::fs;

    use windows_sys::Win32::System::Registry::{
        KEY_ALL_ACCESS, KEY_READ, REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE, RegCloseKey,
        RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegSetValueExW,
    };

    use super::*;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    /// A scratch key under `HKCU\Software`, deleted with all it holds when
    /// dropped, a failed test's included.
    struct ScratchKey {
        name: String,
    }

    impl ScratchKey {
        fn new() -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self {
                name: format!(r"Software\amux-test-{}-{nanos}", std::process::id()),
            }
        }

        fn subkey(&self, child: &str) -> String {
            format!(r"{}\{child}", self.name)
        }

        fn set_path(&self, child: &str, kind: REG_VALUE_TYPE, text: &str) {
            let subkey = wide(&self.subkey(child));
            let mut key: HKEY = ptr::null_mut();
            // SAFETY: `subkey` is NUL-terminated; `key` is closed below.
            let status = unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    subkey.as_ptr(),
                    0,
                    ptr::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_ALL_ACCESS,
                    ptr::null(),
                    &mut key,
                    ptr::null_mut(),
                )
            };
            assert_eq!(status, 0, "creating the scratch key");
            let data = wide(text);
            let name = wide("Path");
            // SAFETY: `data` is `data.len() * 2` bytes, NUL included.
            let status = unsafe {
                RegSetValueExW(
                    key,
                    name.as_ptr(),
                    0,
                    kind,
                    data.as_ptr().cast(),
                    (data.len() * 2) as u32,
                )
            };
            // SAFETY: opened above.
            unsafe { RegCloseKey(key) };
            assert_eq!(status, 0, "setting the scratch Path");
        }

        fn exists(&self) -> bool {
            let subkey = wide(&self.name);
            let mut key: HKEY = ptr::null_mut();
            // SAFETY: `subkey` is NUL-terminated; `key` is closed below.
            let status =
                unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut key) };
            if status == 0 {
                // SAFETY: opened above.
                unsafe { RegCloseKey(key) };
            }
            status == 0
        }

        fn delete(&self) {
            let subkey = wide(&self.name);
            // SAFETY: `subkey` is NUL-terminated.
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, subkey.as_ptr()) };
        }
    }

    impl Drop for ScratchKey {
        fn drop(&mut self) {
            self.delete();
        }
    }

    #[test]
    fn the_merged_path_sees_a_registry_path_change_after_the_process_entries() {
        let key = ScratchKey::new();
        let system = key.subkey("system");
        let user = key.subkey("user");
        let sources = [
            (HKEY_CURRENT_USER, system.as_str()),
            (HKEY_CURRENT_USER, user.as_str()),
        ];
        let process = OsString::from(r"C:\proc-one;C:\Shared\");

        assert_eq!(
            merged_path(process.clone(), &sources),
            process,
            "no key yet"
        );

        let root = std::env::var("SystemRoot").unwrap();
        key.set_path(
            "system",
            REG_EXPAND_SZ,
            r"%SystemRoot%\amux-sys;c:\shared;;%AMUX_NO_SUCH_VARIABLE%\x",
        );
        key.set_path("user", REG_SZ, r"C:\amux-user;C:\PROC-ONE\");
        assert_eq!(
            merged_path(process.clone(), &sources),
            OsString::from(format!(
                r"C:\proc-one;C:\Shared\;{root}\amux-sys;%AMUX_NO_SUCH_VARIABLE%\x;C:\amux-user"
            ))
        );

        key.set_path("user", REG_SZ, r"C:\amux-user;C:\amux-installed-since");
        let changed = merged_path(process, &sources);
        assert!(
            changed
                .to_string_lossy()
                .ends_with(r";C:\amux-user;C:\amux-installed-since"),
            "{changed:?}"
        );

        key.delete();
        assert!(!key.exists(), "the scratch key is gone");
    }

    #[test]
    fn the_real_current_path_starts_with_the_process_path() {
        let process = std::env::var_os("PATH").unwrap_or_default();
        let current = current_path();
        assert!(
            current
                .to_string_lossy()
                .starts_with(&*process.to_string_lossy()),
            "{current:?}"
        );
    }

    #[test]
    fn expanding_replaces_set_variables_and_leaves_the_rest() {
        let root = std::env::var("SystemRoot").unwrap();
        assert_eq!(expand(r"%SystemRoot%\x"), format!(r"{root}\x"));
        assert_eq!(expand(r"%systemroot%\x"), format!(r"{root}\x"));
        assert_eq!(expand("100%"), "100%");
        assert_eq!(expand("%%"), "%%");
        assert_eq!(
            expand(r"%AMUX_NO_SUCH%\%SystemRoot%"),
            format!(r"%AMUX_NO_SUCH%\{root}")
        );
    }

    fn touch(path: &Path) {
        fs::write(path, b"").unwrap();
    }

    #[test]
    fn a_program_is_found_in_path_order_then_pathext_order() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        touch(&first.join("tool.bat"));
        touch(&first.join("tool.cmd"));
        touch(&first.join("tool"));
        touch(&second.join("tool.exe"));
        let path = std::env::join_paths([&first, &second]).unwrap();
        let found =
            |pathext: &str| find_program_with(OsStr::new("tool"), &path, OsStr::new(pathext));

        assert_eq!(
            found(".EXE;.CMD;.BAT"),
            Some(first.join("tool.cmd")),
            "PATH first"
        );
        assert_eq!(found(".EXE;.BAT;.CMD"), Some(first.join("tool.bat")));
        assert_eq!(found(".EXE"), Some(second.join("tool.exe")));
        assert_eq!(found(".COM"), None, "the extensionless file is never taken");
        assert_eq!(
            found(DEFAULT_PATHEXT),
            Some(first.join("tool.bat")),
            "the default lists .BAT before .CMD"
        );
    }

    #[test]
    fn a_name_with_an_extension_or_a_folder_is_taken_as_it_is_first() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("tool.cmd"));
        touch(&dir.path().join("tool.cmd.exe"));
        touch(&dir.path().join("tool.txt"));
        let path = dir.path().as_os_str();
        let pathext = OsStr::new(".EXE;.CMD");
        assert_eq!(
            find_program_with(OsStr::new("tool.cmd"), path, pathext),
            Some(dir.path().join("tool.cmd"))
        );
        assert_eq!(
            find_program_with(OsStr::new("tool.txt"), path, pathext),
            None,
            "an unlisted extension does not run"
        );
        let by_path = dir.path().join("tool");
        assert_eq!(
            find_program_with(by_path.as_os_str(), OsStr::new(""), pathext),
            Some(dir.path().join("tool.cmd"))
        );
        assert_eq!(find_program_with(OsStr::new(""), path, pathext), None);
    }
}
