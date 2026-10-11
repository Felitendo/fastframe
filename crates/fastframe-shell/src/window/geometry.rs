//! The window's remembered size, position and maximized state.

use egui::{Pos2, Rect, Vec2};

/// The largest width or height taken back from the settings. Anything above
/// it is a corrupt value, not a window.
const MAX_SIZE: f32 = 16_384.0;

/// The farthest a remembered position may be from the origin, in either
/// direction. Beyond it the value is corrupt, not a display to the side.
const MAX_COORDINATE: f32 = 32_768.0;

/// Changes smaller than this are the window manager settling by a fraction
/// of a pixel, not a resize or a move, and are not worth saving.
const SETTLING: f32 = 0.5;

/// A window's size, position and maximized state, so the next window opens
/// as the last one was left.
///
/// The app stores the three fields in its own settings, in whatever format
/// those already use, folds the open window in with
/// [`remember`](Self::remember) every frame, and opens the next window with
/// [`apply`](Self::apply).
///
/// Sizes and positions are in logical pixels: egui points at a zoom factor
/// of 1. They do not change when the app zooms its interface, and a window
/// saved while zoomed opens at the same size on screen.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Geometry {
    /// The inner (content) size, `[width, height]`. `None` keeps the size
    /// the viewport builder already has.
    pub size: Option<[f32; 2]>,
    /// The position `ViewportBuilder::with_position` takes: the outer
    /// frame's top-left corner, except on macOS, where it is the content's.
    /// `None` lets the window manager place the window. Wayland never
    /// reveals a position, so there the last known one is kept.
    pub position: Option<[f32; 2]>,
    /// Whether the window was maximized. `size` and `position` stay those of
    /// the normal window behind it, which unmaximizing returns to.
    pub maximized: bool,
}

/// One frame's window state, in logical pixels.
#[derive(Clone, Copy, Debug, Default)]
struct Seen {
    size: Option<Vec2>,
    position: Option<Pos2>,
    /// `None` where it is not known (macOS after the window was created).
    maximized: Option<bool>,
    fullscreen: Option<bool>,
    minimized: Option<bool>,
}

impl Geometry {
    /// Folds the window's current state in, and returns whether anything
    /// changed, so the app saves its settings only then.
    ///
    /// A minimized or full-screen window is skipped: neither is a geometry
    /// to open the next window with. A maximized one only sets
    /// [`maximized`](Self::maximized), keeping the normal size and position.
    /// Where the platform gives no position (Wayland), the size is still
    /// taken and the last position kept.
    ///
    /// egui reads macOS's maximized state only when the window is created
    /// (emilk/egui#3494), so there it is left as it was, and a window zoomed
    /// to fill the screen is remembered at that size.
    ///
    /// Call it from `eframe::App::logic`, which runs for every window state.
    pub fn remember(&mut self, ctx: &egui::Context) -> bool {
        let zoom = ctx.zoom_factor();
        let seen = ctx.input(|input| {
            let viewport = input.viewport();
            Seen {
                size: viewport
                    .inner_rect
                    .or(input.raw.screen_rect)
                    .map(|rect| rect.size() * zoom),
                position: builder_position(
                    viewport.inner_rect,
                    viewport.outer_rect,
                    cfg!(target_os = "macos"),
                )
                .map(|position| position * zoom),
                maximized: if cfg!(target_os = "macos") {
                    None
                } else {
                    viewport.maximized
                },
                fullscreen: viewport.fullscreen,
                minimized: viewport.minimized,
            }
        });
        self.fold(seen)
    }

    /// Sets the remembered size, position and maximized state on a new
    /// window's builder, before the window exists, so it opens in place
    /// instead of jumping there. Give the builder its default size first:
    /// only a usable remembered size replaces it.
    ///
    /// Values out of any sane range (a corrupt settings file) are ignored.
    /// On macOS, a position whose title bar would be on no connected screen
    /// is dropped, because AppKit creates a window wherever it is told, even
    /// on a display that has since been unplugged.
    ///
    /// egui-winit scales these by the new context's zoom factor. That is 1
    /// unless eframe restores egui's saved memory, whose options carry the
    /// zoom. Keep the zoom in the app's settings and set it once the window
    /// exists, with `eframe::App::persist_egui_memory` off, as ZapFast does.
    #[must_use]
    pub fn apply(self, builder: egui::ViewportBuilder) -> egui::ViewportBuilder {
        self.apply_on(builder, super::visible_screens().as_deref())
    }

