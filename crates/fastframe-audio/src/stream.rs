//! The thin layer that talks to cpal: choosing the device, opening the
//! stream in the device's sample format, and watching the default device,
//! the same way for an output and an input.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::plan::{self, Choice, Direction};
use crate::{Buffer, Capture, Clock, Device, DeviceId, DeviceInfo, OpenError, OutputError, Render};

/// Calls `$build::<T, $engine>(args)` with `T` the Rust type of the sample
/// format: every format cpal has.
macro_rules! by_format {
    ($format:expr, $build:ident, $engine:ident, $args:expr) => {{
        use cpal::SampleFormat as F;
        match $format {
            F::F32 => $build::<f32, $engine>($args),
            F::F64 => $build::<f64, $engine>($args),
            F::I8 => $build::<i8, $engine>($args),
            F::I16 => $build::<i16, $engine>($args),
            F::I24 => $build::<cpal::I24, $engine>($args),
            F::I32 => $build::<i32, $engine>($args),
            F::I64 => $build::<i64, $engine>($args),
            F::U8 => $build::<u8, $engine>($args),
            F::U16 => $build::<u16, $engine>($args),
            F::U24 => $build::<cpal::U24, $engine>($args),
            F::U32 => $build::<u32, $engine>($args),
            F::U64 => $build::<u64, $engine>($args),
            other => Err(cpal::Error::with_message(
                cpal::ErrorKind::UnsupportedConfig,
                format!("unsupported sample format {other}"),
            )),
        }
    }};
}

/// The audio system this process chose, the first time it asked.
///
/// Only the choice is kept, not the `cpal::Host`. cpal's PipeWire host lists
/// the devices once, when it is made, so a kept one would never see a headset
/// plugged in later; its PulseAudio host holds a connection that a restart
/// of the server breaks; and a host is not `Sync` on every backend. Making a
/// host from its id skips the probing of `cpal::default_host` and keeps every
/// stream and listing of the process on the same system.
static HOST: OnceLock<cpal::HostId> = OnceLock::new();

/// A fresh host of the audio system this process uses: the default host the
/// first time, the same system afterwards. When that system cannot be
/// reached just now (a sound server restarting), the default host of the
/// moment, without changing the choice, so the next call tries it again.
pub(crate) fn host() -> cpal::Host {
    if let Some(&id) = HOST.get() {
        match cpal::host_from_id(id) {
            Ok(host) => return host,
            Err(error) => {
                log::debug!("audio: {} is not available just now: {error}", id.name());
            }
        }
    }
    let host = cpal::default_host();
    let _ = HOST.set(host.id());
    host
}

/// The name of the audio system this process uses, choosing it if it has not
/// yet.
pub(crate) fn host_name() -> &'static str {
    match HOST.get() {
        Some(id) => id.name(),
        None => host().id().name(),
    }
}

/// What the callback and the error callback share with the
/// [`Output`](crate::Output) or [`Input`](crate::Input).
#[derive(Debug)]
pub(crate) struct Shared {
    /// Which way the stream goes, for the log.
    direction: Direction,
    /// A stream error means the stream has to be opened again.
    pub(crate) failed: AtomicBool,
    /// Errors not yet taken by the app.
    pub(crate) errors: Mutex<Vec<OutputError>>,
    /// Counts pauses and resumes, so the callback knows the device's
    /// timeline started again (see [`plan::Timeline`]).
    pub(crate) epoch: std::sync::atomic::AtomicU64,
    /// What [`Render::idle`] said after the last render, when the output
    /// pauses when idle, marked with the [`wakes`](Self::wakes) count the
    /// render began at (see [`plan::idle_mark`]).
    pub(crate) idle: std::sync::atomic::AtomicU64,
    /// Counts resumes, each of which may have given the renderer something
    /// to play, so an idle mark from a render before it is stale.
    pub(crate) wakes: std::sync::atomic::AtomicU64,
}

