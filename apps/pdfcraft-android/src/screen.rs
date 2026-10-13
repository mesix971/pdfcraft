//! The part of the window the system leaves the app.

/// The insets `[left, top, right, bottom]`, in points, of the content rectangle
/// `[left, top, right, bottom]` (pixels) inside a `[w, h]` pixel window drawn as `points`
/// (`[width, height]`). A rectangle reaching outside the window is clipped to it; an empty one,
/// an empty window or nonsense sizes give no insets.
pub fn insets(content: [i32; 4], window: [i32; 2], points: [f32; 2]) -> [f32; 4] {
    let [w, h] = window;
    let [pw, ph] = points;
    if w <= 0 || h <= 0 || !(pw.is_finite() && ph.is_finite() && pw > 0.0 && ph > 0.0) {
        return [0.0; 4];
    }
    let [left, top, right, bottom] = content;
    let (left, top) = (left.clamp(0, w), top.clamp(0, h));
    let (right, bottom) = (right.clamp(left, w), bottom.clamp(top, h));
    if right <= left || bottom <= top {
        return [0.0; 4];
    }
    let (sx, sy) = (pw / w as f32, ph / h as f32);
    [left as f32 * sx, top as f32 * sy, (w - right) as f32 * sx, (h - bottom) as f32 * sy]
}

#[cfg(test)]
mod tests {
    use super::insets;

    #[test]
    fn the_keyboard_and_bars_become_insets_in_points() {
        // A 1080 × 2400 phone drawn at 2.75 pixels per point; the status bar takes 66 px and
        // the keyboard (with adjustResize) 900 px at the bottom.
        let points = [1080.0 / 2.75, 2400.0 / 2.75];
        let got = insets([0, 66, 1080, 1500], [1080, 2400], points);
        let want = [0.0, 24.0, 0.0, 900.0 / 2.75];
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() < 1e-3, "{got:?}");
        }
        assert_eq!(insets([0, 0, 1080, 2400], [1080, 2400], points), [0.0; 4]);
    }

    #[test]
    fn nonsense_rectangles_and_sizes_give_no_insets() {
        let points = [400.0, 800.0];
        assert_eq!(insets([0, 0, 0, 0], [1080, 2400], points), [0.0; 4], "not laid out yet");
        assert_eq!(insets([500, 500, 100, 100], [1080, 2400], points), [0.0; 4]);
        assert_eq!(insets([0, 0, 1080, 2400], [0, 2400], points), [0.0; 4]);
        assert_eq!(insets([0, 0, 1080, 2400], [1080, 2400], [f32::NAN, 800.0]), [0.0; 4]);
        assert_eq!(insets([0, 0, 1080, 2400], [1080, 2400], [0.0, 800.0]), [0.0; 4]);
        let clipped = insets([-50, -50, 5000, 1200], [1080, 2400], [1080.0, 2400.0]);
        assert_eq!(clipped, [0.0, 0.0, 0.0, 1200.0]);
    }
}
