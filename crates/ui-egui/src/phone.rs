//! The phone layout: PdfCraft's own shell for phone screens, around the same page view, engine
//! and dialogs as the desktop shell.
//!
//! Its ergonomics follow what phone PDF readers settled on (Acrobat's mobile app among them,
//! observed through its public help pages only, never its code or artwork): the page fills the
//! screen and a tap hides or shows the bars; the top bar has Back, the document's name, search and
//! a menu; the bottom bar holds the few tools used most and the rest are a sheet away; a tool is a
//! mode with Undo, Redo and Done. Done sits on the right, away from Back, so finishing a tool can't
//! close the document by a second tap. The look is PdfCraft's: Lucide icons and the theme's tokens.
//!
//! Touch targets are 48 points (Material's minimum), and the shell draws at 100%: a phone's
//! points are its density-independent pixels, so text and targets are the sizes other apps use.

use egui::{Align2, Color32, CornerRadius, Key, Rect, Sense, Stroke, Vec2, pos2, vec2};
use pdfcraft_engine::catalog;

use crate::comments::{self, CommentTool};
use crate::fill_sign::FillTool;
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, QuickTool, RightPanel, icons, widgets};

/// Material's smallest touch target.
pub(crate) const TARGET: f32 = 48.0;
pub(crate) const TOP_BAR: f32 = 56.0;
const BOTTOM_BAR: f32 = 64.0;
/// Room above the first page and below the last, so the bars floating there don't cover them.
pub(crate) const PAGE_PAD: (f32, f32) = (TOP_BAR, BOTTOM_BAR);

/// Which shell draws the window. A phone's screen always gets the phone shell: it has every
/// tool, so there is no desktop layout to fall back to there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellPref {
    /// The phone shell on a phone's screen, the desktop shell elsewhere.
    #[default]
    Auto,
    /// The phone shell on any screen (previews, tests, a narrow desktop window).
    Phone,
}

impl ShellPref {
    /// `auto` or `phone`, in any case.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "phone" => Some(Self::Phone),
            _ => None,
        }
    }
}

/// A sheet that slides up from the bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sheet {
    /// The reader's ⋮ menu.
    Menu,
    /// Every tool group.
    Tools,
    /// Go to a page by number.
    GoTo,
    /// The current comment tool's colour (and thickness).
    Style,
    /// Home's "+": open, create, combine, and the actions started most.
    New,
    /// A file's ⋮ menu on Home.
    Item,
    /// Which document a Home action is for.
    Pick,
}

impl Sheet {
    /// Home's sheets (the others are the reader's).
    fn on_home(self) -> bool {
        matches!(self, Self::New | Self::Item | Self::Pick)
    }
}

/// A tool mode: one of the bottom bar's tool groups, with its tools in the bottom bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolMode {
    /// Highlight, underline and strike out text, add notes and text boxes.
    Comment,
    Draw,
    FillSign,
}

impl ToolMode {
    pub const ALL: [Self; 3] = [Self::Comment, Self::Draw, Self::FillSign];

    fn label(self) -> &'static str {
        match self {
            Self::Comment => "Comments",
            Self::Draw => "Draw",
            Self::FillSign => "Fill & Sign",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Comment => "message-square-plus",
            Self::Draw => "pencil",
            Self::FillSign => "signature",
        }
    }

    /// Where the mode's colour comes from, for [`tool_hue`]: its tool group in the catalog.
    fn hue_id(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Draw => "draw",
            Self::FillSign => "fill_sign",
        }
    }

    /// The comment tools of this mode (none for Fill & Sign). Highlighting comes first: it is
    /// what a phone reader marks text with most.
    fn comment_tools(self) -> &'static [CommentTool] {
        match self {
            Self::Comment => &[
                CommentTool::Highlight,
                CommentTool::Underline,
                CommentTool::StrikeOut,
                CommentTool::Note,
                CommentTool::TextBox,
                CommentTool::Callout,
            ],
            Self::Draw => &[CommentTool::Ink, CommentTool::Line, CommentTool::Arrow, CommentTool::Rectangle, CommentTool::Oval, CommentTool::Eraser],
            Self::FillSign => &[],
        }
    }

    /// Whether `tool` belongs to this mode.
    fn owns(self, tool: QuickTool) -> bool {
        match (self, tool) {
            (Self::FillSign, QuickTool::Fill(_)) => true,
            (_, QuickTool::Comment(t)) => self.comment_tools().contains(&t),
            _ => false,
        }
    }

    /// The command that picks this mode's first tool: the one last used in the quick-bar group of
    /// the mode's first tool, if it is one of the mode's, or else that first tool.
    fn first_command(self, app: &PdfCraftApp) -> &'static str {
        match self {
            Self::FillSign => FillTool::Text.command(),
            _ => {
                let tools = self.comment_tools();
                let last = tools.first().and_then(|t| app.comment_prefs.group_tool.get(t.group()).copied()).filter(|t| tools.contains(t));
                last.or(tools.first().copied()).map_or("comment.highlight", CommentTool::command)
            }
        }
    }
}

/// The phone shell's own state.
#[derive(Default)]
pub struct PhoneState {
    /// The reader's bars are hidden (a tap on the page shows them again).
    pub bars_hidden: bool,
    pub sheet: Option<Sheet>,
    /// The search field replaces the top bar.
    pub searching: bool,
    /// The tool mode, if one is on.
    pub tool: Option<ToolMode>,
    /// The tool drawer is open under the page: a tool group's panel (`app.left`), whose tools
    /// work on the page above it.
    pub drawer: bool,
    /// The AI assistant's screen is open over the reader.
    pub ai_open: bool,
    /// "Keep tool selected" before the tool mode turned it on (a mode keeps its tool until Done).
    pinned_before: bool,
    /// The Go to page sheet's field.
    page_text: String,
    /// The document the reader showed last frame: another one starts with the bars shown and
    /// no panel over it.
    doc: Option<pdfcraft_engine::DocId>,
    /// A text field had the keyboard at the end of last frame. egui takes the focus away as soon
    /// as Escape arrives, so this is how Back knows the Escape was the field's.
    text_focused: bool,
    /// Home: its tab, lists, profile and previews.
    pub(crate) home: crate::phone_home::HomeState,
}

impl PdfCraftApp {
    /// Whether the phone shell draws the window.
    pub fn phone_layout(&self) -> bool {
        self.phone_screen || self.shell == ShellPref::Phone
    }

    /// Whether the tool drawer is open (tests and automation).
    pub fn phone_drawer(&self) -> bool {
        self.phone.drawer
    }

    /// Open tool group `id`'s panel in the drawer under the page.
    pub fn open_phone_drawer(&mut self, id: &'static str) {
        self.leave_phone_tool();
        self.phone.searching = false;
        self.phone.bars_hidden = false;
        self.left = crate::LeftPanel::Tool(id);
        self.left_open = true;
        self.phone.drawer = true;
        if let Some(v) = self.active.and_then(|i| self.views.get_mut(i)) {
            v.organize = false;
        }
    }

