//! The macOS badge: the unread count on the Dock tile, as WhatsApp shows it.
//!
//! The Dock tile belongs to the running process, so the label is set through
//! `NSApp.dockTile` on the main thread, where the interface runs, and an empty
//! label clears it. No notification permission is needed for it.

use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;
use objc2_foundation::NSString;

pub(super) fn set_icon(encoded_image: &[u8]) -> Result<bool, crate::IconError> {
    use objc2::AnyThread;
    use objc2_app_kit::NSImage;
    use objc2_foundation::{NSBundle, NSData};

    let main = MainThreadMarker::new().ok_or(crate::IconError::NotMainThread)?;
    let bundle_path = NSBundle::mainBundle().bundlePath().to_string();
    if crate::is_app_bundle(std::path::Path::new(&bundle_path)) {
        return Ok(false);
    }
    let data = NSData::with_bytes(encoded_image);
    let image =
        NSImage::initWithData(NSImage::alloc(), &data).ok_or(crate::IconError::InvalidImage)?;
    // SAFETY: main-thread marker proves AppKit affinity; the application
    // retains the decoded image after this call returns.
    unsafe { NSApplication::sharedApplication(main).setApplicationIconImage(Some(&image)) };
    Ok(true)
}

/// Counts unread chats on the Dock tile.
#[derive(Default)]
pub(super) struct Badge {
    shown: Option<u32>,
}

impl Badge {
    pub(super) fn new(_desktop_entry: String) -> Self {
        Self::default()
    }
    pub(super) fn count(&self) -> Option<u32> {
        self.shown
    }

    /// Shows the count on the Dock tile, clearing it at zero. A repeat is not
    /// applied, so calling this every frame costs nothing.
    pub(super) fn set(&mut self, count: u32) {
        if self.shown == Some(count) {
            return;
        }
        // AppKit is main-thread only; the interface always calls from there.
        let Some(main) = MainThreadMarker::new() else {
            return;
        };
        let tile = NSApplication::sharedApplication(main).dockTile();
        let label = label(count).map(|label| NSString::from_str(&label));
        tile.setBadgeLabel(label.as_deref());
        self.shown = Some(count);
    }
}

/// The Dock label for a count: none at zero, else the number.
fn label(count: u32) -> Option<String> {
    (count > 0).then(|| count.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_clears_the_label_and_other_counts_show_as_numbers() {
        assert_eq!(label(0), None);
        assert_eq!(label(1).as_deref(), Some("1"));
        assert_eq!(label(1234).as_deref(), Some("1234"));
    }
}
