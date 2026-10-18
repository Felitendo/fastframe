#![allow(
    unsafe_code,
    reason = "registering a portable app's AUMID uses the Win32 registry API, as in ZapFast"
)]
use crate::mapping::windows_xml as xml;
use crate::{App, Error, Event, Notification, Permission, worker};
use tokio::sync::mpsc;
use windows::{
    Data::Xml::Dom::XmlDocument,
    Foundation::TypedEventHandler,
    UI::Notifications::{
        NotificationSetting, ToastActivatedEventArgs, ToastDismissalReason, ToastNotification,
        ToastNotificationManager,
    },
    core::{HSTRING, Interface as _},
};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RegCloseKey, RegCreateKeyW, RegSetValueExW,
};

pub(crate) struct Native;
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn identity(app: &App) -> Result<(), Error> {
    let path = wide(&format!(r"Software\Classes\AppUserModelId\{}", app.id));
    let mut key = std::ptr::null_mut();
    // The validated ID cannot escape the key. Buffers remain alive and are
    // NUL terminated; the opened key is closed on every path after creation.
    let status = unsafe { RegCreateKeyW(HKEY_CURRENT_USER, path.as_ptr(), &mut key) };
    if status != 0 {
        return Err(Error::Delivery);
    }
    let status = crate::mapping::windows_identity(app)
        .iter()
        .map(|(name, value)| {
            let name = wide(name);
            let value = wide(value);
            unsafe {
                RegSetValueExW(
                    key,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    value.as_ptr().cast(),
                    (value.len() * size_of::<u16>()) as u32,
                )
            }
        })
        .find(|status| *status != 0)
        .unwrap_or(0);
    unsafe {
        RegCloseKey(key);
    }
    if status == 0 {
        Ok(())
    } else {
        Err(Error::Delivery)
    }
}
fn tags(key: &str) -> (HSTRING, HSTRING) {
    let (tag, group) = crate::mapping::windows_tags(key);
    (tag.into(), group.into())
}
struct Observed {
    toast: ToastNotification,
    activated: i64,
    dismissed: i64,
    failed: i64,
}
impl Drop for Observed {
    fn drop(&mut self) {
        let _ = self.toast.RemoveActivated(self.activated);
        let _ = self.toast.RemoveDismissed(self.dismissed);
        let _ = self.toast.RemoveFailed(self.failed);
    }
}
fn post(
    app: &App,
    key: &str,
    value: &Notification,
    events: mpsc::UnboundedSender<Event>,
) -> Result<Observed, Error> {
    let document = XmlDocument::new().map_err(|_| Error::Delivery)?;
    document
        .LoadXml(&HSTRING::from(xml(value)))
        .map_err(|_| Error::Delivery)?;
    let toast =
        ToastNotification::CreateToastNotification(&document).map_err(|_| Error::Delivery)?;
    let (tag, group) = tags(key);
    toast
        .SetTag(&tag)
        .and_then(|()| toast.SetGroup(&group))
        .map_err(|_| Error::Delivery)?;
    let activated_events = events.clone();
    let activated = toast
        .Activated(&TypedEventHandler::new(
            move |_, args: windows::core::Ref<'_, windows::core::IInspectable>| {
                let action = args
                    .as_ref()
                    .and_then(|args| args.cast::<ToastActivatedEventArgs>().ok())
                    .and_then(|args| args.Arguments().ok())
                    .map(|arg| arg.to_string())
                    .unwrap_or_default();
                let event = if action.is_empty() || action == "default" {
                    Event::Activated
                } else {
                    Event::Action(action)
                };
                let _ = activated_events.send(event);
                Ok(())
            },
        ))
        .map_err(|_| Error::Delivery)?;
    let dismissed_events = events.clone();
    let dismissed =
        match toast.Dismissed(&TypedEventHandler::new(
            move |_,
                  args: windows::core::Ref<
                '_,
                windows::UI::Notifications::ToastDismissedEventArgs,
            >| {
                if let Some(args) = args.as_ref() {
                    // Expiry of the banner leaves it actionable in Action Center.
                    if args.Reason()? != ToastDismissalReason::TimedOut {
                        let _ = dismissed_events.send(Event::Dismissed);
                    }
                }
                Ok(())
            },
        )) {
            Ok(token) => token,
            Err(_) => {
                let _ = toast.RemoveActivated(activated);
                return Err(Error::Delivery);
            }
        };
    let failed = match toast.Failed(&TypedEventHandler::new(move |_, _| {
        let _ = events.send(Event::Failed(Error::Delivery));
        Ok(())
    })) {
        Ok(token) => token,
        Err(_) => {
            let _ = toast.RemoveActivated(activated);
            let _ = toast.RemoveDismissed(dismissed);
            return Err(Error::Delivery);
        }
    };
    let observed = Observed {
        toast,
        activated,
        dismissed,
        failed,
    };
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(&app.id))
        .and_then(|notifier| notifier.Show(&observed.toast))
        .map_err(|_| Error::Delivery)?;
    Ok(observed)
}
impl worker::Backend for Native {
    type Delivery = Delivered;
    async fn show(
        &self,
        app: &App,
        key: &str,
        notification: &Notification,
    ) -> Result<Delivered, Error> {
        identity(app)?;
        if self.permission(app, false).await? == Permission::Denied {
            return Err(Error::PermissionDenied);
        }
        let (events, receiver) = mpsc::unbounded_channel();
        let observed = post(app, key, notification, events.clone())?;
        Ok(Delivered {
            app: app.clone(),
            key: key.into(),
            observed: Some(observed),
            events,
            receiver: Some(receiver),
        })
    }
    async fn permission(&self, app: &App, _request: bool) -> Result<Permission, Error> {
        identity(app)?;
        let setting = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(&app.id))
            .and_then(|notifier| notifier.Setting())
            .map_err(|_| Error::Delivery)?;
        Ok(if setting == NotificationSetting::Enabled {
            Permission::Granted
        } else {
            Permission::Denied
        })
    }
    async fn withdraw(&self, app: &App, key: &str) -> Result<(), Error> {
        let (tag, group) = tags(key);
        ToastNotificationManager::History()
            .and_then(|history| {
                history.RemoveGroupedTagWithId(&tag, &group, &HSTRING::from(&app.id))
            })
            .map_err(|_| Error::Delivery)
    }
}
pub(crate) struct Delivered {
    app: App,
    key: String,
    observed: Option<Observed>,
    events: mpsc::UnboundedSender<Event>,
    receiver: Option<mpsc::UnboundedReceiver<Event>>,
}
impl worker::Delivery for Delivered {
    fn response(&mut self) -> worker::Response {
        let mut receiver = self.receiver.take().expect("response requested once");
        Box::pin(async move {
            receiver
                .recv()
                .await
                .unwrap_or(Event::Failed(Error::Delivery))
        })
    }
    async fn update(&mut self, notification: &Notification) -> Result<(), Error> {
        self.observed.take();
        self.observed = Some(post(
            &self.app,
            &self.key,
            notification,
            self.events.clone(),
        )?);
        Ok(())
    }
    async fn close(&mut self) -> Result<(), Error> {
        Native.withdraw(&self.app, &self.key).await
    }
}
use worker::Backend as _;
