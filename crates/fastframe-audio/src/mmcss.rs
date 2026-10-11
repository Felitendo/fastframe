//! Windows: has the stream's thread join the Multimedia Class Scheduler
//! Service as an "Audio" task.
//!
//! cpal's WASAPI thread otherwise runs at normal priority, the same as any
//! background work, and Windows' power throttling (EcoQoS, or the Task
//! Manager's efficiency mode) lowers it further. MMCSS keeps the thread that
//! feeds the device ahead of that work, which is what Microsoft asks of the
//! thread rendering a WASAPI stream.
//! <https://learn.microsoft.com/en-us/windows/win32/procthread/multimedia-class-scheduler-service>
#![allow(unsafe_code)]

use std::cell::OnceCell;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
};

/// A thread's place in MMCSS, which it leaves when it exits.
struct Task(HANDLE);

impl Drop for Task {
    fn drop(&mut self) {
        // SAFETY: the handle came from `AvSetMmThreadCharacteristicsW` on
        // this thread, which is where its thread-local owner is dropped, and
        // it is reverted only here, once.
        if unsafe { AvRevertMmThreadCharacteristics(self.0) } == 0 {
            log::debug!(
                "audio output: the stream thread could not leave MMCSS: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

thread_local! {
    /// This thread's MMCSS task, set by its first callback so each stream
    /// thread asks once. `None` if Windows refused.
    static TASK: OnceCell<Option<Task>> = const { OnceCell::new() };
}

/// Joins MMCSS as an "Audio" task, once per thread, and leaves it when the
/// thread exits. A refusal is logged and the stream plays on at normal
/// priority.
pub(crate) fn join_once() {
    TASK.with(|task| {
        task.get_or_init(join);
    });
}

fn join() -> Option<Task> {
    let name: Vec<u16> = "Audio".encode_utf16().chain([0]).collect();
    let mut index = 0u32;
    // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the
    // call, and `index` is a valid place for the task index.
    let handle = unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut index) };
    if handle.is_null() {
        log::debug!(
            "audio output: the stream thread could not join MMCSS: {}",
            std::io::Error::last_os_error()
        );
        return None;
    }
    Some(Task(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked() -> Option<Option<HANDLE>> {
        TASK.with(|task| task.get().map(|joined| joined.as_ref().map(|task| task.0)))
    }

    #[test]
    fn a_thread_asks_once_and_leaves_as_it_exits() {
        std::thread::spawn(|| {
            assert_eq!(asked(), None);
            join_once();
            let first = asked();
            assert!(first.is_some(), "the first callback asks to join");
            join_once();
            assert_eq!(asked(), first, "later callbacks do not ask again");
        })
        .join()
        .expect("the thread leaves MMCSS as it exits");
    }
}
