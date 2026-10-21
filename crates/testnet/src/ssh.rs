//! The net's stand-in for SSH. A daemon reaches a peer over SSH by running
//! `ssh <target> amux relay --profile <id>` and carrying the link over its
//! stdio; in a story `ssh` is `stand-in/ssh`, which this module puts first
//! on the process's PATH. A target is a host's directory, holding a login
//! the net writes when it makes the host: the far host's own `amux` against
//! its own installation, so the relay joins the far daemon's link socket.
//! No real SSH runs, and no network is crossed beyond the two sockets.

use std::path::{Path, PathBuf};
use std::sync::Once;

/// The login a target's directory holds, which the stand-in runs.
const LOGIN: &str = "ssh-login";

/// Where the stand-in lives: beside this crate's sources.
fn stand_in_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("stand-in")
}

/// Puts the stand-in first on this process's PATH, once: daemons in the
/// net's process and the processes it starts all find it there.
pub(crate) fn install() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(stand_in_dir()).chain(std::env::split_paths(&path)),
        )
        .expect("the stand-in's directory joins the PATH");
        // SAFETY: called under a Once by the first net a process starts,
        // before any of its hosts run; nets started on other threads read
        // the environment only through std, which takes the same lock.
        unsafe { std::env::set_var("PATH", path) };
    });
}

/// Writes the login that makes `dir` a target: `amux` against `config`,
/// with whatever the far side was asked to run.
pub(crate) fn write_login(dir: &Path, amux: &Path, config: &Path) -> std::io::Result<()> {
    let login = dir.join(LOGIN);
    std::fs::write(
        &login,
        format!(
            "#!/bin/sh\nexec {} --config {} \"$@\"\n",
            quoted(amux),
            quoted(config)
        ),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&login, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// A path as one shell word.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}
