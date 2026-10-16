#[test]
fn winit_lets_each_android_activity_create_its_event_loop() {
    // vendor/README.md, winit patch (12). Relaunching needs a device; this keeps the patch in the
    // vendored copy, which the workspace's tests don't build for Android.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let android = std::fs::read_to_string(root.join("vendor/winit/src/platform_impl/android/mod.rs")).unwrap();
    assert!(android.contains("impl<T: 'static> Drop for EventLoop<T> {"));
    assert!(android.contains("crate::event_loop::EventLoopBuilder::<()>::allow_event_loop_recreation();"));
    let event_loop = std::fs::read_to_string(root.join("vendor/winit/src/event_loop.rs")).unwrap();
    assert!(event_loop.contains("#[cfg(any(web_platform, android_platform))]\n    pub(crate) fn allow_event_loop_recreation()"));
    let workspace = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(workspace.contains("winit = { path = \"vendor/winit\" }"), "the vendored winit is the one built");
}
