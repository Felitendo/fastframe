//! Opens the default input and shows what it captures, without keeping any
//! of it: `cargo run -p fastframe-audio --example capture`. The samples are
//! only measured (how many, and the loudest) and then dropped; nothing is
//! saved or sent.
//!
//! It captures for a second, pauses for a second to show that a paused input
//! gets no callbacks, and captures for another second. `--device <id>` opens
//! an input by an id from the `devices` example instead of the default,
//! `--name <name>` by its name, and `--channels <n>` asks for that many
//! channels instead of the device's own. The loudest sample is shown for
//! each channel (the first eight).

use std::sync::Arc;
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fastframe_audio::{Capture, Device, DeviceId, Input, InputOptions};

#[derive(Default)]
struct Stats {
    calls: AtomicU64,
    samples: AtomicU64,
    /// The channels the capturer was configured with.
    channels: AtomicU16,
    /// The loudest sample of each channel since the last report, as `f32`
    /// bits.
    peaks: [AtomicU32; 8],
}

struct Meter {
    stats: Arc<Stats>,
}

impl Capture for Meter {
    fn configure(&mut self, _sample_rate: u32, channels: u16) {
        self.stats.channels.store(channels, Ordering::Relaxed);
    }

    fn capture(&mut self, input: &[f32]) {
        let channels = usize::from(self.stats.channels.load(Ordering::Relaxed)).max(1);
        for (channel, peak) in self.stats.peaks.iter().enumerate().take(channels) {
            let loudest = input
                .iter()
                .skip(channel)
                .step_by(channels)
                .fold(0.0_f32, |loudest, sample| loudest.max(sample.abs()));
            peak.fetch_max(loudest.to_bits(), Ordering::Relaxed);
        }
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
        .map(|id| Device::Id(DeviceId::new(id.as_str())))
        .or_else(|| {
            args.iter()
                .position(|arg| arg == "--name")
                .and_then(|at| args.get(at + 1))
                .map(|name| Device::Named(name.clone()))
        })
        .unwrap_or(Device::Default);
    let channels = args
        .iter()
        .position(|arg| arg == "--channels")
        .and_then(|at| args.get(at + 1))
        .and_then(|count| count.parse().ok())
        .unwrap_or(0);

    let stats = Arc::new(Stats::default());
    let meter = Meter {
        stats: Arc::clone(&stats),
    };
    let opening = Instant::now();
    let mut input = Input::open(
        InputOptions {
            device,
            channels,
            ..InputOptions::default()
        },
        meter,
    )?;
    let opened = opening.elapsed();
    let clock = input.clock();
    println!("opened in {opened:?}");
    println!(
        "{} ({}) at {} Hz, {} channels, through {}",
        input.device_name(),
        input.device_id(),
        input.sample_rate(),
        input.channels(),
        fastframe_audio::host()
    );

    let report = |what: &str| {
        for peak in &stats.peaks {
            peak.store(0, Ordering::Relaxed);
        }
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
        let channels = usize::from(stats.channels.load(Ordering::Relaxed)).clamp(1, 8);
        let level = stats.peaks[..channels]
            .iter()
            .map(|peak| {
                let peak = f32::from_bits(peak.load(Ordering::Relaxed));
                if peak > 0.0 {
                    format!("{:.1} dBFS", 20.0 * peak.log10())
                } else {
                    "silence".to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" | ");
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
