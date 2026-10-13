//! PdfCraft on Android: the same egui shell as the desktop app, in a `NativeActivity`, so every
//! tool is there. What Android does differently lives here: the entry point, where settings and
//! recovered work are kept, the document another app opens PdfCraft with, the back key, the
//! room the soft keyboard leaves, the system clipboard, and storage access for the in-app file
//! browser (which replaces the system pickers; see `pdfcraft_ui_egui::file_browser`).
//!
//! Build the APK with `cargo xtask android` (README.md).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod screen;
pub mod storage;
pub mod typing;

#[cfg(target_os = "android")]
mod jvm;
#[cfg(target_os = "android")]
mod shell;

/// Where Android starts the app: `android-activity` calls this function by name once the
/// activity is created, on a thread of its own.
#[cfg(target_os = "android")]
// The name is the contract with android-activity, so it can't be mangled: an unsafe attribute,
// allowed for this item alone (README.md, "Unsafe code").
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn android_main(app: winit::platform::android::activity::AndroidApp) {
    shell::run(app);
}