impl Shared {
    pub(crate) fn new(direction: Direction) -> Self {
        Self {
            direction,
            failed: AtomicBool::new(false),
            errors: Mutex::new(Vec::new()),
            epoch: std::sync::atomic::AtomicU64::new(0),
            idle: std::sync::atomic::AtomicU64::new(0),
            wakes: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub(crate) fn report(&self, error: cpal::Error) {
        let reopen = plan::needs_reopen(error.kind());
        let error = OutputError::from_cpal(&error);
        let direction = self.direction.label();
        if reopen {
            log::warn!("audio {direction} error, reopening: {error}");
            self.failed.store(true, Ordering::Release);
        } else {
            log::debug!("audio {direction}: {error}");
        }
        self.errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(error);
    }
}

/// A stream to play and pause: cpal's, or in tests a stand-in.
pub(crate) enum Handle {
    Cpal(cpal::Stream),
    #[cfg(test)]
    Fake(Arc<FakeStream>),
}

impl Handle {
    pub(crate) fn play(&self) -> Result<(), cpal::Error> {
        match self {
            Self::Cpal(stream) => stream.play(),
            #[cfg(test)]
            Self::Fake(stream) => {
                stream.plays.fetch_add(1, Ordering::Relaxed);
                stream.playing.store(true, Ordering::Relaxed);
                Ok(())
            }
        }
    }

    pub(crate) fn pause(&self) -> Result<(), cpal::Error> {
        match self {
            Self::Cpal(stream) => stream.pause(),
            #[cfg(test)]
            Self::Fake(stream) => {
                stream.pauses.fetch_add(1, Ordering::Relaxed);
                stream.playing.store(false, Ordering::Relaxed);
                Ok(())
            }
        }
    }
}

/// What a test's stand-in stream was asked to do.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct FakeStream {
    pub(crate) playing: AtomicBool,
    pub(crate) plays: std::sync::atomic::AtomicU32,
    pub(crate) pauses: std::sync::atomic::AtomicU32,
}

/// How a session opens its stream: [`open`], or in tests a stand-in.
pub(crate) type Opener<E> =
    fn(&Settings, &Arc<Mutex<E>>, &Clock, &Arc<Shared>, bool) -> Result<Live, OpenError>;

/// An open stream and what it was opened with.
pub(crate) struct Live {
    pub(crate) stream: Handle,
    pub(crate) device: String,
    /// The device's own id, as cpal gives it without the host.
    pub(crate) own_id: String,
    /// The host it is on, as cpal names it in ids (`pipewire`).
    pub(crate) host: String,
    pub(crate) sample_rate: u32,
    /// The channels the renderer or capturer works in (an input's stream
    /// itself is at the source's own, see [`plan::source_channels`]).
    pub(crate) channels: u16,
    /// The default device's id when this stream opened on it, for noticing
    /// that the default changed.
    pub(crate) default_at_open: Option<String>,
    /// The id to report, when opening already found it: on PipeWire's
    /// default input, asked of its PulseAudio server for the source's
    /// channels.
    pub(crate) reported_id: Option<DeviceId>,
}

/// How a stream is opened, from [`OutputOptions`](crate::OutputOptions) or
/// [`InputOptions`](crate::InputOptions).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) device: Device,
    /// `0` takes the device's own.
    pub(crate) channels: u16,
    pub(crate) sample_rate: Option<u32>,
    pub(crate) buffer: Buffer,
    pub(crate) max_block_frames: Option<u32>,
    pub(crate) follow_default: bool,
    pub(crate) release_after: Option<std::time::Duration>,
    pub(crate) pause_when_idle: Option<std::time::Duration>,
}

