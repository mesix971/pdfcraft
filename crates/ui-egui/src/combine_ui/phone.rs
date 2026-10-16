//! Combine files as a phone screen: the files as a list of cards, in the order they are combined,
//! each with its first page and its place in the order; an arrow moves it up or down, a cross
//! takes it out, a tap shows it large (or unlocks it), and Combine waits along the bottom. The
//! desktop's grid and table need a mouse (buttons that show on hover, dragging, column headings);
//! this needs only taps.

use std::collections::BTreeSet;

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, Vec2, pos2, vec2};

use super::{CombineFile, Lock, RowAction, blocker, skip_prompt, unlock_prompt};
use crate::phone::{self, TARGET, TOP_BAR};
use crate::phone_home::{self, Look};
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, i18n, icons, widgets};

/// A file's card, and its first page in it.
const CARD: f32 = 96.0;
const PAPER: Vec2 = vec2(46.0, 62.0);
/// The bar along the bottom with Combine.
const BOTTOM: f32 = 84.0;

/// What a card asks for.
enum Act {
    Up(u64),
    Down(u64),
    Remove(u64),
    Show(u64),
    Unlock(usize, egui::Pos2),
}

impl PdfCraftApp {
    /// Something of Combine's shows over its list (a file shown large, the password box, "Leave
    /// out files that can't be read?"): Back is that window's.
    pub(crate) fn combine_covered(&self) -> bool {
        let tab = &self.combine_tab;
        tab.preview.is_some() || tab.unlock.is_some() || tab.confirm_skip
    }
}

/// The colours at the top and bottom of the screen, for the system's bars.
pub(crate) fn system_bars(app: &PdfCraftApp, t: &Tokens) -> (Color32, Color32) {
    let look = phone_home::look(t);
    (look.ground, if app.combine_draft.is_empty() { look.ground } else { look.bar })
}

