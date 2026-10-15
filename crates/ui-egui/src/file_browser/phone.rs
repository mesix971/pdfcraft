//! The file browser as a phone screen.
//!
//! The same picks as the dialog (a file, several, a folder, a file to write), laid out as the
//! phone's other screens are: the whole screen, finger-sized rows showing each PDF's first page and
//! each picture, and the places to look in a row of chips across the top. A file is chosen with
//! one tap (no OK button), and a pick that wants PDFs starts on every PDF found on the device,
//! newest first, as phone readers do, rather than in one folder. Back goes back through the
//! folders opened on the way, then cancels.

use std::path::{Path, PathBuf};

use egui::{Align2, Color32, CornerRadius, Key, Rect, Sense, Stroke, TextureHandle, Vec2, pos2, vec2};

use super::{Browser, Entry, Kind, MAX_NAME_CHARS, Place, save_name, tail_fit};
use crate::phone::{self, TARGET, TOP_BAR};
use crate::phone_home::{self, Look, Thumbs};
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, i18n, icons, widgets};

/// A row of the list, and a file's preview in it.
const ROW: f32 = 72.0;
const THUMB: Vec2 = vec2(42.0, 54.0);
/// The row of places, and the row of file types.
const CHIPS: f32 = 56.0;
const FILTERS: f32 = 48.0;
/// The line saying which folder the list shows.
const FOLDER: f32 = 52.0;
/// The files shown as themselves (a tile of the picture).
const PICTURES: [&str; 3] = ["png", "jpg", "jpeg"];

/// Where the list comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The folder the browser stands in.
    Folder,
    /// Every PDF found on the device, newest first (Home's "On this device").
    Device,
    /// The files opened lately that are still there.
    Recent,
}

/// The phone screen's state, beside the dialog's.
pub(crate) struct PhoneBrowser {
    pub(crate) source: Source,
    /// The folders left on the way here: Back goes back to them, the last first.
    trail: Vec<PathBuf>,
    /// The files chosen so far (several files), from any folder or list.
    picked: Vec<PathBuf>,
    /// The search field's text while it shows, and whether it takes the keyboard on its first frame.
    search: Option<String>,
    focus_search: bool,
    /// The new folder's name takes the keyboard on its first frame.
    focus_name: bool,
    /// A text field had the keyboard at the end of last frame: the Escape that follows is its own.
    typing: bool,
    /// The chip chosen last frame: a newly chosen one scrolls into view.
    chip_shown: Option<String>,
    /// The recent files that are still there, looked up when the list first shows them.
    recent: Option<Vec<Entry>>,
    /// The previews of the PDFs and pictures shown, made while the browser shows.
    pdfs: Option<Thumbs>,
    pictures: Option<Thumbs>,
}

impl PhoneBrowser {
    pub(crate) fn new(kind: Kind, dialog: &super::Dialog) -> Self {
        // A pick of PDFs (nothing else) that names no folder starts on the device's PDFs: they
        // are all that list holds, so a pick that takes pictures too starts in a folder.
        let pdfs = matches!(kind, Kind::File | Kind::Files)
            && dialog.filters.first().is_some_and(|f| !f.extensions.is_empty() && f.extensions.iter().all(|e| e == "pdf"));
        Self {
            source: if pdfs && dialog.directory.is_none() { Source::Device } else { Source::Folder },
            trail: Vec::new(),
            picked: Vec::new(),
            search: None,
            focus_search: false,
            focus_name: false,
            typing: false,
            chip_shown: None,
            recent: None,
            pdfs: None,
            pictures: None,
        }
    }
}

/// One row of the list: a folder, or a file from wherever the list comes from.
struct Row {
    name: String,
    path: PathBuf,
    dir: bool,
    /// The folder it is in (the device's list), its size and when it last changed, as known.
    folder: Option<String>,
    size: Option<u64>,
    modified: Option<std::time::SystemTime>,
}