    /// Close the drawer (Done or Back): back to reading.
    pub fn close_phone_drawer(&mut self) {
        self.phone.drawer = false;
        self.left_open = false;
        self.form_preview = false;
        self.quick_tool = QuickTool::Select;
    }

    /// The interface size `Auto` draws at: the system's text scale, except in the phone shell,
    /// which is laid out in a phone's own points.
    pub(crate) fn auto_text_scale(&self) -> f32 {
        if self.phone_layout() { 1.0 } else { self.system_text_scale }
    }

    /// The phone shell's tool mode (tests and automation).
    pub fn phone_tool(&self) -> Option<ToolMode> {
        self.phone.tool
    }

    /// The open sheet (tests and automation).
    pub fn phone_sheet(&self) -> Option<Sheet> {
        self.phone.sheet
    }

    /// Whether the reader's bars are showing (tests and automation).
    pub fn phone_bars_shown(&self) -> bool {
        !self.phone.bars_hidden
    }

    /// Open tool group id on the active document: its tool mode (Comments, Fill & Sign), the page
    /// grid (Organize), or its panel in the drawer, its tools on the page above.
    pub(crate) fn open_phone_tool_group(&mut self, id: &'static str) {
        match id {
            "comment" => self.enter_phone_tool(ToolMode::Comment),
            "fill_sign" => self.enter_phone_tool(ToolMode::FillSign),
            "organize" => {
                self.leave_phone_tool();
                if let Some(v) = self.active.and_then(|i| self.views.get_mut(i)) {
                    v.organize = true;
                }
            }
            other => self.open_phone_drawer(other),
        }
    }

    /// Turn a tool mode on: its first tool, kept selected until Done.
    pub fn enter_phone_tool(&mut self, mode: ToolMode) {
        if self.phone.drawer {
            self.close_phone_drawer();
        }
        if self.phone.tool.is_none() {
            self.phone.pinned_before = self.comment_prefs.pinned;
        }
        self.comment_prefs.pinned = true;
        self.phone.tool = Some(mode);
        self.phone.sheet = None;
        self.phone.searching = false;
        self.phone.bars_hidden = false;
        let command = mode.first_command(self);
        self.execute(command);
    }

    /// Leave the tool mode (Done or Back): back to reading.
    pub fn leave_phone_tool(&mut self) {
        if self.phone.tool.take().is_some() {
            self.comment_prefs.pinned = self.phone.pinned_before;
        }
        self.phone.sheet = None;
        self.quick_tool = QuickTool::Select;
    }

    /// The reader's Back: close what is open over the page, one thing at a time, then go Home.
    /// Home itself stays (Back never closes PdfCraft). Returns whether it closed something.
    fn phone_back(&mut self) -> bool {
        let organizing = self.active.and_then(|i| self.views.get(i)).is_some_and(|v| v.organize);
        if self.phone.sheet.is_some() {
            self.phone.sheet = None;
        } else if self.active.is_none() && self.phone.home.back() {
            // Home's profile, settings, search or another tab. The assistant's settings, when they
            // were what closed, close for the assistant too (they'd show with a stale key draft).
            #[cfg(not(target_arch = "wasm32"))]
            if !self.phone.home.ai_settings {
                self.ai.close_settings();
            }
        } else if self.phone.ai_open {
            self.phone_ai_back();
        } else if self.right.is_some() && self.active.is_some() {
            self.right = None;
        } else if self.phone.searching {
            self.stop_phone_search();
        } else if let Some(v) =
            self.active.and_then(|i| self.views.get_mut(i)).filter(|v| v.selected_text().is_some() || v.comments.selected.is_some())
        {
            v.clear_selection();
            v.comments.selected = None;
        } else if self.phone.tool.is_some() {
            self.leave_phone_tool();
        } else if self.phone.drawer {
            self.close_phone_drawer();
        } else if organizing {
            if let Some(v) = self.active.and_then(|i| self.views.get_mut(i)) {
                v.organize = false;
            }
        } else if self.active.is_some() {
            self.go_home();
        } else if self.combine_showing() {
            self.combine_tab.focused = false;
        } else {
            return false;
        }
        true
    }

    /// Leave the AI assistant's screen, its settings with it (left open, they'd show the next
    /// time with a stale key draft).
    fn close_phone_ai(&mut self) {
        self.phone.ai_open = false;
        #[cfg(not(target_arch = "wasm32"))]
        self.ai.close_settings();
    }

    /// Back on the assistant's screen (the key, or its own ‹): its settings first when they were
    /// opened over the conversation (with no key, they are all it shows).
    fn phone_ai_back(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if self.ai.settings_open() && !self.ai.key.trim().is_empty() {
            self.ai.close_settings();
            return;
        }
        self.close_phone_ai();
    }

    /// Back to Home. The document stays open there, with its place kept.
    fn go_home(&mut self) {
        self.leave_phone_tool();
        if self.phone.drawer {
            self.close_phone_drawer();
        }
        self.stop_phone_search();
        self.phone.bars_hidden = false;
        self.close_phone_ai();
        if let Some(v) = self.active.and_then(|i| self.views.get_mut(i)) {
            // A tap still waiting to show or hide the bars belongs to this visit.
            v.tap = None;
        }
        self.active = None;
    }

    fn stop_phone_search(&mut self) {
        self.phone.searching = false;
        if let Some(v) = self.active.and_then(|i| self.views.get_mut(i)) {
            v.find = None;
        }
    }
}

/// Back (Android's back key arrives as Escape) closes one thing at a time. It runs before the
/// shortcuts, and takes the Escape it uses, so one press never closes two things (a selection
/// and then the document). A dialog, the palette, a popup or a text field keeps its own Escape.
pub(crate) fn back_key(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let typing = app.phone.text_focused || ctx.text_edit_focused();
    if !ctx.input(|i| i.key_pressed(Key::Escape)) || typing || app.modal_open() || app.palette_open || egui::Popup::is_any_open(ctx) {
        return;
    }
    if app.phone_back() {
        ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
    }
}

