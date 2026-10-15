#[test]
fn winit_leaves_the_keys_to_a_text_view_while_it_types() {
    // vendor/README.md, winit patch (13). Typing itself needs a device; this keeps the patch in
    // the vendored copy, which the workspace's tests don't build for Android.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let android = std::fs::read_to_string(root.join("vendor/winit/src/platform_impl/android/mod.rs")).unwrap();
    assert!(android.contains("pub(crate) static KEYS_TO_VIEWS: AtomicBool = AtomicBool::new(false);"));
    assert!(android.contains(
        "if KEYS_TO_VIEWS.load(Ordering::Relaxed) && key.key_code() != Keycode::Back {\n                    return InputStatus::Unhandled;"
    ));
    // And the soft keyboard: winit neither shows nor hides it for an app that does.
    assert!(android.contains("pub fn set_ime_allowed(&self, allowed: bool) {\n        // PdfCraft patch (13)"));
    assert!(android.contains(
        "if APP_SOFT_KEYBOARD.load(Ordering::Relaxed) || KEYS_TO_VIEWS.load(Ordering::Relaxed) {\n            return;\n        }\n        if allowed {"
    ));
    let platform = std::fs::read_to_string(root.join("vendor/winit/src/platform/android.rs")).unwrap();
    assert!(platform.contains("pub fn set_keys_to_views(on: bool) {"));
    assert!(platform.contains("pub fn set_app_soft_keyboard(on: bool) {"));
    // The shell says it handles the keyboard itself before the window opens.
    let shell = std::fs::read_to_string(root.join("apps/pdfcraft-android/src/shell.rs")).unwrap();
    assert!(shell.contains("winit::platform::android::set_app_soft_keyboard(true);"));
    // The app turns it on with its text view, and off when the view goes.
    let jvm = std::fs::read_to_string(root.join("apps/pdfcraft-android/src/jvm.rs")).unwrap();
    assert!(jvm.contains("winit::platform::android::set_keys_to_views(true);"));
    assert!(jvm.contains("winit::platform::android::set_keys_to_views(false);"));
}
