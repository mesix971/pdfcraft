//! Home in the phone layout: the user's name and photo, things to start with, and the files —
//! recent, starred and on the device — each with a preview of its first page. Four tabs along the
//! bottom (Home, Files, Create, Tools) and a "+" for what's started most.
//!
//! The arrangement is the one phone PDF apps settled on (a greeting, a row of quick actions, the
//! files with thumbnails, a tab bar at the bottom); the look and the words are PdfCraft's, with
//! the ArtCraft mark and Lucide icons. A file without a preview shows the mark over "PDF".

use std::collections::HashSet;
use std::sync::Arc;

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, TextureHandle, Vec2, pos2, vec2};
use pdfcraft_engine::catalog;

use crate::phone::{self, Sheet, TARGET, TOP_BAR};
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons, widgets};

/// Home's header: the app's mark and name, search, and the profile.
const HEADER: f32 = 64.0;
/// The ArtCraft mark's blue, which the glow behind Home's top fades into from the icon's green.
const MARK_BLUE: Color32 = Color32::from_rgb(0x4E, 0x7B, 0xFB);
/// The tab bar along the bottom.
const NAV: f32 = 64.0;
/// The floating "+".
const FAB: f32 = 60.0;
/// A file's row, and its thumbnail.
const ROW: f32 = 78.0;
const THUMB: Vec2 = vec2(46.0, 60.0);

/// Home's own surfaces: a deeper ground than the rest of the app (its glow shows on it), cards
/// a step up from it, and their edges. The phone's other full screens (the file browser, Combine)
/// share them.
pub(crate) struct Look {
    pub(crate) ground: Color32,
    pub(crate) surface: Color32,
    pub(crate) edge: Color32,
    /// The bars along the bottom (the tab bar, a screen's actions): the surfaces' white in the
    /// light theme, the ground's black in the dark one, an edge above them.
    pub(crate) bar: Color32,
}

pub(crate) fn look(t: &Tokens) -> Look {
    if t.dark() {
        // True black ground (a phone's OLED pixels off), surfaces a step above it.
        Look { ground: Color32::BLACK, surface: Color32::from_rgb(0x13, 0x15, 0x15), edge: Color32::from_rgb(0x25, 0x29, 0x28), bar: Color32::BLACK }
    } else {
        Look { ground: Color32::from_rgb(0xF5, 0xF8, 0xF7), surface: Color32::WHITE, edge: Color32::from_rgb(0xE0, 0xE6, 0xE4), bar: Color32::WHITE }
    }
}

/// The tabs along the bottom of Home.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HomeTab {
    #[default]
    Home,
    Files,
    Create,
    Tools,
}

impl HomeTab {
    /// Its place along the tab bar, from the left.
    fn place(self) -> i32 {
        match self {
            Self::Home => 0,
            Self::Files => 1,
            Self::Create => 2,
            Self::Tools => 3,
        }
    }

    /// `home`, `files`, `create` or `tools`.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "home" => Some(Self::Home),
            "files" => Some(Self::Files),
            "create" => Some(Self::Create),
            "tools" => Some(Self::Tools),
            _ => None,
        }
    }
}

/// Home's lists of files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileList {
    #[default]
    Recent,
    Starred,
    Device,
}

/// What happens once the document an action asked for is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Then {
    /// Just read it.
    Read,
    /// A tool group: its tool mode, the page grid, or its panel in the drawer.
    Tool(&'static str),
    /// The AI assistant's screen.
    Assistant,
}

/// Home's own state.
#[derive(Default)]
pub struct HomeState {
    pub tab: HomeTab,
    pub list: FileList,
    /// The search field's text while file names are searched.
    search: Option<String>,
    /// The search field takes the keyboard when it first shows.
    focus_search: bool,
    /// The file whose ⋮ menu is open, and the open document showing it.
    menu: Option<(String, Option<usize>)>,
    /// The action waiting for a document to be chosen (the Pick sheet).
    then: Option<Then>,
    /// The profile screen is open.
    pub profile: bool,
    /// The name as typed on the profile screen (the name kept is it, trimmed).
    name_draft: Option<String>,
    /// The assistant's settings screen is open (from Home: it needs no document).
    pub ai_settings: bool,
    /// The grid of the device's photos is open, to choose the profile's (over the profile).
    pub picking: bool,
    /// The profile photo as a texture (`None`: it doesn't read as one), and the PNG it was made from.
    photo: Option<(Arc<Vec<u8>>, Option<TextureHandle>)>,
    /// A chosen picture being made into the profile photo, on a worker thread.
    #[cfg(not(target_arch = "wasm32"))]
    photo_job: Option<std::sync::mpsc::Receiver<Result<Vec<u8>, PhotoError>>>,
    /// Whether PdfCraft may read the device's files, and when that was last asked (asking the
    /// system goes through JNI on Android: not every frame).
    access: Option<(f64, Option<bool>)>,
    #[cfg(not(target_arch = "wasm32"))]
    thumbs: Thumbs,
    #[cfg(not(target_arch = "wasm32"))]
    device: DeviceScan,
    /// The device's photos and their tiles, while the photo grid shows (made when it first does).
    #[cfg(not(target_arch = "wasm32"))]
    photos: Option<(DeviceScan, Thumbs)>,
    /// Whether PdfCraft may read the device's photos, and when that was last asked.
    photos_access: Option<(f64, Option<bool>)>,
}

impl HomeState {
    /// Close the photo grid, and let its photos and their tiles go.
    fn leave_grid(&mut self) {
        self.picking = false;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.photos = None;
        }
    }

    /// Something of Home's is open over it (Back closes it first).
    pub(crate) fn back(&mut self) -> bool {
        // The photo grid and the assistant's settings open over the profile.
        if self.picking {
            self.leave_grid();
        } else if self.ai_settings {
            self.ai_settings = false;
        } else if self.profile {
            self.profile = false;
            self.name_draft = None;
        } else if self.search.is_some() {
            self.search = None;
        } else if self.tab != HomeTab::Home {
            self.tab = HomeTab::Home;
        } else {
            return false;
        }
        true
    }
}

impl PdfCraftApp {
    /// The user's name as Home greets them: the name comments are signed with, unless it is still
    /// the system's default ("Guest", a login name).
    pub fn profile_name(&self) -> Option<&str> {
        let name = self.comment_prefs.author.trim();
        // "Guest" is the default where the system gives no login name (Android).
        (!name.is_empty() && name != "Guest" && name != crate::comments::login_name()).then_some(name)
    }

    /// Whether PdfCraft may read the device's files (`None`: it needs no permission), asked of the
    /// system every few seconds at most. When it becomes allowed, the device's PDFs are looked
    /// for again.
    pub(crate) fn home_files_access(&mut self, now: f64) -> Option<bool> {
        const RECHECK_SECS: f64 = 3.0;
        if let Some((at, known)) = self.phone.home.access
            && now - at < RECHECK_SECS
            && now >= at
        {
            return known;
        }
        let known = files_access(self);
        #[cfg(not(target_arch = "wasm32"))]
        if known == Some(true) && self.phone.home.access.is_some_and(|(_, was)| was == Some(false)) {
            // A scan under way started without access: its list would be short. Files that
            // couldn't be read for a preview may be readable now.
            self.phone.home.device.running = None;
            self.phone.home.device.last = None;
            self.phone.home.thumbs.forget_failures();
        }
        self.phone.home.access = Some((now, known));
        known
    }

    /// Whether PdfCraft may read the device's photos (`None`: it needs no permission), asked of
    /// the system every second at most while the grid shows: the answer to the system's question
    /// comes back on its own. Once it is allowed, the photos are looked for again.
    fn home_photos_access(&mut self, now: f64) -> Option<bool> {
        const RECHECK_SECS: f64 = 1.0;
        if let Some((at, known)) = self.phone.home.photos_access
            && now - at < RECHECK_SECS
            && now >= at
        {
            return known;
        }
        let known = photos_access(self);
        #[cfg(not(target_arch = "wasm32"))]
        if known == Some(true) && self.phone.home.photos_access.is_some_and(|(_, was)| was == Some(false)) {
            self.phone.home.photos = None;
        }
        self.phone.home.photos_access = Some((now, known));
        known
    }

    /// Whether Home has a preview of `path`'s first page (tests and automation).
    pub fn home_thumbnail_ready(&self, path: &str) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.phone.home.thumbs.ready.get(path).is_some_and(|t| t.tex.is_some());
        #[cfg(target_arch = "wasm32")]
        {
            let _ = path;
            false
        }
    }

    /// Whether Home asked for a preview of `path`, or has one (tests and automation).
    pub fn home_preview_asked(&self, path: &str) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.phone.home.thumbs.ready.contains_key(path) || self.phone.home.thumbs.asked.contains(path);
        #[cfg(target_arch = "wasm32")]
        {
            let _ = path;
            false
        }
    }

    /// The PDFs Home found on the device (tests and automation).
    pub fn home_device_files(&self) -> Vec<String> {
        #[cfg(not(target_arch = "wasm32"))]
        return self.phone.home.device.files.iter().map(|f| f.path.clone()).collect();
        #[cfg(target_arch = "wasm32")]
        Vec::new()
    }

    /// Look for the device's PDFs (as Home's "On this device" does) unless a look is under way or
    /// recent, and take what the last one found. Returns whether one is under way.
    #[cfg(all(not(target_arch = "wasm32"), any(test, target_os = "android")))]
    pub(crate) fn look_for_device_pdfs(&mut self, ctx: &egui::Context) -> bool {
        let now = ctx.input(|i| i.time);
        self.home_files_access(now);
        self.phone.home.device.poll();
        let roots = scan_roots(self);
        self.phone.home.device.want(roots, ctx);
        self.phone.home.device.scanning()
    }

    /// The PDFs the last look found, newest first.
    #[cfg(all(not(target_arch = "wasm32"), any(test, target_os = "android")))]
    pub(crate) fn device_pdfs(&self) -> &[DeviceFile] {
        &self.phone.home.device.files
    }

    /// Run `then` on a document: one of the open ones, or a file to open (the Pick sheet asks
    /// which when documents are open).
    pub(crate) fn home_action(&mut self, then: Then) {
        if self.views.is_empty() {
            self.open_then(then);
        } else {
            self.phone.home.then = Some(then);
            self.phone.sheet = Some(Sheet::Pick);
        }
    }

    /// Choose a PDF, open it, and run `then` on it.
    fn open_then(&mut self, then: Then) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let dialog = crate::file_dialog::AsyncFileDialog::new().add_filter("PDF", &["pdf"]);
            self.ask_one(crate::pickers::Ask::File(dialog), None, move |app, path| {
                let tabs = app.views.len();
                app.open_recent(&path.to_string_lossy());
                // Opened, or already open and brought forward.
                if app.views.len() > tabs || app.active.is_some() {
                    app.run_then(then);
                }
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = then;
            self.open_dialog();
        }
    }

    /// Run `then` on the active document.
    pub(crate) fn run_then(&mut self, then: Then) {
        if self.active.is_none() {
            return;
        }
        // A document just opened would start plain on its first frame, the tool with it.
        self.phone_adopt_active();
        self.phone.bars_hidden = false;
        match then {
            Then::Read => {}
            Then::Tool(id) => self.open_phone_tool_group(id),
            Then::Assistant => self.phone.ai_open = cfg!(not(target_arch = "wasm32")),
        }
    }

    /// Open a file Home lists: the open document showing it, or the file.
    fn open_listed(&mut self, path: &str) {
        self.open_recent(path);
        self.phone.bars_hidden = false;
    }
}

/// Draw Home in the phone layout. `screen` is the content area.
pub(crate) fn show(app: &mut PdfCraftApp, ui: &mut egui::Ui, screen: Rect, t: &Tokens) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let now = ui.input(|i| i.time);
        app.phone.home.thumbs.poll(ui.ctx(), now);
        app.phone.home.device.poll();
        poll_photo(app, ui.ctx());
    }
    // The photo grid and the assistant's settings open over the profile, which shows again after.
    #[cfg(not(target_arch = "wasm32"))]
    if app.phone.home.picking {
        photo_grid(app, ui, screen, t);
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    if app.phone.home.ai_settings {
        if crate::ai_ui::settings_page(app, ui, screen) {
            app.phone.home.ai_settings = false;
        }
        return;
    }
    if app.phone.home.profile {
        profile(app, ui, screen, t);
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    app.phone.home.thumbs.begin_frame();
    // egui fades a scrolled area's edges in its frame's colour, which isn't Home's ground and
    // glow: a band would show under the header.
    ui.spacing_mut().scroll.fade.strength = 0.0;
    let nav = Rect::from_min_max(pos2(screen.left(), screen.bottom() - NAV), screen.max);
    let body = Rect::from_min_max(screen.min, pos2(screen.right(), nav.top()));
    ui.painter().rect_filled(body, CornerRadius::ZERO, look(t).ground);
    // Another tab slides in from its side of the tab bar and fades in; the bar stays.
    let tab = app.phone.home.tab;
    let arriving = crate::transition::change(ui.ctx(), "phone-home-tab", tab.place(), tab.place());
    ui.scope(|ui| {
        ui.set_clip_rect(body.intersect(ui.clip_rect()));
        ui.multiply_opacity(arriving.opacity());
        if tab == HomeTab::Home {
            glow(ui.painter(), body, t);
        }
        let body = body.translate(vec2(arriving.offset(crate::transition::SLIDE), 0.0));
        match tab {
            HomeTab::Home => home_tab(app, ui, body, t),
            HomeTab::Files => files_tab(app, ui, body, t),
            HomeTab::Create => create_tab(app, ui, body, t),
            HomeTab::Tools => tools_tab(app, ui, body, t),
        }
    });
    tab_bar(app, ui, nav, t);
    if matches!(app.phone.home.tab, HomeTab::Home | HomeTab::Files) {
        fab(app, ui, body, t);
    }
    sheets(app, ui, screen, t);
}

// ---------------------------------------------------------------------------------------------
// The tabs

fn home_tab(app: &mut PdfCraftApp, ui: &mut egui::Ui, body: Rect, t: &Tokens) {
    let top = Rect::from_min_size(body.min, vec2(body.width(), HEADER));
    header(app, ui, top, t);
    let rest = Rect::from_min_max(pos2(body.left(), top.bottom()), body.max);
    ui.scope_builder(egui::UiBuilder::new().max_rect(rest), |ui| {
        egui::ScrollArea::vertical().id_salt("phone-home").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if app.phone.home.search.is_none() {
                ui.add_space(14.0);
                // By first name, as a person greets.
                let greeting = match app.profile_name() {
                    Some(name) => crate::i18n::fmt(tl!("Hello, {name}"), &[("name", first_name(name))]),
                    None => tl!("Welcome to PdfCraft").to_owned(),
                };
                indented(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(greeting).font(theme::semibold(30.0)).color(t.text)).wrap());
                });
                ui.add_space(20.0);
                tips(app, ui, t);
                section_title(ui, tl!("What would you like to do?"), t);
                quick_actions(app, ui, t);
                ui.add_space(10.0);
                list_tabs(app, ui, t);
            }
            file_list(app, ui, t, app.phone.home.list);
            // Room for the "+" over the end of the list.
            ui.add_space(FAB + 32.0);
        });
    });
}

