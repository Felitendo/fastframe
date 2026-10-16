//! The Windows taskbar badge, applied as a shell overlay to the app's button.

use std::time::Instant;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RPC_E_CHANGED_MODE, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, ITaskbarList3, RemoveWindowSubclass, SetWindowSubclass, TaskbarList,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIcon, DestroyIcon, HICON, RegisterWindowMessageW, WM_NCDESTROY,
};
use windows::core::PCWSTR;

use crate::overlay::{ICON_SIZE, Schedule, overlay_rgba};

/// The unread total, kept on the interface thread for the window to apply.
#[derive(Default)]
pub(super) struct Badge {
    shown: Option<u32>,
}

impl Badge {
    pub(super) fn new(_desktop_entry: String) -> Self {
        Self::default()
    }

    /// Records the unread total.
    pub(super) fn set(&mut self, count: u32) {
        self.shown = Some(count);
    }

    /// The latest unread total, once one is known.
    pub(super) fn count(&self) -> Option<u32> {
        self.shown
    }
}

/// One window's taskbar overlay: what it shows and when to try again.
#[derive(Default)]
pub struct Taskbar {
    // Declared before `_com` so it is released while COM is still initialized.
    list: Option<ITaskbarList3>,
    _com: Option<ComGuard>,
    schedule: Schedule,
    /// Whether the hook for a recreated taskbar button has been installed,
    /// or failed to be; it is tried once per window.
    hook_tried: bool,
}

impl Taskbar {
    /// Shows `count` on `window`'s taskbar button, watching for Explorer to
    /// recreate the button so the overlay can be put back. Returns when to try
    /// again after a failure.
    pub fn show(
        &mut self,
        window: &impl HasWindowHandle,
        count: u32,
        description: &str,
    ) -> Option<Instant> {
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return None;
        };
        let hwnd = handle.hwnd.get();
        if !std::mem::replace(&mut self.hook_tried, true) && !install_hook(hwnd) {
            log::debug!("could not watch for a recreated taskbar button");
        }
        if RECREATED.with(|marker| marker.replace(0) == hwnd) {
            self.reset();
        }
        self.update(hwnd, count, description)
    }

    /// Forgets the applied overlay, as when Explorer recreates the button.
    pub fn reset(&mut self) {
        self.list = None;
        self.schedule = Schedule::default();
    }

    /// Shows `count` on the window's button when it differs from what is shown
    /// and no retry is pending. Returns when to try again after a failure.
    fn update(&mut self, hwnd: isize, count: u32, description: &str) -> Option<Instant> {
        let now = Instant::now();
        if !self.schedule.due(count, now) {
            return None;
        }
        match self.apply(hwnd, count, description) {
            Ok(()) => {
                self.schedule.succeeded(count);
                None
            }
            Err(error) => {
                // Explorer may have restarted; the next attempt asks it afresh.
                self.list = None;
                let retry = self.schedule.failed(count, now);
                if retry.is_none() {
                    log::warn!("could not update the Windows taskbar badge: {error}");
                }
                retry
            }
        }
    }

    fn apply(&mut self, hwnd: isize, count: u32, description: &str) -> windows::core::Result<()> {
        let list = match &self.list {
            Some(list) => list,
            None => {
                if self._com.is_none() {
                    self._com = Some(ComGuard::new()?);
                }
                // SAFETY: COM is initialized on this thread, and TaskbarList is an
                // in-process server.
                let list: ITaskbarList3 =
                    unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)? };
                // SAFETY: HrInit must precede all other ITaskbarList calls.
                unsafe { list.HrInit()? };
                self.list.insert(list)
            }
        };
        apply_overlay(
            list,
            HWND(hwnd as *mut std::ffi::c_void),
            count,
            description,
        )
    }
}

/// Keeps COM initialized on the window thread while a taskbar list is held.
struct ComGuard(bool);

impl ComGuard {
    fn new() -> windows::core::Result<Self> {
        // SAFETY: the window shell calls this on the window thread.
        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if initialized.is_err() && initialized != RPC_E_CHANGED_MODE {
            return Err(initialized.into());
        }
        Ok(Self(initialized.is_ok()))
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: each successful CoInitializeEx on this thread needs one matching call.
            unsafe { CoUninitialize() };
        }
    }
}

/// Uses the taskbar's overlay API, which also updates pinned and grouped buttons.
fn apply_overlay(
    list: &ITaskbarList3,
    hwnd: HWND,
    count: u32,
    description: &str,
) -> windows::core::Result<()> {
    if count == 0 {
        // SAFETY: a null icon removes this window's overlay.
        return unsafe { list.SetOverlayIcon(hwnd, HICON::default(), PCWSTR::null()) };
    }

    let mut pixels = overlay_rgba(count);
    let mut and_mask = [0u8; ICON_SIZE * ICON_SIZE / 8];
    for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        if pixel[3] == 0 {
            and_mask[index / 8] |= 1 << (7 - index % 8);
        }
        // CreateIcon takes BGRA.
        pixel.swap(0, 2);
    }
    // SAFETY: both buffers cover the entire 16x16 image for this call.
    let icon = unsafe {
        CreateIcon(
            None,
            ICON_SIZE as i32,
            ICON_SIZE as i32,
            1,
            32,
            and_mask.as_ptr(),
            pixels.as_ptr(),
        )?
    };
    let description: Vec<u16> = description
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: the window and icon are valid for the duration of this call.
    let result = unsafe { list.SetOverlayIcon(hwnd, icon, PCWSTR(description.as_ptr())) };
    // SAFETY: the taskbar copies the icon before SetOverlayIcon returns. A
    // failure here leaks one small icon and must not undo a shown overlay.
    let _ = unsafe { DestroyIcon(icon) };
    result
}

thread_local! {
    /// The window whose taskbar button Explorer recreated, set by the
    /// subclass and taken by the next [`Taskbar::show`].
    static RECREATED: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

const SUBCLASS_ID: usize = 0x5a46_5442;

fn button_created_message() -> u32 {
    static MESSAGE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MESSAGE.get_or_init(|| {
        // SAFETY: RegisterWindowMessageW reads this static null-terminated string.
        unsafe { RegisterWindowMessageW(windows::core::w!("TaskbarButtonCreated")) }
    })
}

fn install_hook(hwnd: isize) -> bool {
    if button_created_message() == 0 {
        return false;
    }
    // SAFETY: this runs on the window thread; the subclass is removed on WM_NCDESTROY.
    unsafe {
        SetWindowSubclass(
            HWND(hwnd as *mut std::ffi::c_void),
            Some(subclass),
            SUBCLASS_ID,
            0,
        )
        .as_bool()
    }
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    _reference_data: usize,
) -> LRESULT {
    if message == button_created_message() {
        RECREATED.with(|marker| marker.set(hwnd.0 as isize));
        // SAFETY: the live window needs a repaint to reapply its overlay.
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
    } else if message == WM_NCDESTROY {
        // SAFETY: this removes our subclass before the window is destroyed.
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID);
        };
    }
    // SAFETY: all messages continue through the window's existing procedure.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
