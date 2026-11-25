//! The tether, moved here from pty-host: a shell in the group that
//! SIGTERMs it when its pipe from us closes (see [`crate::process::Tether`]).

use std::io;
use std::process::Stdio;

use tokio::process::{Child, Command};

use crate::Pid;
use crate::process::TETHER_GRACE_SECS;

/// `others`: whether a process other than the shell itself, and not a
/// zombie, is still in group `$g`. Linux and Android read `/proc` with
/// builtins alone (`others_proc`), so the shell starts nothing in the
/// group to look. Elsewhere `others_ps` asks `ps`, through a shell that
/// names its own pid, and leaves out what the census itself started in
/// the group: that shell, which becomes `ps`, and any child of the tether
/// shell's (a shell that forks a subshell for `$(...)` waits in one, as
/// macOS's bash does). The tether shell's children are only ever its own
/// commands; the group's members are the provider's. Where neither can be
/// read, nothing is found and the leader alone is watched.
const OTHERS: &str = r#"g=$1
others_proc() {
  for s in /proc/[0-9]*/stat; do
    [ "$s" = "/proc/$$/stat" ] && continue
    read -r l < "$s" 2>/dev/null || continue
    set -- ${l##*)}
    [ "$1" != Z ] && [ "$3" = "$g" ] && return 0
  done
  return 1
}
others_ps() {
  set -- $(sh -c 'echo $$; exec ps -A -o pgid= -o pid= -o ppid=' 2>/dev/null)
  [ $# -ge 1 ] || return 1
  n=$1; shift
  while [ $# -ge 3 ]; do
    [ "$1" = "$g" ] && [ "$2" != "$$" ] && [ "$2" != "$n" ] && [ "$3" != "$$" ] && return 0
    shift 3
  done
  return 1
}
others() {
  if [ -r /proc/self/stat ]; then others_proc; else others_ps; fi
}
"#;

/// Waits for the pipe to close, SIGTERMs the group, and SIGKILLs it if
/// anything of it outlives the grace.
fn watch() -> String {
    format!(
        r#"trap '' TERM; read _; kill -TERM "-$g" 2>/dev/null; i=0
while kill -0 "$g" 2>/dev/null || others; do
  if [ "$i" -ge {TETHER_GRACE_SECS} ]; then kill -KILL "-$g" 2>/dev/null; exit 0; fi
  sleep 1; i=$((i + 1))
done"#
    )
}

#[derive(Debug)]
pub(crate) struct Tether {
    pub(super) shell: Child,
}

impl Tether {
    pub(crate) fn new(leader: &Child) -> io::Result<Tether> {
        let group = leader
            .id()
            .ok_or_else(|| io::Error::other("the provider has already been reaped"))?;
        // The shell ignores the SIGTERM it sends its own group, and keeps
        // watching the group, not just its leader, until nothing but itself
        // is left in it or the grace is up: a member that ignored the
        // SIGTERM and outlived the leader still gets the SIGKILL.
        let script = format!("{OTHERS}{}", watch());
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

    pub(crate) fn pid(&self) -> Option<Pid> {
        self.shell.id()
    }

    pub(crate) async fn release(mut self) {
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

    /// The `ps` census, from a shell alone in a group of its own, leaves
    /// out what the census itself starts there: here a subshell of the
    /// shell's, standing for the one macOS's bash forks for `$(...)`,
    /// which counted would make the tether wait out the grace for nothing.
    #[test]
    fn the_ps_census_leaves_out_its_own_shells() {
        use std::os::unix::process::CommandExt as _;

        let script = format!(
            r#"{}g=$$
( others_ps ) && echo found || echo none"#,
            super::OTHERS.replacen("g=$1\n", "", 1)
        );
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &script])
            .process_group(0)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "none");
    }

    /// A provider that exits leaving nothing in its group is let go of at
    /// once, not after the grace.
    #[tokio::test]
    async fn releasing_an_empty_group_is_prompt() {
        let mut provider = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .unwrap();
        let tether = Tether::new(&provider).unwrap();
        provider.wait().await.unwrap();
        let grace = Duration::from_secs(u64::from(crate::process::TETHER_GRACE_SECS));
        let started = std::time::Instant::now();
        tokio::time::timeout(grace, tether.release())
            .await
            .expect("released before the grace is up");
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    }

    /// Whether a process of `group` still runs (a zombie doesn't).
    fn live_in_group(group: i32) -> bool {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            // SAFETY: signal 0 only asks whether the group has a process.
            return unsafe { libc::killpg(group, 0) } == 0;
        };
        entries.filter_map(Result::ok).any(|entry| {
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                return false;
            };
            let Some(rest) = stat.rfind(')').map(|at| &stat[at + 1..]) else {
                return false;
            };
            let fields: Vec<&str> = rest.split_whitespace().collect();
            fields.len() > 2 && fields[0] != "Z" && fields[2] == group.to_string()
        })
    }

    /// A member that ignores SIGTERM and outlives the leader is still
    /// SIGKILLed once the grace is up: the tether watches the group, not
    /// just its leader.
    #[tokio::test]
    async fn a_member_that_ignores_sigterm_is_killed_after_the_grace() {
        let folder = tempfile::tempdir().unwrap();
        let started = folder.path().join("started");
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            r#"(trap '' TERM; : > "$1"; while :; do sleep 1; done) & wait"#,
            "sh",
        ]);
        command.arg(&started).process_group(0);
        let mut provider = command.spawn().unwrap();
        let group = provider.id().unwrap() as i32;
        let tether = Tether::new(&provider).unwrap();
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
        assert!(live_in_group(group), "the member ignored the SIGTERM");
        let grace = Duration::from_secs(u64::from(crate::process::TETHER_GRACE_SECS) + 5);
        tokio::time::timeout(grace, shell.wait())
            .await
            .expect("the tether ends once the grace is up")
            .unwrap();
        let gone = tokio::time::timeout(WAIT, async {
            while live_in_group(group) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(gone.is_ok(), "the member is killed after the grace");
    }
}
