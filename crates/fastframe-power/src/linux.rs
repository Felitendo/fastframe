//! Linux: the power-saver profile, and sleep and wake from logind.
//!
//! Each runs on a thread of its own over D-Bus, so a slow or absent bus
//! never holds up the app.
//!
//! - The power profile comes from the desktop portal's PowerProfileMonitor,
//!   which works inside Flatpak, or else from power-profiles-daemon.
//! - logind announces sleep with `PrepareForSleep`. A "delay" inhibitor lock
//!   gives the app a moment to save first; inside Flatpak it needs
//!   `--system-talk-name=org.freedesktop.login1`, and without it the app
//!   still hears about the wake.
//! - Keeping the computer awake asks the portal's Inhibit for Suspend,
//!   which GNOME and KDE honour for their automatic suspend and which works
//!   inside Flatpak, or else takes an "idle" block lock from logind.
//!
//! <https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.PowerProfileMonitor.html>
//! <https://systemd.io/INHIBITOR_LOCKS/>

use std::sync::Arc;

use zbus::blocking::{Connection, Proxy};

use crate::map::saves;
use crate::{Event, SUSPEND_ACK, Shared};

/// The threads run for the life of the process; nothing to undo.
pub(crate) struct Reader;

impl Reader {
    pub(crate) fn start(shared: Arc<Shared>) -> Result<Self, String> {
        let saver = Arc::clone(&shared);
        spawn("fastframe-power-profile", move || {
            if let Err(error) = watch_saver(&saver) {
                log::debug!("the power profile is unavailable: {error}");
            }
        })?;
        spawn("fastframe-sleep", move || {
            if let Err(error) = watch_sleep(&shared) {
                log::debug!("sleep and wake are unavailable: {error}");
            }
        })?;
        Ok(Self)
    }
}

fn spawn(name: &str, run: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(run)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn watch_saver(shared: &Shared) -> zbus::Result<()> {
    match watch_portal_saver(shared) {
        Ok(()) => Ok(()),
        Err(error) => {
            log::debug!("no power profile portal ({error}); asking power-profiles-daemon");
            watch_daemon_profile(shared)
        }
    }
}

/// The portal's `power-saver-enabled` property, and its changes.
fn watch_portal_saver(shared: &Shared) -> zbus::Result<()> {
    let connection = Connection::session()?;
    let proxy = Proxy::new(
        &connection,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.PowerProfileMonitor",
    )?;
    let changes = proxy.receive_property_changed::<bool>("power-saver-enabled");
    let current: bool = proxy.get_property("power-saver-enabled")?;
    shared.update(|conditions| conditions.saver = current);
    for change in changes {
        if let Ok(on) = change.get() {
            shared.update(|conditions| conditions.saver = on);
        }
    }
    Ok(())
}

/// power-profiles-daemon's `ActiveProfile`, under its current name or the
/// one it had before 0.20.
fn watch_daemon_profile(shared: &Shared) -> zbus::Result<()> {
    let connection = Connection::system()?;
    let proxy = Proxy::new(
        &connection,
        "org.freedesktop.UPower.PowerProfiles",
        "/org/freedesktop/UPower/PowerProfiles",
        "org.freedesktop.UPower.PowerProfiles",
    )
    .and_then(|proxy| proxy.get_property::<String>("ActiveProfile").map(|_| proxy))
    .or_else(|_| {
        Proxy::new(
            &connection,
            "net.hadess.PowerProfiles",
            "/net/hadess/PowerProfiles",
            "net.hadess.PowerProfiles",
        )
    })?;
    let changes = proxy.receive_property_changed::<String>("ActiveProfile");
    let current: String = proxy.get_property("ActiveProfile")?;
    shared.update(|conditions| conditions.saver = saves(&current));
    for change in changes {
        if let Ok(profile) = change.get() {
            shared.update(|conditions| conditions.saver = saves(&profile));
        }
    }
    Ok(())
}

/// logind's `PrepareForSleep`, holding a delay lock while awake.
fn watch_sleep(shared: &Shared) -> zbus::Result<()> {
    let connection = Connection::system()?;
    let manager = Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )?;
    let signals = manager.receive_signal("PrepareForSleep")?;
    let who = who();
    let mut lock = inhibit(&manager, &who);
    for message in signals {
        let Ok(sleeping) = message.body().deserialize::<bool>() else {
            continue;
        };
        if sleeping {
            shared.push_and_wait(Event::Suspending, SUSPEND_ACK);
            // Closing the lock lets the system sleep.
            lock.take();
        } else {
            shared.push(Event::Resumed);
            lock = inhibit(&manager, &who);
        }
    }
    Ok(())
}

