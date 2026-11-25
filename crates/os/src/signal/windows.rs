//! Windows: a console control handler of ours, and a hidden top-level
//! window for the session's end, which a process that has loaded user32
//! gets instead of the console's logoff and shutdown events.

use std::io;
use std::sync::{Condvar, Mutex, OnceLock};

use tokio::sync::watch;
use windows_sys::Win32::Foundation::{ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Console::{
    CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Shutdown::{ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, ENDSESSION_LOGOFF, GetMessageW, MSG,
    PostMessageW, RegisterClassW, TranslateMessage, WM_APP, WM_ENDSESSION, WM_QUERYENDSESSION,
    WNDCLASSW, WS_OVERLAPPED,
};

use super::{GRACE, Stop};

/// Posted to the window by [`finished`]: drop the block reason, which only
/// the window's own thread may do.
const FINISHED: u32 = WM_APP + 1;

struct State {
    stop: watch::Sender<Option<Stop>>,
    done: Mutex<bool>,
    done_changed: Condvar,
    /// The hidden window, once its thread has made it.
    window: OnceLock<isize>,
}

fn state() -> &'static State {
    static STATE: OnceLock<State> = OnceLock::new();
    STATE.get_or_init(|| State {
        stop: watch::channel(None).0,
        done: Mutex::new(false),
        done_changed: Condvar::new(),
        window: OnceLock::new(),
    })
}

/// The console handler and the window, once per process.
fn register() -> io::Result<&'static State> {
    static REGISTERED: OnceLock<Result<(), (io::ErrorKind, String)>> = OnceLock::new();
    let registered = REGISTERED.get_or_init(|| {
        // SAFETY: adds our handler, a plain function, to this process's list.
        if unsafe { SetConsoleCtrlHandler(Some(console), 1) } == 0 {
            let error = io::Error::last_os_error();
            return Err((error.kind(), format!("adding the console handler: {error}")));
        }
        let (ready, made) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("amux-session-end".into())
            .spawn(move || window_thread(&ready))
            .map_err(|error| {
                (
                    error.kind(),
                    format!("starting the window's thread: {error}"),
                )
            })?;
        let window = made
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|_| {
                (
                    io::ErrorKind::TimedOut,
                    "the session-end window wasn't made".to_owned(),
                )
            })?
            .map_err(|error| {
                (
                    error.kind(),
                    format!("making the session-end window: {error}"),
                )
            })?;
        let _ = state().window.set(window);
        Ok(())
    });
    match registered {
        Ok(()) => Ok(state()),
        Err((kind, message)) => Err(io::Error::new(*kind, message.clone())),
    }
}

pub(super) async fn terminated() -> io::Result<Stop> {
    let state = register()?;
    let mut stops = state.stop.subscribe();
    let stop = stops
        .wait_for(Option::is_some)
        .await
        .map_err(|_| io::Error::other("the stop channel closed"))?;
    Ok(stop.expect("waited for a stop"))
}

pub(super) fn finished() {
    let state = state();
    *state
        .done
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    state.done_changed.notify_all();
    if let Some(&window) = state.window.get() {
        // SAFETY: posting to our own window; it may be gone at exit.
        unsafe {
            PostMessageW(window as HWND, FINISHED, 0, 0);
        }
    }
}

/// The first stop wins; later ones change nothing.
fn deliver_stop(stop: Stop) {
    state().stop.send_if_modified(|seen| {
        if seen.is_none() {
            *seen = Some(stop);
            true
        } else {
            false
        }
    });
}

