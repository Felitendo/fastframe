//! The Windows notification-area item, on its own thread with a message loop.

// The Win32 message loop is FFI.
#![allow(unsafe_code)]

use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx,
    PM_REMOVE, PeekMessageW, PostQuitMessage, PostThreadMessageW, QS_ALLINPUT, TranslateMessage,
    WM_APP, WM_QUIT,
};

use crate::{Config, DrawIcon, Router};

/// A change to the item, carried to the tray thread.
enum Change {
    Label(String, String),
    Visible(String, bool),
    Enabled(String, bool),
    Icon(DrawIcon, Option<DrawIcon>),
    Tooltip(String),
}

pub(crate) struct Host {
    changes: Sender<Change>,
    thread_id: u32,
}

impl Host {
    /// Makes the item on a new thread and waits for it to exist.
    pub(crate) fn start(config: Config, router: Router) -> Result<Self, String> {
        let (changes, pending) = std::sync::mpsc::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name(format!("{}-tray", config.id))
            .spawn(move || serve(&config, router, &pending, &ready_tx))
            .map_err(|error| error.to_string())?;
        let thread_id = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "the tray thread did not answer".to_owned())??;
        Ok(Self { changes, thread_id })
    }

    pub(crate) fn set_label(&mut self, id: &str, label: String) {
        self.change(Change::Label(id.to_owned(), label));
    }

    pub(crate) fn set_visible(&mut self, id: &str, visible: bool) {
        self.change(Change::Visible(id.to_owned(), visible));
    }

    pub(crate) fn set_enabled(&mut self, id: &str, enabled: bool) {
        self.change(Change::Enabled(id.to_owned(), enabled));
    }

    pub(crate) fn set_icon(&mut self, icon: DrawIcon, template_icon: Option<DrawIcon>) {
        self.change(Change::Icon(icon, template_icon));
    }

    pub(crate) fn set_tooltip(&mut self, text: String) {
        self.change(Change::Tooltip(text));
    }

    fn change(&self, change: Change) {
        if self.changes.send(change).is_ok() {
            self.poke(WM_APP);
        }
    }

    /// The item runs on its own thread from the start.
    pub(crate) fn attach(&mut self) {}

    /// Posts `message` to the tray thread's loop.
    fn poke(&self, message: u32) {
        // SAFETY: posting to a thread id is valid even if the thread has
        // ended; the call then fails, which is harmless here.
        unsafe {
            PostThreadMessageW(self.thread_id, message, 0, 0);
        }
    }
}

impl Drop for Host {
    /// Ends the tray thread, which removes the item.
    fn drop(&mut self) {
        self.poke(WM_QUIT);
    }
}

/// The tray thread: makes the item, reports its thread id, and runs the
/// message loop until `WM_QUIT`.
fn serve(
    config: &Config,
    router: Router,
    pending: &Receiver<Change>,
    ready: &Sender<Result<u32, String>>,
) {
    let mut item = match crate::native::build(config, router) {
        Ok(item) => item,
        Err(error) => {
            let _ = ready.send(Err(error.to_string()));
            return;
        }
    };
    // SAFETY: no preconditions.
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
    // SAFETY: MSG is plain data; all-zero is a valid value to fill in.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: `message` is a valid MSG for this thread's queue.
    while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
        // The open menu's modal loop takes the thread's messages and drops
        // the WM_APP poke, so every message this loop gets applies what is
        // waiting, and the one tray-icon posts as its menu closes catches up.
        for change in pending.try_iter() {
            match change {
                Change::Label(id, label) => item.set_label(&id, &label),
                Change::Visible(id, visible) => item.set_visible(&id, visible),
                Change::Enabled(id, enabled) => item.set_enabled(&id, enabled),
                Change::Icon(icon, template) => item.set_icon(icon, template),
                Change::Tooltip(text) => item.set_tooltip(&text),
            }
        }
        if message.message == WM_APP {
            continue;
        }
        // SAFETY: `message` was just filled in by GetMessageW.
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

/// Waits `duration`, answering this thread's window messages meanwhile.
/// winit keeps a visible event-target window on the main thread between
/// windows, and Windows calls a thread that leaves it unanswered for five
/// seconds hung.
pub(crate) fn idle(duration: Duration) {
    let deadline = std::time::Instant::now() + duration;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        // Rounded up, so a sub-millisecond rest waits rather than spins.
        let millis = u32::try_from(left.as_micros().div_ceil(1000)).unwrap_or(u32::MAX - 1);
        // SAFETY: no handles; the call returns on a message or the timeout.
        unsafe {
            MsgWaitForMultipleObjectsEx(
                0,
                std::ptr::null(),
                millis,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            );
        }
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
}

#[cfg(test)]
mod idle_tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::PM_NOREMOVE;

    fn peek(first: u32, remove: bool) -> Option<MSG> {
        // SAFETY: an all-zero MSG is a valid value for PeekMessageW to fill.
        let mut message: MSG = unsafe { std::mem::zeroed() };
        let flags = if remove { PM_REMOVE } else { PM_NOREMOVE };
        // SAFETY: `message` outlives the call; messages `first` alone.
        let found =
            unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), first, first, flags) };
        (found != 0).then_some(message)
    }

    /// Idling answers the thread's messages, so Windows never calls an app
    /// in the tray hung, and leaves a quit for the next window's loop.
    #[test]
    fn idling_answers_the_threads_messages_and_keeps_a_quit() {
        // Gives the thread its message queue.
        assert!(peek(WM_APP + 2, false).is_none());
        // SAFETY: posts a message with no data to this thread.
        assert_ne!(
            unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_APP + 2, 0, 0) },
            0
        );
        idle(Duration::from_millis(50));
        assert!(peek(WM_APP + 2, false).is_none(), "answered");
        // SAFETY: posts WM_QUIT to this thread.
        unsafe { PostQuitMessage(3) };
        idle(Duration::from_millis(50));
        assert_eq!(peek(WM_QUIT, true).expect("the quit is kept").wParam, 3);
    }
}
