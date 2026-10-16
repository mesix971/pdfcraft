//! The Android shell around [`PdfCraftApp`]: the window, settings and recovery folders, the
//! document the app was opened with, the back key, the soft keyboard's room, the clipboard,
//! and the in-app file browser's places and storage access.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, PoisonError};

use pdfcraft_ui_egui::file_browser::{Place, PlaceKind};
use pdfcraft_ui_egui::{OsEvent, PdfCraftApp, RecoveryStore};
use winit::platform::android::activity::AndroidApp;

use crate::jvm;
use crate::storage;

/// Settings are kept under this key (the desktop app uses the same one).
const STORAGE_KEY: &str = "pdfcraft";
/// Below this smallest width (in dp) the screen is a phone's.
const PHONE_DP: i32 = 600;
/// Start PdfCraft in `android`'s activity. Returns when the activity is destroyed.
pub fn run(android: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("PdfCraft"));
    // Last-resort guard (AGENTS.md §4): the engine catches panics in commands, opens and saves;
    // this records every panic, caught or not.
    std::panic::set_hook(Box::new(|info| log::error!("internal error: {info}")));
    log::info!("PdfCraft {} on Android (API {})", env!("CARGO_PKG_VERSION"), AndroidApp::sdk_version());
    if hand_off(&android) {
        return;
    }

    let config = android.config();
    if let Some(language) = config.language() {
        let tag = config.country().map_or_else(|| language.clone(), |country| format!("{language}_{country}"));
        pdfcraft_ui_egui::set_system_locale(&tag);
    }
    let phone = config.smallest_screen_width_dp().is_some_and(|dp| dp > 0 && dp < PHONE_DP);
    // App-private storage: settings, the log of recovered work. Shared storage holds documents.
    let data = android.internal_data_path();
    let persistence_path = data.as_ref().map(|d| d.join("settings").join("app.ron"));
    // Typing shows and hides the soft keyboard itself (`typing`): winit's own, as egui's text
    // input goes off and on, raced it and could leave a field without a keyboard.
    winit::platform::android::set_app_soft_keyboard(true);
    let options =
        eframe::NativeOptions { android_app: Some(android.clone()), persistence_path, renderer: eframe::Renderer::Wgpu, ..Default::default() };
    let creator: eframe::AppCreator<'static> = Box::new(move |cc| {
        let mut app = PdfCraftApp::new();
        app.units = pdfcraft_ui_egui::units::Unit::system();
        match cc.storage.and_then(|s| s.get_string(STORAGE_KEY)) {
            Some(json) => app.restore(&json),
            // First start on a phone: the tool panel would take half of a portrait screen, so the
            // page starts alone (the Tools tab opens it, and the choice is kept from then on).
            None if phone => {
                if let Err(e) = app.set_option("left", "closed") {
                    log::warn!("closing the tool panel: {e}");
                }
            }
            None => {}
        }
        if phone {
            // Always the phone layout: it has every tool, and draws at the phone's own points.
            app.phone_screen = true;
        }
        // Light or dark as the phone is, until the user chooses (Profile ▸ Appearance).
        if !app.theme_chosen {
            app.set_theme_preference(pdfcraft_ui_egui::theme::ThemePreference::System);
        }
        app.file_browser_host = browser_host(&android);
        let clipboard = android.clone();
        app.clipboard_text = Some(Box::new(move || jvm::clipboard_text(&clipboard)));
        let printing = android.clone();
        app.host_print = Some(Box::new(move |name, bytes| jvm::print_with_app(&printing, name, &bytes, pdfcraft_ui_egui::i18n::t("Print"))));
        // The system's animation setting (asked again every few seconds) and font size.
        let motion = android.clone();
        app.motion.source = Some(Box::new(move || jvm::animations_off(&motion)));
        if let Some(scale) = jvm::font_scale(&android) {
            app.system_text_scale = scale;
        }
        let back = android.clone();
        app.to_background = Some(Box::new(move || jvm::move_task_to_back(&back)));
        if let Some(dir) = &data {
            app.enable_recovery(RecoveryStore::new(dir.join("recovery")));
            install_models(&android, dir.join("models"));
        }
        app.os_events = Some(launch_document(&android, &app, &cc.egui_ctx));
        app.reopen_last_files(&[]);
        Ok(Box::new(Shell {
            app,
            android: android.clone(),
            typing: Default::default(),
            bars: None,
            bars_failed: std::sync::Arc::default(),
            bars_tries: 0,
            window_insets: std::sync::Arc::default(),
            insets_asked: None,
        }))
    });
    if let Err(e) = eframe::run_native("PdfCraft", options, creator) {
        log::error!("PdfCraft stopped: {e}");
    }
    // This Activity is finishing: the next one starts PdfCraft afresh.
    *running() = None;
}

