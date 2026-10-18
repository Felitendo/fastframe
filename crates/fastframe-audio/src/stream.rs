//! The thin layer that talks to cpal: choosing the device, opening the
//! stream in the device's sample format, and watching the default output.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::plan::{self, Attempt, Choice, Direction};
use crate::{Clock, Device, DeviceId, DeviceInfo, OpenError, OutputError, OutputOptions, Render};

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

/// What the callback and the error callback share with the [`Output`](crate::Output).
#[derive(Debug, Default)]
pub(crate) struct Shared {
    /// A stream error means the stream has to be opened again.
    pub(crate) failed: AtomicBool,
    /// Errors not yet taken by the app.
    pub(crate) errors: Mutex<Vec<OutputError>>,
    /// Counts pauses and resumes, so the callback knows the device's
    /// timeline started again (see [`plan::Timeline`]).
    pub(crate) epoch: std::sync::atomic::AtomicU64,
}

impl Shared {
    pub(crate) fn report(&self, error: cpal::Error) {
        let reopen = plan::needs_reopen(error.kind());
        let error = OutputError::from_cpal(&error);
        if reopen {
            log::warn!("audio output error, reopening: {error}");
            self.failed.store(true, Ordering::Release);
        } else {
            log::debug!("audio output: {error}");
        }
        self.errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(error);
    }
}

/// An open stream and what it was opened with.
pub(crate) struct Live {
    pub(crate) stream: cpal::Stream,
    pub(crate) device: String,
    pub(crate) id: DeviceId,
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
    /// The default device's id when this stream opened on it, for noticing
    /// that the default changed.
    pub(crate) default_at_open: Option<String>,
}

/// The buffer asked of PulseAudio when the app wants the driver's own: about
/// a PipeWire quantum.
const PULSEAUDIO_DRIVER_BUFFER: crate::BufferSize =
    crate::BufferSize::Duration(std::time::Duration::from_millis(20));

