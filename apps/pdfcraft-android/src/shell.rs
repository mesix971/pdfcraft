//! The Android shell around [`PdfCraftApp`]: the window, settings and recovery folders, the
//! document the app was opened with, the back key, the soft keyboard's room, the clipboard,
//! and the in-app file browser's places and storage access.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use pdfcraft_ui_egui::file_browser::{Place, PlaceKind};
use pdfcraft_ui_egui::{OsEvent, PdfCraftApp, RecoveryStore};
use winit::platform::android::activity::AndroidApp;

use crate::jvm;
use crate::storage;

/// Settings are kept under this key (the desktop app uses the same one).
const STORAGE_KEY: &str = "pdfcraft";
/// Below this smallest width (in dp) the screen is a phone's.
const PHONE_DP: i32 = 600;
/// The interface size `Auto` gives a phone, so more of the desktop layout fits on its screen.
const PHONE_SCALE: f32 = 0.8;

/// Start PdfCraft in `android`'s activity. Returns when the activity is destroyed.
pub fn run(android: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("PdfCraft"));
    // Last-resort guard (AGENTS.md §4): the engine catches panics in commands, opens and saves;
    // this records every panic, caught or not.
    std::panic::set_hook(Box::new(|info| log::error!("internal error: {info}")));
    log::info!("PdfCraft {} on Android (API {})", env!("CARGO_PKG_VERSION"), AndroidApp::sdk_version());

    let config = android.config();
    if let Some(language) = config.language() {
        let tag = config.country().map_or_else(|| language.clone(), |country| format!("{language}_{country}"));
        pdfcraft_ui_egui::set_system_locale(&tag);
    }
    let phone = config.smallest_screen_width_dp().is_some_and(|dp| dp > 0 && dp < PHONE_DP);
    // App-private storage: settings, the log of recovered work. Shared storage holds documents.
    let data = android.internal_data_path();
    let persistence_path = data.as_ref().map(|d| d.join("settings").join("app.ron"));
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
            app.system_text_scale = PHONE_SCALE;
        }
        app.file_browser_host = browser_host(&android);
        let clipboard = android.clone();
        app.clipboard_text = Some(Box::new(move || jvm::clipboard_text(&clipboard)));
        if let Some(dir) = &data {
            app.enable_recovery(RecoveryStore::new(dir.join("recovery")));
            install_models(&android, dir.join("models"));
        }
        app.os_events = Some(launch_document(&android, &app, &cc.egui_ctx));
        app.reopen_last_files(&[]);
        Ok(Box::new(Shell { app, android: android.clone(), keyboard: false }))
    });
    if let Err(e) = eframe::run_native("PdfCraft", options, creator) {
        log::error!("PdfCraft stopped: {e}");
    }
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
    pdfcraft_ui_egui::file_browser::Host {
        places,
        access_granted: Some(Arc::new(move || jvm::storage_access(&granted))),
        request_access: Some(Arc::new(move || jvm::request_storage_access(&request))),
    }
}

/// Read the document the app was opened with (if any) off the UI thread, and deliver it: a file
/// on shared storage opens in place through the returned poll; bytes go to the startup inbox.
fn launch_document(android: &AndroidApp, app: &PdfCraftApp, ctx: &egui::Context) -> pdfcraft_ui_egui::OsEventsFn {
    let (send, receive) = channel::<String>();
    let android = android.clone();
    let inbox = app.startup_inbox.clone();
    let failed = app.failed_inbox.clone();
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new().name("open-intent".into()).spawn(move || {
        match jvm::launch_document(&android) {
            Ok(Some(jvm::Incoming::Path(path))) => {
                let _ = send.send(path);
            }
            Ok(Some(jvm::Incoming::Bytes(name, bytes))) => {
                inbox.lock().map(|mut q| q.push((name, bytes))).ok();
            }
            Ok(Some(jvm::Incoming::TooLarge(name))) => {
                failed.lock().map(|mut q| q.push((name, "the document is larger than 512 MB".into()))).ok();
            }
            Ok(None) => {}
            Err(e) => {
                log::warn!("reading the document the app was opened with: {e}");
                failed.lock().map(|mut q| q.push(("document".into(), e))).ok();
            }
        }
        ctx.request_repaint();
    });
    if let Err(e) = spawned {
        log::warn!("couldn't start reading the opened document: {e}");
    }
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
    /// Whether the soft keyboard was asked for last frame.
    keyboard: bool,
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
        // Copies go to the system clipboard as well as egui's own.
        let copied: Vec<String> = ctx.output(|o| {
            o.commands
                .iter()
                .filter_map(|c| match c {
                    egui::OutputCommand::CopyText(text) => Some(text.clone()),
                    _ => None,
                })
                .collect()
        });
        if let Some(text) = copied.into_iter().last() {
            jvm::set_clipboard_text(&self.android, text);
        }
        // winit asks for the keyboard implicitly, which some devices ignore: ask outright while
        // a text field has focus, and again when the field is tapped after the keyboard was put
        // away (focus didn't change, so nothing else would bring it back).
        let want = ctx.egui_wants_keyboard_input();
        let released = ctx.input(|i| if i.pointer.any_released() { i.pointer.interact_pos() } else { None });
        let field = ctx.memory(|m| m.focused()).and_then(|id| ctx.read_response(id));
        let tapped = want && released.zip(field).is_some_and(|(at, field)| field.rect.contains(at));
        if want != self.keyboard || tapped {
            if want {
                self.android.show_soft_input(false);
            } else {
                self.android.hide_soft_input(false);
            }
            self.keyboard = want;
        }
    }
}

impl Shell {
    /// How much of the window the system covers (the soft keyboard, the status and navigation
    /// bars), in points: the window minus the activity's content rectangle. winit doesn't pass
    /// content-rectangle changes on, so they are read every frame.
    fn uncovered(&self, raw: &egui::RawInput) -> egui::SafeAreaInsets {
        let none = egui::SafeAreaInsets::default();
        let Some(window) = self.android.native_window() else { return none };
        let (w, h) = (window.width(), window.height());
        let Some(screen) = raw.screen_rect else { return none };
        if w <= 0 || h <= 0 || screen.width() <= 0.0 || screen.height() <= 0.0 {
            return none;
        }
        let content = self.android.content_rect();
        let [left, top, right, bottom] =
            crate::screen::insets([content.left, content.top, content.right, content.bottom], [w, h], [screen.width(), screen.height()]);
        egui::SafeAreaInsets(egui::epaint::MarginF32 { left, right, top, bottom })
    }
}
