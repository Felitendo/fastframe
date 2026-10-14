//! Windows: the headless sleep, which waits on the waker's event and the
//! thread's messages together, and answers the messages.

// The event and the message loop are FFI.
#![allow(unsafe_code)]

use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
    PeekMessageW, PostQuitMessage, QS_ALLINPUT, TranslateMessage, WM_QUIT,
};

/// An auto-reset event that a wake sets and the sleep waits on.
pub(crate) struct Event(HANDLE);

// SAFETY: an event handle may be set, waited on and closed from any thread.
unsafe impl Send for Event {}
// SAFETY: as above; `set` and `wait_pumping` only pass the handle to the OS.
unsafe impl Sync for Event {}

impl Default for Event {
    fn default() -> Self {
        // SAFETY: no security attributes or name; auto-reset, not signalled.
        // A null handle (out of resources) makes `set` fail and the wait
        // fall back to messages and its timeout, which still ends the sleep.
        Self(unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) })
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle came from CreateEventW and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

impl Event {
    pub(crate) fn set(&self) {
        // SAFETY: the handle is this event's, or null, which fails harmlessly.
        unsafe { SetEvent(self.0) };
    }

    /// Waits up to `timeout` for the event or a message for this thread,
    /// then answers every message waiting.
    pub(crate) fn wait_pumping(&self, timeout: Duration) {
        // Rounded up, so a sub-millisecond rest is a wait and not a spin;
        // INFINITE itself is never asked for, so the caller's countdown
        // decides when the sleep ends.
        let millis = u32::try_from(timeout.as_micros().div_ceil(1000))
            .unwrap_or(u32::MAX)
            .min(INFINITE - 1);
        let handles = [self.0];
        let count = u32::from(!self.0.is_null());
        // SAFETY: `handles` outlives the call and holds `count` valid handles.
        unsafe {
            MsgWaitForMultipleObjectsEx(
                count,
                handles.as_ptr(),
                millis,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            );
        }
        pump();
    }
}

/// Dispatches every message waiting for this thread.
fn pump() {
    // SAFETY: an all-zero MSG is a valid value for PeekMessageW to fill.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: `message` outlives each call; a null window takes every
    // message for this thread.
    while unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
        if message.message == WM_QUIT {
            // Not ours to act on: put it back for the next window's loop.
            // SAFETY: posts WM_QUIT to this thread with the same code.
            unsafe { PostQuitMessage(message.wParam as i32) };
            return;
        }
        // SAFETY: `message` came from PeekMessageW just now.
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{PM_NOREMOVE, PostThreadMessageW, WM_APP};

    fn peek(first: u32, remove: bool) -> Option<MSG> {
        // SAFETY: as in `pump`.
        let mut message: MSG = unsafe { std::mem::zeroed() };
        let flags = if remove { PM_REMOVE } else { PM_NOREMOVE };
        // SAFETY: as in `pump`, for messages `first` alone.
        let found =
            unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), first, first, flags) };
        (found != 0).then_some(message)
    }

    /// The sleep takes the thread's messages off its queue, so Windows sees
    /// them answered, and leaves a quit for the next window's loop.
    #[test]
    fn a_wait_answers_the_threads_messages_and_keeps_a_quit() {
        let event = Event::default();
        // Gives the thread its message queue.
        assert!(peek(WM_APP + 1, false).is_none());
        // SAFETY: posts a message with no data to this thread.
        assert_ne!(
            unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_APP + 1, 0, 0) },
            0
        );
        let started = Instant::now();
        event.wait_pumping(Duration::from_secs(5));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a message ends it"
        );
        assert!(peek(WM_APP + 1, false).is_none(), "answered");
        // SAFETY: posts WM_QUIT to this thread.
        unsafe { PostQuitMessage(7) };
        event.wait_pumping(Duration::from_millis(50));
        let quit = peek(WM_QUIT, true).expect("the quit is kept");
        assert_eq!(quit.wParam, 7);
    }

    #[test]
    fn a_set_event_ends_the_wait() {
        let event = Event::default();
        event.set();
        let started = Instant::now();
        event.wait_pumping(Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
