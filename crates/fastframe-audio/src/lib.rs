//! Audio output for egui apps: one device stream that costs nothing while
//! paused, follows the default output, and reopens after a failure, with the
//! app's own renderer (a rodio mixer, a kira renderer, a decoder) filling it.
//!
//! ```no_run
//! use fastframe_audio::{Output, OutputOptions, Render};
//!
//! struct Tone { phase: f32, step: f32, channels: usize }
//!
//! impl Render for Tone {
//!     fn configure(&mut self, sample_rate: u32, channels: u16) {
//!         self.step = 440.0 * std::f32::consts::TAU / sample_rate as f32;
//!         self.channels = usize::from(channels);
//!     }
//!     fn render(&mut self, out: &mut [f32]) {
//!         for frame in out.chunks_mut(self.channels) {
//!             self.phase = (self.phase + self.step) % std::f32::consts::TAU;
//!             frame.fill(self.phase.sin() * 0.1);
//!         }
//!     }
//! }
//!
//! // Opening asks the device, which can take a moment: off the UI thread.
//! let mut output = Output::open(
//!     OutputOptions::default(),
//!     Tone { phase: 0.0, step: 0.0, channels: 2 },
//! )?;
//! let clock = output.clock(); // for any thread
//!
//! // On the thread that decodes and queues what the renderer plays, so a
//! // busy machine does not keep it waiting until the queue runs dry.
//! fastframe_audio::raise_feeder_priority();
//!
//! // Pausing stops the device asking for sound: no callbacks, no CPU.
//! output.pause();
//! output.resume();
//!
//! // Now and then, on the thread that owns the output:
//! match output.maintain() {
//!     fastframe_audio::Maintained::Reopened { sample_rate, .. } => {
//!         // Another device, perhaps another rate: resample for it.
//!         let _ = sample_rate;
//!     }
//!     fastframe_audio::Maintained::Failed(error) => eprintln!("no audio: {error}"),
//!     _ => {}
//! }
//! for error in output.take_errors() {
//!     if error.is_fatal() { /* fail what is playing */ }
//! }
//! let _played = clock.played();
//! # Ok::<(), fastframe_audio::OpenError>(())
//! ```

mod clock;
mod feeder;
#[cfg(target_os = "windows")]
mod mmcss;
mod plan;
#[cfg(target_os = "linux")]
mod realtime;
mod stream;

use std::fmt;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

pub use clock::Clock;
pub use feeder::raise_feeder_priority;

use stream::{DefaultWatch, Live, Shared};

/// What fills the output with sound. It runs on the audio thread.
///
/// The output keeps the same renderer across pauses and reopens, so what is
/// playing carries on from the same sample. The app reaches it through its
/// own handles (rodio's mixer, kira's manager, a channel), never through the
/// output, so nothing the app does can make the callback wait.
pub trait Render: Send + 'static {
    /// The stream's sample rate and channel count. Called before the first
    /// [`render`](Self::render), and again whenever the output opens a stream,
    /// which can be at another rate or channel count after a reopen. It may
    /// be called more than once while a stream is being opened; the last call
    /// is the one that holds.
    fn configure(&mut self, sample_rate: u32, channels: u16);

    /// Fills `out` with interleaved samples, in the channel count from
    /// [`configure`](Self::configure). Write silence (zeros) when there is
    /// nothing to play. Must not block.
    fn render(&mut self, out: &mut [f32]);
}

/// A device's own id, to save in settings and open the same device again
/// later, whatever its name or its place in the list. It names no audio
/// system, so a saved one still reads on another machine, where it simply
/// matches nothing and the default plays.
///
/// What it holds is the system's own id for the device:
///
/// - PipeWire: the node's `node.name`, such as
///   `alsa_output.pci-0000_00_1f.3.analog-stereo`, which is also the sink or
///   source name PulseAudio clients see under `pipewire-pulse`;
/// - PulseAudio: the sink or source name, the same string;
/// - ALSA: the PCM name, such as `hw:CARD=PCH,DEV=0` or `default`;
/// - WASAPI: the endpoint id, `{0.0.0.00000000}.{guid}`;
/// - CoreAudio: the device's UID.
///
/// These stay the same across restarts and reconnections, as long as the
/// device is plugged into the same port (ALSA and PipeWire derive theirs
/// from the card and the port).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(String);