fn files_tab(app: &mut PdfCraftApp, ui: &mut egui::Ui, body: Rect, t: &Tokens) {
    let top = Rect::from_min_size(body.min, vec2(body.width(), HEADER));
    title_bar(ui, "phone-files-bar", top, tl!("Files"), t);
    let rest = Rect::from_min_max(pos2(body.left(), top.bottom()), body.max);
    let places = places(app);
    ui.scope_builder(egui::UiBuilder::new().max_rect(rest), |ui| {
        egui::ScrollArea::vertical().id_salt("phone-files").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(6.0);
            if !places.is_empty() {
                section_title(ui, tl!("Places"), t);
                for (label, icon, path) in &places {
                    if phone::sheet_row(ui, icon, label, true, None, t).clicked() {
                        browse(app, path);
                    }
                }
                ui.add_space(8.0);
            }
            if phone::sheet_row(ui, "folder-open", tl!("Open file"), true, None, t).clicked() {
                app.execute("file.open");
            }
            ui.add_space(8.0);
            section_title(ui, tl!("PDFs on this device"), t);
            file_list(app, ui, t, FileList::Device);
            ui.add_space(FAB + 32.0);
        });
    });
}

/// Open the file browser at `dir`, and the file chosen there.
fn browse(app: &mut PdfCraftApp, dir: &std::path::Path) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dialog = crate::file_dialog::AsyncFileDialog::new().add_filter("PDF", &["pdf"]).set_directory(dir);
        app.ask_one(crate::pickers::Ask::File(dialog), None, |app, path| app.open_listed(&path.to_string_lossy()));
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = dir;
        app.open_dialog();
    }
}

fn create_tab(app: &mut PdfCraftApp, ui: &mut egui::Ui, body: Rect, t: &Tokens) {
    let top = Rect::from_min_size(body.min, vec2(body.width(), HEADER));
    title_bar(ui, "phone-create-bar", top, tl!("Create"), t);
    let rest = Rect::from_min_max(pos2(body.left(), top.bottom()), body.max);
    // (command, icon, the tool group whose colour it takes, what it makes)
    let rows: [(&'static str, &str, &str, &str); 4] = [
        ("create.images", "image", "create", tl!("Photos and scans become the pages of a new PDF.")),
        ("create.file", "file-input", "export", tl!("Text, pictures and other files, converted to PDF.")),
        ("page.combine", "files", "combine", tl!("Several PDFs and pictures in one document.")),
        ("create.blank", "file-plus", "organize", tl!("An empty page to start from.")),
    ];
    let mut run = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rest), |ui| {
        egui::ScrollArea::vertical().id_salt("phone-create").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(8.0);
            for (command, icon, hue, detail) in rows {
                let title = phone::command_label(app, command);
                if action_row(ui, icon, phone::tool_color(hue), &title, detail, t).clicked() {
                    run = Some(command);
                }
            }
        });
    });
    if let Some(command) = run {
        app.execute(command);
    }
}

fn tools_tab(app: &mut PdfCraftApp, ui: &mut egui::Ui, body: Rect, t: &Tokens) {
    let top = Rect::from_min_size(body.min, vec2(body.width(), HEADER));
    title_bar(ui, "phone-tools-bar", top, tl!("Tools"), t);
    let rest = Rect::from_min_max(pos2(body.left(), top.bottom()), body.max);
    let mut pick = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rest), |ui| {
        egui::ScrollArea::vertical().id_salt("phone-tools-tab").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(8.0);
            let w = (ui.available_width() / 4.0).floor();
            let groups: Vec<&catalog::ToolGroup> = catalog::TOOL_GROUPS.iter().filter(|g| phone::ready_tools(g)).collect();
            for row in groups.chunks(4) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for g in row {
                        if phone::tool_tile(ui, g, w, t).clicked() {
                            pick = Some(g.id);
                        }
                    }
                });
            }
        });
    });
    match pick {
        // These work without a document.
        Some("combine") => {
            app.execute("page.combine");
        }
        Some("create") => app.phone.home.tab = HomeTab::Create,
        Some(id) => app.home_action(Then::Tool(id)),
        None => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Home's parts

/// The header, over the glow rather than on a bar of its own: the ArtCraft mark and the app's
/// name, search, and the profile.
fn header(app: &mut PdfCraftApp, ui: &mut egui::Ui, rect: Rect, t: &Tokens) {
    let mut search = false;
    let mut profile = false;
    let mut close_search = false;
    let photo = photo_texture(app, ui.ctx());
    let initials = initials(app.profile_name());
    let searching = app.phone.home.search.is_some();
    let fade = ui.opacity();
    egui::Area::new(egui::Id::new("phone-home-bar")).order(egui::Order::Middle).fixed_pos(rect.min).constrain(false).show(ui.ctx(), |ui| {
        crate::transition::follow_screen(ui, fade);
        let (r, _) = ui.allocate_exact_size(rect.size(), Sense::hover());
        let inner = r.shrink2(vec2(8.0, 0.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            if let Some(query) = app.phone.home.search.as_mut() {
                close_search = header_button(ui, "chevron-left", tl!("Back"), t).clicked();
                ui.add_space(4.0);
                let edit = egui::TextEdit::singleline(query)
                    .id(egui::Id::new("phone-home-search"))
                    .hint_text(tl!("Search files"))
                    .font(theme::regular(16.0))
                    .margin(egui::Margin::symmetric(14, 10))
                    .background_color(look(t).surface)
                    .desired_width(ui.available_width() - 8.0)
                    .char_limit(200);
                let r = widgets::named_edit(ui, edit, tl!("Search files"));
                // On the frame it shows: focus asked for earlier would name a widget not there yet.
                if std::mem::take(&mut app.phone.home.focus_search) {
                    r.request_focus();
                }
                return;
            }
            ui.add_space(8.0);
            let (mark, _) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
            egui::Image::from_bytes("bytes://artcraft-mark.svg", include_bytes!("../../../docs/brand/artcraft-mark.svg")).paint_at(ui, mark);
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width() - 2.0 * TARGET, HEADER), Sense::hover());
            ui.painter().text(r.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, "PdfCraft", theme::semibold(21.0), t.text);
            search = header_button(ui, "search", tl!("Search"), t).clicked();
            let (b, resp) = ui.allocate_exact_size(Vec2::splat(TARGET), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!("Profile")));
            let press = theme::Press::track(ui, &resp);
            let face = Rect::from_center_size(b.center() + press.offset(), Vec2::splat(34.0));
            avatar(ui, face, photo.as_ref(), &initials, t);
            gradient_ring(ui.painter(), face.center(), 19.0, 21.5, t.brand, MARK_BLUE);
            profile = theme::hand(resp).clicked();
        });
    });
    if search && !searching {
        app.phone.home.search = Some(String::new());
        app.phone.home.focus_search = true;
    }
    if close_search {
        app.phone.home.search = None;
    }
    if profile {
        app.phone.home.profile = true;
    }
}

/// An icon button in the header, in the text's colour.
fn header_button(ui: &mut egui::Ui, icon: &str, label: &str, t: &Tokens) -> egui::Response {
    let (b, resp) = ui.allocate_exact_size(Vec2::splat(TARGET), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let press = theme::Press::track(ui, &resp);
    press.wash(ui, b.shrink(4.0), 20, false);
    icons::paint(ui, b.translate(press.offset()), icon, 22.0, t.text);
    theme::hand(resp)
}

/// The Files, Create and Tools tabs' title: large, on Home's ground as Home's header is.
fn title_bar(ui: &mut egui::Ui, id: &str, rect: Rect, title: &str, t: &Tokens) {
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).id_salt(id), |ui| {
        let (r, _) = ui.allocate_exact_size(rect.size(), Sense::hover());
        let width = (r.width() - 32.0).max(0.0);
        widgets::row_text(ui, pos2(r.left() + 16.0, r.center().y + 2.0), title, theme::semibold(26.0), t.text, width);
    });
}

/// The photo, or the name's initials on the accent, or a person: `rect`'s circle.
fn avatar(ui: &egui::Ui, rect: Rect, photo: Option<&TextureHandle>, initials: &str, t: &Tokens) {
    let radius = rect.width().min(rect.height()) / 2.0;
    match photo {
        Some(tex) => {
            egui::Image::new((tex.id(), rect.size())).corner_radius(CornerRadius::same(radius.min(255.0) as u8)).paint_at(ui, rect);
        }
        None => {
            // From the accent, deeper: white initials read on all of it.
            gradient_disc(ui.painter(), rect.center(), radius, t.accent, t.accent.lerp_to_gamma(Color32::BLACK, 0.25));
            if initials.is_empty() {
                icons::paint(ui, rect, "user-round", radius, Color32::WHITE);
            } else {
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, initials, theme::semibold(radius * 0.8), Color32::WHITE);
            }
        }
    }
}

/// The glow at the top of Home: the icon's green at the left and the mark's blue at the right,
/// fading out downwards (eased, so it has no edge).
fn glow(painter: &egui::Painter, body: Rect, t: &Tokens) {
    const BANDS: u32 = 20;
    let strength = if t.dark() { 0.62 } else { 0.30 };
    let height = body.height().min(320.0);
    let mut mesh = egui::Mesh::default();
    for i in 0..=BANDS {
        let f = i as f32 / BANDS as f32;
        // It rises from nothing over the first few points, so the status bar above (in the
        // ground's colour) meets it without an edge, then fades out downwards.
        let rise = (f / 0.12).min(1.0);
        let fade = rise * (2.0 - rise) * (1.0 - f) * (1.0 - f) * strength;
        let y = body.top() + height * f;
        mesh.colored_vertex(pos2(body.left(), y), t.brand.gamma_multiply(fade));
        mesh.colored_vertex(pos2(body.right(), y), MARK_BLUE.gamma_multiply(fade * 0.85));
    }
    for i in 0..BANDS {
        let a = i * 2;
        mesh.add_triangle(a, a + 1, a + 2);
        mesh.add_triangle(a + 1, a + 3, a + 2);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// A disc shaded from `from` (top left) to `to` (bottom right).
pub(crate) fn gradient_disc(painter: &egui::Painter, center: egui::Pos2, radius: f32, from: Color32, to: Color32) {
    const STEPS: u32 = 48;
    let mut mesh = egui::Mesh::default();
    let shade = |p: egui::Pos2| {
        // Along the diagonal: 0 at the top left of the disc, 1 at the bottom right.
        let d = ((p.x - center.x) + (p.y - center.y)) / (2.0 * radius.max(0.5) * std::f32::consts::SQRT_2) + 0.5;
        from.lerp_to_gamma(to, d.clamp(0.0, 1.0))
    };
    mesh.colored_vertex(center, shade(center));
    for i in 0..=STEPS {
        let a = i as f32 / STEPS as f32 * std::f32::consts::TAU;
        let p = center + radius * vec2(a.cos(), a.sin());
        mesh.colored_vertex(p, shade(p));
        if i > 0 {
            mesh.add_triangle(0, i, i + 1);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// A ring between `inner` and `outer`, shaded from `from` (top left) to `to` (bottom right).
fn gradient_ring(painter: &egui::Painter, center: egui::Pos2, inner: f32, outer: f32, from: Color32, to: Color32) {
    const STEPS: u32 = 64;
    let mut mesh = egui::Mesh::default();
    for i in 0..=STEPS {
        let a = i as f32 / STEPS as f32 * std::f32::consts::TAU;
        let dir = vec2(a.cos(), a.sin());
        // Green at the top left, blue at the bottom right.
        let c = from.lerp_to_gamma(to, ((dir.x + dir.y) / std::f32::consts::SQRT_2 * 0.5 + 0.5).clamp(0.0, 1.0));
        mesh.colored_vertex(center + inner * dir, c);
        mesh.colored_vertex(center + outer * dir, c);
        if i > 0 {
            let k = i * 2;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k + 1, k);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// The colours of Home's top and bottom this frame (for the system's bars around them).
pub(crate) fn system_bars(app: &PdfCraftApp, t: &Tokens) -> (Color32, Color32) {
    let home = &app.phone.home;
    if home.profile || home.picking || home.ai_settings {
        (t.chrome, t.pasteboard)
    } else {
        let look = look(t);
        (look.ground, look.bar)
    }
}

/// A row of choices, one of them taken (the theme): the taken one in the accent.
fn segmented(ui: &mut egui::Ui, choices: &[(&str, &str)], taken: usize, t: &Tokens) -> Option<usize> {
    let look = look(t);
    // The icon above its name, so a long name (Automatique, Automatisch) has the cell's width.
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::hover());
    ui.painter().rect(rect, CornerRadius::same(14), look.surface, Stroke::new(1.0, look.edge), egui::StrokeKind::Inside);
    let n = choices.len().max(1) as f32;
    let w = rect.width() / n;
    let mut chosen = None;
    for (i, (icon, label)) in choices.iter().enumerate() {
        let cell = Rect::from_min_size(pos2(rect.left() + w * i as f32, rect.top()), vec2(w, rect.height())).shrink(4.0);
        let resp = ui.interact(cell, egui::Id::new(("phone-segment", *label)), Sense::click());
        let on = i == taken;
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, on, *label));
        if on {
            phone::vivid_tile(ui.painter(), cell, 10.0, t.accent);
        } else {
            theme::Press::track(ui, &resp).wash(ui, cell, 10, false);
        }
        let ink = if on { Color32::WHITE } else { t.text };
        icons::paint(ui, Rect::from_center_size(pos2(cell.center().x, cell.top() + 18.0), Vec2::splat(20.0)), icon, 20.0, ink);
        let mut job = egui::text::LayoutJob::simple_singleline((*label).to_owned(), theme::semibold(13.5), ink);
        job.wrap = egui::text::TextWrapping::truncate_at_width((cell.width() - 8.0).max(0.0));
        job.halign = egui::Align::Center;
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(pos2(cell.center().x, cell.bottom() - 22.0), galley, ink);
        if theme::hand(resp).clicked() {
            chosen = Some(i);
        }
    }
    chosen
}

/// An on/off row: a title and what it does, and a switch.
fn switch_row(ui: &mut egui::Ui, title: &str, detail: &str, on: bool, t: &Tokens) -> egui::Response {
    let width = ui.available_width();
    let mut job = egui::text::LayoutJob::simple(detail.to_owned(), theme::regular(12.5), t.text_muted, (width - 72.0).max(0.0));
    job.wrap.max_rows = 2;
    let detail = ui.painter().layout_job(job);
    let height = (detail.size().y + 34.0).max(TARGET);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, title));
    theme::Press::track(ui, &resp).wash(ui, rect, 10, false);
    widgets::row_text(ui, pos2(rect.left(), rect.top() + 14.0), title, theme::medium(15.5), t.text, (width - 72.0).max(0.0));
    ui.painter().galley(pos2(rect.left(), rect.top() + 26.0), detail, t.text_muted);
    // The switch: a track, and a knob at the side it is on.
    let track = Rect::from_center_size(pos2(rect.right() - 28.0, rect.center().y), vec2(46.0, 26.0));
    // Off, a track dark enough for the white knob (and the track itself) to show on a light page.
    let fill = if on { t.accent } else { t.text_faint };
    ui.painter().rect_filled(track, CornerRadius::same(13), fill);
    let knob = if on { track.right() - 13.0 } else { track.left() + 13.0 };
    ui.painter().circle_filled(pos2(knob, track.center().y), 10.0, Color32::WHITE);
    theme::hand(resp)
}

/// The first word of a name ("Ana Diaz" → "Ana").
fn first_name(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}

/// The first letters of the name's first two words ("Ana Diaz" → "AD").
fn initials(name: Option<&str>) -> String {
    name.unwrap_or_default().split_whitespace().filter_map(|w| w.chars().next()).take(2).flat_map(char::to_uppercase).collect()
}

/// The profile photo as a texture, made again only when the photo changes (a photo that doesn't
/// read as one isn't decoded again every frame either).
fn photo_texture(app: &mut PdfCraftApp, ctx: &egui::Context) -> Option<TextureHandle> {
    let Some(png) = app.profile_photo.clone() else {
        app.phone.home.photo = None;
        return None;
    };
    if let Some((made_from, tex)) = &app.phone.home.photo
        && Arc::ptr_eq(made_from, &png)
    {
        return tex.clone();
    }
    let tex = pdfcraft_engine::signature_image::profile_picture_pixels(&png)
        .map(|(w, h, rgba)| ctx.load_texture("profile-photo", egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba), egui::TextureOptions::LINEAR));
    app.phone.home.photo = Some((png, tex.clone()));
    tex
}

fn indented<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(16, 0)).show(ui, add).inner
}

