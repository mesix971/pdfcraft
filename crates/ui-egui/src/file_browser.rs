//! The in-app file browser: Open, Save, folder and multi-file choices drawn by PdfCraft itself.
//!
//! Android has no file picker a native app can show without an activity written in Java (the
//! system's picker answers through `onActivityResult`, which a `NativeActivity` never sees), so
//! there every picker is this browser. It works on paths, like the desktop pickers, so what
//! follows a pick (opening, saving in place, watching the file, recent files, recovery) is the
//! same code on every platform.
//!
//! [`crate::PdfCraftApp::ask`] routes a pick here; the choice goes back through the same queue a
//! native picker's does ([`crate::pickers`]), so it is used on a later frame, and only one
//! picker shows at a time. Cancelling hands back no paths, which ends the pick the same way.
//!
//! In the phone layout it is a screen of its own ([`phone`]); elsewhere a dialog.

mod phone;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Align, Align2, CornerRadius, Layout, Rect, Sense, vec2};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, i18n, icons, widgets};

/// The most entries read from one folder: a folder with more lists the first ones.
const MAX_ENTRIES: usize = 5_000;
/// The longest file name the Save box takes, in characters (file systems allow 255 bytes).
const MAX_NAME_CHARS: usize = 255;
/// Height of one row, tall enough for a finger.
const ROW_HEIGHT: f32 = 44.0;
/// How often a failed listing or a missing permission is checked again while the browser shows,
/// so access granted in the system settings shows up without a tap.
const RECHECK_SECS: f64 = 1.0;

/// A file dialog request, built with the same calls as rfd's `AsyncFileDialog`.
#[derive(Clone, Debug, Default)]
pub struct Dialog {
    title: Option<String>,
    filters: Vec<Filter>,
    file_name: Option<String>,
    directory: Option<PathBuf>,
}

/// One entry of the type list: a name and the extensions it shows (lowercase, without dots).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Filter {
    name: String,
    extensions: Vec<String>,
}

impl Dialog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer the files with one of `extensions` (with or without a leading dot, any case). The
    /// first filter is the one shown at first.
    pub fn add_filter(mut self, name: impl Into<String>, extensions: &[impl ToString]) -> Self {
        let extensions = extensions.iter().map(|e| e.to_string().trim_start_matches('.').to_ascii_lowercase()).filter(|e| !e.is_empty()).collect();
        self.filters.push(Filter { name: name.into(), extensions });
        self
    }

    pub fn set_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// The name a Save starts with.
    pub fn set_file_name(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = Some(file_name.into());
        self
    }

    /// The folder the browser starts in.
    pub fn set_directory<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.directory = Some(path.as_ref().to_path_buf());
        self
    }
}

/// What the browser chooses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// One existing file.
    File,
    /// One or more existing files.
    Files,
    /// A folder.
    Folder,
    /// A file to write (existing or new).
    Save,
}

/// What the system gives the browser: where to start, and how to ask for access to shared
/// storage. The Android app fills it in; elsewhere it stays empty.
#[derive(Clone, Default)]
pub struct Host {
    /// Starting points, in the order they are offered; the first is where a pick starts when it
    /// names no folder and none was used before.
    pub places: Vec<Place>,
    /// Whether the app may read and write anywhere on shared storage (Android's "all files
    /// access"). `None` when the system has no such permission: the browser never asks.
    pub access_granted: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    /// Shows the system page where the user allows that access.
    pub request_access: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Whether the app may read the device's photos (Android's photos permission, or all files
    /// access). `None` when the system has no such permission.
    pub photos_granted: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    /// Asks for it: the system's own question, as other apps ask.
    pub request_photos: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host").field("places", &self.places).finish_non_exhaustive()
    }
}

/// A starting point offered above the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub kind: PlaceKind,
    pub path: PathBuf,
}

/// What a [`Place`] is, which names it in the interface language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceKind {
    /// The device's shared storage (`/storage/emulated/0` on Android).
    Storage,
    /// Its Download folder.
    Downloads,
    /// Its Documents folder.
    Documents,
    /// The app's own folder, which needs no permission.
    AppFiles,
    /// A removable card, by the name the system gives it.
    Removable(String),
}