impl DeviceId {
    /// An id read back from settings.
    pub fn new(saved: impl Into<String>) -> Self {
        Self(saved.into())
    }

    /// The id as a string, to save.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which device to open. Only this one is ever opened; other devices are at
/// most listed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Device {
    /// The system's default device.
    #[default]
    Default,
    /// A device by the id [`output_devices`] or [`input_devices`] gives. When
    /// it is not there, the default device plays instead, and a warning is
    /// logged.
    Id(DeviceId),
    /// An output device by the name [`output_device_names`] gives, or any
    /// device by its [`DeviceInfo::name`]. When it is not there, the default
    /// device plays instead, and a warning is logged. Two devices can share a
    /// name; prefer [`Device::Id`].
    Named(String),
}

/// A device a picker can offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Its id, to save and open it by ([`Device::Id`]).
    pub id: DeviceId,
    /// Its name, to show.
    pub name: String,
    /// Whether it is the system's default device just now.
    pub is_default: bool,
}

/// A buffer size, in frames or as a duration at the stream's rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferSize {
    /// Frames, whatever the sample rate.
    Frames(u32),
    /// A duration, turned into frames at the stream's sample rate.
    Duration(Duration),
}

/// The device's buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Buffer {
    /// The driver's own size. On PulseAudio, whose own target is about two
    /// seconds, 20 ms instead.
    #[default]
    Driver,
    /// A fixed size on every platform, asked for as given and clamped to
    /// the range the device reports only if it refuses that. PulseAudio
    /// otherwise targets about two seconds.
    Fixed(BufferSize),
    /// A fixed size on Windows, where shared-mode output underruns with the
    /// driver's; the driver's own elsewhere, as for [`Buffer::Driver`].
    FixedOnWindows(BufferSize),
}

/// How to open an [`Output`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputOptions {
    /// The device.
    pub device: Device,
    /// Channels to ask for; `0` takes the device's own.
    pub channels: u16,
    /// A sample rate to try first; `None` takes the device's default rate.
    pub sample_rate: Option<u32>,
    /// The device's buffer.
    pub buffer: Buffer,
    /// Calls [`Render::render`] in blocks of at most this many frames,
    /// whatever the device's buffer, so a renderer's clock moves in small
    /// steps.
    pub max_block_frames: Option<u32>,
    /// With [`Device::Default`], move to the new default output when it
    /// changes (checked every two seconds on macOS and Windows; PipeWire and
    /// PulseAudio move the stream themselves).
    pub follow_default: bool,
    /// Let the device go after the output has been paused this long; the next
    /// [`Output::resume`] opens it again. The device is let go by
    /// [`Output::maintain`], so an app whose audio thread sleeps while paused
    /// has to call it now and then during the pause (every few seconds is
    /// enough), or the device stays open, still costing nothing.
    pub release_after: Option<Duration>,
}

impl Default for OutputOptions {
    fn default() -> Self {
        Self {
            device: Device::Default,
            channels: 2,
            sample_rate: None,
            buffer: Buffer::Driver,
            max_block_frames: None,
            follow_default: true,
            release_after: Some(Duration::from_secs(5 * 60)),
        }
    }
}

/// How the app should treat an [`OutputError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// What is playing cannot carry on: the audio system is missing, access
    /// was denied, a resource ran out, or the configuration or operation is
    /// not supported. The output still reopens.
    Fatal,
    /// Worth logging: a glitch, a busy or vanished device the output reopens,
    /// a rerouted stream, refused real-time priority, or what the backend
    /// could not classify.
    Recoverable,
}

/// An error from the audio device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputError {
    kind: ErrorKind,
    message: String,
}

impl OutputError {
    fn from_cpal(error: &cpal::Error) -> Self {
        Self {
            kind: plan::kind_of(error.kind()),
            message: error.to_string(),
        }
    }

