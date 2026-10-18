//! A [`Render`] that plays rodio's mixer, with the `rodio` feature: the app
//! adds rodio sources to the mixer, and the output plays what it mixes.

use std::sync::{Arc, Mutex, PoisonError};

use crate::Render;

/// Plays a rodio mixer, made at the output's rate and channel count.
///
/// The output configures it on the thread that opens the stream. A stream in
/// the format the mixer already has keeps it, so a reopen on another device
/// carries on from the same sample; another rate or channel count gets a new
/// mixer, which the app takes from the [`MixerSlot`] and adds its sources to
/// from then on. Until the first mixer is made it plays silence. A sound
/// added to the old mixer just before such a reopen is not played; take the
/// new mixer after each `Reopened`.
///
/// It is [idle](Render::idle) while the mixer has nothing to play, so with
/// [`OutputOptions::pause_when_idle`](crate::OutputOptions::pause_when_idle)
/// the output pauses between sounds; call
/// [`Output::resume`](crate::Output::resume) after adding one.
///
/// ```no_run
/// use fastframe_audio::{MixerRender, Output, OutputOptions};
///
/// let (render, slot) = MixerRender::new();
/// let mut output = Output::open(OutputOptions::default(), render)?;
/// let mut mixer = None;
/// // Now and then, and after each `maintain` that reopened:
/// if let Some(made) = slot.take() {
///     mixer = Some(made.mixer); // the old one's sources are gone with it
/// }
/// if let Some(mixer) = &mixer {
///     mixer.add(rodio::source::SineWave::new(440.0));
///     output.resume();
/// }
/// # Ok::<(), fastframe_audio::OpenError>(())
/// ```
pub struct MixerRender {
    source: Option<rodio::mixer::MixerSource>,
    format: (u32, u16),
    slot: MixerSlot,
    idle: bool,
}

impl std::fmt::Debug for MixerRender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MixerRender")
            .field("format", &self.format)
            .field("mixing", &self.source.is_some())
            .field("idle", &self.idle)
            .finish_non_exhaustive()
    }
}

/// A mixer [`MixerRender`] made for a new stream format.
#[derive(Clone)]
pub struct NewMixer {
    /// The mixer to add sources to. Sources added to an earlier one no
    /// longer play.
    pub mixer: rodio::mixer::Mixer,
    /// The rate it mixes at, the stream's.
    pub sample_rate: u32,
    /// The channels it mixes, the stream's.
    pub channels: u16,
}

impl std::fmt::Debug for NewMixer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewMixer")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

/// Where [`MixerRender`] leaves each mixer it makes, for the app to take.
/// Cloning it shares the slot. The audio thread never touches it.
#[derive(Clone, Debug, Default)]
pub struct MixerSlot(Arc<Mutex<Option<NewMixer>>>);

impl MixerSlot {
    /// The mixer made since the last call, if any. Only the latest is kept.
    #[must_use]
    pub fn take(&self) -> Option<NewMixer> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    fn put(&self, made: NewMixer) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(made);
    }
}

impl MixerRender {
    /// A renderer with no mixer yet, and the slot its mixers arrive in.
    #[must_use]
    pub fn new() -> (Self, MixerSlot) {
        let slot = MixerSlot::default();
        let render = Self {
            source: None,
            format: (0, 0),
            slot: slot.clone(),
            idle: true,
        };
        (render, slot)
    }
}

impl Render for MixerRender {
    fn configure(&mut self, sample_rate: u32, channels: u16) {
        if self.source.is_some() && self.format == (sample_rate, channels) {
            return;
        }
        let (mixer, source) = rodio::mixer::mixer(
            rodio::ChannelCount::from(channels),
            rodio::SampleRate::from(sample_rate),
        );
        self.source = Some(source);
        self.format = (sample_rate, channels);
        self.idle = true;
        self.slot.put(NewMixer {
            mixer,
            sample_rate,
            channels,
        });
    }

    fn render(&mut self, out: &mut [f32]) {
        let Some(source) = &mut self.source else {
            out.fill(0.0);
            return;
        };
        // The mixer ends (`None`) while it has no source playing or waiting,
        // and starts again from the next sample once one is added.
        let mut ended = self.idle;
        for sample in out.iter_mut() {
            let next = source.next();
            ended = next.is_none();
            *sample = next.unwrap_or(0.0);
        }
        self.idle = ended;
    }

    fn idle(&self) -> bool {
        self.idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rodio::buffer::SamplesBuffer;

    #[test]
    fn a_new_format_makes_a_new_mixer_and_the_same_one_keeps_it() {
        let (mut render, slot) = MixerRender::new();
        let mut out = [1.0; 4];
        render.render(&mut out);
        assert_eq!(out, [0.0; 4], "silence before the first mixer");
        assert!(slot.take().is_none());

        render.configure(48_000, 2);
        let first = slot.take().expect("a mixer for 48 kHz stereo");
        assert_eq!((first.sample_rate, first.channels), (48_000, 2));
        first
            .mixer
            .add(SamplesBuffer::new(2, 48_000, vec![0.5_f32; 8]));
        render.render(&mut out);
        assert_eq!(out, [0.5; 4]);

        // A reopen in the same format keeps the mixer and what it plays.
        render.configure(48_000, 2);
        assert!(slot.take().is_none(), "no new mixer");
        render.render(&mut out);
        assert_eq!(out, [0.5; 4], "the same sound carries on");

        // Another rate gets a new mixer; the old one's sound is gone.
        render.configure(44_100, 1);
        let second = slot.take().expect("a mixer for 44.1 kHz mono");
        assert_eq!((second.sample_rate, second.channels), (44_100, 1));
        render.render(&mut out);
        assert_eq!(out, [0.0; 4]);
        second
            .mixer
            .add(SamplesBuffer::new(1, 44_100, vec![0.25_f32; 2]));
        render.render(&mut out);
        assert_eq!(out, [0.25, 0.25, 0.0, 0.0]);
    }

    #[test]
    fn it_is_idle_while_the_mixer_has_nothing_to_play() {
        let (mut render, slot) = MixerRender::new();
        assert!(render.idle(), "nothing to play before the first mixer");
        render.configure(48_000, 1);
        let mixer = slot.take().expect("a mixer").mixer;
        let mut out = [0.0; 4];
        render.render(&mut out);
        assert!(render.idle());

        // A sound longer than the block: playing.
        mixer.add(SamplesBuffer::new(1, 48_000, vec![0.5_f32; 6]));
        render.render(&mut out);
        assert!(!render.idle());
        // It ends within the next block: idle once it has.
        render.render(&mut out);
        assert_eq!(out, [0.5, 0.5, 0.0, 0.0]);
        assert!(render.idle());

        // A sound queued while idle plays from the next block.
        mixer.add(SamplesBuffer::new(1, 48_000, vec![0.5_f32; 8]));
        render.render(&mut out);
        assert_eq!(out, [0.5; 4]);
        assert!(!render.idle());
    }
}
