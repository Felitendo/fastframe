//! Opens the default output with a renderer that writes silence, and shows
//! that a paused output gets no callbacks: `cargo run -p fastframe-audio
//! --example idle`. Nothing is audible.
//!
//! By default it opens the output as Solco does (512 frames, rendered in
//! blocks of at most 512). `--buffer-ms 100` asks for a fixed 100 ms buffer
//! instead, as Spotifast does on Windows, without blocks, so the largest
//! callback shows the buffer the device gave. `--device <id>` opens a
//! device by an id from the `devices` example instead of the default.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fastframe_audio::{Buffer, BufferSize, Device, DeviceId, Output, OutputOptions, Render};

#[derive(Default)]
struct Stats {
    calls: AtomicU64,
    /// The most frames one render call was asked for.
    max_frames: AtomicU64,
}

struct Silence {
    stats: Arc<Stats>,
    channels: u64,
}

impl Render for Silence {
    fn configure(&mut self, _sample_rate: u32, channels: u16) {
        self.channels = u64::from(channels.max(1));
    }
    fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let frames = out.len() as u64 / self.channels;
        self.stats.max_frames.fetch_max(frames, Ordering::Relaxed);
        self.stats.calls.fetch_add(1, Ordering::Relaxed);
    }
}

#[allow(clippy::print_stdout)]
fn main() -> Result<(), fastframe_audio::OpenError> {
    let args: Vec<String> = std::env::args().collect();
    let buffer_ms = args
        .iter()
        .position(|arg| arg == "--buffer-ms")
        .and_then(|at| args.get(at + 1))
        .and_then(|ms| ms.parse::<u64>().ok());
    let device = args
        .iter()
        .position(|arg| arg == "--device")
        .and_then(|at| args.get(at + 1))
        .map_or(Device::Default, |id| Device::Id(DeviceId::new(id.as_str())));
    let options = match buffer_ms {
        Some(ms) => OutputOptions {
            device,
            buffer: Buffer::Fixed(BufferSize::Duration(Duration::from_millis(ms))),
            ..OutputOptions::default()
        },
        None => OutputOptions {
            device,
            buffer: Buffer::Fixed(BufferSize::Frames(512)),
            max_block_frames: Some(512),
            ..OutputOptions::default()
        },
    };

    let stats = Arc::new(Stats::default());
    let renderer = Silence {
        stats: Arc::clone(&stats),
        channels: 1,
    };
    let mut output = Output::open(options, renderer)?;
    let clock = output.clock();
    println!(
        "{} ({}) at {} Hz, {} channels, through {}",
        output.device_name(),
        output.device_id(),
        output.sample_rate(),
        output.channels(),
        fastframe_audio::host()
    );

    let report = |what: &str| {
        stats.max_frames.store(0, Ordering::Relaxed);
        let before = (stats.calls.load(Ordering::Relaxed), clock.played());
        std::thread::sleep(Duration::from_secs(1));
        let after = (stats.calls.load(Ordering::Relaxed), clock.played());
        println!(
            "{what}: {} render calls of at most {} frames, played {:?} -> {:?}, latency {:?}",
            after.0 - before.0,
            stats.max_frames.load(Ordering::Relaxed),
            before.1,
            after.1,
            clock.latency()
        );
    };
    report("playing");
    output.pause();
    std::thread::sleep(Duration::from_millis(200));
    report("paused");

    let calls = stats.calls.load(Ordering::Relaxed);
    let start = Instant::now();
    output.resume();
    let resumed = start.elapsed();
    while stats.calls.load(Ordering::Relaxed) == calls && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_micros(200));
    }
    println!(
        "resume() took {resumed:?}; the first callback came {:?} after it",
        start.elapsed()
    );
    report("resumed");
    println!(
        "maintain: {:?}, errors: {:?}",
        output.maintain(),
        output.take_errors()
    );
    Ok(())
}
