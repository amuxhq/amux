//! Windows: private bytes from `GetProcessMemoryInfo`; no model an
//! enrolled machine is named by, no load average and no open-file limit.

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

use crate::perf::{MemoryMetric, MemorySample};

/// `PROCESS_MEMORY_COUNTERS_EX`, which the windows-sys features the
/// workspace builds don't carry (`Win32_System_ProcessStatus`).
#[repr(C)]
#[derive(Default)]
struct ProcessMemoryCountersEx {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_usage: usize,
}

// kernel32's export of psapi's GetProcessMemoryInfo, present since
// Windows 7. raw-dylib needs no import library.
#[link(name = "kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn K32GetProcessMemoryInfo(
        process: HANDLE,
        counters: *mut ProcessMemoryCountersEx,
        size: u32,
    ) -> i32;
}

pub(in crate::platform) fn memory(pid: u32) -> io::Result<MemorySample> {
    // SAFETY: plain call; a null handle is checked below.
    let process = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
            FALSE,
            pid,
        )
    };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut counters = ProcessMemoryCountersEx {
        cb: size_of::<ProcessMemoryCountersEx>() as u32,
        ..ProcessMemoryCountersEx::default()
    };
    // SAFETY: `counters` is writable storage of the size passed, and
    // `process` is the handle opened above.
    let read = unsafe { K32GetProcessMemoryInfo(process, &mut counters, counters.cb) };
    let error = io::Error::last_os_error();
    // SAFETY: closing the handle opened above, once.
    unsafe { CloseHandle(process) };
    if read == 0 {
        return Err(error);
    }
    Ok(MemorySample {
        bytes: counters.private_usage as u64,
        metric: MemoryMetric::PrivateBytes,
    })
}

pub(in crate::platform) fn machine_model() -> io::Result<String> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no Windows machine is enrolled for the performance workloads",
    ))
}

pub(in crate::platform) fn load_average() -> io::Result<[f64; 3]> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows keeps no load average",
    ))
}

/// Windows limits open handles per process only by memory.
pub(in crate::platform) fn raise_open_files(_wanted: u64) -> io::Result<Option<u64>> {
    Ok(None)
}

pub(in crate::platform) fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}