/// What a stream's callback drives: the app's renderer for an output, its
/// capturer for an input.
pub(crate) trait Engine: Send + Sized + 'static {
    /// Which way the stream goes.
    const DIRECTION: Direction;

    /// Told the stream's format before it starts.
    fn configure(&mut self, sample_rate: u32, channels: u16);

    /// Builds a stream on `device` that drives `engine`, in `format`.
    fn build(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        format: cpal::SampleFormat,
        engine: Arc<Mutex<Self>>,
        callback: Callback,
    ) -> Result<cpal::Stream, cpal::Error>;
}

/// What the callback keeps besides the engine.
pub(crate) struct Callback {
    pub(crate) clock: Clock,
    pub(crate) shared: Arc<Shared>,
    pub(crate) max_block_frames: Option<u32>,
    /// Ask the renderer whether it is idle after each render.
    pub(crate) watch_idle: bool,
    /// The channels the engine works in. An output's are the stream's; an
    /// input's capturer can take others, which the callback remixes to
    /// ([`plan::remix`]).
    pub(crate) channels: u16,
}

/// An output's engine: the app's renderer.
pub(crate) struct Rendering<R>(pub(crate) R);

impl<R: Render> Engine for Rendering<R> {
    const DIRECTION: Direction = Direction::Output;

    fn configure(&mut self, sample_rate: u32, channels: u16) {
        self.0.configure(sample_rate, channels);
    }

    fn build(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        format: cpal::SampleFormat,
        engine: Arc<Mutex<Self>>,
        callback: Callback,
    ) -> Result<cpal::Stream, cpal::Error> {
        by_format!(format, play, R, (device, config, engine, callback))
    }
}

/// An input's engine: the app's capturer.
pub(crate) struct Capturing<C>(pub(crate) C);

impl<C: Capture> Engine for Capturing<C> {
    const DIRECTION: Direction = Direction::Input;

    fn configure(&mut self, sample_rate: u32, channels: u16) {
        self.0.configure(sample_rate, channels);
    }

    fn build(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        format: cpal::SampleFormat,
        engine: Arc<Mutex<Self>>,
        callback: Callback,
    ) -> Result<cpal::Stream, cpal::Error> {
        by_format!(format, record, C, (device, config, engine, callback))
    }
}