    /// [`apply`](Self::apply), with the screens a position must keep its
    /// title bar on, or `None` to keep any sane position.
    fn apply_on(
        self,
        mut builder: egui::ViewportBuilder,
        screens: Option<&[Rect]>,
    ) -> egui::ViewportBuilder {
        if let Some(size) = self.size.filter(|&size| sane_size(size.into())) {
            builder = builder.with_inner_size(size);
        }
        if let Some(position) = self.position.filter(|&position| {
            sane_position(position.into())
                && screens.is_none_or(|screens| super::titlebar_on_screen(position.into(), screens))
        }) {
            builder = builder.with_position(position);
        }
        if self.maximized {
            builder = builder.with_maximized(true);
        }
        builder
    }

    fn fold(&mut self, seen: Seen) -> bool {
        if seen.minimized == Some(true) || seen.fullscreen == Some(true) {
            return false;
        }
        let mut changed = false;
        if let Some(maximized) = seen.maximized {
            changed = self.maximized != maximized;
            self.maximized = maximized;
            if maximized {
                return changed;
            }
        }
        if let Some(size) = seen.size.filter(|&size| sane_size(size))
            && moved(self.size, size.into())
        {
            self.size = Some(size.into());
            changed = true;
        }
        if let Some(position) = seen.position.filter(|&position| sane_position(position))
            && moved(self.position, position.into())
        {
            self.position = Some(position.into());
            changed = true;
        }
        changed
    }
}

/// The corner `ViewportBuilder::with_position` places, from the viewport's
/// rectangles: the content's on macOS (`content_origin`), the outer frame's
/// elsewhere, falling back to the other when one is missing.
fn builder_position(
    inner: Option<Rect>,
    outer: Option<Rect>,
    content_origin: bool,
) -> Option<Pos2> {
    let (first, second) = if content_origin {
        (inner, outer)
    } else {
        (outer, inner)
    };
    first.or(second).map(|rect| rect.min)
}

fn sane_size(size: Vec2) -> bool {
    [size.x, size.y]
        .iter()
        .all(|&side| side.is_finite() && side > 0.0 && side <= MAX_SIZE)
}

fn sane_position(position: Pos2) -> bool {
    [position.x, position.y]
        .iter()
        .all(|&coordinate| coordinate.is_finite() && coordinate.abs() <= MAX_COORDINATE)
}

