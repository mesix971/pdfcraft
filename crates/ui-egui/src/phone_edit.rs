//! Editing a PDF on a phone: the Edit mode's bars, and a screen of its own to type in.
//!
//! The desktop edits a paragraph in place, at the page's size, beside a Format panel. On a phone
//! that text is tiny, the panel covers the page and the keyboard the rest. So in the Edit mode a
//! finger scrolls the pages and a tap picks: a paragraph (or the place new text goes) opens this
//! screen, the text large with the keyboard under it, a row of the formats that matter (size,
//! bold, italic, underline, alignment, colour), and Done or ✕; a picture gets a bar of what can be
//! done to it. The edits are the desktop editor's, so undo, saving and the rest are the same.

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, Vec2, pos2, vec2};
use pdfcraft_engine::{AddedContent, Edit, ImageEdit, TextAlign, catalog};

use crate::phone::{self, Sheet, TARGET, TOP_BAR};
use crate::phone_home;
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, QuickTool, icons, widgets};

/// The colours offered for text, with their names.
const COLOURS: [(&str, [f64; 3]); 8] = [
    ("Black", [0.0, 0.0, 0.0]),
    ("Gray", [0.4, 0.4, 0.42]),
    ("Red", [0.84, 0.16, 0.16]),
    ("Blue", [0.1, 0.36, 0.85]),
    ("Green", [0.1, 0.55, 0.27]),
    ("Orange", [0.95, 0.5, 0.05]),
    ("Purple", [0.5, 0.22, 0.76]),
    ("White", [1.0, 1.0, 1.0]),
];
/// Below this height (a phone on its side, the keyboard up) the formats and colours share a row.
const COMPACT: f32 = 380.0;
/// The sizes text can take here, in points.
const SIZES: std::ops::RangeInclusive<f64> = 6.0..=72.0;
/// The Edit group's actions the Edit mode has buttons for (the rest are under More).
const ON_THE_BAR: [&str; 3] = ["edit.edit_text", "edit.text", "edit.image"];

impl PdfCraftApp {
    /// Text is being typed on the active document (a paragraph, or new text): the phone shows
    /// the screen to type it in.
    pub fn phone_text_editing(&self) -> bool {
        self.active.and_then(|i| self.views.get(i)).is_some_and(|v| v.line_editor.is_some() || v.content.draft.is_some())
    }

    /// Close the typing screen: with what was typed (`apply`) or without it. Text the page's
    /// fonts can't draw stays open rather than being lost (its screen says why). Returns whether
    /// it closed.
    pub(crate) fn finish_phone_text(&mut self, apply: bool) -> bool {
        let Some(index) = self.active else { return false };
        let added = self.views.get(index).and_then(|v| self.session.get(v.id)).map(|d| d.added().to_vec()).unwrap_or_default();
        let Some(view) = self.views.get_mut(index) else { return false };
        if let Some(ed) = view.line_editor.take() {
            let style = ed.style();
            if apply && (ed.has_changes() || style != pdfcraft_engine::BlockStyle::default()) {
                view.pending_edit = Some(Edit::EditTextBlock { page: ed.page, block: ed.block, text: ed.text, style });
            }
        } else if view.content.draft.is_some() {
            if !apply {
                view.content.draft = None;
            } else if view.content.blocked().is_some() {
                return false;
            } else if let Some(edit) = crate::content_ui::finish_draft(view, &added, true) {
                view.pending_edit = Some(edit);
            }
        }
        self.phone.typing_shown = false;
        // Back to changing what is on the page (new text was a one-off).
        if self.phone.tool == Some(phone::ToolMode::Edit) {
            self.quick_tool = QuickTool::EditText;
        }
        true
    }
}

/// What the typing screen edits: a paragraph of the page, or text PdfCraft adds.
struct Typing<'a> {
    text: &'a mut String,
    look: &'a mut pdfcraft_engine::AddedText,
    /// A paragraph's underline (added text has none).
    underline: Option<&'a mut bool>,
    new: bool,
}