impl Row {
    /// "Download · 1.2 MB · yesterday", or what is known.
    fn detail(&self) -> String {
        let size = self.size.map(|n| crate::panels::human_size(usize::try_from(n).unwrap_or(usize::MAX)));
        [self.folder.clone(), size, self.modified.map(crate::combine_ui::ago)]
            .into_iter()
            .flatten()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// What a row shows before its name.
enum Preview {
    Folder,
    Pdf(Option<TextureHandle>),
    Picture(Option<TextureHandle>),
    Icon(&'static str),
}

impl Browser {
    /// Whether the device's PDFs and the recent files can answer this pick: one file or more, of
    /// a type PDFs are.
    fn takes_pdfs(&self) -> bool {
        matches!(self.kind, Kind::File | Kind::Files) && self.extensions().is_none_or(|e| e.iter().any(|x| x == "pdf"))
    }

    /// List `dir`, keeping the files picked so far.
    fn open_dir(&mut self, dir: PathBuf) {
        self.phone.source = Source::Folder;
        self.dir = dir;
        self.listed_at = None;
        self.replace = None;
        self.error = None;
        self.new_folder = None;
    }

    /// Go to `dir` from here: Back comes back.
    fn enter(&mut self, dir: PathBuf) {
        if self.phone.source == Source::Folder {
            self.phone.trail.push(self.dir.clone());
        }
        self.open_dir(dir);
    }

    /// Back: the search closes, or the new folder's name; then the folders opened on the way, last
    /// first; then the pick is cancelled (an empty answer).
    fn phone_back(&mut self) -> Option<Vec<PathBuf>> {
        if self.phone.search.take().is_some() || self.new_folder.take().is_some() {
            return None;
        }
        if let Some(dir) = self.phone.trail.pop() {
            self.open_dir(dir);
            return None;
        }
        Some(Vec::new())
    }

    /// The title: the one the pick asked for, or what is being chosen.
    fn phone_title(&self) -> String {
        if let Some(title) = &self.dialog.title {
            return title.clone();
        }
        match self.kind {
            Kind::File => tl!("Choose a file").to_owned(),
            Kind::Files => tl!("Choose files").to_owned(),
            Kind::Folder => tl!("Choose a folder").to_owned(),
            Kind::Save => tl!("Save").to_owned(),
        }
    }

    /// The colours at the top and bottom of the screen, for the system's bars.
    pub(crate) fn phone_bars(&self, t: &Tokens) -> (Color32, Color32) {
        let look = phone_home::look(t);
        (look.ground, if self.kind == Kind::File { look.ground } else { look.bar })
    }
}

/// Draw the browser as a phone screen. Returns the paths to hand back once the user chooses or
/// cancels (an empty list), `None` while it stays open.
pub(crate) fn page(app: &mut PdfCraftApp, b: &mut Browser, ctx: &egui::Context) -> Option<Vec<PathBuf>> {
    let t = Tokens::get(ctx);
    let look = phone_home::look(&t);
    b.refresh(ctx, &app.file_browser_host);
    let now = ctx.input(|i| i.time);
    if !b.takes_pdfs() && b.phone.source != Source::Folder {
        b.phone.source = Source::Folder;
    }
    let scanning = b.phone.source == Source::Device && app.look_for_device_pdfs(ctx);
    for previews in [b.phone.pdfs.as_mut(), b.phone.pictures.as_mut()].into_iter().flatten() {
        previews.begin_frame();
        previews.poll(ctx, now);
    }
    let mut answer = None;
    // Back (Android's back key arrives as Escape), unless a text field or a popup takes it.
    if !b.phone.typing && !egui::Popup::is_any_open(ctx) && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
        answer = b.phone_back();
    }
    let rows = rows(app, b);
    let host = &app.file_browser_host;
    let screen = ctx.content_rect();
    // The browser slides in from the right as it opens, and the list from the side a folder leads
    // to (into one from the right, back out from the left).
    let id = egui::Id::new("phone-file-browser");
    let arriving = crate::transition::appearing(ctx, id);
    let dx = arriving.offset(crate::transition::SLIDE);
    crate::transition::place_layer(ctx, egui::LayerId::new(egui::Order::Foreground, id), vec2(dx, 0.0));
    let depth = i32::try_from(b.phone.trail.len()).unwrap_or(i32::MAX);
    let moving = crate::transition::change(ctx, "phone-file-browser-list", (b.phone.source as u8, &b.dir), depth);
    // While the browser itself arrives, its list comes with it.
    let moving = if arriving.done < 1.0 { crate::transition::Arrival::IN_PLACE } else { moving };
    egui::Modal::new(id).frame(egui::Frame::NONE).backdrop_color(look.ground).show(ctx, |ui| {
        ui.set_min_size(screen.size());
        ui.set_max_size(screen.size());
        let whole = Rect::from_min_size(ui.max_rect().min, screen.size());
        crate::transition::bare_strip(ui, whole, dx, look.ground);
        ui.painter().rect_filled(whole, CornerRadius::ZERO, look.ground);
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let band = |top: f32, height: f32| Rect::from_min_size(pos2(whole.left(), top), vec2(whole.width(), height));
        let mut y = whole.top();
        if top_bar(ui, b, band(y, TOP_BAR), &t, &look) {
            answer = b.phone_back();
        }
        y += TOP_BAR;
        if b.takes_pdfs() || host.places.len() > 1 {
            places(ui, b, &host.places, band(y, CHIPS), &t, &look);
            y += CHIPS;
        }
        if b.dialog.filters.len() > 1 && matches!(b.kind, Kind::File | Kind::Files) {
            filters(ui, b, band(y, FILTERS), now, &t, &look);
            y += FILTERS;
        }
        if b.phone.source == Source::Folder {
            folder_line(ui, b, &host.places, band(y, FOLDER), &t);
            y += FOLDER;
        }
        let low = bottom_height(b).min((whole.bottom() - y).max(0.0));
        let list_rect = Rect::from_min_max(pos2(whole.left(), y), pos2(whole.right(), (whole.bottom() - low).max(y)));
        let shown = list_rect.translate(vec2(moving.offset(crate::transition::SLIDE), 0.0));
        let chosen = ui
            .scope(|ui| {
                ui.multiply_opacity(moving.opacity());
                list(ui, b, &rows, shown, scanning, host, now, &t, &look)
            })
            .inner;
        if let Some(chosen) = chosen {
            answer = Some(chosen);
        }
        if low > 0.0
            && let Some(chosen) = bottom(ui, b, band(whole.bottom() - low, low), &t, &look)
        {
            answer = Some(chosen);
        }
    });
    b.phone.typing = ctx.text_edit_focused();
    answer
}

/// The list's rows, the search applied.
fn rows(app: &PdfCraftApp, b: &mut Browser) -> Vec<Row> {
    let mut rows: Vec<Row> = match b.phone.source {
        Source::Device => app
            .device_pdfs()
            .iter()
            .map(|f| Row {
                name: f.name.clone(),
                path: PathBuf::from(&f.path),
                dir: false,
                folder: Some(f.folder.clone()),
                size: u64::try_from(f.size).ok(),
                modified: f.modified,
            })
            .collect(),
        Source::Recent => {
            if b.phone.recent.is_none() {
                let extensions = b.extensions().map(<[String]>::to_vec);
                let still_there = app.recent.iter().filter_map(|r| {
                    let path = PathBuf::from(&r.path);
                    let meta = std::fs::metadata(&path).ok().filter(std::fs::Metadata::is_file)?;
                    super::matches_extension(&path, extensions.as_deref()).then(|| Entry {
                        name: r.name.clone(),
                        path,
                        dir: false,
                        size: meta.len(),
                        modified: meta.modified().ok(),
                    })
                });
                b.phone.recent = Some(still_there.collect());
            }
            b.phone
                .recent
                .iter()
                .flatten()
                .map(|e| Row { name: e.name.clone(), path: e.path.clone(), dir: false, folder: None, size: Some(e.size), modified: e.modified })
                .collect()
        }
        Source::Folder => match &b.listing {
            Ok(entries) => entries
                .iter()
                .map(|e| Row {
                    name: e.name.clone(),
                    path: e.path.clone(),
                    dir: e.dir,
                    folder: None,
                    size: (!e.dir).then_some(e.size),
                    modified: if e.dir { None } else { e.modified },
                })
                .collect(),
            Err(_) => Vec::new(),
        },
    };
    if let Some(query) = b.phone.search.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let query = query.to_lowercase();
        rows.retain(|r| r.name.to_lowercase().contains(&query));
    }
    rows
}

/// Back, the title (or the search field) and search. Returns whether Back was tapped.
fn top_bar(ui: &mut egui::Ui, b: &mut Browser, rect: Rect, t: &Tokens, look: &Look) -> bool {
    let mut back = false;
    let mut search = false;
    let title = b.phone_title();
    let searching = b.phone.search.is_some();
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(4.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        back = icons::button(ui, "chevron-left", TARGET, false, tl!("Back")).clicked();
        let room = (ui.available_width() - TARGET).max(0.0);
        match b.phone.search.as_mut() {
            Some(query) => {
                let edit = egui::TextEdit::singleline(query)
                    .id(egui::Id::new("phone-browser-search"))
                    .hint_text(tl!("Search files"))
                    .font(theme::regular(16.0))
                    .margin(egui::Margin::symmetric(14, 10))
                    .background_color(look.surface)
                    .desired_width((room - 8.0).max(0.0))
                    .char_limit(200);
                let r = ui.add(edit);
                if std::mem::take(&mut b.phone.focus_search) {
                    r.request_focus();
                }
                ui.add_space(8.0);
            }
            None => {
                let (r, said) = ui.allocate_exact_size(vec2(room, TOP_BAR), Sense::hover());
                said.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &title));
                widgets::row_text(ui, r.left_center() + vec2(8.0, 0.0), crate::bidi::visual(&title), theme::semibold(20.0), t.text, r.width() - 12.0);
            }
        }
        let (icon, label) = if searching { ("x", tl!("Close")) } else { ("search", tl!("Search")) };
        search = icons::button(ui, icon, TARGET, false, label).clicked();
    });
    if search {
        b.phone.search = if searching { None } else { Some(String::new()) };
        b.phone.focus_search = !searching;
    }
    back
}

