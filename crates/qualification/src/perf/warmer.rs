//! The reference state qualifying runs are measured in: one core kept busy
//! by a child process for the whole run. An otherwise idle Apple Silicon
//! machine drops its cores into a low-power state between bursts and
//! measures several times slower with wider spread.

use std::io::Read;
use std::process::{Child, ChildStdin, Command, Stdio};

use anyhow::{Context, Result};

/// The argument a measuring binary is re-run with to become the warmer.
pub const CLUSTER_WARMER_ARG: &str = "--cluster-warmer";

/// Keeps one core busy until stopped or dropped.
pub struct ClusterWarmer {
    child: Option<Child>,
    pipe: Option<ChildStdin>,
}

impl ClusterWarmer {
    /// Starts the current executable with [`CLUSTER_WARMER_ARG`]; its
    /// `main` must hand that argument to [`run_cluster_warmer`].
    pub fn start() -> Result<Self> {
        let mut child = Command::new(std::env::current_exe()?)
            .arg(CLUSTER_WARMER_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start performance cluster warmer")?;
        let pipe = child
            .stdin
            .take()
            .context("cluster warmer has no parent pipe")?;
        Ok(Self {
            child: Some(child),
            pipe: Some(pipe),
        })
    }

    pub fn stop(&mut self) -> Result<()> {
        drop(self.pipe.take());
        let status = self
            .child
            .take()
            .context("cluster warmer was already reaped")?
            .wait()
            .context("reap performance cluster warmer")?;
        if !status.success() {
            anyhow::bail!("performance cluster warmer exited with {status}");
        }
        Ok(())
    }
}

impl Drop for ClusterWarmer {
    fn drop(&mut self) {
        drop(self.pipe.take());
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

/// Spins one core until the parent's pipe closes.
pub fn run_cluster_warmer() -> Result<()> {
    let pipe_monitor = std::thread::spawn(|| {
        let mut stdin = std::io::stdin().lock();
        let mut byte = [0_u8; 1];
        loop {
            match stdin.read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });
    while !pipe_monitor.is_finished() {
        std::hint::spin_loop();
    }
    pipe_monitor
        .join()
        .map_err(|_| anyhow::anyhow!("cluster warmer pipe monitor panicked"))
}