/// Draw the window in the phone shell.
pub fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let t = Tokens::get(&ctx);
    let screen = ctx.content_rect();
    // A tool that ended by itself (a one-shot tool, a refused command) ends its mode too.
    if app.phone.tool.is_some_and(|m| !m.owns(app.quick_tool)) {
        app.leave_phone_tool();
    }
    // Sheets belong to the screen that opened them (Home's "+", or the reader's).
    if app.phone.sheet.is_some_and(|s| s.on_home() != app.active.is_none()) {
        app.phone.sheet = None;
    }
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.pasteboard)).show(ui, |ui| match app.active {
        None if app.combine_showing() => {
            if top_bar_titled(ui, screen, tl!("Combine files")) {
                app.combine_tab.focused = false;
            }
            let rest = Rect::from_min_max(pos2(screen.left(), screen.top() + TOP_BAR), screen.max);
            ui.scope_builder(egui::UiBuilder::new().max_rect(rest), |ui| crate::combine_ui::page(app, ui));
        }
        None => crate::phone_home::show(app, ui, screen, &t),
        Some(index) => reader(app, index, ui, screen, &t),
    });
    // Android draws its status bar over PdfCraft's window (a NativeActivity's takes the whole
    // screen): the strip above the content is ours to fill, in the green its white icons stand
    // out on, whatever the theme.
    let whole = ctx.input(|i| i.viewport_rect());
    if screen.top() > whole.top() {
        let strip = Rect::from_min_max(whole.min, pos2(whole.right(), screen.top()));
        ctx.layer_painter(egui::LayerId::background()).rect_filled(strip, CornerRadius::ZERO, t.accent);
    }
    app.phone.text_focused = ctx.text_edit_focused();
}

/// A top bar with Back and a title (Combine files, panels). Returns whether Back was tapped.
pub(crate) fn top_bar_titled(ui: &mut egui::Ui, screen: Rect, title: &str) -> bool {
    let t = Tokens::get(ui.ctx());
    let rect = Rect::from_min_size(screen.min, vec2(screen.width(), TOP_BAR));
    let mut back = false;
    bar(ui, "phone-titled-bar", rect, &t, true, |ui| {
        back = icons::button(ui, "chevron-left", TARGET, false, tl!("Back")).clicked();
        let width = ui.available_width() - 8.0;
        title_text(ui, title, width, &t);
    });
    back
}

/// A bar's frame at `rect`, drawn over the page (the page doesn't move when bars hide), its
/// contents laid out left to right and centred vertically.
pub(crate) fn bar(ui: &mut egui::Ui, id: &str, rect: Rect, t: &Tokens, top: bool, add: impl FnOnce(&mut egui::Ui)) {
    bar_filled(ui, id, rect, t.chrome, Some(t.divider), top, add);
}

/// [`bar`] in `fill`, with a `divider` line on the side facing the page.
pub(crate) fn bar_filled(
    ui: &mut egui::Ui,
    id: &str,
    rect: Rect,
    fill: Color32,
    divider: Option<Color32>,
    top: bool,
    add: impl FnOnce(&mut egui::Ui),
) {
    egui::Area::new(egui::Id::new(id)).order(egui::Order::Middle).fixed_pos(rect.min).constrain(false).show(ui.ctx(), |ui| {
        let (r, _) = ui.allocate_exact_size(rect.size(), Sense::click());
        ui.painter().rect_filled(r, CornerRadius::ZERO, fill);
        let edge = if top { r.bottom() } else { r.top() };
        if let Some(line) = divider {
            ui.painter().hline(r.x_range(), edge, Stroke::new(1.0, line));
        }
        let inner = r.shrink2(vec2(4.0, 0.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            add(ui);
        });
    });
}

pub(crate) fn title_text(ui: &mut egui::Ui, title: &str, width: f32, t: &Tokens) {
    let (r, _) = ui.allocate_exact_size(vec2(width.max(0.0), TOP_BAR), Sense::hover());
    widgets::row_text(ui, r.left_center() + vec2(8.0, 0.0), crate::bidi::visual(title), theme::semibold(17.0), t.text, r.width() - 8.0);
}

/// The bottom-bar labels' size: Material's 11.5 points, or smaller until every word of every
/// label fits its item (French "Commentaires"), never below 9. Labels then wrap between words.
fn bar_label_size(ui: &egui::Ui, labels: &[&str], room: f32) -> f32 {
    const SIZE: f32 = 11.5;
    let widest = labels
        .iter()
        .flat_map(|l| l.split_whitespace())
        .map(|w| ui.fonts_mut(|f| f.layout_no_wrap(w.to_owned(), theme::medium(SIZE), Color32::WHITE).size().x))
        .fold(0.0, f32::max);
    if widest <= room || widest <= 0.0 { SIZE } else { (SIZE * room / widest).floor().max(9.0) }
}

/// A tool's colour, as the All tools sheet shows its group (`id` in the catalog), made a little
/// darker on a light bar and lighter on a dark one so a thin icon still stands out.
pub(crate) fn tool_hue(t: &Tokens, id: &str) -> Color32 {
    // Draw has no group of its own (its tools are comments): the catalog's blue.
    let rgb = match id {
        "draw" => [0x3A, 0x6F, 0xE8],
        id => catalog::group(id).map_or([0x12, 0xA5, 0x8A], |g| g.hue),
    };
    let hue = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    if t.dark() { hue.lerp_to_gamma(Color32::WHITE, 0.25) } else { hue.lerp_to_gamma(Color32::BLACK, 0.15) }
}

/// A bottom-bar item: an icon above its label (`size` points, up to two lines), a finger wide.
/// The icon is drawn in `hue`.
fn bar_item(ui: &mut egui::Ui, icon: &str, label: &str, width: f32, size: f32, hue: Color32, t: &Tokens) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, BOTTOM_BAR), Sense::click());
    let text = label.to_owned();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &text));
    let press = theme::Press::track(ui, &resp);
    press.wash(ui, rect.shrink2(vec2(3.0, 4.0)), 12, false);
    let body = rect.translate(press.offset());
    let text_color = t.text_muted;
    let mut job = egui::text::LayoutJob::simple(label.to_owned(), theme::medium(size), text_color, (width - 6.0).max(0.0));
    job.wrap.max_rows = 2;
    job.halign = egui::Align::Center;
    let galley = ui.painter().layout_job(job);
    // The icon and the label are centred together.
    let block = 22.0 + 4.0 + galley.size().y;
    let top = body.center().y - block / 2.0;
    icons::paint(ui, Rect::from_center_size(pos2(body.center().x, top + 11.0), vec2(24.0, 24.0)), icon, 22.0, hue);
    ui.painter().galley(pos2(body.center().x, top + 26.0), galley, text_color);
    theme::hand(resp)
}

/// One row of a sheet or list: an icon and a label, a finger tall and the sheet wide.
pub(crate) fn sheet_row(ui: &mut egui::Ui, icon: &str, label: &str, enabled: bool, checked: Option<bool>, t: &Tokens) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), if enabled { Sense::click() } else { Sense::hover() });
    let text = label.to_owned();
    match checked {
        Some(on) => resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, on, &text)),
        None => resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, &text)),
    }
    let press = theme::Press::track(ui, &resp);
    if enabled {
        press.wash(ui, rect, 10, false);
    }
    let body = rect.translate(press.offset());
    let color = if enabled { t.icon } else { t.text_faint };
    icons::paint(ui, Rect::from_center_size(body.left_center() + vec2(26.0, 0.0), vec2(22.0, 22.0)), icon, 20.0, color);
    let reserve = if checked.is_some() { 44.0 } else { 12.0 };
    widgets::paint_left(
        ui,
        body.left_center() + vec2(56.0, 0.0),
        label,
        theme::regular(15.5),
        if enabled { t.text } else { t.text_faint },
        (rect.width() - 56.0 - reserve).max(0.0),
    );
    if checked == Some(true) {
        icons::paint(ui, Rect::from_center_size(body.right_center() - vec2(24.0, 0.0), vec2(20.0, 20.0)), "check", 20.0, t.accent_text);
    }
    if enabled { theme::hand(resp) } else { resp }
}

