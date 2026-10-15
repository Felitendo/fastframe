use crate::{App, Error, Event, Notification, Permission};
use std::{collections::HashMap, future::Future, pin::Pin, rc::Rc, sync::Arc};
use tokio::{
    sync::{OwnedSemaphorePermit, mpsc, oneshot, watch},
    task::JoinSet,
};

pub(crate) type Response = Pin<Box<dyn Future<Output = Event>>>;
pub(crate) trait Delivery {
    fn response(&mut self) -> Response;
    async fn update(&mut self, notification: &Notification) -> Result<(), Error>;
    async fn close(&mut self) -> Result<(), Error>;
}
pub(crate) trait Backend: 'static {
    type Delivery: Delivery;
    async fn show(
        &self,
        app: &App,
        key: &str,
        notification: &Notification,
    ) -> Result<Self::Delivery, Error>;
    async fn permission(&self, app: &App, request: bool) -> Result<Permission, Error>;
    async fn withdraw(&self, app: &App, key: &str) -> Result<(), Error>;
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Close,
    Release,
}
#[derive(Clone)]
pub(crate) struct State {
    pub notification: Notification,
    pub revision: u64,
    pub stop: Option<Stop>,
}
pub(crate) struct Control(pub watch::Sender<State>);
impl Control {
    pub(crate) fn stop(&self, stop: Stop) {
        self.0.send_modify(|state| {
            if state.stop.is_none() {
                state.stop = Some(stop);
            }
        });
    }
}
impl Drop for Control {
    fn drop(&mut self) {
        self.stop(Stop::Release);
    }
}
#[derive(Clone)]
pub(crate) struct Events(pub Arc<dyn Fn(Event) + Send + Sync>);
impl Events {
    fn send(&self, event: Event) {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.0)(event))).is_err() {
            log::warn!("notification event callback panicked");
        }
    }
}
struct Active {
    control: watch::Sender<State>,
    done: oneshot::Receiver<()>,
}
pub(crate) enum Command {
    Show {
        control: watch::Sender<State>,
        state: watch::Receiver<State>,
        events: Events,
        permit: OwnedSemaphorePermit,
    },
    Permission {
        request: bool,
        reply: oneshot::Sender<Result<Permission, Error>>,
        permit: OwnedSemaphorePermit,
    },
    Withdraw(String),
}
pub(crate) fn send_error<T>(error: mpsc::error::TrySendError<T>) -> Error {
    match error {
        mpsc::error::TrySendError::Full(_) => Error::Busy,
        mpsc::error::TrySendError::Closed(_) => Error::Stopped,
    }
}
pub(crate) fn run(app: App, receiver: mpsc::Receiver<Command>) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    tokio::task::LocalSet::new().block_on(
        &runtime,
        service(Rc::new(crate::platform::Native), app, receiver),
    );
}
async fn service<B: Backend>(backend: Rc<B>, app: App, mut receiver: mpsc::Receiver<Command>) {
    let app = Arc::new(app);
    let mut tasks = JoinSet::new();
    let mut active: HashMap<String, Active> = HashMap::new();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut sequence = 0_u64;
    loop {
        tokio::select! {
            command = receiver.recv() => {
                let Some(command) = command else { break; };
                active.retain(|_, entry| matches!(entry.done.try_recv(), Err(oneshot::error::TryRecvError::Empty)));
                match command {
                    Command::Show { control, state, events, permit } => {
                        sequence = sequence.wrapping_add(1);
                        let key = state.borrow().notification.key.clone().unwrap_or_else(|| format!("{nonce:x}-{sequence:x}"));
                        // Serialize equal keys even while the old send is in flight.
                        // Otherwise a slow old request could overwrite a newer one.
                        let (done, finished) = oneshot::channel();
                        let previous = active.insert(key.clone(), Active { control, done: finished });
                        if let Some(old) = previous.as_ref() { old.control.send_modify(|state| { if state.stop.is_none() { state.stop = Some(Stop::Release); } }); }
                        let backend = Rc::clone(&backend);
                        let app = Arc::clone(&app);
                        tasks.spawn_local(async move {
                            let _permit = permit;
                            if let Some(previous) = previous { let _ = previous.done.await; }
                            deliver(backend.as_ref(), &app, &key, state, events).await;
                            let _ = done.send(());
                        });
                    }
                    Command::Permission { request, reply, permit } => {
                        let backend = Rc::clone(&backend);
                        let app = Arc::clone(&app);
                        tasks.spawn_local(async move {
                            let _permit = permit;
                            let _ = reply.send(backend.permission(&app, request).await);
                        });
                    }
                    Command::Withdraw(key) => {
                        if let Some(control) = active.get(&key) { control.control.send_modify(|state| state.stop = Some(Stop::Close)); }
                        else if backend.withdraw(&app, &key).await.is_err() { log::debug!("notification withdrawal failed"); }
                    }
                }
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    // Dropping the client releases observations; OS history stays intact.
    while tasks.join_next().await.is_some() {}
}
async fn deliver<B: Backend>(
    backend: &B,
    app: &App,
    key: &str,
    mut state: watch::Receiver<State>,
    events: Events,
) {
    let initial = state.borrow_and_update().clone();
    if initial.stop == Some(Stop::Close) {
        events.send(Event::Closed);
        return;
    }
    let mut delivery = match backend.show(app, key, &initial.notification).await {
        Ok(delivery) => delivery,
        Err(error) => {
            events.send(Event::Failed(error));
            return;
        }
    };
    events.send(Event::Shown);
    let mut response = delivery.response();
    let mut revision = initial.revision;
    loop {
        let latest = state.borrow_and_update().clone();
        match latest.stop {
            Some(Stop::Close) => {
                match delivery.close().await {
                    Ok(()) => events.send(Event::Closed),
                    Err(error) => events.send(Event::Failed(error)),
                }
                return;
            }
            Some(Stop::Release) => {}
            None => {}
        }
        if latest.revision != revision {
            if let Err(error) = delivery.update(&latest.notification).await {
                events.send(Event::Failed(error));
                return;
            }
            revision = latest.revision;
        }
        // A completion update followed immediately by dropping the handle
        // must reach the OS before observation is released.
        if latest.stop == Some(Stop::Release) {
            return;
        }
        tokio::select! {
            biased;
            changed = state.changed() => { if changed.is_err() { return; } }
            event = &mut response => { events.send(event); return; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc, sync::Mutex};
    #[derive(Default)]
    struct Fake {
        calls: Rc<RefCell<Vec<String>>>,
        gate: Option<Rc<tokio::sync::Notify>>,
        fail: bool,
    }
    struct FakeDelivery(Rc<RefCell<Vec<String>>>);
    impl Backend for Fake {
        type Delivery = FakeDelivery;
        async fn show(
            &self,
            _app: &App,
            _key: &str,
            notification: &Notification,
        ) -> Result<FakeDelivery, Error> {
            self.calls
                .borrow_mut()
                .push(format!("show {}", notification.title));
            if let Some(gate) = &self.gate {
                gate.notified().await;
            }
            if self.fail {
                return Err(Error::Delivery);
            }
            Ok(FakeDelivery(Rc::clone(&self.calls)))
        }
        async fn permission(&self, _app: &App, _request: bool) -> Result<Permission, Error> {
            Ok(Permission::Unknown)
        }
        async fn withdraw(&self, _app: &App, _key: &str) -> Result<(), Error> {
            Ok(())
        }
    }
    impl Delivery for FakeDelivery {
        fn response(&mut self) -> Response {
            Box::pin(std::future::pending())
        }
        async fn update(&mut self, notification: &Notification) -> Result<(), Error> {
            self.0
                .borrow_mut()
                .push(format!("update {}", notification.title));
            Ok(())
        }
        async fn close(&mut self) -> Result<(), Error> {
            self.0.borrow_mut().push("close".into());
            Ok(())
        }
    }
    fn state() -> (Arc<Control>, watch::Receiver<State>) {
        let (tx, rx) = watch::channel(State {
            notification: Notification::new("start"),
            revision: 0,
            stop: None,
        });
        (Arc::new(Control(tx)), rx)
    }
    fn events() -> (Events, Arc<Mutex<Vec<Event>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let copy = Arc::clone(&seen);
        (
            Events(Arc::new(move |event| {
                copy.lock().expect("events").push(event)
            })),
            seen,
        )
    }
    #[tokio::test]
    async fn closing_before_delivery_suppresses_the_notification() {
        let fake = Fake::default();
        let (control, state) = state();
        control.stop(Stop::Close);
        let (events, seen) = events();
        deliver(&fake, &App::new("test", "Test"), "key", state, events).await;
        assert!(fake.calls.borrow().is_empty());
        assert_eq!(*seen.lock().expect("events"), [Event::Closed]);
    }
    #[tokio::test]
    async fn releasing_before_delivery_still_posts_to_desktop_history() {
        let fake = Fake::default();
        let (control, state) = state();
        drop(control);
        let (events, seen) = events();
        deliver(&fake, &App::new("test", "Test"), "key", state, events).await;
        assert_eq!(*fake.calls.borrow(), ["show start"]);
        assert_eq!(*seen.lock().expect("events"), [Event::Shown]);
    }
    #[tokio::test]
    async fn closing_during_delivery_withdraws_after_acceptance() {
        let gate = Rc::new(tokio::sync::Notify::new());
        let fake = Fake {
            gate: Some(Rc::clone(&gate)),
            ..Fake::default()
        };
        let (control, state) = state();
        let (events, seen) = events();
        let app = App::new("test", "Test");
        tokio::join!(deliver(&fake, &app, "key", state, events), async {
            control.stop(Stop::Close);
            gate.notify_one();
        });
        assert_eq!(*fake.calls.borrow(), ["show start", "close"]);
        assert_eq!(*seen.lock().expect("events"), [Event::Shown, Event::Closed]);
    }
    #[tokio::test]
    async fn updates_coalesce_and_completion_is_flushed_before_release() {
        let gate = Rc::new(tokio::sync::Notify::new());
        let fake = Fake {
            gate: Some(Rc::clone(&gate)),
            ..Fake::default()
        };
        let (control, state) = state();
        let (events, _) = events();
        let app = App::new("test", "Test");
        tokio::join!(deliver(&fake, &app, "key", state, events), async {
            for title in ["halfway", "done"] {
                control.0.send_modify(|state| {
                    state.notification = Notification::new(title);
                    state.revision += 1;
                });
            }
            drop(control);
            gate.notify_one();
        });
        assert_eq!(*fake.calls.borrow(), ["show start", "update done"]);
    }
    #[tokio::test]
    async fn delivery_failure_does_not_claim_success() {
        let fake = Fake {
            fail: true,
            ..Fake::default()
        };
        let (_control, state) = state();
        let (events, seen) = events();
        deliver(&fake, &App::new("test", "Test"), "key", state, events).await;
        assert_eq!(
            *seen.lock().expect("events"),
            [Event::Failed(Error::Delivery)]
        );
    }
    #[test]
    fn close_is_not_overwritten_by_drop() {
        let (control, state) = state();
        control.stop(Stop::Close);
        drop(control);
        assert!(state.borrow().stop == Some(Stop::Close));
    }
    #[test]
    fn a_callback_panic_cannot_end_the_worker() {
        Events(Arc::new(|_| panic!("app callback"))).send(Event::Shown);
    }
}

#[cfg(test)]
mod service_tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    struct SerialBackend {
        started: Rc<RefCell<Vec<String>>>,
        gate: Rc<tokio::sync::Notify>,
    }
    struct Sent;
    impl Backend for SerialBackend {
        type Delivery = Sent;
        async fn show(&self, _: &App, _: &str, notification: &Notification) -> Result<Sent, Error> {
            self.started.borrow_mut().push(notification.title.clone());
            self.gate.notified().await;
            Ok(Sent)
        }
        async fn permission(&self, _: &App, _: bool) -> Result<Permission, Error> {
            Ok(Permission::Unknown)
        }
        async fn withdraw(&self, _: &App, _: &str) -> Result<(), Error> {
            Ok(())
        }
    }
    impl Delivery for Sent {
        fn response(&mut self) -> Response {
            Box::pin(std::future::pending())
        }
        async fn update(&mut self, _: &Notification) -> Result<(), Error> {
            Ok(())
        }
        async fn close(&mut self) -> Result<(), Error> {
            Ok(())
        }
    }
    fn command(title: &str) -> Command {
        let (control, state) = watch::channel(State {
            notification: Notification::new(title).key("same"),
            revision: 0,
            stop: Some(Stop::Release),
        });
        Command::Show {
            control,
            state,
            events: Events(Arc::new(|_| {})),
            permit: Arc::new(tokio::sync::Semaphore::new(1))
                .try_acquire_owned()
                .expect("permit"),
        }
    }
    #[tokio::test]
    async fn a_slow_old_delivery_cannot_overwrite_its_replacement() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let started = Rc::new(RefCell::new(Vec::new()));
                let gate = Rc::new(tokio::sync::Notify::new());
                let backend = Rc::new(SerialBackend {
                    started: Rc::clone(&started),
                    gate: Rc::clone(&gate),
                });
                let (sender, receiver) = mpsc::channel(8);
                sender.send(command("old")).await.expect("queue");
                sender.send(command("new")).await.expect("queue");
                drop(sender);
                let backend = Rc::new(RcBackend(backend));
                let task =
                    tokio::task::spawn_local(service(backend, App::new("test", "Test"), receiver));
                tokio::time::timeout(std::time::Duration::from_secs(1), async {
                    while started.borrow().is_empty() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("old send starts");
                assert_eq!(*started.borrow(), ["old"]);
                gate.notify_one();
                tokio::time::timeout(std::time::Duration::from_secs(1), async {
                    while started.borrow().len() != 2 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("new send starts after old finishes");
                assert_eq!(*started.borrow(), ["old", "new"]);
                gate.notify_one();
                task.await.expect("worker");
            })
            .await;
    }
    // A local backend intentionally uses Rc; the worker never sends it to a thread.
    struct RcBackend(Rc<SerialBackend>);
    impl Backend for RcBackend {
        type Delivery = Sent;
        async fn show(
            &self,
            app: &App,
            key: &str,
            notification: &Notification,
        ) -> Result<Sent, Error> {
            self.0.show(app, key, notification).await
        }
        async fn permission(&self, app: &App, request: bool) -> Result<Permission, Error> {
            self.0.permission(app, request).await
        }
        async fn withdraw(&self, app: &App, key: &str) -> Result<(), Error> {
            self.0.withdraw(app, key).await
        }
    }
}
