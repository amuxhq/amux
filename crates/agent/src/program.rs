//! Finding the program a provider command names, however the provider was
//! installed: the one rule for the provider's launch, the catalogue and the
//! version probe.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The program to start for `command`, found as a shell finds it
/// ([`os::paths::find_program`]): on the `PATH` in the provider's
/// environment `env` when it sets one, else on the `PATH` a newly logged-on
/// process would see ([`os::paths::current_path`]), so a provider installed
/// after the daemon started is found. On Windows a bare name is found under
/// each of `PATHEXT`'s extensions, so npm's `.cmd` shim is too, which
/// starting the name alone never finds. The command as given when nothing
/// is found, so the start fails naming it.
pub fn find(command: &str, env: &HashMap<String, String>) -> PathBuf {
    let path = env
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| OsString::from(value))
        .unwrap_or_else(os::paths::current_path);
    os::paths::find_program(command.as_ref(), &path).unwrap_or_else(|| PathBuf::from(command))
}

/// Whether `program` is a batch file (`.cmd` or `.bat`, as npm's shims
/// are), which Windows runs through `cmd.exe`: its arguments then pass a
/// shell that can't carry a line break, and that cuts a command line
/// longer than 8191 characters.
pub fn through_cmd(program: &Path) -> bool {
    program
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    /// A shim named on the provider's own `PATH` is found by its bare
    /// name, under the extension this platform runs it by; a name found
    /// nowhere is left as it was given.
    #[test]
    fn a_shim_on_the_providers_path_is_found_by_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let file = if cfg!(windows) { "tool.cmd" } else { "tool" };
        let shim = dir.path().join(file);
        std::fs::write(&shim, "").unwrap();
        os::fs::make_executable(&shim).unwrap();
        let env = HashMap::from([("PATH".to_owned(), dir.path().display().to_string())]);
        let found = super::find("tool", &env);
        assert_eq!(
            found.file_name().unwrap().to_ascii_lowercase(),
            file,
            "{found:?}"
        );
        assert_eq!(super::through_cmd(&found), cfg!(windows));
        assert_eq!(super::find("no-such-tool", &env), Path::new("no-such-tool"));
    }
}
