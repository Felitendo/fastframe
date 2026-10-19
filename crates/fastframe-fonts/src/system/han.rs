//! Select a small set of complementary Han faces from CoreText's cascade.

/// Shared Han, simplified, traditional and Japanese gaps reported by apps.
pub(super) const PROBES: &[char] = &[
    '中', '们', '谢', '联', '历', '单', '门', '単', '歷', '込', '気', '楽', '駅', '辺', '歩',
];
/// Separate keys keep later faces from replacing the first in egui.
pub(super) const KEYS: &[&str] = &["han", "han-2", "han-3", "han-4"];

/// Coverage measured by the OS reader, without any platform handles.
pub(super) struct Coverage {
    /// A bit per probe whose outline can be rasterized.
    pub(super) probes: u32,
    /// Number of mapped ideographs in the basic Han block.
    pub(super) ideographs: usize,
}

/// Keep the first broad face, then faces that fill a gap, up to four.
/// A short Korean hanja face cannot take precedence over a broad Han face.
pub(super) fn select(candidates: &[Coverage]) -> Vec<usize> {
    let mut order: Vec<_> = (0..candidates.len()).collect();
    order.sort_by_key(|&index| candidates[index].ideographs < 10_000);
    let mut covered = 0;
    let mut kept = Vec::new();
    for index in order {
        let coverage = candidates[index].probes;
        if coverage & 1 != 0 && coverage & !covered != 0 {
            kept.push(index);
            covered |= coverage;
            if kept.len() == KEYS.len() {
                break;
            }
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    fn face(text: &str, ideographs: usize) -> Coverage {
        Coverage {
            probes: PROBES.iter().enumerate().fold(0, |bits, (i, ch)| {
                bits | (u32::from(text.contains(*ch)) << i)
            }),
            ideographs,
        }
    }
    #[test]
    fn japanese_first_keeps_its_glyphs_and_adds_simplified_chinese() {
        let faces = [
            face("中単込気楽駅辺歩", 10767),
            face("中们谢联历单门歷", 20000),
            face("中们谢联历单门歷", 20000),
        ];
        assert_eq!(select(&faces), [0, 1]);
    }
    #[test]
    fn korean_first_puts_a_full_face_ahead_and_covers_both_gaps() {
        let faces = [
            face("中歷", 4620),
            face("中単込気楽駅辺歩", 10767),
            face("中们谢联历单门歷", 20000),
        ];
        assert_eq!(select(&faces), [1, 2]);
    }
    #[test]
    fn unreadable_redundant_and_excess_faces_are_left_out() {
        let mut faces = vec![face("", 20000), face("中们", 20000), face("中们", 20000)];
        for ch in ['谢', '联', '历', '单', '门'] {
            faces.push(face(&format!("中{ch}"), 20000));
        }
        assert_eq!(select(&faces), [1, 3, 4, 5]);
        assert!(select(&[]).is_empty());
    }
    #[test]
    fn a_short_face_still_works_when_it_is_all_the_system_has() {
        assert_eq!(select(&[face("中歷", 4620)]), [0]);
    }
}
