//! `SMAppService`: the main app as a login item for a start with its window,
//! the bundled agent for a hidden one.
#![allow(unsafe_code)]

use std::io;

use objc2::rc::Retained;
use objc2::runtime::AnyClass;
use objc2_foundation::{NSBundle, NSString};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use crate::{Autostart, ServiceStatus, Start, State, service_state};

/// The two services of a bundled app on macOS 13 or later.
struct Services {
    main: Retained<SMAppService>,
    agent: Retained<SMAppService>,
}

impl Services {
    /// `None` for an executable outside an `.app` bundle, which has no login
    /// item to register, and before macOS 13, which has no `SMAppService`.
    fn new() -> Option<Self> {
        AnyClass::get(c"SMAppService")?;
        let bundle = NSBundle::mainBundle();
        if !bundle.bundlePath().to_string().ends_with(".app") {
            return None;
        }
        let id = bundle.bundleIdentifier()?.to_string();
        let plist = NSString::from_str(&crate::macos_agent_plist_name(&id));
        // SAFETY: the class exists (checked above), and both constructors
        // take any name; a missing plist reports `NotFound` as its status.
        let (main, agent) = unsafe {
            (
                SMAppService::mainAppService(),
                SMAppService::agentServiceWithPlistName(&plist),
            )
        };
        Some(Self { main, agent })
    }
}

fn status(service: &SMAppService) -> ServiceStatus {
    // SAFETY: a plain property read on a live service.
    let status = unsafe { service.status() };
    match status {
        SMAppServiceStatus::Enabled => ServiceStatus::Enabled,
        SMAppServiceStatus::RequiresApproval => ServiceStatus::RequiresApproval,
        SMAppServiceStatus::NotFound => ServiceStatus::NotFound,
        _ => ServiceStatus::NotRegistered,
    }
}

fn registered(status: ServiceStatus) -> bool {
    matches!(
        status,
        ServiceStatus::Enabled | ServiceStatus::RequiresApproval
    )
}

fn register(service: &SMAppService) -> io::Result<()> {
    if registered(status(service)) {
        return Ok(());
    }
    // SAFETY: a live service; the error comes back as a value.
    unsafe { service.registerAndReturnError() }
        .map_err(|error| io::Error::other(error.localizedDescription().to_string()))
}

fn unregister(service: &SMAppService) -> io::Result<()> {
    if !registered(status(service)) {
        return Ok(());
    }
    // SAFETY: as in `register`. For the agent this kills its running job,
    // which is `open` and long gone, never the app.
    unsafe { service.unregisterAndReturnError() }
        .map_err(|error| io::Error::other(error.localizedDescription().to_string()))
}

pub(crate) fn state(_app: &Autostart) -> State {
    match Services::new() {
        Some(services) => service_state(status(&services.main), status(&services.agent)),
        None => State::Unsupported,
    }
}

pub(crate) fn set(app: &Autostart, start: Option<Start>) -> io::Result<State> {
    let services = Services::new().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "login items need an app bundle on macOS 13 or later",
        )
    })?;
    // The new registration first, so a failure leaves the old one working.
    match start {
        Some(Start::Hidden) => {
            if status(&services.agent) == ServiceStatus::NotFound {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "the bundle carries no launch agent to start hidden",
                ));
            }
            register(&services.agent)?;
            unregister(&services.main)?;
        }
        Some(Start::Window) => {
            register(&services.main)?;
            unregister(&services.agent)?;
        }
        None => {
            unregister(&services.agent)?;
            unregister(&services.main)?;
        }
    }
    Ok(state(app))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test binary is no bundle: nothing is registered or asked.
    #[test]
    fn an_unbundled_run_cannot_start_at_login() {
        let app = Autostart::new("x", "X");
        assert_eq!(state(&app), State::Unsupported);
        assert_eq!(
            set(&app, None).map_err(|error| error.kind()),
            Err(io::ErrorKind::Unsupported)
        );
    }
}