    /// How to treat it.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Whether what is playing has to stop.
    #[must_use]
    pub fn is_fatal(&self) -> bool {
        self.kind == ErrorKind::Fatal
    }
}

impl fmt::Display for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OutputError {}

/// Why an output could not be opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// There is no output device.
    NoDevice,
    /// The device refused.
    Device(OutputError),
}

impl From<cpal::Error> for OpenError {
    fn from(error: cpal::Error) -> Self {
        Self::Device(OutputError::from_cpal(&error))
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDevice => f.write_str("There is no audio output device."),
            Self::Device(error) => write!(f, "Cannot open the audio output: {error}"),
        }
    }
}

impl std::error::Error for OpenError {}

/// Why the output opened its stream again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The stream failed or its device went away.
    Failed,
    /// The default output changed, and [`OutputOptions::follow_default`] is on.
    DefaultChanged,
    /// [`Output::resume`] took back a device let go after a long pause.
    Resumed,
    /// The stream stopped asking for sound while playing, without reporting
    /// an error: the [`Clock`] stood still for two seconds.
    Stalled,
}

/// What [`Output::maintain`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Maintained {
    /// Nothing changed.
    Unchanged,
    /// A new stream is playing, possibly on another device, at another rate
    /// or with other channels; [`Render::configure`] has been told. The new
    /// device's id is [`Output::device_id`] from now on.
    Reopened {
        /// The device's name.
        device: String,
        /// The new stream's sample rate.
        sample_rate: u32,
        /// The new stream's channel count.
        channels: u16,
        /// Why.
        reason: Reason,
    },
    /// The device was let go: paused past
    /// [`OutputOptions::release_after`], or failed while paused. The next
    /// [`Output::resume`] opens it again.
    Released,
    /// The stream had to be opened again and could not be. Nothing is
    /// playing; the next `maintain` or `resume` tries again.
    Failed(OpenError),
}

/// An audio output: one stream on one device, with the app's renderer.
///
/// Keep one for as long as the app may play. Pausing costs nothing, and
/// [`maintain`](Self::maintain) keeps it on a working device.
pub struct Output<R: Render> {
    options: OutputOptions,
    renderer: Arc<Mutex<R>>,
    live: Option<Live>,
    shared: Arc<Shared>,
    clock: Clock,
    paused: bool,
    paused_at: Option<Instant>,
    /// A reopen [`resume`](Self::resume) made, for the next `maintain` to report.
    resumed: bool,
    /// Whether the clock still moves while playing.
    stall: plan::Stall,
    watch: Option<DefaultWatch>,
    device: String,
    id: DeviceId,
    sample_rate: u32,
    channels: u16,
}

impl<R: Render> fmt::Debug for Output<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Output")
            .field("device", &self.device)
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("paused", &self.paused)
            .field("open", &self.live.is_some())
            .finish_non_exhaustive()
    }
}

impl<R: Render> Output<R> {
    /// Opens the device and starts playing through `renderer`.
    ///
    /// Asking the device can take a perceptible time, so call it off the UI
    /// thread. A failure is not remembered: the next `open` tries again.
    pub fn open(options: OutputOptions, renderer: R) -> Result<Self, OpenError> {
        let renderer = Arc::new(Mutex::new(renderer));
        let shared = Arc::new(Shared::default());
        let clock = Clock::default();
        let live = stream::open(&options, &renderer, &clock, &shared, false)?;
        let watch = (options.follow_default
            && options.device == Device::Default
            && cfg!(any(target_os = "macos", windows)))
        .then(|| DefaultWatch::start(live.default_at_open.clone(), plan::Direction::Output));
        Ok(Self {
            device: live.device.clone(),
            id: live.id.clone(),
            sample_rate: live.sample_rate,
            channels: live.channels,
            options,
            renderer,
            live: Some(live),
            shared,
            clock,
            paused: false,
            paused_at: None,
            resumed: false,
            stall: plan::Stall::new(Duration::ZERO, Instant::now()),
            watch,
        })
    }