/// The typing screen over the reader. `screen` is the content area (the keyboard's room is
/// already taken out of it).
pub(crate) fn screen(app: &mut PdfCraftApp, index: usize, ui: &mut egui::Ui, screen: Rect) {
    let t = Tokens::get(ui.ctx());
    let look = phone_home::look(&t);
    ui.painter().rect_filled(screen, CornerRadius::ZERO, look.ground);
    let first = !std::mem::replace(&mut app.phone.typing_shown, true);
    let refocus = std::mem::take(&mut app.phone.typing_refocus);
    let Some(view) = app.views.get_mut(index) else { return };
    let blocked = view.content.blocked();
    let (mut done, mut cancel, mut formatted) = (false, false, false);
    {
        let typing = if let Some(ed) = view.line_editor.as_mut() {
            Typing { text: &mut ed.text, look: &mut ed.look, underline: Some(&mut ed.extras.underline), new: false }
        } else if let Some(d) = view.content.draft.as_mut() {
            Typing { text: &mut d.text, look: &mut d.style, underline: None, new: d.index.is_none() }
        } else {
            return;
        };
        let Typing { text, look: style, mut underline, new } = typing;

        // Top: ✕, what this is, Done.
        let top = Rect::from_min_size(screen.min, vec2(screen.width(), TOP_BAR));
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(top.shrink2(vec2(4.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                cancel = icons::button(ui, "x", TARGET, false, tl!("Cancel")).clicked();
                let done_label = tl!("Done");
                let pill = widgets::text_width(ui, done_label, &theme::semibold(15.0)) + 62.0;
                let title = if new { tl!("Add text") } else { tl!("Edit text") };
                phone::title_text(ui, title, (ui.available_width() - pill - 8.0).max(0.0), &t);
                done = phone::action_pill(ui, Some("check"), done_label, blocked.is_none(), &t).clicked();
            },
        );

        // The formats: the size, bold, italic, underline and alignment on a row that swipes
        // sideways, the colours on the next (on the same row when the screen is short, so the
        // text keeps its room).
        let compact = screen.height() < COMPACT;
        let formats = Rect::from_min_size(pos2(screen.left(), top.bottom()), vec2(screen.width(), 56.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(formats), |ui| {
            phone_home::swipe_row(ui, "phone-typing-formats", |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(12.0);
                    ui.spacing_mut().item_spacing.x = 6.0;
                    formatted |= format_row(ui, style, underline.take(), &t, &look);
                    if compact {
                        divider(ui, &look);
                        formatted |= colour_row(ui, style, &t, &look, TARGET);
                    }
                    ui.add_space(12.0);
                });
            });
        });
        let mut below = formats.bottom();
        if !compact {
            let colours = Rect::from_min_size(pos2(screen.left(), formats.bottom()), vec2(screen.width(), 48.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(colours), |ui| {
                phone_home::swipe_row(ui, "phone-typing-colours", |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.add_space(10.0);
                        ui.spacing_mut().item_spacing.x = 2.0;
                        // A finger wide each where the screen has room for every colour; never
                        // narrower than 40 (the row swipes for the rest).
                        let room = colours.width() - 20.0 - 2.0 * (COLOURS.len() as f32 - 1.0);
                        formatted |= colour_row(ui, style, &t, &look, (room / COLOURS.len() as f32).clamp(40.0, TARGET));
                        ui.add_space(10.0);
                    });
                });
            });
            below = colours.bottom();
        }

        // The text, large, filling what is left above the keyboard.
        below += 4.0;
        if let Some(c) = blocked {
            let warning = Rect::from_min_size(pos2(screen.left() + 16.0, below), vec2(screen.width() - 32.0, 40.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(warning), |ui| {
                let message = crate::content_ui::undrawable_message(c);
                ui.add(egui::Label::new(egui::RichText::new(message).font(theme::regular(13.5)).color(t.error)).wrap());
            });
            below += 44.0;
        }
        let field = Rect::from_min_max(pos2(screen.left() + 12.0, below), pos2(screen.right() - 12.0, screen.bottom() - 12.0));
        if field.height() > 20.0 {
            ui.scope_builder(egui::UiBuilder::new().max_rect(field), |ui| {
                egui::ScrollArea::vertical().id_salt("phone-typing-text").auto_shrink([false, false]).show(ui, |ui| {
                    let edit = egui::TextEdit::multiline(text)
                        .id(egui::Id::new("phone-typing-input"))
                        .font(theme::regular(18.0))
                        .text_color(t.text)
                        .hint_text(tl!("Type text"))
                        .margin(egui::Margin::same(14))
                        .background_color(look.surface)
                        .desired_width(field.width())
                        .desired_rows(((field.height() - 28.0) / 24.0).floor().clamp(3.0, 40.0) as usize)
                        .lock_focus(true);
                    let r = widgets::named_edit(ui, edit, tl!("Text"));
                    if first || refocus {
                        r.request_focus();
                    }
                });
            });
        }
    }
    // A tap on a format took the keyboard from the text: it goes back next frame, so typing
    // carries on.
    if formatted {
        app.phone.typing_refocus = true;
        ui.ctx().request_repaint();
    }
    if cancel {
        app.finish_phone_text(false);
    } else if done {
        app.finish_phone_text(true);
    }
}