fn is_finished() -> bool {
    *state()
        .done
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Holds the OS off until [`finished`] or [`GRACE`].
fn wait_finished() {
    let state = state();
    let done = state
        .done
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = state
        .done_changed
        .wait_timeout_while(done, GRACE, |done| !*done);
}

/// Called by the OS on a thread of its own. For close, logoff and
/// shutdown the OS ends the process when this returns, so it waits for the
/// clean shutdown.
unsafe extern "system" fn console(event: u32) -> windows_sys::core::BOOL {
    let stop = match event {
        CTRL_C_EVENT => Stop::Interrupt,
        CTRL_CLOSE_EVENT => Stop::Close,
        CTRL_LOGOFF_EVENT => Stop::Logoff,
        CTRL_SHUTDOWN_EVENT => Stop::Shutdown,
        // CTRL_BREAK and anything newer: not ours, so the next handler
        // (in the end the default, which exits) has it.
        _ => return 0,
    };
    deliver_stop(stop);
    if stop != Stop::Interrupt {
        wait_finished();
    }
    1
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn window_thread(ready: &std::sync::mpsc::Sender<io::Result<isize>>) {
    let class = wide("amux-os-session-end");
    // SAFETY: registering a class whose strings outlive the thread's
    // window, making a window that is never shown, and pumping its messages
    // on the thread that made it.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let mut description: WNDCLASSW = std::mem::zeroed();
        description.lpfnWndProc = Some(window_proc);
        description.hInstance = instance;
        description.lpszClassName = class.as_ptr();
        if RegisterClassW(&description) == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_CLASS_ALREADY_EXISTS as i32) {
                let _ = ready.send(Err(error));
                return;
            }
        }
        let title = wide("amux");
        // Top-level, not message-only: only top-level windows are sent the
        // session's end.
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if window.is_null() {
            let _ = ready.send(Err(io::Error::last_os_error()));
            return;
        }
        let _ = ready.send(Ok(window as isize));
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

const REASON: &str = "amux is saving its state";

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_QUERYENDSESSION => {
            if !is_finished() {
                let reason = wide(REASON);
                // SAFETY: on the window's own thread, with a string the
                // call copies.
                unsafe {
                    ShutdownBlockReasonCreate(window, reason.as_ptr());
                }
            }
            // The session may end: a stop needs no veto, only time.
            1
        }
        WM_ENDSESSION => {
            if wparam != 0 {
                let logoff = (lparam as u32) & ENDSESSION_LOGOFF != 0;
                deliver_stop(if logoff { Stop::Logoff } else { Stop::Shutdown });
                // The session ends once this returns.
                wait_finished();
            }
            // SAFETY: on the window's own thread.
            unsafe {
                ShutdownBlockReasonDestroy(window);
            }
            0
        }
        FINISHED => {
            // SAFETY: on the window's own thread.
            unsafe {
                ShutdownBlockReasonDestroy(window);
            }
            0
        }
        // SAFETY: the default for everything else.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

#[cfg(any(test, feature = "testing"))]
pub(super) fn deliver(event: super::testing::Event) -> io::Result<bool> {
    use windows_sys::Win32::System::Console::CTRL_BREAK_EVENT;
    use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;

    use super::testing::Event;
    let state = register()?;
    let control = |event| {
        // SAFETY: our own handler, called as the OS would call it.
        Ok(unsafe { console(event) } != 0)
    };
    match event {
        Event::Interrupt => control(CTRL_C_EVENT),
        Event::ConsoleBreak => control(CTRL_BREAK_EVENT),
        Event::ConsoleClose => control(CTRL_CLOSE_EVENT),
        Event::ConsoleLogoff => control(CTRL_LOGOFF_EVENT),
        Event::ConsoleShutdown => control(CTRL_SHUTDOWN_EVENT),
        Event::SessionEnd { .. } | Event::SessionEndCancelled => {
            let window = *state
                .window
                .get()
                .ok_or_else(|| io::Error::other("no session-end window"))?
                as HWND;
            let ending = !matches!(event, Event::SessionEndCancelled);
            let lparam = match event {
                Event::SessionEnd { logoff: true } => ENDSESSION_LOGOFF as LPARAM,
                _ => 0,
            };
            // SAFETY: sending to our own window, as the OS does.
            let allowed = unsafe { SendMessageW(window, WM_QUERYENDSESSION, 0, lparam) };
            // SAFETY: as above.
            unsafe {
                SendMessageW(window, WM_ENDSESSION, usize::from(ending), lparam);
            }
            Ok(allowed != 0)
        }
        Event::Terminate | Event::Hangup => Err(io::Error::from(io::ErrorKind::Unsupported)),
    }
}

#[cfg(any(test, feature = "testing"))]
pub(super) fn block_reason() -> io::Result<Option<String>> {
    use windows_sys::Win32::System::Shutdown::ShutdownBlockReasonQuery;
    let state = register()?;
    let Some(&window) = state.window.get() else {
        return Ok(None);
    };
    let mut buffer = vec![0u16; 256];
    let mut length = buffer.len() as u32;
    // SAFETY: the buffer holds `length` characters.
    if unsafe { ShutdownBlockReasonQuery(window as HWND, buffer.as_mut_ptr(), &mut length) } == 0 {
        return Ok(None);
    }
    let text = &buffer[..buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len())];
    Ok(Some(String::from_utf16_lossy(text)))
}
