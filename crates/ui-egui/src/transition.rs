//! The phone shell's transitions: a screen arriving from the side it leads to, a Home tab sliding
//! in from its side, a sheet rising from the bottom, the reader's bars sliding away and back,
//! Home's tiles coming in when the app starts.
//!
//! Each is short (about a fifth of a second), eased out, and only says where something came from:
//! nothing moves while pages scroll or text is typed. A tap during one lands on the button under
//! the finger (egui moves a layer's input with its graphics; a page's own gestures read the
//! finger unmoved, a few points off for that fifth of a second). With animation effects off
//! ([`crate::motion::animated`]) everything is in place at once.

use egui::emath::{TSTransform, easing};
use egui::{Color32, Context, CornerRadius, Id, LayerId, Rect, Vec2, vec2};

/// How long a screen, a tab or a sheet takes to arrive, in seconds.
const ARRIVE: f64 = 0.22;
/// How far a screen or a tab slides in from its side, in points.
pub(crate) const SLIDE: f32 = 36.0;
/// How long the reader's bars take to slide away or back, in seconds.
const BARS: f32 = 0.18;
/// How dark the scrim under a sheet is, and how long it takes to clear once the sheet closes.
pub(crate) const SCRIM_ALPHA: u8 = 110;
const SCRIM_CLEAR: f64 = 0.15;
/// Home's tiles when the app starts: each one's time to come in, the gap between two, and how far
/// below its place it starts.
const TILE_IN: f64 = 0.28;
const TILE_GAP: f64 = 0.045;
const TILE_RISE: f32 = 18.0;

/// What a key showed, since when, and the pass it was last drawn in.
#[derive(Clone, Copy, Debug)]
struct Shown {
    what: u64,
    rank: i32,
    since: f64,
    from: f32,
    pass: u64,
    seen: f64,
}

/// How far along something's arrival is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Arrival {
    /// 0 as it starts, 1 once in place, eased out.
    pub(crate) done: f32,
    /// The side it comes from: 1 the right (further in), -1 the left (back out).
    pub(crate) from: f32,
}

impl Arrival {
    pub(crate) const IN_PLACE: Self = Self { done: 1.0, from: 1.0 };

    /// How far it still is from its place, over a way in `distance` long.
    pub(crate) fn offset(self, distance: f32) -> f32 {
        self.from * (1.0 - self.done) * distance
    }

    pub(crate) fn opacity(self) -> f32 {
        self.done
    }
}

/// How far along `since` is on a way `length` seconds long (1 at the end, or for a time that
/// isn't a number); repaints until it ends.
fn along(ctx: &Context, now: f64, since: f64, length: f64) -> f32 {
    let x = (now - since) / length;
    let x = if x.is_nan() { 1.0 } else { x.clamp(0.0, 1.0) };
    if x < 1.0 {
        ctx.request_repaint();
    }
    x as f32
}

/// Record what `key` shows this pass and how far along its arrival is. It arrives anew when it
/// shows something else than last time (`what`, from the side `rank` says), and, when it
/// `appears`, also when it wasn't drawn the pass before. The first time, it is in place already
/// unless it `appears`.
fn arrive(ctx: &Context, key: Id, what: u64, rank: i32, appears: bool) -> Arrival {
    let (now, pass) = (ctx.input(|i| i.time), ctx.cumulative_pass_nr());
    let last: Option<Shown> = ctx.data(|d| d.get_temp(key));
    let shown = match last {
        Some(s) if s.what == what && !(appears && s.pass.saturating_add(1) < pass) => Shown { pass, seen: now, ..s },
        Some(s) => Shown { what, rank, since: now, from: if rank < s.rank { -1.0 } else { 1.0 }, pass, seen: now },
        None => Shown { what, rank, since: if appears { now } else { f64::NEG_INFINITY }, from: 1.0, pass, seen: now },
    };
    ctx.data_mut(|d| d.insert_temp(key, shown));
    if !crate::motion::animated(ctx) {
        return Arrival::IN_PLACE;
    }
    Arrival { done: easing::cubic_out(along(ctx, now, shown.since, ARRIVE)), from: shown.from }
}

