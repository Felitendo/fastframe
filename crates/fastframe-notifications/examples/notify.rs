//! Manual native smoke test. All content is synthetic.
//! On macOS package this binary in a signed app whose bundle identifier matches
//! FASTFRAME_NOTIFICATION_APP_ID, then launch the executable from that bundle.
//! FASTFRAME_NOTIFICATION_LOGO names a PNG for `App::logo`, and
//! FASTFRAME_NOTIFICATION_BACKGROUND its tile colour as `rrggbb` (violet if unset).
use fastframe_notifications::{App, Client, Notification, Sound};

fn main() {
    let (done, finished) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = exercise();
        let _ = done.send(result);
    });
    fastframe_notifications::run_main(async {
        finished
            .await
            .expect("example worker")
            .expect("native notification test");
    });
}
#[expect(
    clippy::print_stdout,
    reason = "the manual example reports synthetic native events"
)]
fn exercise() -> Result<(), fastframe_notifications::Error> {
    let id = std::env::var("FASTFRAME_NOTIFICATION_APP_ID")
        .unwrap_or_else(|_| "org.fastframe.notification-example".into());
    let mut app = App::new(id, "Fastframe notification example").desktop_entry("fastframe-example");
    // A PNG of a logo, to check the app icon.
    if let Some(logo) = std::env::var_os("FASTFRAME_NOTIFICATION_LOGO") {
        let png = std::fs::read(logo).map_err(|_| fastframe_notifications::Error::InvalidInput)?;
        let background = std::env::var("FASTFRAME_NOTIFICATION_BACKGROUND")
            .ok()
            .and_then(|hex| u32::from_str_radix(&hex, 16).ok())
            .unwrap_or(0x6a4fd8)
            .to_be_bytes();
        app = app.logo(&png, [background[1], background[2], background[3]]);
    }
    let client = Client::new(app)?;
    println!("Permission: {:?}", client.request_permission_blocking()?);
    let handle = client.show_with(
        Notification::new("Fastframe notification test")
            .body("Plain text: <tag> & café")
            .key("synthetic-test")
            .sound(Sound::Silent),
        |event| println!("Event: {event:?}"),
    )?;
    std::thread::sleep(std::time::Duration::from_secs(3));
    if client.capabilities().progress {
        handle.update(
            Notification::new("Fastframe notification test")
                .body("Halfway complete")
                .progress(50)
                .sound(Sound::Silent),
        )?;
    }
    std::thread::sleep(std::time::Duration::from_secs(3));
    handle.close();
    std::thread::sleep(std::time::Duration::from_secs(1));
    Ok(())
}