/// Unpack the text recognition models from the APK's assets into `dir` (once; a model already
/// there at its full size stays), off the UI thread, and tell OCR to look there. A model appears
/// under its name only once it is complete, so OCR never loads half a file. An APK built without
/// the models simply has none to unpack, and Recognize Text says they are missing.
fn install_models(android: &AndroidApp, dir: PathBuf) {
    pdfcraft_ocr::Models::set_installed_dir(dir.clone());
    let assets = android.asset_manager();
    let unpack = move || {
        for name in [pdfcraft_ocr::DETECTION_MODEL, pdfcraft_ocr::RECOGNITION_MODEL] {
            let Ok(path) = std::ffi::CString::new(format!("models/{name}")) else { continue };
            let Some(mut asset) = assets.open(&path) else {
                log::info!("this APK has no {name}: text recognition is unavailable");
                continue;
            };
            let target = dir.join(name);
            let size = u64::try_from(asset.length()).unwrap_or(u64::MAX);
            if std::fs::metadata(&target).is_ok_and(|m| m.len() == size) {
                continue;
            }
            let partial = dir.join(format!("{name}.partial"));
            let written = std::fs::create_dir_all(&dir)
                .and_then(|()| std::fs::File::create(&partial))
                .and_then(|mut file| std::io::copy(&mut asset, &mut file))
                .and_then(|_| std::fs::rename(&partial, &target));
            if let Err(e) = written {
                log::warn!("unpacking {name}: {e}");
                let _ = std::fs::remove_file(&partial);
            }
        }
    };
    if let Err(e) = std::thread::Builder::new().name("unpack-models".into()).spawn(unpack) {
        log::warn!("couldn't start unpacking the text recognition models: {e}");
    }
}

/// The places the file browser offers, and how it asks for storage access.
fn browser_host(android: &AndroidApp) -> pdfcraft_ui_egui::file_browser::Host {
    let shared = Path::new(storage::SHARED);
    let mut places = vec![
        Place { kind: PlaceKind::Downloads, path: shared.join("Download") },
        Place { kind: PlaceKind::Documents, path: shared.join("Documents") },
        Place { kind: PlaceKind::Storage, path: shared.to_path_buf() },
    ];
    for (name, path) in storage::removable(Path::new(storage::MOUNTS)) {
        places.push(Place { kind: PlaceKind::Removable(name), path });
    }
    // Readable and writable without any permission, so there is always somewhere to save.
    if let Some(own) = android.external_data_path().or_else(|| android.internal_data_path()) {
        let _ = std::fs::create_dir_all(&own);
        places.push(Place { kind: PlaceKind::AppFiles, path: own });
    }
    let granted = android.clone();
    let request = android.clone();
    let photos = android.clone();
    let ask_photos = android.clone();
    pdfcraft_ui_egui::file_browser::Host {
        places,
        access_granted: Some(Arc::new(move || jvm::storage_access(&granted))),
        request_access: Some(Arc::new(move || jvm::request_storage_access(&request))),
        photos_granted: Some(Arc::new(move || jvm::photos_access(&photos))),
        request_photos: Some(Arc::new(move || jvm::request_photos_access(&ask_photos))),
    }
}

