//! The decisions behind opening and keeping an output, as pure functions over
//! what the device reported, so they are tested on every platform without a
//! device.

use std::time::{Duration, Instant};

use crate::{Buffer, BufferSize, ErrorKind};

/// One way to open the stream: a sample rate, and a fixed buffer or the
/// driver's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Attempt {
    pub(crate) sample_rate: u32,
    /// Frames, or `None` for the driver's buffer.
    pub(crate) buffer_frames: Option<u32>,
}

/// The order in which to try configurations: the app's preferred rate, then
/// the device's own rate (which Windows insists on for a shared device),
/// each with the fixed buffer when the app asked for one, then the first of
/// those with the driver's buffer, which a device that rejects the size still
/// accepts. Duplicates are left out. The device's other listed configurations
/// come after these, from the caller.
///
/// A fixed buffer is tried as asked before it is clamped to the range the
/// device reported, because a range is not always a limit: WASAPI in shared
/// mode reports its period as both ends and takes any size, and Spotifast
/// needs its 100 ms there against underruns. CoreAudio rejects a size out of
/// its range, and then gets the clamped one.
pub(crate) fn attempts(
    preferred_rate: Option<u32>,
    device_rate: u32,
    buffer: Option<BufferSize>,
    buffer_range: Option<(u32, u32)>,
) -> Vec<Attempt> {
    let mut rates = Vec::with_capacity(2);
    if let Some(rate) = preferred_rate.filter(|&rate| rate > 0) {
        rates.push(rate);
    }
    if !rates.contains(&device_rate) {
        rates.push(device_rate);
    }
    let mut out: Vec<Attempt> = Vec::new();
    let mut push = |attempt: Attempt| {
        if !out.contains(&attempt) {
            out.push(attempt);
        }
    };
    if let Some(size) = buffer {
        for &rate in &rates {
            for range in [None, buffer_range] {
                push(Attempt {
                    sample_rate: rate,
                    buffer_frames: Some(buffer_frames(size, rate, range)),
                });
            }
        }
    }
    for &rate in &rates {
        push(Attempt {
            sample_rate: rate,
            buffer_frames: None,
        });
    }
    out
}

/// What the app asked of a stream, whichever way it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Wanted {
    /// A rate to try first.
    pub(crate) sample_rate: Option<u32>,
    /// `0` takes the device's own.
    pub(crate) channels: u16,
    /// A fixed buffer, or `None` for the driver's.
    pub(crate) buffer: Option<BufferSize>,
}

/// A configuration the device offers: a range of rates, a channel count and
/// a sample format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Offered {
    pub(crate) rates: (u32, u32),
    pub(crate) channels: u16,
    pub(crate) format: cpal::SampleFormat,
}

/// One stream configuration to try.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) attempt: Attempt,
    pub(crate) channels: u16,
    pub(crate) format: cpal::SampleFormat,
}

/// The configurations to try, in order, for an output and an input alike:
/// the [`attempts`] at the asked channel count (or the device's own) in the
/// device's default format, then every other configuration the device
/// offers, at the default rate within its range, with the driver's buffer.
/// Duplicates are left out.
pub(crate) fn candidates(
    wanted: &Wanted,
    default: &Offered,
    buffer_range: Option<(u32, u32)>,
    offered: &[Offered],
) -> Vec<Candidate> {
    let channels = match wanted.channels {
        0 => default.channels,
        channels => channels,
    };
    let device_rate = default.rates.0;
    let mut out: Vec<Candidate> =
        attempts(wanted.sample_rate, device_rate, wanted.buffer, buffer_range)
            .into_iter()
            .map(|attempt| Candidate {
                attempt,
                channels,
                format: default.format,
            })
            .collect();
    for config in offered {
        let (min, max) = config.rates;
        let candidate = Candidate {
            attempt: Attempt {
                sample_rate: if min <= max {
                    device_rate.clamp(min, max)
                } else {
                    device_rate
                },
                buffer_frames: None,
            },
            channels: config.channels,
            format: config.format,
        };
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// The buffer to ask for. PulseAudio's own target is about two seconds, for
/// playback and recording alike, which no app means by the driver's
/// buffer; it is asked for 20 ms, about a PipeWire quantum, instead.
pub(crate) fn buffer_for(buffer: Buffer, host: &str, windows: bool) -> Option<BufferSize> {
    let fixed = match buffer {
        Buffer::Driver => None,
        Buffer::Fixed(size) => Some(size),
        Buffer::FixedOnWindows(size) => windows.then_some(size),
    };
    fixed.or_else(|| (host == "pulseaudio").then_some(PULSEAUDIO_DRIVER_BUFFER))
}

/// What PulseAudio is asked for in place of its own buffer.
const PULSEAUDIO_DRIVER_BUFFER: BufferSize = BufferSize::Duration(Duration::from_millis(20));

/// A buffer size in frames at `sample_rate`, clamped to `range` when there is
/// one. At least one frame.
pub(crate) fn buffer_frames(size: BufferSize, sample_rate: u32, range: Option<(u32, u32)>) -> u32 {
    let frames = match size {
        BufferSize::Frames(frames) => frames,
        BufferSize::Duration(duration) => {
            let frames = duration.as_nanos() * u128::from(sample_rate) / 1_000_000_000;
            u32::try_from(frames).unwrap_or(u32::MAX)
        }
    }
    .max(1);
    match range {
        Some((min, max)) if min <= max && max > 0 => frames.clamp(min.max(1), max),
        _ => frames,
    }
}

/// Which way a stream goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    /// To the device: an [`Output`](crate::Output).
    Output,
    /// From the device: an [`Input`](crate::Input).
    Input,
}

impl Direction {
    /// The word for the log and for errors: "output" or "input".
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Output => "output",
            Self::Input => "input",
        }
    }
}

