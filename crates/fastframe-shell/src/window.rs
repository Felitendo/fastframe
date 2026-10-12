//! Open each window where the last one was, bring back a window restored
//! where no monitor shows it, and bring an existing window forward on
//! compositors that allow it.
//!
//! [`Geometry`] remembers the window's size, position and maximized state.
//! The app keeps it in its settings, folds the window into it every frame,
//! and applies it to the next window's `ViewportBuilder` before that window
//! exists, so the window opens in place instead of jumping there:
//!
//! ```no_run
//! # struct Settings { geometry: fastframe_shell::window::Geometry }
//! # let settings = Settings { geometry: Default::default() };
//! let viewport = egui::ViewportBuilder::default().with_inner_size([1180.0, 780.0]);
//! let viewport = settings.geometry.apply(viewport);
//! ```
//!
//! eframe restores a window's saved position as it is. When the displays
//! were rearranged, or a secondary display comes up late after a restart,
//! that position can be on no monitor at all, and Windows then leaves the
//! window unreachable (ZapFast #171), as macOS does a borderless one
//! (Spotifast #719). Call [`recover_offscreen`] once, on a new window's first
//! frame:
//!
//! ```no_run
//! struct Window { recovery_checked: bool }
//!
//! impl eframe::App for Window {
//!     fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
//!         if !std::mem::replace(&mut self.recovery_checked, true) {
//!             fastframe_shell::window::recover_offscreen(ctx, frame);
//!         }
//!     }
//!     fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {}
//! }
//! ```
//!
//! Wayland does not reveal global window positions, so nothing happens
//! there. Windows and X11 report the window and every monitor in the same
//! physical pixels. winit's macOS monitor coordinates disagree between
//! displays with different scale factors, so there the screens come from
//! AppKit, in points, and the window must keep its title bar on one of them,
//! clear of the menu bar and the Dock.
//!
//! Wayland also drops a client's request to focus its own window. Where the
//! compositor has a command for it, [`raise_on_compositor`] asks it instead.

use egui::{Pos2, Rect};

mod geometry;
mod raise;
mod zoom;

pub use geometry::Geometry;
pub use raise::raise_on_compositor;
pub use zoom::keep_zoom;

/// Where to move a window that no connected monitor shows, in physical
/// virtual-desktop pixels, or `None` to leave it where it is.
///
/// Any overlap with any monitor counts as visible, so a valid position on a
/// secondary display (including negative coordinates left of or above the
/// primary one) is never moved. The window goes to the middle of the first
/// monitor listed (put the primary one first), pinned to its top-left corner
/// when it is larger than that monitor. Without monitors nothing moves.
pub fn recovered_position(window: Rect, monitors: &[Rect]) -> Option<Pos2> {
    let target = *monitors.first()?;
    if monitors
        .iter()
        .any(|monitor| window.intersect(*monitor).area() > 0.0)
    {
        return None;
    }
    Some(centered(window, target))
}

/// Where `window` sits in the middle of `target`, pinned to its top-left
/// corner when it is larger.
fn centered(window: Rect, target: Rect) -> Pos2 {
    let slack = (target.size() - window.size()).max(egui::Vec2::ZERO);
    (target.min + slack / 2.0).round()
}

/// Moves the window onto the primary monitor when it is on none of the
/// connected ones (on macOS, when its title bar is on none of the screens'
/// visible areas). Does nothing on Wayland, for a minimized window (on
/// macOS, a full-screen one), and without a native window (tests).
pub fn recover_offscreen(ctx: &egui::Context, frame: &eframe::Frame) {
    recover(ctx, frame);
}

#[cfg(not(target_os = "macos"))]
fn recover(ctx: &egui::Context, frame: &eframe::Frame) {
    let Some(window) = frame.winit_window() else {
        return;
    };
    // A minimized window on Windows reports a parking position far off
    // screen; restoring it brings back its real one.
    if window.is_minimized() == Some(true) {
        return;
    }
    let Ok(position) = window.outer_position() else {
        return;
    };
    let size = window.outer_size();
    let rect = |x: i32, y: i32, width: u32, height: u32| {
        Rect::from_min_size(
            egui::pos2(x as f32, y as f32),
            egui::vec2(width as f32, height as f32),
        )
    };
    // The primary monitor comes first, as the place a lost window goes; a
    // platform without one falls back to the first connected monitor.
    let monitors: Vec<_> = window
        .primary_monitor()
        .into_iter()
        .chain(window.available_monitors())
        .map(|monitor| {
            let (position, size) = (monitor.position(), monitor.size());
            rect(position.x, position.y, size.width, size.height)
        })
        .collect();
    let Some(target) = recovered_position(
        rect(position.x, position.y, size.width, size.height),
        &monitors,
    ) else {
        return;
    };
    log::warn!("the restored window was on no connected monitor; moving it to the primary one");
    let pixels_per_point = ctx.input(|input| input.pixels_per_point);
    ctx.send_viewport_cmd(move_to_physical(target, pixels_per_point));
}