/// A modal sheet sliding up from the bottom: a scrim over the rest closes it when tapped.
/// Returns `false` once it should close (the scrim was tapped).
pub(crate) fn sheet(ctx: &egui::Context, id: &str, screen: Rect, title: Option<&str>, add: impl FnOnce(&mut egui::Ui)) -> bool {
    let t = Tokens::get(ctx);
    let mut open = true;
    egui::Area::new(egui::Id::new(("phone-scrim", id))).order(egui::Order::Middle).fixed_pos(screen.min).constrain(false).show(ctx, |ui| {
        let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
        ui.painter().rect_filled(r, CornerRadius::ZERO, Color32::from_black_alpha(110));
        if resp.clicked() {
            open = false;
        }
    });
    let width = screen.width();
    egui::Area::new(egui::Id::new(("phone-sheet", id)))
        .order(egui::Order::Foreground)
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(screen.center_bottom())
        .constrain(false)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(t.panel)
                .corner_radius(CornerRadius { nw: 18, ne: 18, sw: 0, se: 0 })
                .inner_margin(egui::Margin { left: 8, right: 8, top: 6, bottom: 14 })
                .show(ui, |ui| {
                    ui.set_width(width - 16.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    // The grab handle Material sheets show.
                    let (h, _) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::hover());
                    ui.painter().rect_filled(Rect::from_center_size(h.center(), vec2(36.0, 4.0)), CornerRadius::same(2), t.text_faint);
                    if let Some(title) = title {
                        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
                        widgets::row_text(ui, r.left_center() + vec2(14.0, 0.0), title, theme::semibold(16.0), t.text, r.width() - 28.0);
                    }
                    egui::ScrollArea::vertical().max_height(screen.height() * 0.72).auto_shrink([false, true]).show(ui, add);
                });
        });
    open
}

/// A command's label in the interface language, as menus show it.
pub(crate) fn command_label(app: &PdfCraftApp, id: &str) -> String {
    match pdfcraft_engine::commands::command(id) {
        Some(spec) => {
            let label = pdfcraft_engine::commands::current_label(spec, &app.session, app.active_ids().map(|(_, id)| id));
            crate::i18n::menu_label(spec.id, &label)
        }
        None => id.to_owned(),
    }
}

/// Whether a registered command can run now.
fn command_enabled(app: &PdfCraftApp, id: &str) -> bool {
    pdfcraft_engine::commands::command(id).is_some_and(|spec| app.command_enabled(spec))
}

// ---------------------------------------------------------------------------------------------
// Reader

fn reader(app: &mut PdfCraftApp, index: usize, ui: &mut egui::Ui, screen: Rect, t: &Tokens) {
    // Another document starts plain: bars shown, no panel, search or tool from the last one.
    let id = app.views.get(index).map(|v| v.id);
    if app.phone.doc != id {
        app.phone.doc = id;
        app.leave_phone_tool();
        app.phone.searching = false;
        app.phone.bars_hidden = false;
        app.close_phone_ai();
        app.right = None;
    }
    // The AI assistant takes the whole screen.
    #[cfg(not(target_arch = "wasm32"))]
    if app.phone.ai_open {
        if crate::ai_ui::page(app, ui, screen) {
            app.phone_ai_back();
        }
        return;
    }
    // A panel (bookmarks, page thumbnails, comments…) takes the whole screen, as a page of its own.
    if app.right.is_some() {
        let before = app.views.get(index).map(|v| (v.goto, v.current));
        crate::panels::right_panel(app, ui);
        // Going somewhere from the panel (a bookmark, a thumbnail, a comment) shows that place.
        let after = app.views.get(index).map(|v| (v.goto, v.current));
        if after.is_some_and(|(goto, _)| goto.is_some()) && after != before {
            app.right = None;
        }
        return;
    }
    let organizing = app.views.get(index).is_some_and(|v| v.organize);
    if organizing && app.phone.drawer {
        app.close_phone_drawer();
    }
    // The page view fills the screen and the bars float over it; an open tool drawer takes the
    // bottom of the screen, and the pages the rest.
    let drawer = drawer_rect(app, screen);
    match drawer {
        Some(d) => {
            let pages = Rect::from_min_max(ui.max_rect().min, pos2(ui.max_rect().right(), d.top()));
            ui.scope_builder(egui::UiBuilder::new().max_rect(pages), |ui| crate::canvas::document_area(app, index, ui));
            ui.scope_builder(egui::UiBuilder::new().max_rect(d), |ui| {
                ui.set_clip_rect(d);
                crate::panels::left_panel(app, ui);
            });
            // Its own ✕ closes the panel, and so the drawer.
            if !app.left_open {
                app.close_phone_drawer();
            }
        }
        None => crate::canvas::document_area(app, index, ui),
    }
    let ctx = ui.ctx().clone();
    let now = ctx.input(|i| i.time);
    let delay = ctx.options(|o| o.input_options.max_double_click_delay);
    if let Some(view) = app.views.get_mut(index)
        && let Some((_, at)) = view.tap
    {
        if now - at > delay {
            view.tap = None;
            if app.phone.tool.is_none() && !app.phone.drawer && !app.phone.searching && app.phone.sheet.is_none() {
                app.phone.bars_hidden = !app.phone.bars_hidden;
            }
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64((delay - (now - at)).max(0.0) + 0.01));
        }
    }
    // A tool, the drawer, a search or a sheet always shows its bars.
    if app.phone.tool.is_some() || app.phone.drawer || app.phone.searching || app.phone.sheet.is_some() || organizing {
        app.phone.bars_hidden = false;
    }
    if app.phone.bars_hidden {
        return;
    }
    let top = Rect::from_min_size(screen.min, vec2(screen.width(), TOP_BAR));
    let bottom = Rect::from_min_size(pos2(screen.left(), screen.bottom() - BOTTOM_BAR), vec2(screen.width(), BOTTOM_BAR));
    if organizing {
        organize_bar(app, index, ui, top, t);
        return;
    }
    if app.phone.searching {
        search_bar(app, index, ui, top, t);
    } else if let Some(mode) = app.phone.tool {
        if mode_top_bar(app, ui, top, t, mode.icon(), tl!(mode.label()), mode.hue_id()) {
            app.leave_phone_tool();
        }
    } else if app.phone.drawer {
        let (icon, title, hue_id) = match app.left {
            crate::LeftPanel::Tool(id) => catalog::group(id).map_or(("layout-grid", tl!("Tools"), ""), |g| (g.icon, tl!(g.label), g.id)),
            crate::LeftPanel::AllTools => ("layout-grid", tl!("All tools"), ""),
        };
        if mode_top_bar(app, ui, top, t, icon, title, hue_id) {
            app.close_phone_drawer();
        }
    } else {
        reader_top_bar(app, index, ui, top, t);
    }
    match app.phone.tool {
        Some(mode) => tool_bottom_bar(app, mode, ui, bottom, t),
        None if !app.phone.searching && !app.phone.drawer => reader_bottom_bar(app, ui, bottom, t),
        None => {}
    }
    if app.phone.tool.is_none() && !app.phone.searching && !app.phone.drawer {
        page_pill(app, index, ui, bottom, t);
    }
    sheets(app, index, ui, screen, t);
}

