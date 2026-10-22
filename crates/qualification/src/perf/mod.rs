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
}

impl MemoryMetric {
    /// The exact name performance reports use. The platform distinction is
    /// important: physical footprint and RSS are not interchangeable values.
    pub const fn name(self) -> &'static str {
        match self {
            Self::PhysicalFootprint => "physical footprint",
            Self::Rss => "RSS",
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
    platform::sample(pid)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::io;
    use std::mem::MaybeUninit;

    use super::{MemoryMetric, MemorySample};

    const TASK_VM_INFO: libc::task_flavor_t = 22;

    // TASK_VM_INFO_REV1 is the stable prefix through `phys_footprint`.
    // Asking for this count lets old and new kernels return the same field
    // without copying later revisions past this buffer.
    #[repr(C)]
    struct TaskVmInfoRev1 {
        virtual_size: u64,
        region_count: libc::integer_t,
        page_size: libc::integer_t,
        resident_size: u64,
        resident_size_peak: u64,
        device: u64,
        device_peak: u64,
        internal: u64,
        internal_peak: u64,
        external: u64,
        external_peak: u64,
        reusable: u64,
        reusable_peak: u64,
        purgeable_volatile_pmap: u64,
        purgeable_volatile_resident: u64,
        purgeable_volatile_virtual: u64,
        compressed: u64,
        compressed_peak: u64,
        compressed_lifetime: u64,
        phys_footprint: u64,
    }

    unsafe extern "C" {
        static mach_task_self_: libc::mach_port_t;
        fn task_name_for_pid(
            target_task: libc::mach_port_t,
            pid: libc::pid_t,
            task: *mut libc::mach_port_t,
        ) -> libc::kern_return_t;
        fn mach_port_deallocate(
            task: libc::mach_port_t,
            name: libc::mach_port_t,
        ) -> libc::kern_return_t;
    }

    pub(super) fn sample(pid: u32) -> io::Result<MemorySample> {
        let pid = libc::pid_t::try_from(pid)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid exceeds pid_t"))?;
        // SAFETY: libSystem initializes the caller's task send right before
        // process startup.
        let own_task = unsafe { mach_task_self_ };
        let borrowed = pid == std::process::id() as libc::pid_t;
        let mut task = own_task;
        if !borrowed {
            // A task-name port carries inspection rights without the control
            // rights `task_for_pid` requests (and ordinary macOS processes
            // are denied). It is sufficient for TASK_VM_INFO.
            // SAFETY: `task` points to writable storage and `pid` is a valid
            // pid_t. A successful call initializes the send right.
            let result = unsafe { task_name_for_pid(own_task, pid, &mut task) };
            if result != libc::KERN_SUCCESS {
                // Hardened-runtime and sandbox policy can deny even a parent
                // the read-only task port. libproc exposes the same physical
                // footprint ledger for those processes, so child sampling
                // remains useful without silently falling back to RSS.
                return sample_via_rusage(pid);
            }
        }

        let mut info = MaybeUninit::<TaskVmInfoRev1>::zeroed();
        let mut count = (size_of::<TaskVmInfoRev1>() / size_of::<libc::natural_t>())
            as libc::mach_msg_type_number_t;
        // SAFETY: `info` is aligned writable storage for `count` natural_t
        // words, and TASK_VM_INFO_REV1 initializes the entire prefix.
        let result = unsafe {
            libc::task_info(
                task,
                TASK_VM_INFO,
                info.as_mut_ptr().cast::<libc::integer_t>(),
                &mut count,
            )
        };
        if !borrowed {
            // SAFETY: a successful task_name_for_pid gave this process a send
            // right, which is released exactly once here.
            let _ = unsafe { mach_port_deallocate(own_task, task) };
        }
        if result != libc::KERN_SUCCESS {
            return sample_via_rusage(pid);
        }
        // SAFETY: task_info succeeded with the full REV1 count.
        let info = unsafe { info.assume_init() };
        Ok(MemorySample {
            bytes: info.phys_footprint,
            metric: MemoryMetric::PhysicalFootprint,
        })
    }

    fn sample_via_rusage(pid: libc::pid_t) -> io::Result<MemorySample> {
        let mut usage = MaybeUninit::<libc::rusage_info_v0>::zeroed();
        // SAFETY: the buffer has the exact layout requested by
        // RUSAGE_INFO_V0 and is initialized on success.
        let result =
            unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V0, usage.as_mut_ptr().cast()) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: proc_pid_rusage succeeded and initialized the V0 buffer.
        let usage = unsafe { usage.assume_init() };
        Ok(MemorySample {
            bytes: usage.ri_phys_footprint,
            metric: MemoryMetric::PhysicalFootprint,
        })
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::io;

    use super::{MemoryMetric, MemorySample};

    pub(super) fn sample(pid: u32) -> io::Result<MemorySample> {
        let statm = std::fs::read_to_string(format!("/proc/{pid}/statm"))?;
        let resident_pages = statm
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "statm has no RSS field"))?
            .parse::<u64>()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        // SAFETY: sysconf has no pointer arguments and _SC_PAGESIZE has no
        // side effects. A non-positive answer is handled as an error.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page_size = u64::try_from(page_size)
            .map_err(|_| io::Error::other("sysconf(_SC_PAGESIZE) failed"))?;
        let bytes = resident_pages
            .checked_mul(page_size)
            .ok_or_else(|| io::Error::other("RSS byte count overflowed"))?;
        Ok(MemorySample {
            bytes,
            metric: MemoryMetric::Rss,
        })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use std::io;

    use super::MemorySample;

    pub(super) fn sample(_pid: u32) -> io::Result<MemorySample> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process memory sampling is supported only on macOS and Linux",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn sampler_names_and_reads_the_current_process_measure() {
        let sample = sample_memory(std::process::id()).expect("sample this process");
        assert!(sample.bytes > 0);
        #[cfg(target_os = "macos")]
        assert_eq!(sample.name(), "physical footprint");
        #[cfg(target_os = "linux")]
        assert_eq!(sample.name(), "RSS");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
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
        let result = sample_memory(child.id());
        let _ = child.kill();
        let _ = child.wait();
        let sample = result.expect("sample child process");
        assert!(sample.bytes > 0);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    #[ignore = "spawned only by sampler_reads_a_child_process"]
    fn sampler_child_target() {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
}