/// Draw Combine files in the phone layout. `screen` is the content area.
pub(crate) fn page(app: &mut PdfCraftApp, ui: &mut egui::Ui, screen: Rect) {
    let ctx = ui.ctx().clone();
    let t = Tokens::get(&ctx);
    let look = phone_home::look(&t);
    ui.painter().rect_filled(screen, CornerRadius::ZERO, look.ground);
    let checks: Vec<Result<usize, String>> = app.combine_draft.iter_mut().map(CombineFile::selection).collect();
    let n = app.combine_draft.len();
    let blocked = blocker(&app.combine_draft, &checks);

    let top = Rect::from_min_size(screen.min, vec2(screen.width(), TOP_BAR));
    let (mut back, mut run) = (false, None);
    let (can_undo, can_redo) = (app.combine_can_undo(true), app.combine_can_undo(false));
    ui.scope_builder(egui::UiBuilder::new().max_rect(top.shrink2(vec2(4.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        back = icons::button(ui, "chevron-left", TARGET, false, tl!("Back")).clicked();
        let width = ui.available_width() - 2.0 * TARGET;
        phone::title_text(ui, tl!("Combine files"), width, &t);
        if ui.add_enabled_ui(can_undo, |ui| icons::button(ui, "undo-2", TARGET, false, tl!("Undo"))).inner.clicked() {
            run = Some("edit.undo");
        }
        if ui.add_enabled_ui(can_redo, |ui| icons::button(ui, "redo-2", TARGET, false, tl!("Redo"))).inner.clicked() {
            run = Some("edit.redo");
        }
    });

    let low = if n > 0 { BOTTOM } else { 0.0 };
    let list = Rect::from_min_max(pos2(screen.left(), top.bottom()), pos2(screen.right(), (screen.bottom() - low).max(top.bottom())));
    let open_docs = app.views.len();
    let (mut act, mut add, mut add_open) = (None, false, false);
    ui.scope_builder(egui::UiBuilder::new().max_rect(list), |ui| {
        ui.set_clip_rect(list.intersect(ui.clip_rect()));
        if n == 0 {
            // It scrolls when the screen is short (a phone on its side).
            egui::ScrollArea::vertical().id_salt("phone-combine-empty").auto_shrink([false, false]).show(ui, |ui| {
                (add, add_open) = empty(ui, list, open_docs, &t);
            });
            return;
        }
        egui::ScrollArea::vertical().id_salt("phone-combine").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::Frame::NONE.inner_margin(egui::Margin { left: 16, right: 16, top: 6, bottom: 8 }).show(ui, |ui| {
                let hint = tl!("Combined from top to bottom. Use the arrows to change the order.");
                ui.add(egui::Label::new(egui::RichText::new(hint).font(theme::regular(13.5)).color(t.text_muted)).wrap());
            });
            let ppp = ui.ctx().pixels_per_point();
            let selected = app.combine_tab.selected.clone();
            let (draft, thumbs) = (&mut app.combine_draft, &mut app.combine_thumbs);
            for (i, (f, check)) in draft.iter_mut().zip(&checks).enumerate() {
                if let Some(a) = card(ui, f, i, n, check, selected.contains(&f.id), thumbs, ppp, &t, &look) {
                    act = Some(a);
                }
            }
            ui.add_space(4.0);
            add = add_card(ui, &t);
            ui.add_space(24.0);
        });
    });

    let mut combine = false;
    if n > 0 {
        let rect = Rect::from_min_max(pos2(screen.left(), screen.bottom() - low), screen.max);
        combine = bottom(app, ui, rect, &checks, blocked.as_deref(), &t, &look);
    }

    if back {
        app.combine_tab.focused = false;
    }
    if let Some(id) = run {
        app.execute(id);
    }
    if add {
        app.combine_dialog();
    }
    if add_open {
        app.add_open_documents(&(0..open_docs).collect::<Vec<_>>());
    }
    match act {
        // A move acts on the selection: the file alone, and nothing left selected after (the
        // phone shows no selection).
        Some(Act::Up(id)) => {
            app.combine_tab.selected = BTreeSet::from([id]);
            app.combine_apply(RowAction::MoveUp);
            app.combine_tab.selected.clear();
        }
        Some(Act::Down(id)) => {
            app.combine_tab.selected = BTreeSet::from([id]);
            app.combine_apply(RowAction::MoveDown);
            app.combine_tab.selected.clear();
        }
        Some(Act::Remove(id)) => app.combine_apply(RowAction::RemoveFile(id)),
        Some(Act::Show(id)) => app.combine_apply(RowAction::Preview(id)),
        Some(Act::Unlock(i, at)) => app.combine_apply(RowAction::Unlock(Some(i), at)),
        None => {}
    }
    if combine && blocked.is_none() {
        if app.combine_draft.iter().any(CombineFile::unreadable) {
            app.combine_tab.confirm_skip = true;
        } else {
            app.combine_staged();
        }
    }
    unlock_prompt(app, &ctx);
    crate::combine_grid::preview(app, &ctx, &t);
    skip_prompt(app, &ctx);
}

