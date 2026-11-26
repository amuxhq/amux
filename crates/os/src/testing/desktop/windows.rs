use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt as _;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, HWND, LPARAM};
use windows_sys::Win32::System::JobObjects::{
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JobObjectBasicProcessIdList, QueryInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, EnumWindows, GWL_STYLE, GetClassNameW, GetWindowLongW,
    GetWindowThreadProcessId, InternalGetWindowText, WS_POPUP, WS_VISIBLE,
};
use windows_sys::core::BOOL;

use super::Window;
use crate::Pid;

pub(super) fn visible_windows() -> io::Result<Vec<Window>> {
    unsafe extern "system" fn each(window: HWND, found: LPARAM) -> BOOL {
        // SAFETY: `found` is the vector `visible_windows` passed, which
        // outlives the enumeration, and only this callback touches it.
        let found = unsafe { &mut *(found as *mut Vec<HWND>) };
        // Its own style, not IsWindowVisible, which asks the desktop too:
        // a desktop nobody sees (a scheduled task's) never shows, so
        // nothing on it would count.
        // SAFETY: a query of a window the enumeration names.
        if unsafe { GetWindowLongW(window, GWL_STYLE) } as u32 & WS_VISIBLE != 0 {
            found.push(window);
        }
        1
    }
    let mut found: Vec<HWND> = Vec::new();
    // SAFETY: the callback reads `found` only while EnumWindows runs.
    if unsafe { EnumWindows(Some(each), (&raw mut found) as LPARAM) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(found
        .into_iter()
        .map(|window| {
            let mut pid = 0u32;
            // SAFETY: queries of a window that may have gone since, which
            // answer zero and an empty text then.
            unsafe { GetWindowThreadProcessId(window, &mut pid) };
            let class = text(|buffer, room| unsafe { GetClassNameW(window, buffer, room) });
            // The text as the window manager keeps it: GetWindowTextW would
            // ask a window of this process by a message to its thread, which
            // a thread that pumps none never answers.
            let title = text(|buffer, room| unsafe { InternalGetWindowText(window, buffer, room) });
            Window {
                id: window as usize,
                pid,
                program: program(pid),
                class,
                title,
            }
        })
        .collect())
}

/// What `read` writes into a buffer of 512 UTF-16 units, its count
/// answered.
fn text(read: impl FnOnce(*mut u16, i32) -> i32) -> String {
    let mut buffer = [0u16; 512];
    let length = read(buffer.as_mut_ptr(), buffer.len() as i32).max(0) as usize;
    String::from_utf16_lossy(&buffer[..length.min(buffer.len())])
}

/// The program `pid` runs, when it can be opened to ask.
fn program(pid: Pid) -> Option<PathBuf> {
    // SAFETY: opening a process by id for a query; null is a failure.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut buffer = vec![0u16; 32_768];
    let mut size = buffer.len() as u32;
    // SAFETY: `buffer` holds `size` units; the handle is closed after.
    let read = unsafe {
        QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size)
    };
    // SAFETY: opened above.
    unsafe { CloseHandle(process) };
    (read != 0).then(|| PathBuf::from(OsString::from_wide(&buffer[..size as usize])))
}

pub(super) fn show_a_window(title: &str, while_shown: impl FnOnce()) -> io::Result<()> {
    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain([0]).collect() };
    let (class, name) = (wide("STATIC"), wide(title));
    // SAFETY: a system class and NUL-terminated names; the window is
    // destroyed below, on this thread, which made it.
    let window = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            name.as_ptr(),
            WS_POPUP | WS_VISIBLE,
            -32_000,
            -32_000,
            1,
            1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if window.is_null() {
        return Err(io::Error::last_os_error());
    }
    while_shown();
    // SAFETY: made above, on this thread.
    unsafe { DestroyWindow(window) };
    Ok(())
}

pub(super) fn job_members() -> io::Result<Vec<Pid>> {
    let header = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList);
    let mut room = 64usize;
    loop {
        let words = header.div_ceil(size_of::<usize>()) + room;
        let mut buffer = vec![0usize; words];
        let bytes = (words * size_of::<usize>()) as u32;
        // SAFETY: a null job is this process's own; the buffer is `bytes`
        // long and aligned for the list, which the call writes no more
        // than.
        let done = unsafe {
            QueryInformationJobObject(
                std::ptr::null_mut(),
                JobObjectBasicProcessIdList,
                buffer.as_mut_ptr().cast(),
                bytes,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: the buffer starts with the list's header.
        let list = unsafe { &*buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
        if done == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_MORE_DATA as i32) {
                room = (list.NumberOfAssignedProcesses as usize).max(room * 2) + 16;
                continue;
            }
            return Err(error);
        }
        let count = list.NumberOfProcessIdsInList as usize;
        let first = header / size_of::<usize>();
        return Ok(buffer[first..first + count]
            .iter()
            .map(|&id| id as Pid)
            .collect());
    }
}

#[cfg(test)]
mod tests {
    use super::super::{job_members, show_a_window, visible_windows};

    /// A window shown is listed, with this process as its owner and this
    /// program as the owner's, until it is destroyed.
    #[test]
    fn a_window_that_shows_is_listed_with_its_owner_until_it_goes() {
        let title = format!("os-testing-desktop-{}", std::process::id());
        let mut listed = Vec::new();
        show_a_window(&title, || listed = visible_windows().unwrap()).unwrap();

        let ours = listed
            .iter()
            .find(|shown| shown.title == title)
            .unwrap_or_else(|| panic!("{title} is not among {listed:?}"));
        assert_eq!(ours.pid, std::process::id());
        assert!(ours.class.eq_ignore_ascii_case("static"), "{ours:?}");
        let me = std::env::current_exe().unwrap();
        assert_eq!(
            ours.program
                .as_deref()
                .map(std::fs::canonicalize)
                .map(Result::unwrap),
            Some(std::fs::canonicalize(me).unwrap())
        );
        assert!(
            visible_windows()
                .unwrap()
                .iter()
                .all(|shown| shown.id != ours.id),
            "a destroyed window is gone"
        );
    }

    /// A process in a job finds itself among the job's members; one in
    /// none is told so.
    #[test]
    fn this_process_is_among_its_jobs_members() {
        match job_members() {
            Ok(members) => assert!(members.contains(&std::process::id()), "{members:?}"),
            Err(error) => assert_ne!(error.kind(), std::io::ErrorKind::Unsupported, "{error}"),
        }
    }
}
