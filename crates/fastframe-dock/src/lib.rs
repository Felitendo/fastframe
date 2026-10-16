//! Application icons and badges that outlive individual windows.
//!
//! Extracted from ZapFast's Dock and taskbar badge implementations. See the
//! crate README for platform support and the Windows per-window overlay.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // AppKit icon setters, with safety notes at each call.
mod macos;
#[cfg(any(target_os = "windows", test))]
mod overlay;
#[cfg(target_os = "windows")]
#[allow(unsafe_code)] // COM and the window subclass require native calls.
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

/// The application-wide count. Keep this alive when closing a window.
///
/// macOS draws the number on the Dock tile. Linux publishes the Unity Launcher
/// API for compatible launchers. Windows stores the count for [`Taskbar`].
pub struct Badge(platform::Badge);

impl Badge {
    /// Creates a badge for a desktop entry, without the `.desktop` suffix.
    ///
    /// Resolve Flatpak identities with `fastframe_desktop::desktop_entry`.
    /// The name is only used on Linux. Construction does not contact the OS.
    pub fn new(desktop_entry: impl Into<String>) -> Self {
        Self(platform::Badge::new(desktop_entry.into()))
    }

    /// Shows a count, or clears the badge at zero. Call on the UI thread.
    /// Repeated values do not publish again. Linux caps the count at 9999.
    pub fn set(&mut self, count: u32) {
        self.0.set(count);
    }

    /// The last accepted count, or `None` before the first update.
    pub fn count(&self) -> Option<u32> {
        self.0.count()
    }
}

#[cfg(target_os = "windows")]
pub use windows::Taskbar;

/// A no-op overlay on platforms where [`Badge`] updates the launcher directly.
#[cfg(not(target_os = "windows"))]
#[derive(Default)]
pub struct Taskbar;

#[cfg(not(target_os = "windows"))]
impl Taskbar {
    /// Does nothing here; only Windows needs a per-window overlay.
    pub fn show(
        &mut self,
        _window: &impl raw_window_handle::HasWindowHandle,
        _count: u32,
        _description: &str,
    ) -> Option<std::time::Instant> {
        None
    }

    /// Does nothing here; only Windows caches the last applied overlay.
    pub fn reset(&mut self) {}
}

/// Why the unbundled macOS application icon could not be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconError {
    /// AppKit requires the application's main thread.
    NotMainThread,
    /// AppKit could not decode the supplied image.
    InvalidImage,
}

impl std::fmt::Display for IconError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotMainThread => "the Dock icon must be installed on the main thread",
            Self::InvalidImage => "AppKit could not decode the Dock icon",
        })
    }
}

impl std::error::Error for IconError {}

/// Installs an embedded PNG or ICNS as the Dock icon of an unbundled macOS app.
///
/// Call once on the main thread before opening the first window. This replaces
/// the generic executable icon in `cargo run`, and survives window destruction.
/// Returns `true` when installed, `false` for a packaged `.app` (whose branding
/// is preserved) or on other platforms. Does not change the executable's name.
pub fn set_icon(encoded_image: &[u8]) -> Result<bool, IconError> {
    #[cfg(target_os = "macos")]
    return macos::set_icon(encoded_image);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = encoded_image;
        Ok(false)
    }
}

#[cfg(any(target_os = "macos", test))]
fn is_app_bundle(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|extension| extension == "app")
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod platform {
    #[derive(Default)]
    pub(super) struct Badge(Option<u32>);
    impl Badge {
        pub(super) fn new(_: String) -> Self {
            Self::default()
        }
        pub(super) fn set(&mut self, count: u32) {
            self.0 = Some(count);
        }
        pub(super) fn count(&self) -> Option<u32> {
            self.0
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_real_app_bundles_keep_their_packaged_icon() {
        use std::path::Path;
        assert!(super::is_app_bundle(Path::new(
            "/Applications/Spotifast.app"
        )));
        assert!(!super::is_app_bundle(Path::new("/code/target/release")));
        assert!(!super::is_app_bundle(Path::new("/code/app")));
    }
}
