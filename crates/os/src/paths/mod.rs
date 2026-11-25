//! Where things are: the home folder, a program's file name, the `PATH` a
//! newly logged-on process would see, and the program a name runs.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
pub(crate) mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(test)]
mod tests;

/// The user's home folder: `HOME` on Unix; on Windows `USERPROFILE`, then
/// the profile folder the system keeps ([`std::env::home_dir`]). One rule
/// for every caller.
pub fn home_dir() -> Option<PathBuf> {
    std::env::home_dir()
}

/// The file name of the program `stem`: `amux.exe` on Windows, `amux`
/// elsewhere.
pub fn exe_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

/// `PATH` as a newly logged-on process would see it.
///
/// Windows: this process's `PATH`, then the entries of the system's and
/// the user's `Path` (`HKLM\SYSTEM\CurrentControlSet\Control\Session
/// Manager\Environment`, `HKCU\Environment`, in the order Windows builds a
/// logon environment, `%VARIABLES%` expanded) it lacks: a daemon started at
/// logon keeps the `PATH` from logon and would otherwise miss a tool
/// installed since. An entry is listed once, whatever its case or trailing
/// separator. Unix: this process's `PATH`.
pub fn current_path() -> OsString {
    imp::current_path()
}

/// The program `name` runs when looked up on `path` (a `PATH` value), as
/// an absolute path; `None` when there is none.
///
/// A name holding a separator is that path, not looked up. Otherwise each
/// `path` folder in turn. On Windows, a file whose extension `PATHEXT`
/// lists, in `PATHEXT`'s order (`.COM;.EXE;.BAT;.CMD` when unset): a name
/// with a listed extension is tried as it is first, then with each
/// extension added; a name without one never bare, as the extensionless
/// file npm puts beside its `.cmd` shim is a shell script Windows cannot
/// start. On Unix, a file with an execute bit.
pub fn find_program(name: &OsStr, path: &OsStr) -> Option<PathBuf> {
    imp::find_program(name, path)
}

/// The first of `candidates` that `runnable` accepts, made absolute: a
/// relative command or `PATH` entry was checked against this process's
/// working directory, while the program may start in another.
fn first_runnable(
    candidates: impl IntoIterator<Item = PathBuf>,
    runnable: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    candidates
        .into_iter()
        .find(|candidate| runnable(candidate))
        .and_then(|found| std::path::absolute(found).ok())
}

/// The folders of `path`, without empty entries, which name no folder.
fn folders(path: &OsStr) -> impl Iterator<Item = PathBuf> + '_ {
    std::env::split_paths(path).filter(|folder| !folder.as_os_str().is_empty())
}