/// A device as cpal lists it: its host-less id and its name, either of which
/// a device that is going away can fail to give, and which ways it can play.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Listed {
    pub(crate) id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) input: bool,
    pub(crate) output: bool,
}

impl Listed {
    fn goes(&self, direction: Direction) -> bool {
        match direction {
            Direction::Output => self.output,
            Direction::Input => self.input,
        }
    }
}

/// The device to open, among the listed ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    /// The device asked for, at this position in the list.
    Listed(usize),
    /// The default device: none was asked for, or the one asked for is
    /// missing.
    Default {
        /// What was asked for, for the log, when it was missing.
        missing: Option<String>,
    },
}

/// Finds the device the app asked for among `listed`, in `direction`: by its
/// id for [`Device::Id`](crate::Device::Id), by its name for
/// [`Device::Named`](crate::Device::Named), and otherwise, or when it is not
/// there, the default device. An empty id or name asks for the default. A
/// device that cannot give its id or name is never matched by it.
pub(crate) fn choose(listed: &[Listed], wanted: &crate::Device, direction: Direction) -> Choice {
    let (by_id, wanted) = match wanted {
        crate::Device::Default => return Choice::Default { missing: None },
        crate::Device::Id(id) => (true, id.as_str()),
        crate::Device::Named(name) => (false, name.as_str()),
    };
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return Choice::Default { missing: None };
    }
    let key = |device: &Listed| {
        if by_id {
            device.id.clone()
        } else {
            device.name.clone()
        }
    };
    let describe = if by_id { "id" } else { "name" };
    match listed
        .iter()
        .position(|device| device.goes(direction) && key(device).as_deref() == Some(wanted))
    {
        Some(index) => Choice::Listed(index),
        None => Choice::Default {
            missing: Some(format!("{describe} {wanted:?}")),
        },
    }
}

/// cpal's PipeWire host lists three devices of its own that follow the
/// server's default: the default output, the default input, and the default
/// output's monitor.
pub(crate) const PIPEWIRE_DEFAULTS: [&str; 3] = ["output_default", "input_default", "sink_default"];

/// Whether `id` is one of the PipeWire host's default devices rather than a
/// device of its own.
pub(crate) fn is_pipewire_default(host: &str, id: &str) -> bool {
    host == "pipewire" && PIPEWIRE_DEFAULTS.contains(&id)
}

/// The positions in `listed` of the devices a picker offers in `direction`
/// on the cpal host named `host`: those that have an id and a name, without
/// what a picker should not offer.
///
/// - PipeWire: not its default devices, which `Device::Default` stands for.
///   cpal lists a sink as duplex, because its monitor can be captured, and
///   other apps' playback streams as outputs only, so an output is a duplex
///   node, and an input is a node that only captures: not a sink's monitor.
/// - PulseAudio: not a sink's monitor, the source named after the sink with
///   `.monitor`.
/// - ALSA: not the `null` device, which discards what it plays and captures
///   silence.
pub(crate) fn listable(host: &str, direction: Direction, listed: &[Listed]) -> Vec<usize> {
    let sinks: Vec<&str> = listed
        .iter()
        .filter(|device| device.output)
        .filter_map(|device| device.id.as_deref())
        .collect();
    listed
        .iter()
        .enumerate()
        .filter(|(_, device)| device.goes(direction) && device.name.is_some())
        .filter(|(_, device)| {
            let Some(id) = device.id.as_deref() else {
                return false;
            };
            match (host, direction) {
                ("pipewire", _) if is_pipewire_default(host, id) => false,
                ("pipewire", Direction::Output) => device.input,
                ("pipewire", Direction::Input) => !device.output,
                ("pulseaudio", Direction::Input) => id
                    .strip_suffix(".monitor")
                    .is_none_or(|sink| !sinks.contains(&sink)),
                ("alsa", _) => id != "null",
                _ => true,
            }
        })
        .map(|(index, _)| index)
        .collect()
}