fn section_title(ui: &mut egui::Ui, title: &str, t: &Tokens) {
    egui::Frame::NONE.inner_margin(egui::Margin { left: 16, right: 16, top: 12, bottom: 10 }).show(ui, |ui| {
        let r = ui.add(egui::Label::new(egui::RichText::new(title).font(theme::semibold(18.0)).color(t.text)).wrap());
        widgets::heading(ui, &r, title);
    });
}

/// The cards across the top: what PdfCraft can do that the user hasn't set up yet. Each can be
/// dismissed for good.
fn tips(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    // (id, icon, colour, title, text, action)
    let mut cards: Vec<(&'static str, &str, Color32, &str, &str, &str)> = Vec::new();
    #[cfg(not(target_arch = "wasm32"))]
    if !app.ai.ready() {
        cards.push((
            "ai",
            "sparkles",
            phone::tool_color("ai"),
            tl!("Your AI assistant"),
            tl!("Summarize, question and correct your PDFs with Claude, ChatGPT or Gemini, with your own key."),
            tl!("Set up"),
        ));
    }
    if app.profile_name().is_none() {
        cards.push((
            "profile",
            "user-round",
            t.brand,
            tl!("Make PdfCraft yours"),
            tl!("Add your name and a photo. Your name also signs your comments."),
            tl!("Add my name"),
        ));
    }
    let now = ui.input(|i| i.time);
    if app.home_files_access(now) == Some(false) {
        cards.push((
            "access",
            "folder-open",
            phone::tool_color("export"),
            tl!("See your PDFs"),
            tl!("Allow access to your files and PdfCraft lists the PDFs on this device."),
            tl!("Allow access"),
        ));
    }
    cards.retain(|c| !app.home_tips_dismissed.iter().any(|d| d == c.0));
    if cards.is_empty() {
        return;
    }
    let mut act = None;
    let mut dismiss = None;
    let look = look(t);
    // The next card shows at the edge, so the row reads as one that scrolls.
    let width = (ui.available_width() - 48.0).min(360.0);
    // The tinted panel at each card's left, with the tip's icon.
    const PANEL: f32 = 84.0;
    swipe_row(ui, "phone-tips", |ui| {
        ui.horizontal_top(|ui| {
            ui.add_space(16.0);
            ui.spacing_mut().item_spacing.x = 12.0;
            for (id, icon, hue, title, text, action) in &cards {
                // The whole card is its action; its button and ✕, drawn over it, keep theirs. It is
                // laid out below, so it takes taps where it was last frame.
                let card_key = egui::Id::new(("phone-tip-card", *id));
                if let Some(was) = ui.data(|d| d.get_temp::<Rect>(card_key)) {
                    let whole = ui.interact(was, card_key, Sense::click());
                    whole.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, *title));
                    if theme::hand(whole).clicked() {
                        act = Some(*id);
                    }
                }
                let card = egui::Frame::NONE
                    .fill(look.surface)
                    .stroke(Stroke::new(1.0, look.edge))
                    .corner_radius(CornerRadius::same(18))
                    // The panel (84) and a gap (14) at the left.
                    .inner_margin(egui::Margin { left: 98, right: 4, top: 12, bottom: 10 })
                    .show(ui, |ui| {
                        ui.set_width(width - PANEL - 18.0);
                        ui.set_min_height(104.0);
                        ui.horizontal_top(|ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 3.0;
                                // Room for the ✕.
                                ui.set_width(ui.available_width() - TARGET);
                                ui.add_space(2.0);
                                // Not selectable: a tap on the words is a tap on the card.
                                ui.add(
                                    egui::Label::new(egui::RichText::new(*title).font(theme::semibold(16.0)).color(t.text)).wrap().selectable(false),
                                );
                                ui.add(
                                    egui::Label::new(egui::RichText::new(*text).font(theme::regular(13.5)).color(t.text_muted))
                                        .wrap()
                                        .selectable(false),
                                );
                                ui.add_space(4.0);
                                let r = ui.add(
                                    egui::Button::new(egui::RichText::new(*action).font(theme::semibold(15.0)).color(t.accent_text))
                                        .frame(false)
                                        .min_size(vec2(0.0, TARGET)),
                                );
                                if r.clicked() {
                                    act = Some(*id);
                                }
                            });
                            // Each its own name: "Dismiss" three times says nothing to a screen reader.
                            if icons::button(ui, "x", TARGET, false, &format!("{} · {title}", tl!("Dismiss"))).clicked() {
                                dismiss = Some(*id);
                            }
                        });
                    });
                ui.data_mut(|d| d.insert_temp(card_key, card.response.rect));
                // The tip's colour as a block at the card's left, as tall as the card came out.
                let r = card.response.rect.shrink(8.0);
                let panel = Rect::from_min_size(r.min, vec2(PANEL - 14.0, r.height()));
                phone::glowing_tile(ui.painter(), panel, 13.0, *hue, t);
                phone::tile_glyph(ui, panel, icon, 32.0);
            }
            ui.add_space(4.0);
        });
    });
    ui.add_space(8.0);
    if let Some(id) = dismiss {
        app.home_tips_dismissed.push(id.to_owned());
    }
    match act {
        Some("ai") => app.phone.home.ai_settings = true,
        Some("profile") => app.phone.home.profile = true,
        Some("access") => ask_files_access(app),
        _ => {}
    }
}

/// A row that scrolls sideways (the tips, the quick actions) in Home's page, which scrolls up and
/// down. egui gives a drag to the innermost scroll area, so the row doesn't drag itself: a swipe
/// up or down goes to the page (its own scrolling, fling and all), and one sideways is read from
/// the finger here and moves the row. Nothing is laid over the row's buttons, so taps stay
/// theirs (a widget sensing drags over them took their taps on a touch screen).
pub(crate) fn swipe_row<R>(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let source = egui::scroll_area::ScrollSource { drag: egui::scroll_area::DragScroll::Never, ..Default::default() };
    egui::ScrollArea::horizontal()
        .id_salt(id)
        .auto_shrink([false, true])
        .scroll_source(source)
        .show(ui, |ui| {
            if ui.input(|i| i.has_touch_screen()) {
                // The row's own: inside a scroll area `ui.id()` is the same for every row.
                swipe(ui, ui.clip_rect(), egui::Id::new(("phone-swipe", id)));
            }
            add(ui)
        })
        .inner
}

/// A finger on a row, or a fling after one.
#[derive(Clone, Copy, Default)]
struct Swipe {
    /// The finger went down on the row and hasn't lifted.
    tracking: bool,
    /// 0: sideways, 1: up and down; `None` until the finger has moved enough to tell.
    axis: Option<usize>,
    /// How far the finger has moved since it went down.
    moved: Vec2,
    /// After a sideways swipe: the row's speed, slowing down.
    speed: f32,
}

/// Move the row at `row` (the visible part of its scroll area) with a finger swiping sideways on
/// it: the row's scroll area takes the sideways part of the delta given to it.
fn swipe(ui: &egui::Ui, row: Rect, key: egui::Id) {
    // How far a finger moves before its direction counts, and the fling's slowing (a scroll
    // area's own figures).
    const DECIDE: f32 = 6.0;
    const FRICTION: f32 = 1000.0;
    const STOP: f32 = 20.0;
    let mut s: Swipe = ui.data(|d| d.get_temp(key)).unwrap_or_default();
    let (pressed, origin, down, delta, velocity, dt) = ui.input(|i| {
        (i.pointer.any_pressed(), i.pointer.press_origin(), i.pointer.primary_down(), i.pointer.delta(), i.pointer.velocity(), i.stable_dt)
    });
    let sideways = |amount: f32| ui.scroll_with_delta_animation(vec2(amount, 0.0), egui::style::ScrollAnimation::none());
    if pressed {
        // On the row itself, not on something drawn over it (a sheet, the "+").
        let on_row = origin.is_some_and(|p| row.contains(p) && ui.ctx().layer_id_at(p) == Some(ui.layer_id()));
        s = Swipe { tracking: on_row, ..Swipe::default() };
    }
    if s.tracking && down {
        s.moved += delta;
        match s.axis {
            Some(0) => sideways(delta.x),
            Some(_) => {}
            None if s.moved.length() > DECIDE => {
                let axis = usize::from(s.moved.x.abs() <= s.moved.y.abs());
                s.axis = Some(axis);
                // What moved before the direction was known goes that way too.
                if axis == 0 {
                    sideways(s.moved.x);
                }
            }
            None => {}
        }
    } else if s.tracking {
        // Lifted: a sideways swipe carries on for a moment.
        if s.axis == Some(0) && velocity.x.is_finite() {
            s.speed = velocity.x;
        }
        s.tracking = false;
        s.axis = None;
    } else if s.speed != 0.0 {
        let dt = dt.min(0.1);
        if !s.speed.is_finite() || s.speed.abs() < STOP || FRICTION * dt > s.speed.abs() {
            s.speed = 0.0;
        } else {
            s.speed -= FRICTION * dt * s.speed.signum();
            sideways(s.speed * dt);
            ui.ctx().request_repaint();
        }
    }
    ui.data_mut(|d| d.insert_temp(key, s));
}

/// The row of quick actions: a coloured tile each, its name under it.
fn quick_actions(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    // (tool group for the colour, icon and name; what it does)
    let mut actions: Vec<(&'static str, Then)> =
        vec![("edit", Then::Tool("edit")), ("fill_sign", Then::Tool("fill_sign")), ("comment", Then::Tool("comment"))];
    if cfg!(not(target_arch = "wasm32")) {
        actions.push(("ai", Then::Assistant));
    }
    actions.extend([("combine", Then::Read), ("create", Then::Read), ("organize", Then::Tool("organize")), ("compress", Then::Tool("compress"))]);
    let mut chosen = None;
    swipe_row(ui, "phone-quick", |ui| {
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            for (k, (id, then)) in actions.into_iter().enumerate() {
                // The assistant isn't a tool group of the catalog's that ships (it is its own screen).
                let Some(g) = catalog::group(id).filter(|g| id == "ai" || phone::ready_tools(g)) else { continue };
                let label = match id {
                    "ai" => tl!("AI assistant"),
                    // Shorter than the group's "Add comments", for the tile.
                    "comment" => tl!("Comments"),
                    _ => tl!(g.label),
                };
                let icon = if id == "ai" { "sparkles" } else { g.icon };
                if quick_tile(ui, icon, phone::tool_color(id), label, crate::transition::tile_entrance(ui.ctx(), k), t).clicked() {
                    chosen = Some((id, then));
                }
            }
            ui.add_space(10.0);
        });
    });
    match chosen {
        Some(("combine", _)) => {
            app.execute("page.combine");
        }
        Some(("create", _)) => app.phone.home.tab = HomeTab::Create,
        Some((_, then)) => app.home_action(then),
        None => {}
    }
}