impl PlaceKind {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Storage => tl!("Internal storage").to_owned(),
            Self::Downloads => tl!("Downloads").to_owned(),
            Self::Documents => tl!("Documents").to_owned(),
            Self::AppFiles => tl!("PdfCraft files").to_owned(),
            Self::Removable(name) => i18n::fmt(tl!("SD card {name}"), &[("name", name)]),
        }
    }

    pub(crate) fn icon(&self) -> &'static str {
        match self {
            Self::Downloads => "file-down",
            Self::AppFiles => "files",
            _ => "folder",
        }
    }
}

/// One row of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) dir: bool,
    pub(crate) size: u64,
    pub(crate) modified: Option<std::time::SystemTime>,
}

/// The folders and the files with one of `extensions` (any file when `None`) in `dir`: folders
/// first, each group by name ignoring case. Hidden entries (names starting with a dot) are left
/// out, and only the first [`MAX_ENTRIES`] are read. `folders_only` leaves out every file.
pub(crate) fn list(dir: &Path, extensions: Option<&[String]>, folders_only: bool) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten().take(MAX_ENTRIES) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // A plain file of a kind not asked for needs no look at its details (a big folder of
        // photos, when picking PDFs); a link is followed below.
        if entry.file_type().is_ok_and(|k| k.is_file()) && (folders_only || !matches_extension(&path, extensions)) {
            continue;
        }
        // Follows links, like opening the file does; an entry that can't be read is left out.
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        let dir = meta.is_dir();
        if !dir && (folders_only || !meta.is_file() || !matches_extension(&path, extensions)) {
            continue;
        }
        out.push(Entry { name, path, dir, size: if dir { 0 } else { meta.len() }, modified: meta.modified().ok() });
    }
    // Folders first, then by name ignoring case (each name lowered once, not at every comparison).
    out.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase(), e.name.clone()));
    Ok(out)
}

fn matches_extension(path: &Path, extensions: Option<&[String]>) -> bool {
    let Some(extensions) = extensions else { return true };
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).is_some_and(|e| extensions.contains(&e))
}

