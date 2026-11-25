//! The net's stand-in for SSH. A daemon reaches a peer over SSH by running
//! `amux relay --profile <id>` on it through `ssh <target>` and carrying
//! the link over its stdio; in a story `ssh` is `stand-in/ssh`, a copy of
//! which this module puts first on the process's PATH. A target is a host's directory,
//! holding a login the net writes when it makes the host: an `amux` that is
//! the far host's own against its own installation, so the relay joins the
//! far daemon's link socket. No real SSH runs, and no network is crossed
//! beyond the two sockets.

use std::path::Path;
use std::sync::OnceLock;

/// The directory in a target that the stand-in puts first on the PATH of the
/// command it runs, holding the login as `amux`.
const LOGIN_DIR: &str = "ssh-path";

/// The stand-in, `stand-in/ssh` beside this crate's sources, compiled in:
/// under Buck a test of another crate that starts a net runs with only its
/// own crate's files beside it, not this one's.
const STAND_IN: &str = include_str!("../stand-in/ssh");

/// Puts the stand-in first on this process's PATH, once: daemons in the
/// net's process and the processes it starts all find it there.
///
/// What goes on the PATH is a copy, made executable, in a folder of this
/// process's own: the checkout's file need not carry an execute bit (one
/// uploaded from a Windows checkout to a Linux worker has none), and a
/// program search skips a file without one, so the system's own `ssh`
/// would answer instead.
///
/// The folder is made afresh under a random name, which fails rather than
/// takes a folder already there, and is owner-only: in a temp directory
/// other users share, a folder at a name they could guess would put their
/// programs first on this process's PATH. It is held in a static, so it
/// lives as long as the PATH names it: the process's life.
pub(crate) fn install() {
    static INSTALL: OnceLock<tempfile::TempDir> = OnceLock::new();
    INSTALL.get_or_init(|| {
        let made = tempfile::Builder::new().prefix("testnet-ssh-").tempdir();
        let dir = match made {
            Ok(dir) => dir,
            Err(error) => panic!("making a folder for the ssh stand-in: {error}"),
        };
        let ssh = dir.path().join("ssh");
        let copied = os::fs::make_private(dir.path())
            .and_then(|()| std::fs::write(&ssh, STAND_IN))
            .and_then(|()| os::fs::make_executable(&ssh));
        if let Err(error) = copied {
            panic!(
                "copying the ssh stand-in into {}: {error}",
                dir.path().display()
            );
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(dir.path().to_owned()).chain(std::env::split_paths(&path)),
        )
        .expect("the stand-in's directory joins the PATH");
        // SAFETY: called once, by the first net a process starts, before
        // any of its hosts run; nets started on other threads wait on the
        // OnceLock, and read the environment only through std, which takes
        // the same lock.
        unsafe { std::env::set_var("PATH", path) };
        dir
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The folder the stand-in goes on the PATH from is one this process
    /// made, never one that was there: a folder at a name another user
    /// could guess and make first would put their programs ahead of every
    /// other on the PATH.
    #[test]
    fn the_stand_in_goes_on_the_path_from_a_folder_made_fresh() {
        let guessed = std::env::temp_dir().join(format!("testnet-ssh-{}", std::process::id()));
        std::fs::create_dir_all(&guessed).unwrap();
        std::fs::write(guessed.join("planted"), b"").unwrap();
        install();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let first = std::env::split_paths(&path).next().expect("a PATH");
        let planted = first.join("planted").exists();
        let _ = std::fs::remove_dir_all(&guessed);
        assert_ne!(
            first, guessed,
            "the stand-in went into a folder that was there"
        );
        assert!(!planted, "the PATH leads with a folder someone else filled");
        assert_eq!(
            std::fs::read_to_string(first.join("ssh")).unwrap(),
            STAND_IN
        );
    }
}