/// Where documents go in the PdfCraft running in this process: files to open in place, and the
/// bytes of documents only the system can read.
#[derive(Clone)]
struct Deliver {
    paths: Sender<String>,
    inbox: pdfcraft_ui_egui::Inbox,
    failed: pdfcraft_ui_egui::FailedInbox,
    ctx: egui::Context,
}

impl Deliver {
    /// Read `address` on a thread of its own and hand the document over.
    fn read_in_background(self, address: jvm::Address) {
        let spawned = std::thread::Builder::new().name("open-intent".into()).spawn(move || {
            match jvm::read(address) {
                Ok(jvm::Incoming::Path(path)) => {
                    let _ = self.paths.send(path);
                }
                Ok(jvm::Incoming::Bytes(name, bytes)) => {
                    self.inbox.lock().map(|mut q| q.push((name, bytes))).ok();
                }
                Ok(jvm::Incoming::TooLarge(name)) => {
                    self.failed.lock().map(|mut q| q.push((name, "the document is larger than 512 MB".into()))).ok();
                }
                Err(e) => {
                    log::warn!("reading a document another app handed over: {e}");
                    self.failed.lock().map(|mut q| q.push(("document".into(), e))).ok();
                }
            }
            self.ctx.request_repaint();
        });
        if let Err(e) = spawned {
            log::warn!("couldn't start reading the document: {e}");
        }
    }
}

/// The PdfCraft running in this process, for a later Activity to hand its document to, and the
/// task to bring forward.
#[derive(Clone)]
struct Running {
    deliver: Deliver,
    task: Option<i32>,
}

/// Set while an Activity runs PdfCraft (see [`hand_off`]).
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

fn running() -> std::sync::MutexGuard<'static, Option<Running>> {
    RUNNING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A NativeActivity never hears of an intent sent to it once it runs, so an "Open with" or
/// "Share" while PdfCraft runs starts a second Activity (the manifest's standard launch mode).
/// That one takes its document, hands it to the PdfCraft already running, brings it forward and
/// finishes (by returning from `android_main`). Returns false when no PdfCraft runs yet.
fn hand_off(android: &AndroidApp) -> bool {
    let Some(running) = running().clone() else { return false };
    log::info!("PdfCraft already runs: handing this document to it");
    let address = jvm::launch_address(android);
    if let Some(task) = running.task {
        jvm::move_task_to_front(android, task);
    }
    match address {
        Ok(Some(address)) => running.deliver.read_in_background(address),
        Ok(None) => {}
        Err(e) => log::warn!("the document handed to a second window: {e}"),
    }
    true
}

/// Read the document this Activity was started with (if any) off the UI thread and deliver it:
/// a file on shared storage opens in place through the returned poll, bytes go to the startup
/// inbox. Also makes this the PdfCraft later Activities hand their documents to.
fn launch_document(android: &AndroidApp, app: &PdfCraftApp, ctx: &egui::Context) -> pdfcraft_ui_egui::OsEventsFn {
    let (paths, receive) = channel::<String>();
    let startup = Deliver { paths: paths.clone(), inbox: app.startup_inbox.clone(), failed: app.failed_inbox.clone(), ctx: ctx.clone() };
    match jvm::launch_address(android) {
        Ok(Some(address)) => startup.read_in_background(address),
        Ok(None) => {}
        Err(e) => {
            log::warn!("the document the app was opened with: {e}");
            app.failed_inbox.lock().map(|mut q| q.push(("document".into(), e))).ok();
        }
    }
    // Documents handed over later open in front, like a deliberate Open.
    let later = Deliver { paths, inbox: app.inbox.clone(), failed: app.failed_inbox.clone(), ctx: ctx.clone() };
    *running() = Some(Running { deliver: later, task: jvm::task_id(android) });
    Box::new(move || poll(&receive))
}

