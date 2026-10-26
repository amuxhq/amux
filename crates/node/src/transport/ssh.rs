//! SSH stdio transport helpers.

use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, ReadBuf};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

use super::{Result, TransportError};

fn ssh_program() -> std::ffi::OsString {
    #[cfg(debug_assertions)]
    if let Some(program) = std::env::var_os("AMUX_SSH") {
        return program;
    }
    "ssh".into()
}
const RELAY_ARGS: [&str; 1] = ["relay"];
const PAIR_RECV_ARGS: [&str; 1] = ["pair-recv"];

/// Where an install method puts `amux` when that place may not be on the
/// PATH of a command run over SSH. sshd hands the command to the remote
/// user's shell with no login or interactive startup, so the PATH there is
/// the system's default; an installer that adds its directory in a shell's
/// startup files (as amux's own does, in `.bashrc` or `.zshrc`) is not on it.
/// A new install method that puts amux somewhere off that default PATH adds
/// its directory here, in the same change.
const INSTALL_DIRS: [&str; 1] = ["$HOME/.amux/bin"];

/// How many bytes of the session's error stream are kept, from its end.
const STDERR_KEPT: usize = 2048;

/// How long a failed session's error stream is waited for once the caller
/// asks what it said: long enough for ssh to finish writing why it gave up.
const STDERR_PATIENCE: Duration = Duration::from_secs(1);

pub(crate) type SshPairRecvIo = SshChildIo;

pub(crate) struct SshChildIo {
    child: Child,
    stdout: ChildStdout,
    stdin: ChildStdin,
    stderr: Option<SshStderr>,
}

impl SshChildIo {
    /// What the session writes to its error stream, for saying why it ended:
    /// ssh's own refusals ("Permission denied (publickey)") and the far
    /// end's ("amux not found …") both arrive there. Taken once, before the
    /// stream is handed on.
    pub(crate) fn take_stderr(&mut self) -> Option<SshStderr> {
        self.stderr.take()
    }
}

/// The tail of an SSH session's error stream, drained as it is written so
/// the session never stalls on a full pipe.
pub(crate) struct SshStderr {
    kept: Arc<Mutex<Vec<u8>>>,
    reader: JoinHandle<()>,
}

impl SshStderr {
    fn drain(mut stderr: ChildStderr) -> Self {
        let kept = Arc::new(Mutex::new(Vec::new()));
        let sink = kept.clone();
        let reader = tokio::spawn(async move {
            let mut chunk = [0_u8; 512];
            while let Ok(read) = stderr.read(&mut chunk).await {
                if read == 0 {
                    break;
                }
                let mut kept = sink.lock().unwrap();
                kept.extend_from_slice(&chunk[..read]);
                let excess = kept.len().saturating_sub(STDERR_KEPT);
                kept.drain(..excess);
            }
        });
        Self { kept, reader }
    }

