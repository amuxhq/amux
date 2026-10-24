//! SSH stdio transport helpers.

use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use super::{Result, TransportError};

fn ssh_program() -> std::ffi::OsString {
    #[cfg(debug_assertions)]
    if let Some(program) = std::env::var_os("AMUX_SSH") {
        return program;
    }
    "ssh".into()
}
const AMUX_RELAY_COMMAND: [&str; 2] = ["amux", "relay"];
const AMUX_PAIR_RECV_COMMAND: [&str; 2] = ["amux", "pair-recv"];

pub(crate) type SshPairRecvIo = SshChildIo;

pub(crate) struct SshChildIo {
    child: Child,
    stdout: ChildStdout,
    stdin: ChildStdin,
}

pub(crate) fn spawn_ssh_relay(target: &str, profile: crate::ProfileId) -> Result<SshChildIo> {
    let profile = profile.to_string();
    spawn_ssh_amux_command(
        target,
        &[
            AMUX_RELAY_COMMAND[0],
            AMUX_RELAY_COMMAND[1],
            "--profile",
            &profile,
        ],
        "SSH relay",
    )
}

pub(crate) fn spawn_ssh_pair_recv(target: &str) -> Result<SshPairRecvIo> {
    spawn_ssh_amux_command(target, &AMUX_PAIR_RECV_COMMAND, "SSH pair-recv")
}

fn spawn_ssh_amux_command(target: &str, amux_command: &[&str], label: &str) -> Result<SshChildIo> {
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

    let mut child = Command::new(ssh_program())
        .args(ssh_amux_args(target, amux_command))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| TransportError::Config(format!("{label} child stdin was not piped")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| TransportError::Config(format!("{label} child stdout was not piped")))?;

    Ok(SshChildIo {
        child,
        stdout,
        stdin,
    })
}

fn ssh_amux_args(target: &str, amux_command: &[&str]) -> Vec<String> {
    ["-T", "-o", "BatchMode=yes", "--", target]
        .into_iter()
        .chain(amux_command.iter().copied())
        .map(str::to_string)
        .collect()
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
    fn ssh_pair_recv_args_are_noninteractive_and_binary_safe() {
        assert_eq!(
            ssh_amux_args("workstation", &AMUX_PAIR_RECV_COMMAND),
            vec![
                "-T",
                "-o",
                "BatchMode=yes",
                "--",
                "workstation",
                "amux",
                "pair-recv"
            ]
        );
    }
}