/// Whether `events` ask to paste: egui-winit's paste, or the paste shortcut it dropped.
fn pastes(events: &[egui::Event]) -> bool {
    events.iter().any(|e| match e {
        egui::Event::Paste(_) => true,
        egui::Event::Key { key: egui::Key::V, pressed: true, modifiers, .. } => modifiers.command && !modifiers.alt,
        _ => false,
    })
}

fn poll(receive: &Receiver<String>) -> Vec<OsEvent> {
    let paths: Vec<String> = receive.try_iter().collect();
    if paths.is_empty() { Vec::new() } else { vec![OsEvent::Open(paths)] }
}

/// [`PdfCraftApp`] with what Android needs around it.
struct Shell {
    app: PdfCraftApp,
    android: AndroidApp,
    /// Text entry through the system's text views (`typing`).
    typing: crate::typing::Typing<jvm::TextDialog, jvm::TextMirror>,
    /// The colours last given to the system's bars (status, navigation).
    bars: Option<([u8; 3], [u8; 3])>,
    /// A paint of the bars failed: they are painted again, a few times at most.
    bars_failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    bars_tries: u8,
    /// The window's insets in pixels ([`jvm::read_window_insets`]), and when they were last asked.
    window_insets: std::sync::Arc<std::sync::Mutex<[i32; 4]>>,
    insets_asked: Option<std::time::Instant>,
}

/// How often the window's insets are asked again (a native app hears nothing when they change:
/// the keyboard opening, the device turning).
const INSETS_EVERY: std::time::Duration = std::time::Duration::from_millis(150);

/// The system's text views and the window's keyboard, for [`crate::typing`].
struct TypingHost<'a>(&'a AndroidApp);

impl crate::typing::Host for TypingHost<'_> {
    type Dialog = jvm::TextDialog;
    type Mirror = jvm::TextMirror;

    fn open_mirror(&mut self, request: crate::typing::Request) -> jvm::TextMirror {
        jvm::TextMirror::open(self.0, request)
    }

    fn poll_mirror(&mut self, mirror: &jvm::TextMirror) -> crate::typing::MirrorPoll {
        mirror.poll()
    }

    fn select_mirror(&mut self, mirror: &jvm::TextMirror, start: usize, end: usize) {
        mirror.select(start, end);
    }

    fn reshow_mirror(&mut self, mirror: &jvm::TextMirror) {
        mirror.show_keyboard();
    }

    fn close_mirror(&mut self, mirror: jvm::TextMirror) {
        // Dropping it puts it away.
        drop(mirror);
    }

    fn open(&mut self, request: crate::typing::Request) -> jvm::TextDialog {
        let t = |s: &str| pdfcraft_ui_egui::i18n::t(s).to_owned();
        jvm::TextDialog::open(self.0, request, t("OK"), t("Cancel"))
    }

    fn poll(&mut self, dialog: &jvm::TextDialog) -> Option<crate::typing::Outcome> {
        dialog.poll(self.0)
    }

    fn soft_keyboard(&mut self, show: bool) {
        if show {
            self.0.show_soft_input(false);
        } else {
            self.0.hide_soft_input(false);
        }
    }
}

/// Whether a hardware keyboard is attached and open: typing then goes straight to the window.
fn hardware_keyboard(android: &AndroidApp) -> bool {
    use winit::platform::android::activity::ndk::configuration::{Keyboard, KeysHidden};
    let config = android.config();
    config.keyboard() == Keyboard::Qwerty && config.keys_hidden() == KeysHidden::No
}