/// One quick action: a 68-point tile in its colour, its name in two lines under it. `entrance`
/// is how far below its place it is drawn and its opacity, as it comes in.
fn quick_tile(ui: &mut egui::Ui, icon: &str, hue: Color32, label: &str, entrance: (f32, f32), t: &Tokens) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(86.0, 118.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let press = theme::Press::track(ui, &resp);
    let (rise, opacity) = entrance;
    let mut painter = ui.painter().clone();
    painter.multiply_opacity(opacity);
    let rect = rect.translate(vec2(0.0, rise));
    let tile = Rect::from_center_size(rect.center_top() + vec2(0.0, 38.0), Vec2::splat(68.0)).translate(press.offset());
    // The tool's colour at full strength, glowing a little on the ground; deeper while pressed.
    let glow = egui::Shadow { offset: [0, 5], blur: 18, spread: 1, color: hue.gamma_multiply(if t.dark() { 0.7 } else { 0.42 }) };
    painter.add(glow.as_shape(tile, CornerRadius::same(20)));
    phone::vivid_tile(&painter, tile, 20.0, press.fill(hue, hue.lerp_to_gamma(Color32::BLACK, 0.08), hue.lerp_to_gamma(Color32::BLACK, 0.18)));
    // A child that takes no room of the row's: only its opacity differs.
    let mut glyph = ui.new_child(egui::UiBuilder::new().max_rect(tile));
    glyph.multiply_opacity(opacity);
    phone::tile_glyph(&glyph, tile, icon, 32.0);
    // Smaller type, rather than a word cut in two ("Commentaires").
    let widest = label
        .split_whitespace()
        .map(|w| ui.fonts_mut(|f| f.layout_no_wrap(w.to_owned(), theme::regular(12.5), Color32::WHITE).size().x))
        .fold(0.0, f32::max);
    let size = if widest > 80.0 { (12.5 * 80.0 / widest).max(10.0) } else { 12.5 };
    let mut job = egui::text::LayoutJob::simple(label.to_owned(), theme::regular(size), t.text, 80.0);
    job.wrap.max_rows = 2;
    job.halign = egui::Align::Center;
    let galley = painter.layout_job(job);
    painter.galley(pos2(rect.center().x, tile.bottom() + 8.0), galley, t.text);
    theme::hand(resp)
}

/// Recent · Starred · On this device, the chosen one underlined.
fn list_tabs(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let lists = [(FileList::Recent, tl!("Recent")), (FileList::Starred, tl!("Starred")), (FileList::Device, tl!("On this device"))];
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::hover());
    ui.painter().hline(row.x_range(), row.bottom() - 0.5, Stroke::new(1.0, look(t).edge));
    const SIZE: f32 = 15.5;
    const GAP: f32 = 28.0;
    let widths: Vec<f32> =
        lists.iter().map(|(_, l)| ui.fonts_mut(|f| f.layout_no_wrap((*l).to_owned(), theme::semibold(SIZE), Color32::WHITE).size().x)).collect();
    let room = (row.width() - 32.0 - GAP * (lists.len() as f32 - 1.0)).max(1.0);
    let scale = (room / widths.iter().sum::<f32>().max(1.0)).clamp(0.75, 1.0);
    let font = theme::semibold(SIZE * scale);
    let mut x = row.left() + 16.0;
    for ((list, label), w) in lists.into_iter().zip(widths) {
        let w = w * scale;
        let hit = Rect::from_min_max(pos2(x - 8.0, row.top()), pos2(x + w + 8.0, row.bottom()));
        let resp = ui.interact(hit, egui::Id::new(("phone-list-tab", label)), Sense::click());
        let selected = app.phone.home.list == list;
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, label));
        let color = if selected { t.text } else { t.text_muted };
        ui.painter().text(pos2(x, row.center().y), Align2::LEFT_CENTER, label, font.clone(), color);
        if selected {
            let bar = Rect::from_min_max(pos2(x, row.bottom() - 3.0), pos2(x + w, row.bottom()));
            ui.painter().rect_filled(bar, CornerRadius { nw: 2, ne: 2, sw: 0, se: 0 }, t.brand);
        }
        if theme::hand(resp).clicked() {
            app.phone.home.list = list;
        }
        x += w + 28.0;
    }
}

/// A file Home lists.
struct Listed {
    path: String,
    name: String,
    /// "12 pages · 3.5 MB", or what is known.
    detail: String,
    /// The open document showing it (its tab).
    open: Option<usize>,
    starred: bool,
}

/// The files of `list`, filtered by the search, open documents first, each once.
fn listed(app: &PdfCraftApp, list: FileList) -> Vec<Listed> {
    let mut out: Vec<Listed> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let pages = |n: usize| crate::i18n::trn(crate::i18n::current(), n as u64, "{n} page", "{n} pages");
    // The open documents' files, and their tabs.
    let open: std::collections::HashMap<String, usize> =
        app.views.iter().enumerate().filter_map(|(i, v)| Some((app.session.get(v.id)?.path.clone()?, i))).collect();
    match list {
        FileList::Recent => {
            for (i, view) in app.views.iter().enumerate() {
                let Some(doc) = app.session.get(view.id) else { continue };
                let detail = format!("{} · {}", pages(doc.info.pages.len()), crate::panels::human_size(doc.info.file_size));
                // A document with no file yet (a new one) is listed by its name.
                let path = doc.path.clone().unwrap_or_else(|| format!("doc:{}", i));
                if seen.insert(path.clone()) {
                    out.push(Listed { path, name: doc.display_name(), detail, open: Some(i), starred: false });
                }
            }
            for r in &app.recent {
                if seen.insert(r.path.clone()) {
                    let detail = format!("{} · {}", pages(r.pages), crate::panels::human_size(r.size));
                    out.push(Listed { path: r.path.clone(), name: r.name.clone(), detail, open: None, starred: false });
                }
            }
        }
        FileList::Starred => {
            for path in &app.starred {
                if !seen.insert(path.clone()) {
                    continue;
                }
                let name = std::path::Path::new(path).file_name().map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
                let detail = app
                    .recent
                    .iter()
                    .find(|r| &r.path == path)
                    .map_or_else(String::new, |r| format!("{} · {}", pages(r.pages), crate::panels::human_size(r.size)));
                out.push(Listed { path: path.clone(), name, detail, open: open.get(path).copied(), starred: false });
            }
        }
        FileList::Device =>
        {
            #[cfg(not(target_arch = "wasm32"))]
            for f in &app.phone.home.device.files {
                if seen.insert(f.path.clone()) {
                    let detail = format!("{} · {}", f.folder, crate::panels::human_size(f.size));
                    out.push(Listed { path: f.path.clone(), name: f.name.clone(), detail, open: open.get(&f.path).copied(), starred: false });
                }
            }
        }
    }
    if let Some(query) = app.phone.home.search.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let query = query.to_lowercase();
        out.retain(|l| l.name.to_lowercase().contains(&query));
    }
    let starred: HashSet<&str> = app.starred.iter().map(String::as_str).collect();
    for l in &mut out {
        l.starred = starred.contains(l.path.as_str());
    }
    out
}

/// The rows of `list` (or, while searching, of every list), with what to show when it's empty.
fn file_list(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, list: FileList) {
    let searching = app.phone.home.search.is_some();
    let mut files = listed(app, list);
    if searching {
        let mut seen: HashSet<String> = files.iter().map(|f| f.path.clone()).collect();
        for other in [FileList::Recent, FileList::Starred, FileList::Device] {
            for l in listed(app, other) {
                if seen.insert(l.path.clone()) {
                    files.push(l);
                }
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    if list == FileList::Device || searching {
        let roots = scan_roots(app);
        app.phone.home.device.want(roots, ui.ctx());
    }
    if files.is_empty() {
        empty(app, ui, t, if searching { None } else { Some(list) });
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    let now = ui.input(|i| i.time);
    let mut open = None;
    let mut menu = None;
    for f in &files {
        // Rows off screen only take their room: a list of hundreds lays out (and previews) the
        // ten shown.
        let next = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), ROW));
        if !ui.is_rect_visible(next) {
            ui.allocate_space(next.size());
            continue;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let thumb = (!f.path.starts_with("doc:")).then(|| app.phone.home.thumbs.get(&f.path, now, ui.ctx())).flatten();
        #[cfg(target_arch = "wasm32")]
        let thumb: Option<TextureHandle> = None;
        let (row, more) = file_row(ui, f, thumb.as_ref(), t);
        if row.clicked() {
            open = Some((f.path.clone(), f.open));
        }
        if more.clicked() {
            menu = Some((f.path.clone(), f.open));
        }
    }
    if let Some((path, tab)) = open {
        match tab {
            Some(i) => {
                app.active = Some(i);
                app.phone.bars_hidden = false;
            }
            None => app.open_listed(&path),
        }
    }
    if menu.is_some() {
        app.phone.home.menu = menu;
        app.phone.sheet = Some(Sheet::Item);
    }
}

/// One file: its first page (or the mark over "PDF"), its name and detail, and ⋮.
fn file_row(ui: &mut egui::Ui, f: &Listed, thumb: Option<&TextureHandle>, t: &Tokens) -> (egui::Response, egui::Response) {
    let (rect, row) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    let name = f.name.clone();
    row.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
    let more_rect = Rect::from_center_size(rect.right_center() - vec2(30.0, 0.0), Vec2::splat(TARGET));
    let more = ui.interact(more_rect, egui::Id::new(("phone-file-more", &f.path)), Sense::click());
    let more_label = crate::i18n::fmt(tl!("More for {name}"), &[("name", &f.name)]);
    more.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &more_label));
    let press = theme::Press::track(ui, &row);
    press.wash(ui, rect.shrink2(vec2(6.0, 2.0)), 12, false);
    let body = rect.translate(press.offset());
    let card = Rect::from_center_size(body.left_center() + vec2(16.0 + THUMB.x / 2.0, 0.0), THUMB);
    thumbnail(ui, card, thumb, t);
    let text_left = card.right() + 14.0;
    let width = (more_rect.left() - text_left - 4.0).max(0.0);
    let title = if f.open.is_some() { format!("{} ", crate::bidi::visual(&f.name)) } else { crate::bidi::visual(&f.name).into_owned() };
    widgets::row_text(ui, pos2(text_left, body.center().y - 11.0), title, theme::medium(16.0), t.text, width);
    let mut detail_x = text_left;
    if f.open.is_some() {
        // "Open", as a small chip before the detail.
        // "Open" as a state (a document now open), not the command.
        let chip = tl_ctx!("file state", "Open");
        let font = theme::semibold(11.0);
        let w = ui.fonts_mut(|fonts| fonts.layout_no_wrap(chip.to_owned(), font.clone(), Color32::WHITE).size().x) + 12.0;
        let r = Rect::from_min_size(pos2(detail_x, body.center().y + 4.0), vec2(w, 18.0));
        ui.painter().rect_filled(r, CornerRadius::same(9), t.accent_soft);
        ui.painter().text(r.center(), Align2::CENTER_CENTER, chip, font, t.accent_text);
        detail_x = r.right() + 6.0;
    }
    if f.starred {
        icons::paint(
            ui,
            Rect::from_center_size(pos2(detail_x + 7.0, body.center().y + 13.0), Vec2::splat(14.0)),
            "star",
            13.0,
            phone::tool_color("comment"),
        );
        detail_x += 18.0;
    }
    widgets::row_text(
        ui,
        pos2(detail_x, body.center().y + 13.0),
        &f.detail,
        theme::regular(12.5),
        t.text_faint,
        (more_rect.left() - detail_x - 4.0).max(0.0),
    );
    let more_press = theme::Press::track(ui, &more);
    more_press.wash(ui, more_rect.shrink(6.0), 18, false);
    icons::paint(ui, more_rect.translate(more_press.offset()), "ellipsis-vertical", 20.0, t.icon);
    ui.painter().hline(pos2(text_left, 0.0).x..=rect.right() - 16.0, rect.bottom() - 0.5, Stroke::new(1.0, look(t).edge));
    (theme::hand(row), theme::hand(more))
}

/// A file's first page in `card`, or, before it's ready (or for a file that can't show one), a
/// page with the ArtCraft mark over "PDF".
pub(crate) fn thumbnail(ui: &egui::Ui, card: Rect, thumb: Option<&TextureHandle>, t: &Tokens) {
    let shadow = egui::Shadow { offset: [0, 1], blur: 4, spread: 0, color: Color32::from_black_alpha(if t.dark() { 90 } else { 30 }) };
    if let Some(tex) = thumb {
        // The page fitted in the card, white behind it.
        let size = tex.size_vec2();
        let k = (card.width() / size.x.max(1.0)).min(card.height() / size.y.max(1.0));
        let page = Rect::from_center_size(card.center(), size * k);
        ui.painter().add(shadow.as_shape(page, CornerRadius::same(3)));
        ui.painter().rect_filled(page, CornerRadius::same(3), Color32::WHITE);
        egui::Image::new((tex.id(), page.size())).corner_radius(CornerRadius::same(3)).paint_at(ui, page);
        ui.painter().rect_stroke(page, CornerRadius::same(3), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
        return;
    }
    let look = look(t);
    ui.painter().add(shadow.as_shape(card, CornerRadius::same(6)));
    ui.painter().rect(card, CornerRadius::same(6), look.surface, Stroke::new(1.0, look.edge), egui::StrokeKind::Inside);
    let mark = Rect::from_center_size(card.center() - vec2(0.0, 8.0), Vec2::splat(card.width() * 0.56));
    egui::Image::from_bytes("bytes://artcraft-mark.svg", include_bytes!("../../../docs/brand/artcraft-mark.svg")).paint_at(ui, mark);
    ui.painter().text(pos2(card.center().x, card.bottom() - 10.0), Align2::CENTER_CENTER, "PDF", theme::semibold(10.5), t.accent_text);
}

/// What an empty list says, with a way forward. `None`: a search found nothing.
fn empty(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, list: Option<FileList>) {
    let (title, text) = match list {
        None => (tl!("No files match."), ""),
        Some(FileList::Recent) => (tl!("No recent files yet."), tl!("The files you open show here.")),
        Some(FileList::Starred) => (tl!("No starred files."), tl!("Star a file from its menu to find it here.")),
        Some(FileList::Device) => {
            #[cfg(not(target_arch = "wasm32"))]
            let scanning = app.phone.home.device.scanning();
            #[cfg(target_arch = "wasm32")]
            let scanning = false;
            if scanning {
                (tl!("Looking for PDFs…"), "")
            } else {
                (tl!("No PDFs found."), tl!("PdfCraft looks in Download, Documents and its own folder."))
            }
        }
    };
    let mut open_file = false;
    egui::Frame::NONE.inner_margin(egui::Margin { left: 28, right: 28, top: 24, bottom: 8 }).show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add(icons::image(if list == Some(FileList::Starred) { "star" } else { "file-text" }, 40.0, t.text_faint));
            ui.add_space(10.0);
            ui.label(egui::RichText::new(title).font(theme::semibold(15.5)).color(t.text));
            if !text.is_empty() {
                ui.add_space(4.0);
                ui.add(egui::Label::new(egui::RichText::new(text).color(t.text_muted)).wrap());
            }
            if list == Some(FileList::Recent) {
                ui.add_space(16.0);
                open_file = pill(ui, "folder-open", tl!("Open file"), t).clicked();
            }
        });
    });
    if open_file {
        app.execute("file.open");
    }
}

