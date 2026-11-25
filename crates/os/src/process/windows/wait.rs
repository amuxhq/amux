//! Waiting on a handle without a thread of our own: the thread pool runs a
//! callback when the handle is signalled, which wakes the future.

use std::io;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

use tokio::sync::oneshot;
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Threading::{
    INFINITE, RegisterWaitForSingleObject, UnregisterWaitEx, WT_EXECUTEONLYONCE,
};

struct Wake {
    wake: Mutex<Option<oneshot::Sender<()>>>,
}

/// Resolves once `handle` is signalled. The handle must stay open while
/// the future lives.
pub(super) fn signalled(handle: HANDLE) -> io::Result<Signalled> {
    let (sender, receiver) = oneshot::channel();
    let context = Box::into_raw(Box::new(Wake {
        wake: Mutex::new(Some(sender)),
    }));
    let mut wait: HANDLE = std::ptr::null_mut();
    // SAFETY: the context outlives the wait, which Drop unregisters (and
    // waits out) before freeing it.
    let registered = unsafe {
        RegisterWaitForSingleObject(
            &mut wait,
            handle,
            Some(callback),
            context.cast(),
            INFINITE,
            WT_EXECUTEONLYONCE,
        )
    };
    if registered == 0 {
        let error = io::Error::last_os_error();
        // SAFETY: nothing else has the context.
        drop(unsafe { Box::from_raw(context) });
        return Err(error);
    }
    Ok(Signalled {
        receiver,
        wait,
        context,
    })
}

unsafe extern "system" fn callback(context: *mut core::ffi::c_void, _timed_out: bool) {
    // SAFETY: the context lives until the wait is unregistered, which
    // waits for this callback to return.
    let context = unsafe { &*context.cast::<Wake>() };
    if let Some(sender) = context
        .wake
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        let _ = sender.send(());
    }
}

pub(super) struct Signalled {
    receiver: oneshot::Receiver<()>,
    wait: HANDLE,
    context: *mut Wake,
}

// SAFETY: the wait handle and the context are used from any thread; the
// context's own state is behind a mutex.
unsafe impl Send for Signalled {}

impl Future for Signalled {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.receiver).poll(cx).map(drop)
    }
}

impl Drop for Signalled {
    fn drop(&mut self) {
        // SAFETY: unregistering our wait, blocking until a running
        // callback returns, then freeing the context nothing uses now.
        unsafe {
            UnregisterWaitEx(self.wait, INVALID_HANDLE_VALUE);
            drop(Box::from_raw(self.context));
        }
    }
}