impl eframe::App for Shell {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // The back key closes what is open (a menu, a dialog), like Escape; it never leaves the
        // app behind unsaved work.
        for event in &mut raw.events {
            if let egui::Event::Key { key, .. } = event
                && *key == egui::Key::BrowserBack
            {
                *key = egui::Key::Escape;
            }
        }
        // egui-winit has only an in-app clipboard here, and sends no paste at all while it is
        // empty: paste what the system holds instead.
        if pastes(&raw.events)
            && let Some(system) = jvm::clipboard_text(&self.android)
        {
            raw.events.retain(|e| !matches!(e, egui::Event::Paste(_)));
            raw.events.push(egui::Event::Paste(system));
        }
        self.typing.queue(raw);
        if self.insets_asked.is_none_or(|at| at.elapsed() >= INSETS_EVERY) {
            self.insets_asked = Some(std::time::Instant::now());
            let window = ctx.clone();
            jvm::read_window_insets(&self.android, self.window_insets.clone(), move || window.request_repaint());
        }
        raw.safe_area_insets = Some(self.uncovered(raw));
        raw.system_theme = Some(match self.android.config().ui_mode_night() {
            winit::platform::android::activity::ndk::configuration::UiModeNight::Yes => egui::Theme::Dark,
            _ => egui::Theme::Light,
        });
        self.app.raw_input_hook(ctx, raw);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.app.save(storage);
        // Android may stop the app at any time once it is in the background (eframe saves when it
        // is suspended): keep unsaved documents too.
        self.app.autosave_now();
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.app.logic(ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
        let ctx = ui.ctx().clone();
        let copied: Option<String> = ctx.output(|o| {
            o.commands
                .iter()
                .filter_map(|c| match c {
                    egui::OutputCommand::CopyText(text) => Some(text.clone()),
                    _ => None,
                })
                .next_back()
        });
        let reading = self.typing.reading();
        // Copies go to the system clipboard as well as egui's own, except the one that reads a
        // field for the text dialog.
        if !reading && let Some(text) = &copied {
            jvm::set_clipboard_text(&self.android, text.clone());
        }
        // The system's bars in the colours of the screen's top and bottom, when they change.
        let bars = self.app.phone_system_bars().map(|(top, bottom)| ([top.r(), top.g(), top.b()], [bottom.r(), bottom.g(), bottom.b()]));
        // A paint that failed is tried again on a later frame (three tries for the same colours).
        if self.bars_failed.swap(false, std::sync::atomic::Ordering::SeqCst) && self.bars_tries < 3 {
            self.bars = None;
        }
        if let Some((top, bottom)) = bars
            && self.bars != bars
        {
            self.bars_tries = if self.bars.is_none() { self.bars_tries.saturating_add(1) } else { 0 };
            self.bars = bars;
            jvm::set_system_bars(&self.android, top, bottom, self.bars_failed.clone());
        }
        let system_views = !hardware_keyboard(&self.android);
        self.typing.frame(&ctx, copied.filter(|_| reading), system_views, &mut TypingHost(&self.android));
    }
}

impl Shell {
    /// How much of the window the system covers (the soft keyboard, the status and navigation
    /// bars, a cutout), in points: the window minus the activity's content rectangle, or the
    /// window's own insets where they say more (Android 15 and later draw the app under the bars,
    /// and its content rectangle is then the whole window). winit doesn't pass content-rectangle
    /// changes on, so they are read every frame.
    fn uncovered(&self, raw: &egui::RawInput) -> egui::SafeAreaInsets {
        let none = egui::SafeAreaInsets::default();
        let Some(window) = self.android.native_window() else { return none };
        let (w, h) = (window.width(), window.height());
        let Some(screen) = raw.screen_rect else { return none };
        if w <= 0 || h <= 0 || screen.width() <= 0.0 || screen.height() <= 0.0 {
            return none;
        }
        let content = self.android.content_rect();
        let points = [screen.width(), screen.height()];
        let from_content = crate::screen::insets([content.left, content.top, content.right, content.bottom], [w, h], points);
        let pixels = *self.window_insets.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let [left, top, right, bottom] = crate::screen::widest(from_content, crate::screen::window_insets(pixels, [w, h], points));
        egui::SafeAreaInsets(egui::epaint::MarginF32 { left, right, top, bottom })
    }
}