    /// Stops the device asking for sound, so the callback and the renderer
    /// cost nothing, and keeps the device open for an instant
    /// [`resume`](Self::resume). Never fails: a stream that cannot pause keeps
    /// running.
    pub fn pause(&mut self) {
        if self.paused {
            return;
        }
        self.paused = true;
        self.paused_at = Some(Instant::now());
        self.shared.epoch.fetch_add(1, Ordering::Relaxed);
        if let Some(live) = &self.live {
            use cpal::traits::StreamTrait;
            if let Err(error) = live.stream.pause() {
                log::warn!("cannot pause the audio output: {error}");
                self.shared.report(error);
            }
        }
    }

    /// Has the device ask for sound again, opening it again if it was let go.
    /// Never fails: a device that will not start is reported by the next
    /// [`maintain`](Self::maintain), as is a reopen, with the new rate.
    pub fn resume(&mut self) {
        if !self.paused {
            return;
        }
        self.paused = false;
        self.paused_at = None;
        self.shared.epoch.fetch_add(1, Ordering::Relaxed);
        match &self.live {
            Some(live) => {
                use cpal::traits::StreamTrait;
                if let Err(error) = live.stream.play() {
                    log::warn!("cannot restart the audio output: {error}");
                    self.shared.report(error);
                }
            }
            None => {
                if self.reopen().is_ok() {
                    self.resumed = true;
                }
            }
        }
    }

    /// Whether the output is paused.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Whether the stream has failed or could not be opened again. Cheap
    /// enough to ask before every write; [`maintain`](Self::maintain) repairs it.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Acquire) || (self.live.is_none() && !self.paused)
    }

    /// Keeps the output on a working device. Call it now and then on the
    /// thread that owns the output (each frame, or before writing): it is
    /// cheap when nothing changed.
    ///
    /// It reopens a stream that failed or whose device went away, moves to a
    /// new default output, and lets the device go after a long pause. A
    /// failed reopen is not remembered: the next call tries again. Nothing
    /// happens between calls, so keep calling it while paused for
    /// [`OutputOptions::release_after`] to take effect.
    pub fn maintain(&mut self) -> Maintained {
        if std::mem::take(&mut self.resumed) {
            return self.reopened(Reason::Resumed);
        }
        if self.paused
            && self.live.is_some()
            && plan::should_release(self.paused_at, Instant::now(), self.options.release_after)
        {
            log::info!(
                "audio output: letting {} go after a long pause",
                self.device
            );
            self.live = None;
            return Maintained::Released;
        }
        let reason = if self.shared.failed.load(Ordering::Acquire) {
            Some(Reason::Failed)
        } else if self.default_changed() {
            Some(Reason::DefaultChanged)
        } else if self.live.is_none() && !self.paused {
            Some(Reason::Failed)
        } else if self.stalled() {
            log::warn!("audio output: {} stopped playing; reopening", self.device);
            Some(Reason::Stalled)
        } else {
            None
        };
        let Some(reason) = reason else {
            return Maintained::Unchanged;
        };
        if self.paused {
            // Nothing is playing: let the device go, and open the right one
            // on resume.
            self.live = None;
            self.shared.failed.store(false, Ordering::Release);
            return Maintained::Released;
        }
        match self.reopen() {
            Ok(()) => self.reopened(reason),
            Err(error) => Maintained::Failed(error),
        }
    }

    /// The errors the device reported since the last call, fatal and
    /// recoverable alike. A fatal one is still reported when the output has
    /// already reopened.
    pub fn take_errors(&mut self) -> Vec<OutputError> {
        std::mem::take(
            &mut *self
                .shared
                .errors
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// How long the output has played, readable from any thread.
    #[must_use]
    pub fn clock(&self) -> Clock {
        self.clock.clone()
    }

    /// The device's name.
    #[must_use]
    pub fn device_name(&self) -> &str {
        &self.device
    }

    /// The device's id, to save and open it again by ([`Device::Id`]).
    ///
    /// For the default device, the device it is just now. On PipeWire the
    /// stream follows the default itself and its node is not named, so this
    /// is the default's node as PipeWire's PulseAudio server names it, or
    /// cpal's own `output_default` without one.
    #[must_use]
    pub fn device_id(&self) -> &DeviceId {
        &self.id
    }

    /// The stream's sample rate.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The stream's channel count.
    #[must_use]
    pub fn channels(&self) -> u16 {
        self.channels
    }

    fn default_changed(&self) -> bool {
        let (Some(watch), Some(live)) = (&self.watch, &self.live) else {
            return false;
        };
        match (watch.id(), &live.default_at_open) {
            (Some(now), Some(then)) => &now != then,
            _ => false,
        }
    }

    /// Closes the stream, if any, and opens the one the options ask for.
    fn reopen(&mut self) -> Result<(), OpenError> {
        // Let the old device go first: some only take one stream.
        self.live = None;
        match stream::open(
            &self.options,
            &self.renderer,
            &self.clock,
            &self.shared,
            self.paused,
        ) {
            Ok(live) => {
                self.stall = plan::Stall::new(self.clock.played(), Instant::now());
                self.device = live.device.clone();
                self.id = live.id.clone();
                self.sample_rate = live.sample_rate;
                self.channels = live.channels;
                self.live = Some(live);
                Ok(())
            }
            Err(error) => {
                log::warn!("audio output: {error}");
                Err(error)
            }
        }
    }

    /// Whether the output is playing but its clock has stood still for
    /// [`plan::STALL`]. Paused, nothing is expected to move.
    fn stalled(&mut self) -> bool {
        let now = Instant::now();
        let played = self.clock.played();
        if self.paused || self.live.is_none() {
            self.stall = plan::Stall::new(played, now);
            return false;
        }
        self.stall.observe(played, now)
    }

    fn reopened(&self, reason: Reason) -> Maintained {
        Maintained::Reopened {
            device: self.device.clone(),
            sample_rate: self.sample_rate,
            channels: self.channels,
            reason,
        }
    }
}

/// The name of the audio system the crate plays through, for the log:
/// `PipeWire`, `PulseAudio` or `ALSA` on Linux, `CoreAudio` on macOS,
/// `WASAPI` on Windows.
///
/// The process chooses it the first time it opens or lists a device (cpal's
/// default host: on Linux PipeWire, then PulseAudio, then ALSA, whichever is
/// running) and keeps it. Asking before that makes the choice, which can take
/// a moment, so ask off the UI thread or after opening.
#[must_use]
pub fn host() -> &'static str {
    stream::host_name()
}