/// Who holds the lock, as `systemd-inhibit --list` shows it: the program's
/// name.
fn who() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "fastframe".to_owned())
}

/// The portal's Inhibit flag for suspending the session.
/// <https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Inhibit.html>
const PORTAL_SUSPEND: u32 = 4;

/// Keeps the computer from sleeping on its own until dropped: a thread asks
/// the desktop portal, or else logind, and holds what it got until the
/// sender is dropped.
pub(crate) struct KeepAwake {
    _release: std::sync::mpsc::Sender<()>,
}

impl KeepAwake {
    pub(crate) fn begin(reason: &str) -> Self {
        let (release, released) = std::sync::mpsc::channel::<()>();
        let reason = reason.to_owned();
        let started = spawn("fastframe-keep-awake", move || {
            hold_awake(&reason, &released);
        });
        if let Err(error) = started {
            log::debug!("cannot keep the computer awake: {error}");
        }
        Self { _release: release }
    }
}

/// Inhibits idle sleep, waits for the release, then lets it go.
fn hold_awake(reason: &str, released: &std::sync::mpsc::Receiver<()>) {
    match portal_inhibit(reason) {
        Ok((connection, request)) => {
            // Blocks until the `KeepAwake` is dropped.
            let _ = released.recv();
            let closed = Proxy::new(
                &connection,
                "org.freedesktop.portal.Desktop",
                request.as_ref(),
                "org.freedesktop.portal.Request",
            )
            .and_then(|proxy| proxy.call::<_, _, ()>("Close", &()));
            if let Err(error) = closed {
                // Closing the connection, as this thread now does, ends it too.
                log::debug!("the portal's inhibit did not close: {error}");
            }
        }
        Err(error) => {
            log::debug!("no inhibit portal ({error}); asking logind");
            match logind_inhibit(reason) {
                Ok(lock) => {
                    let _ = released.recv();
                    drop(lock);
                }
                Err(error) => log::debug!("cannot keep the computer awake: {error}"),
            }
        }
    }
}

/// The portal's Inhibit with the Suspend flag. The inhibition lasts until
/// its request is closed or the connection ends.
fn portal_inhibit(reason: &str) -> zbus::Result<(Connection, zbus::zvariant::OwnedObjectPath)> {
    let connection = Connection::session()?;
    let proxy = Proxy::new(
        &connection,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Inhibit",
    )?;
    let mut options = std::collections::HashMap::new();
    options.insert("reason", zbus::zvariant::Value::from(reason));
    let request: zbus::zvariant::OwnedObjectPath =
        proxy.call("Inhibit", &("", PORTAL_SUSPEND, options))?;
    Ok((connection, request))
}

/// An "idle" block lock from logind, held as long as its descriptor is
/// open.
fn logind_inhibit(reason: &str) -> zbus::Result<zbus::zvariant::OwnedFd> {
    let connection = Connection::system()?;
    let manager = Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )?;
    manager.call("Inhibit", &("idle", who().as_str(), reason, "block"))
}

/// Takes a "delay" lock on sleep, or gets on without one.
fn inhibit(manager: &Proxy<'_>, who: &str) -> Option<zbus::zvariant::OwnedFd> {
    manager
        .call(
            "Inhibit",
            &("sleep", who, "Saves its state before sleeping", "delay"),
        )
        .inspect_err(|error| log::debug!("no delay before sleep: {error}"))
        .ok()
}
