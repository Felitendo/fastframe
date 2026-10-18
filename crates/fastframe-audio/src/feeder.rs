//! Precedence for the thread that feeds an output: the one that decodes and
//! queues what the renderer plays.
//!
//! The output's callback runs ahead of ordinary work: on CoreAudio's
//! real-time I/O thread on macOS, on the WASAPI thread in MMCSS on Windows
//! (see `mmcss.rs`), and on Linux on cpal's own thread, which the output asks
//! RTKit to make real-time (see `realtime.rs`; cpal itself leaves it at
//! ordinary priority). The thread that fills the renderer's queue does not. It runs as any thread
//! does, so a busy or swapping machine can keep it waiting longer than the
//! queue lasts, and the callback then plays silence: crackles and gaps that
//! the device never reports (crmne/spotifast#785, #88). Solco raises its
//! audition decoder the same way (its ADR 0190); this is that policy:
//!
//! - macOS: the user-interactive QoS class, the class of the frontmost
//!   app's main thread, ahead of default, utility and background work.
//! - Windows: one step above normal priority.
//! - Linux: nice -5, which an unprivileged process is usually refused; the
//!   thread then stays as it was.
#![allow(unsafe_code)]

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

thread_local! {
    /// Whether this thread has asked already, and what it was told.
    static ASKED: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Gives the calling thread precedence over ordinary work, for a thread that
/// decodes and queues sound for an [`Output`](crate::Output). Call it on that
/// thread, before or as it starts feeding; later calls on the same thread do
/// nothing. Returns whether the system granted it.
///
/// Best effort: a refusal is logged once, at debug level, and the thread
/// carries on as it was. Do not call it from a thread pool shared with other
/// work, which would all get the same precedence.
pub fn raise_feeder_priority() -> bool {
    ASKED.with(|asked| {
        if let Some(granted) = asked.get() {
            return granted;
        }
        let granted = match os::raise() {
            Ok(()) => true,
            Err(error) => {
                static LOGGED: AtomicBool = AtomicBool::new(false);
                if !LOGGED.swap(true, Ordering::Relaxed) {
                    log::debug!("audio feeder: thread priority not raised: {error}");
                }
                false
            }
        };
        asked.set(Some(granted));
        granted
    })
}

#[cfg(target_os = "macos")]
mod os {
    pub(super) fn raise() -> std::io::Result<()> {
        // SAFETY: changes only the calling thread's own QoS class, with a
        // valid class and a relative priority of zero, as the call allows.
        let result = unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
        };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::from_raw_os_error(result))
        }
    }

    /// The calling thread's QoS class, for the tests.
    #[cfg(test)]
    pub(super) fn current() -> libc::qos_class_t {
        let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
        let mut relative = 0;
        // SAFETY: reads the calling thread's own class into valid places.
        unsafe {
            libc::pthread_get_qos_class_np(libc::pthread_self(), &mut class, &mut relative);
        }
        class
    }
}

#[cfg(target_os = "windows")]
mod os {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
    };

    pub(super) fn raise() -> std::io::Result<()> {
        // SAFETY: the current thread's pseudo-handle needs no closing, and
        // the call changes only that thread.
        if unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// The calling thread's priority, for the tests.
    #[cfg(test)]
    pub(super) fn current() -> i32 {
        // SAFETY: reads the current thread's priority through its
        // pseudo-handle.
        unsafe { windows_sys::Win32::System::Threading::GetThreadPriority(GetCurrentThread()) }
    }
}

#[cfg(target_os = "linux")]
mod os {
    /// Ahead of ordinary threads, well short of what a real-time audio
    /// thread gets.
    const NICE: libc::c_int = -5;

    pub(super) fn raise() -> std::io::Result<()> {
        // SAFETY: `gettid` takes nothing; with a thread id, PRIO_PROCESS
        // changes only that thread on Linux.
        let result =
            unsafe { libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, NICE) };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod os {
    pub(super) fn raise() -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_asks_once_and_keeps_the_answer() {
        std::thread::spawn(|| {
            assert_eq!(ASKED.get(), None);
            let granted = raise_feeder_priority();
            assert_eq!(ASKED.get(), Some(granted));
            assert_eq!(
                raise_feeder_priority(),
                granted,
                "later calls do not ask again"
            );
        })
        .join()
        .expect("the thread raises its priority without panicking");
    }

    #[test]
    fn only_the_calling_thread_is_raised() {
        let other = std::thread::spawn(|| {
            raise_feeder_priority();
        });
        other.join().expect("the other thread raises its own");
        assert_eq!(ASKED.get(), None, "this thread has not asked");
    }

    /// Any app may ask for the user-interactive class, so macOS grants it.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_feeds_from_the_user_interactive_class() {
        std::thread::spawn(|| {
            assert!(raise_feeder_priority());
            // qos_class_t has no PartialEq.
            assert!(matches!(
                os::current(),
                libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE
            ));
        })
        .join()
        .expect("the class is set");
    }

    /// Any process may raise a thread to above normal.
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_feeds_from_above_normal() {
        std::thread::spawn(|| {
            assert!(raise_feeder_priority());
            assert_eq!(
                os::current(),
                windows_sys::Win32::System::Threading::THREAD_PRIORITY_ABOVE_NORMAL
            );
        })
        .join()
        .expect("the priority is set");
    }
}