/// Size, bold, italic, underline and alignment, for `look`. Returns whether one changed.
fn format_row(ui: &mut egui::Ui, look: &mut pdfcraft_engine::AddedText, underline: Option<&mut bool>, t: &Tokens, l: &phone_home::Look) -> bool {
    let before = (look.size, look.bold, look.italic, look.align, underline.as_deref().copied());
    let mut underlined = underline.as_deref().copied();
    // Size: smaller, the size, larger.
    let step = |size: f64| if size < 12.0 { 1.0 } else { 2.0 };
    if format_button(ui, "minus", tl!("Smaller text"), None, look.size > *SIZES.start(), t, l) {
        look.size = (look.size - step(look.size - 1.0)).clamp(*SIZES.start(), *SIZES.end()).round();
    }
    let size = format!("{} pt", look.size.round());
    let (r, said) = ui.allocate_exact_size(vec2(widgets::text_width(ui, &size, &theme::semibold(14.0)) + 8.0, TARGET), Sense::hover());
    said.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &size));
    ui.painter().text(r.center(), Align2::CENTER_CENTER, &size, theme::semibold(14.0), t.text);
    if format_button(ui, "plus", tl!("Larger text"), None, look.size < *SIZES.end(), t, l) {
        look.size = (look.size + step(look.size)).clamp(*SIZES.start(), *SIZES.end()).round();
    }
    divider(ui, l);
    if format_button(ui, "bold", tl!("Bold"), Some(look.bold), true, t, l) {
        look.bold = !look.bold;
    }
    if format_button(ui, "italic", tl!("Italic"), Some(look.italic), true, t, l) {
        look.italic = !look.italic;
    }
    if let Some(underline) = underline
        && format_button(ui, "underline", tl!("Underline"), Some(*underline), true, t, l)
    {
        *underline = !*underline;
        underlined = Some(*underline);
    }
    // Alignment: one button showing the paragraph's, each tap the next. Named by the alignment
    // a tap gives.
    let (icon, next, says) = match look.align {
        TextAlign::Left => ("align-left", TextAlign::Center, tl!("Centre")),
        TextAlign::Center => ("align-center", TextAlign::Right, tl!("Align right")),
        TextAlign::Right => ("align-right", TextAlign::Justify, tl!("Justify")),
        TextAlign::Justify => ("align-justify", TextAlign::Left, tl!("Align left")),
    };
    if format_button(ui, icon, says, None, true, t, l) {
        look.align = next;
    }
    before != (look.size, look.bold, look.italic, look.align, underlined)
}

/// The colours text can take, the one it has ringed, each `width` wide and a finger tall.
/// Returns whether one was chosen.
fn colour_row(ui: &mut egui::Ui, look: &mut pdfcraft_engine::AddedText, t: &Tokens, l: &phone_home::Look, width: f32) -> bool {
    let mut chosen = false;
    for (name, c) in COLOURS {
        let on = look.color.iter().zip(c).all(|(a, b)| (a - b).abs() < 0.02);
        let (r, resp) = ui.allocate_exact_size(vec2(width, TARGET), Sense::click());
        let name = tl!(name);
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, on, name));
        let fill = Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8);
        ui.painter().circle_filled(r.center(), 14.0, fill);
        ui.painter().circle_stroke(r.center(), 14.0, Stroke::new(1.0, l.edge));
        if on {
            ui.painter().circle_stroke(r.center(), 18.0, Stroke::new(2.5, t.accent));
        }
        if theme::hand(resp).clicked() {
            look.color = c;
            chosen = true;
        }
    }
    chosen
}