/// Where the tool drawer goes when it is open: the bottom of the screen, tall enough for a tool's
/// options and short enough to leave the page above usable.
fn drawer_rect(app: &PdfCraftApp, screen: Rect) -> Option<Rect> {
    if !(app.phone.drawer && app.left_open) {
        return None;
    }
    let room = (screen.height() - TOP_BAR - 160.0).max(0.0);
    // `clamp` can't panic: its lower bound is at most `room`, and its upper bound at least that.
    let height = (screen.height() * 0.45).clamp(220.0_f32.min(room), room.max(1.0));
    Some(Rect::from_min_max(pos2(screen.left(), screen.bottom() - height), screen.max))
}

fn reader_top_bar(app: &mut PdfCraftApp, index: usize, ui: &mut egui::Ui, rect: Rect, t: &Tokens) {
    let (name, dirty) =
        app.views.get(index).and_then(|v| app.session.get(v.id)).map(|d| (d.display_name(), d.dirty)).unwrap_or_else(|| (String::new(), false));
    let mut home = false;
    let mut search = false;
    let mut menu = false;
    let mut assistant = false;
    // The web build has no AI assistant (no HTTPS client of its own).
    let ai = cfg!(not(target_arch = "wasm32"));
    bar(ui, "phone-top-bar", rect, t, true, |ui| {
        home = icons::button(ui, "chevron-left", TARGET, false, tl!("Back")).clicked();
        let title = if dirty { format!("{} •", crate::bidi::visual(&name)) } else { crate::bidi::visual(&name).into_owned() };
        let width = ui.available_width() - if ai { 3.0 } else { 2.0 } * TARGET;
        title_text(ui, &title, width, t);
        if ai {
            assistant = icons::button(ui, "sparkles", TARGET, false, tl!("AI assistant")).clicked();
        }
        search = icons::button(ui, "search", TARGET, false, tl!("Search")).clicked();
        menu = icons::button(ui, "ellipsis-vertical", TARGET, false, tl!("Menu")).clicked();
    });
    if home {
        app.go_home();
    }
    if assistant {
        app.phone.ai_open = true;
    }
    if search {
        app.phone.searching = true;
        if let Some(v) = app.views.get_mut(index) {
            v.open_find();
        }
    }
    if menu {
        app.phone.sheet = Some(Sheet::Menu);
    }
}

fn reader_bottom_bar(app: &mut PdfCraftApp, ui: &mut egui::Ui, rect: Rect, t: &Tokens) {
    let mut picked: Option<ToolMode> = None;
    let mut edit = false;
    let mut tools = false;
    let edit_label = tl_ctx!("phone bar", "Edit");
    bar(ui, "phone-bottom-bar", rect, t, false, |ui| {
        let width = (ui.available_width() / 5.0).floor();
        let labels: Vec<&str> = [edit_label].into_iter().chain(ToolMode::ALL.iter().map(|m| tl!(m.label()))).chain([tl!("Tools")]).collect();
        let size = bar_label_size(ui, &labels, width - 6.0);
        edit = bar_item(ui, "file-pen-line", edit_label, width, size, tool_hue(t, "edit"), t).clicked();
        for mode in ToolMode::ALL {
            if bar_item(ui, mode.icon(), tl!(mode.label()), width, size, tool_hue(t, mode.hue_id()), t).clicked() {
                picked = Some(mode);
            }
        }
        tools = bar_item(ui, "layout-grid", tl!("Tools"), width, size, t.accent_text, t).clicked();
    });
    if edit {
        app.open_phone_drawer("edit");
    }
    if let Some(mode) = picked {
        app.enter_phone_tool(mode);
    }
    if tools {
        app.phone.sheet = Some(Sheet::Tools);
    }
}

/// The top bar of a mode (a tool mode, or a tool group in the drawer): its icon and name, Undo,
/// Redo, and Done on the right, away from where Back usually is. The bar takes a wash of the
/// tool's colour (`hue_id`, as [`tool_hue`] takes it), so the mode shows at a glance. Returns
/// whether Done was tapped.
fn mode_top_bar(app: &mut PdfCraftApp, ui: &mut egui::Ui, rect: Rect, t: &Tokens, icon: &str, title: &str, hue_id: &str) -> bool {
    let can_undo = command_enabled(app, "edit.undo");
    let can_redo = command_enabled(app, "edit.redo");
    let mut run: Option<&'static str> = None;
    let mut done = false;
    let hue = tool_hue(t, hue_id);
    let fill = t.chrome.lerp_to_gamma(hue, if t.dark() { 0.16 } else { 0.10 });
    bar_filled(ui, "phone-tool-top-bar", rect, fill, Some(t.divider), true, |ui| {
        let (r, _) = ui.allocate_exact_size(vec2(TARGET, TOP_BAR), Sense::hover());
        icons::paint(ui, Rect::from_center_size(r.center(), vec2(22.0, 22.0)), icon, 22.0, hue);
        let width = ui.available_width() - 2.0 * TARGET - done_width(ui);
        title_text(ui, title, width, t);
        if ui.add_enabled_ui(can_undo, |ui| icons::button(ui, "undo-2", TARGET, false, tl!("Undo"))).inner.clicked() {
            run = Some("edit.undo");
        }
        if ui.add_enabled_ui(can_redo, |ui| icons::button(ui, "redo-2", TARGET, false, tl!("Redo"))).inner.clicked() {
            run = Some("edit.redo");
        }
        done = done_button(ui, t).clicked();
    });
    if let Some(id) = run {
        app.execute(id);
    }
    done
}

fn done_width(ui: &egui::Ui) -> f32 {
    let w = ui.fonts_mut(|f| f.layout_no_wrap(tl!("Done").to_owned(), theme::semibold(15.0), Color32::WHITE).size().x);
    w + 36.0
}

