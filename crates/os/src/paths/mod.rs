//! Where things are: the home folder, a program's file name, the `PATH` a
//! newly logged-on process would see, and the program a name runs.

use std::collections::HashMap;
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
    imp::find_program(name, path, None)
}

/// Whether the environment variable `key` is the one called `name`:
/// Windows names a variable whatever its case (`Path` is `PATH`), Unix by
/// its exact spelling.
pub fn same_variable(key: &str, name: &str) -> bool {
    imp::same_variable(key, name)
}

/// Sets each of `vars` in `env`, as a program started with `env` would
/// see it: one set replaces the variable whatever the spelling `env` has
/// it under ([`same_variable`]; on Windows a program's `PATH` replaces the
/// host's `Path`), so `env` holds each variable once, and a lookup over it
/// ([`Lookup`]) reads the value the program starts with.
pub fn set_vars(
    env: &mut HashMap<String, String>,
    vars: impl IntoIterator<Item = (String, String)>,
) {
    for (key, value) in vars {
        env.retain(|have, _| !imp::same_variable(have, &key));
        env.insert(key, value);
    }
}

/// How a program started with the variables `env` on top of this
/// process's environment is found, and the `PATH` it starts with: one rule
/// for both, so the program found is the one that starts, and what it runs
/// in turn (npm's `claude.cmd` runs `node`) is found on the same `PATH`.
///
/// `PATH`: `env`'s when it sets one, else [`current_path`]. `PATHEXT`
/// (Windows): `env`'s when it sets one, else this process's. Windows names
/// a variable whatever its case (`Path` is `PATH`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lookup {
    path: OsString,
    pathext: Option<OsString>,
    /// Set when `env` sets no `PATH` and [`current_path`] is not this
    /// process's own (Windows, after something was installed since it
    /// started): the program is started with it.
    refreshed: Option<OsString>,
}

impl Lookup {
    pub fn new(env: &HashMap<String, String>) -> Lookup {
        let var = |name: &str| {
            env.iter()
                .find(|(key, _)| imp::same_variable(key, name))
                .map(|(_, value)| OsString::from(value))
        };
        let pathext = var("PATHEXT");
        if let Some(path) = var("PATH") {
            return Lookup {
                path,
                pathext,
                refreshed: None,
            };
        }
        let path = current_path();
        let refreshed =
            (std::env::var_os("PATH").unwrap_or_default() != path).then(|| path.clone());
        Lookup {
            path,
            pathext,
            refreshed,
        }
    }

    /// The program `name` runs, as [`find_program`] finds it on this
    /// lookup's `PATH`, under its `PATHEXT` on Windows.
    pub fn find(&self, name: &OsStr) -> Option<PathBuf> {
        imp::find_program(name, &self.path, self.pathext.as_deref())
    }

    /// The `PATH` programs are looked for on.
    pub fn path(&self) -> &OsStr {
        &self.path
    }

    /// The `PATH` to set on the program beyond `env`, when there is one:
    /// [`current_path`], where `env` sets none and it differs from this
    /// process's. Started without it, a program found on a folder added
    /// since this process started would inherit a `PATH` lacking that
    /// folder.
    pub fn refreshed_path(&self) -> Option<&OsStr> {
        self.refreshed.as_deref()
    }
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