/// Where to look: the device's PDFs and the recent files (for a pick of PDFs), then the places.
fn places(ui: &mut egui::Ui, b: &mut Browser, places: &[Place], rect: Rect, t: &Tokens, look: &Look) {
    enum Go {
        Source(Source),
        Place(PathBuf),
    }
    let mut go = None;
    let mut chosen = None;
    // The place the browser is in: the one whose folder holds it most closely.
    let here = (b.phone.source == Source::Folder)
        .then(|| {
            places.iter().enumerate().filter(|(_, p)| b.dir.starts_with(&p.path)).max_by_key(|(_, p)| p.path.components().count()).map(|(i, _)| i)
        })
        .flatten();
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        phone_home::swipe_row(ui, "phone-browser-places", |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(12.0);
                ui.spacing_mut().item_spacing.x = 8.0;
                let mut add = |ui: &mut egui::Ui, icon: &str, label: &str, on: bool, then: Go| {
                    let resp = chip(ui, icon, label, on, t, look);
                    if on {
                        chosen = Some((label.to_owned(), resp.clone()));
                    }
                    if resp.clicked() {
                        go = Some(then);
                    }
                };
                if b.takes_pdfs() {
                    add(ui, "file-search", tl!("On this device"), b.phone.source == Source::Device, Go::Source(Source::Device));
                    add(ui, "clock", tl!("Recent"), b.phone.source == Source::Recent, Go::Source(Source::Recent));
                }
                for (i, p) in places.iter().enumerate() {
                    add(ui, p.kind.icon(), &p.kind.label(), here == Some(i), Go::Place(p.path.clone()));
                }
                ui.add_space(4.0);
            });
        });
    });
    // A chip chosen some other way (a folder opened, a pick starting in Documents) comes into view.
    let label = chosen.as_ref().map(|(label, _)| label.clone());
    if label != b.phone.chip_shown {
        if let Some((_, resp)) = &chosen {
            resp.scroll_to_me(Some(egui::Align::Center));
        }
        b.phone.chip_shown = label;
    }
    match go {
        Some(Go::Source(source)) => {
            b.phone.source = source;
            b.phone.trail.clear();
            b.phone.recent = None;
        }
        Some(Go::Place(dir)) => {
            b.phone.trail.clear();
            b.open_dir(dir);
        }
        None => {}
    }
}

/// The file types the pick offers, and every file.
fn filters(ui: &mut egui::Ui, b: &mut Browser, rect: Rect, now: f64, t: &Tokens, look: &Look) {
    let all = b.dialog.filters.len();
    let mut picked = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        phone_home::swipe_row(ui, "phone-browser-filters", |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(12.0);
                ui.spacing_mut().item_spacing.x = 8.0;
                for i in 0..=all {
                    let label = b.dialog.filters.get(i).map_or_else(|| tl!("All files").to_owned(), |f| f.name.clone());
                    if small_chip(ui, &label, b.filter == i, t, look).clicked() {
                        picked = Some(i);
                    }
                }
                ui.add_space(4.0);
            });
        });
    });
    if let Some(i) = picked.filter(|i| *i != b.filter) {
        b.filter = i;
        b.relist(now);
        b.phone.recent = None;
    }
}