/// One file: its place in the order on its first page, its name and what it adds (or what's
/// wrong), and its arrows and cross.
#[allow(clippy::too_many_arguments)]
fn card(
    ui: &mut egui::Ui,
    f: &mut CombineFile,
    i: usize,
    n: usize,
    check: &Result<usize, String>,
    selected: bool,
    thumbs: &mut crate::combine_grid::Thumbs,
    ppp: f32,
    t: &Tokens,
    look: &Look,
) -> Option<Act> {
    let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width(), CARD + 10.0), Sense::hover());
    let rect = outer.shrink2(vec2(12.0, 5.0));
    if !ui.is_rect_visible(outer) {
        return None;
    }
    let mut act = None;
    let resp = ui.interact(rect, egui::Id::new(("phone-combine-card", f.id)), Sense::click());
    let label = format!("{}. {}", i + 1, f.name);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    let press = theme::Press::track(ui, &resp);
    let stroke = if selected { Stroke::new(1.5, t.accent) } else { Stroke::new(1.0, look.edge) };
    let fill = press.fill(look.surface, look.surface.lerp_to_gamma(t.hover, 0.6), t.pressed);
    ui.painter().rect(rect, CornerRadius::same(16), fill, stroke, egui::StrokeKind::Inside);

    // The first page, white behind it, and its place in the order on its corner.
    let slot = Rect::from_min_size(pos2(rect.left() + 16.0, rect.center().y - PAPER.y / 2.0), PAPER);
    let (size, tex, failed) = crate::combine_grid::first_page_thumb(thumbs, f, PAPER, ppp, true);
    let paper = Rect::from_center_size(slot.center(), size);
    let shadow = egui::Shadow { offset: [0, 1], blur: 4, spread: 0, color: Color32::from_black_alpha(if t.dark() { 90 } else { 30 }) };
    ui.painter().add(shadow.as_shape(paper, CornerRadius::same(3)));
    ui.painter().rect_filled(paper, CornerRadius::same(3), Color32::WHITE);
    if let Some(tex) = tex {
        egui::Image::new((tex, paper.size())).corner_radius(CornerRadius::same(3)).paint_at(ui, paper);
    } else if f.lock.is_some() {
        icons::paint(ui, paper, "lock", 22.0, t.warning);
    } else if f.problem.is_some() {
        icons::paint(ui, paper, "circle-x", 22.0, t.error);
    } else if failed {
        icons::paint(ui, paper, "triangle-alert", 22.0, t.warning);
    }
    ui.painter().rect_stroke(paper, CornerRadius::same(3), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let badge = paper.left_top() + vec2(-2.0, -2.0);
    ui.painter().circle_filled(badge, 11.5, t.accent);
    ui.painter().circle_stroke(badge, 11.5, Stroke::new(2.0, look.surface));
    ui.painter().text(badge, Align2::CENTER_CENTER, (i + 1).to_string(), theme::semibold(11.5), Color32::WHITE);

    // The name, and what it adds or what is wrong.
    let text_left = slot.right() + 14.0;
    let buttons = 2.0 * TARGET + 6.0;
    let width = (rect.right() - buttons - text_left - 4.0).max(0.0);
    let pages = if f.pages == 1 { tl!("1 page").to_owned() } else { i18n::fmt(tl!("{n} pages"), &[("n", &f.pages.to_string())]) };
    let (detail, colour) = match (&f.lock, &f.problem, check) {
        (Some(Lock::Open), ..) => (format!("{} · {}", tl!("Password-protected"), tl!("Tap to unlock")), t.warning),
        (Some(Lock::Permissions), ..) => (tl!("Its security settings don't allow copying pages").to_owned(), t.warning),
        (None, Some(problem), _) => (problem.clone(), t.error),
        (None, None, Err(_)) => (tl!("Check the pages").to_owned(), t.error),
        (None, None, Ok(k)) if !f.range.trim().is_empty() => {
            (i18n::fmt(tl!("Pages: {k} of {n}"), &[("k", &k.to_string()), ("n", &f.pages.to_string())]), t.text_faint)
        }
        (None, None, Ok(_)) => (format!("{pages} · {}", crate::panels::human_size(f.bytes.len())), t.text_faint),
    };
    let body = rect.translate(press.offset());
    // For screen readers: what it adds, or what is wrong, as the card's description.
    let said = detail.clone();
    ui.ctx().accesskit_node_builder(resp.id, |node| node.set_description(said));
    widgets::row_text(ui, pos2(text_left, body.center().y - 11.0), crate::bidi::visual(&f.name), theme::medium(15.5), t.text, width);
    widgets::row_text(ui, pos2(text_left, body.center().y + 12.0), detail, theme::regular(12.5), colour, width);

    // Up and down, one above the other, and the cross. Drawn after the card, so a tap on one is
    // theirs and not the card's.
    let column = Rect::from_min_size(pos2(rect.right() - buttons, rect.top()), vec2(TARGET, rect.height()));
    let half = |k: f32| Rect::from_min_size(pos2(column.left(), column.top() + k * column.height() / 2.0), vec2(TARGET, column.height() / 2.0));
    // A button keeps its taps even when it can't be used (the first file's Up), so a tap there
    // never shows the file large instead.
    let button = |ui: &mut egui::Ui, at: Rect, icon: &str, label: &str, enabled: bool| {
        let r = ui.interact(at, egui::Id::new(("phone-combine-button", f.id, icon)), Sense::click());
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
        let press = theme::Press::track(ui, &r);
        if enabled {
            press.wash(ui, Rect::from_center_size(at.center(), Vec2::splat(36.0)), 18, false);
        }
        let ink = if enabled { t.icon } else { t.text_faint.gamma_multiply(0.5) };
        icons::paint(ui, at.translate(press.offset()), icon, 20.0, ink);
        enabled && theme::hand(r).clicked()
    };
    let up = format!("{} · {}", tl!("Move up"), f.name);
    if button(ui, half(0.0), "chevron-up", &up, i > 0) {
        act = Some(Act::Up(f.id));
    }
    let down = format!("{} · {}", tl!("Move down"), f.name);
    if button(ui, half(1.0), "chevron-down", &down, i + 1 < n) {
        act = Some(Act::Down(f.id));
    }
    let cross = Rect::from_min_size(pos2(column.right() + 6.0, rect.top()), vec2(TARGET, rect.height()));
    let remove = i18n::fmt(tl!("Remove {name}"), &[("name", &f.name)]);
    if button(ui, cross, "x", &remove, true) {
        act = Some(Act::Remove(f.id));
    }
    // A tap on the card unlocks it, or shows it large (a file that can't be read has nothing to
    // show: its card says why).
    if act.is_none() && theme::hand(resp).clicked() {
        act = match (f.lock, &f.problem) {
            (Some(_), _) => Some(Act::Unlock(i, rect.left_bottom())),
            (None, Some(_)) => None,
            (None, None) => Some(Act::Show(f.id)),
        };
    }
    act
}

