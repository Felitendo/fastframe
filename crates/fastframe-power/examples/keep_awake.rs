//! Keeps the computer awake for a while, then lets it sleep again:
//! `cargo run -p fastframe-power --example keep_awake -- 60` holds it for
//! sixty seconds. Meanwhile `pmset -g assertions` (macOS),
//! `powercfg /requests` (Windows, as administrator) or
//! `systemd-inhibit --list` (Linux, logind's lock) shows the hold.

use std::time::Duration;

use fastframe_power::KeepAwake;

#[allow(clippy::print_stdout)]
fn main() {
    let seconds = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(30);
    let awake = KeepAwake::begin("Trying fastframe-power's KeepAwake");
    println!("keeping the computer awake for {seconds} s");
    std::thread::sleep(Duration::from_secs(seconds));
    drop(awake);
    println!("the computer may sleep again");
}
