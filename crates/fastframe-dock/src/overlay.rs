use std::time::{Duration, Instant};

pub(crate) const ICON_SIZE: usize = 16;

const PADDING_X: usize = 1;
const BORDER: usize = 1;

const BORDER_COLOR: [u8; 4] = [27, 31, 35, 255];
const BADGE_COLOR: [u8; 4] = [220, 53, 69, 255];
const TEXT_COLOR: [u8; 4] = [255, 255, 255, 255];

/// Attempts at one count before waiting for the taskbar to announce itself.
const MAX_ATTEMPTS: u8 = 5;
const FIRST_RETRY: Duration = Duration::from_millis(500);

/// Which count is shown, and the backoff for one that failed to apply.
#[derive(Default)]
pub(crate) struct Schedule {
    applied: Option<u32>,
    retry: Option<Retry>,
}

struct Retry {
    count: u32,
    attempts: u8,
    /// `None` once the attempts are spent.
    at: Option<Instant>,
}

impl Schedule {
    pub(crate) fn due(&self, count: u32, now: Instant) -> bool {
        if self.applied == Some(count) {
            return false;
        }
        match &self.retry {
            Some(retry) if retry.count == count => retry.at.is_some_and(|at| now >= at),
            _ => true,
        }
    }

    pub(crate) fn succeeded(&mut self, count: u32) {
        self.applied = Some(count);
        self.retry = None;
    }

    /// Returns when to try again, or `None` when this count has had its attempts.
    pub(crate) fn failed(&mut self, count: u32, now: Instant) -> Option<Instant> {
        let attempts = match &self.retry {
            Some(retry) if retry.count == count => retry.attempts + 1,
            _ => 1,
        };
        let at = (attempts < MAX_ATTEMPTS)
            .then(|| now + FIRST_RETRY * 2u32.pow(u32::from(attempts - 1)));
        self.retry = Some(Retry {
            count,
            attempts,
            at,
        });
        at
    }
}

fn label(count: u32) -> Option<String> {
    match count {
        0 => None,
        1..=99 => Some(count.to_string()),
        _ => Some("99+".to_owned()),
    }
}

pub(crate) fn overlay_rgba(count: u32) -> Vec<u8> {
    let mut rgba = vec![0; ICON_SIZE * ICON_SIZE * 4];
    let Some(label) = label(count) else {
        return rgba;
    };

    let characters = label.chars().count();
    let compact = characters > 2;
    let glyph_width = if compact { 3 } else { 5 };
    let glyph_height = if compact { 5 } else { 7 };
    let padding_y = if compact { 3 } else { 2 };
    let text_width = characters * glyph_width + characters.saturating_sub(1);
    let inner_height = glyph_height + padding_y * 2;
    let inner_width = (text_width + PADDING_X * 2).max(inner_height);
    let outer_width = inner_width + BORDER * 2;
    let outer_height = inner_height + BORDER * 2;
    let left = (ICON_SIZE - outer_width) / 2;
    let top = (ICON_SIZE - outer_height) / 2;

    fill_capsule(
        &mut rgba,
        left,
        top,
        outer_width,
        outer_height,
        BORDER_COLOR,
    );
    fill_capsule(
        &mut rgba,
        left + BORDER,
        top + BORDER,
        inner_width,
        inner_height,
        BADGE_COLOR,
    );

    let text_left = left + (outer_width - text_width) / 2;
    let text_top = top + (outer_height - glyph_height) / 2;
    for (index, character) in label.chars().enumerate() {
        let rows = if compact {
            glyph_small(character).map(|rows| [rows[0], rows[1], rows[2], rows[3], rows[4], 0, 0])
        } else {
            glyph_large(character)
        };
        let Some(rows) = rows else {
            continue;
        };
        let glyph_left = text_left + index * (glyph_width + 1);
        for (row, bits) in rows.into_iter().take(glyph_height).enumerate() {
            for column in 0..glyph_width {
                if bits & (1 << (glyph_width - column - 1)) != 0 {
                    set_pixel(&mut rgba, glyph_left + column, text_top + row, TEXT_COLOR);
                }
            }
        }
    }
    rgba
}