/// Opens the stream the settings ask for, in the engine's direction, and
/// starts it, unless `paused`.
pub(crate) fn open<E: Engine>(
    options: &Settings,
    engine: &Arc<Mutex<E>>,
    clock: &Clock,
    shared: &Arc<Shared>,
    paused: bool,
) -> Result<Live, OpenError> {
    let direction = E::DIRECTION;
    let label = direction.label();
    let host = host();
    let (device, default_at_open) = pick(&host, &options.device, direction)?;
    let name = device_name(&device).map_err(OpenError::from)?;
    let own_id = own_id(&device);
    let host_name = host.id().to_string();
    let default = match direction {
        Direction::Output => device.default_output_config(),
        Direction::Input => device.default_input_config(),
    }
    .map_err(OpenError::from)?;
    // An input opens at the source's own channels; on PipeWire's default
    // that takes asking its PulseAudio server, which also names the source.
    let mut reported_id = None;
    let own_channels = match direction {
        Direction::Output => default.channels(),
        Direction::Input => plan::source_channels(&host_name, &own_id, default.channels(), || {
            let source = server_default(&host_name, direction)?;
            reported_id = Some(DeviceId::new(self::own_id(&source)));
            source
                .default_input_config()
                .ok()
                .map(|config| config.channels())
        }),
    };
    let stream_channels = match direction {
        Direction::Output => options.channels,
        Direction::Input => 0,
    };
    let range = match *default.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => Some((min, max)),
        cpal::SupportedBufferSize::Unknown => None,
    };
    let offered: Vec<plan::Offered> = match direction {
        Direction::Output => device
            .supported_output_configs()
            .map(Iterator::collect::<Vec<_>>)
            .unwrap_or_default(),
        Direction::Input => device
            .supported_input_configs()
            .map(Iterator::collect::<Vec<_>>)
            .unwrap_or_default(),
    }
    .into_iter()
    .map(|config: cpal::SupportedStreamConfigRange| plan::Offered {
        rates: (config.min_sample_rate(), config.max_sample_rate()),
        channels: config.channels(),
        format: config.sample_format(),
    })
    .collect();
    let tried = plan::candidates(
        &plan::Wanted {
            sample_rate: options.sample_rate,
            channels: stream_channels,
            buffer: plan::buffer_for(options.buffer, &host_name, cfg!(windows)),
        },
        &plan::Offered {
            rates: (default.sample_rate(), default.sample_rate()),
            channels: own_channels,
            format: default.sample_format(),
        },
        range,
        &offered,
    );

    let mut last_error = None;
    for plan::Candidate {
        attempt,
        channels,
        format,
    } in tried
    {
        let config = cpal::StreamConfig {
            channels,
            sample_rate: attempt.sample_rate,
            buffer_size: attempt
                .buffer_frames
                .map_or(cpal::BufferSize::Default, cpal::BufferSize::Fixed),
        };
        let engine_channels = plan::engine_channels(direction, options.channels, channels);
        engine
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .configure(attempt.sample_rate, engine_channels);
        let callback = Callback {
            clock: clock.clone(),
            shared: Arc::clone(shared),
            max_block_frames: options.max_block_frames,
            watch_idle: options.pause_when_idle.is_some(),
            channels: engine_channels,
        };
        match E::build(&device, &config, format, Arc::clone(engine), callback) {
            Ok(stream) => {
                clock.restart(attempt.sample_rate);
                shared.failed.store(false, Ordering::Release);
                if !paused {
                    stream.play().map_err(OpenError::from)?;
                }
                log::info!(
                    "audio {label}: {name} at {} Hz, {channels} channels{}, {}, through {}",
                    attempt.sample_rate,
                    if engine_channels == channels {
                        String::new()
                    } else {
                        format!(" as {engine_channels}")
                    },
                    attempt
                        .buffer_frames
                        .map_or("the driver's buffer".to_owned(), |frames| format!(
                            "{frames} frames"
                        )),
                    host.id().name()
                );
                return Ok(Live {
                    stream: Handle::Cpal(stream),
                    device: name,
                    own_id,
                    host: host.id().to_string(),
                    sample_rate: attempt.sample_rate,
                    channels: engine_channels,
                    default_at_open,
                    reported_id,
                });
            }
            Err(error) => {
                log::debug!("audio {label}: {name} refused {config:?} as {format}: {error}");
                last_error = Some(error);
            }
        }
    }
    Err(last_error.map_or(OpenError::NoDevice, OpenError::from))
}

/// Whether a stream on a device may run its callback in real time (see
/// [`plan::realtime_eligible`]), and the device's cpal id, for the log.
#[cfg(target_os = "linux")]
pub(crate) struct Realtime {
    pub(crate) eligible: bool,
    pub(crate) device: String,
}

#[cfg(target_os = "linux")]
impl Realtime {
    fn of(device: &cpal::Device) -> Self {
        match device.id() {
            Ok(id) => Self {
                eligible: plan::realtime_eligible(&id.host().to_string(), id.id()),
                device: id.to_string(),
            },
            Err(_) => Self {
                eligible: false,
                device: "an unnamed device".to_owned(),
            },
        }
    }
}

