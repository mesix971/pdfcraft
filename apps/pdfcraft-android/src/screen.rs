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

/// The window's own insets `[left, top, right, bottom]` (pixels: the bars, a cutout, the keyboard)
/// in points, for a `[w, h]` pixel window drawn as `points`. Negative insets count as none, and
/// none reaches past the window.
pub fn window_insets(pixels: [i32; 4], window: [i32; 2], points: [f32; 2]) -> [f32; 4] {
    let [w, h] = window;
    let [pw, ph] = points;
    if w <= 0 || h <= 0 || !(pw.is_finite() && ph.is_finite() && pw > 0.0 && ph > 0.0) {
        return [0.0; 4];
    }
    let [left, top, right, bottom] = pixels;
    let (sx, sy) = (pw / w as f32, ph / h as f32);
    [left.clamp(0, w) as f32 * sx, top.clamp(0, h) as f32 * sy, right.clamp(0, w) as f32 * sx, bottom.clamp(0, h) as f32 * sy]
}

/// Each side's larger inset: what the content rectangle leaves out (older Androids) or what the
/// window says it covers (Android 15 and later draw the app under the bars).
pub fn widest(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let mut out = [0.0; 4];
    for (o, (x, y)) in out.iter_mut().zip(a.into_iter().zip(b)) {
        *o = x.max(y);
    }
    out
}

/// Whether white icons would be hard to read on `rgb` (so a system bar in that colour gets dark
/// ones).
pub fn is_light([r, g, b]: [u8; 3]) -> bool {
    0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b) > 0.6 * 255.0
}

#[cfg(test)]
mod tests {
    /// Android 15 and later: the window covers the bars and says so in its insets, which count as
    /// much as the content rectangle did; a keyboard larger than the window stops at its edge.
    #[test]
    fn the_windows_insets_cover_the_bars_and_the_keyboard() {
        let points = [360.0, 800.0];
        let window = super::window_insets([0, 66, 0, 132], [1080, 2400], points);
        assert_eq!(window, [0.0, 22.0, 0.0, 44.0]);
        assert_eq!(super::window_insets([-5, 0, 0, 9999], [1080, 2400], points), [0.0, 0.0, 0.0, 800.0]);
        assert_eq!(super::window_insets([10, 10, 10, 10], [0, 2400], points), [0.0; 4]);
        // Edge-to-edge, the content rectangle is the whole window: the window's insets count.
        let content = super::insets([0, 0, 1080, 2400], [1080, 2400], points);
        assert_eq!(super::widest(content, window), [0.0, 22.0, 0.0, 44.0]);
        // Before, the content rectangle left the bars and the keyboard out.
        assert_eq!(super::widest(super::insets([0, 66, 1080, 1500], [1080, 2400], points), [0.0; 4]), [0.0, 22.0, 0.0, 300.0]);
    }

    #[test]
    fn a_bar_gets_icons_that_read_on_it() {
        // Home's grounds and the bars' colours, light and dark.
        assert!(super::is_light([0xf5, 0xf8, 0xf7]));
        assert!(super::is_light([0xff, 0xff, 0xff]));
        assert!(!super::is_light([0x0d, 0x11, 0x10]));
        assert!(!super::is_light([0x26, 0x26, 0x2a]));
        // The accent green takes white icons.
        assert!(!super::is_light([0x0e, 0x7f, 0x6a]));
    }

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