/// The device to open for the default output: the endpoint that is the
/// default right now, found among the listed devices by its id, or the
/// default device itself when it is not among them.
///
/// On Windows, cpal's default device is a virtual one that WASAPI activates
/// through `ActivateAudioInterfaceAsync`. Once cpal's device change callbacks
/// have run on Windows' thread pool, that activation fails with
/// `RPC_E_CHANGED_MODE` for the rest of the process (RustAudio/cpal#1402,
/// crmne/spotifast#689), so a stream that a new Bluetooth headset broke never
/// opens again. The endpoint itself keeps opening, and the output follows the
/// default on its own, so it opens the endpoint.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn concrete_default<D, I: PartialEq>(
    default: D,
    listed: impl IntoIterator<Item = D>,
    id: impl Fn(&D) -> Option<I>,
) -> D {
    let Some(wanted) = id(&default) else {
        return default;
    };
    listed
        .into_iter()
        .find(|device| id(device).as_ref() == Some(&wanted))
        .unwrap_or(default)
}

/// Whether an output paused since `paused_at` has rested long enough to let
/// the device go.
pub(crate) fn should_release(
    paused_at: Option<Instant>,
    now: Instant,
    release_after: Option<Duration>,
) -> bool {
    match (paused_at, release_after) {
        (Some(since), Some(after)) => now.saturating_duration_since(since) >= after,
        _ => false,
    }
}

/// How the app should treat an error, as Solco does (ADR 0165): a fatal one
/// fails what is playing; a recoverable one is logged. A lost device or an
/// invalidated stream is recoverable because the output reopens and the
/// renderer carries on.
pub(crate) fn kind_of(kind: cpal::ErrorKind) -> ErrorKind {
    use cpal::ErrorKind as Cpal;
    match kind {
        Cpal::HostUnavailable
        | Cpal::PermissionDenied
        | Cpal::ResourceExhausted
        | Cpal::UnsupportedConfig
        | Cpal::UnsupportedOperation
        | Cpal::InvalidInput => ErrorKind::Fatal,
        _ => ErrorKind::Recoverable,
    }
}

/// Whether a stream that reported `kind` has to be opened again. Only a
/// glitch, a route the backend already followed, and refused real-time
/// priority leave the stream working; anything else may have stopped it, as
/// Spotifast's output has always assumed.
pub(crate) fn needs_reopen(kind: cpal::ErrorKind) -> bool {
    use cpal::ErrorKind as Cpal;
    !matches!(
        kind,
        Cpal::Xrun | Cpal::DeviceChanged | Cpal::RealtimeDenied
    )
}

/// Follows the device's own timeline of when each callback's audio plays,
/// to notice when it played something else in between: the callback came
/// too late and the device played silence (an underrun).
///
/// Each block should play where the one before it ends. A block scheduled
/// more than half a block after that left a gap. The playback times come
/// from the device (PipeWire's graph clock, CoreAudio's host time, WASAPI's
/// padding), so a late callback that still made its deadline leaves none,
/// and neither does a callback that renders slowly. A pause, a resume or a
/// new stream starts the timeline again.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Timeline {
    /// When the previous block's audio ends, in the stream's nanoseconds.
    next: Option<u128>,
    /// The output's pause and resume count when `next` was set.
    epoch: u64,
}

impl Timeline {
    /// Notes a block of `frames` at `rate` that plays at `playback`
    /// nanoseconds; true when the device played a gap before it.
    pub(crate) fn observe(&mut self, epoch: u64, playback: u128, frames: u64, rate: u32) -> bool {
        if rate == 0 || frames == 0 || playback == 0 {
            self.next = None;
            return false;
        }
        let length = u128::from(frames) * 1_000_000_000 / u128::from(rate);
        let gap = match self.next {
            Some(next) if epoch == self.epoch => playback > next + length / 2,
            _ => false,
        };
        self.epoch = epoch;
        self.next = Some(playback + length);
        gap
    }
}

/// Whether the callback thread of a stream on `device`, of the cpal host
/// named `host`, waits for the device between callbacks, so that making it
/// real-time cannot let it spin through the CPU time a real-time thread may
/// use (`RLIMIT_RTTIME`), after which the kernel kills the process.
///
/// cpal's own rule (`is_rt_eligible`, ALSA) promotes only PCMs of the
/// hardware types: not the `null` PCM, whose poll is always ready, nor the
/// I/O and external plugins that may route to a sound server, nor the
/// plugins that can chain to either. cpal keeps the PCM's type to itself, so
/// this goes by the PCM's name: only `hw` PCMs. cpal's native PipeWire and
/// PulseAudio hosts wait on the server's clock.
// Only Linux promotes the callback thread (`realtime.rs`); the decision
// stays pure and platform-independent so its tests run everywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn realtime_eligible(host: &str, device: &str) -> bool {
    match host {
        "pipewire" | "pulseaudio" => true,
        "alsa" => device == "hw" || device.starts_with("hw:"),
        _ => false,
    }
}

