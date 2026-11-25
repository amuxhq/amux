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
    if BLIND.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(std::io::Error::other(
            "the process table is hidden from this test",
        ));
    }
    read_table()
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

/// The row for `pid`, if it names a process.
#[cfg(target_os = "macos")]
pub(super) fn row(pid: i32) -> Option<Row> {
    /// `proc_bsdinfo.pbi_status` of a process waiting to be reaped.
    const SZOMB: u32 = 5;
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
        (written == size).then(|| info.assume_init())?
    };
    let started = info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec;
    Some(Row {
        pid,
        ppid: info.pbi_ppid as i32,
        started,
        zombie: info.pbi_status == SZOMB,
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
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name in parentheses may hold spaces and parentheses of
    // its own; the fields after it are plain. They start at the state, so
    // the parent pid is the second and the start time (field 22) the
    // twentieth.
    let mut fields = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
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