/// The accent "Done" pill that ends a mode.
fn done_button(ui: &mut egui::Ui, t: &Tokens) -> egui::Response {
    let w = done_width(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, TARGET), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!("Done")));
    let press = theme::Press::track(ui, &resp);
    let pill = Rect::from_center_size(rect.center(), vec2(w - 8.0, 36.0)).translate(press.offset());
    ui.painter().rect_filled(pill, CornerRadius::same(18), press.fill(t.accent, t.accent.gamma_multiply(0.92), t.accent.gamma_multiply(0.82)));
    ui.painter().text(pill.center(), Align2::CENTER_CENTER, tl!("Done"), theme::semibold(15.0), Color32::WHITE);
    theme::hand(resp)
}

fn tool_bottom_bar(app: &mut PdfCraftApp, mode: ToolMode, ui: &mut egui::Ui, rect: Rect, t: &Tokens) {
    let mut run: Option<&'static str> = None;
    let mut style = false;
    let current = app.quick_tool;
    let colour = match current {
        QuickTool::Comment(tool) => Some(comments::color32(app.comment_prefs.style(tool).color)),
        _ => None,
    };
    let signature_label = command_label(app, FillTool::Signature.command());
    bar(ui, "phone-tool-bottom-bar", rect, t, false, |ui| {
        let swatch = if colour.is_some() { TARGET + 8.0 } else { 0.0 };
        let room = ui.available_width() - swatch;
        ui.scope_builder(egui::UiBuilder::new().max_rect(Rect::from_min_size(ui.cursor().min, vec2(room, BOTTOM_BAR))), |ui| {
            egui::ScrollArea::horizontal().id_salt("phone-tools").auto_shrink([false, true]).show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    match mode {
                        ToolMode::FillSign => {
                            for tool in crate::fill_sign::FILL_TOOLS {
                                let label = if tool == FillTool::Signature { signature_label.as_str() } else { tl!(tool.label()) };
                                if icons::button(ui, tool.icon(), TARGET, current == QuickTool::Fill(tool), label).clicked() {
                                    run = Some(tool.command());
                                }
                            }
                        }
                        _ => {
                            for tool in mode.comment_tools() {
                                if icons::button(ui, tool.icon(), TARGET, current == QuickTool::Comment(*tool), tl!(tool.label())).clicked() {
                                    run = Some(tool.command());
                                }
                            }
                        }
                    }
                });
            });
        });
        if let Some(c) = colour {
            ui.add_space(ui.available_width() - TARGET - 4.0);
            let (r, resp) = ui.allocate_exact_size(Vec2::splat(TARGET), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!("Colour")));
            ui.painter().circle_filled(r.center(), 13.0, c);
            ui.painter().circle_stroke(r.center(), 13.0, Stroke::new(1.5, t.border));
            style = theme::hand(resp).clicked();
        }
    });
    if let Some(id) = run {
        app.execute(id);
    }
    if style {
        app.phone.sheet = Some(Sheet::Style);
    }
}

fn search_bar(app: &mut PdfCraftApp, index: usize, ui: &mut egui::Ui, rect: Rect, t: &Tokens) {
    let Some(view) = app.views.get_mut(index) else { return };
    let pages = view.page_count();
    let searched = view.texts.len() + view.text_failed.len();
    let mut close = false;
    let mut step: Option<bool> = None;
    let Some(find) = view.find.as_mut() else {
        app.phone.searching = false;
        return;
    };
    bar(ui, "phone-search-bar", rect, t, true, |ui| {
        close = icons::button(ui, "chevron-left", TARGET, false, tl!("Back")).clicked();
        let status = if find.query.trim().is_empty() {
            String::new()
        } else if find.matches.is_empty() {
            if searched < pages { "…".to_owned() } else { tl!("No matches").to_owned() }
        } else {
            let more = if searched < pages { "+" } else { "" };
            crate::i18n::fmt(
                tl!("{c} of {n}{more}"),
                &[("c", &find.current.map(|c| c + 1).unwrap_or(0).to_string()), ("n", &find.matches.len().to_string()), ("more", more)],
            )
        };
        let status_w = ui.fonts_mut(|f| f.layout_no_wrap(status.clone(), theme::regular(13.0), t.text_muted).size().x);
        let field_w = (ui.available_width() - 2.0 * TARGET - status_w - 12.0).max(60.0);
        let edit = egui::TextEdit::singleline(&mut find.query)
            .id(egui::Id::new("phone-find-input"))
            .hint_text(tl!("Find text"))
            .font(theme::regular(16.0))
            .desired_width(field_w)
            .frame(egui::Frame::NONE);
        let r = ui.add(edit);
        if find.focus {
            r.request_focus();
            find.focus = false;
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            step = Some(true);
        }
        ui.add_space(4.0);
        ui.label(egui::RichText::new(status).font(theme::regular(13.0)).color(t.text_muted));
        ui.add_space(8.0);
        if icons::button(ui, "chevron-up", TARGET, false, tl!("Previous page")).clicked() {
            step = Some(false);
        }
        if icons::button(ui, "chevron-down", TARGET, false, tl!("Next page")).clicked() {
            step = Some(true);
        }
    });
    if find.query != find.case_query {
        view.rerun_find();
    }
    if let Some(forward) = step {
        view.find_step(forward);
    }
    if close {
        app.stop_phone_search();
    }
}

fn organize_bar(app: &mut PdfCraftApp, index: usize, ui: &mut egui::Ui, rect: Rect, t: &Tokens) {
    let title = catalog::group("organize").map_or("Organize pages", |g| g.label);
    let mut done = false;
    bar(ui, "phone-organize-bar", rect, t, true, |ui| {
        let (r, _) = ui.allocate_exact_size(vec2(TARGET, TOP_BAR), Sense::hover());
        icons::paint(ui, Rect::from_center_size(r.center(), vec2(22.0, 22.0)), "layout-grid", 22.0, t.accent_text);
        let width = ui.available_width() - done_width(ui);
        title_text(ui, tl!(title), width, t);
        done = done_button(ui, t).clicked();
    });
    if done && let Some(v) = app.views.get_mut(index) {
        v.organize = false;
    }
}