/// How long the spin guard looks at the callback thread at a time.
// Only Linux promotes the callback thread (`realtime.rs`); the decision
// stays pure and platform-independent so its tests run everywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) const SPIN_WINDOW: Duration = Duration::from_millis(40);
/// The share of a window's wall time a thread that waits for its device
/// never comes near. A Kira or rodio render takes a few percent.
// Only Linux promotes the callback thread (`realtime.rs`); the decision
// stays pure and platform-independent so its tests run everywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const SPIN_BUSY: f64 = 0.8;

/// Watches how busy the callback thread is, window by window, to notice one
/// that never waits: a device that is always ready asks for audio again as
/// soon as the callback returns.
// Only Linux promotes the callback thread (`realtime.rs`); the decision
// stays pure and platform-independent so its tests run everywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SpinWindow {
    /// When the current window started, and the thread's CPU time then.
    started: Option<(Instant, Duration)>,
}

// Only Linux promotes the callback thread (`realtime.rs`); the decision
// stays pure and platform-independent so its tests run everywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl SpinWindow {
    /// Notes the thread's CPU time `cpu` at `now`; true when a window just
    /// ended in which the thread ran for most of the time.
    pub(crate) fn observe(&mut self, now: Instant, cpu: Duration) -> bool {
        let Some((at, cpu_then)) = self.started else {
            self.started = Some((now, cpu));
            return false;
        };
        let wall = now.saturating_duration_since(at);
        if wall < SPIN_WINDOW {
            return false;
        }
        self.started = Some((now, cpu));
        cpu.saturating_sub(cpu_then).as_secs_f64() >= SPIN_BUSY * wall.as_secs_f64()
    }
}

/// How long an output's renderer has had nothing to play, for
/// [`OutputOptions::pause_when_idle`](crate::OutputOptions::pause_when_idle).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Idle {
    /// When the renderer was first seen idle, since it last played.
    since: Option<Instant>,
}

impl Idle {
    /// Notes whether the renderer is idle at `now`; true once it has been
    /// idle at every look for `after`.
    pub(crate) fn observe(&mut self, idle: bool, now: Instant, after: Duration) -> bool {
        if !idle {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        now.saturating_duration_since(since) >= after
    }
}

/// How long a playing output may stand still before it counts as stuck: a
/// few times the longest device buffer.
pub(crate) const STALL: Duration = Duration::from_secs(2);

/// Watches a playing output's clock for a stream that stopped asking for
/// sound without reporting an error, which some drivers do when a device is
/// rerouted (Spotifast #731).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Stall {
    played: Duration,
    since: Instant,
    seen: Instant,
}

impl Stall {
    pub(crate) fn new(played: Duration, now: Instant) -> Self {
        Self {
            played,
            since: now,
            seen: now,
        }
    }

    /// Notes how far the output has played; true once it has stood still
    /// for [`STALL`]. A gap of [`STALL`] or more between two looks is time
    /// the app did not run (the system slept, or suspended it), when the
    /// output could not play either, so it starts the count again.
    pub(crate) fn observe(&mut self, played: Duration, now: Instant) -> bool {
        let asleep = now.saturating_duration_since(self.seen) >= STALL;
        self.seen = now;
        if played != self.played || asleep {
            *self = Self::new(played, now);
            return false;
        }
        now.saturating_duration_since(self.since) >= STALL
    }
}