/// The arrival of what `key` shows: `what` identifies it, and `rank` says how far in it is (Home 0,
/// a document 1, a panel over it 2; or a tab's place along the tab bar). One further in comes from
/// the right, one further out from the left.
pub(crate) fn change(ctx: &Context, key: &str, what: impl std::hash::Hash, rank: i32) -> Arrival {
    arrive(ctx, Id::new(key), egui::epaint::util::hash(what), rank, false)
}

/// The arrival of something that comes and goes (a sheet, the file browser): it arrives whenever
/// it wasn't drawn the pass before.
pub(crate) fn appearing(ctx: &Context, key: Id) -> Arrival {
    arrive(ctx, key, 0, 0, true)
}

fn screen_id() -> Id {
    Id::new("phone-screen-arrival")
}

/// The shell's screen this pass (`what` identifies it, `rank` as for [`change`]): its arrival,
/// which the screen's own layers follow ([`follow_screen`]).
pub(crate) fn screen(ctx: &Context, what: impl std::hash::Hash, rank: i32) -> Arrival {
    let arrival = change(ctx, "phone-screen", what, rank);
    ctx.data_mut(|d| d.insert_temp(screen_id(), arrival));
    arrival
}

/// The screen's arrival this pass.
fn screen_arrival(ctx: &Context) -> Arrival {
    ctx.data(|d| d.get_temp(screen_id())).unwrap_or(Arrival::IN_PLACE)
}

/// Move the layer `ui` paints on (a bar, the page pill: the screen's own [`egui::Area`]s) along
/// with the screen's arrival, at the `opacity` of the [`egui::Ui`] that drew it (the screen's
/// fade, and a Home tab's).
pub(crate) fn follow_screen(ui: &mut egui::Ui, opacity: f32) {
    place_layer(ui.ctx(), ui.layer_id(), vec2(screen_arrival(ui.ctx()).offset(SLIDE), 0.0));
    ui.set_opacity(opacity);
}

/// Draw `layer` moved by `offset` (nothing once it is zero), its input with it.
pub(crate) fn place_layer(ctx: &Context, layer: LayerId, offset: Vec2) {
    // A zero offset is egui's identity, which forgets the layer's transform.
    ctx.set_transform_layer(layer, TSTransform::from_translation(offset));
}

/// Under a screen moved `dx` across: the strip it leaves bare, in the screen's `ground` (painted
/// first, with a clip moved back so the strip is covered once the layer is moved).
pub(crate) fn bare_strip(ui: &egui::Ui, screen: Rect, dx: f32, ground: Color32) {
    if dx == 0.0 {
        return;
    }
    let behind = screen.translate(vec2(-dx, 0.0));
    let mut painter = ui.painter().clone();
    painter.set_clip_rect(behind);
    painter.rect_filled(behind, CornerRadius::ZERO, ground);
}

fn scrim_id() -> Id {
    Id::new("phone-sheet-scrim")
}

fn scrim_alpha_id() -> Id {
    Id::new("phone-sheet-scrim-alpha")
}

/// How dark a sheet's scrim is this pass: darkening as the first sheet rises, and as dark at once
/// when one sheet gives way to another.
pub(crate) fn sheet_scrim(ctx: &Context) -> u8 {
    let alpha = (f32::from(SCRIM_ALPHA) * appearing(ctx, scrim_id()).done).round() as u8;
    // Where the scrim clears from, if the sheet closes now.
    ctx.data_mut(|d| d.insert_temp(scrim_alpha_id(), alpha));
    alpha
}

