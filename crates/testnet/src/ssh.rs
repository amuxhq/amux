//! The net's stand-in for SSH. A daemon reaches a peer over SSH by running
//! `amux relay --profile <id>` on it through `ssh <target>` and carrying
//! the link over its stdio; in a story `ssh` is `stand-in/ssh`, a copy of
//! which this module puts first on the process's PATH. A target is a host's directory,
//! holding a login the net writes when it makes the host: an `amux` that is
//! the far host's own against its own installation, so the relay joins the
//! far daemon's link socket. No real SSH runs, and no network is crossed
//! beyond the two sockets.

use std::path::{Path, PathBuf};
use std::sync::Once;

/// The directory in a target that the stand-in puts first on the PATH of the
/// command it runs, holding the login as `amux`.
const LOGIN_DIR: &str = "ssh-path";

/// Where the stand-in lives: beside this crate's sources.
fn stand_in_dir() -> PathBuf {
    source_tree::crate_dir!().join("stand-in")
}

/// Puts the stand-in first on this process's PATH, once: daemons in the
/// net's process and the processes it starts all find it there.
///
/// What goes on the PATH is a copy, made executable, in a folder of this
/// process's own: the checkout's file need not carry an execute bit (one
/// uploaded from a Windows checkout to a Linux worker has none), and a
/// program search skips a file without one, so the system's own `ssh`
/// would answer instead. The folder outlives every net, as the PATH does.
///
/// A test target that does not carry this crate's files (another crate's
/// suite starting a net, which never dials SSH) has no stand-in to copy:
/// then nothing goes on the PATH, and a story there that dials SSH reaches
/// the system's own `ssh`, after the warning this prints.
pub(crate) fn install() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let stand_in = stand_in_dir().join("ssh");
        if !stand_in.is_file() {
            eprintln!(
                "testnet: no ssh stand-in at {} (this target does not carry testnet's \
                 stand-in folder), so a story dialling SSH here reaches the system's ssh",
                stand_in.display()
            );
            return;
        }
        let dir = std::env::temp_dir().join(format!("testnet-ssh-{}", std::process::id()));
        let copied = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::copy(&stand_in, dir.join("ssh")))
            .and_then(|_| os::fs::make_executable(&dir.join("ssh")));
        if let Err(error) = copied {
            panic!(
                "copying the ssh stand-in {} into {}: {error}",
                stand_in.display(),
                dir.display()
            );
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(std::iter::once(dir).chain(std::env::split_paths(&path)))
            .expect("the stand-in's directory joins the PATH");
        // SAFETY: called under a Once by the first net a process starts,
        // before any of its hosts run; nets started on other threads read
        // the environment only through std, which takes the same lock.
        unsafe { std::env::set_var("PATH", path) };
    });
}

/// Writes the login that makes `dir` a target: `amux` against `config`,
/// with whatever the far side was asked to run. The login is the
/// `fake-provider` binary running `amux --config <config>` with the
/// caller's arguments ([`provider_fakes::dispatch::Plan::Run`]).
pub(crate) fn write_login(dir: &Path, amux: &Path, config: &Path) -> std::io::Result<()> {
    let login = provider_fakes::dispatch::program(&dir.join(LOGIN_DIR), "amux");
    provider_fakes::dispatch::install(
        &crate::binaries::Binaries::built().fake_provider(),
        &login,
        &provider_fakes::dispatch::Plan::Run {
            program: amux.to_owned(),
            args: vec!["--config".to_owned(), config.to_string_lossy().into_owned()],
        },
    )
}
