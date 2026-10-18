//! How long an output has played or an input has captured, shared between
//! the audio callback (which counts frames) and any thread that asks.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

/// How long an [`Output`](crate::Output) has played, or an
/// [`Input`](crate::Input) has captured, readable from any thread without
/// blocking the audio callback.
///
/// For an output, [`played`](Self::played) counts what has reached the
/// device, not what was handed to the renderer: it subtracts the output
/// latency the device reports. For an input it counts what was handed to the
/// capturer. It never goes backwards, stands still while the output is paused,
/// and carries on across reopens (another device, another sample rate)
/// without jumping or starting again from zero. Take differences between two
/// readings to follow a clip.
#[derive(Clone, Debug, Default)]
pub struct Clock(Arc<Shared>);

#[derive(Debug, Default)]
struct Shared {
    /// What earlier streams of this output played, in nanoseconds.
    base: AtomicU64,
    /// Frames rendered by the current stream.
    frames: AtomicU64,
    /// The current stream's sample rate.
    rate: AtomicU32,
    /// The latency the current stream last reported, in nanoseconds.
    latency: AtomicU64,
    /// The largest value handed out, so a reading never goes backwards.
    last: AtomicU64,
    /// Gaps the device played because a callback came too late.
    underruns: AtomicU64,
    /// An input's clock: what was handed over has been captured already, so
    /// the latency is not subtracted.
    capturing: AtomicBool,
}

impl Clock {
    /// How long the output has played, or the input has captured.
    #[must_use]
    pub fn played(&self) -> Duration {
        let shared = &self.0;
        let latency = if shared.capturing.load(Ordering::Relaxed) {
            0
        } else {
            shared.latency.load(Ordering::Acquire)
        };
        let now = played_nanos(
            shared.base.load(Ordering::Acquire),
            shared.frames.load(Ordering::Acquire),
            shared.rate.load(Ordering::Acquire),
            latency,
        );
        let previous = shared.last.fetch_max(now, Ordering::AcqRel);
        Duration::from_nanos(now.max(previous))
    }

    /// For an output, the time between rendering a sample and the device
    /// playing it; for an input, between the device capturing a sample and
    /// the capturer getting it. As the device last reported it. Zero before
    /// the first callback, or where the backend does not say.
    #[must_use]
    pub fn latency(&self) -> Duration {
        Duration::from_nanos(self.0.latency.load(Ordering::Acquire))
    }

    /// How many times the device played silence because the callback came
    /// too late (an underrun), by the device's own timeline of when each
    /// block plays, across pauses and reopens. For an input, how many times
    /// it lost what it captured because the callback came too late (an
    /// overrun), by when each block was captured. A pause is not one.
    /// Backends that give no timestamps count none.
    #[must_use]
    pub fn underruns(&self) -> u64 {
        self.0.underruns.load(Ordering::Relaxed)
    }

    /// The device played a gap before the latest block.
    pub(crate) fn underran(&self) {
        self.0.underruns.fetch_add(1, Ordering::Relaxed);
    }

    /// The callback rendered `frames` more frames, and the device said they
    /// play `latency` after the callback.
    pub(crate) fn rendered(&self, frames: u64, latency: Duration) {
        let latency = u64::try_from(latency.as_nanos()).unwrap_or(u64::MAX);
        self.0.latency.store(latency, Ordering::Release);
        self.0.frames.fetch_add(frames, Ordering::AcqRel);
    }

    /// The callback handed `frames` more captured frames to the capturer,
    /// which the device captured `latency` before the callback.
    pub(crate) fn captured(&self, frames: u64, latency: Duration) {
        self.0.capturing.store(true, Ordering::Relaxed);
        self.rendered(frames, latency);
    }

    /// A new stream starts at `rate`: what has played so far becomes the
    /// base, so the reading carries on from where it was.
    pub(crate) fn restart(&self, rate: u32) {
        let played = u64::try_from(self.played().as_nanos()).unwrap_or(u64::MAX);
        let shared = &self.0;
        shared.base.store(played, Ordering::Release);
        shared.frames.store(0, Ordering::Release);
        shared.latency.store(0, Ordering::Release);
        shared.rate.store(rate, Ordering::Release);
    }
}

/// Nanoseconds played: what earlier streams played, plus the current
/// stream's frames at its rate, less the latency of those still on their
/// way. Never below the base.
pub(crate) fn played_nanos(base: u64, frames: u64, rate: u32, latency: u64) -> u64 {
    if rate == 0 {
        return base;
    }
    let rendered = u128::from(frames) * 1_000_000_000 / u128::from(rate);
    let played = rendered.saturating_sub(u128::from(latency));
    base.saturating_add(u64::try_from(played).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn played_is_frames_at_the_rate_less_the_latency() {
        // One second at 48 kHz, 20 ms still on its way.
        assert_eq!(played_nanos(0, 48_000, 48_000, 20_000_000), 980_000_000);
        // Nothing has reached the device yet.
        assert_eq!(played_nanos(0, 480, 48_000, 20_000_000), 0);
        assert_eq!(played_nanos(5, 0, 0, 0), 5);
    }

    #[test]
    fn a_reopen_at_another_rate_carries_on_from_where_it_was() {
        let clock = Clock::default();
        clock.restart(44_100);
        clock.rendered(44_100, Duration::ZERO);
        assert_eq!(clock.played(), Duration::from_secs(1));
        // The headphones come out: the built-in output runs at 48 kHz.
        clock.restart(48_000);
        assert_eq!(clock.played(), Duration::from_secs(1));
        clock.rendered(24_000, Duration::ZERO);
        assert_eq!(clock.played(), Duration::from_millis(1500));
    }

    #[test]
    fn an_inputs_clock_counts_what_was_captured() {
        let clock = Clock::default();
        clock.restart(48_000);
        clock.captured(48_000, Duration::from_millis(5));
        // Already captured: the delay is not taken off.
        assert_eq!(clock.played(), Duration::from_secs(1));
        assert_eq!(clock.latency(), Duration::from_millis(5));
    }

    #[test]
    fn the_reading_never_goes_backwards_and_stands_still_without_callbacks() {
        let clock = Clock::default();
        clock.restart(48_000);
        clock.rendered(48_000, Duration::ZERO);
        let before = clock.played();
        // A larger latency reported later must not take the reading back.
        clock.rendered(0, Duration::from_millis(50));
        assert_eq!(clock.played(), before);
        // Paused: no callbacks, no change.
        assert_eq!(clock.played(), before);
        assert_eq!(clock.latency(), Duration::from_millis(50));
    }
}
