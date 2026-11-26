//! Shared instrumentation for performance workloads.

use std::io;

pub mod flood;
pub mod phone;
pub mod relay;
mod report;
pub mod warmer;
pub use report::{
    Baselines, DESKTOP_REFERENCE_STATE, Machine, Metric, MetricRun, PerfError, Report, Sample,
    Statistic, Unit, Verdict, Workload,
};

/// The platform's process-residency measure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryMetric {
    /// Darwin's ledger-backed physical footprint from `TASK_VM_INFO`.
    PhysicalFootprint,
    /// Linux's resident set size from `/proc/<pid>/statm`.
    Rss,
    /// Windows' private bytes (`PrivateUsage` from `GetProcessMemoryInfo`):
    /// the memory committed to the process alone. The working set is the
    /// closer cousin of RSS, but Windows trims it under pressure and when a
    /// process idles, so it would drift with the machine rather than with
    /// the process; private bytes grow only with what the process keeps.
    PrivateBytes,
}

impl MemoryMetric {
    /// The exact name performance reports use. The platform distinction is
    /// important: physical footprint and RSS are not interchangeable values.
    pub const fn name(self) -> &'static str {
        match self {
            Self::PhysicalFootprint => "physical footprint",
            Self::Rss => "RSS",
            Self::PrivateBytes => "private bytes",
        }
    }
}

/// One process-memory observation in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemorySample {
    pub bytes: u64,
    pub metric: MemoryMetric,
}

impl MemorySample {
    pub const fn name(self) -> &'static str {
        self.metric.name()
    }
}

/// Sample a process using the platform-native residency measure promised by
/// the performance report.
pub fn sample_memory(pid: u32) -> io::Result<MemorySample> {
    crate::platform::memory(pid)
}

#[cfg(test)]
mod tests {
    #[test]
    fn sampler_names_and_reads_the_current_process_measure() {
        let sample = super::sample_memory(std::process::id()).expect("sample this process");
        assert!(sample.bytes > 0);
        let expected = match std::env::consts::OS {
            "macos" => "physical footprint",
            "windows" => "private bytes",
            _ => "RSS",
        };
        assert_eq!(sample.name(), expected);
    }

    #[test]
    fn sampler_reads_a_child_process() {
        // Use another copy of this test binary so the sampled process has a
        // deterministic lifetime on every supported host.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "perf::tests::sampler_child_target", "--ignored"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start sample target");
        let result = super::sample_memory(child.id());
        let _ = child.kill();
        let _ = child.wait();
        let sample = result.expect("sample child process");
        assert!(sample.bytes > 0);
    }

    #[test]
    #[ignore = "spawned only by sampler_reads_a_child_process"]
    fn sampler_child_target() {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
}
