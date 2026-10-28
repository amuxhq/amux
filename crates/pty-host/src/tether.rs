//! Ties a provider to the agent process that started it.

use std::io;
use std::process::Stdio;

use tokio::process::{Child, Command};

/// How long a tethered provider has to finish once asked, before its group
/// is killed.
pub const TETHER_GRACE_SECS: u32 = 5;

/// A provider leads a process group of its own, so nothing the agent's
/// death does reaches it: a server on a socket would serve on with no one
/// to serve, and headless Claude finishes its turn after its input closes,
/// running tools for no one. The tether is a shell in the provider's process
/// group, reading a pipe only the agent holds: when the agent dies, however
/// it dies, the pipe closes and the shell sends the group SIGTERM. A
/// provider still running `TETHER_GRACE_SECS` later (Codex with an approval
/// open ignores SIGTERM, and would hold its thread from every later client)
/// has its group killed. Being in the group, the shell keeps the group's id,
/// and so the provider's pid, from being reused while it waits.
pub struct Tether {
    shell: Child,
}

impl Tether {
    /// Tethers the process group `leader` leads, which must not have been
    /// reaped.
    pub fn new(leader: &Child) -> io::Result<Tether> {
        let group = leader
            .id()
            .ok_or_else(|| io::Error::other("the provider has already been reaped"))?;
        // The shell ignores the SIGTERM it sends its own group.
        let script = format!(
            r#"trap '' TERM; read _; kill -TERM "-$1" 2>/dev/null; i=0
while kill -0 "$1" 2>/dev/null; do
  if [ "$i" -ge {TETHER_GRACE_SECS} ]; then kill -KILL "-$1" 2>/dev/null; exit 0; fi
  sleep 1; i=$((i + 1))
done"#
        );
        let shell = Command::new("/bin/sh")
            .args(["-c", &script, "amux-tether"])
            .arg(group.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(group as i32)
            .spawn()?;
        Ok(Tether { shell })
    }

    /// Lets go once the provider has exited: whatever it left in its group
    /// is sent SIGTERM, and the shell is reaped.
    pub async fn release(mut self) {
        self.shell.stdin.take();
        let _ = self.shell.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::process::Command;

    use super::Tether;

    const WAIT: Duration = Duration::from_secs(10);

    /// Its agent dying, which is what closes the tether's pipe, asks the
    /// provider's whole group to finish.
    #[tokio::test]
    async fn a_tethered_provider_finishes_when_its_agent_dies() {
        let folder = tempfile::tempdir().unwrap();
        let started = folder.path().join("started");
        let mut command = Command::new("/bin/sh");
        // A child in the group that would outlive the provider alone.
        command.args(["-c", r#"sleep 30 & : > "$1"; wait"#, "sh"]);
        command.arg(&started).process_group(0);
        let mut provider = command.spawn().unwrap();
        let group = provider.id().unwrap() as i32;
        let tether = Tether::new(&provider).unwrap();
        // A signal sent while a process forks can miss the new child.
        tokio::time::timeout(WAIT, async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the provider's child starts");
        let Tether { mut shell } = tether;
        drop(shell.stdin.take());
        let status = tokio::time::timeout(WAIT, provider.wait())
            .await
            .expect("the provider is asked to finish")
            .unwrap();
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(15)
        );
        let _ = shell.wait().await;
        let gone = tokio::time::timeout(WAIT, async {
            // SAFETY: signal 0 only asks whether the group has a process.
            while unsafe { libc::killpg(group, 0) } == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(gone.is_ok(), "nothing of the group lives on");
    }
}