/// Up, and the folder the list shows: a place by its name, another folder by its own, with its
/// path under it.
fn folder_line(ui: &mut egui::Ui, b: &mut Browser, places: &[Place], rect: Rect, t: &Tokens) {
    let parent = b.dir.parent().map(Path::to_path_buf);
    let name = places
        .iter()
        .find(|p| p.path == b.dir)
        .map(|p| p.kind.label())
        .or_else(|| b.dir.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| b.dir.to_string_lossy().into_owned());
    let path = b.dir.to_string_lossy().into_owned();
    let mut up = false;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(4.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if parent.is_some() {
            up = icons::button(ui, "chevron-up", TARGET, false, tl!("Up")).clicked();
        } else {
            ui.add_space(12.0);
        }
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), FOLDER), Sense::hover());
        let width = (r.width() - 12.0).max(0.0);
        widgets::row_text(ui, r.left_center() + vec2(4.0, -9.0), crate::bidi::visual(&name), theme::semibold(16.0), t.text, width);
        let font = theme::regular(11.5);
        // The end of a long path is the part that tells folders apart: cut from the front.
        let shown = tail_fit(&path, width, |s| widgets::text_width(ui, s, &font));
        ui.painter().text(r.left_center() + vec2(4.0, 11.0), Align2::LEFT_CENTER, shown, font, t.text_faint);
    });
    if up && let Some(parent) = parent {
        b.enter(parent);
    }
}

/// The rows, or what the list says when it has none. Returns the paths to hand back when a tap
/// chose a file.
#[allow(clippy::too_many_arguments)]
fn list(
    ui: &mut egui::Ui,
    b: &mut Browser,
    rows: &[Row],
    rect: Rect,
    scanning: bool,
    host: &super::Host,
    now: f64,
    t: &Tokens,
    look: &Look,
) -> Option<Vec<PathBuf>> {
    let mut tapped = None;
    let mut allow = false;
    let mut browse = false;
    let ctx = ui.ctx().clone();
    // Each list its own scroll, from its top.
    let salt = (b.phone.source as u8, b.dir.clone(), b.filter);
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.set_clip_rect(rect.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical().id_salt(("phone-browser-list", salt)).auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if b.access.is_some_and(|(granted, _)| !granted) && host.request_access.is_some() {
                allow = access_card(ui, t, look);
            }
            if rows.is_empty() {
                browse = empty(ui, b, scanning, t);
            }
            for (i, r) in rows.iter().enumerate() {
                // Rows off screen only take their room: a folder of thousands lays out (and
                // previews) the ten shown.
                let next = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), ROW));
                if !ui.is_rect_visible(next) {
                    ui.allocate_space(next.size());
                    continue;
                }
                let preview = preview_of(b, r, now, &ctx);
                let picked = (b.kind == Kind::Files && !r.dir).then(|| b.phone.picked.contains(&r.path));
                if row(ui, r, &preview, picked, t, look).clicked() {
                    tapped = Some(i);
                }
            }
            ui.add_space(24.0);
        });
    });
    if allow && let Some(ask) = host.request_access.clone() {
        ask();
    }
    if browse && let Some(first) = host.places.first() {
        b.phone.trail.clear();
        b.open_dir(first.path.clone());
    }
    let r = rows.get(tapped?)?;
    if r.dir {
        b.enter(r.path.clone());
        return None;
    }
    match b.kind {
        Kind::File => return Some(vec![r.path.clone()]),
        Kind::Files => {
            if let Some(at) = b.phone.picked.iter().position(|p| p == &r.path) {
                b.phone.picked.remove(at);
            } else {
                b.phone.picked.push(r.path.clone());
            }
        }
        Kind::Save => {
            b.name = r.name.chars().take(MAX_NAME_CHARS).collect();
            b.replace = None;
        }
        Kind::Folder => {}
    }
    None
}

/// A row's preview: a PDF's first page, a picture, or an icon for what the file is.
fn preview_of(b: &mut Browser, r: &Row, now: f64, ctx: &egui::Context) -> Preview {
    if r.dir {
        return Preview::Folder;
    }
    let ext = r.path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let path = r.path.to_string_lossy();
    match ext.as_str() {
        "pdf" => Preview::Pdf(b.phone.pdfs.get_or_insert_with(Thumbs::pdfs).get(&path, now, ctx)),
        e if PICTURES.contains(&e) => Preview::Picture(b.phone.pictures.get_or_insert_with(Thumbs::pictures).get(&path, now, ctx)),
        "tif" | "tiff" | "gif" | "bmp" | "jp2" | "j2k" | "jpx" => Preview::Icon("image"),
        "csv" => Preview::Icon("file-spreadsheet"),
        "xml" | "xfdf" | "fdf" | "xdp" => Preview::Icon("file-code"),
        "ttf" | "otf" => Preview::Icon("type"),
        _ => Preview::Icon("file-text"),
    }
}