/// Whether `now` differs from `last` by more than settling.
fn moved(last: Option<[f32; 2]>, now: [f32; 2]) -> bool {
    last.is_none_or(|last| {
        (last[0] - now[0]).abs() > SETTLING || (last[1] - now[1]).abs() > SETTLING
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    fn normal(size: [f32; 2], position: [f32; 2]) -> Seen {
        Seen {
            size: Some(size.into()),
            position: Some(position.into()),
            maximized: Some(false),
            fullscreen: Some(false),
            minimized: Some(false),
        }
    }

    /// The window's size and position are taken, so the next window opens
    /// where this one is.
    #[test]
    fn the_window_remembers_its_size_and_position() {
        let mut geometry = Geometry::default();
        assert!(geometry.fold(normal([1280.0, 800.0], [96.0, 46.0])));
        assert_eq!(
            geometry,
            Geometry {
                size: Some([1280.0, 800.0]),
                position: Some([96.0, 46.0]),
                maximized: false,
            }
        );

        // A sub-pixel change is the window manager settling.
        assert!(!geometry.fold(normal([1280.2, 800.2], [96.2, 46.2])));
        assert_eq!(geometry.size, Some([1280.0, 800.0]));

        assert!(geometry.fold(normal([1400.0, 900.0], [200.0, 120.0])));
        assert_eq!(geometry.size, Some([1400.0, 900.0]));
        assert_eq!(geometry.position, Some([200.0, 120.0]));
    }

    /// Maximizing remembers the state without losing the normal size behind
    /// it, and unmaximizing clears it.
    #[test]
    fn maximizing_keeps_the_normal_geometry() {
        let mut geometry = Geometry::default();
        geometry.fold(normal([1280.0, 800.0], [100.0, 50.0]));

        let maximized = Seen {
            maximized: Some(true),
            ..normal([1920.0, 1050.0], [0.0, 0.0])
        };
        assert!(geometry.fold(maximized));
        assert!(!geometry.fold(maximized), "already remembered");
        assert_eq!(
            geometry,
            Geometry {
                size: Some([1280.0, 800.0]),
                position: Some([100.0, 50.0]),
                maximized: true,
            }
        );

        assert!(geometry.fold(normal([1280.0, 800.0], [100.0, 50.0])));
        assert!(!geometry.maximized);
    }

    #[test]
    fn minimized_and_full_screen_windows_are_not_remembered() {
        let mut geometry = Geometry::default();
        geometry.fold(normal([1280.0, 800.0], [100.0, 50.0]));
        let before = geometry;
        for seen in [
            Seen {
                minimized: Some(true),
                ..normal([0.0, 0.0], [-32000.0, -32000.0])
            },
            Seen {
                fullscreen: Some(true),
                ..normal([2560.0, 1440.0], [0.0, 0.0])
            },
        ] {
            assert!(!geometry.fold(seen));
            assert_eq!(geometry, before);
        }
    }

    /// Without a position (Wayland) the size is still taken and the last
    /// position kept.
    #[test]
    fn a_window_without_a_position_keeps_the_last_one() {
        let mut geometry = Geometry {
            position: Some([100.0, 50.0]),
            ..Geometry::default()
        };
        let seen = Seen {
            position: None,
            ..normal([1280.0, 800.0], [0.0, 0.0])
        };
        assert!(geometry.fold(seen));
        assert_eq!(geometry.size, Some([1280.0, 800.0]));
        assert_eq!(geometry.position, Some([100.0, 50.0]));
    }

    /// macOS: egui reads the maximized state only at creation, so an unknown
    /// state leaves the flag alone and the size is taken as it is.
    #[test]
    fn an_unknown_maximized_state_leaves_the_flag() {
        let mut geometry = Geometry {
            maximized: true,
            ..Geometry::default()
        };
        let seen = Seen {
            maximized: None,
            ..normal([1280.0, 800.0], [100.0, 50.0])
        };
        assert!(geometry.fold(seen));
        assert!(geometry.maximized);
        assert_eq!(geometry.size, Some([1280.0, 800.0]));
    }

    #[test]
    fn unusable_values_are_not_remembered() {
        let mut geometry = Geometry::default();
        let seen = Seen {
            size: Some(vec2(0.0, 800.0)),
            position: Some(pos2(f32::NAN, 50.0)),
            ..normal([0.0, 0.0], [0.0, 0.0])
        };
        assert!(!geometry.fold(seen));
        assert_eq!(geometry, Geometry::default());
    }

    #[test]
    fn the_builder_position_is_the_corner_with_position_places() {
        let inner = Rect::from_min_size(pos2(100.0, 78.0), vec2(800.0, 600.0));
        let outer = Rect::from_min_size(pos2(96.0, 50.0), vec2(808.0, 632.0));
        assert_eq!(
            builder_position(Some(inner), Some(outer), false),
            Some(pos2(96.0, 50.0))
        );
        assert_eq!(
            builder_position(Some(inner), Some(outer), true),
            Some(pos2(100.0, 78.0))
        );
        assert_eq!(
            builder_position(Some(inner), None, false),
            Some(pos2(100.0, 78.0))
        );
        assert_eq!(builder_position(None, None, true), None);
    }

    #[test]
    fn the_builder_gets_the_remembered_geometry() {
        let remembered = Geometry {
            size: Some([1280.0, 800.0]),
            position: Some([100.0, 50.0]),
            maximized: true,
        };
        let default = egui::ViewportBuilder::default().with_inner_size([1180.0, 780.0]);
        let built = remembered.apply_on(default.clone(), None);
        assert_eq!(built.inner_size, Some(vec2(1280.0, 800.0)));
        assert_eq!(built.position, Some(pos2(100.0, 50.0)));
        assert_eq!(built.maximized, Some(true));

        let built = Geometry::default().apply_on(default, None);
        assert_eq!(built.inner_size, Some(vec2(1180.0, 780.0)));
        assert_eq!(built.position, None);
        assert_eq!(built.maximized, None);
    }

    /// A corrupt settings file opens the default window.
    #[test]
    fn insane_values_are_not_applied() {
        let default = egui::ViewportBuilder::default().with_inner_size([1180.0, 780.0]);
        for (size, position) in [
            ([f32::NAN, 800.0], [0.0, f32::NAN]),
            ([0.0, 800.0], [f32::INFINITY, 0.0]),
            ([1280.0, 20_000.0], [40_000.0, 0.0]),
            ([-1280.0, 800.0], [0.0, -40_000.0]),
        ] {
            let geometry = Geometry {
                size: Some(size),
                position: Some(position),
                maximized: false,
            };
            let built = geometry.apply_on(default.clone(), None);
            assert_eq!(built.inner_size, Some(vec2(1180.0, 780.0)));
            assert_eq!(built.position, None);
        }
    }

    /// Spotifast #719 on macOS: a position on a display that has since been
    /// unplugged is dropped before the window is made there.
    #[test]
    fn a_position_on_no_screen_is_dropped() {
        let screens = [Rect::from_min_size(pos2(0.0, 25.0), vec2(1440.0, 875.0))];
        let geometry = Geometry {
            position: Some([-1800.0, 200.0]),
            ..Geometry::default()
        };
        assert_eq!(
            geometry
                .apply_on(Default::default(), Some(&screens))
                .position,
            None
        );
        let geometry = Geometry {
            position: Some([200.0, 200.0]),
            ..Geometry::default()
        };
        assert_eq!(
            geometry
                .apply_on(Default::default(), Some(&screens))
                .position,
            Some(pos2(200.0, 200.0))
        );
    }
}