/// macOS: winit gives the window's frame in physical pixels at the window's
/// own scale factor, so dividing by it gives AppKit points again, the unit
/// the screens come in. A window just created is not minimized, and asking
/// AppKit whether it is can deadlock (emilk/egui#3494), so that is not asked.
/// A full-screen window covers the menu bar, where no title bar may be, and
/// is left alone.
#[cfg(target_os = "macos")]
fn recover(ctx: &egui::Context, frame: &eframe::Frame) {
    let Some(window) = frame.winit_window() else {
        return;
    };
    if window.fullscreen().is_some() {
        return;
    }
    let Some(screens) = visible_screens() else {
        return;
    };
    let Ok(position) = window.outer_position() else {
        return;
    };
    let scale = window.scale_factor();
    let position = position.to_logical::<f32>(scale);
    let size = window.outer_size().to_logical::<f32>(scale);
    let window = Rect::from_min_size(
        egui::pos2(position.x, position.y),
        egui::vec2(size.width, size.height),
    );
    let Some(target) = recovered_titlebar_position(window, &screens) else {
        return;
    };
    log::warn!("the restored window's title bar was on no screen; moving it to the main one");
    let pixels_per_point = ctx.input(|input| input.pixels_per_point);
    ctx.send_viewport_cmd(move_to_physical(target * scale as f32, pixels_per_point));
}

/// Where to move a window whose title bar is on none of `screens`, in the
/// same points, or `None` to leave it. It goes to the middle of the first
/// screen (the main one).
#[cfg(any(target_os = "macos", test))]
fn recovered_titlebar_position(window: Rect, screens: &[Rect]) -> Option<Pos2> {
    let target = *screens.first()?;
    (!titlebar_on_screen(window.min, screens)).then(|| centered(window, target))
}

/// Whether a window placed at `position` keeps its title bar on one of
/// `screens`, so it can be dragged. A point a little inside the frame is
/// checked, because a maximized window can start a few pixels off the
/// screen's edge.
fn titlebar_on_screen(position: Pos2, screens: &[Rect]) -> bool {
    let anchor = position + egui::vec2(32.0, 16.0);
    anchor.is_finite() && screens.iter().any(|screen| screen.contains(anchor))
}

/// The area each connected screen leaves for windows (without the menu bar
/// and the Dock), main screen first, in points measured down from the top of
/// the main screen as winit measures windows. `None` off the main thread,
/// where AppKit cannot be asked, and without any screen.
#[cfg(target_os = "macos")]
fn visible_screens() -> Option<Vec<Rect>> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;

    let screens = NSScreen::screens(MainThreadMarker::new()?);
    let main = screens.iter().next()?.frame();
    let main_top = main.origin.y + main.size.height;
    Some(
        screens
            .iter()
            .map(|screen| {
                let area = screen.visibleFrame();
                flipped(
                    main_top,
                    [
                        area.origin.x,
                        area.origin.y,
                        area.size.width,
                        area.size.height,
                    ],
                )
            })
            .collect(),
    )
}

/// Only macOS is asked before the window exists; elsewhere
/// [`recover_offscreen`] checks the window once it does.
#[cfg(not(target_os = "macos"))]
fn visible_screens() -> Option<Vec<Rect>> {
    None
}

/// Turns an AppKit screen rectangle `[x, y, width, height]` into window
/// coordinates. AppKit measures up from the bottom of the main screen, and
/// winit down from its top.
#[cfg(any(target_os = "macos", test))]
fn flipped(main_top: f64, [x, y, width, height]: [f64; 4]) -> Rect {
    Rect::from_min_size(
        egui::pos2(x as f32, (main_top - y - height) as f32),
        egui::vec2(width as f32, height as f32),
    )
}