/// One row: its preview, name and detail; a folder's chevron, or a check circle when several
/// files are being chosen (`picked`).
fn row(ui: &mut egui::Ui, r: &Row, preview: &Preview, picked: Option<bool>, t: &Tokens, look: &Look) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    match picked {
        Some(on) => resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, &r.name)),
        None => resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &r.name)),
    }
    let press = theme::Press::track(ui, &resp);
    press.wash(ui, rect.shrink2(vec2(6.0, 2.0)), 12, picked == Some(true));
    let body = rect.translate(press.offset());
    let lead = Rect::from_center_size(body.left_center() + vec2(16.0 + THUMB.x / 2.0, 0.0), THUMB);
    let tile = Rect::from_center_size(lead.center(), Vec2::splat(44.0));
    match preview {
        Preview::Folder => phone::icon_chip(ui, tile, "folder", 22.0, phone::icon_hue("folder"), t),
        Preview::Pdf(tex) => phone_home::thumbnail(ui, lead, tex.as_ref(), t),
        Preview::Picture(Some(tex)) => {
            egui::Image::new((tex.id(), tile.size())).corner_radius(CornerRadius::same(10)).paint_at(ui, tile);
        }
        Preview::Picture(None) => phone::icon_chip(ui, tile, "image", 22.0, phone::icon_hue("image"), t),
        Preview::Icon(icon) => phone::icon_chip(ui, tile, icon, 22.0, phone::icon_hue(icon), t),
    }
    let text_left = lead.right() + 14.0;
    let end = if r.dir || picked.is_some() { 48.0 } else { 14.0 };
    let width = (rect.right() - end - text_left).max(0.0);
    let name = crate::bidi::visual(&r.name).into_owned();
    let detail = r.detail();
    // For screen readers: the detail as the row's description (its name is its label).
    if !detail.is_empty() {
        let said = detail.clone();
        ui.ctx().accesskit_node_builder(resp.id, |node| node.set_description(said));
    }
    if detail.is_empty() {
        widgets::row_text(ui, pos2(text_left, body.center().y), name, theme::medium(15.5), t.text, width);
    } else {
        widgets::row_text(ui, pos2(text_left, body.center().y - 10.0), name, theme::medium(15.5), t.text, width);
        widgets::row_text(ui, pos2(text_left, body.center().y + 12.0), &detail, theme::regular(12.5), t.text_faint, width);
    }
    let mark = Rect::from_center_size(body.right_center() - vec2(28.0, 0.0), Vec2::splat(24.0));
    if r.dir {
        icons::paint(ui, mark, "chevron-right", 18.0, t.text_faint);
    }
    match picked {
        Some(true) => {
            ui.painter().circle_filled(mark.center(), 12.0, t.accent);
            icons::paint_bold(ui, mark, "check", 15.0, Color32::WHITE);
        }
        Some(false) => {
            ui.painter().circle_stroke(mark.center(), 11.0, Stroke::new(1.5, t.text_faint));
        }
        None => {}
    }
    ui.painter().hline(text_left..=rect.right() - 16.0, rect.bottom() - 0.5, Stroke::new(1.0, look.edge));
    theme::hand(resp)
}

/// What an empty list says, with a way on. Returns whether "Browse folders" was tapped.
fn empty(ui: &mut egui::Ui, b: &Browser, scanning: bool, t: &Tokens) -> bool {
    let searched = b.phone.search.as_deref().is_some_and(|q| !q.trim().is_empty());
    let browse_label = tl!("Browse folders");
    let failed;
    let (icon, title, text, browse): (&str, &str, &str, bool) = if searched {
        ("search", tl!("No files match."), "", false)
    } else {
        match b.phone.source {
            Source::Device if scanning => ("file-search", tl!("Looking for PDFs…"), "", false),
            Source::Device => ("file-search", tl!("No PDFs found."), tl!("PdfCraft looks in Download, Documents and its own folder."), true),
            Source::Recent => ("clock", tl!("No recent files yet."), tl!("The files you open show here."), true),
            Source::Folder => match &b.listing {
                Err(e) => {
                    let folder = b.dir.file_name().map_or_else(|| b.dir.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
                    failed = i18n::fmt(tl!("Couldn't open {name}: {e}"), &[("name", &folder), ("e", e)]);
                    ("folder", failed.as_str(), "", false)
                }
                Ok(_) => ("folder-open", tl!("This folder is empty"), "", false),
            },
        }
    };
    let mut tapped = false;
    egui::Frame::NONE.inner_margin(egui::Margin { left: 32, right: 32, top: 40, bottom: 16 }).show(ui, |ui| {
        ui.vertical_centered(|ui| {
            let (disc, _) = ui.allocate_exact_size(Vec2::splat(76.0), Sense::hover());
            let hue = phone::icon_hue(icon);
            phone_home::gradient_disc(
                ui.painter(),
                disc.center(),
                38.0,
                hue.lerp_to_gamma(Color32::WHITE, 0.15),
                hue.lerp_to_gamma(Color32::BLACK, 0.2),
            );
            phone::tile_glyph(ui, disc, icon, 34.0);
            ui.add_space(16.0);
            ui.add(egui::Label::new(egui::RichText::new(title).font(theme::semibold(17.0)).color(t.text)).wrap());
            if !text.is_empty() {
                ui.add_space(6.0);
                ui.add(egui::Label::new(egui::RichText::new(text).font(theme::regular(14.0)).color(t.text_muted)).wrap());
            }
            if browse {
                ui.add_space(18.0);
                tapped = phone::action_pill(ui, Some("folder-open"), browse_label, true, t).clicked();
            }
        });
    });
    tapped
}

/// Shared storage may not be read yet: why, and the way to allow it. Returns whether "Allow
/// access" was tapped.
fn access_card(ui: &mut egui::Ui, t: &Tokens, look: &Look) -> bool {
    let mut allow = false;
    egui::Frame::NONE.outer_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
        egui::Frame::NONE
            .fill(look.surface)
            .stroke(Stroke::new(1.0, look.edge))
            .corner_radius(CornerRadius::same(16))
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (chip, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                    phone::icon_chip(ui, chip, "lock-open", 20.0, phone::icon_hue("folder"), t);
                    ui.add_space(10.0);
                    let text = tl!("To open and save files anywhere on this device, allow PdfCraft to manage all files.");
                    ui.add(egui::Label::new(egui::RichText::new(text).font(theme::regular(14.0)).color(t.text)).wrap());
                });
                ui.add_space(10.0);
                allow = phone::action_pill(ui, Some("lock-open"), tl!("Allow access"), true, t).clicked();
            });
    });
    allow
}

/// The height of the panel along the bottom: none for one file (a tap chooses it).
fn bottom_height(b: &Browser) -> f32 {
    let message = if b.replace.is_some() || b.error.is_some() { 44.0 } else { 0.0 };
    match b.kind {
        Kind::File => 0.0,
        Kind::Files => 76.0,
        Kind::Folder => 76.0 + message,
        // The name and Save, then New folder.
        Kind::Save => 188.0 + message,
    }
}