/// A format's button: its icon on a chip, the accent's when it is on. `on` is `None` for a button
/// that isn't on or off (the size, the alignment). Returns whether tapped.
fn format_button(ui: &mut egui::Ui, icon: &str, label: &str, on: Option<bool>, enabled: bool, t: &Tokens, l: &phone_home::Look) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(TARGET), if enabled { Sense::click() } else { Sense::hover() });
    match on {
        Some(on) => resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, on, label)),
        None => resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label)),
    }
    let on = on == Some(true);
    let press = theme::Press::track(ui, &resp);
    let chip = rect.shrink(4.0).translate(press.offset());
    if on {
        phone::vivid_tile(ui.painter(), chip, 12.0, t.accent);
        icons::paint_bold(ui, chip, icon, 19.0, Color32::WHITE);
    } else {
        ui.painter().rect(
            chip,
            CornerRadius::same(12),
            press.fill(l.surface, t.hover, t.pressed),
            Stroke::new(1.0, l.edge),
            egui::StrokeKind::Inside,
        );
        icons::paint_bold(ui, chip, icon, 19.0, if enabled { t.text } else { t.text_faint });
    }
    enabled && theme::hand(resp).clicked()
}

fn divider(ui: &mut egui::Ui, l: &phone_home::Look) {
    let (r, _) = ui.allocate_exact_size(vec2(9.0, 28.0), Sense::hover());
    ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, l.edge));
}

/// How long a finger stays down, without moving (as far as a tap may), to pick up what is under
/// it.
const HOLD: f64 = 0.45;
/// A corner handle of what is picked: its dot's radius, and the finger-sized room around it.
const HANDLE_DOT: f32 = 9.0;
const HANDLE_TARGET: f32 = TARGET;

/// A corner handle on the page (the phone's Edit): what a finger on it resizes.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Handle {
    /// A picture of the page, by its index, from the corner opposite (which stays put).
    Picture { index: usize, opposite: egui::Pos2 },
    /// An item PdfCraft added, by its place among the page's items, from its corner (`left`,
    /// `top` say which).
    Added { index: usize, left: bool, top: bool },
}

/// Draw the handles at the corners of what is picked on `page` (a picture, or an item PdfCraft
/// added), finger-sized, and remember them for the next frame: a finger that goes down on one
/// resizes it from that frame on (so the pages don't scroll under it: see `canvas.rs`).
pub(crate) fn handles(
    ui: &egui::Ui,
    xf: &crate::canvas::PageXform,
    page: usize,
    info: &pdfcraft_render::DocInfo,
    added: &[pdfcraft_engine::Added],
    generation: u64,
    view: &mut crate::canvas::DocView,
) {
    if view.carrying() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let corners = |r: Rect| [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom()];
    let mut found: Vec<(Rect, Handle)> = Vec::new();
    if let Some(s) = view.image_selection.as_ref().filter(|s| s.page == page)
        && let Some(r) = view
            .edit_images
            .get(&page)
            .filter(|(g, _)| *g == generation)
            .and_then(|(_, images)| images.get(s.index))
            .and_then(|im| im.visible_rect)
            .map(|r| xf.user_rect(info, page, r.map(|v| v as f32)))
            .filter(|r| r.is_positive())
    {
        // A small picture's handles shrink with it, so its middle stays its own (a long press there
        // moves it).
        let target = (r.width().min(r.height()) / 2.0).clamp(16.0, HANDLE_TARGET);
        let c = corners(r);
        for (k, at) in c.iter().enumerate() {
            let opposite = c.get(3 - k).copied().unwrap_or(*at);
            found.push((Rect::from_center_size(*at, Vec2::splat(target)), Handle::Picture { index: s.index, opposite }));
        }
    } else if let Some((p, index)) = view.content.selected.filter(|(p, _)| *p == page)
        && let Some(r) = crate::content_ui::item_screen_rect(xf, info, p, added, index).filter(|r| r.is_positive())
    {
        // Added text (or a picture) is resized by its sides: its text rewraps to the width, and a
        // line of text is too short for handles at its top and bottom.
        for (at, left) in [(r.left_center(), true), (r.right_center(), false)] {
            found.push((Rect::from_center_size(at, Vec2::splat(HANDLE_TARGET)), Handle::Added { index, left, top: true }));
        }
    }
    for (r, _) in &found {
        let c = r.center();
        ui.painter().circle_filled(c, HANDLE_DOT + 2.0, Color32::from_black_alpha(40));
        ui.painter().circle(c, HANDLE_DOT, Color32::WHITE, Stroke::new(2.5, t.accent));
    }
    view.phone_handles_next.extend(found.into_iter().map(|(r, h)| (page, r, h)));
}