/// "3 / 12" over the page's bottom-left corner: where you are, and the way to another page.
fn page_pill(app: &mut PdfCraftApp, index: usize, ui: &mut egui::Ui, bottom: Rect, t: &Tokens) {
    let Some(view) = app.views.get(index) else { return };
    let n = view.page_count();
    if n == 0 {
        return;
    }
    let text = format!("{} / {n}", view.current.saturating_add(1).min(n));
    let font = theme::medium(13.5);
    let w = ui.fonts_mut(|f| f.layout_no_wrap(text.clone(), font.clone(), t.text).size().x) + 28.0;
    let pos = pos2(bottom.left() + 12.0, bottom.top() - 12.0 - 36.0);
    let mut tapped = false;
    egui::Area::new(egui::Id::new("phone-page-pill")).order(egui::Order::Middle).fixed_pos(pos).constrain(false).show(ui.ctx(), |ui| {
        // A finger-tall target around the pill.
        let (r, resp) = ui.allocate_exact_size(vec2(w.max(TARGET), 36.0), Sense::click());
        let label = crate::i18n::fmt(tl!("Page {p} of {n}"), &[("p", &view.current.saturating_add(1).to_string()), ("n", &n.to_string())]);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
        let press = theme::Press::track(ui, &resp);
        let shadow = egui::Shadow { offset: [0, 2], blur: 10, spread: 0, color: Color32::from_black_alpha(if t.dark() { 110 } else { 40 }) };
        ui.painter().add(shadow.as_shape(r, CornerRadius::same(18)));
        ui.painter().rect_filled(r, CornerRadius::same(18), press.fill(t.card, t.hover, t.pressed));
        ui.painter().text(r.center() + press.offset(), Align2::CENTER_CENTER, &text, font, t.text);
        tapped = theme::hand(resp).clicked();
    });
    if tapped {
        app.phone.page_text = view.current.saturating_add(1).to_string();
        app.phone.sheet = Some(Sheet::GoTo);
    }
}

fn sheets(app: &mut PdfCraftApp, index: usize, ui: &mut egui::Ui, screen: Rect, t: &Tokens) {
    let ctx = ui.ctx().clone();
    match app.phone.sheet {
        Some(Sheet::Menu) => menu_sheet(app, index, &ctx, screen, t),
        Some(Sheet::Tools) => tools_sheet(app, index, &ctx, screen, t),
        Some(Sheet::GoTo) => go_to_sheet(app, index, &ctx, screen, t),
        Some(Sheet::Style) => style_sheet(app, &ctx, screen, t),
        Some(Sheet::New | Sheet::Item | Sheet::Pick) | None => {}
    }
}

/// What a menu row does.
enum MenuPick {
    Command(&'static str),
    Panel(RightPanel),
    GoTo,
    Continuous,
}

fn menu_sheet(app: &mut PdfCraftApp, index: usize, ctx: &egui::Context, screen: Rect, t: &Tokens) {
    let continuous = app.views.get(index).is_some_and(|v| v.continuous);
    let mut pick: Option<MenuPick> = None;
    // (icon, label, enabled, checked, what): `checked` makes the row an on/off switch.
    let rows: Vec<(&'static str, String, bool, Option<bool>, MenuPick)> = vec![
        // Undo and Redo name what they take back ("Undo Edit text"): the reader has no bar for them.
        ("undo-2", command_label(app, "edit.undo"), command_enabled(app, "edit.undo"), None, MenuPick::Command("edit.undo")),
        ("redo-2", command_label(app, "edit.redo"), command_enabled(app, "edit.redo"), None, MenuPick::Command("edit.redo")),
        ("save", command_label(app, "file.save"), command_enabled(app, "file.save"), None, MenuPick::Command("file.save")),
        ("save", command_label(app, "file.save_as"), command_enabled(app, "file.save_as"), None, MenuPick::Command("file.save_as")),
        ("printer", command_label(app, "print.dialog"), command_enabled(app, "print.dialog"), None, MenuPick::Command("print.dialog")),
        ("text-cursor-input", command_label(app, "view.focus_page_input"), true, None, MenuPick::GoTo),
        ("files", tl!("Page thumbnails").to_owned(), true, None, MenuPick::Panel(RightPanel::Pages)),
        ("bookmark", tl!("Bookmarks").to_owned(), true, None, MenuPick::Panel(RightPanel::Bookmarks)),
        ("message-square-text", tl!("Comments").to_owned(), true, None, MenuPick::Panel(RightPanel::Comments)),
        ("arrow-up-down", command_label(app, "view.continuous"), true, Some(continuous), MenuPick::Continuous),
        ("moon", command_label(app, "view.theme"), true, None, MenuPick::Command("view.theme")),
        ("info", command_label(app, "file.properties"), command_enabled(app, "file.properties"), None, MenuPick::Command("file.properties")),
        ("settings", command_label(app, "app.preferences"), true, None, MenuPick::Command("app.preferences")),
        ("x", command_label(app, "file.close"), true, None, MenuPick::Command("file.close")),
    ];
    let open = sheet(ctx, "menu", screen, None, |ui| {
        for (icon, label, enabled, checked, what) in rows {
            if sheet_row(ui, icon, &label, enabled, checked, t).clicked() {
                pick = Some(what);
            }
        }
    });
    if !open || pick.is_some() {
        app.phone.sheet = None;
    }
    match pick {
        Some(MenuPick::Command(id)) => {
            app.execute(id);
        }
        Some(MenuPick::Panel(panel)) => app.choose_right_panel(Some(panel)),
        Some(MenuPick::GoTo) => {
            app.phone.page_text = app.views.get(index).map(|v| v.current.saturating_add(1).to_string()).unwrap_or_default();
            app.phone.sheet = Some(Sheet::GoTo);
        }
        Some(MenuPick::Continuous) => {
            app.execute("view.continuous");
        }
        None => {}
    }
}

/// Whether a tool group has at least one tool that works in this build.
pub(crate) fn ready_tools(g: &catalog::ToolGroup) -> bool {
    g.sections.iter().flat_map(|s| s.items).any(|i| i.availability == catalog::Availability::Ready)
}

fn tools_sheet(app: &mut PdfCraftApp, index: usize, ctx: &egui::Context, screen: Rect, t: &Tokens) {
    let mut pick: Option<&'static str> = None;
    let open = sheet(ctx, "tools", screen, Some(tl!("All tools")), |ui| {
        let cols = 4.0;
        let w = (ui.available_width() / cols).floor();
        // Every group with a tool that works in this build: a group can still be marked planned as
        // a whole (Edit a PDF) while most of its tools ship.
        let groups: Vec<&catalog::ToolGroup> = catalog::TOOL_GROUPS.iter().filter(|g| ready_tools(g)).collect();
        for row in groups.chunks(4) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for g in row {
                    if tool_tile(ui, g, w, t).clicked() {
                        pick = Some(g.id);
                    }
                }
            });
        }
    });
    if !open || pick.is_some() {
        app.phone.sheet = None;
    }
    let _ = index;
    if let Some(id) = pick {
        app.open_phone_tool_group(id);
    }
}

/// A tool group in the All tools sheet: its coloured icon above its name.
pub(crate) fn tool_tile(ui: &mut egui::Ui, g: &catalog::ToolGroup, width: f32, t: &Tokens) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 88.0), Sense::click());
    let label = tl!(g.label);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let press = theme::Press::track(ui, &resp);
    press.wash(ui, rect.shrink(3.0), 12, false);
    let body = rect.translate(press.offset());
    let hue = Color32::from_rgb(g.hue[0], g.hue[1], g.hue[2]);
    let disc = Rect::from_center_size(body.center_top() + vec2(0.0, 28.0), Vec2::splat(42.0));
    ui.painter().rect_filled(disc, CornerRadius::same(14), hue.gamma_multiply(if t.dark() { 0.28 } else { 0.14 }));
    icons::paint(ui, disc, g.icon, 22.0, hue);
    let mut job = egui::text::LayoutJob::simple(label.to_owned(), theme::regular(11.5), t.text, width - 8.0);
    job.wrap.max_rows = 2;
    job.halign = egui::Align::Center;
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(pos2(body.center().x, body.top() + 56.0), galley, t.text);
    theme::hand(resp)
}