fn glyph_small(character: char) -> Option<[u8; 5]> {
    Some(match character {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '+' => [0b000, 0b010, 0b111, 0b010, 0b000],
        _ => return None,
    })
}

fn glyph_large(character: char) -> Option<[u8; 7]> {
    Some(match character {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        '6' => [
            0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110,
        ],
        _ => return None,
    })
}

fn fill_capsule(
    rgba: &mut [u8],
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    color: [u8; 4],
) {
    let radius = height as f32 / 2.0;
    let center_y = top as f32 + radius;
    let left_center = left as f32 + radius;
    let right_center = left as f32 + width as f32 - radius;
    for y in top..top + height {
        for x in left..left + width {
            let px = x as f32 + 0.5;
            let center_x = if px < left_center {
                left_center
            } else if px > right_center {
                right_center
            } else {
                set_pixel(rgba, x, y, color);
                continue;
            };
            if (px - center_x).powi(2) + (y as f32 + 0.5 - center_y).powi(2) <= radius.powi(2) {
                set_pixel(rgba, x, y, color);
            }
        }
    }
}

fn set_pixel(rgba: &mut [u8], x: usize, y: usize, color: [u8; 4]) {
    let start = (y * ICON_SIZE + x) * 4;
    rgba[start..start + 4].copy_from_slice(&color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_counts_use_a_compact_label() {
        assert_eq!(label(0), None);
        assert_eq!(label(7).as_deref(), Some("7"));
        assert_eq!(label(42).as_deref(), Some("42"));
        assert_eq!(label(100).as_deref(), Some("99+"));
    }

    #[test]
    fn zero_has_no_overlay_pixels() {
        assert!(overlay_rgba(0).iter().all(|channel| *channel == 0));
    }

    #[test]
    fn unread_count_is_drawn_on_a_transparent_overlay() {
        for count in [1, 42, 100] {
            let icon = overlay_rgba(count);
            let pixels = icon.as_chunks::<4>().0;
            assert!(pixels.contains(&BADGE_COLOR));
            assert!(pixels.contains(&TEXT_COLOR));
            assert_eq!(pixels[0], [0; 4]);
        }
    }

    #[test]
    fn plus_sign_is_centred() {
        assert_eq!(glyph_small('+'), Some([0b000, 0b010, 0b111, 0b010, 0b000]));
    }

    #[test]
    fn an_applied_count_is_not_applied_again() {
        let now = Instant::now();
        let mut schedule = Schedule::default();
        assert!(schedule.due(2, now));
        schedule.succeeded(2);
        assert!(!schedule.due(2, now));
        assert!(schedule.due(3, now));
    }

    #[test]
    fn a_failed_count_waits_for_its_backoff() {
        let now = Instant::now();
        let mut schedule = Schedule::default();
        let at = schedule
            .failed(2, now)
            .expect("first failure schedules a retry");
        assert_eq!(at, now + FIRST_RETRY);
        assert!(!schedule.due(2, now));
        assert!(schedule.due(2, at));
        assert_eq!(schedule.failed(2, at), Some(at + FIRST_RETRY * 2));
        // A new count does not wait for the old one's backoff.
        assert!(schedule.due(3, now));
    }

    #[test]
    fn a_count_stops_retrying_after_its_attempts() {
        let now = Instant::now();
        let mut schedule = Schedule::default();
        for _ in 1..MAX_ATTEMPTS {
            assert!(schedule.failed(2, now).is_some());
        }
        assert_eq!(schedule.failed(2, now), None);
        assert!(!schedule.due(2, now + Duration::from_secs(3600)));
        // A changed count, or a recreated button, tries again.
        assert!(schedule.due(3, now));
        schedule = Schedule::default();
        assert!(schedule.due(2, now));
    }
}