/// `text` within `max_width`: whole when it fits, else its end after an ellipsis, as long as
/// fits (or the ellipsis alone). Found by bisection, so a long path is measured a few times only.
pub(crate) fn tail_fit(text: &str, max_width: f32, mut width_of: impl FnMut(&str) -> f32) -> String {
    if width_of(text) <= max_width {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |start: usize| std::iter::once('\u{2026}').chain(chars.iter().skip(start).copied()).collect::<String>();
    // The fewest characters dropped from the front that fit; `chars.len()` keeps the ellipsis only.
    let (mut lo, mut hi) = (1, chars.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if width_of(&cut(mid)) <= max_width {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    cut(lo)
}

/// The name a Save writes: `name` trimmed, with the first of `extensions` added when it has none
/// of them. `None` when it can't name a file in the folder (empty, `.`, `..`, a path separator
/// or a NUL).
pub(crate) fn save_name(name: &str, extensions: &[String]) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return None;
    }
    let has = Path::new(name).extension().is_some_and(|e| extensions.contains(&e.to_string_lossy().to_ascii_lowercase()));
    match extensions.first() {
        Some(ext) if !has => Some(format!("{name}.{ext}")),
        _ => Some(name.to_owned()),
    }
}

/// A browser on screen: what it is choosing, where it is, and what is chosen so far.
pub(crate) struct Browser {
    kind: Kind,
    dialog: Dialog,
    dir: PathBuf,
    listing: Result<Vec<Entry>, String>,
    /// When the folder was listed, or `None` to list it on the next frame.
    listed_at: Option<f64>,
    /// The filter shown: an index into the dialog's filters, or their count for "All files".
    filter: usize,
    /// The chosen files (File: at most one).
    chosen: Vec<PathBuf>,
    /// The Save box.
    name: String,
    /// The New folder box, while it shows.
    new_folder: Option<String>,
    /// A Save that would replace this file, waiting for the user to confirm.
    replace: Option<PathBuf>,
    /// A failure to show above the buttons.
    error: Option<String>,
    /// The last answer of [`Host::access_granted`], and when it was asked.
    access: Option<(bool, f64)>,
    /// The pick this browser answers (see [`crate::pickers`]).
    pub(crate) waiting: crate::pickers::Waiting,
    /// The phone screen's own state.
    phone: phone::PhoneBrowser,
}

impl Browser {
    /// A browser for `kind` starting in `dialog`'s folder, else `last`, else the first of
    /// `places`, else the file system root.
    pub(crate) fn new(kind: Kind, dialog: Dialog, last: Option<&Path>, places: &[Place], waiting: crate::pickers::Waiting) -> Self {
        let start = dialog
            .directory
            .clone()
            .filter(|d| d.is_dir())
            .or_else(|| last.filter(|d| d.is_dir()).map(Path::to_path_buf))
            .or_else(|| places.iter().map(|p| p.path.clone()).find(|p| p.is_dir()))
            .unwrap_or_else(|| PathBuf::from("/"));
        let name = dialog.file_name.clone().unwrap_or_default().chars().take(MAX_NAME_CHARS).collect();
        let phone = phone::PhoneBrowser::new(kind, &dialog);
        Self {
            kind,
            dialog,
            dir: start,
            listing: Ok(Vec::new()),
            listed_at: None,
            filter: 0,
            chosen: Vec::new(),
            name,
            new_folder: None,
            replace: None,
            error: None,
            access: None,
            waiting,
            phone,
        }
    }

    /// The extensions the list shows, or `None` for every file.
    fn extensions(&self) -> Option<&[String]> {
        self.dialog.filters.get(self.filter).map(|f| f.extensions.as_slice())
    }

    fn relist(&mut self, now: f64) {
        let extensions = self.extensions().map(<[String]>::to_vec);
        self.listing = list(&self.dir, extensions.as_deref(), self.kind == Kind::Folder).map_err(|e| e.to_string());
        self.listed_at = Some(now);
        // A chosen file that went away (or that the new filter hides) is no longer chosen.
        if let Ok(entries) = &self.listing {
            self.chosen.retain(|c| entries.iter().any(|e| !e.dir && &e.path == c));
        }
    }

    /// Show `dir` instead.
    pub(crate) fn go(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.listed_at = None;
        self.chosen.clear();
        self.replace = None;
        self.error = None;
        self.new_folder = None;
    }

    /// What a tap on `entry` does: a folder opens; a file is chosen (in Save, its name fills the box).
    pub(crate) fn tap(&mut self, entry: &Entry) {
        if entry.dir {
            self.go(entry.path.clone());
            return;
        }
        match self.kind {
            Kind::File => self.chosen = vec![entry.path.clone()],
            Kind::Files => {
                if let Some(i) = self.chosen.iter().position(|c| c == &entry.path) {
                    self.chosen.remove(i);
                } else {
                    self.chosen.push(entry.path.clone());
                }
            }
            Kind::Save => {
                self.name = entry.name.chars().take(MAX_NAME_CHARS).collect();
                self.replace = None;
            }
            Kind::Folder => {}
        }
    }

    /// The paths the primary button would hand back now, or `None` while it can't be pressed.
    /// A Save onto an existing file first asks to replace it (`replace`).
    pub(crate) fn confirm(&mut self) -> Option<Vec<PathBuf>> {
        match self.kind {
            Kind::File | Kind::Files => (!self.chosen.is_empty()).then(|| self.chosen.clone()),
            Kind::Folder => Some(vec![self.dir.clone()]),
            Kind::Save => {
                let extensions = self.dialog.filters.first().map(|f| f.extensions.clone()).unwrap_or_default();
                let path = self.dir.join(save_name(&self.name, &extensions)?);
                if path.is_dir() {
                    self.go(path);
                    return None;
                }
                if path.exists() && self.replace.as_ref() != Some(&path) {
                    self.replace = Some(path);
                    return None;
                }
                Some(vec![path])
            }
        }
    }

    /// Make the folder typed in the New folder box and show it.
    fn create_folder(&mut self) {
        let Some(name) = self.new_folder.as_deref().and_then(|n| save_name(n, &[])) else { return };
        let path = self.dir.join(&name);
        match std::fs::create_dir(&path) {
            Ok(()) => self.go(path),
            Err(e) => self.error = Some(i18n::fmt(tl!("Couldn't create the folder {name}: {e}"), &[("name", &name), ("e", &e.to_string())])),
        }
    }

    fn title(&self) -> String {
        if let Some(title) = &self.dialog.title {
            return title.clone();
        }
        match self.kind {
            Kind::File | Kind::Files => tl!("Open").to_owned(),
            Kind::Folder => tl!("Choose this folder").to_owned(),
            Kind::Save => tl!("Save").to_owned(),
        }
    }

    /// Before drawing: ask again whether shared storage may be read (about once a second while
    /// it may not), and list the folder when it hasn't been or its listing failed a while ago.
    fn refresh(&mut self, ctx: &egui::Context, host: &Host) {
        let now = ctx.input(|i| i.time);
        if let Some(granted) = &host.access_granted {
            let stale = self.access.is_none_or(|(_, at)| now - at >= RECHECK_SECS);
            if stale {
                let was = self.access.map(|(g, _)| g);
                let is = granted();
                // Access just granted: what the folder holds may have changed.
                if was == Some(false) && is {
                    self.listed_at = None;
                }
                self.access = Some((is, now));
            }
        }
        if self.listed_at.is_none() || (self.listing.is_err() && self.listed_at.is_some_and(|at| now - at >= RECHECK_SECS)) {
            self.relist(now);
        }
        if self.listing.is_err() || self.access.is_some_and(|(g, _)| !g) {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(RECHECK_SECS));
        }
    }

    /// Draw the browser. Returns the paths to hand back once the user confirms or cancels (an
    /// empty list), `None` while it stays open.
    fn ui(&mut self, ctx: &egui::Context, host: &Host) -> Option<Vec<PathBuf>> {
        self.refresh(ctx, host);
        let now = ctx.input(|i| i.time);
        let t = Tokens::get(ctx);
        let screen = ctx.content_rect();
        let width = (screen.width() - 32.0).clamp(240.0, 640.0);
        let list_height = (screen.height() - 330.0).clamp(120.0, 420.0);
        let mut result = None;
        let modal = widgets::show_modal(ctx, "file_browser", |ui| {
            ui.set_width(width);
            ui.horizontal(|ui| {
                ui.add(icons::image(if self.kind == Kind::Save { "save" } else { "folder-open" }, 22.0, t.accent));
                ui.add(egui::Label::new(egui::RichText::new(self.title()).font(theme::semibold(16.0))).truncate());
            });
            ui.add_space(6.0);
            if self.access.is_some_and(|(g, _)| !g) {
                self.access_banner(ui, host);
            }
            self.places_row(ui, host);
            ui.add_space(4.0);
            self.location_row(ui);
            if !self.dialog.filters.is_empty() && self.kind != Kind::Folder {
                self.filter_row(ui, now);
            }
            ui.add_space(4.0);
            egui::Frame::new().stroke(egui::Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(6)).show(ui, |ui| {
                ui.set_width(width);
                egui::ScrollArea::vertical().id_salt("file_browser_list").max_height(list_height).min_scrolled_height(list_height).show(ui, |ui| {
                    ui.set_width(width);
                    if self.entries(ui) {
                        result = self.confirm();
                    }
                });
            });
            ui.add_space(8.0);
            if self.kind == Kind::Save {
                ui.horizontal(|ui| {
                    ui.label(tl!("File name"));
                    let field = ui.add(widgets::line(&mut self.name).desired_width(ui.available_width()));
                    if field.changed() {
                        self.replace = None;
                        if self.name.chars().count() > MAX_NAME_CHARS {
                            self.name = self.name.chars().take(MAX_NAME_CHARS).collect();
                        }
                    }
                    if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        result = self.confirm();
                    }
                });
            }
            if matches!(self.kind, Kind::Save | Kind::Folder) {
                self.new_folder_row(ui);
            }
            if let Some(path) = &self.replace {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.add(
                    egui::Label::new(egui::RichText::new(i18n::fmt(tl!("{name} already exists. Replace it?"), &[("name", &name)])).color(t.text))
                        .wrap(),
                );
            }
            if let Some(error) = &self.error {
                ui.add(egui::Label::new(egui::RichText::new(error).color(egui::Color32::from_rgb(0xD1, 0x3B, 0x3B))).wrap());
            }
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let primary = match self.kind {
                    Kind::File | Kind::Files => tl!("Open"),
                    Kind::Folder => tl!("Choose this folder"),
                    Kind::Save if self.replace.is_some() => tl!("Replace"),
                    Kind::Save => tl!("Save"),
                };
                let ready = match self.kind {
                    Kind::File | Kind::Files => !self.chosen.is_empty(),
                    Kind::Folder => self.listing.is_ok(),
                    Kind::Save => save_name(&self.name, &[]).is_some(),
                };
                if ui.add_enabled_ui(ready, |ui| widgets::pill_button(ui, primary, true)).inner.clicked() {
                    result = self.confirm();
                }
                if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                    result = Some(Vec::new());
                }
            });
        });
        if result.is_none() && modal.should_close() {
            result = Some(Vec::new());
        }
        result
    }

    fn access_banner(&mut self, ui: &mut egui::Ui, host: &Host) {
        let t = Tokens::get(ui.ctx());
        egui::Frame::new().fill(t.accent_soft).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
            ui.add(egui::Label::new(tl!("To open and save files anywhere on this device, allow PdfCraft to manage all files.")).wrap());
            if let Some(request) = &host.request_access
                && widgets::pill_button(ui, tl!("Allow access"), true).clicked()
            {
                request();
            }
        });
        ui.add_space(4.0);
    }

    fn places_row(&mut self, ui: &mut egui::Ui, host: &Host) {
        let mut go = None;
        ui.horizontal_wrapped(|ui| {
            if let Some(parent) = self.dir.parent()
                && widgets::ghost_button(ui, "chevron-up", tl!("Up")).clicked()
            {
                go = Some(parent.to_path_buf());
            }
            for place in &host.places {
                if widgets::ghost_button(ui, place.kind.icon(), &place.kind.label()).clicked() {
                    go = Some(place.path.clone());
                }
            }
        });
        if let Some(dir) = go {
            self.go(dir);
        }
    }

    fn location_row(&self, ui: &mut egui::Ui) {
        let t = Tokens::get(ui.ctx());
        let shown = self.dir.to_string_lossy();
        let width = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(width, 22.0), Sense::hover());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, shown.as_ref()));
        // The end of a long path is the part that tells folders apart: cut from the front.
        let font = theme::regular(12.5);
        let text = tail_fit(&shown, width, |s| widgets::text_width(ui, s, &font));
        ui.painter().with_clip_rect(rect).text(rect.left_center(), Align2::LEFT_CENTER, text, font, t.text_muted);
    }

    fn filter_row(&mut self, ui: &mut egui::Ui, now: f64) {
        let all = self.dialog.filters.len();
        let label = |i: usize| self.dialog.filters.get(i).map_or_else(|| tl!("All files").to_owned(), |f| f.name.clone());
        let mut filter = self.filter;
        crate::widgets::combo("file_browser_filter").selected_text(label(filter)).width(ui.available_width().min(320.0)).show_ui(ui, |ui| {
            for i in 0..=all {
                ui.selectable_value(&mut filter, i, label(i));
            }
        });
        if filter != self.filter {
            self.filter = filter;
            self.relist(now);
        }
    }

    /// The list. Returns true when a file was double-tapped to open it at once.
    fn entries(&mut self, ui: &mut egui::Ui) -> bool {
        let t = Tokens::get(ui.ctx());
        let entries = match &self.listing {
            Ok(entries) => entries.clone(),
            Err(e) => {
                let folder = self.dir.file_name().map_or_else(|| self.dir.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
                ui.add_space(12.0);
                ui.add(egui::Label::new(i18n::fmt(tl!("Couldn't open {name}: {e}"), &[("name", &folder), ("e", e)])).wrap());
                ui.add_space(12.0);
                return false;
            }
        };
        if entries.is_empty() {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| ui.label(egui::RichText::new(tl!("This folder is empty")).color(t.text_muted)));
            ui.add_space(12.0);
            return false;
        }
        let mut tapped = None;
        let mut open_now = None;
        let width = ui.available_width();
        for entry in &entries {
            let chosen = self.chosen.contains(&entry.path);
            let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_HEIGHT), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, chosen, &entry.name));
            let fill = if chosen {
                t.selected
            } else if resp.hovered() {
                t.hover
            } else {
                egui::Color32::TRANSPARENT
            };
            ui.painter().rect_filled(rect.shrink(1.0), CornerRadius::same(4), fill);
            let icon = if entry.dir { "folder" } else { "file-text" };
            icons::paint(ui, Rect::from_min_size(rect.min + vec2(8.0, (ROW_HEIGHT - 20.0) / 2.0), vec2(20.0, 20.0)), icon, 19.0, t.icon);
            let size = if entry.dir { String::new() } else { crate::panels::human_size(usize::try_from(entry.size).unwrap_or(usize::MAX)) };
            let size_width = if size.is_empty() { 0.0 } else { widgets::text_width(ui, &size, &theme::regular(12.0)) + 12.0 };
            if !size.is_empty() {
                ui.painter().text(rect.right_center() - vec2(10.0, 0.0), Align2::RIGHT_CENTER, &size, theme::regular(12.0), t.text_muted);
            }
            let name_left = rect.left() + 38.0;
            widgets::paint_left(
                ui,
                egui::pos2(name_left, rect.center().y),
                &entry.name,
                theme::regular(14.0),
                t.text,
                (rect.right() - size_width - name_left - 8.0).max(0.0),
            );
            if resp.clicked() {
                tapped = Some(entry.clone());
            }
            if resp.double_clicked() && !entry.dir && self.kind == Kind::File {
                open_now = Some(entry.path.clone());
            }
        }
        if let Some(entry) = tapped {
            self.tap(&entry);
        }
        if let Some(path) = open_now {
            self.chosen = vec![path];
            return true;
        }
        false
    }

    fn new_folder_row(&mut self, ui: &mut egui::Ui) {
        match &mut self.new_folder {
            None => {
                if widgets::ghost_button(ui, "folder-plus", tl!("New folder")).clicked() {
                    self.new_folder = Some(tl!("New folder").to_owned());
                    self.error = None;
                }
            }
            Some(name) => {
                let mut create = false;
                let mut cancel = false;
                ui.horizontal(|ui| {
                    ui.add(icons::image("folder-plus", 18.0, Tokens::get(ui.ctx()).icon));
                    let field = ui.add(widgets::line(name).desired_width((ui.available_width() - 190.0).max(80.0)));
                    if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        create = true;
                    }
                    if ui.add_enabled_ui(save_name(name, &[]).is_some(), |ui| widgets::pill_button(ui, tl!("Create"), false)).inner.clicked() {
                        create = true;
                    }
                    if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                        cancel = true;
                    }
                });
                if create {
                    self.create_folder();
                } else if cancel {
                    self.new_folder = None;
                }
            }
        }
    }
}

