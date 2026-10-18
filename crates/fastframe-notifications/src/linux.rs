use crate::{App, Error, Event, Notification, Permission, Sound, worker};
use futures_lite::StreamExt as _;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use zbus::{Proxy, zvariant::Value};

pub(crate) struct Native;
struct Session {
    proxy: Proxy<'static>,
    ids: RefCell<crate::mapping::History>,
    markup: bool,
}
thread_local! { static SESSION: RefCell<Option<Rc<Session>>> = const { RefCell::new(None) }; }
impl Session {
    fn remembered(&self, app: &App, key: &str) -> Option<u32> {
        self.ids.borrow().get(&app.id, key)
    }
    fn remember(&self, app: &App, key: &str, id: u32) {
        self.ids.borrow_mut().remember(&app.id, key, id);
    }
    fn forget(&self, app: &App, key: &str, expected: u32) {
        self.ids.borrow_mut().forget(&app.id, key, expected);
    }
    async fn post(&self, app: &App, value: &Notification, replace: u32) -> Result<u32, Error> {
        let desktop_entry = fastframe_desktop::desktop_entry(&app.desktop_entry);
        let body = if self.markup {
            crate::mapping::escape(&value.body)
        } else {
            value.body.clone()
        };
        let icon = app_icon(app);
        let image = value.image.as_ref().map(|path| path.to_string_lossy());
        let mut hints = HashMap::<&str, Value<'_>>::new();
        hints.insert("desktop-entry", Value::from(desktop_entry.as_str()));
        if let Some(category) = &value.category {
            hints.insert("category", Value::from(category.as_str()));
        }
        if let Some(image) = &image {
            hints.insert("image-path", Value::from(image.as_ref()));
        }
        if value.persistent || value.progress.is_some() {
            hints.insert("transient", Value::from(true));
        }
        if let Some(percent) = value.progress {
            hints.insert("value", Value::from(i32::from(percent)));
        }
        match &value.sound {
            Sound::Silent => {
                hints.insert("suppress-sound", Value::from(true));
            }
            Sound::Default => {
                hints.insert("sound-name", Value::from("message-new-instant"));
            }
            Sound::Named(name) => {
                hints.insert("sound-name", Value::from(name.as_str()));
            }
        }
        let mut actions: Vec<&str> = value
            .actions
            .iter()
            .flat_map(|(id, label)| [id.as_str(), label.as_str()])
            .collect();
        if !value.actions.iter().any(|(id, _)| id == "default") {
            actions.extend(["default", "Open"]);
        }
        let timeout: i32 = if value.persistent { 0 } else { -1 };
        self.proxy
            .call(
                "Notify",
                &(
                    &app.name,
                    replace,
                    &icon,
                    &value.title,
                    body,
                    actions,
                    hints,
                    timeout,
                ),
            )
            .await
            .map_err(|_| unavailable(self))
    }
    async fn close(&self, id: u32) -> Result<(), Error> {
        self.proxy
            .call::<_, _, ()>("CloseNotification", &(id,))
            .await
            .map_err(|_| unavailable(self))
    }
}
/// The theme icon while one is installed, otherwise the app's logo file.
fn app_icon(app: &App) -> String {
    crate::mapping::linux_app_icon(app, |name| {
        let folders = crate::mapping::linux_icon_folders(
            std::env::var_os("HOME").map(Into::into),
            std::env::var_os("XDG_DATA_HOME").map(Into::into),
            std::env::var_os("XDG_DATA_DIRS"),
        );
        crate::mapping::linux_icon_installed(
            name,
            &folders,
            |folder| {
                std::fs::read_dir(folder)
                    .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
                    .unwrap_or_default()
            },
            std::path::Path::is_file,
        )
    })
}
fn unavailable(failed: &Session) -> Error {
    SESSION.with_borrow_mut(|current| {
        if current
            .as_ref()
            .is_some_and(|current| std::ptr::eq(current.as_ref(), failed))
        {
            *current = None;
        }
    });
    Error::Delivery
}

