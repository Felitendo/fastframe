# fastframe-notifications

One notification interface for GUI apps, TUIs and background agents on Linux,
macOS and Windows. Extracted from ZapFast's message notifications and Solco's
background task notifications. No window or egui dependency.

```rust,no_run
use fastframe_notifications::{App, Client, Event, Notification, Sound};

let notifications = Client::new(
    App::new("com.example.app", "Example")
        .desktop_entry("example")
        .icon("example")
        .icon_file("/home/me/.local/share/example/logo.png"),
)?;
let notice = notifications.show_with(
    Notification::new("Export complete")
        .body("24 tracks copied")
        .key("export-42")
        .sound(Sound::Default),
    |event| {
        if event == Event::Activated {
            // Send a message into the app's event queue and wake its UI.
        }
    },
)?;
// Keep `notice` while clicks matter. These calls never wait for the desktop.
notice.update(Notification::new("Export complete").body("Safe to eject"))?;
notice.close();
# Ok::<(), fastframe_notifications::Error>(())
```

`Client::new` starts one background runtime. Delivery, replacement, permission
queries and native callbacks stay off the caller's thread. Up to 64 operations
can be in flight, and the command queue is bounded. `Error::Busy` lets the app
retry or use its existing fallback. Updates on a handle coalesce to the latest
content. Apps should also release old handles, as ZapFast does after 32 waiting
messages, because some desktops retain notification history without reporting
dismissal.

A successful `show` means queued. `Event::Shown` means the OS accepted it,
not that a banner was displayed. Focus, Do Not Disturb, user preferences and
notification service policy still control presentation. `Event::Failed` reports
asynchronous failures without including private notification text. Callbacks
run on the worker, must stay short, and must not call a blocking permission
method. Send events to the app's queue. A panicking callback is contained.

Dropping the last handle releases its listener and leaves the delivered
notification in OS history. `close` removes it, including when delivery was
still in flight. Closing before delivery starts suppresses it. A completion
update immediately followed by dropping the handle is still applied. Reusing
a key serializes delivery with its predecessor and replaces the OS notification;
a stale handle cannot close its replacement.

## Permissions and identity

`permission().await` checks without prompting. `request_permission().await`
requests permission, normally after the user enables notifications. A synchronous
background thread can use `request_permission_blocking()`. Delivery never
requests permission itself. Permission denial is rechecked on later calls so
changes in system settings take effect without a restart.

Use the installed application's identity, not another program's identity.
On Windows, `App::new`'s ID must match the shortcut's AppUserModelID. Fastframe
registers its display name for portable builds, and `icon_file` as its
`IconUri`, without which toasts show a blank icon. Linux uses the desktop entry,
including Flatpak's exported identity, and the `icon` theme name. When neither
the `hicolor` theme nor `pixmaps` has that icon, as in a portable archive or a
development build, Linux sends the `icon_file` path instead. Write the logo to
a stable file once, for example in the app's data folder, and pass its
absolute path. Linux reports `Permission::Unknown`
when the service is available because Freedesktop does not expose per-app
permission settings.

macOS requires a real signed application bundle. Unbundled command-line builds
return `BundleRequired` before accessing the native service. GUI apps already
run the main event loop. A daemon or TUI uses a signed bundled broker whose main
thread calls `run_main(future)` while its async work runs on another runtime.
A sandboxed worker should send authenticated IPC to that broker. No chooser,
AppleScript, Terminal impersonation, or busy-run-loop heuristic is used.

## Platform presentation

| Feature | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Click and named actions while observed | Yes | Yes | Yes |
| Replace and withdraw by key | Yes, within this client | Yes, including after restart | Yes, including after restart |
| Live progress | Percentage and plain text | Ordinary notifications | Ordinary notifications |
| App icon | Theme icon, or `icon_file` | Application bundle icon | `icon_file` |
| Avatar | Local image | Application icon | Local circular image |
| Default / silent sound | Yes | Yes | Yes |
| Named sound | Sound theme | File in bundle Resources | System default |

`capabilities()` exposes live-progress support and persistent key support. The
OS may ignore optional hints. Linux remembers at most 256 integer IDs without
retaining connections; keys cannot be recovered after a process restart. Apps
keep their own durable routing data for a stable key. Never put message bodies
or credentials in keys. Click callbacks belong to the running process; the
crate does not install an app-launch COM service or persist application routing.

Keep account scoping, visibility, preferences, event deduplication, rate limits
and product text in the app. A daemon should be the single native delivery
owner when a GUI and TUI share it. Browser/native deduplication belongs to that
application's delivery protocol, not the OS backend.

## Tests

Lifecycle tests use a fake backend and controlled futures. They cover close
before and during delivery, releasing observation without removing history,
coalesced updates, completion followed by release, failure reporting and bounded
admission. Text escaping and Windows ID/payload mappings run without native
services. No unit test contacts D-Bus, the registry or Notification Center.
