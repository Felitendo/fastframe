use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

/// A platform that cannot tell holds nothing back.
#[test]
fn the_default_is_unconstrained() {
    let conditions = Conditions::default();
    assert!(conditions.online);
    assert!(!conditions.metered);
    assert!(!conditions.saver);
    assert!(!conditions.on_battery);
    assert!(!conditions.display_off);
    assert!(!conditions.locked);
}

/// Demo runs and tests get a fixed `Power`, and the device's own state
/// (Low Power Mode on a Mac) must not leak into what they see.
#[test]
fn a_fixed_monitor_keeps_its_conditions() {
    let given = Conditions {
        online: false,
        ..Conditions::default()
    };
    let mut power = Power::fixed(given);
    assert_eq!(power.conditions(), given);
    power.set_background(true);
    assert_eq!(power.conditions(), given);
    assert!(power.events().is_empty());
}

#[test]
fn a_change_wakes_the_app_once_and_a_repeat_does_not() {
    let woken = Arc::new(AtomicU32::new(0));
    let count = Arc::clone(&woken);
    let shared = Shared::new(move || {
        count.fetch_add(1, Ordering::Relaxed);
    });
    shared.update(|conditions| conditions.saver = true);
    shared.update(|conditions| conditions.saver = true);
    shared.update(|conditions| conditions.online = false);
    assert_eq!(woken.load(Ordering::Relaxed), 2);
}

/// What an app's own tests and demo runs use in place of a reader.
#[test]
fn tests_can_set_the_conditions_and_send_events() {
    let power = Power::fixed(Conditions::default());
    let metered = Conditions {
        metered: true,
        ..Conditions::default()
    };
    power.set_conditions(metered);
    assert_eq!(power.conditions(), metered);

    let ack = Ack::default();
    power.send(Event::Resumed);
    power.send(Event::Suspending(ack.clone()));
    let events = power.events();
    assert!(matches!(
        events.as_slice(),
        [Event::Resumed, Event::Suspending(_)]
    ));
    assert!(power.events().is_empty(), "each event is taken once");

    if let Some(Event::Suspending(answer)) = events.into_iter().nth(1) {
        answer.done();
    }
    assert!(
        ack.wait(Duration::ZERO),
        "the app's answer reaches the sender"
    );
}

#[test]
fn a_reader_waits_for_the_answer_but_not_forever() {
    let power = Power::fixed(Conditions::default());
    let shared = Arc::clone(&power.shared);
    let answered = std::thread::spawn(move || {
        let started = std::time::Instant::now();
        shared.push_and_wait(Event::Suspending, Duration::from_secs(30));
        started.elapsed()
    });
    let ack = loop {
        if let Some(Event::Suspending(ack)) = power.events().pop() {
            break ack;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    ack.done();
    assert!(answered.join().unwrap() < Duration::from_secs(10));

    let started = std::time::Instant::now();
    power
        .shared
        .push_and_wait(Event::EndingSession, Duration::from_millis(30));
    assert!(started.elapsed() >= Duration::from_millis(25), "gave up");
}

#[test]
fn events_read_plainly_in_the_log() {
    let event = Event::Suspending(Ack::default());
    assert_eq!(format!("{event:?}"), "Suspending(Ack)");
    assert_eq!(format!("{:?}", Activity::begin("test")), "Activity");
}

/// An app keeps both in state that may move between threads (a worker
/// that owns the player, say), on every platform alike.
#[test]
fn power_and_activities_can_move_between_threads() {
    fn send_and_sync<T: Send + Sync>() {}
    send_and_sync::<Power>();
    send_and_sync::<Activity>();
    send_and_sync::<Ack>();
}

/// Beginning and ending activities, one inside another, as playback starts
/// and stops. On macOS this asks NSProcessInfo; elsewhere it does nothing.
#[test]
fn activities_begin_and_end() {
    let outer = Activity::begin("Playing audio");
    let inner = Activity::begin("Loading a track");
    drop(inner);
    drop(outer);
}
