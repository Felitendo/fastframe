//! Real-time scheduling for the thread the output's callback runs on, on
//! Linux.
//!
//! On macOS the callback runs on CoreAudio's real-time I/O thread, and on
//! Windows the output joins MMCSS (`mmcss.rs`). On Linux cpal calls back from
//! a thread of its own (PipeWire's main loop, the PulseAudio or ALSA
//! worker) with ordinary priority, unless built with its `realtime-dbus`
//! feature, which links the C libdbus. With a 512-frame buffer the callback
//! has about 10 ms each period; on a busy machine an ordinary thread waits
//! longer than that, and the device plays silence (crmne/spotifast#785).
//!
//! So on the first callback of each stream the output hands that thread's
//! id to a short-lived helper, which asks RTKit (`org.freedesktop.RealtimeKit1`
//! on the system bus, which PipeWire uses for its own threads) to make it
//! real-time, or failing that to raise its nice level. The callback itself
//! never waits for D-Bus. Best effort: without RTKit (a minimal system, a
//! Flatpak sandbox without the portal) or when RTKit refuses, the outcome is
//! logged once and the thread carries on as it was.
#![allow(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

/// The real-time priority asked for, within RTKit's maximum: below the
/// PipeWire daemon's own threads, as for Firefox and cpal.
const PRIORITY: u32 = 10;
/// The nice level asked for when real-time is refused, within RTKit's
/// minimum: PipeWire's own fallback.
const NICE: i32 = -11;

/// Hands the callback thread to the helper once.
pub(crate) struct Promoter(Option<SyncSender<libc::pid_t>>);

impl Promoter {
    /// Starts the helper, which waits for the callback's first call and ends
    /// with the stream if that never comes.
    pub(crate) fn start() -> Self {
        let (sender, receiver) = sync_channel(1);
        let started = std::thread::Builder::new()
            .name("audio-realtime".into())
            .spawn(move || promote_from(&receiver));
        match started {
            Ok(_) => Self(Some(sender)),
            Err(error) => {
                log::debug!("audio output: cannot start the real-time helper: {error}");
                Self(None)
            }
        }
    }

    /// Called on every callback; only the first sends the thread's id. A
    /// system call and a send on a channel with room, so it never blocks.
    pub(crate) fn on_callback(&mut self) {
        if let Some(sender) = self.0.take() {
            // SAFETY: `gettid` takes nothing and returns the calling thread's id.
            let thread = unsafe { libc::gettid() };
            let _ = sender.try_send(thread);
        }
    }
}

fn promote_from(receiver: &Receiver<libc::pid_t>) {
    let Ok(thread) = receiver.recv() else {
        return;
    };
    if already_real_time(thread) {
        return;
    }
    let outcome = rtkit::promote(thread, PRIORITY, NICE);
    static REPORTED: AtomicBool = AtomicBool::new(false);
    let first = !REPORTED.swap(true, Ordering::Relaxed);
    match outcome {
        Ok(Promoted::RealTime(priority)) => {
            if first {
                log::info!("audio output: callback thread real-time at priority {priority}");
            }
        }
        Ok(Promoted::Nice(nice)) => {
            if first {
                log::info!("audio output: callback thread not real-time; raised to nice {nice}");
            }
        }
        Err(error) => {
            if first {
                log::info!("audio output: callback thread keeps ordinary priority: {error}");
            }
        }
    }
}

/// What RTKit granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Promoted {
    RealTime(u32),
    Nice(i32),
}

/// Whether the thread already runs under a real-time policy (a JACK
/// callback, say), which needs nothing more.
fn already_real_time(thread: libc::pid_t) -> bool {
    // SAFETY: reads the scheduling policy of a thread of this process.
    let policy = unsafe { libc::sched_getscheduler(thread) };
    policy == libc::SCHED_FIFO || policy == libc::SCHED_RR
}

/// The `RLIMIT_RTTIME` RTKit requires before it grants real-time: the
/// process's limit, lowered to at most `max` microseconds. `None` when the
/// current one already fits.
pub(crate) fn rttime_limit(current: (u64, u64), max: u64) -> Option<(u64, u64)> {
    let (soft, hard) = current;
    (hard > max || soft > max).then(|| (soft.min(max), hard.min(max)))
}

