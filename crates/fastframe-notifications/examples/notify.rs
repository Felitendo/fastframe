//! Manual native smoke test. All content is synthetic.
//! On macOS package this binary in a signed app whose bundle identifier matches
//! FASTFRAME_NOTIFICATION_APP_ID, then launch the executable from that bundle.
//! FASTFRAME_NOTIFICATION_ICON names an absolute logo file for `App::icon_file`.
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
    // An absolute PNG (or ICO on Windows) of a logo, to check the app icon.
    if let Some(logo) = std::env::var_os("FASTFRAME_NOTIFICATION_ICON") {
        app = app.icon_file(logo);
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