/// The command that moves the window to `target` physical pixels.
///
/// egui-winit multiplies the position by the window's pixels per point, so
/// dividing by the same factor asks for exactly these physical pixels,
/// whatever the scale of the monitor the window is on now.
fn move_to_physical(target: Pos2, pixels_per_point: f32) -> egui::ViewportCommand {
    egui::ViewportCommand::OuterPosition(target / pixels_per_point)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    fn monitor(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), vec2(width, height))
    }

    #[test]
    fn a_window_above_every_monitor_moves_to_the_middle_of_the_first() {
        let window = Rect::from_min_size(pos2(100.0, -500.0), vec2(400.0, 300.0));
        assert_eq!(
            recovered_position(window, &[monitor(0.0, 0.0, 1920.0, 1080.0)]),
            Some(pos2(760.0, 390.0))
        );
    }

    #[test]
    fn a_window_partly_on_a_monitor_stays() {
        let window = Rect::from_min_size(pos2(-100.0, 100.0), vec2(400.0, 300.0));
        assert_eq!(
            recovered_position(window, &[monitor(0.0, 0.0, 1920.0, 1080.0)]),
            None
        );
    }

    #[test]
    fn a_window_on_a_monitor_left_of_or_above_the_primary_stays() {
        let monitors = [
            monitor(0.0, 0.0, 1920.0, 1080.0),
            monitor(-1920.0, 0.0, 1920.0, 1080.0),
            monitor(0.0, -1440.0, 2560.0, 1440.0),
        ];
        let left = Rect::from_min_size(pos2(-1600.0, 100.0), vec2(800.0, 600.0));
        let above = Rect::from_min_size(pos2(200.0, -1300.0), vec2(800.0, 600.0));
        assert_eq!(recovered_position(left, &monitors), None);
        assert_eq!(recovered_position(above, &monitors), None);
    }

    /// ZapFast #171: a window saved on a display to the right, which is gone
    /// after a restart. The primary one here is not at the origin.
    #[test]
    fn a_window_left_where_a_disconnected_monitor_was_moves_to_the_primary() {
        let window = Rect::from_min_size(pos2(3891.0, -358.0), vec2(1180.0, 780.0));
        let monitors = [
            monitor(1920.0, 0.0, 1920.0, 1080.0),
            monitor(0.0, 0.0, 1920.0, 1080.0),
        ];
        assert_eq!(
            recovered_position(window, &monitors),
            Some(pos2(2290.0, 150.0))
        );
    }

    #[test]
    fn a_window_larger_than_the_monitor_is_pinned_to_its_corner() {
        let window = Rect::from_min_size(pos2(5000.0, 5000.0), vec2(2000.0, 1200.0));
        assert_eq!(
            recovered_position(window, &[monitor(-1280.0, 0.0, 1280.0, 1024.0)]),
            Some(pos2(-1280.0, 0.0))
        );
    }

    #[test]
    fn without_monitors_nothing_moves() {
        let window = Rect::from_min_size(pos2(-5000.0, -5000.0), vec2(400.0, 300.0));
        assert_eq!(recovered_position(window, &[]), None);
    }

    #[test]
    fn the_move_asks_for_physical_pixels_at_any_scale() {
        assert_eq!(
            move_to_physical(pos2(2290.0, 150.0), 1.25),
            egui::ViewportCommand::OuterPosition(pos2(1832.0, 120.0))
        );
    }

    /// AppKit's screens grow up from the bottom of the main one; a screen
    /// above it has a larger y there and a negative one in window
    /// coordinates.
    #[test]
    fn appkit_screens_are_flipped_to_window_coordinates() {
        // The main screen, 1440 by 900 points, with a 25-point menu bar.
        assert_eq!(
            flipped(900.0, [0.0, 0.0, 1440.0, 875.0]),
            monitor(0.0, 25.0, 1440.0, 875.0)
        );
        // A 1920 by 1080 screen above it.
        assert_eq!(
            flipped(900.0, [0.0, 900.0, 1920.0, 1080.0]),
            monitor(0.0, -1080.0, 1920.0, 1080.0)
        );
    }

    /// Spotifast #719: a window saved on a display that has since been
    /// unplugged comes back to the main screen; one whose title bar is
    /// under the menu bar too.
    #[test]
    fn a_title_bar_on_no_screen_moves_to_the_main_one() {
        let screens = [
            monitor(0.0, 25.0, 1440.0, 875.0),
            monitor(1440.0, 0.0, 1920.0, 1080.0),
        ];
        let on_second = Rect::from_min_size(pos2(2000.0, 100.0), vec2(400.0, 300.0));
        assert_eq!(recovered_titlebar_position(on_second, &screens), None);

        let unplugged = Rect::from_min_size(pos2(-2000.0, 100.0), vec2(400.0, 300.0));
        assert_eq!(
            recovered_titlebar_position(unplugged, &screens),
            Some(pos2(520.0, 313.0))
        );

        // Most of this window is on the screen, but not its title bar.
        let under_menu_bar = Rect::from_min_size(pos2(100.0, 0.0), vec2(400.0, 300.0));
        assert!(recovered_titlebar_position(under_menu_bar, &screens).is_some());
    }

    #[test]
    fn a_maximized_window_a_few_pixels_off_the_edge_keeps_its_title_bar() {
        let screens = [monitor(0.0, 0.0, 1920.0, 1040.0)];
        assert!(titlebar_on_screen(pos2(-8.0, -8.0), &screens));
        assert!(!titlebar_on_screen(pos2(f32::NAN, 0.0), &screens));
        assert!(!titlebar_on_screen(pos2(0.0, 0.0), &[]));
    }
}