/// A rounded accent button a finger tall, with an icon.
fn pill(ui: &mut egui::Ui, icon: &str, label: &str, t: &Tokens) -> egui::Response {
    let font = theme::semibold(15.0);
    let w = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE).size().x) + 64.0;
    let (r, resp) = ui.allocate_exact_size(vec2(w, TARGET), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let press = theme::Press::track(ui, &resp);
    let body = r.translate(press.offset());
    let deeper = |k: f32| t.accent.lerp_to_gamma(Color32::BLACK, k);
    ui.painter().rect_filled(body, CornerRadius::same(24), press.fill(t.accent, deeper(0.08), deeper(0.18)));
    icons::paint(ui, Rect::from_center_size(body.left_center() + vec2(26.0, 0.0), vec2(18.0, 18.0)), icon, 18.0, Color32::WHITE);
    ui.painter().text(body.left_center() + vec2(42.0, 0.0), Align2::LEFT_CENTER, label, font, Color32::WHITE);
    theme::hand(resp)
}

/// An action with an explanation under it, its icon in its colour: the "+" sheet and Create.
fn action_row(ui: &mut egui::Ui, icon: &str, hue: Color32, title: &str, detail: &str, t: &Tokens) -> egui::Response {
    // A menu's "…" (a dialog follows) says nothing here.
    let title = title.trim_end_matches(['…', '.']);
    let left = 78.0;
    let width = (ui.available_width() - left - 12.0).max(0.0);
    // The explanation wraps to two lines.
    let mut job = egui::text::LayoutJob::simple(detail.to_owned(), theme::regular(12.5), t.text_muted, width);
    job.wrap.max_rows = 2;
    let detail = ui.painter().layout_job(job);
    let height = (detail.size().y + 40.0).max(68.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title));
    let press = theme::Press::track(ui, &resp);
    press.wash(ui, rect.shrink2(vec2(4.0, 2.0)), 12, false);
    let body = rect.translate(press.offset());
    let disc = Rect::from_center_size(body.left_center() + vec2(36.0, 0.0), Vec2::splat(44.0));
    phone::glowing_tile(ui.painter(), disc, 14.0, hue, t);
    phone::tile_glyph(ui, disc, icon, 22.0);
    let top = body.center().y - (detail.size().y + 22.0) / 2.0;
    widgets::row_text(ui, pos2(body.left() + left, top + 10.0), title, theme::medium(15.5), t.text, width);
    ui.painter().galley(pos2(body.left() + left, top + 22.0), detail, t.text_muted);
    theme::hand(resp)
}

/// The tab bar: Home, Files, Create, Tools.
fn tab_bar(app: &mut PdfCraftApp, ui: &mut egui::Ui, rect: Rect, t: &Tokens) {
    let tabs = [
        (HomeTab::Home, "house", tl!("Home")),
        (HomeTab::Files, "folder", tl!("Files")),
        (HomeTab::Create, "file-plus", tl!("Create")),
        (HomeTab::Tools, "layout-grid", tl!("Tools")),
    ];
    let mut picked = None;
    let look = look(t);
    phone::bar_filled(ui, "phone-home-tabs", rect, look.bar, Some(look.edge), false, |ui| {
        let w = (ui.available_width() / tabs.len() as f32).floor();
        for (tab, icon, label) in tabs {
            let selected = app.phone.home.tab == tab;
            let (r, resp) = ui.allocate_exact_size(vec2(w, NAV), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, label));
            let press = theme::Press::track(ui, &resp);
            let body = r.translate(press.offset());
            // Material's indicator: a pill behind the chosen tab's icon.
            let pill = Rect::from_center_size(body.center_top() + vec2(0.0, 20.0), vec2(56.0, 30.0));
            if selected {
                phone::vivid_tile(ui.painter(), pill, 15.0, t.accent);
            } else {
                press.wash(ui, pill, 15, false);
            }
            if selected {
                phone::tile_glyph(ui, pill, icon, 21.0);
            } else {
                icons::paint(ui, pill, icon, 22.0, t.icon);
            }
            let text = if selected { t.accent_text } else { t.text_muted };
            ui.painter().text(pos2(body.center().x, pill.bottom() + 11.0), Align2::CENTER_CENTER, label, theme::medium(12.0), text);
            if theme::hand(resp).clicked() {
                picked = Some(tab);
            }
        }
    });
    if let Some(tab) = picked {
        app.phone.home.tab = tab;
        app.phone.home.search = None;
    }
}

/// The floating "+" over the bottom right of `body`.
fn fab(app: &mut PdfCraftApp, ui: &mut egui::Ui, body: Rect, t: &Tokens) {
    let fab = Rect::from_min_size(body.right_bottom() - vec2(FAB + 18.0, FAB + 18.0), Vec2::splat(FAB));
    let label = phone::command_label(app, "file.open");
    let mut tapped = false;
    let fade = ui.opacity();
    egui::Area::new(egui::Id::new("phone-fab")).order(egui::Order::Middle).fixed_pos(fab.min).constrain(false).show(ui.ctx(), |ui| {
        crate::transition::follow_screen(ui, fade);
        let (r, resp) = ui.allocate_exact_size(fab.size(), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
        let press = theme::Press::track(ui, &resp);
        let radius = FAB / 2.0;
        let shadow = egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(if t.dark() { 140 } else { 60 }) };
        ui.painter().add(shadow.as_shape(r, CornerRadius::same(radius as u8)));
        // The icon's green into the accent; darker while pressed.
        let deeper = |k: f32| t.brand.lerp_to_gamma(Color32::BLACK, k);
        let (from, to) = (press.fill(t.brand, deeper(0.08), deeper(0.18)), t.accent);
        gradient_disc(ui.painter(), r.center() + press.offset(), radius, from, to);
        icons::paint(ui, r.translate(press.offset()), "plus", 28.0, Color32::WHITE);
        tapped = theme::hand(resp).clicked();
    });
    if tapped {
        app.phone.sheet = Some(Sheet::New);
    }
}

// ---------------------------------------------------------------------------------------------
// Sheets

fn sheets(app: &mut PdfCraftApp, ui: &mut egui::Ui, screen: Rect, t: &Tokens) {
    match app.phone.sheet {
        Some(Sheet::New) => new_sheet(app, ui.ctx(), screen, t),
        Some(Sheet::Item) => item_sheet(app, ui.ctx(), screen, t),
        Some(Sheet::Pick) => pick_sheet(app, ui.ctx(), screen, t),
        _ => {}
    }
}