/// The device to open, and the default device's id when that is the one.
fn pick(
    host: &cpal::Host,
    wanted: &Device,
    direction: Direction,
) -> Result<(cpal::Device, Option<String>), OpenError> {
    if *wanted != Device::Default {
        let devices: Vec<cpal::Device> = host.devices().map_err(OpenError::from)?.collect();
        let listed: Vec<plan::Listed> = devices.iter().map(listed).collect();
        match plan::choose(&host.id().to_string(), &listed, wanted, direction) {
            Choice::Listed(index) => {
                if let Some(device) = devices.into_iter().nth(index) {
                    return Ok((device, None));
                }
            }
            Choice::Default { missing } => {
                if let Some(missing) = missing {
                    log::warn!(
                        "audio device with {missing} is not available; using the default {}",
                        direction.label()
                    );
                }
            }
        }
    }
    let device = default_device(host, direction).ok_or(OpenError::NoDevice)?;
    #[cfg(windows)]
    let device = match host.devices() {
        Ok(listed) => plan::concrete_default(
            device,
            listed.filter(|device| goes(device, direction)),
            |device| device.id().ok(),
        ),
        Err(error) => {
            log::debug!(
                "audio {}: cannot list the devices to open the default's: {error}",
                direction.label()
            );
            device
        }
    };
    let id = own_id(&device);
    Ok((device, Some(id)))
}

/// The system's default device in `direction`.
fn default_device(host: &cpal::Host, direction: Direction) -> Option<cpal::Device> {
    match direction {
        Direction::Output => host.default_output_device(),
        Direction::Input => host.default_input_device(),
    }
}

/// Whether the device plays or captures, as `direction` asks.
#[cfg_attr(not(windows), allow(dead_code))]
fn goes(device: &cpal::Device, direction: Direction) -> bool {
    match direction {
        Direction::Output => device.supports_output(),
        Direction::Input => device.supports_input(),
    }
}

/// The device's id without its host, or an empty one (which opens the
/// default) when it cannot say.
fn own_id(device: &cpal::Device) -> String {
    device.id().map(|id| id.id().to_owned()).unwrap_or_default()
}

/// What choosing and listing need to know of a device.
fn listed(device: &cpal::Device) -> plan::Listed {
    plan::Listed {
        id: device.id().ok().map(|id| id.id().to_owned()),
        name: device_name(device).ok(),
        input: device.supports_input(),
        output: device.supports_output(),
    }
}

/// The device behind cpal's PipeWire default devices, which name none:
/// PipeWire's PulseAudio server's default sink or source. `None` without
/// that server, or on other hosts, which open a device of their own.
///
/// It connects to that server, which takes about 10 ms, and up to cpal's
/// two seconds when the server does not answer, so it is only asked when an
/// app wants the id, or opens the default input, whose channels cpal's
/// PipeWire host does not know.
fn server_default(host: &str, direction: Direction) -> Option<cpal::Device> {
    #[cfg(target_os = "linux")]
    if host == "pipewire" && cpal::available_hosts().contains(&cpal::HostId::PulseAudio) {
        let pulse = cpal::host_from_id(cpal::HostId::PulseAudio).ok()?;
        return default_device(&pulse, direction);
    }
    let _ = (host, direction);
    None
}

/// The id to report for a device whose own id is `own` on `host`: that, or
/// for a PipeWire default device the device it stands for just now, when
/// that can be found.
pub(crate) fn reported_id(host: &str, own: &str, direction: Direction) -> DeviceId {
    if plan::is_pipewire_default(host, own)
        && let Some(server) = server_default(host, direction)
    {
        return DeviceId::new(own_id(&server));
    }
    DeviceId::new(own)
}

/// The devices a picker can offer in `direction` (see [`plan::listable`]).
pub(crate) fn devices(direction: Direction) -> Result<Vec<DeviceInfo>, OpenError> {
    let host = host();
    let listed: Vec<plan::Listed> = host
        .devices()
        .map_err(OpenError::from)?
        .map(|device| listed(&device))
        .collect();
    let default = default_device(&host, direction).map(|device| {
        let id = own_id(&device);
        if plan::is_pipewire_default(&host.id().to_string(), &id) {
            server_default(&host.id().to_string(), direction).map(|device| own_id(&device))
        } else {
            Some(id)
        }
    });
    let default = default.flatten();
    Ok(plan::listable(&host.id().to_string(), direction, &listed)
        .into_iter()
        .filter_map(|index| {
            let device = listed.get(index)?;
            let id = device.id.clone()?;
            Some(DeviceInfo {
                is_default: default.as_deref() == Some(id.as_str()),
                id: DeviceId::new(id),
                name: device.name.clone()?,
            })
        })
        .collect())
}