/// "Add files" after the last card. Returns whether it was tapped.
fn add_card(ui: &mut egui::Ui, t: &Tokens) -> bool {
    let (outer, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::click());
    let label = tl!("Add files…").trim_end_matches(['…', '.']).to_owned();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    let press = theme::Press::track(ui, &resp);
    let rect = outer.shrink2(vec2(12.0, 5.0)).translate(press.offset());
    let fill = press.fill(t.accent.gamma_multiply(0.07), t.accent.gamma_multiply(0.12), t.accent.gamma_multiply(0.18));
    ui.painter().rect(rect, CornerRadius::same(16), fill, Stroke::new(1.2, t.accent.gamma_multiply(0.6)), egui::StrokeKind::Inside);
    let font = theme::semibold(15.0);
    let w = widgets::text_width(ui, &label, &font) + 28.0;
    let left = rect.center().x - w / 2.0;
    icons::paint_bold(ui, Rect::from_center_size(pos2(left + 9.0, rect.center().y), Vec2::splat(18.0)), "plus", 18.0, t.accent_text);
    ui.painter().text(pos2(left + 28.0, rect.center().y), Align2::LEFT_CENTER, label, font, t.accent_text);
    theme::hand(resp).clicked()
}

/// No files yet: what Combine does, and the ways to add them. Returns whether "Add files" and
/// "Add open documents" were tapped.
fn empty(ui: &mut egui::Ui, rect: Rect, open_docs: usize, t: &Tokens) -> (bool, bool) {
    let (mut add, mut add_open) = (false, false);
    ui.vertical_centered(|ui| {
        ui.add_space((rect.height() / 2.0 - 190.0).max(24.0));
        let (disc, _) = ui.allocate_exact_size(Vec2::splat(104.0), Sense::hover());
        let hue = phone::tool_color("combine");
        let glow = egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: hue.gamma_multiply(if t.dark() { 0.45 } else { 0.3 }) };
        ui.painter().add(glow.as_shape(disc, CornerRadius::same(52)));
        phone_home::gradient_disc(
            ui.painter(),
            disc.center(),
            52.0,
            hue.lerp_to_gamma(Color32::WHITE, 0.18),
            hue.lerp_to_gamma(Color32::BLACK, 0.25),
        );
        phone::tile_glyph(ui, disc, "files", 46.0);
        ui.add_space(22.0);
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(32, 0)).show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(tl!("Add the files to combine")).font(theme::semibold(20.0)).color(t.text)).wrap());
            ui.add_space(8.0);
            let text = tl!("PDFs and pictures, combined into one PDF in the order you choose.");
            ui.add(egui::Label::new(egui::RichText::new(text).font(theme::regular(14.5)).color(t.text_muted)).wrap());
        });
        ui.add_space(26.0);
        let label = tl!("Add files…").trim_end_matches(['…', '.']).to_owned();
        add = phone::action_pill(ui, Some("plus"), &label, true, t).clicked();
        if open_docs > 0 {
            ui.add_space(10.0);
            add_open = phone::soft_pill(ui, "file-plus", tl!("Add open documents"), t).clicked();
        }
    });
    (add, add_open)
}