/// The output devices' names, for a device picker. Nothing is opened, so a
/// device another program or a USB peripheral is using is left alone.
///
/// The names are every output cpal lists, as before [`output_devices`]: on
/// PipeWire that includes cpal's own default devices and other apps'
/// playback streams. Prefer [`output_devices`], whose ids also tell apart
/// two devices with the same name.
pub fn output_device_names() -> Result<Vec<String>, OpenError> {
    stream::output_device_names()
}

/// The output devices a picker can offer, with their ids and which one is
/// the default. Nothing is opened. Listing asks the audio system, which can
/// take a moment: call it off the UI thread.
///
/// Left out: what is not a device of its own, such as PipeWire's default
/// devices (which [`Device::Default`] stands for) and other apps' playback
/// streams, and ALSA's `null` device. A device that cannot give its id or
/// name just now (it is going away) is left out too.
///
/// On PipeWire, the default is found through its PulseAudio server; without
/// one, no device is marked the default.
pub fn output_devices() -> Result<Vec<DeviceInfo>, OpenError> {
    stream::devices(plan::Direction::Output)
}

/// The input devices a picker can offer: microphones, line inputs and other
/// sources, with their ids and which one is the default. Nothing is opened.
///
/// Left out: the monitors of output devices (what an output is playing),
/// PipeWire's default devices and ALSA's `null` device, as for
/// [`output_devices`]. On PipeWire another app's recording stream can
/// still be listed: cpal does not tell it apart from a source.
pub fn input_devices() -> Result<Vec<DeviceInfo>, OpenError> {
    stream::devices(plan::Direction::Input)
}
