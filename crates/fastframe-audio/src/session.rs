//! The state of one stream on one device, whichever way it goes: pausing and
//! resuming it, and keeping it on a working device. [`Output`](crate::Output)
//! and [`Input`](crate::Input) are this with the app's renderer or capturer.

use std::fmt;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::plan::{self, Direction};
use crate::stream::{self, DefaultWatch, Engine, Live, Settings, Shared};
use crate::{Clock, Device, DeviceId, Maintained, OpenError, OutputError, Reason};

pub(crate) struct Session<E: Engine> {
    settings: Settings,
    engine: Arc<Mutex<E>>,
    live: Option<Live>,
    shared: Arc<Shared>,
    clock: Clock,
    paused: bool,
    paused_at: Option<Instant>,
    /// A reopen [`resume`](Self::resume) made, for the next `maintain` to report.
    resumed: bool,
    /// Whether the clock still moves while running.
    stall: plan::Stall,
    /// How long the renderer has had nothing to play.
    idle: plan::Idle,
    watch: Option<DefaultWatch>,
    device: String,
    id: DeviceId,
    sample_rate: u32,
    channels: u16,
}

impl<E: Engine> fmt::Debug for Session<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match E::DIRECTION {
            Direction::Output => "Output",
            Direction::Input => "Input",
        };
        f.debug_struct(name)
            .field("device", &self.device)
            .field("id", &self.id)
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("paused", &self.paused)
            .field("open", &self.live.is_some())
            .finish_non_exhaustive()
    }
}

impl<E: Engine> Session<E> {
    pub(crate) fn open(settings: Settings, engine: E) -> Result<Self, OpenError> {
        let engine = Arc::new(Mutex::new(engine));
        let shared = Arc::new(Shared::new(E::DIRECTION));
        let clock = Clock::default();
        let live = stream::open(&settings, &engine, &clock, &shared, false)?;
        let watch = (settings.follow_default
            && settings.device == Device::Default
            && cfg!(any(target_os = "macos", windows)))
        .then(|| DefaultWatch::start(live.default_at_open.clone(), E::DIRECTION));
        Ok(Self {
            device: live.device.clone(),
            id: live.id.clone(),
            sample_rate: live.sample_rate,
            channels: live.channels,
            settings,
            engine,
            live: Some(live),
            shared,
            clock,
            paused: false,
            paused_at: None,
            resumed: false,
            stall: plan::Stall::new(Duration::ZERO, Instant::now()),
            idle: plan::Idle::default(),
            watch,
        })
    }

    pub(crate) fn pause(&mut self) {
        if self.paused {
            return;
        }
        self.paused = true;
        self.paused_at = Some(Instant::now());
        self.idle = plan::Idle::default();
        self.shared.epoch.fetch_add(1, Ordering::Relaxed);
        if let Some(live) = &self.live {
            use cpal::traits::StreamTrait;
            if let Err(error) = live.stream.pause() {
                log::warn!("cannot pause the audio {}: {error}", E::DIRECTION.label());
                self.shared.report(error);
            }
        }
    }

    pub(crate) fn resume(&mut self) {
        // The app has just given the renderer something to play, so what it
        // said before is stale, even while playing: the next `maintain`
        // must not pause before the callback has rendered it.
        self.idle = plan::Idle::default();
        self.shared.idle.store(false, Ordering::Relaxed);
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
                    log::warn!("cannot restart the audio {}: {error}", E::DIRECTION.label());
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

    pub(crate) fn is_paused(&self) -> bool {
        self.paused
    }

    pub(crate) fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Acquire) || (self.live.is_none() && !self.paused)
    }

    pub(crate) fn maintain(&mut self) -> Maintained {
        if std::mem::take(&mut self.resumed) {
            return self.reopened(Reason::Resumed);
        }
        if let Some(after) = self.settings.pause_when_idle
            && !self.paused
            && self.live.is_some()
            && self.idle.observe(
                self.shared.idle.load(Ordering::Relaxed),
                Instant::now(),
                after,
            )
        {
            log::debug!(
                "audio {}: nothing to play for {after:?}; pausing",
                E::DIRECTION.label()
            );
            self.pause();
        }
        if self.paused
            && self.live.is_some()
            && plan::should_release(self.paused_at, Instant::now(), self.settings.release_after)
        {
            log::info!(
                "audio {}: letting {} go after a long pause",
                E::DIRECTION.label(),
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
            log::warn!(
                "audio {}: {} stopped running; reopening",
                E::DIRECTION.label(),
                self.device
            );
            Some(Reason::Stalled)
        } else {
            None
        };
        let Some(reason) = reason else {
            return Maintained::Unchanged;
        };
        if self.paused {
            // Nothing is running: let the device go, and open the right one
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

    pub(crate) fn take_errors(&mut self) -> Vec<OutputError> {
        std::mem::take(
            &mut *self
                .shared
                .errors
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    pub(crate) fn clock(&self) -> Clock {
        self.clock.clone()
    }

    pub(crate) fn device_name(&self) -> &str {
        &self.device
    }

    pub(crate) fn device_id(&self) -> &DeviceId {
        &self.id
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub(crate) fn channels(&self) -> u16 {
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

    /// Closes the stream, if any, and opens the one the settings ask for.
    fn reopen(&mut self) -> Result<(), OpenError> {
        // Let the old device go first: some only take one stream.
        self.live = None;
        match stream::open(
            &self.settings,
            &self.engine,
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
                log::warn!("audio {}: {error}", E::DIRECTION.label());
                Err(error)
            }
        }
    }

    /// Whether the stream is running but its clock has stood still for
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