/// The device's name. A device that is going away can fail to describe
/// itself, and cpal's `Display` turns that into a `fmt::Error`, which
/// `to_string` panics on. This asks for the description instead.
fn device_name(device: &cpal::Device) -> Result<String, cpal::Error> {
    Ok(device.description()?.name().to_owned())
}

type BuildArgs<'a, E> = (
    &'a cpal::Device,
    &'a cpal::StreamConfig,
    Arc<Mutex<E>>,
    Callback,
);

/// What every callback does first on its thread: join MMCSS on Windows, and
/// on Linux hand the thread to the real-time helper and watch it spin.
struct Thread {
    #[cfg(target_os = "linux")]
    promoter: crate::realtime::Promoter,
}

impl Thread {
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    fn new(device: &cpal::Device, direction: Direction) -> Self {
        Self {
            #[cfg(target_os = "linux")]
            promoter: {
                let realtime = Realtime::of(device);
                crate::realtime::Promoter::start(realtime.eligible, &realtime.device, direction)
            },
        }
    }

    fn on_callback(&mut self) {
        #[cfg(target_os = "windows")]
        crate::mmcss::join_once();
        #[cfg(target_os = "linux")]
        self.promoter.on_callback();
    }
}

/// An output stream that renders through the app's renderer in `T`.
fn play<T, R>(
    (device, config, renderer, callback): BuildArgs<'_, Rendering<R>>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
    R: Render,
{
    let Callback {
        clock,
        shared,
        max_block_frames: max_block,
        watch_idle,
        channels: _,
    } = callback;
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    let mut scratch: Vec<f32> = Vec::new();
    let errors = Arc::clone(&shared);
    let mut timeline = plan::Timeline::default();
    let mut thread = Thread::new(device, Direction::Output);
    device.build_output_stream::<T, _, _>(
        *config,
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
            thread.on_callback();
            scratch.clear();
            scratch.resize(data.len(), 0.0);
            // The app's thread only takes the lock while (re)opening, before
            // the stream starts; a contended lock renders silence rather
            // than wait.
            // Taken before rendering: a resume after this makes the mark
            // this render leaves stale.
            let wake = shared.wakes.load(Ordering::Acquire);
            let rendered = match renderer.try_lock() {
                Ok(mut renderer) => {
                    for (start, end) in plan::blocks(scratch.len(), channels, max_block) {
                        renderer.0.render(&mut scratch[start..end]);
                    }
                    if watch_idle {
                        let mark = plan::idle_mark(wake, renderer.0.idle());
                        shared.idle.store(mark, Ordering::Release);
                    }
                    true
                }
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    let mut renderer = poisoned.into_inner();
                    for (start, end) in plan::blocks(scratch.len(), channels, max_block) {
                        renderer.0.render(&mut scratch[start..end]);
                    }
                    if watch_idle {
                        let mark = plan::idle_mark(wake, renderer.0.idle());
                        shared.idle.store(mark, Ordering::Release);
                    }
                    true
                }
                Err(std::sync::TryLockError::WouldBlock) => false,
            };
            for (out, sample) in data.iter_mut().zip(&scratch) {
                *out = if rendered {
                    T::from_sample(*sample)
                } else {
                    T::EQUILIBRIUM
                };
            }
            let timestamp = info.timestamp();
            let latency = timestamp
                .playback
                .saturating_duration_since(timestamp.callback);
            let frames = (data.len() / channels.max(1)) as u64;
            if timeline.observe(
                shared.epoch.load(Ordering::Relaxed),
                timestamp.playback.as_nanos(),
                frames,
                rate,
            ) {
                clock.underran();
            }
            clock.rendered(frames, latency);
        },
        move |error| errors.report(error),
        None,
    )
}