    /// What the session said on its error stream, as one line; empty when
    /// it said nothing. Waits briefly for a session that is still ending.
    pub(crate) async fn said(self) -> String {
        let _ = tokio::time::timeout(STDERR_PATIENCE, self.reader).await;
        let kept = self.kept.lock().unwrap();
        String::from_utf8_lossy(&kept)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// `failed`, with what the SSH session said appended when it said anything.
pub(crate) async fn with_said(failed: String, stderr: Option<SshStderr>) -> String {
    let said = match stderr {
        Some(stderr) => stderr.said().await,
        None => String::new(),
    };
    if said.is_empty() {
        failed
    } else {
        format!("{failed}; ssh: {said}")
    }
}

pub(crate) fn spawn_ssh_relay(target: &str, profile: crate::ProfileId) -> Result<SshChildIo> {
    let profile = profile.to_string();
    spawn_ssh_amux_command(
        &ssh_program(),
        target,
        &[RELAY_ARGS[0], "--profile", &profile],
        "SSH relay",
    )
}

pub(crate) fn spawn_ssh_pair_recv(target: &str) -> Result<SshPairRecvIo> {
    spawn_ssh_amux_command(&ssh_program(), target, &PAIR_RECV_ARGS, "SSH pair-recv")
}

fn spawn_ssh_amux_command(
    program: &std::ffi::OsStr,
    target: &str,
    amux_args: &[&str],
    label: &str,
) -> Result<SshChildIo> {
    if target.trim().is_empty() {
        return Err(TransportError::Config(format!(
            "{label} target must not be empty"
        )));
    }
    if target.starts_with('-') {
        return Err(TransportError::Config(format!(
            "{label} target must not begin with '-'"
        )));
    }
    let command = remote_amux_command(amux_args).ok_or_else(|| {
        TransportError::Config(format!("{label} has an argument a shell would read"))
    })?;

    let mut child = Command::new(program)
        .args(ssh_amux_args(target, &command))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| TransportError::Config(format!("{label} child stdin was not piped")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| TransportError::Config(format!("{label} child stdout was not piped")))?;
    let stderr = child.stderr.take().map(SshStderr::drain);

    Ok(SshChildIo {
        child,
        stdout,
        stdin,
        stderr,
    })
}

fn ssh_amux_args(target: &str, command: &str) -> Vec<String> {
    ["-T", "-o", "BatchMode=yes", "--", target, command]
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// The command the far end runs: `amux ARGS`, with amux found on the PATH or
/// in one of [`INSTALL_DIRS`], or a line saying where it was looked for.
///
/// It is handed to `sh`, not run as it stands, because the far user's own
/// shell may be any shell and only a POSIX one can be relied on to search
/// the extra directories. The script sits in single quotes and holds none,
/// so the far shell passes it to `sh` untouched; the arguments are plain
/// words, refused otherwise, so no shell reads anything into them. A far
/// host without `sh` (Windows) is not reachable over SSH.
fn remote_amux_command(amux_args: &[&str]) -> Option<String> {
    let plain = |arg: &&str| {
        !arg.is_empty()
            && arg
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    };
    if !amux_args.iter().all(plain) {
        return None;
    }
    let script = remote_amux_script();
    Some(format!("sh -c '{script}' amux {}", amux_args.join(" ")))
}

fn remote_amux_script() -> String {
    let path = INSTALL_DIRS.join(":");
    let shown = INSTALL_DIRS.map(|dir| dir.replace("$HOME", "~")).join(", ");
    format!(
        "command -v amux >/dev/null 2>&1 || PATH=\"$PATH:{path}\"; \
         command -v amux >/dev/null 2>&1 || {{ echo \"amux not found on PATH or in {shown}\" >&2; exit 127; }}; \
         exec amux \"$@\""
    )
}

impl Drop for SshChildIo {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl AsyncRead for SshChildIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for SshChildIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stdin).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_args_are_noninteractive_and_carry_one_command() {
        let command = remote_amux_command(&PAIR_RECV_ARGS).unwrap();
        assert_eq!(
            ssh_amux_args("workstation", &command),
            vec!["-T", "-o", "BatchMode=yes", "--", "workstation", &command]
        );
        assert!(command.starts_with("sh -c '"));
        assert!(command.ends_with("' amux pair-recv"));
    }

    #[test]
    fn the_script_survives_single_quotes_and_names_where_it_looks() {
        let script = remote_amux_script();
        assert!(!script.contains('\''), "{script}");
        // Spelled out because the far end's shell, not this code, reads it:
        // a change here wants checking over a real SSH login, with and
        // without amux in each directory.
        assert_eq!(
            script,
            "command -v amux >/dev/null 2>&1 || PATH=\"$PATH:$HOME/.amux/bin\"; \
             command -v amux >/dev/null 2>&1 || { echo \"amux not found on PATH or in \
             ~/.amux/bin\" >&2; exit 127; }; exec amux \"$@\""
        );
    }

    #[test]
    fn an_argument_a_shell_would_read_is_refused() {
        for arg in ["", "a b", "$(id)", "x;y", "'", "~"] {
            assert_eq!(remote_amux_command(&["relay", arg]), None, "{arg:?}");
        }
        let profile = uuid::Uuid::nil().to_string();
        assert!(remote_amux_command(&["relay", "--profile", &profile]).is_some());
    }

    /// A stand-in for ssh that drops its options and target and runs the
    /// command as sshd does: as one string, through a shell, with nothing
    /// but HOME and the system's default PATH.
    #[cfg(unix)]
    fn fake_ssh(dir: &std::path::Path, home: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let ssh = dir.join("ssh");
        std::fs::write(
            &ssh,
            format!(
                "#!/bin/sh\nwhile [ \"$1\" != -- ]; do shift; done; shift 2\n\
                 exec env -i HOME='{}' PATH=/usr/bin:/bin sh -c \"$*\"\n",
                home.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        ssh
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn amux_in_the_installers_directory_runs_though_it_is_off_the_path() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let bin = home.join(".amux/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("amux"), "#!/bin/sh\necho \"installed amux $*\"\n").unwrap();
        std::fs::set_permissions(bin.join("amux"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let ssh = fake_ssh(dir.path(), &home);

        let mut io =
            spawn_ssh_amux_command(ssh.as_os_str(), "far", &PAIR_RECV_ARGS, "test").unwrap();
        let mut out = String::new();
        io.read_to_string(&mut out).await.unwrap();
        assert_eq!(out, "installed amux pair-recv\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_far_end_without_amux_says_where_it_looked() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let ssh = fake_ssh(dir.path(), &home);

        let mut io =
            spawn_ssh_amux_command(ssh.as_os_str(), "far", &PAIR_RECV_ARGS, "test").unwrap();
        let stderr = io.take_stderr();
        let mut out = Vec::new();
        io.read_to_end(&mut out).await.unwrap();
        assert!(out.is_empty());
        assert_eq!(
            with_said("the exchange ended".to_owned(), stderr).await,
            "the exchange ended; ssh: amux not found on PATH or in ~/.amux/bin"
        );
    }
}