/// Draw the browser while one is open, and hand its answer to the pick that asked.
pub(crate) fn show(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let Some(mut browser) = app.file_browser.take() else { return };
    let answer = if app.phone_layout() { phone::page(app, &mut browser, ctx) } else { browser.ui(ctx, &app.file_browser_host) };
    match answer {
        None => app.file_browser = Some(browser),
        Some(paths) => {
            // The next pick starts where this one ended, unless it names its own folder.
            app.file_browser_dir = Some(browser.dir.clone());
            app.finish_pick(browser.waiting, paths);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pdfcraft-file-browser-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn folders_come_first_and_only_matching_visible_files_are_listed() {
        let dir = temp_dir("list");
        for f in ["b.pdf", "A.PDF", "notes.txt", ".hidden.pdf", "c.Pdf"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        std::fs::create_dir(dir.join("zeta")).unwrap();
        std::fs::create_dir(dir.join("Alpha")).unwrap();
        std::fs::create_dir(dir.join(".git")).unwrap();
        let pdf = vec!["pdf".to_string()];
        let names: Vec<_> = list(&dir, Some(&pdf), false).unwrap().into_iter().map(|e| (e.name, e.dir)).collect();
        assert_eq!(names, [("Alpha".into(), true), ("zeta".into(), true), ("A.PDF".into(), false), ("b.pdf".into(), false), ("c.Pdf".into(), false)]);
        let every: Vec<_> = list(&dir, None, false).unwrap().into_iter().map(|e| e.name).collect();
        assert!(every.contains(&"notes.txt".to_string()));
        let folders: Vec<_> = list(&dir, None, true).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(folders, ["Alpha", "zeta"]);
        assert!(list(&dir.join("missing"), None, false).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_names_get_the_filters_extension_and_never_leave_the_folder() {
        let pdf = vec!["pdf".to_string()];
        assert_eq!(save_name("report", &pdf).as_deref(), Some("report.pdf"));
        assert_eq!(save_name("  report.PDF ", &pdf).as_deref(), Some("report.PDF"));
        assert_eq!(save_name("report.final", &pdf).as_deref(), Some("report.final.pdf"));
        assert_eq!(save_name("notes", &[]).as_deref(), Some("notes"));
        for bad in ["", "   ", ".", "..", "../x", "a/b", "a\\b", "a\0b"] {
            assert_eq!(save_name(bad, &pdf), None, "{bad:?}");
        }
    }

    #[test]
    fn long_paths_keep_their_end_within_the_width() {
        let width = |s: &str| s.chars().count() as f32;
        assert_eq!(tail_fit("/storage/emulated/0", 40.0, width), "/storage/emulated/0");
        assert_eq!(tail_fit("/storage/emulated/0/Download", 10.0, width), "\u{2026}/Download");
        assert_eq!(tail_fit("abc", 0.5, width), "\u{2026}", "only the ellipsis when nothing fits");
        assert_eq!(tail_fit("", 0.0, width), "");
        let long: String = std::iter::repeat_n("dossier/", 600).collect();
        let mut calls = 0;
        let fitted = tail_fit(&long, 30.0, |s| {
            calls += 1;
            width(s)
        });
        assert_eq!(fitted.chars().count(), 30);
        assert!(long.ends_with(fitted.trim_start_matches('\u{2026}')));
        assert!(calls < 20, "a long path is measured {calls} times");
    }

    #[test]
    fn filters_take_extensions_with_or_without_dots_in_any_case() {
        let d = Dialog::new().add_filter("PDF", &[".PDF", "pdf", ""]).set_title("t").set_file_name("f").set_directory("/tmp");
        assert_eq!(d.filters, [Filter { name: "PDF".into(), extensions: vec!["pdf".into(), "pdf".into()] }]);
        assert_eq!(d.title.as_deref(), Some("t"));
        assert_eq!(d.file_name.as_deref(), Some("f"));
        assert_eq!(d.directory.as_deref(), Some(Path::new("/tmp")));
    }
}

#[cfg(test)]
mod ui_tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use egui::accesskit::Role;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    type Got = Arc<Mutex<Option<Vec<PathBuf>>>>;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pdfcraft-file-browser-ui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for f in ["a.pdf", "b.pdf", "notes.txt", "sub/inner.pdf"] {
            std::fs::write(dir.join(f), b"%PDF-1.7").unwrap();
        }
        dir
    }

    /// The app with the browser's only place set to `dir`.
    fn harness(dir: &Path) -> Harness<'static, PdfCraftApp> {
        let dir = dir.to_path_buf();
        let mut h = Harness::builder().with_size(egui::vec2(420.0, 860.0)).build_eframe(move |_| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", "en").unwrap();
            app.file_browser_host.places = vec![Place { kind: PlaceKind::Storage, path: dir.clone() }];
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

    /// The browser's button labelled `label`. Home behind it may have one with the same label;
    /// the modal is drawn last, so its button comes last in the tree.
    fn button(h: &Harness<'static, PdfCraftApp>, label: &str) {
        h.query_all_by_role_and_label(Role::Button, label).last().unwrap_or_else(|| panic!("no {label} button")).click();
    }

    #[test]
    fn open_shows_the_matching_files_and_hands_back_the_one_tapped() {
        let dir = temp_dir("open");
        let mut h = harness(&dir);
        let got = ask(&mut h, Kind::File, Dialog::new().add_filter("PDF", &["pdf"]).set_title("Pick a PDF"));
        assert!(h.query_by_label("notes.txt").is_none(), "the PDF filter hides other files");
        h.get_by_label("sub").click();
        h.run_steps(3);
        assert!(h.query_by_label("inner.pdf").is_some(), "a tapped folder opens");
        button(&h, "Up");
        h.run_steps(3);
        h.get_by_label("b.pdf").click();
        h.run_steps(2);
        button(&h, "Open");
        h.run_steps(4);
        assert_eq!(got.lock().unwrap().clone(), Some(vec![dir.join("b.pdf")]));
        assert!(h.state().file_browser.is_none());
        assert_eq!(h.state().file_browser_dir.as_deref(), Some(dir.as_path()), "the next pick starts here");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn all_files_shows_every_file_and_several_can_be_chosen() {
        let dir = temp_dir("files");
        let mut h = harness(&dir);
        let got = ask(&mut h, Kind::Files, Dialog::new().add_filter("PDF", &["pdf"]));
        h.state_mut().file_browser.as_mut().unwrap().filter = 1;
        h.state_mut().file_browser.as_mut().unwrap().listed_at = None;
        h.run_steps(3);
        assert!(h.query_by_label("notes.txt").is_some(), "All files lists every file");
        h.get_by_label("notes.txt").click();
        h.run_steps(2);
        h.get_by_label("a.pdf").click();
        h.run_steps(2);
        h.get_by_label("notes.txt").click();
        h.run_steps(2);
        h.get_by_label("b.pdf").click();
        h.run_steps(2);
        button(&h, "Open");
        h.run_steps(4);
        assert_eq!(got.lock().unwrap().clone(), Some(vec![dir.join("a.pdf"), dir.join("b.pdf")]), "a second tap unchooses");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_adds_the_extension_and_asks_before_replacing_a_file() {
        let dir = temp_dir("save");
        let mut h = harness(&dir);
        let got = ask(&mut h, Kind::Save, Dialog::new().add_filter("PDF", &["pdf"]).set_file_name("a"));
        button(&h, "Save");
        h.run_steps(3);
        assert!(got.lock().unwrap().is_none(), "a.pdf exists: nothing is written before the user agrees");
        assert!(h.query_by_label_contains("already exists").is_some());
        button(&h, "Replace");
        h.run_steps(4);
        assert_eq!(got.lock().unwrap().clone(), Some(vec![dir.join("a.pdf")]));

        let got = ask(&mut h, Kind::Save, Dialog::new().add_filter("PDF", &["pdf"]).set_file_name("new name"));
        button(&h, "Save");
        h.run_steps(4);
        assert_eq!(got.lock().unwrap().clone(), Some(vec![dir.join("new name.pdf")]), "a new file needs no question");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_is_chosen_where_the_browser_stands_and_new_folders_can_be_made() {
        let dir = temp_dir("folder");
        let mut h = harness(&dir);
        let got = ask(&mut h, Kind::Folder, Dialog::new().set_title("Where to"));
        assert!(h.query_by_label("a.pdf").is_none(), "a folder pick lists folders only");
        h.get_by_role_and_label(Role::Button, "New folder").click();
        h.run_steps(3);
        button(&h, "Create");
        h.run_steps(3);
        assert!(dir.join("New folder").is_dir());
        button(&h, "Choose this folder");
        h.run_steps(4);
        assert_eq!(got.lock().unwrap().clone(), Some(vec![dir.join("New folder")]), "the new folder opens and is the one chosen");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_hands_back_nothing_and_frees_the_picker() {
        let dir = temp_dir("cancel");
        let mut h = harness(&dir);
        let got = ask(&mut h, Kind::File, Dialog::new());
        let refused = h.state_mut().ask_in_app(Kind::File, Dialog::new(), None, |_, _| panic!("must not run"));
        assert!(!refused, "one picker at a time");
        button(&h, "Cancel");
        h.run_steps(4);
        assert!(got.lock().unwrap().is_none(), "a cancelled pick never calls back");
        assert!(h.state().file_browser.is_none());
        let again = ask(&mut h, Kind::File, Dialog::new());
        assert!(h.state().file_browser.is_some(), "the next pick shows");
        button(&h, "Cancel");
        h.run_steps(4);
        assert!(again.lock().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_storage_access_offers_the_system_page_and_goes_once_granted() {
        let dir = temp_dir("access");
        let mut h = harness(&dir);
        let granted = Arc::new(AtomicBool::new(false));
        let asked = Arc::new(AtomicBool::new(false));
        {
            let host = &mut h.state_mut().file_browser_host;
            let g = granted.clone();
            host.access_granted = Some(Arc::new(move || g.load(Ordering::SeqCst)));
            let a = asked.clone();
            host.request_access = Some(Arc::new(move || a.store(true, Ordering::SeqCst)));
        }
        let _got = ask(&mut h, Kind::File, Dialog::new());
        button(&h, "Allow access");
        h.run_steps(2);
        assert!(asked.load(Ordering::SeqCst), "the button opens the system page");
        granted.store(true, Ordering::SeqCst);
        // Checked again about once a second, without a tap.
        h.state_mut().file_browser.as_mut().unwrap().access = Some((false, -10.0));
        h.run_steps(3);
        assert!(h.query_by_role_and_label(Role::Button, "Allow access").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
