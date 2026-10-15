# fastframe-desktop

Who the app is to the desktop, and what the desktop says.

```rust
// The name MPRIS, notifications and the window's app id share: the
// sandbox's app id inside a Flatpak, "zapfast" elsewhere.
let entry = fastframe_desktop::desktop_entry("zapfast");

// GNOME handles some requests its own way: it turns a window's request for
// attention into a notice of its own.
if !fastframe_desktop::is_gnome() {
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
        egui::UserAttentionType::Informational,
    ));
}

// One of the desktop's settings, through the desktop portal.
let twelve_hour = fastframe_desktop::setting("org.gnome.desktop.interface", "clock-format")
    .is_some_and(|format| format == "12h");
let buttons = fastframe_desktop::setting("org.gnome.desktop.wm.preferences", "button-layout");
```

`setting` asks `org.freedesktop.portal.Settings` on the session bus, which
works inside a Flatpak, where running `gsettings` does not, and starts no
process. It returns `None` off Linux, without a portal, or for a key the
portal does not serve, and gives up after half a second. GNOME's portal and
the GTK one (used by Hyprland, Sway and others) serve GNOME's schemas: the
interface, window-manager and peripheral settings among them.

It came from Spotifast, which read the button layout and icon theme with
`gsettings` for its Linux title bar, ZapFast, which read the clock format the
same way and checked for GNOME before asking for attention, and
`fastframe-now-playing`, whose Flatpak-aware desktop entry name ZapFast and
Solco needed for their notifications too.

## macOS notification permission

On a notification worker, before native delivery:

```rust
if fastframe_desktop::macos_notifications_allowed() {
    // Send through mac-usernotifications; keep content, sound and clicks
    // in the app's notification adapter.
}
```

This is ZapFast's `UNUserNotificationCenter` guard: check the actual process
bundle once, then ask for authorization each time. Unbundled development runs
stay silent, and System Settings permission changes apply without restarting.
Errors and denial return `false`; other platforms return `false` without
calling an OS service. Call only when the app intends to notify and its own
notification preference allows it. The permission request may wait for the
user, so keep it off the UI thread and keep the native event loop running.

It does not set or impersonate a bundle ID. Solco's legacy `notify-rust`
macOS backend tried to resolve an application named `use_default` through
AppleScript, opening a blocking application chooser. This guard is for the
modern `mac-usernotifications` API ZapFast already uses, not a workaround for
that legacy backend. Spotifast currently sends no system notifications.

New notification integrations use [`fastframe-notifications`](../fastframe-notifications),
which owns delivery, permission status and requests, click listeners, replacement
and withdrawal on all three platforms. The guard above remains for existing
consumers; apps using the shared client do not also call it.
