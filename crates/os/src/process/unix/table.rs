//! The kernel's process table, read directly rather than through `ps`:
//! macOS's `ps` is setuid root, and a sandboxed process may not run a
//! setuid program, so under a sandbox `ps` printed nothing and a stop left
//! every command running.

/// One row of the table.
#[derive(Clone, Copy, Debug)]
pub(super) struct Row {
    pub pid: i32,
    pub ppid: i32,
    /// When it started, in the kernel's own unit: with the pid, the
    /// process's identity.
    pub started: u64,
    /// It has exited and waits to be reaped.
    pub zombie: bool,
}

/// Every process, or none where the table can't be read: for a census
/// that only looks. One that must know it saw everything (a family's
/// end) uses [`try_table`].
pub(super) fn table() -> Vec<Row> {
    try_table().unwrap_or_default()
}

/// Every process, or why the table couldn't be read.
pub(super) fn try_table() -> std::io::Result<Vec<Row>> {
    #[cfg(test)]
    READS.with(|reads| reads.set(reads.get() + 1));
    #[cfg(test)]
    if BLIND.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(std::io::Error::other(
            "the process table is hidden from this test",
        ));
    }
    read_table()
}

#[cfg(test)]
thread_local! {
    /// How often this thread has read the table, for a test to count.
    pub(super) static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A test's way to make this process's census fail, as an unreadable
/// `/proc` would.
#[cfg(test)]
pub(super) static BLIND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "macos")]
fn read_table() -> std::io::Result<Vec<Row>> {
    // SAFETY: a null buffer asks only for the number of pids.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Room for processes started since the count.
    let mut pids = vec![0 as libc::pid_t; count as usize + 64];
    let bytes = (pids.len() * size_of::<libc::pid_t>()) as libc::c_int;
    // SAFETY: the buffer holds `bytes` bytes of pids.
    let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    if count <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    pids.truncate(count as usize);
    Ok(pids.into_iter().filter_map(row).collect())
}

/// The row for `pid`, if it names a process. `proc_pidinfo` answers ESRCH
/// for a process that has exited unreaped, though its pid and its group's
/// id are still held; `sysctl` still has its row. Another user's process,
/// which `proc_pidinfo` refuses (EPERM), stays out of the table: no census
/// needs it, and asking `sysctl` for each would slow every census.
#[cfg(target_os = "macos")]
pub(super) fn row(pid: i32) -> Option<Row> {
    match live_row(pid) {
        Ok(row) => Some(row),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            unreaped_row(pid).filter(|row| row.zombie)
        }
        Err(_) => None,
    }
}

/// `proc_bsdinfo.pbi_status` and `extern_proc.p_stat` of a process
/// waiting to be reaped.
#[cfg(target_os = "macos")]
const SZOMB: u32 = 5;

#[cfg(target_os = "macos")]
pub(super) fn live_row(pid: i32) -> std::io::Result<Row> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the buffer holds one proc_bsdinfo, and is read only when the
    // kernel wrote all of it.
    let info = unsafe {
        let written = libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        );
        if written != size {
            return Err(std::io::Error::last_os_error());
        }
        info.assume_init()
    };
    let started = info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec;
    Ok(Row {
        pid,
        ppid: info.pbi_ppid as i32,
        started,
        zombie: info.pbi_status == SZOMB,
    })
}

/// The row `sysctl(KERN_PROC_PID)` gives, which an exited, unreaped
/// process still has. Its `struct kinfo_proc` (`<sys/sysctl.h>`), which
/// libc doesn't define for Apple, is read as bytes at its fields'
/// offsets, the same on arm64 and x86_64.
#[cfg(target_os = "macos")]
pub(super) fn unreaped_row(pid: i32) -> Option<Row> {
    const SIZE: usize = 648;
    /// `kp_proc.p_starttime`, a `struct timeval`.
    const STARTED_SECS: usize = 0;
    const STARTED_USECS: usize = 8;
    /// `kp_proc.p_stat`, one byte.
    const STATUS: usize = 36;
    /// `kp_eproc.e_ppid`.
    const PPID: usize = 560;
    let mut buffer = [0u8; SIZE];
    let mut length = SIZE;
    let mut name = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    // SAFETY: the name is four ints long, and the kernel writes at most
    // `length` bytes into the buffer, saying how many it wrote.
    let read = unsafe {
        libc::sysctl(
            name.as_mut_ptr(),
            name.len() as libc::c_uint,
            buffer.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    // No such pid: success, with nothing written.
    if read != 0 || length != SIZE {
        return None;
    }
    let field = |at: usize| -> [u8; 4] { buffer[at..at + 4].try_into().expect("four bytes") };
    let secs = i64::from_ne_bytes(
        buffer[STARTED_SECS..STARTED_SECS + 8]
            .try_into()
            .expect("eight bytes"),
    );
    let usecs = i32::from_ne_bytes(field(STARTED_USECS));
    Some(Row {
        pid,
        ppid: i32::from_ne_bytes(field(PPID)),
        started: secs as u64 * 1_000_000 + usecs as u64,
        zombie: u32::from(buffer[STATUS]) == SZOMB,
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_table() -> std::io::Result<Vec<Row>> {
    let mut rows = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        if let Some(row) = entry?
            .file_name()
            .to_str()
            .and_then(|name| row(name.parse().ok()?))
        {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// The row for `pid`, if it names a process.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(super) fn row(pid: i32) -> Option<Row> {
    parse_stat(pid, &std::fs::read(format!("/proc/{pid}/stat")).ok()?)
}

/// A row from the bytes of `/proc/<pid>/stat`. The command name in
/// parentheses may hold spaces, parentheses and bytes that aren't UTF-8
/// (it is whatever the process named itself), so only what follows the
/// last `)` is read, which is plain ASCII. Its fields start at the state,
/// so the parent pid is the second and the start time (field 22) the
/// twentieth.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(super) fn parse_stat(pid: i32, stat: &[u8]) -> Option<Row> {
    let after = stat.iter().rposition(|&byte| byte == b')')? + 1;
    let rest = std::str::from_utf8(&stat[after..]).ok()?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?;
    let ppid = fields.next()?.parse().ok()?;
    let started = fields.nth(17)?.parse().ok()?;
    Some(Row {
        pid,
        ppid,
        started,
        zombie: state == "Z" || state == "X",
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
fn read_table() -> std::io::Result<Vec<Row>> {
    Err(std::io::ErrorKind::Unsupported.into())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
pub(super) fn row(_pid: i32) -> Option<Row> {
    None
}

/// Whether this OS's table can be read at all.
pub(super) const READABLE: bool = cfg!(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android"
));