/// A long press on `page` in the Edit mode picks up what is under the finger: text PdfCraft added,
/// else a paragraph, else a picture. It then follows the finger, and lands where the finger lifts
/// (the moves the desktop's drags make). A tap still opens it, and a finger that moves at once
/// scrolls. `generation` is the document's, which the paragraphs listed belong to.
#[allow(clippy::too_many_arguments)]
pub(crate) fn long_press(
    ui: &egui::Ui,
    resp: &egui::Response,
    xf: &crate::canvas::PageXform,
    page: usize,
    info: &pdfcraft_render::DocInfo,
    added: &[pdfcraft_engine::Added],
    generation: u64,
    view: &mut crate::canvas::DocView,
) {
    // A second finger (a pinch) ends what one was carrying, and never picks anything up.
    if ui.input(|i| i.multi_touch().is_some()) {
        view.block_drag = None;
        view.content.cancel_grab();
        if let Some(s) = view.image_selection.as_mut() {
            s.cancel_drag();
        }
        return;
    }
    // Pressed on the page itself, not on a bar or a sheet over it.
    if view.carrying() || !resp.is_pointer_button_down_on() {
        return;
    }
    let still = ui.ctx().options(|o| o.input_options.max_click_dist);
    let (origin, latest, since, now) = ui.input(|i| (i.pointer.press_origin(), i.pointer.latest_pos(), i.pointer.press_start_time(), i.time));
    let (Some(origin), Some(since)) = (origin, since) else { return };
    // A finger on a handle of what is picked resizes it at once, no long press needed.
    if view.carried_press != Some(since)
        && let Some(handle) = handle_at(&view.phone_handles, page, origin)
    {
        view.carried_press = Some(since);
        match handle {
            Handle::Picture { index, opposite } => {
                view.image_selection = Some(crate::edit_text_ui::ImageSelection::resizing(page, index, origin, opposite));
            }
            Handle::Added { index, left, top } => crate::content_ui::grab_corner(view, page, index, left, top, origin),
        }
        return;
    }
    // A finger that moved further than a tap may is scrolling.
    if !xf.rect.contains(origin) || latest.is_some_and(|p| p.distance(origin) > still) {
        return;
    }
    if view.carried_press == Some(since) {
        return;
    }
    let held = now - since;
    if held < HOLD {
        // No input comes while a finger rests: wake up when the press becomes a long one.
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((HOLD - held).max(0.0) + 0.01));
        return;
    }
    // This press is spent, whatever it finds.
    view.carried_press = Some(since);
    if let Some(i) = crate::content_ui::item_at(xf, info, page, added, origin) {
        crate::content_ui::carry(view, page, i, origin);
        return;
    }
    let paragraph = view
        .edit_lines
        .get(&page)
        .filter(|(g, _)| *g == generation)
        .and_then(|(_, lines)| lines.iter().position(|l| xf.user_rect(info, page, l.rect.map(|v| v as f32)).expand(2.0).contains(origin)));
    if let Some(block) = paragraph {
        view.image_selection = None;
        view.content.selected = None;
        view.block_drag = Some(crate::edit_text_ui::BlockDrag::carried(page, block, generation, origin));
        return;
    }
    // By its shape, as a tap picks it (a turned or clipped picture isn't its box).
    let (x, y) = xf.screen_to_view(origin);
    let user = info.pages.get(page).map(|p| p.view_to_user(x, y).map(f64::from));
    let picture = view.edit_images.get(&page).filter(|(g, _)| *g == generation).and_then(|(_, images)| {
        images.iter().rposition(|im| {
            im.visible_rect.is_some_and(|r| xf.user_rect(info, page, r.map(|v| v as f32)).contains(origin)) && user.is_some_and(|u| im.contains(u))
        })
    });
    if let Some(index) = picture {
        view.content.selected = None;
        view.image_selection = Some(crate::edit_text_ui::ImageSelection::carried(page, index, origin));
    }
}