/// The "+": open, create, combine, and the actions started most.
fn new_sheet(app: &mut PdfCraftApp, ctx: &egui::Context, screen: Rect, t: &Tokens) {
    enum Pick {
        Command(&'static str),
        Then(Then),
        Create,
    }
    let mut pick = None;
    let combine = phone::command_label(app, "page.combine");
    let open = phone::sheet(ctx, "new", screen, None, |ui| {
        if action_row(ui, "folder-open", t.brand, tl!("Open file"), tl!("A PDF on this device."), t).clicked() {
            pick = Some(Pick::Command("file.open"));
        }
        if action_row(ui, "file-plus", phone::tool_color("create"), tl!("Create a PDF"), tl!("From photos, scans or other files."), t).clicked() {
            pick = Some(Pick::Create);
        }
        if action_row(ui, "files", phone::tool_color("combine"), &combine, tl!("Several PDFs and pictures in one document."), t).clicked() {
            pick = Some(Pick::Command("page.combine"));
        }
        #[cfg(not(target_arch = "wasm32"))]
        if action_row(ui, "sparkles", phone::tool_color("ai"), tl!("AI assistant"), tl!("Summarize, ask and translate."), t).clicked() {
            pick = Some(Pick::Then(Then::Assistant));
        }
        if action_row(ui, "file-pen-line", phone::tool_color("edit"), tl!("Edit a PDF"), tl!("Change its text and pictures."), t).clicked() {
            pick = Some(Pick::Then(Then::Tool("edit")));
        }
        if action_row(ui, "signature", phone::tool_color("fill_sign"), tl!("Fill & Sign"), tl!("Fill in a form and sign it."), t).clicked() {
            pick = Some(Pick::Then(Then::Tool("fill_sign")));
        }
    });
    if !open || pick.is_some() {
        app.phone.sheet = None;
    }
    match pick {
        Some(Pick::Command(id)) => {
            app.execute(id);
        }
        Some(Pick::Create) => app.phone.home.tab = HomeTab::Create,
        Some(Pick::Then(then)) => app.home_action(then),
        None => {}
    }
}

/// A file's ⋮ menu: open, star, and remove from Recent or close.
fn item_sheet(app: &mut PdfCraftApp, ctx: &egui::Context, screen: Rect, t: &Tokens) {
    let Some((path, listed_tab)) = app.phone.home.menu.clone() else {
        app.phone.sheet = None;
        return;
    };
    // A document without a file yet is listed under "doc:{tab}", with its tab.
    let file = !path.starts_with("doc:");
    let tab = if file {
        app.views.iter().position(|v| app.session.get(v.id).and_then(|d| d.path.as_deref()) == Some(path.as_str()))
    } else {
        listed_tab.filter(|i| *i < app.views.len())
    };
    let starred = app.starred.iter().any(|s| s == &path);
    let recent = app.recent.iter().any(|r| r.path == path);
    let name = tab
        .and_then(|i| app.views.get(i))
        .and_then(|v| app.session.get(v.id))
        .map(|d| d.display_name())
        .or_else(|| std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    #[derive(Clone, Copy)]
    enum Pick {
        Open,
        Star,
        Unstar,
        Forget,
        Close,
    }
    let mut pick = None;
    let open = phone::sheet(ctx, "item", screen, Some(&name), |ui| {
        if phone::sheet_row(ui, "book-open", tl!("Open"), true, None, t).clicked() {
            pick = Some(Pick::Open);
        }
        if file && !starred && phone::sheet_row(ui, "star", tl!("Add to Starred"), true, None, t).clicked() {
            pick = Some(Pick::Star);
        }
        if starred && phone::sheet_row(ui, "star", tl!("Remove from Starred"), true, None, t).clicked() {
            pick = Some(Pick::Unstar);
        }
        if recent && phone::sheet_row(ui, "trash-2", tl!("Remove from Recent"), true, None, t).clicked() {
            pick = Some(Pick::Forget);
        }
        if tab.is_some() && phone::sheet_row(ui, "x", tl!("Close"), true, None, t).clicked() {
            pick = Some(Pick::Close);
        }
    });
    if !open || pick.is_some() {
        app.phone.sheet = None;
        app.phone.home.menu = None;
    }
    match pick {
        Some(Pick::Open) => match tab {
            Some(i) => {
                app.active = Some(i);
                app.phone.bars_hidden = false;
            }
            None => app.open_listed(&path),
        },
        Some(Pick::Star) => {
            // Newest first; past the most kept, the oldest star goes.
            app.starred.insert(0, path);
            app.starred.truncate(MAX_STARRED);
        }
        Some(Pick::Unstar) => app.starred.retain(|s| s != &path),
        Some(Pick::Forget) => app.recent.retain(|r| r.path != path),
        Some(Pick::Close) => {
            if let Some(i) = tab {
                app.request_close_tab(i);
                // Closing from Home stays on Home.
                if app.close_request.is_none() {
                    app.active = None;
                }
            }
        }
        None => {}
    }
}

/// The most files Home keeps starred.
pub(crate) const MAX_STARRED: usize = 200;

/// Which document an action is for: one of the open ones, or another file.
fn pick_sheet(app: &mut PdfCraftApp, ctx: &egui::Context, screen: Rect, t: &Tokens) {
    let Some(then) = app.phone.home.then else {
        app.phone.sheet = None;
        return;
    };
    let docs: Vec<String> = app.views.iter().map(|v| app.session.get(v.id).map(|d| d.display_name()).unwrap_or_default()).collect();
    let mut pick: Option<Option<usize>> = None;
    let open = phone::sheet(ctx, "pick", screen, Some(tl!("Which document?")), |ui| {
        for (i, name) in docs.iter().enumerate() {
            if phone::sheet_row(ui, "file-text", name, true, None, t).clicked() {
                pick = Some(Some(i));
            }
        }
        if phone::sheet_row(ui, "folder-open", tl!("Open another file…"), true, None, t).clicked() {
            pick = Some(None);
        }
    });
    if !open || pick.is_some() {
        app.phone.sheet = None;
        app.phone.home.then = None;
    }
    match pick {
        Some(Some(i)) => {
            app.active = Some(i);
            app.run_then(then);
        }
        Some(None) => app.open_then(then),
        None => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Profile

fn profile(app: &mut PdfCraftApp, ui: &mut egui::Ui, screen: Rect, t: &Tokens) {
    let top = Rect::from_min_size(screen.min, vec2(screen.width(), TOP_BAR));
    if phone::top_bar_titled(ui, top, tl!("Profile")) {
        app.phone.home.profile = false;
        app.phone.home.name_draft = None;
        // Home draws from the next frame.
        ui.ctx().request_repaint();
        return;
    }
    let photo = photo_texture(app, ui.ctx());
    let initials = initials(app.profile_name());
    // The name as typed, spaces and all: the one kept is trimmed. The system's default ("Guest",
    // a login name) isn't shown as a name to keep.
    let name_id = egui::Id::new("phone-profile-name");
    let typing = ui.memory(|m| m.has_focus(name_id));
    let kept = app.profile_name().unwrap_or_default();
    if !typing && app.phone.home.name_draft.as_deref().is_none_or(|d| d.trim() != kept) {
        // First shown, or the name changed elsewhere (Preferences) meanwhile.
        app.phone.home.name_draft = Some(kept.to_owned());
    }
    #[cfg(not(target_arch = "wasm32"))]
    let making = app.phone.home.photo_job.is_some();
    #[cfg(target_arch = "wasm32")]
    let making = false;
    // The browser build has no file picker that gives back a picture's bytes here.
    let can_choose = cfg!(not(target_arch = "wasm32"));
    let rest = Rect::from_min_max(pos2(screen.left(), top.bottom()), screen.max);
    let mut choose = false;
    let mut remove = false;
    let mut run = None;
    let mut theme_choice: Option<&'static str> = None;
    let mut dark_pages: Option<bool> = None;
    #[cfg(not(target_arch = "wasm32"))]
    let mut ai = false;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rest), |ui| {
        egui::ScrollArea::vertical().id_salt("phone-profile").auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::NONE.inner_margin(egui::Margin::symmetric(20, 18)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    let sense = if can_choose && !making { Sense::click() } else { Sense::hover() };
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(104.0), sense);
                    if can_choose {
                        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, !making, tl!("Choose a photo")));
                    }
                    avatar(ui, r, photo.as_ref(), &initials, t);
                    if making {
                        // The picture is being made small.
                        ui.painter().circle_filled(r.center(), r.width() / 2.0, Color32::from_black_alpha(110));
                        egui::Spinner::new().color(Color32::WHITE).paint_at(ui, Rect::from_center_size(r.center(), Vec2::splat(36.0)));
                    } else if can_choose {
                        // A camera on the photo's corner says it can be changed.
                        let badge = Rect::from_center_size(r.right_bottom() - vec2(14.0, 14.0), Vec2::splat(32.0));
                        ui.painter().circle_filled(badge.center(), 16.0, t.accent);
                        icons::paint(ui, badge, "camera", 16.0, Color32::WHITE);
                    }
                    choose |= theme::hand(resp).clicked();
                    ui.add_space(8.0);
                    if can_choose && !making {
                        ui.add_space(4.0);
                        choose |= pill(ui, "image", tl!("Choose a photo"), t).clicked();
                    }
                    if app.profile_photo.is_some() && !making {
                        ui.add_space(2.0);
                        let label = egui::RichText::new(tl!("Remove the photo")).font(theme::medium(14.5)).color(t.text_muted);
                        remove = ui.add(egui::Button::new(label).frame(false).min_size(vec2(0.0, TARGET))).clicked();
                    }
                });
                ui.add_space(18.0);
                let l = ui.label(egui::RichText::new(tl!("Your name")).font(theme::semibold(14.0)));
                let mut name = app.phone.home.name_draft.clone().unwrap_or_default();
                let r = ui
                    .add(
                        widgets::line(&mut name)
                            .id(name_id)
                            .hint_text(tl!("Your name"))
                            .desired_width(f32::INFINITY)
                            .char_limit(crate::MAX_AUTHOR_CHARS),
                    )
                    .labelled_by(l.id);
                if r.changed() {
                    // Emptied: comments are signed with the system's name again.
                    let kept = name.trim();
                    app.comment_prefs.author = if kept.is_empty() { crate::comments::login_name().to_owned() } else { kept.to_owned() };
                    app.phone.home.name_draft = Some(name);
                }
                ui.label(egui::RichText::new(tl!("It shows on Home and signs your comments.")).small().color(t.text_muted));
                ui.add_space(18.0);
                ui.label(egui::RichText::new(tl!("Appearance")).font(theme::semibold(14.0)));
                ui.add_space(6.0);
                let choices = [("contrast", tl!("Automatic")), ("sun", tl!("Light")), ("moon", tl!("Dark"))];
                let taken = match app.theme_preference {
                    crate::theme::ThemePreference::System => 0,
                    crate::theme::ThemePreference::Light => 1,
                    crate::theme::ThemePreference::Dark => 2,
                };
                if let Some(i) = segmented(ui, &choices, taken, t) {
                    theme_choice = Some(["view.theme.system", "view.theme.light", "view.theme.dark"].get(i).copied().unwrap_or("view.theme.system"));
                }
                ui.add_space(6.0);
                if switch_row(ui, tl!("Dark pages"), tl!("Light text on a dark page, for reading at night."), app.dark_pages(), t).clicked() {
                    dark_pages = Some(!app.dark_pages());
                }
                ui.add_space(8.0);
            });
            if phone::sheet_row(ui, "settings", &phone::command_label(app, "app.preferences"), true, None, t).clicked() {
                run = Some("app.preferences");
            }
            #[cfg(not(target_arch = "wasm32"))]
            if phone::sheet_row(ui, "sparkles", tl!("AI assistant"), true, None, t).clicked() {
                ai = true;
            }
            if phone::sheet_row(ui, "info", &phone::command_label(app, "help.about"), true, None, t).clicked() {
                run = Some("help.about");
            }
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(format!("PdfCraft · {}", crate::i18n::fmt(tl!("Version {v}"), &[("v", env!("CARGO_PKG_VERSION"))])))
                        .small()
                        .color(t.text_faint),
                );
            });
        });
    });
    // The grid of the device's photos, as phone apps choose one (the folders are a tap away there).
    #[cfg(not(target_arch = "wasm32"))]
    if choose {
        app.phone.home.picking = true;
    }
    if remove {
        app.profile_photo = None;
    }
    if let Some(command) = theme_choice {
        app.execute(command);
    }
    if let Some(on) = dark_pages {
        app.set_dark_pages(on);
    }
    // The settings open over the profile, which Back shows again.
    #[cfg(not(target_arch = "wasm32"))]
    if ai {
        app.phone.home.ai_settings = true;
    }
    if let Some(id) = run {
        app.execute(id);
    }
}

/// Choose a picture for the profile in the file browser (the photo grid's "Other folders…").
#[cfg(not(target_arch = "wasm32"))]
fn choose_photo(app: &mut PdfCraftApp) {
    let dialog = crate::file_dialog::AsyncFileDialog::new().add_filter(tl!("Pictures"), &["png", "jpg", "jpeg"]);
    app.ask_one(crate::pickers::Ask::File(dialog), None, start_photo);
}

/// Make the picture at `path` the profile photo: its centred square, made small on a worker
/// thread (a phone's photo takes a moment to decode), is kept in the settings.
#[cfg(not(target_arch = "wasm32"))]
fn start_photo(app: &mut PdfCraftApp, path: std::path::PathBuf) {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("pdfcraft-photo".into()).spawn(move || {
        let _ = tx.send(make_photo(&path));
    });
    match spawned {
        Ok(_) => app.phone.home.photo_job = Some(rx),
        Err(e) => app.notify(crate::i18n::fmt(tl!("Couldn't use this picture: {e}"), &[("e", &e.to_string())])),
    }
}

/// The side of a photo's tile in the grid, in pixels.
#[cfg(not(target_arch = "wasm32"))]
const PHOTO_TILE_PX: u32 = 192;

/// A photo's tile: its centred square, small, premultiplied as the previews are uploaded.
#[cfg(not(target_arch = "wasm32"))]
fn photo_tile(path: &str) -> Option<Pixels> {
    use std::io::Read as _;
    let limit = pdfcraft_engine::signature_image::MAX_PROFILE_SOURCE_BYTES;
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
    let (w, h, mut rgba) = pdfcraft_engine::signature_image::picture_thumbnail(&bytes, PHOTO_TILE_PX)?;
    for [r, g, b, a] in rgba.as_chunks_mut::<4>().0 {
        let alpha = u16::from(*a);
        for c in [r, g, b] {
            // At most 255 · 255 + 127 before dividing: a u16 holds it.
            *c = ((u16::from(*c) * alpha + 127) / 255) as u8;
        }
    }
    Some((w, h, rgba))
}

/// The folders the photo grid looks in: [PdfCraftApp::photo_folders], or the device's camera
/// folder, Pictures and Download.
#[cfg(not(target_arch = "wasm32"))]
fn photo_roots(app: &PdfCraftApp) -> Vec<std::path::PathBuf> {
    if !app.photo_folders.is_empty() {
        return app.photo_folders.clone();
    }
    #[cfg(any(test, target_os = "android"))]
    {
        use crate::file_browser::PlaceKind;
        app.file_browser_host
            .places
            .iter()
            .filter(|p| matches!(p.kind, PlaceKind::Storage))
            .flat_map(|p| ["DCIM", "Pictures", "Download"].map(|f| p.path.join(f)))
            .collect()
    }
    #[cfg(not(any(test, target_os = "android")))]
    Vec::new()
}

/// The device's photos in a grid, newest first, to choose the profile's; the folders are a row
/// away.
#[cfg(not(target_arch = "wasm32"))]
fn photo_grid(app: &mut PdfCraftApp, ui: &mut egui::Ui, screen: Rect, t: &Tokens) {
    let top = Rect::from_min_size(screen.min, vec2(screen.width(), TOP_BAR));
    if phone::top_bar_titled(ui, top, tl!("Choose a photo")) {
        app.phone.home.leave_grid();
        ui.ctx().request_repaint();
        return;
    }
    let rest = Rect::from_min_max(pos2(screen.left(), top.bottom()), screen.max);
    ui.painter().rect_filled(rest, CornerRadius::ZERO, look(t).ground);
    let now = ui.input(|i| i.time);
    let roots = photo_roots(app);
    let access = app.home_photos_access(now);
    let (scan, tiles) = app.phone.home.photos.get_or_insert_with(|| (DeviceScan::of(Wanted::Pictures), Thumbs::new(photo_tile)));
    scan.poll();
    scan.want(roots, ui.ctx());
    tiles.poll(ui.ctx(), now);
    tiles.begin_frame();
    let scanning = scan.scanning();
    let photos: Vec<(String, String)> = scan.files.iter().map(|f| (f.path.clone(), f.name.clone())).collect();
    let mut chosen = None;
    let mut browse = false;
    let mut allow = false;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rest), |ui| {
        egui::ScrollArea::vertical().id_salt("phone-photos").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            browse = phone::sheet_row(ui, "folder-open", tl!("Other folders…"), true, None, t).clicked();
            ui.add_space(6.0);
            if photos.is_empty() {
                let (title, text) = if scanning {
                    (tl!("Looking for photos…"), "")
                } else if access == Some(false) {
                    (tl!("See your photos"), tl!("Allow PdfCraft to see your photos, and choose one here."))
                } else {
                    (tl!("No photos found."), tl!("PdfCraft shows the photos in Camera, Pictures and Download."))
                };
                egui::Frame::NONE.inner_margin(egui::Margin { left: 28, right: 28, top: 28, bottom: 8 }).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add(icons::image("image", 40.0, t.text_faint));
                        ui.add_space(10.0);
                        ui.label(egui::RichText::new(title).font(theme::semibold(15.5)).color(t.text));
                        if !text.is_empty() {
                            ui.add_space(4.0);
                            ui.add(egui::Label::new(egui::RichText::new(text).color(t.text_muted)).wrap());
                        }
                        if access == Some(false) && !scanning {
                            ui.add_space(16.0);
                            allow = pill(ui, "image", tl!("Allow access"), t).clicked();
                        }
                    });
                });
                return;
            }
            // Three to a row, a hair apart, as phone galleries show them.
            const GAP: f32 = 3.0;
            let side = ((ui.available_width() - 2.0 * GAP) / 3.0).floor().max(1.0);
            for row in photos.chunks(3) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    for (path, name) in row {
                        let (cell, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
                        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
                        // Only the tiles on screen are made.
                        let tile = if ui.is_rect_visible(cell) {
                            app.phone.home.photos.as_mut().and_then(|(_, tiles)| tiles.get(path, now, ui.ctx()))
                        } else {
                            None
                        };
                        let press = theme::Press::track(ui, &resp);
                        let body = cell.shrink(press.down * 3.0);
                        match tile {
                            Some(tex) => {
                                egui::Image::new((tex.id(), body.size())).paint_at(ui, body);
                            }
                            None => {
                                ui.painter().rect_filled(body, CornerRadius::ZERO, look(t).surface);
                                icons::paint(ui, body, "image", 24.0, t.text_faint);
                            }
                        }
                        if theme::hand(resp).clicked() {
                            chosen = Some(path.clone());
                        }
                    }
                });
                ui.add_space(GAP);
            }
        });
    });
    if browse {
        app.phone.home.leave_grid();
        choose_photo(app);
    }
    if allow {
        ask_photos_access(app);
    }
    if let Some(path) = chosen {
        app.phone.home.leave_grid();
        start_photo(app, std::path::PathBuf::from(path));
    }
}

/// Why a picture couldn't become the profile photo.
#[cfg(not(target_arch = "wasm32"))]
enum PhotoError {
    /// Not a PNG or JPEG.
    Format,
    /// Too large a file or picture.
    Size,
    /// Anything else, in the system's or the decoder's words.
    Other(String),
    /// The worker ended without an answer.
    Stopped,
}

