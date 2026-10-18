//! Opens the default input and shows what it captures, without keeping any
//! of it: `cargo run -p fastframe-audio --example capture`. The samples are
//! only measured (how many, and the loudest) and then dropped; nothing is
//! saved or sent.
//!
//! It captures for a second, pauses for a second to show that a paused input
//! gets no callbacks, and captures for another second. `--device <id>` opens
//! an input by an id from the `devices` example instead of the default.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use fastframe_audio::{Capture, Device, DeviceId, Input, InputOptions};

#[derive(Default)]
struct Stats {
    calls: AtomicU64,
    samples: AtomicU64,
    /// The loudest sample since the last report, as `f32` bits.
    peak: AtomicU32,
}

struct Meter {
    stats: Arc<Stats>,
}

impl Capture for Meter {
    fn configure(&mut self, _sample_rate: u32, _channels: u16) {}

    fn capture(&mut self, input: &[f32]) {
        let peak = input
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        self.stats.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
        self.stats
            .samples
            .fetch_add(input.len() as u64, Ordering::Relaxed);
        self.stats.calls.fetch_add(1, Ordering::Relaxed);
    }
}

#[allow(clippy::print_stdout)]
fn main() -> Result<(), fastframe_audio::OpenError> {
    let args: Vec<String> = std::env::args().collect();
    let device = args
        .iter()
        .position(|arg| arg == "--device")
        .and_then(|at| args.get(at + 1))
        .map_or(Device::Default, |id| Device::Id(DeviceId::new(id.as_str())));

    let stats = Arc::new(Stats::default());
    let meter = Meter {
        stats: Arc::clone(&stats),
    };
    let mut input = Input::open(
        InputOptions {
            device,
            ..InputOptions::default()
        },
        meter,
    )?;
    let clock = input.clock();
    println!(
        "{} ({}) at {} Hz, {} channels, through {}",
        input.device_name(),
        input.device_id(),
        input.sample_rate(),
        input.channels(),
        fastframe_audio::host()
    );

    let report = |what: &str| {
        stats.peak.store(0, Ordering::Relaxed);
        let before = (
            stats.calls.load(Ordering::Relaxed),
            stats.samples.load(Ordering::Relaxed),
            clock.played(),
        );
        std::thread::sleep(Duration::from_secs(1));
        let after = (
            stats.calls.load(Ordering::Relaxed),
            stats.samples.load(Ordering::Relaxed),
            clock.played(),
        );
        let peak = f32::from_bits(stats.peak.load(Ordering::Relaxed));
        let level = if peak > 0.0 {
            format!("{:.1} dBFS", 20.0 * peak.log10())
        } else {
            "silence".to_owned()
        };
        println!(
            "{what}: {} callbacks, {} samples, loudest {level}, captured {:?} -> {:?}, latency {:?}",
            after.0 - before.0,
            after.1 - before.1,
            before.2,
            after.2,
            clock.latency()
        );
    };
    report("capturing");
    input.pause();
    std::thread::sleep(Duration::from_millis(200));
    report("paused");
    input.resume();
    report("resumed");
    println!(
        "maintain: {:?}, errors: {:?}, overruns: {}",
        input.maintain(),
        input.take_errors(),
        clock.underruns()
    );
    Ok(())
}