/// After the last sheet closed: its scrim clearing over `screen`. Once a pass, after the sheets.
pub(crate) fn clearing_scrim(ctx: &Context, screen: Rect) {
    let Some(last) = ctx.data(|d| d.get_temp::<Shown>(scrim_id())) else { return };
    if last.pass >= ctx.cumulative_pass_nr() || !crate::motion::animated(ctx) {
        return;
    }
    let now = ctx.input(|i| i.time);
    let left = 1.0 - along(ctx, now, last.seen, SCRIM_CLEAR);
    if left <= 0.0 {
        return;
    }
    // From as dark as it was (a sheet closed as it rose had a lighter one).
    let from = ctx.data(|d| d.get_temp::<u8>(scrim_alpha_id())).unwrap_or(SCRIM_ALPHA);
    let painter = ctx.layer_painter(LayerId::new(egui::Order::Middle, Id::new("phone-scrim-clearing")));
    painter.rect_filled(screen, CornerRadius::ZERO, Color32::from_black_alpha((f32::from(from) * left).round() as u8));
}

/// How far out the reader's bars are: 1 shown, 0 away, sliding between. They come with a
/// document arriving, not after it (hidden in the last one read, they would slide in again).
pub(crate) fn bars(ctx: &Context, shown: bool) -> f32 {
    let time = if crate::motion::animated(ctx) && screen_arrival(ctx).done >= 1.0 { BARS } else { 0.0 };
    ctx.animate_bool_with_time_and_easing(Id::new("phone-reader-bars"), shown, time, easing::cubic_out)
}