/// The panel along the bottom: what several files, a file to write or a folder need to finish.
/// Returns the paths to hand back once chosen.
fn bottom(ui: &mut egui::Ui, b: &mut Browser, rect: Rect, t: &Tokens, look: &Look) -> Option<Vec<PathBuf>> {
    ui.painter().rect_filled(rect, CornerRadius::ZERO, look.bar);
    ui.painter().hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, look.edge));
    let mut answer = None;
    let inner = rect.shrink2(vec2(16.0, 14.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
        match b.kind {
            Kind::File => {}
            Kind::Files => {
                let n = b.phone.picked.len();
                ui.horizontal_centered(|ui| {
                    let chosen = i18n::fmt(tl!("{n} selected"), &[("n", &n.to_string())]);
                    ui.label(egui::RichText::new(chosen).font(theme::medium(15.5)).color(if n > 0 { t.text } else { t.text_muted }));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if phone::action_pill(ui, Some("check"), tl!("Done"), n > 0, t).clicked() {
                            // A file picked, then moved or deleted meanwhile, isn't handed back.
                            answer = Some(b.phone.picked.iter().filter(|p| p.is_file()).cloned().collect());
                        }
                    });
                });
            }
            Kind::Save => {
                ui.label(egui::RichText::new(tl!("File name")).font(theme::regular(13.0)).color(t.text_muted));
                let mut confirm = false;
                ui.horizontal(|ui| {
                    let primary = if b.replace.is_some() { tl!("Replace") } else { tl!("Save") };
                    let pill_w = widgets::text_width(ui, primary, &theme::semibold(15.0)) + 64.0;
                    let field = egui::TextEdit::singleline(&mut b.name)
                        .id(egui::Id::new("phone-browser-name"))
                        .font(theme::regular(16.0))
                        .margin(egui::Margin::symmetric(12, 12))
                        .background_color(look.ground)
                        .desired_width((ui.available_width() - pill_w - 10.0).max(60.0))
                        .char_limit(MAX_NAME_CHARS);
                    let r = ui.add(field);
                    if r.changed() {
                        b.replace = None;
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        confirm = true;
                    }
                    if phone::action_pill(ui, Some("save"), primary, save_name(&b.name, &[]).is_some(), t).clicked() {
                        confirm = true;
                    }
                });
                if confirm {
                    // A name that is a folder opens it: Back comes back here.
                    let here = b.dir.clone();
                    answer = b.confirm();
                    if b.dir != here {
                        b.phone.trail.push(here);
                    }
                }
                // A folder to save into, made here.
                ui.add_space(4.0);
                ui.horizontal_centered(|ui| {
                    if b.new_folder.is_some() {
                        new_folder_field(ui, b, t, look);
                    } else {
                        new_folder_button(ui, b, t);
                    }
                });
                message(ui, b, t);
            }
            Kind::Folder => {
                ui.horizontal_centered(|ui| {
                    if b.new_folder.is_some() {
                        new_folder_field(ui, b, t, look);
                    } else {
                        new_folder_button(ui, b, t);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if phone::action_pill(ui, Some("check"), tl!("Choose this folder"), b.listing.is_ok(), t).clicked() {
                                answer = b.confirm();
                            }
                        });
                    }
                });
                message(ui, b, t);
            }
        }
    });
    answer
}

/// New folder: opens the field for its name.
fn new_folder_button(ui: &mut egui::Ui, b: &mut Browser, t: &Tokens) {
    if phone::soft_pill(ui, "folder-plus", tl!("New folder"), t).clicked() {
        b.new_folder = Some(tl!("New folder").to_owned());
        b.phone.focus_name = true;
        b.error = None;
    }
}

/// The new folder's name and Create: the folder is made in the one shown, and opens (Back
/// comes back).
fn new_folder_field(ui: &mut egui::Ui, b: &mut Browser, t: &Tokens, look: &Look) {
    let Some(name) = b.new_folder.as_mut() else { return };
    let mut create = false;
    let pill_w = widgets::text_width(ui, tl!("Create"), &theme::semibold(15.0)) + 40.0;
    let field = egui::TextEdit::singleline(name)
        .id(egui::Id::new("phone-browser-new-folder"))
        .font(theme::regular(16.0))
        .margin(egui::Margin::symmetric(12, 12))
        .background_color(look.ground)
        .desired_width((ui.available_width() - pill_w - 10.0).max(60.0))
        .char_limit(MAX_NAME_CHARS);
    let r = ui.add(field);
    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
        create = true;
    }
    let can = save_name(name, &[]).is_some();
    if phone::action_pill(ui, None, tl!("Create"), can, t).clicked() {
        create = true;
    }
    if std::mem::take(&mut b.phone.focus_name) {
        r.request_focus();
    }
    if create {
        let here = b.dir.clone();
        b.create_folder();
        if b.dir != here {
            b.phone.trail.push(here);
        }
    }
}

/// A Save waiting to replace a file, or what went wrong.
fn message(ui: &mut egui::Ui, b: &Browser, t: &Tokens) {
    if let Some(path) = &b.replace {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let text = i18n::fmt(tl!("{name} already exists. Replace it?"), &[("name", &name)]);
        ui.add(egui::Label::new(egui::RichText::new(text).font(theme::regular(13.5)).color(t.text)).wrap());
    }
    if let Some(error) = &b.error {
        ui.add(egui::Label::new(egui::RichText::new(error).font(theme::regular(13.5)).color(crate::combine_ui::ERROR)).wrap());
    }
}

/// A place in the row across the top: its icon and name on a pill, the chosen one in the accent.
fn chip(ui: &mut egui::Ui, icon: &str, label: &str, on: bool, t: &Tokens, look: &Look) -> egui::Response {
    let font = theme::medium(14.0);
    let w = widgets::text_width(ui, label, &font) + 52.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, TARGET), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, label));
    let press = theme::Press::track(ui, &resp);
    let body = Rect::from_center_size(rect.center(), vec2(w, 38.0)).translate(press.offset());
    let icon_rect = Rect::from_center_size(body.left_center() + vec2(21.0, 0.0), Vec2::splat(18.0));
    if on {
        phone::vivid_tile(ui.painter(), body, 19.0, t.accent);
        icons::paint_bold(ui, icon_rect, icon, 17.0, Color32::WHITE);
    } else {
        ui.painter().rect(
            body,
            CornerRadius::same(19),
            press.fill(look.surface, t.hover, t.pressed),
            Stroke::new(1.0, look.edge),
            egui::StrokeKind::Inside,
        );
        icons::paint_bold(ui, icon_rect, icon, 17.0, phone::icon_ink(phone::icon_hue(icon), t));
    }
    ui.painter().text(body.left_center() + vec2(38.0, 0.0), Align2::LEFT_CENTER, label, font, if on { Color32::WHITE } else { t.text });
    theme::hand(resp)
}

