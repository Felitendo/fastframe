//! Plays two short, quiet tones through rodio's mixer, pausing the output
//! between them: `cargo run -p fastframe-audio --features rodio --example
//! mixer`. Each tone is a quarter of a second of 440 Hz at -40 dBFS.

use std::time::{Duration, Instant};

use fastframe_audio::{MixerRender, Output, OutputOptions};
use rodio::Source as _;

#[allow(clippy::print_stdout)]
fn main() -> Result<(), fastframe_audio::OpenError> {
    let (render, slot) = MixerRender::new();
    let options = OutputOptions {
        pause_when_idle: Some(Duration::from_millis(300)),
        ..OutputOptions::default()
    };
    let mut output = Output::open(options, render)?;
    let made = slot.take().expect("opening made a mixer");
    println!(
        "{} at {} Hz, {} channels; mixing at {} Hz, {} channels",
        output.device_name(),
        output.sample_rate(),
        output.channels(),
        made.sample_rate,
        made.channels
    );
    let tone = || {
        rodio::source::SineWave::new(440.0)
            .amplify(0.01)
            .take_duration(Duration::from_millis(250))
    };
    for round in 1..=2 {
        made.mixer.add(tone());
        output.resume();
        let start = Instant::now();
        while !output.is_paused() && start.elapsed() < Duration::from_secs(3) {
            let _ = output.maintain();
            std::thread::sleep(Duration::from_millis(20));
        }
        println!(
            "tone {round}: paused {} after {:?}, played {:?}",
            output.is_paused(),
            start.elapsed(),
            output.clock().played()
        );
    }
    Ok(())
}