/// The interleaved sample ranges to render one callback's buffer in, of at
/// most `max_frames` frames each. Lengths in samples, so the last block
/// holds what is left.
pub(crate) fn blocks(
    samples: usize,
    channels: usize,
    max_frames: Option<u32>,
) -> Vec<(usize, usize)> {
    let channels = channels.max(1);
    let step = match max_frames {
        Some(frames) if frames > 0 => (frames as usize).saturating_mul(channels),
        _ => samples.max(1),
    };
    let mut out = Vec::new();
    let mut start = 0;
    while start < samples {
        let end = (start + step).min(samples);
        out.push((start, end));
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_devices_that_wait_for_the_hardware_are_made_real_time() {
        // The "Discard all samples" PCM is always ready: a real-time thread
        // on it spins until the kernel kills the process.
        assert!(!realtime_eligible("alsa", "null"));
        // Plugins that may route to a sound server, or chain to one.
        for device in [
            "default",
            "pulse",
            "pipewire",
            "plughw:CARD=PCH,DEV=0",
            "dmix:0",
        ] {
            assert!(!realtime_eligible("alsa", device), "{device}");
        }
        assert!(realtime_eligible("alsa", "hw:CARD=PCH,DEV=0"));
        assert!(realtime_eligible("alsa", "hw"));
        assert!(!realtime_eligible("alsa", "hwplug"));
        assert!(realtime_eligible(
            "pipewire",
            "alsa_output.pci-0000_00_1f.3"
        ));
        assert!(realtime_eligible("pulseaudio", "@DEFAULT_SINK@"));
        // A host this does not know keeps its own scheduling.
        assert!(!realtime_eligible("jack", "system"));
    }

    #[test]
    fn a_callback_thread_that_never_waits_is_caught_within_a_window() {
        let start = Instant::now();
        let mut window = SpinWindow::default();
        // Waits for the device: a few percent of each window.
        assert!(!window.observe(start, Duration::ZERO));
        let mut cpu = Duration::ZERO;
        for step in 1..=10_u32 {
            cpu += Duration::from_millis(1);
            assert!(!window.observe(start + SPIN_WINDOW * step, cpu));
        }
        // Then never waits: busy for the whole of the next window.
        let at = start + SPIN_WINDOW * 10;
        assert!(!window.observe(at + SPIN_WINDOW / 2, cpu + SPIN_WINDOW / 2));
        assert!(window.observe(at + SPIN_WINDOW, cpu + SPIN_WINDOW));
    }

    #[test]
    fn the_timeline_counts_blocks_the_device_played_late() {
        let block = 512_u64;
        let rate = 48_000;
        let length = u128::from(block) * 1_000_000_000 / u128::from(rate);
        let mut timeline = Timeline::default();
        let mut at = 1_000_000_u128;
        let mut gaps = 0;
        // Contiguous blocks, timestamps a little off either way: no gaps.
        for jitter in [0_i64, 3_000, -2_000, 1_000, 0] {
            let playback =
                u128::try_from(i128::try_from(at).unwrap() + i128::from(jitter)).unwrap();
            gaps += u32::from(timeline.observe(0, playback, block, rate));
            at += length;
        }
        assert_eq!(gaps, 0);
        // The device played one block of silence before this one.
        at += length;
        assert!(timeline.observe(0, at, block, rate));
        at += length;
        assert!(!timeline.observe(0, at, block, rate));
        // After a pause and a resume the jump is not a gap.
        at += 5_000_000_000;
        assert!(!timeline.observe(1, at, block, rate));
        // A backend without timestamps never counts.
        assert!(!timeline.observe(1, 0, block, rate));
        assert!(!timeline.observe(1, 0, block, rate));
    }

    #[test]
    fn the_default_opens_as_the_endpoint_it_is_now() {
        // (name, id): the virtual default is endpoint 2 just now.
        let id = |device: &(&str, Option<u8>)| device.1;
        let listed = [("a", Some(1)), ("b", Some(2)), ("c", None)];
        assert_eq!(concrete_default(("default", Some(2)), listed, id).0, "b");
    }

    #[test]
    fn the_default_stays_when_its_endpoint_is_not_listed() {
        let id = |device: &(&str, Option<u8>)| device.1;
        assert_eq!(
            concrete_default(("default", Some(2)), [("a", Some(1))], id).0,
            "default"
        );
        assert_eq!(
            concrete_default(("default", None), [("a", None)], id).0,
            "default",
            "a default without an id is not matched to a device without one"
        );
    }

    fn listed(id: &str, name: &str, input: bool, output: bool) -> Listed {
        Listed {
            id: Some(id.to_owned()),
            name: Some(name.to_owned()),
            input,
            output,
        }
    }

    #[test]
    fn a_device_is_found_by_its_id_then_by_its_name() {
        use crate::{Device, DeviceId};
        let devices = [
            listed("alsa_output.pci", "Speakers", true, true),
            listed("alsa_output.usb", "USB DAC", true, true),
            listed("alsa_input.usb", "USB DAC", true, false),
        ];
        let out = Direction::Output;
        let id = |id: &str| Device::Id(DeviceId::new(id));
        assert_eq!(
            choose(&devices, &id("alsa_output.usb"), out),
            Choice::Listed(1)
        );
        // Two devices share the name; the direction tells them apart.
        let named = Device::Named("USB DAC".into());
        assert_eq!(choose(&devices, &named, out), Choice::Listed(1));
        assert_eq!(
            choose(&devices, &named, Direction::Input),
            Choice::Listed(1)
        );
        assert_eq!(
            choose(&devices, &id("alsa_input.usb"), Direction::Input),
            Choice::Listed(2)
        );
        // An id is never matched by name, nor a name by id.
        assert_eq!(
            choose(&devices, &id("Speakers"), out),
            Choice::Default {
                missing: Some("id \"Speakers\"".into())
            }
        );
        assert_eq!(
            choose(&devices, &Device::Named("alsa_output.pci".into()), out),
            Choice::Default {
                missing: Some("name \"alsa_output.pci\"".into())
            }
        );
        // An input's id does not open it as an output.
        assert!(matches!(
            choose(&devices, &id("alsa_input.usb"), out),
            Choice::Default { missing: Some(_) }
        ));
    }

    #[test]
    fn a_missing_device_falls_back_to_the_default() {
        use crate::{Device, DeviceId};
        let devices = [listed("a", "Speakers", false, true)];
        let out = Direction::Output;
        assert_eq!(
            choose(&devices, &Device::Default, out),
            Choice::Default { missing: None }
        );
        for blank in [Device::Named("  ".into()), Device::Id(DeviceId::new(""))] {
            assert_eq!(
                choose(&devices, &blank, out),
                Choice::Default { missing: None }
            );
        }
        assert_eq!(
            choose(&devices, &Device::Named("Headphones".into()), out),
            Choice::Default {
                missing: Some("name \"Headphones\"".into())
            }
        );
    }

    #[test]
    fn a_device_that_cannot_name_itself_is_matched_only_by_its_id() {
        use crate::{Device, DeviceId};
        // cpal cannot describe a device that is going away.
        let devices = [
            Listed {
                id: Some("b".into()),
                name: None,
                input: false,
                output: true,
            },
            listed("c", "output 3", false, true),
        ];
        let out = Direction::Output;
        assert_eq!(
            choose(&devices, &Device::Named("output 3".into()), out),
            Choice::Listed(1)
        );
        assert_eq!(
            choose(&devices, &Device::Id(DeviceId::new("b")), out),
            Choice::Listed(0)
        );
        // A picker leaves it out until it can be named.
        assert_eq!(listable("coreaudio", out, &devices), [1]);
    }

    #[test]
    fn pipewire_pickers_offer_sinks_and_sources_only() {
        // As cpal 0.18 lists them on PipeWire.
        let devices = [
            listed("sink_default", "default_sink", true, true),
            listed("input_default", "default_input", true, false),
            listed("output_default", "default_output", false, true),
            // Another app's playback stream.
            listed("alsa_playback.voxtype", "ALSA Playback", false, true),
            listed("alsa_output.pci.hdmi-stereo", "HDMI", true, true),
            listed("alsa_input.usb.mic", "Mic", true, false),
        ];
        assert_eq!(listable("pipewire", Direction::Output, &devices), [4]);
        assert_eq!(listable("pipewire", Direction::Input, &devices), [5]);
        assert!(is_pipewire_default("pipewire", "output_default"));
        assert!(!is_pipewire_default("pulseaudio", "output_default"));
    }

    #[test]
    fn pulseaudio_inputs_leave_out_the_sinks_monitors() {
        let devices = [
            listed("alsa_output.hdmi", "HDMI", false, true),
            listed("alsa_output.hdmi.monitor", "Monitor of HDMI", true, false),
            listed("alsa_input.mic", "Mic", true, false),
            // Not the monitor of any sink listed.
            listed("loopback.monitor", "Loopback", true, false),
        ];
        assert_eq!(listable("pulseaudio", Direction::Output, &devices), [0]);
        assert_eq!(listable("pulseaudio", Direction::Input, &devices), [2, 3]);
    }

    #[test]
    fn alsa_pickers_leave_out_the_null_device() {
        let devices = [
            listed("null", "Discard all samples", true, true),
            listed("default", "Default ALSA Output", true, true),
            listed("hw:CARD=PCH,DEV=0", "HDA Intel PCH", true, true),
            listed("hdmi:CARD=NVidia,DEV=0", "HDA NVidia", false, true),
        ];
        assert_eq!(listable("alsa", Direction::Output, &devices), [1, 2, 3]);
        assert_eq!(listable("alsa", Direction::Input, &devices), [1, 2]);
        // WASAPI and CoreAudio list their endpoints as they are.
        assert_eq!(
            listable("wasapi", Direction::Output, &devices),
            [0, 1, 2, 3]
        );
    }

    #[test]
    fn the_preferred_rate_comes_first_then_the_devices_then_the_drivers_buffer() {
        let tried = attempts(
            Some(44_100),
            48_000,
            Some(BufferSize::Frames(512)),
            Some((64, 4096)),
        );
        assert_eq!(
            tried,
            [
                Attempt {
                    sample_rate: 44_100,
                    buffer_frames: Some(512)
                },
                Attempt {
                    sample_rate: 48_000,
                    buffer_frames: Some(512)
                },
                Attempt {
                    sample_rate: 44_100,
                    buffer_frames: None
                },
                Attempt {
                    sample_rate: 48_000,
                    buffer_frames: None
                },
            ]
        );
    }

    #[test]
    fn a_fixed_buffer_is_tried_as_asked_before_it_is_clamped() {
        // WASAPI in shared mode reports its 10 ms period as the whole range,
        // and takes Spotifast's 100 ms anyway.
        let tried = attempts(
            Some(44_100),
            48_000,
            Some(BufferSize::Duration(Duration::from_millis(100))),
            Some((480, 480)),
        );
        assert_eq!(
            tried,
            [
                Attempt {
                    sample_rate: 44_100,
                    buffer_frames: Some(4410)
                },
                Attempt {
                    sample_rate: 44_100,
                    buffer_frames: Some(480)
                },
                Attempt {
                    sample_rate: 48_000,
                    buffer_frames: Some(4800)
                },
                Attempt {
                    sample_rate: 48_000,
                    buffer_frames: Some(480)
                },
                Attempt {
                    sample_rate: 44_100,
                    buffer_frames: None
                },
                Attempt {
                    sample_rate: 48_000,
                    buffer_frames: None
                },
            ]
        );
    }

    #[test]
    fn without_a_preference_or_a_buffer_the_device_rate_is_tried_once() {
        assert_eq!(
            attempts(None, 48_000, None, None),
            [Attempt {
                sample_rate: 48_000,
                buffer_frames: None
            }]
        );
        assert_eq!(
            attempts(Some(48_000), 48_000, Some(BufferSize::Frames(256)), None),
            [
                Attempt {
                    sample_rate: 48_000,
                    buffer_frames: Some(256)
                },
                Attempt {
                    sample_rate: 48_000,
                    buffer_frames: None
                },
            ]
        );
    }

    #[test]
    fn inputs_and_outputs_are_planned_alike_from_what_the_device_offers() {
        use cpal::SampleFormat as F;
        // A USB microphone: mono 48 kHz in i16 by default, and also stereo
        // in i32 over 44.1 to 96 kHz.
        let default = Offered {
            rates: (48_000, 48_000),
            channels: 1,
            format: F::I16,
        };
        let offered = [
            default,
            Offered {
                rates: (44_100, 96_000),
                channels: 2,
                format: F::I32,
            },
            // A range a driver reports wrongly keeps the default rate.
            Offered {
                rates: (96_000, 44_100),
                channels: 2,
                format: F::F32,
            },
        ];
        let wanted = Wanted {
            sample_rate: Some(44_100),
            channels: 0,
            buffer: Some(BufferSize::Frames(256)),
        };
        let tried = candidates(&wanted, &default, None, &offered);
        let at = |rate, frames, channels, format| Candidate {
            attempt: Attempt {
                sample_rate: rate,
                buffer_frames: frames,
            },
            channels,
            format,
        };
        assert_eq!(
            tried,
            [
                // The device's own channels and format, preferred rate first.
                at(44_100, Some(256), 1, F::I16),
                at(48_000, Some(256), 1, F::I16),
                at(44_100, None, 1, F::I16),
                at(48_000, None, 1, F::I16),
                // Then what else it offers, at its default rate.
                at(48_000, None, 2, F::I32),
                at(48_000, None, 2, F::F32),
            ]
        );
        // Channels the app asks for are asked for.
        let stereo = Wanted {
            channels: 2,
            ..wanted
        };
        assert_eq!(candidates(&stereo, &default, None, &[])[0].channels, 2);
    }

    #[test]
    fn pulseaudio_is_asked_for_20_ms_in_place_of_its_own_buffer() {
        let ms = |ms| BufferSize::Duration(Duration::from_millis(ms));
        assert_eq!(
            buffer_for(Buffer::Driver, "pulseaudio", false),
            Some(ms(20))
        );
        assert_eq!(buffer_for(Buffer::Driver, "pipewire", false), None);
        assert_eq!(
            buffer_for(Buffer::FixedOnWindows(ms(100)), "pulseaudio", false),
            Some(ms(20))
        );
        assert_eq!(
            buffer_for(Buffer::FixedOnWindows(ms(100)), "wasapi", true),
            Some(ms(100))
        );
        assert_eq!(
            buffer_for(Buffer::FixedOnWindows(ms(100)), "coreaudio", false),
            None
        );
        assert_eq!(
            buffer_for(Buffer::Fixed(BufferSize::Frames(512)), "pulseaudio", false),
            Some(BufferSize::Frames(512))
        );
    }

    #[test]
    fn buffer_sizes_are_frames_at_the_rate_and_stay_in_the_devices_range() {
        let ms = BufferSize::Duration(Duration::from_millis(100));
        assert_eq!(buffer_frames(ms, 44_100, None), 4410);
        assert_eq!(buffer_frames(ms, 48_000, None), 4800);
        // Solco's 512 frames are 512 at any rate.
        assert_eq!(buffer_frames(BufferSize::Frames(512), 48_000, None), 512);
        assert_eq!(
            buffer_frames(BufferSize::Frames(512), 44_100, Some((1024, 8192))),
            1024
        );
        assert_eq!(
            buffer_frames(BufferSize::Frames(9000), 44_100, Some((64, 8192))),
            8192
        );
        // A range a driver reports wrongly is ignored, and the size is never 0.
        assert_eq!(
            buffer_frames(BufferSize::Frames(512), 44_100, Some((900, 100))),
            512
        );
        assert_eq!(buffer_frames(BufferSize::Frames(0), 44_100, None), 1);
    }

    #[test]
    fn a_long_pause_lets_the_device_go() {
        let start = Instant::now();
        let later = start + Duration::from_secs(301);
        let after = Some(Duration::from_secs(300));
        assert!(should_release(Some(start), later, after));
        assert!(!should_release(
            Some(start),
            start + Duration::from_secs(10),
            after
        ));
        assert!(!should_release(None, later, after));
        assert!(!should_release(Some(start), later, None));
    }

    #[test]
    fn errors_split_as_solco_handles_them() {
        use cpal::ErrorKind as Cpal;
        for fatal in [
            Cpal::HostUnavailable,
            Cpal::PermissionDenied,
            Cpal::ResourceExhausted,
            Cpal::UnsupportedConfig,
            Cpal::UnsupportedOperation,
            Cpal::InvalidInput,
        ] {
            assert_eq!(kind_of(fatal), ErrorKind::Fatal, "{fatal:?}");
            assert!(needs_reopen(fatal), "{fatal:?}");
        }
        // Logged, and the output carries on: reopened where the stream may
        // have stopped, left alone where it is still playing.
        for (kind, reopen) in [
            (Cpal::DeviceNotAvailable, true),
            (Cpal::StreamInvalidated, true),
            (Cpal::DeviceBusy, true),
            (Cpal::BackendError, true),
            (Cpal::Other, true),
            (Cpal::Xrun, false),
            (Cpal::DeviceChanged, false),
            (Cpal::RealtimeDenied, false),
        ] {
            assert_eq!(kind_of(kind), ErrorKind::Recoverable, "{kind:?}");
            assert_eq!(needs_reopen(kind), reopen, "{kind:?}");
        }
    }

    #[test]
    fn an_output_pauses_once_it_has_had_nothing_to_play_for_long_enough() {
        let start = Instant::now();
        let ms = Duration::from_millis;
        let after = ms(1000);
        let mut idle = Idle::default();
        assert!(!idle.observe(false, start, after), "playing");
        // Idle from 100 ms: counted from the first look that saw it.
        assert!(!idle.observe(true, start + ms(100), after));
        assert!(!idle.observe(true, start + ms(1099), after));
        assert!(idle.observe(true, start + ms(1100), after));
        // A sound in between starts the count again.
        let mut idle = Idle::default();
        assert!(!idle.observe(true, start, after));
        assert!(!idle.observe(false, start + ms(900), after));
        assert!(!idle.observe(true, start + ms(1000), after));
        assert!(!idle.observe(true, start + ms(1999), after));
        assert!(idle.observe(true, start + ms(2000), after));
        // A fresh tracker, as after a resume, waits the whole time again.
        let mut idle = Idle::default();
        assert!(!idle.observe(true, start + ms(5000), after));
        assert!(idle.observe(true, start + ms(6000), after));
        // Zero pauses at the first look.
        assert!(Idle::default().observe(true, start, Duration::ZERO));
    }

    #[test]
    fn an_output_that_stands_still_while_playing_is_stuck() {
        let start = Instant::now();
        let ms = Duration::from_millis;
        let mut stall = Stall::new(ms(0), start);
        assert!(!stall.observe(ms(0), start + STALL / 2));
        assert!(!stall.observe(ms(10), start + STALL), "it moved");
        assert!(!stall.observe(ms(10), start + STALL + STALL / 2));
        assert!(
            stall.observe(ms(10), start + STALL * 2),
            "nothing for STALL"
        );
        assert!(
            !stall.observe(ms(20), start + STALL * 2 + ms(16)),
            "moving again"
        );
    }

    #[test]
    fn time_asleep_does_not_count_as_stuck() {
        let start = Instant::now();
        let ms = Duration::from_millis;
        let mut stall = Stall::new(ms(0), start);
        assert!(!stall.observe(ms(0), start + ms(50)));
        let woke = start + Duration::from_secs(600);
        assert!(!stall.observe(ms(0), woke), "asleep, not stuck");
        assert!(!stall.observe(ms(0), woke + STALL / 2));
        assert!(stall.observe(ms(0), woke + STALL), "stuck after waking");
    }

    #[test]
    fn a_callback_is_rendered_in_blocks_of_at_most_the_asked_frames() {
        // 1200 stereo frames in blocks of 512 frames.
        assert_eq!(
            blocks(2400, 2, Some(512)),
            [(0, 1024), (1024, 2048), (2048, 2400)]
        );
        assert_eq!(blocks(2400, 2, None), [(0, 2400)]);
        assert_eq!(blocks(0, 2, Some(512)), []);
    }
}