/// Home's tile `k` coming in, one after another, the first time Home shows: how far below its
/// place it is, and its opacity.
pub(crate) fn tile_entrance(ctx: &Context, k: usize) -> (f32, f32) {
    let now = ctx.input(|i| i.time);
    let start = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(Id::new("phone-home-entrance"), || now));
    if !crate::motion::animated(ctx) {
        return (0.0, 1.0);
    }
    let done = easing::cubic_out(along(ctx, now, start + TILE_GAP * k as f64, TILE_IN));
    ((1.0 - done) * TILE_RISE, done)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pass at `time` with animation effects on or off, running `run`.
    fn pass<R>(ctx: &Context, time: f64, animate: bool, run: impl FnOnce(&Context) -> R) -> R {
        ctx.all_styles_mut(|s| s.animation_time = if animate { 1.0 / 12.0 } else { 0.0 });
        let (mut run, mut out) = (Some(run), None);
        let input = egui::RawInput { time: Some(time), ..Default::default() };
        ctx.run_ui(input, |ui| {
            if let Some(run) = run.take() {
                out = Some(run(ui.ctx()));
            }
        })
        .textures_delta
        .clear();
        out.expect("the pass ran")
    }

    #[test]
    fn a_new_screen_arrives_from_the_side_its_rank_says() {
        let ctx = Context::default();
        // The first screen is in place already.
        assert_eq!(pass(&ctx, 1.0, true, |c| change(c, "k", "home", 0)), Arrival::IN_PLACE);
        // Further in: from the right, starting out of place and transparent.
        let a = pass(&ctx, 2.0, true, |c| change(c, "k", "doc", 1));
        assert_eq!((a.done, a.from), (0.0, 1.0));
        assert_eq!(a.offset(SLIDE), SLIDE);
        let half = pass(&ctx, 2.0 + ARRIVE / 2.0, true, |c| change(c, "k", "doc", 1));
        assert!(half.done > 0.5 && half.done < 1.0, "eased out: {half:?}");
        assert_eq!(pass(&ctx, 2.0 + ARRIVE, true, |c| change(c, "k", "doc", 1)).done, 1.0);
        // Back out: from the left.
        let back = pass(&ctx, 3.0, true, |c| change(c, "k", "home", 0));
        assert_eq!((back.done, back.from), (0.0, -1.0));
        assert_eq!(back.offset(SLIDE), -SLIDE);
        // The same screen on: no new arrival.
        assert_eq!(pass(&ctx, 3.0 + ARRIVE, true, |c| change(c, "k", "home", 0)).done, 1.0);
    }

    #[test]
    fn with_animation_effects_off_everything_is_in_place_at_once() {
        let ctx = Context::default();
        pass(&ctx, 1.0, false, |c| change(c, "k", "home", 0));
        assert_eq!(pass(&ctx, 2.0, false, |c| change(c, "k", "doc", 1)), Arrival::IN_PLACE);
        assert_eq!(pass(&ctx, 2.0, false, |c| appearing(c, Id::new("sheet"))), Arrival::IN_PLACE);
        assert_eq!(pass(&ctx, 2.0, false, |c| tile_entrance(c, 3)), (0.0, 1.0));
        assert_eq!(pass(&ctx, 2.0, false, |c| bars(c, false)), 0.0, "the bars away at once");
    }

    #[test]
    fn something_that_appears_arrives_again_after_a_pass_without_it() {
        let ctx = Context::default();
        let key = Id::new("sheet");
        assert_eq!(pass(&ctx, 1.0, true, |c| appearing(c, key)).done, 0.0, "the first time too");
        assert_eq!(pass(&ctx, 1.0 + ARRIVE, true, |c| appearing(c, key)).done, 1.0);
        // A pass without it, then back.
        pass(&ctx, 2.0, true, |_| ());
        assert_eq!(pass(&ctx, 2.1, true, |c| appearing(c, key)).done, 0.0);
    }

    #[test]
    fn a_closed_sheets_scrim_clears_then_is_gone() {
        let ctx = Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, vec2(400.0, 800.0));
        pass(&ctx, 1.0, true, sheet_scrim);
        assert!(pass(&ctx, 1.0 + ARRIVE, true, sheet_scrim) == SCRIM_ALPHA);
        let drawn = |ctx: &Context, time: f64| {
            pass(ctx, time, true, |c| {
                clearing_scrim(c, screen);
                c.graphics(|g| g.get(LayerId::new(egui::Order::Middle, Id::new("phone-scrim-clearing"))).is_some_and(|l| !l.is_empty()))
            })
        };
        assert!(drawn(&ctx, 1.0 + ARRIVE + 0.05), "clearing");
        assert!(!drawn(&ctx, 1.0 + ARRIVE + SCRIM_CLEAR + 0.01), "gone");
        // A sheet drawn pass after pass (or one giving way to the next in the pass after) keeps
        // it as dark.
        pass(&ctx, 5.0, true, sheet_scrim);
        assert_eq!(pass(&ctx, 5.0 + ARRIVE, true, sheet_scrim), SCRIM_ALPHA);
        assert_eq!(pass(&ctx, 5.0 + ARRIVE + 0.02, true, sheet_scrim), SCRIM_ALPHA);
    }

    #[test]
    fn homes_tiles_come_in_one_after_another() {
        let ctx = Context::default();
        let first = pass(&ctx, 1.0, true, |c| (tile_entrance(c, 0), tile_entrance(c, 4)));
        assert_eq!(first, ((TILE_RISE, 0.0), (TILE_RISE, 0.0)));
        let (a, b) = pass(&ctx, 1.0 + TILE_IN * 0.5, true, |c| (tile_entrance(c, 0), tile_entrance(c, 4)));
        assert!(a.1 > b.1, "the first ahead of the fifth: {a:?} {b:?}");
        assert_eq!(pass(&ctx, 1.0 + TILE_IN + 4.0 * TILE_GAP, true, |c| tile_entrance(c, 4)), (0.0, 1.0));
    }

    #[test]
    fn a_time_that_is_not_a_number_is_the_end() {
        let ctx = Context::default();
        assert_eq!(along(&ctx, f64::NAN, 0.0, ARRIVE), 1.0);
        assert_eq!(along(&ctx, 1.0, f64::NEG_INFINITY, ARRIVE), 1.0);
    }
}