/// Opens the stream the options ask for and starts it, unless `paused`.
pub(crate) fn open<R: Render>(
    options: &OutputOptions,
    renderer: &Arc<Mutex<R>>,
    clock: &Clock,
    shared: &Arc<Shared>,
    paused: bool,
) -> Result<Live, OpenError> {
    let host = host();
    let (device, default_at_open) = pick(&host, &options.device, Direction::Output)?;
    let name = device_name(&device).map_err(OpenError::from)?;
    let id = reported_id(&host, &device, Direction::Output);
    let default = device.default_output_config().map_err(OpenError::from)?;
    let format = default.sample_format();
    let range = match *default.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => Some((min, max)),
        cpal::SupportedBufferSize::Unknown => None,
    };
    let buffer = match options.buffer {
        crate::Buffer::Driver => None,
        crate::Buffer::Fixed(size) => Some(size),
        crate::Buffer::FixedOnWindows(size) => cfg!(windows).then_some(size),
    };
    // PulseAudio's own target is about two seconds, which no app means by
    // the driver's buffer; ask it for what PipeWire would give instead.
    let buffer = buffer
        .or_else(|| (host.id().to_string() == "pulseaudio").then_some(PULSEAUDIO_DRIVER_BUFFER));
    let channels = if options.channels == 0 {
        default.channels()
    } else {
        options.channels
    };

    let mut tried = plan::attempts(options.sample_rate, default.sample_rate(), buffer, range)
        .into_iter()
        .map(|attempt| (attempt, channels, format))
        .collect::<Vec<_>>();
    // Then anything else the device lists, with the driver's buffer.
    if let Ok(configs) = device.supported_output_configs() {
        for config in configs {
            let rate = default
                .sample_rate()
                .clamp(config.min_sample_rate(), config.max_sample_rate());
            let candidate = (
                Attempt {
                    sample_rate: rate,
                    buffer_frames: None,
                },
                config.channels(),
                config.sample_format(),
            );
            if !tried.contains(&candidate) {
                tried.push(candidate);
            }
        }
    }

    let mut last_error = None;
    for (attempt, channels, format) in tried {
        let config = cpal::StreamConfig {
            channels,
            sample_rate: attempt.sample_rate,
            buffer_size: attempt
                .buffer_frames
                .map_or(cpal::BufferSize::Default, cpal::BufferSize::Fixed),
        };
        renderer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .configure(attempt.sample_rate, channels);
        match build(
            &device,
            &config,
            format,
            Arc::clone(renderer),
            clock.clone(),
            Arc::clone(shared),
            options.max_block_frames,
        ) {
            Ok(stream) => {
                clock.restart(attempt.sample_rate);
                shared.failed.store(false, Ordering::Release);
                if !paused {
                    stream.play().map_err(OpenError::from)?;
                }
                log::info!(
                    "audio output: {name} at {} Hz, {channels} channels, {}, through {}",
                    attempt.sample_rate,
                    attempt
                        .buffer_frames
                        .map_or("the driver's buffer".to_owned(), |frames| format!(
                            "{frames} frames"
                        )),
                    host.id().name()
                );
                return Ok(Live {
                    stream,
                    device: name,
                    id,
                    sample_rate: attempt.sample_rate,
                    channels,
                    default_at_open,
                });
            }
            Err(error) => {
                log::debug!("audio output: {name} refused {config:?} as {format}: {error}");
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
        match plan::choose(&listed, wanted, direction) {
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

/// The id of the device behind cpal's PipeWire default devices, which name
/// none: PipeWire's PulseAudio server's default sink or source. `None`
/// without that server, or on other hosts, which open a device of their own.
fn server_default(host: &cpal::Host, direction: Direction) -> Option<String> {
    #[cfg(target_os = "linux")]
    if host.id() == cpal::HostId::PipeWire
        && cpal::available_hosts().contains(&cpal::HostId::PulseAudio)
    {
        let pulse = cpal::host_from_id(cpal::HostId::PulseAudio).ok()?;
        return default_device(&pulse, direction).map(|device| own_id(&device));
    }
    let _ = (host, direction);
    None
}

/// The id to report for an opened device: its own, or for a PipeWire
/// default device the device it stands for just now, when that can be found.
fn reported_id(host: &cpal::Host, device: &cpal::Device, direction: Direction) -> DeviceId {
    let id = own_id(device);
    if plan::is_pipewire_default(&host.id().to_string(), &id)
        && let Some(server) = server_default(host, direction)
    {
        return DeviceId::new(server);
    }
    DeviceId::new(id)
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
            server_default(&host, direction)
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

/// Builds a stream that renders through `renderer` in the device's sample
/// format.
fn build<R: Render>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: cpal::SampleFormat,
    renderer: Arc<Mutex<R>>,
    clock: Clock,
    shared: Arc<Shared>,
    max_block_frames: Option<u32>,
) -> Result<cpal::Stream, cpal::Error> {
    use cpal::SampleFormat as F;
    let args = (device, config, renderer, clock, shared, max_block_frames);
    match format {
        F::F32 => typed::<f32, R>(args),
        F::F64 => typed::<f64, R>(args),
        F::I8 => typed::<i8, R>(args),
        F::I16 => typed::<i16, R>(args),
        F::I24 => typed::<cpal::I24, R>(args),
        F::I32 => typed::<i32, R>(args),
        F::I64 => typed::<i64, R>(args),
        F::U8 => typed::<u8, R>(args),
        F::U16 => typed::<u16, R>(args),
        F::U24 => typed::<cpal::U24, R>(args),
        F::U32 => typed::<u32, R>(args),
        F::U64 => typed::<u64, R>(args),
        other => Err(cpal::Error::with_message(
            cpal::ErrorKind::UnsupportedConfig,
            format!("unsupported sample format {other}"),
        )),
    }
}

type BuildArgs<'a, R> = (
    &'a cpal::Device,
    &'a cpal::StreamConfig,
    Arc<Mutex<R>>,
    Clock,
    Arc<Shared>,
    Option<u32>,
);

fn typed<T, R>(
    (device, config, renderer, clock, shared, max_block): BuildArgs<'_, R>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
    R: Render,
{
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    let mut scratch: Vec<f32> = Vec::new();
    let errors = Arc::clone(&shared);
    let mut timeline = plan::Timeline::default();
    #[cfg(target_os = "linux")]
    let mut promoter = {
        let realtime = Realtime::of(device);
        crate::realtime::Promoter::start(realtime.eligible, &realtime.device)
    };
    device.build_output_stream::<T, _, _>(
        *config,
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
            #[cfg(target_os = "windows")]
            crate::mmcss::join_once();
            #[cfg(target_os = "linux")]
            promoter.on_callback();
            scratch.clear();
            scratch.resize(data.len(), 0.0);
            // The app's thread only takes the lock while (re)opening, before
            // the stream starts; a contended lock renders silence rather
            // than wait.
            let rendered = match renderer.try_lock() {
                Ok(mut renderer) => {
                    for (start, end) in plan::blocks(scratch.len(), channels, max_block) {
                        renderer.render(&mut scratch[start..end]);
                    }
                    true
                }
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    let mut renderer = poisoned.into_inner();
                    for (start, end) in plan::blocks(scratch.len(), channels, max_block) {
                        renderer.render(&mut scratch[start..end]);
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