fn go_to_sheet(app: &mut PdfCraftApp, index: usize, ctx: &egui::Context, screen: Rect, t: &Tokens) {
    let n = app.views.get(index).map_or(0, crate::DocView::page_count);
    let title = command_label(app, "view.focus_page_input");
    let mut go = false;
    let text = &mut app.phone.page_text;
    let open = sheet(ctx, "goto", screen, Some(&title), |ui| {
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            let field = egui::TextEdit::singleline(text)
                .id(egui::Id::new("phone-page-number"))
                .font(theme::regular(18.0))
                .desired_width(110.0)
                .char_limit(9)
                .horizontal_align(egui::Align::Center);
            let r = ui.add(field);
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                go = true;
            }
            ui.label(egui::RichText::new(format!("/ {n}")).font(theme::regular(16.0)).color(t.text_muted));
            ui.add_space(ui.available_width() - 120.0);
            let (r, resp) = ui.allocate_exact_size(vec2(110.0, TARGET), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!("OK")));
            ui.painter().rect_filled(Rect::from_center_size(r.center(), vec2(100.0, 40.0)), CornerRadius::same(20), t.accent);
            ui.painter().text(r.center(), Align2::CENTER_CENTER, tl!("OK"), theme::semibold(15.0), Color32::WHITE);
            go |= theme::hand(resp).clicked();
        });
        ui.add_space(8.0);
    });
    if go {
        // Page numbers count from 1; anything else is ignored rather than guessed.
        if let Some(page) = app.phone.page_text.trim().parse::<usize>().ok().filter(|p| (1..=n).contains(p))
            && let Some(v) = app.views.get_mut(index)
        {
            v.go_to_page(page - 1);
        }
        app.phone.sheet = None;
    } else if !open {
        app.phone.sheet = None;
    }
}

fn style_sheet(app: &mut PdfCraftApp, ctx: &egui::Context, screen: Rect, t: &Tokens) {
    let QuickTool::Comment(tool) = app.quick_tool else {
        app.phone.sheet = None;
        return;
    };
    let style = app.comment_prefs.style(tool);
    let mut picked: Option<[f64; 3]> = None;
    let mut width = style.width;
    let mut opacity = (style.opacity * 100.0).round();
    let (mut width_changed, mut opacity_changed) = (false, false);
    let open = sheet(ctx, "style", screen, Some(tl!("Colour")), |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.add_space(8.0);
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (name, c) in comments::SWATCHES {
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(TARGET), Sense::click());
                resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, c == style.color, tl!(name)));
                ui.painter().circle_filled(r.center(), 16.0, comments::color32(c));
                ui.painter().circle_stroke(r.center(), 16.0, Stroke::new(1.0, Color32::from_black_alpha(60)));
                if c == style.color {
                    ui.painter().circle_stroke(r.center(), 21.0, Stroke::new(2.5, t.accent));
                }
                if theme::hand(resp).clicked() {
                    picked = Some(c);
                }
            }
        });
        ui.add_space(10.0);
        slider_row(ui, tl!("Opacity"), &mut opacity, 10.0..=100.0, 5.0, " %", &mut opacity_changed);
        if tool.has_width() {
            slider_row(ui, tl!("Line thickness"), &mut width, 0.5..=12.0, 0.5, " pt", &mut width_changed);
        }
    });
    if let Some(c) = picked {
        app.comment_prefs.set_color(tool, c);
    }
    if opacity_changed {
        app.comment_prefs.set_opacity(tool, opacity / 100.0);
    }
    if width_changed {
        app.comment_prefs.set_width(tool, width);
    }
    if !open || picked.is_some() {
        app.phone.sheet = None;
    }
}

fn slider_row<N: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut N,
    range: std::ops::RangeInclusive<N>,
    step: f64,
    suffix: &str,
    changed: &mut bool,
) {
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.label(egui::RichText::new(label).font(theme::regular(14.0)));
    });
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.spacing_mut().slider_width = (ui.available_width() - 110.0).max(80.0);
        ui.spacing_mut().interact_size.y = TARGET;
        *changed |= ui.add(egui::Slider::new(value, range).step_by(step).suffix(suffix)).changed();
    });
    ui.add_space(6.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phone_screen_always_gets_the_phone_shell() {
        assert_eq!(ShellPref::parse(" Phone "), Some(ShellPref::Phone));
        assert_eq!(ShellPref::parse("auto"), Some(ShellPref::Auto));
        assert_eq!(ShellPref::parse("desktop"), None, "a phone has no desktop layout to ask for");
        let mut app = PdfCraftApp::new();
        assert!(!app.phone_layout(), "a desktop screen gets the desktop shell");
        app.phone_screen = true;
        assert!(app.phone_layout());
        app.phone_screen = false;
        app.shell = ShellPref::Phone;
        assert!(app.phone_layout(), "and any screen can ask for the phone one");
        // Settings from a build that had a desktop layout on phones read as Auto.
        assert_eq!(serde_json::from_value::<ShellPref>(serde_json::json!("desktop")).unwrap_or_default(), ShellPref::Auto);
    }

    #[test]
    fn the_phone_shell_draws_at_its_own_points() {
        let mut app = PdfCraftApp::new();
        app.system_text_scale = 0.8;
        assert_eq!(app.auto_text_scale(), 0.8, "the desktop shell keeps the system's text scale");
        app.shell = ShellPref::Phone;
        assert_eq!(app.auto_text_scale(), 1.0);
    }

    #[test]
    fn all_tools_lists_every_group_with_a_working_tool() {
        let group = |id| catalog::group(id).expect("catalog group");
        assert!(ready_tools(group("edit")), "Edit a PDF is planned as a group, but its tools ship");
        assert!(ready_tools(group("protect")));
        assert!(!ready_tools(group("ai")), "the AI assistant has no provider yet");
    }

    #[test]
    fn tool_modes_own_their_tools() {
        assert!(ToolMode::Comment.owns(QuickTool::Comment(CommentTool::Underline)));
        assert!(!ToolMode::Comment.owns(QuickTool::Comment(CommentTool::Ink)));
        assert!(ToolMode::FillSign.owns(QuickTool::Fill(FillTool::Check)));
        assert!(!ToolMode::Draw.owns(QuickTool::Select));
        for mode in ToolMode::ALL {
            assert!(icons::exists(mode.icon()), "{mode:?}");
            assert!(mode.comment_tools().iter().all(|t| mode.owns(QuickTool::Comment(*t))));
        }
    }
}