/// The profile photo made from the picture at `path`.
#[cfg(not(target_arch = "wasm32"))]
fn make_photo(path: &std::path::Path) -> Result<Vec<u8>, PhotoError> {
    use pdfcraft_engine::signature_image::{MAX_PROFILE_SOURCE_BYTES, SignatureImageError, profile_picture};
    use std::io::Read as _;
    let mut bytes = Vec::new();
    // One byte past the limit is enough to know the file is too large.
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_PROFILE_SOURCE_BYTES as u64 + 1).read_to_end(&mut bytes))
        .map_err(|e| PhotoError::Other(e.to_string()))?;
    match pdfcraft_engine::guard(|| profile_picture(&bytes)) {
        Ok(Ok(png)) => Ok(png),
        Ok(Err(SignatureImageError::Format)) => Err(PhotoError::Format),
        Ok(Err(SignatureImageError::Size | SignatureImageError::PlacedSize)) => Err(PhotoError::Size),
        // The cause, not the signature image's sentence around it.
        Ok(Err(SignatureImageError::Read(e))) => Err(PhotoError::Other(e.to_string())),
        Ok(Err(SignatureImageError::Decode(e))) => Err(PhotoError::Other(e.to_string())),
        Err(e) => Err(PhotoError::Other(e.to_string())),
    }
}

/// Take the profile photo once it's made.
#[cfg(not(target_arch = "wasm32"))]
fn poll_photo(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let Some(rx) = app.phone.home.photo_job.as_ref() else { return };
    let made = match rx.try_recv() {
        Ok(made) => made,
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            // The worker can't wake the screen: look again shortly.
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(PhotoError::Stopped),
    };
    app.phone.home.photo_job = None;
    match made {
        Ok(png) => app.profile_photo = Some(Arc::new(png)),
        Err(PhotoError::Format) => app.notify(tl!("Choose a PNG or JPEG picture.").to_owned()),
        Err(PhotoError::Size) => app.notify(tl!("This picture is too large.").to_owned()),
        Err(PhotoError::Stopped) => app.notify(tl!("Couldn't use this picture.").to_owned()),
        // A literal, so the catalogs' tests see it.
        Err(PhotoError::Other(e)) => app.notify(crate::i18n::fmt(tl!("Couldn't use this picture: {e}"), &[("e", &e)])),
    }
}

// ---------------------------------------------------------------------------------------------
// Thumbnails and the device's PDFs (not in the browser: no threads, no files)

/// The in-app browser's places (Android): their names, icons and folders.
fn places(app: &PdfCraftApp) -> Vec<(String, &'static str, std::path::PathBuf)> {
    #[cfg(all(not(target_arch = "wasm32"), any(test, target_os = "android")))]
    return app.file_browser_host.places.iter().map(|p| (p.kind.label(), p.kind.icon(), p.path.clone())).collect();
    #[cfg(not(all(not(target_arch = "wasm32"), any(test, target_os = "android"))))]
    {
        let _ = app;
        Vec::new()
    }
}

/// Whether PdfCraft may read the device's shared storage; None where it needs no permission.
fn files_access(app: &PdfCraftApp) -> Option<bool> {
    #[cfg(all(not(target_arch = "wasm32"), any(test, target_os = "android")))]
    return app.file_browser_host.access_granted.as_ref().map(|granted| granted());
    #[cfg(not(all(not(target_arch = "wasm32"), any(test, target_os = "android"))))]
    {
        let _ = app;
        None
    }
}

/// Whether PdfCraft may read the device's photos; None where it needs no permission.
fn photos_access(app: &PdfCraftApp) -> Option<bool> {
    #[cfg(all(not(target_arch = "wasm32"), any(test, target_os = "android")))]
    return app.file_browser_host.photos_granted.as_ref().map(|granted| granted());
    #[cfg(not(all(not(target_arch = "wasm32"), any(test, target_os = "android"))))]
    {
        let _ = app;
        None
    }
}

/// Ask for it: the system's own question.
#[cfg(not(target_arch = "wasm32"))]
fn ask_photos_access(app: &PdfCraftApp) {
    #[cfg(any(test, target_os = "android"))]
    if let Some(ask) = app.file_browser_host.request_photos.clone() {
        ask();
    }
    #[cfg(not(any(test, target_os = "android")))]
    let _ = app;
}

/// Show the system page where the user allows that access.
fn ask_files_access(app: &PdfCraftApp) {
    #[cfg(all(not(target_arch = "wasm32"), any(test, target_os = "android")))]
    if let Some(ask) = app.file_browser_host.request_access.clone() {
        ask();
    }
    #[cfg(not(all(not(target_arch = "wasm32"), any(test, target_os = "android"))))]
    let _ = app;
}

/// The folders Home looks in for PDFs: [PdfCraftApp::device_folders], or the in-app browser's
/// Download, Documents and PdfCraft's own folder.
#[cfg(not(target_arch = "wasm32"))]
fn scan_roots(app: &PdfCraftApp) -> Vec<std::path::PathBuf> {
    if !app.device_folders.is_empty() {
        return app.device_folders.clone();
    }
    #[cfg(any(test, target_os = "android"))]
    {
        use crate::file_browser::PlaceKind;
        app.file_browser_host
            .places
            .iter()
            .filter(|p| matches!(p.kind, PlaceKind::Downloads | PlaceKind::Documents | PlaceKind::AppFiles))
            .map(|p| p.path.clone())
            .collect()
    }
    #[cfg(not(any(test, target_os = "android")))]
    Vec::new()
}

/// The longest side of a thumbnail, in pixels.
#[cfg(not(target_arch = "wasm32"))]
const THUMB_PX: f32 = 180.0;
/// The scale a thumbnail is rendered at first: Letter and A4 pages come out about [`THUMB_PX`]
/// tall, so most files need one render.
#[cfg(not(target_arch = "wasm32"))]
const THUMB_SCALE: f32 = 0.23;
/// The largest file a thumbnail is made for (a larger one shows the mark): it is read whole,
/// up to three at once, so less on a phone.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
const MAX_THUMB_FILE: u64 = 64 << 20;
#[cfg(target_os = "android")]
const MAX_THUMB_FILE: u64 = 16 << 20;
/// The most thumbnails kept: past it, the ones shown longest ago go.
#[cfg(not(target_arch = "wasm32"))]
const MAX_THUMBS: usize = 300;
/// How long one preview may take before its file shows the mark instead.
#[cfg(not(target_arch = "wasm32"))]
const THUMB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// The most previews rendering at once: the one waited for, and those given up on that still run.
/// A render stuck for good keeps its thread; past this many, no more previews are made this
/// session and files show the mark (nothing else waits on them).
#[cfg(not(target_arch = "wasm32"))]
const MAX_RENDERS: usize = 3;
/// How often a shown preview's file is looked at again, so a changed file shows its new page.
#[cfg(not(target_arch = "wasm32"))]
const RESTAT_SECS: f64 = 5.0;

/// A preview's pixels: width, height and RGBA (premultiplied).
#[cfg(not(target_arch = "wasm32"))]
type Pixels = (usize, usize, Vec<u8>);

/// What a preview was made from: the file's size and when it last changed (`None`: no file).
#[cfg(not(target_arch = "wasm32"))]
type Stamp = Option<(u64, Option<std::time::SystemTime>)>;

/// Up to [`RESTAT_SECS`], the same for a path every time: when in its period it is looked at.
#[cfg(not(target_arch = "wasm32"))]
fn stagger(path: &str) -> f64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    (hasher.finish() % 1000) as f64 / 1000.0 * RESTAT_SECS
}

#[cfg(not(target_arch = "wasm32"))]
fn stamp(path: &str) -> Stamp {
    std::fs::metadata(path).ok().map(|m| (m.len(), m.modified().ok()))
}

/// A file's preview.
#[cfg(not(target_arch = "wasm32"))]
struct Thumb {
    /// `None`: the file can't show one.
    tex: Option<TextureHandle>,
    stamp: Stamp,
    /// The frame it was last shown in, and when its file was last looked at.
    seen: u64,
    checked: f64,
}

/// The files waiting for a preview, shared with the worker thread.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct Queue {
    jobs: std::collections::VecDeque<String>,
    /// Home is gone: the worker ends.
    closed: bool,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct Shared {
    queue: std::sync::Mutex<Queue>,
    wake: std::sync::Condvar,
}

#[cfg(not(target_arch = "wasm32"))]
impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        // A thread that panicked holding it leaves a queue that is still fine to use.
        self.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Previews of the rows on screen (a PDF's first page, a photo's tile), made one after another on