/// An input stream that hands what the device captured, in `T`, to the
/// app's capturer as `f32`.
fn record<T, C>(
    (device, config, capturer, callback): BuildArgs<'_, Capturing<C>>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
    C: Capture,
{
    let Callback {
        clock,
        shared,
        channels: wanted,
        ..
    } = callback;
    let channels = usize::from(config.channels);
    let wanted = usize::from(wanted);
    let rate = config.sample_rate;
    // Sized here for a fixed buffer; with the driver's, they grow to the
    // largest callback and are reused from then on.
    let frames = match config.buffer_size {
        cpal::BufferSize::Fixed(frames) => frames as usize,
        cpal::BufferSize::Default => 0,
    };
    let mut scratch: Vec<f32> = Vec::with_capacity(frames * channels);
    // What the capturer takes when it asked for other channels than the
    // source's (`plan::remix`).
    let mut mixed: Vec<f32> = Vec::with_capacity(if wanted == channels {
        0
    } else {
        frames * wanted
    });
    let errors = Arc::clone(&shared);
    let mut timeline = plan::Timeline::default();
    let mut thread = Thread::new(device, Direction::Input);
    device.build_input_stream::<T, _, _>(
        *config,
        move |data: &[T], info: &cpal::InputCallbackInfo| {
            thread.on_callback();
            scratch.clear();
            scratch.extend(data.iter().map(|sample| sample.to_sample::<f32>()));
            let block: &[f32] = if wanted == channels {
                &scratch
            } else {
                plan::remix(&scratch, channels, wanted, &mut mixed);
                &mixed
            };
            // As for an output: a contended lock drops this block rather
            // than wait.
            match capturer.try_lock() {
                Ok(mut capturer) => capturer.0.capture(block),
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    poisoned.into_inner().0.capture(block);
                }
                Err(std::sync::TryLockError::WouldBlock) => {}
            }
            let timestamp = info.timestamp();
            let latency = timestamp
                .callback
                .saturating_duration_since(timestamp.capture);
            let frames = (data.len() / channels.max(1)) as u64;
            // A block captured later than where the one before it ends lost
            // what came in between: an overrun.
            if timeline.observe(
                shared.epoch.load(Ordering::Relaxed),
                timestamp.capture.as_nanos(),
                frames,
                rate,
            ) {
                clock.underran();
            }
            clock.captured(frames, latency);
        },
        move |error| errors.report(error),
        None,
    )
}

/// The default device's id, or `None` when there is none.
pub(crate) fn default_id(direction: Direction) -> Option<String> {
    default_device(&host(), direction).map(|device| own_id(&device))
}

/// Every output device's name. Nothing is opened. A device that cannot be
/// named is left out.
pub(crate) fn output_device_names() -> Result<Vec<String>, OpenError> {
    Ok(host()
        .output_devices()
        .map_err(OpenError::from)?
        .filter_map(|device| device_name(&device).ok())
        .collect())
}

/// The default device's id, polled on a thread of its own because
/// enumerating devices can block on Windows. The thread ends with the watch.
#[derive(Debug)]
pub(crate) struct DefaultWatch(Arc<Mutex<Option<String>>>);

/// How often the default output is asked for.
const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_secs(2);

impl DefaultWatch {
    pub(crate) fn start(initial: Option<String>, direction: Direction) -> Self {
        let shared = Arc::new(Mutex::new(initial));
        let weak = Arc::downgrade(&shared);
        let started = std::thread::Builder::new()
            .name("audio-default-watch".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(DEFAULT_CHECK_INTERVAL);
                    let Some(shared) = weak.upgrade() else {
                        break;
                    };
                    let id = default_id(direction);
                    *shared.lock().unwrap_or_else(PoisonError::into_inner) = id;
                }
            });
        if let Err(error) = started {
            log::warn!(
                "cannot watch the default audio {}: {error}",
                direction.label()
            );
        }
        Self(shared)
    }

    /// The last polled id.
    pub(crate) fn id(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}