/// The totals, what stops Combine if anything does, and Combine. Returns whether it was tapped.
fn bottom(
    app: &PdfCraftApp,
    ui: &mut egui::Ui,
    rect: Rect,
    checks: &[Result<usize, String>],
    blocked: Option<&str>,
    t: &Tokens,
    look: &Look,
) -> bool {
    ui.painter().rect_filled(rect, CornerRadius::ZERO, look.bar);
    ui.painter().hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, look.edge));
    let pages: usize = checks.iter().filter_map(|c| c.as_ref().ok()).sum();
    // A file split into parts counts once.
    let files = app.combine_draft.iter().map(|f| f.group).collect::<BTreeSet<u64>>().len();
    let files = if files == 1 { tl!("1 file").to_owned() } else { i18n::fmt(tl!("{n} files"), &[("n", &files.to_string())]) };
    let pages = if pages == 1 { tl!("1 page").to_owned() } else { i18n::fmt(tl!("{n} pages"), &[("n", &pages.to_string())]) };
    let mut combine = false;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(16.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let label = tl!("Combine");
        let pill_w = widgets::text_width(ui, label, &theme::semibold(15.0)) + 62.0;
        let width = (ui.available_width() - pill_w - 12.0).max(0.0);
        let (r, resp) = ui.allocate_exact_size(vec2(width, rect.height()), Sense::hover());
        // For screen readers: the totals, and why Combine can't run (it is only painted).
        let said = match blocked {
            Some(why) => format!("{files} · {pages}. {why}"),
            None => format!("{files} · {pages}"),
        };
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &said));
        match blocked {
            Some(why) => {
                widgets::row_text(ui, pos2(r.left(), r.center().y - 10.0), format!("{files} · {pages}"), theme::medium(15.0), t.text, width);
                widgets::row_text(ui, pos2(r.left(), r.center().y + 12.0), why, theme::regular(12.5), t.error, width);
            }
            None => widgets::row_text(ui, pos2(r.left(), r.center().y), format!("{files} · {pages}"), theme::medium(15.0), t.text, width),
        }
        ui.add_space(12.0);
        combine = phone::action_pill(ui, Some("files"), label, blocked.is_none(), t).clicked();
    });
    combine
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    /// One page of text, `n` written on it.
    fn pdf(n: usize) -> Vec<u8> {
        let text = format!("BT /F1 40 Tf 60 200 Td (Page {n}) Tj ET");
        format!(
            "%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >> endobj
4 0 obj << /Length {} >> stream
{text}
endstream endobj
5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
trailer << /Root 1 0 R >>
%%EOF",
            text.len()
        )
        .into_bytes()
    }

    fn harness(language: &str, dark: bool, files: usize) -> Harness<'static, PdfCraftApp> {
        let language = language.to_owned();
        let mut h = Harness::builder().with_size(egui::vec2(412.0, 892.0)).with_step_dt(0.05).build_eframe(move |_| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", &language).unwrap();
            app.set_option("shell", "phone").unwrap();
            app.set_option("theme", if dark { "dark" } else { "light" }).unwrap();
            app.open_combine_tab();
            app.stage_combine((1..=files).map(|n| (format!("{}.pdf", ["Lease", "Invoice", "Report"][(n - 1) % 3]), pdf(n))).collect());
            app
        });
        h.run_steps(4);
        h
    }

    fn order(h: &Harness<'static, PdfCraftApp>) -> Vec<String> {
        h.state().combine_draft.iter().map(|f| f.name.clone()).collect()
    }

    #[test]
    fn files_are_moved_taken_out_and_combined_with_taps() {
        let mut h = harness("en", false, 3);
        assert_eq!(order(&h), ["Lease.pdf", "Invoice.pdf", "Report.pdf"]);
        assert!(h.query_by_label("1. Lease.pdf").is_some(), "each card says its place in the order");
        h.get_by_role_and_label(egui::accesskit::Role::Heading, "Combine files");
        for name in ["Move up · Invoice.pdf", "Move down · Invoice.pdf", "Remove Invoice.pdf"] {
            let r = h.get_by_label(name).rect();
            assert!(r.width() >= 48.0 && r.height() >= 48.0, "{name}: {r:?} is a finger's");
        }
        // The first card's Down, the last card's Up.
        h.get_by_label("Move down · Lease.pdf").click();
        h.run_steps(3);
        assert_eq!(order(&h), ["Invoice.pdf", "Lease.pdf", "Report.pdf"]);
        h.get_by_label("Move up · Report.pdf").click();
        h.run_steps(3);
        assert_eq!(order(&h), ["Invoice.pdf", "Report.pdf", "Lease.pdf"]);
        h.get_by_label("Remove Report.pdf").click();
        h.run_steps(3);
        assert_eq!(order(&h), ["Invoice.pdf", "Lease.pdf"]);
        h.get_by_label("Undo").click();
        h.run_steps(3);
        assert_eq!(order(&h), ["Invoice.pdf", "Report.pdf", "Lease.pdf"], "every change can be undone");
        let docs = h.state().views.len();
        h.get_by_label("Combine").click();
        h.run_steps(6);
        assert_eq!(h.state().views.len(), docs + 1, "Combine makes the combined document");
        let combined = h.state().active.and_then(|i| h.state().views.get(i)).map(crate::DocView::page_count);
        assert_eq!(combined, Some(3));
    }

    #[test]
    fn back_leaves_combine_unless_a_file_is_shown_large() {
        let mut h = harness("en", false, 2);
        h.get_by_label("1. Lease.pdf").click();
        h.run_steps(3);
        assert!(h.state().combine_tab.preview.is_some(), "a tap shows the file large");
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        assert!(h.state().combine_showing(), "Back closes what shows over the list first");
        assert!(h.state().combine_tab.preview.is_none());
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        assert!(!h.state().combine_showing());
    }

    #[test]
    fn one_file_cant_be_combined_and_the_bar_says_why() {
        let mut h = harness("en", false, 1);
        assert!(h.query_by_label_contains("Add at least two files").is_some(), "the bar says why");
        h.get_by_label("Combine").click();
        h.run_steps(3);
        assert_eq!(h.state().views.len(), 0);
    }

    /// Pictures of the screen, for a look: `PDFCRAFT_PHONE_SHOTS=dir`.
    #[test]
    fn shots() {
        let Ok(dir) = std::env::var("PDFCRAFT_PHONE_SHOTS") else { return };
        for dark in [false, true] {
            let theme = if dark { "dark" } else { "light" };
            for files in [0, 3] {
                let mut h = harness("fr", dark, files);
                for _ in 0..40 {
                    h.run_steps(1);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                h.render().unwrap().save(format!("{dir}/combine-{files}-{theme}.png")).unwrap();
            }
        }
    }
}
