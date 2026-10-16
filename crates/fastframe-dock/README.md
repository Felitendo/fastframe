# fastframe-dock

The application's Dock icon and unread badge, independent of its windows.
Extracted from ZapFast's shipped desktop badges.

```no_run
// At startup on the main thread, use your embedded high-resolution PNG or ICNS.
// fastframe_dock::set_icon(include_bytes!("../assets/icon.png"))?;
let mut badge = fastframe_dock::Badge::new("my-app");
badge.set(12);
badge.set(0); // Clear the badge.
```

`set_icon` fixes the generic executable Dock image for unbundled macOS runs,
including `cargo run --release`. It leaves a packaged `.app`'s icon alone and
does nothing on Windows or Linux, whose existing executable/window icons apply.
It does not rename the process or invent bundle identity for notifications.

Keep one `Badge` with the application, across window closure and recreation.
Call `set` on the UI thread. Counts are drawn natively on macOS; Linux emits
the Unity Launcher API on a background connection for Plasma and compatible
launchers. Pass the desktop entry without `.desktop`, using
`fastframe_desktop::desktop_entry` when Flatpak identity applies. Unsupported
Linux launchers simply do not show a badge. Zero clears it.

Windows uses a per-window `Taskbar` in addition to the application `Badge`:
call `Taskbar::show(&window, count, &localized_description)` when the count is
known, and schedule another repaint at the returned retry time if present.
The description is app-owned accessible text, such as "3 unread messages".
Create a fresh `Taskbar` for every recreated window. It watches Explorer's
taskbar recreation, retries failures with bounded backoff, and displays `99+`
for larger counts. No taskbar button exists while every window is closed;
keep the application count and apply it when a window returns.

The `Taskbar` interface is a no-op on other platforms. Linux counts cap at
9999; macOS displays the full number. This crate never requests notification
permission or chooses what an app should count.