/// A file type: a smaller pill, its name only.
fn small_chip(ui: &mut egui::Ui, label: &str, on: bool, t: &Tokens, look: &Look) -> egui::Response {
    let font = theme::medium(13.0);
    let w = widgets::text_width(ui, label, &font) + 28.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 40.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, label));
    let press = theme::Press::track(ui, &resp);
    let body = Rect::from_center_size(rect.center(), vec2(w, 32.0)).translate(press.offset());
    let (fill, ink) = if on { (t.accent_soft, t.accent_text) } else { (press.fill(look.surface, t.hover, t.pressed), t.text_muted) };
    ui.painter().rect(body, CornerRadius::same(16), fill, Stroke::new(1.0, if on { t.accent } else { look.edge }), egui::StrokeKind::Inside);
    ui.painter().text(body.center(), Align2::CENTER_CENTER, label, font, ink);
    theme::hand(resp)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use egui::accesskit::Role;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::*;
    use crate::file_browser::{Dialog, PlaceKind};

    type Got = Arc<Mutex<Option<Vec<PathBuf>>>>;

    /// One page of text, for the previews.
    const PDF: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >> endobj
4 0 obj << /Length 50 >> stream
BT /F1 14 Tf 20 250 Td (The quick brown fox) Tj ET
endstream endobj
5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
trailer << /Root 1 0 R >>
%%EOF";

    /// A device: Download (two PDFs, a text file and a folder with a PDF) and Documents (a PDF).
    fn device(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("pdfcraft-phone-browser-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["Download/Trips", "Documents"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for f in ["Download/Lease.pdf", "Download/Invoice.pdf", "Download/Trips/Rome.pdf", "Documents/Report.pdf"] {
            std::fs::write(root.join(f), PDF).unwrap();
        }
        std::fs::write(root.join("Download/notes.txt"), b"notes").unwrap();
        root
    }

    fn harness(root: &Path, language: &str, dark: bool) -> Harness<'static, PdfCraftApp> {
        let root = root.to_path_buf();
        let language = language.to_owned();
        let mut h = Harness::builder().with_size(egui::vec2(412.0, 892.0)).with_step_dt(0.05).build_eframe(move |_| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", &language).unwrap();
            app.set_option("shell", "phone").unwrap();
            app.set_option("theme", if dark { "dark" } else { "light" }).unwrap();
            app.file_browser_host.places = vec![
                Place { kind: PlaceKind::Downloads, path: root.join("Download") },
                Place { kind: PlaceKind::Documents, path: root.join("Documents") },
                Place { kind: PlaceKind::Storage, path: root.clone() },
            ];
            app
        });
        h.run_steps(4);
        h
    }

    fn ask(h: &mut Harness<'static, PdfCraftApp>, kind: Kind, dialog: Dialog) -> Got {
        let got: Got = Arc::default();
        let into = got.clone();
        assert!(h.state_mut().ask_in_app(kind, dialog, None, move |_, paths| *into.lock().unwrap() = Some(paths)));
        h.run_steps(3);
        got
    }

    /// Run until `label` shows (the device's PDFs are looked for on a thread).
    fn wait_for(h: &mut Harness<'static, PdfCraftApp>, label: &str) {
        for _ in 0..200 {
            if h.query_by_label(label).is_some() {
                return;
            }
            h.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("{label} never showed");
    }

    fn escape(h: &mut Harness<'static, PdfCraftApp>) {
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
    }

    fn pdf() -> Dialog {
        Dialog::new().add_filter("PDF", &["pdf"])
    }

    #[test]
    fn a_pick_of_pdfs_starts_on_the_devices_pdfs_and_one_tap_chooses() {
        let root = device("device");
        let mut h = harness(&root, "en", false);
        let got = ask(&mut h, Kind::File, pdf());
        wait_for(&mut h, "Rome.pdf");
        // Every folder's PDFs, the folder each is in under its name; nothing else.
        for name in ["Lease.pdf", "Invoice.pdf", "Report.pdf"] {
            assert!(h.query_by_label(name).is_some(), "{name}");
        }
        assert!(h.query_by_label("notes.txt").is_none());
        // (Home, under the browser, has a tab of the same name: the browser's comes last.)
        let chip = h.query_all_by_role_and_label(Role::Button, "On this device").last().map(|n| n.accesskit_node().toggled());
        assert_eq!(chip, Some(Some(egui::accesskit::Toggled::True)), "the list it starts on is the chip chosen");
        h.get_by_label("Report.pdf").click();
        h.run_steps(4);
        assert_eq!(got.lock().unwrap().clone(), Some(vec![root.join("Documents/Report.pdf")]), "one tap, no OK button");
        assert!(h.state().file_browser.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn back_goes_back_through_the_folders_then_cancels() {
        let root = device("back");
        let mut h = harness(&root, "en", false);
        let got = ask(&mut h, Kind::File, pdf().set_directory(root.join("Download")));
        wait_for(&mut h, "Trips");
        assert!(h.query_by_label("Lease.pdf").is_some(), "a pick naming a folder starts there");
        h.get_by_label("Trips").click();
        h.run_steps(3);
        assert!(h.query_by_label("Rome.pdf").is_some(), "a tapped folder opens");
        escape(&mut h);
        assert!(h.query_by_label("Lease.pdf").is_some(), "Back goes back to the folder before");
        // A place's chip goes there, and Back from it cancels (nothing to go back to).
        h.get_by_role_and_label(Role::Button, "Documents").scroll_to_me();
        h.run_steps(3);
        h.get_by_role_and_label(Role::Button, "Documents").click();
        h.run_steps(3);
        assert!(h.query_by_label("Report.pdf").is_some());
        escape(&mut h);
        assert!(h.state().file_browser.is_none(), "Back with nowhere to go back to cancels");
        assert!(got.lock().unwrap().is_none(), "and a cancelled pick never answers");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The browser slides in from the right as it opens, and a folder's list from the side it
    /// leads to: into a folder from the right, back out from the left.
    #[test]
    fn the_browser_slides_in_and_a_folders_list_from_its_side() {
        let root = device("slide");
        let mut h = harness(&root, "en", false);
        // kittest turns egui's animations off; back on, as on a phone.
        h.ctx.all_styles_mut(|s| s.animation_time = 1.0 / 12.0);
        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("phone-file-browser"));
        let into: Got = Arc::default();
        assert!(h.state_mut().ask_in_app(Kind::File, pdf().set_directory(root.join("Download")), None, move |_, p| *into.lock().unwrap() = Some(p)));
        h.run_steps(1);
        let x = h.ctx.layer_transform_to_global(layer).map(|t| t.translation.x);
        assert!(x.is_some_and(|x| x > 0.0), "from the right: {x:?}");
        wait_for(&mut h, "Trips");
        h.run_steps(6);
        assert_eq!(h.ctx.layer_transform_to_global(layer), None, "in place");
        let left = |h: &Harness<'static, PdfCraftApp>, name: &str| h.get_by_label(name).rect().left();
        let rest = left(&h, "Lease.pdf");
        h.get_by_label("Trips").click();
        h.run_steps(2);
        assert!(left(&h, "Rome.pdf") > rest + 10.0, "into a folder: from the right");
        h.run_steps(6);
        assert_eq!(left(&h, "Rome.pdf"), rest);
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert!(left(&h, "Lease.pdf") < rest - 10.0, "back out: from the left");
        h.run_steps(6);
        assert_eq!(left(&h, "Lease.pdf"), rest);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn several_files_are_picked_from_different_places_in_the_order_tapped() {
        let root = device("files");
        let mut h = harness(&root, "en", false);
        let got = ask(&mut h, Kind::Files, pdf());
        wait_for(&mut h, "Report.pdf");
        assert!(h.get_by_role_and_label(Role::Button, "Done").accesskit_node().is_disabled(), "nothing picked yet");
        h.get_by_label("Report.pdf").click();
        h.run_steps(2);
        h.get_by_role_and_label(Role::Button, "Downloads").scroll_to_me();
        h.run_steps(3);
        h.get_by_role_and_label(Role::Button, "Downloads").click();
        h.run_steps(3);
        h.get_by_label("Lease.pdf").click();
        h.run_steps(2);
        assert!(h.query_by_label("2 selected").is_some());
        h.get_by_role_and_label(Role::Button, "Done").click();
        h.run_steps(4);
        assert_eq!(got.lock().unwrap().clone(), Some(vec![root.join("Documents/Report.pdf"), root.join("Download/Lease.pdf")]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn save_names_the_file_and_asks_before_replacing_one() {
        let root = device("save");
        let mut h = harness(&root, "en", false);
        let got = ask(&mut h, Kind::Save, pdf().set_file_name("Lease").set_directory(root.join("Download")));
        h.get_by_role_and_label(Role::Button, "Save").click();
        h.run_steps(3);
        assert!(got.lock().unwrap().is_none(), "Lease.pdf is there: nothing is written before the user agrees");
        assert!(h.query_by_label_contains("already exists").is_some());
        h.get_by_role_and_label(Role::Button, "Replace").click();
        h.run_steps(4);
        assert_eq!(got.lock().unwrap().clone(), Some(vec![root.join("Download/Lease.pdf")]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn save_makes_a_new_folder_and_saves_into_it() {
        let root = device("save-folder");
        let mut h = harness(&root, "en", false);
        let got = ask(&mut h, Kind::Save, pdf().set_file_name("Notes").set_directory(root.join("Documents")));
        h.get_by_role_and_label(Role::Button, "New folder").click();
        h.run_steps(3);
        // (Home, under the browser, has a Create tab: the browser's button comes last.)
        h.query_all_by_role_and_label(Role::Button, "Create").last().unwrap().click();
        h.run_steps(3);
        assert!(root.join("Documents/New folder").is_dir(), "made where the browser stands");
        h.get_by_role_and_label(Role::Button, "Save").click();
        h.run_steps(4);
        assert_eq!(got.lock().unwrap().clone(), Some(vec![root.join("Documents/New folder/Notes.pdf")]), "and saved into it");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Pictures of the screen, for a look: `PDFCRAFT_PHONE_SHOTS=dir`.
    #[test]
    fn shots() {
        let Ok(dir) = std::env::var("PDFCRAFT_PHONE_SHOTS") else { return };
        let root = device("shots");
        for dark in [false, true] {
            let theme = if dark { "dark" } else { "light" };
            let mut h = harness(&root, "fr", dark);
            let _got = ask(&mut h, Kind::File, pdf());
            wait_for(&mut h, "Rome.pdf");
            for _ in 0..60 {
                h.run_steps(1);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            h.render().unwrap().save(format!("{dir}/browser-device-{theme}.png")).unwrap();
            h.state_mut().file_browser.as_mut().unwrap().open_dir(root.join("Download"));
            h.run_steps(6);
            h.render().unwrap().save(format!("{dir}/browser-folder-{theme}.png")).unwrap();
            let mut h = harness(&root, "fr", dark);
            let _got = ask(&mut h, Kind::Save, pdf().set_file_name("Rapport").set_directory(root.join("Documents")));
            h.run_steps(6);
            h.render().unwrap().save(format!("{dir}/browser-save-{theme}.png")).unwrap();
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