/// The handle on `page` under a finger at `p`: the nearest, where a small item's handles overlap.
fn handle_at(handles: &[(usize, Rect, Handle)], page: usize, p: egui::Pos2) -> Option<Handle> {
    handles
        .iter()
        .filter(|(at, r, _)| *at == page && r.contains(p))
        .min_by(|(_, a, _), (_, b, _)| a.center().distance_sq(p).total_cmp(&b.center().distance_sq(p)))
        .map(|(_, _, h)| *h)
}

/// What the Edit mode's bottom bar asks for.
enum Pick {
    Tool(QuickTool),
    Command(&'static str),
    More,
    Picture(ImageEdit),
    Replace,
    SavePicture,
    Retype,
    DeleteAdded,
    Deselect,
}

/// The Edit mode's bottom bar: its tools, or what can be done to the picture or added item
/// selected.
pub(crate) fn bottom_bar(app: &mut PdfCraftApp, index: usize, ui: &mut egui::Ui, rect: Rect, t: &Tokens) {
    let Some(view) = app.views.get(index) else { return };
    let picture = view.image_selection.as_ref().map(|s| (s.page, s.index));
    let raster = picture.is_some_and(|(page, i)| view.edit_images.get(&page).and_then(|(_, images)| images.get(i)).is_some_and(|im| !im.is_form));
    let added = view.content.selected;
    let added_text = added
        .and_then(|(page, i)| app.session.get(view.id).and_then(|d| d.added().iter().filter(|a| a.page == page).nth(i).cloned()))
        .is_some_and(|a| matches!(a.content, AddedContent::Text(_)));
    let tool = app.quick_tool;
    let mut pick = None;
    phone::bar(ui, "phone-edit-bottom-bar", rect, t, false, |ui| {
        // (icon, label, colour, on, enabled, what)
        let items: Vec<(&str, &str, Color32, bool, bool, Pick)> = if picture.is_some() {
            vec![
                ("rotate-cw", tl!("Rotate"), Color32::from_rgb(0x3A, 0x6F, 0xE8), false, true, Pick::Picture(ImageEdit::Rotate(1))),
                (
                    "flip-horizontal-2",
                    tl!("Flip"),
                    Color32::from_rgb(0x8B, 0x5C, 0xF6),
                    false,
                    true,
                    Pick::Picture(ImageEdit::Flip { horizontal: true }),
                ),
                ("replace", tl!("Replace"), Color32::from_rgb(0xF2, 0x9F, 0x05), false, raster, Pick::Replace),
                ("save", tl!("Save"), Color32::from_rgb(0x16, 0xA3, 0x4A), false, raster, Pick::SavePicture),
                ("trash-2", tl!("Delete"), Color32::from_rgb(0xE5, 0x48, 0x4D), false, true, Pick::Picture(ImageEdit::Delete)),
                ("x", tl!("Close"), t.text_muted, false, true, Pick::Deselect),
            ]
        } else if added.is_some() {
            let mut items = Vec::new();
            if added_text {
                items.push(("pencil", tl!("Edit text"), phone::tool_hue(t, "edit"), false, true, Pick::Retype));
            }
            items.push(("trash-2", tl!("Delete"), Color32::from_rgb(0xE5, 0x48, 0x4D), false, true, Pick::DeleteAdded));
            items.push(("x", tl!("Close"), t.text_muted, false, true, Pick::Deselect));
            items
        } else {
            vec![
                (
                    "text-cursor-input",
                    tl!("Edit text"),
                    phone::tool_hue(t, "edit"),
                    tool == QuickTool::EditText,
                    true,
                    Pick::Tool(QuickTool::EditText),
                ),
                ("type", tl!("Add text"), Color32::from_rgb(0x3A, 0x6F, 0xE8), tool == QuickTool::AddText, true, Pick::Tool(QuickTool::AddText)),
                ("image-plus", tl!("Image"), Color32::from_rgb(0x16, 0xA3, 0x4A), false, true, Pick::Command("edit.image")),
                ("ellipsis", tl!("More"), t.text_muted, false, true, Pick::More),
            ]
        };
        let width = (ui.available_width() / items.len().max(1) as f32).floor();
        let labels: Vec<&str> = items.iter().map(|i| i.1).collect();
        let size = phone::bar_label_size(ui, &labels, width - 6.0);
        for (icon, label, hue, on, enabled, what) in items {
            let r = ui.add_enabled_ui(enabled, |ui| phone::bar_item(ui, icon, label, width, size, hue, on, t)).inner;
            if r.clicked() {
                pick = Some(what);
            }
        }
    });
    let Some(pick) = pick else { return };
    let Some(view) = app.views.get_mut(index) else { return };
    match pick {
        Pick::Tool(tool) => {
            app.quick_tool = tool;
            if tool == QuickTool::AddText {
                app.notify_tr("Tap where the text goes");
            }
        }
        Pick::Command(id) => {
            app.execute(id);
        }
        Pick::More => app.phone.sheet = Some(Sheet::EditMore),
        Pick::Picture(change) => {
            if let Some((page, i)) = picture {
                if change == ImageEdit::Delete {
                    view.image_selection = None;
                }
                view.pending_edit = Some(Edit::EditPageImage { page, index: i, change });
            }
        }
        Pick::Replace => {
            if let Some((page, i)) = picture {
                app.replace_page_image_dialog(page, i);
            }
        }
        Pick::SavePicture => {
            if let Some((page, i)) = picture {
                app.save_page_image(page, i);
            }
        }
        Pick::Retype => crate::content_ui::retype_selected(app, index),
        Pick::DeleteAdded => {
            if let Some((page, i)) = view.content.selected.take() {
                view.pending_edit = Some(Edit::DeleteContent { page, index: i });
            }
        }
        Pick::Deselect => {
            view.content.selected = None;
            view.image_selection = None;
        }
    }
}

/// More: the rest of Edit a PDF (headers and footers, a watermark, the background, Bates
/// numbers, and the pages).
pub(crate) fn more_sheet(app: &mut PdfCraftApp, ctx: &egui::Context, screen: Rect, t: &Tokens) {
    let Some(g) = catalog::group("edit") else {
        app.phone.sheet = None;
        return;
    };
    let mut pick: Option<&'static str> = None;
    let open = phone::sheet(ctx, "edit-more", screen, Some(tl!("Edit a PDF")), |ui| {
        for s in g.sections.iter().rev() {
            for item in s.items {
                // On the bar already; links are drawn with a mouse; the rest only when it works.
                if ON_THE_BAR.contains(&item.command) || item.command == "edit.link" || item.availability != catalog::Availability::Ready {
                    continue;
                }
                if phone::sheet_row(ui, item.icon, tl!(item.label), true, None, t).clicked() {
                    pick = Some(item.command);
                }
            }
        }
    });
    if !open || pick.is_some() {
        app.phone.sheet = None;
    }
    match pick {
        // The page grid is a screen of its own: the Edit mode ends.
        Some("page.organize") => {
            app.leave_phone_tool();
            app.open_phone_tool_group("organize");
        }
        Some(command) => {
            app.execute(command);
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finger_takes_the_nearest_of_overlapping_handles() {
        // A 20-point picture: its corners' handles overlap over all of it.
        let corners = [pos2(100.0, 100.0), pos2(120.0, 100.0), pos2(100.0, 120.0), pos2(120.0, 120.0)];
        let handles: Vec<(usize, Rect, Handle)> = corners
            .iter()
            .enumerate()
            .map(|(k, c)| (0, Rect::from_center_size(*c, Vec2::splat(44.0)), Handle::Added { index: k, left: k % 2 == 0, top: k < 2 }))
            .collect();
        let index = |p| match handle_at(&handles, 0, p) {
            Some(Handle::Added { index, .. }) => Some(index),
            _ => None,
        };
        assert_eq!(index(pos2(118.0, 119.0)), Some(3), "the bottom right, not the first in the list");
        assert_eq!(index(pos2(101.0, 102.0)), Some(0));
        assert_eq!(index(pos2(119.0, 101.0)), Some(1));
        assert_eq!(index(pos2(300.0, 300.0)), None, "nowhere near");
        assert_eq!(handle_at(&handles, 1, pos2(118.0, 119.0)).map(|_| ()), None, "another page's");
    }
}