/// The priority to ask for: ours, within RTKit's maximum, at least 1.
pub(crate) fn priority_within(wanted: u32, max: i32) -> u32 {
    wanted.min(u32::try_from(max).unwrap_or(0)).max(1)
}

mod rtkit {
    use super::{Promoted, priority_within, rttime_limit};

    const SERVICE: &str = "org.freedesktop.RealtimeKit1";
    const PATH: &str = "/org/freedesktop/RealtimeKit1";

    pub(super) fn promote(
        thread: libc::pid_t,
        priority: u32,
        nice: i32,
    ) -> Result<Promoted, String> {
        let connection = zbus::blocking::Connection::system()
            .map_err(|error| format!("no system bus: {error}"))?;
        let proxy = zbus::blocking::Proxy::new(&connection, SERVICE, PATH, SERVICE)
            .map_err(|error| format!("no RTKit: {error}"))?;
        let thread = u64::try_from(thread).map_err(|_| "a negative thread id".to_owned())?;
        let real_time = (|| {
            let max_priority: i32 = proxy
                .get_property("MaxRealtimePriority")
                .map_err(|error| format!("RTKit: {error}"))?;
            let max_rttime: i64 = proxy
                .get_property("RTTimeUSecMax")
                .map_err(|error| format!("RTKit: {error}"))?;
            limit_rttime(u64::try_from(max_rttime).unwrap_or(0))?;
            let priority = priority_within(priority, max_priority);
            proxy
                .call_method("MakeThreadRealtime", &(thread, priority))
                .map_err(|error| format!("RTKit refused real-time: {error}"))?;
            Ok::<_, String>(priority)
        })();
        match real_time {
            Ok(priority) => Ok(Promoted::RealTime(priority)),
            Err(refused) => {
                let min_nice: i32 = proxy.get_property("MinNiceLevel").unwrap_or(nice);
                let nice = nice.max(min_nice);
                proxy
                    .call_method("MakeThreadHighPriority", &(thread, nice))
                    .map_err(|error| format!("{refused}; and a higher priority: {error}"))?;
                Ok(Promoted::Nice(nice))
            }
        }
    }

    /// RTKit grants real-time only to a process whose `RLIMIT_RTTIME` is at
    /// most its own maximum, so that a runaway real-time thread is stopped
    /// rather than freezing the machine. PipeWire's clients set it too.
    fn limit_rttime(max: u64) -> Result<(), String> {
        let mut current = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: reads this process's limit into a valid struct.
        if unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut current) } != 0 {
            return Err(format!(
                "RLIMIT_RTTIME: {}",
                std::io::Error::last_os_error()
            ));
        }
        let Some((soft, hard)) = rttime_limit((current.rlim_cur, current.rlim_max), max) else {
            return Ok(());
        };
        let limit = libc::rlimit {
            rlim_cur: soft,
            rlim_max: hard,
        };
        // SAFETY: lowers this process's own limit, which needs no privilege.
        if unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &limit) } != 0 {
            return Err(format!(
                "RLIMIT_RTTIME: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rttime_limit_comes_down_to_what_rtkit_allows() {
        // Unlimited, as a process starts: both come down to RTKit's 200 ms.
        assert_eq!(
            rttime_limit((u64::MAX, u64::MAX), 200_000),
            Some((200_000, 200_000))
        );
        // Already within it (PipeWire's own client set it): left alone.
        assert_eq!(rttime_limit((100_000, 200_000), 200_000), None);
        assert_eq!(
            rttime_limit((50_000, 400_000), 200_000),
            Some((50_000, 200_000))
        );
    }

    #[test]
    fn the_priority_stays_within_rtkits_maximum() {
        assert_eq!(priority_within(10, 20), 10);
        assert_eq!(priority_within(10, 5), 5);
        assert_eq!(priority_within(10, 0), 1);
        assert_eq!(priority_within(10, -1), 1);
    }

    #[test]
    fn a_stream_that_never_calls_back_lets_its_helper_go() {
        // The helper waits on the channel; dropping the promoter ends it.
        let promoter = Promoter::start();
        drop(promoter);
    }
}