async fn session() -> Result<Rc<Session>, Error> {
    if let Some(session) = SESSION.with_borrow(Clone::clone) {
        return Ok(session);
    }
    let connection = zbus::Connection::session()
        .await
        .map_err(|_| Error::Delivery)?;
    let bus = zbus::fdo::DBusProxy::new(&connection)
        .await
        .map_err(|_| Error::Delivery)?;
    // Bind IDs and signal subscriptions to this unique server instance. A
    // restarted daemon may reuse integers; it must never receive an old close.
    // Match D-Bus auto-start behavior before resolving its unique owner.
    let _ = bus
        .start_service_by_name(
            "org.freedesktop.Notifications"
                .try_into()
                .map_err(|_| Error::Delivery)?,
            0,
        )
        .await;
    let owner = bus
        .get_name_owner(
            "org.freedesktop.Notifications"
                .try_into()
                .map_err(|_| Error::Delivery)?,
        )
        .await
        .map_err(|_| Error::Delivery)?;
    let proxy = Proxy::new_owned(
        connection,
        owner,
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await
    .map_err(|_| Error::Delivery)?;
    let capabilities: Vec<String> = proxy
        .call("GetCapabilities", &())
        .await
        .map_err(|_| Error::Delivery)?;
    let markup = capabilities
        .iter()
        .any(|capability| capability == "body-markup");
    let session = Rc::new(Session {
        proxy,
        ids: RefCell::default(),
        markup,
    });
    SESSION.with_borrow_mut(|current| *current = Some(Rc::clone(&session)));
    Ok(session)
}
impl worker::Backend for Native {
    type Delivery = Delivered;
    async fn show(&self, app: &App, key: &str, value: &Notification) -> Result<Delivered, Error> {
        let session = session().await?;
        // Subscribe before posting, so even an immediate action is observed.
        let actions = session
            .proxy
            .receive_signal("ActionInvoked")
            .await
            .map_err(|_| unavailable(&session))?;
        let closed = session
            .proxy
            .receive_signal("NotificationClosed")
            .await
            .map_err(|_| unavailable(&session))?;
        let id = session
            .post(app, value, session.remembered(app, key).unwrap_or(0))
            .await?;
        session.remember(app, key, id);
        let observed = Rc::clone(&session);
        let identity = app.clone();
        let observed_key = key.to_owned();
        let response: worker::Response = Box::pin(async move {
            let mut messages = futures_lite::stream::or(actions, closed);
            while let Some(message) = messages.next().await {
                let event = match message.header().member().map(|name| name.as_str()) {
                    Some("ActionInvoked") => {
                        message.body().deserialize::<(u32, String)>().ok().and_then(
                            |(number, action)| {
                                (number == id).then(|| {
                                    if action == "default" {
                                        Event::Activated
                                    } else {
                                        Event::Action(action)
                                    }
                                })
                            },
                        )
                    }
                    Some("NotificationClosed") => message
                        .body()
                        .deserialize::<(u32, u32)>()
                        .ok()
                        .and_then(|(number, _)| (number == id).then_some(Event::Dismissed)),
                    _ => None,
                };
                if let Some(event) = event {
                    observed.forget(&identity, &observed_key, id);
                    return event;
                }
            }
            Event::Failed(unavailable(&observed))
        });
        Ok(Delivered {
            app: app.clone(),
            key: key.into(),
            session,
            id,
            response: Some(response),
        })
    }
    async fn permission(&self, _app: &App, _request: bool) -> Result<Permission, Error> {
        let session = session().await?;
        let _: Vec<String> = session
            .proxy
            .call("GetCapabilities", &())
            .await
            .map_err(|_| unavailable(&session))?;
        Ok(Permission::Unknown)
    }
    async fn withdraw(&self, app: &App, key: &str) -> Result<(), Error> {
        let session = session().await?;
        if let Some(id) = session.remembered(app, key) {
            session.close(id).await?;
            session.forget(app, key, id);
        }
        Ok(())
    }
}
pub(crate) struct Delivered {
    app: App,
    key: String,
    session: Rc<Session>,
    id: u32,
    response: Option<worker::Response>,
}
impl worker::Delivery for Delivered {
    fn response(&mut self) -> worker::Response {
        self.response.take().expect("response requested once")
    }
    async fn update(&mut self, value: &Notification) -> Result<(), Error> {
        let updated = self.session.post(&self.app, value, self.id).await?;
        if updated != self.id {
            // Dismissal won the race with progress. Do not resurrect its card.
            self.session.close(updated).await?;
            return Err(Error::Stopped);
        }
        Ok(())
    }
    async fn close(&mut self) -> Result<(), Error> {
        self.session.close(self.id).await?;
        self.session.forget(&self.app, &self.key, self.id);
        Ok(())
    }
}