/// a worker thread.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Thumbs {
    /// What makes a file's preview.
    make: fn(&str) -> Option<Pixels>,
    ready: std::collections::HashMap<String, Thumb>,
    /// Queued or being made.
    asked: HashSet<String>,
    /// The files whose rows showed since the frame began.
    shown: HashSet<String>,
    frame: u64,
    shared: Option<Arc<Shared>>,
    done: Option<std::sync::mpsc::Receiver<(String, Stamp, Option<Pixels>)>>,
    /// No worker thread could start: every file shows the mark.
    failed: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for Thumbs {
    /// PDFs' first pages.
    fn default() -> Self {
        Self::new(first_page)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Thumbs {
    fn new(make: fn(&str) -> Option<Pixels>) -> Self {
        Self {
            make,
            ready: std::collections::HashMap::new(),
            asked: HashSet::new(),
            shown: HashSet::new(),
            frame: 0,
            shared: None,
            done: None,
            failed: false,
        }
    }

    /// PDFs' first pages, for a list of files other than Home's own (the file browser).
    #[cfg(any(test, target_os = "android"))]
    pub(crate) fn pdfs() -> Self {
        Self::new(first_page)
    }

    /// Pictures' tiles (their centred squares).
    #[cfg(any(test, target_os = "android"))]
    pub(crate) fn pictures() -> Self {
        Self::new(photo_tile)
    }

    /// A new frame: files queued for rows no longer on screen leave the queue (scrolling past
    /// a hundred files doesn't render them all).
    pub(crate) fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        if let Some(shared) = &self.shared {
            let (shown, asked) = (&self.shown, &mut self.asked);
            shared.lock().jobs.retain(|path| {
                let keep = shown.contains(path);
                if !keep {
                    asked.remove(path);
                }
                keep
            });
        }
        self.shown.clear();
    }

    /// The preview of `path`, whose row is on screen: asked for when there's none yet, and again
    /// when the file changed (the old one shows until then).
    pub(crate) fn get(&mut self, path: &str, now: f64, ctx: &egui::Context) -> Option<TextureHandle> {
        // Each file at its own time: the rows on screen don't all look at their files in one frame.
        self.shown.insert(path.to_owned());
        let frame = self.frame;
        let shown = match self.ready.get_mut(path) {
            Some(t) => {
                t.seen = frame;
                let due = !(0.0..RESTAT_SECS).contains(&(now - t.checked));
                if !due {
                    return t.tex.clone();
                }
                // Every RESTAT_SECS from here: the phase set when it was made stays.
                t.checked = now;
                if stamp(path) == t.stamp {
                    return t.tex.clone();
                }
                t.tex.clone()
            }
            None => None,
        };
        self.ask(path, now, ctx);
        shown
    }

    fn ask(&mut self, path: &str, now: f64, ctx: &egui::Context) {
        if self.asked.contains(path) {
            return;
        }
        let shared = match (&self.shared, self.failed) {
            (Some(shared), _) => shared.clone(),
            (None, false) => {
                let shared = Arc::new(Shared::default());
                let (outbox, done) = std::sync::mpsc::channel();
                let (worker, wake, make) = (shared.clone(), ctx.clone(), self.make);
                let spawned = std::thread::Builder::new().name("pdfcraft-thumbs".into()).spawn(move || thumbs_worker(&worker, &outbox, &wake, make));
                if spawned.is_err() {
                    self.failed = true;
                    return self.ask(path, now, ctx);
                }
                self.shared = Some(shared.clone());
                self.done = Some(done);
                shared
            }
            (None, true) => {
                // No thread: the file shows the mark, looked at again like any other.
                self.ready.insert(path.to_owned(), Thumb { tex: None, stamp: stamp(path), seen: self.frame, checked: now - stagger(path) });
                return;
            }
        };
        shared.lock().jobs.push_back(path.to_owned());
        shared.wake.notify_one();
        self.asked.insert(path.to_owned());
    }

    /// Forget the files that couldn't show a preview, so they're tried again.
    fn forget_failures(&mut self) {
        self.ready.retain(|_, t| t.tex.is_some());
    }

    /// Take the previews made since last frame.
    pub(crate) fn poll(&mut self, ctx: &egui::Context, now: f64) {
        let Some(done) = self.done.as_ref() else { return };
        let mut made_any = false;
        while let Ok((path, stamp, made)) = done.try_recv() {
            self.asked.remove(&path);
            let tex = made.map(|(w, h, rgba)| {
                ctx.load_texture(format!("home-thumb-{path}"), egui::ColorImage::from_rgba_premultiplied([w, h], &rgba), egui::TextureOptions::LINEAR)
            });
            let checked = now - stagger(&path);
            self.ready.insert(path, Thumb { tex, stamp, seen: self.frame, checked });
            made_any = true;
        }
        if made_any && self.ready.len() > MAX_THUMBS {
            // Down to three quarters at once, not one every frame: the ones shown longest ago go.
            let mut seen: Vec<u64> = self.ready.values().map(|t| t.seen).collect();
            seen.sort_unstable();
            if let Some(&cut) = seen.get(self.ready.len().saturating_sub(MAX_THUMBS * 3 / 4)) {
                self.ready.retain(|_, t| t.seen >= cut);
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Thumbs {
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            shared.lock().closed = true;
            shared.wake.notify_all();
        }
    }
}

/// The thumbnails' worker: the queue's files in turn, until Home goes.
#[cfg(not(target_arch = "wasm32"))]
fn thumbs_worker(
    shared: &Shared,
    outbox: &std::sync::mpsc::Sender<(String, Stamp, Option<Pixels>)>,
    ctx: &egui::Context,
    make: fn(&str) -> Option<Pixels>,
) {
    let renders = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    loop {
        let path = {
            let mut queue = shared.lock();
            loop {
                if queue.closed {
                    return;
                }
                if let Some(path) = queue.jobs.pop_front() {
                    break path;
                }
                queue = shared.wake.wait(queue).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        // Previews given up on still render on their own threads: wait for them rather than
        // start more.
        while renders.load(std::sync::atomic::Ordering::Acquire) >= MAX_RENDERS {
            if shared.lock().closed {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        // Looked at before the file is read: a change while it renders is seen next time.
        let stamp = stamp(&path);
        let made = made_within(make, &path, &renders);
        if outbox.send((path, stamp, made)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

/// `make`'s preview of `path` on a thread of its own, given up on (the mark shows) after
/// [`THUMB_TIMEOUT`]. `renders` counts the threads still rendering.
#[cfg(not(target_arch = "wasm32"))]
fn made_within(make: fn(&str) -> Option<Pixels>, path: &str, renders: &Arc<std::sync::atomic::AtomicUsize>) -> Option<Pixels> {
    /// Counts a render thread down when it ends, however it ends.
    struct Running(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for Running {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        }
    }
    renders.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    let running = Running(renders.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    let owned = path.to_owned();
    let spawned = std::thread::Builder::new().name("pdfcraft-thumb".into()).spawn(move || {
        let _running = running;
        let _ = tx.send(make(&owned));
    });
    // Not started: the closure (and the count with it) is dropped.
    spawned.ok()?;
    rx.recv_timeout(THUMB_TIMEOUT).ok().flatten()
}

/// The first page of the PDF at `path`, about [`THUMB_PX`] on its longer side (RGBA,
/// premultiplied), or `None` for a file that can't show one (missing, too large, protected,
/// damaged).
#[cfg(not(target_arch = "wasm32"))]
fn first_page(path: &str) -> Option<Pixels> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(MAX_THUMB_FILE + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_THUMB_FILE || !bytes.starts_with(b"%PDF") && !bytes.windows(5).take(1024).any(|w| w == b"%PDF-") {
        return None;
    }
    let mut renderer = pdfcraft_render::PageRenderer::new(Arc::new(bytes), pdfcraft_render::RenderConfig::default());
    if renderer.page_count() == 0 {
        return None;
    }
    let request =
        |scale: f32| pdfcraft_render::RenderRequest { page: 0, kind: pdfcraft_render::RequestKind::Pixels, tile: None, scale, tag: 0, preview: true };
    // One render for nearly every page. A giant one (a 200-inch poster) comes out a few thousand
    // pixels first, within the renderer's own limits, then again at its size.
    let mut out = renderer.render(request(THUMB_SCALE));
    // A page far from Letter or A4 (a poster, a ticket) renders again at its size.
    let long = out.width.max(out.height) as f32;
    if out.error.is_none() && long > 0.0 && !(THUMB_PX / 2.0..=THUMB_PX * 2.0).contains(&long) {
        out = renderer.render(request((THUMB_SCALE * THUMB_PX / long).clamp(0.002, 4.0)));
    }
    let (w, h) = (out.width as usize, out.height as usize);
    let size = w.checked_mul(h)?.checked_mul(4)?;
    if out.error.is_some() || w == 0 || h == 0 || w > 1024 || h > 1024 || out.rgba.len() != size {
        return None;
    }
    Some((w, h, out.rgba.to_vec()))
}

/// A PDF found on the device.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub(crate) struct DeviceFile {
    pub(crate) path: String,
    pub(crate) name: String,
    /// The folder it is in.
    pub(crate) folder: String,
    pub(crate) size: usize,
    pub(crate) modified: Option<std::time::SystemTime>,
}

/// What a scan looks for.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Wanted {
    #[default]
    Pdfs,
    /// The pictures a profile photo can be made from.
    Pictures,
}

#[cfg(not(target_arch = "wasm32"))]
impl Wanted {
    /// Whether a file named `name` is one (by its extension).
    fn matches(self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        match self {
            Self::Pdfs => name.ends_with(".pdf"),
            Self::Pictures => [".jpg", ".jpeg", ".png"].iter().any(|e| name.ends_with(e)),
        }
    }
}

/// The device's PDFs (or pictures), looked for on a worker thread.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct DeviceScan {
    wanted: Wanted,
    files: Vec<DeviceFile>,
    /// The scan under way, and the folders it looks in.
    running: Option<(std::sync::mpsc::Receiver<Vec<DeviceFile>>, Vec<std::path::PathBuf>)>,
    /// When the last one ended, and over which folders.
    last: Option<(std::time::Instant, Vec<std::path::PathBuf>)>,
}

/// How long a scan's list stands before showing the list starts another.
#[cfg(not(target_arch = "wasm32"))]
const RESCAN_AFTER: std::time::Duration = std::time::Duration::from_secs(30);
/// The scan's limits: folders deep, folders visited, PDFs looked at, and PDFs listed (the newest).
#[cfg(not(target_arch = "wasm32"))]
const SCAN_DEPTH: usize = 4;
#[cfg(not(target_arch = "wasm32"))]
const SCAN_FOLDERS: usize = 4000;
#[cfg(not(target_arch = "wasm32"))]
const SCAN_SEEN: usize = 2000;
#[cfg(not(target_arch = "wasm32"))]
const SCAN_FILES: usize = 500;

#[cfg(not(target_arch = "wasm32"))]
impl DeviceScan {
    fn of(wanted: Wanted) -> Self {
        Self { wanted, ..Self::default() }
    }

    fn scanning(&self) -> bool {
        self.running.is_some()
    }

    /// Look in `roots` unless a scan of them is under way or recent.
    fn want(&mut self, roots: Vec<std::path::PathBuf>, ctx: &egui::Context) {
        if self.running.is_some() || roots.is_empty() {
            return;
        }
        if self.last.as_ref().is_some_and(|(at, scanned)| *scanned == roots && at.elapsed() < RESCAN_AFTER) {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let (for_thread, wanted) = (roots.clone(), self.wanted);
        let spawned = std::thread::Builder::new().name("pdfcraft-scan".into()).spawn(move || {
            let _ = tx.send(scan(&for_thread, wanted));
            ctx.request_repaint();
        });
        match spawned {
            Ok(_) => self.running = Some((rx, roots)),
            // No thread: tried again after a while, not every frame.
            Err(_) => self.last = Some((std::time::Instant::now(), roots)),
        }
    }

    fn poll(&mut self) {
        let Some((rx, _)) = self.running.as_ref() else { return };
        let files = match rx.try_recv() {
            Ok(files) => Some(files),
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        if let Some((_, roots)) = self.running.take() {
            // The wait before the next scan counts from this one's end.
            self.last = Some((std::time::Instant::now(), roots));
        }
        if let Some(files) = files {
            self.files = files;
        }
    }
}

/// The PDFs in `roots` and the folders under them (not hidden ones), the newest
/// [`SCAN_FILES`] of the first [`SCAN_SEEN`] found, newest first.
#[cfg(not(target_arch = "wasm32"))]
fn scan(roots: &[std::path::PathBuf], wanted: Wanted) -> Vec<DeviceFile> {
    let mut files = Vec::new();
    let mut folders = 0usize;
    let mut stack: Vec<(std::path::PathBuf, usize)> = roots.iter().map(|r| (r.clone(), 0)).collect();
    let mut seen = HashSet::new();
    while let Some((dir, depth)) = stack.pop() {
        if folders >= SCAN_FOLDERS || files.len() >= SCAN_SEEN {
            break;
        }
        // A folder reached twice (nested roots: Documents inside the storage) is read once. Links
        // to folders aren't followed (`file_type` doesn't follow them).
        if !seen.insert(std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone())) {
            continue;
        }
        folders += 1;
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let folder = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if depth < SCAN_DEPTH && stack.len() < SCAN_FOLDERS {
                    stack.push((entry.path(), depth + 1));
                }
            } else if files.len() < SCAN_SEEN && wanted.matches(&name) {
                let meta = entry.metadata().ok();
                files.push(DeviceFile {
                    path: entry.path().to_string_lossy().into_owned(),
                    name,
                    folder: folder.clone(),
                    size: meta.as_ref().map_or(0, |m| usize::try_from(m.len()).unwrap_or(usize::MAX)),
                    modified: meta.and_then(|m| m.modified().ok()),
                });
            }
        }
    }
    files.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.name.cmp(&b.name)));
    files.truncate(SCAN_FILES);
    files
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn initials_take_the_first_two_words() {
        assert_eq!(initials(Some("ana maría diaz")), "AM");
        assert_eq!(initials(Some("  Zoé ")), "Z");
        assert_eq!(initials(None), "");
    }

    #[test]
    fn the_scan_finds_pdfs_newest_first_and_skips_hidden_folders() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Sub/Deeper")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join("a.pdf"), b"%PDF-1.7").unwrap();
        std::fs::write(dir.join("Sub/Deeper/b.PDF"), b"%PDF-1.7").unwrap();
        std::fs::write(dir.join(".hidden/c.pdf"), b"%PDF-1.7").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let mut names: Vec<String> = scan(std::slice::from_ref(&dir), Wanted::Pdfs).into_iter().map(|f| f.name).collect();
        names.sort();
        assert_eq!(names, ["a.pdf", "b.PDF"]);
        // The same folder twice is read once.
        assert_eq!(scan(&[dir.clone(), dir.clone()], Wanted::Pdfs).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_isnt_a_pdf_has_no_thumbnail() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fake.pdf");
        std::fs::write(&path, b"not a pdf").unwrap();
        assert_eq!(first_page(&path.to_string_lossy()), None);
        assert_eq!(first_page(&dir.join("missing.pdf").to_string_lossy()), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The wait before the next scan counts from the last one's end, and looking at the list
    /// doesn't restart it (it once did every frame, so no scan ever ran again).
    #[test]
    fn a_scan_is_done_again_only_once_its_time_is_up() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-rescan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.pdf"), b"%PDF-1.7").unwrap();
        let ctx = egui::Context::default();
        let mut device = DeviceScan::default();
        device.want(vec![dir.clone()], &ctx);
        assert!(device.scanning());
        for _ in 0..500 {
            device.poll();
            if !device.scanning() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(device.files.len(), 1);
        let ended = device.last.as_ref().map(|(at, _)| *at);
        std::thread::sleep(std::time::Duration::from_millis(20));
        device.poll();
        device.want(vec![dir.clone()], &ctx);
        assert!(!device.scanning(), "recent: not again");
        assert_eq!(device.last.as_ref().map(|(at, _)| *at), ended, "the list being looked at doesn't move the time");
        // Once its time is up, it runs again.
        device.last = device.last.take().and_then(|(at, roots)| Some((at.checked_sub(RESCAN_AFTER)?, roots)));
        device.want(vec![dir.clone()], &ctx);
        assert!(device.scanning());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that couldn't show a preview (unreadable before access was allowed) is tried
    /// again once forgotten; one that showed is kept.
    #[test]
    fn forgotten_failures_are_asked_for_again() {
        let mut thumbs = Thumbs::default();
        thumbs.shared = Some(Arc::new(Shared::default()));
        let ctx = egui::Context::default();
        let tex = ctx.load_texture("t", egui::ColorImage::filled([1, 1], Color32::WHITE), egui::TextureOptions::LINEAR);
        thumbs.ready.insert("/failed.pdf".into(), Thumb { tex: None, stamp: None, seen: 0, checked: 0.0 });
        thumbs.ready.insert("/shown.pdf".into(), Thumb { tex: Some(tex), stamp: None, seen: 0, checked: 0.0 });
        assert!(thumbs.get("/failed.pdf", 1.0, &ctx).is_none());
        assert!(!thumbs.asked.contains("/failed.pdf"), "a failure stands while its file doesn't change");
        thumbs.forget_failures();
        assert!(thumbs.get("/failed.pdf", 1.0, &ctx).is_none());
        assert!(thumbs.asked.contains("/failed.pdf"));
        assert!(thumbs.get("/shown.pdf", 1.0, &ctx).is_some());
    }

    /// The photos are asked for with the system's question; once the answer is yes, the grid looks
    /// for them again (they couldn't be read before).
    #[test]
    fn allowing_the_photos_looks_for_them_again() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let allowed = Arc::new(AtomicBool::new(false));
        let asked = Arc::new(AtomicBool::new(false));
        let mut app = PdfCraftApp::new();
        let (a, q) = (allowed.clone(), asked.clone());
        app.file_browser_host.photos_granted = Some(Arc::new(move || a.load(Ordering::SeqCst)));
        app.file_browser_host.request_photos = Some(Arc::new(move || q.store(true, Ordering::SeqCst)));
        assert_eq!(app.home_photos_access(0.0), Some(false));
        app.phone.home.photos = Some((DeviceScan::of(Wanted::Pictures), Thumbs::new(photo_tile)));
        ask_photos_access(&app);
        assert!(asked.load(Ordering::SeqCst), "the system's question");
        allowed.store(true, Ordering::SeqCst);
        assert_eq!(app.home_photos_access(0.5), Some(false), "not asked again within a second");
        assert_eq!(app.home_photos_access(1.5), Some(true));
        assert!(app.phone.home.photos.is_none(), "looked for again");
    }

    /// Previews queued for rows scrolled away leave the queue.
    #[test]
    fn previews_for_rows_no_longer_shown_leave_the_queue() {
        let mut thumbs = Thumbs::default();
        // A queue with no worker, so nothing is taken from it meanwhile.
        thumbs.shared = Some(Arc::new(Shared::default()));
        let ctx = egui::Context::default();
        thumbs.begin_frame();
        for path in ["/a.pdf", "/b.pdf", "/c.pdf"] {
            assert!(thumbs.get(path, 0.0, &ctx).is_none());
        }
        thumbs.begin_frame();
        // Scrolled: only b is still on screen.
        thumbs.get("/b.pdf", 0.1, &ctx);
        thumbs.begin_frame();
        let queued: Vec<String> = thumbs.shared.as_ref().map(|s| s.lock().jobs.iter().cloned().collect()).unwrap_or_default();
        assert_eq!(queued, ["/b.pdf"]);
        assert!(thumbs.asked.contains("/b.pdf") && !thumbs.asked.contains("/a.pdf"), "a and c can be asked for again");
    }
}
