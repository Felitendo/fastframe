//! Warns in the log when the window stops drawing for a moment.

use std::time::{Duration, Instant};

/// An update (logic and drawing) this long freezes the window visibly.
const STALL: Duration = Duration::from_millis(250);
/// At most one stall warning in this long; the next counts the rest.
const WARNING_GAP: Duration = Duration::from_secs(10);

/// Times each update of a window and logs a warning when one takes long
/// enough to freeze it, so a freeze shows in the log with how long it
/// lasted.
///
/// Keep one in the window's `eframe::App` and run `logic` and `ui` through
/// it. A pass whose `ui` eframe skips (a minimized window) counts its logic
/// alone. The first update, which builds fonts and textures, is never
/// reported, and a burst of stalls makes one warning every ten seconds that
/// counts the others.
///
/// ```no_run
/// # struct App;
/// # impl App {
/// #     fn logic(&mut self, _: &egui::Context) {}
/// #     fn ui(&mut self, _: &mut egui::Ui) {}
/// # }
/// struct Window { app: App, stalls: fastframe_shell::StallWatch }
///
/// impl eframe::App for Window {
///     fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
///         self.stalls.logic(|| self.app.logic(ctx));
///     }
///     fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
///         self.stalls.ui(|| self.app.ui(ui));
///     }
/// }
/// ```
#[derive(Debug, Default)]
pub struct StallWatch {
    /// The first update builds fonts and textures; it is not a freeze.
    started: bool,
    warned_at: Option<Instant>,
    /// Stalls since the last warning that it did not report.
    unreported: u32,
    /// How long this pass's logic took, until its drawing adds to it.
    logic_took: Option<Duration>,
}

impl StallWatch {
    /// Runs the window's logic, timing it.
    pub fn logic<T>(&mut self, logic: impl FnOnce() -> T) -> T {
        // The last pass's logic drew nothing after it: it was the update.
        if let Some(took) = self.logic_took.take() {
            self.check(took);
        }
        let started = Instant::now();
        let result = logic();
        self.logic_took = Some(started.elapsed());
        result
    }

    /// Runs the window's drawing, and warns if it and this pass's logic
    /// together took too long.
    pub fn ui<T>(&mut self, ui: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let result = ui();
        let logic_took = self.logic_took.take().unwrap_or_default();
        self.check(logic_took + started.elapsed());
        result
    }

    fn check(&mut self, took: Duration) {
        if let Some(warning) = self.finished(took, Instant::now()) {
            log::warn!("{warning}");
        }
    }

    /// The warning to log for an update that took `took`, if any.
    fn finished(&mut self, took: Duration, now: Instant) -> Option<String> {
        if !std::mem::replace(&mut self.started, true) || took < STALL {
            return None;
        }
        if self
            .warned_at
            .is_some_and(|at| now.duration_since(at) < WARNING_GAP)
        {
            self.unreported += 1;
            return None;
        }
        self.warned_at = Some(now);
        let mut warning = format!(
            "the window stalled for {} ms in one update",
            took.as_millis()
        );
        match std::mem::take(&mut self.unreported) {
            0 => {}
            1 => warning.push_str(" (and once more since the last warning)"),
            more => warning.push_str(&format!(" (and {more} more times since the last warning)")),
        }
        Some(warning)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slow update is logged with its length, once per stall, and a burst
    /// of stalls makes one warning that counts the rest.
    #[test]
    fn a_stalled_update_is_logged_once_with_its_length() {
        let mut watch = StallWatch::default();
        let start = Instant::now();
        assert_eq!(
            watch.finished(Duration::from_secs(2), start),
            None,
            "the first update sets the window up"
        );
        assert_eq!(watch.finished(Duration::from_millis(16), start), None);
        assert_eq!(
            watch
                .finished(Duration::from_millis(1200), start)
                .as_deref(),
            Some("the window stalled for 1200 ms in one update")
        );
        let soon = start + Duration::from_secs(1);
        assert_eq!(watch.finished(Duration::from_millis(400), soon), None);
        assert_eq!(watch.finished(Duration::from_millis(300), soon), None);
        let later = start + WARNING_GAP;
        assert_eq!(
            watch.finished(Duration::from_millis(900), later).as_deref(),
            Some(
                "the window stalled for 900 ms in one update (and 2 more times since the last warning)"
            )
        );
        let much_later = later + WARNING_GAP;
        assert_eq!(
            watch
                .finished(Duration::from_millis(300), much_later)
                .as_deref(),
            Some("the window stalled for 300 ms in one update"),
            "the count starts again"
        );
    }

    /// The logic and the drawing of one pass make one update, and a pass
    /// that only ran its logic is an update of its own.
    #[test]
    fn logic_and_drawing_are_timed_together() {
        let mut watch = StallWatch {
            started: true,
            ..StallWatch::default()
        };
        assert_eq!(watch.logic(|| 7), 7);
        assert!(watch.logic_took.is_some());
        assert_eq!(watch.ui(|| "drawn"), "drawn");
        assert_eq!(watch.logic_took, None, "the drawing took the logic's time");
        watch.logic(|| ());
        watch.logic(|| ());
        assert!(watch.logic_took.is_some(), "the second pass is still open");
    }
}
