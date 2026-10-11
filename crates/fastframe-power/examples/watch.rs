//! Prints the conditions and every event as the system reports them:
//! `cargo run -p fastframe-power --example watch`. Turn on the energy
//! saver, unplug the charger, lock the session, mark the Wi-Fi as metered or
//! put the machine to sleep, and watch the lines change. Stop it with
//! Ctrl-C.
//!
//! On macOS the network's reach is delivered on the main dispatch queue,
//! which this example does not run (an app's event loop does), so only its
//! first reading shows here.

use std::sync::mpsc;
use std::time::Duration;

use fastframe_power::{Event, Power};

#[allow(clippy::print_stdout)]
fn main() {
    let (woken, wakes) = mpsc::channel();
    let power = Power::spawn(move || {
        let _ = woken.send(());
    });
    let mut last = None;
    loop {
        // Woken by a change, or every few seconds for Low Power Mode on
        // macOS, which is read rather than followed.
        let _ = wakes.recv_timeout(Duration::from_secs(5));
        let conditions = power.conditions();
        if last != Some(conditions) {
            println!("{conditions:?}");
            last = Some(conditions);
        }
        for event in power.events() {
            println!("{event:?}");
            match event {
                Event::Suspending(ack) | Event::EndingSession(ack) => ack.done(),
                Event::Resumed => {}
            }
        }
    }
}
