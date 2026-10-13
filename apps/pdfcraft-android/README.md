# pdfcraft-android

PdfCraft on Android phones and tablets: the same egui shell as the desktop app
(`crates/ui-egui`), run in a `NativeActivity`, so every tool, dialog and panel of the desktop app is
there. There is no Java or Kotlin code and no Gradle: the app is one native library,
`libpdfcraft_android.so`, packaged by `cargo xtask android`.

Status: **preview**. It builds, passes clippy for Android and its host-side tests, and packages as a
signed APK; it has not yet been run through the test suite on a device, so expect rough edges (see
[Known limits](#known-limits)).

## Build

Needs the Rust target, the Android NDK (r26 or later) and the Android SDK (build-tools and a
platform, API 34), and a JDK (for `apksigner` and `keytool`):

```sh
rustup target add aarch64-linux-android
sdkmanager "platforms;android-34" "build-tools;34.0.0" "ndk;27.2.12479018"
export ANDROID_HOME=~/Android/Sdk ANDROID_NDK_HOME=$ANDROID_HOME/ndk/27.2.12479018
cargo xtask models            # optional: the OCR models, so Recognize Text works
cargo xtask android           # dist/android/pdfcraft-<version>-arm64-v8a.apk
```

- `--abi arm64-v8a|armeabi-v7a|x86_64|x86` (repeatable) or `--all-abis` choose the ABIs (one APK
  holds them all); the default is arm64-v8a, which nearly every phone sold since 2017 runs.
  `rustup target add` each one first.
- `--debug` builds without optimizations (faster to build, much slower to run).
- `--check` only runs clippy (`-D warnings`) on this crate and `pdfcraft-ui-egui` for each ABI, as
  CI does; it needs the NDK but not the SDK.
- `CRAFT_FONTS_DIR` embeds the Japanese and other craft-fonts faces, as on the desktop.

The APK is signed with the standard debug key (`~/.android/debug.keystore`, created when missing),
which is fine for installing it yourself. To sign with your own key, set
`PDFCRAFT_ANDROID_KEYSTORE` (the keystore file), `PDFCRAFT_ANDROID_KEYSTORE_PASS` and, if it isn't
`pdfcraft`, `PDFCRAFT_ANDROID_KEY_ALIAS`. Updates must be signed with the same key as the installed
app.

CI builds a debug-signed APK on every pull request (`.github/workflows/ci.yml`, job "Android"); it is
attached to the run as the `pdfcraft-android` artifact.

## Install

Copy the APK to the device and open it (allow installing apps from that source when Android asks),
or with USB debugging on: `adb install -r dist/android/pdfcraft-*.apk`. Logs:
`adb logcat -s PdfCraft`.

Android 8.0 (API 26) or later, 64-bit ARM by default.

## What is different from the desktop app

- **Files.** Android has no file picker a native activity can show (the system picker answers only
  through `onActivityResult`, which a `NativeActivity` never receives), so every Open, Save as,
  folder and multi-file choice uses PdfCraft's own browser (`crates/ui-egui/src/file_browser.rs`).
  It starts in Download and offers Documents, the internal storage, SD cards and PdfCraft's own
  folder. To reach files outside PdfCraft's folder, Android 11 and later need "All files access":
  the browser has an **Allow access** button that opens that settings page (older Android asks for
  the storage permission). Because files are real paths, Save writes the file in place, recent files
  and Reopen last session work, and changed files reload, as on the desktop.
- **Open with / Share.** PdfCraft appears in "Open with" and "Share" for PDFs. A file on shared
  storage opens in place (Save writes it back); a document only another app can read (a mail
  attachment) opens as an unsaved copy, and Save asks where to keep it. This works while PdfCraft
  already runs too: a `NativeActivity` never hears of a new intent, so Android starts a second
  window, which hands the document to the running PdfCraft, brings it forward and closes.
- **Back key** closes the open menu or dialog, like Escape. It never closes the app, so unsaved work
  isn't dropped without the usual question.
- **Typing.** Tapping a text field opens Android's own text box with the field's text, so every
  keyboard works: accents, other alphabets, predictive text, voice typing (a `NativeActivity`'s
  window only gets key events, and loses the characters a keyboard map can't produce). **OK** puts
  the text back and acts as the keyboard's Done: in a one-line field it also presses Enter, so a page
  number, a zoom, a search or a file name takes effect (in the command palette, it runs the top
  match). **Cancel** leaves the field as it was. A password is never shown in the box's prefill.
  With a hardware keyboard, typing goes straight to the field; the window keeps the part the soft
  keyboard leaves free.
- **Clipboard.** Copied text goes to the system clipboard and pasted text comes from it. Images and
  files copied in PdfCraft stay in PdfCraft (Android takes those only from a content provider, which
  needs Java); use Save Image As.
- **Print.** Android has no print spooler a native app can reach without Java, so the Print dialog's
  first printer is **Another app**: the print-ready PDF (with n-up, booklet, poster and the other
  layouts) is saved in Download/PdfCraft and opens in the app you pick, which prints it (a PDF
  viewer's Print reaches every printer Android knows). Copies, two-sided printing and colour are set
  in that app. **Save as PDF** is still there.
- **Text recognition.** The OCR models ship inside the APK (when `cargo xtask models` fetched them
  before the build) and are unpacked into the app's storage on first start.
- **Settings and recovery.** Kept in the app's private storage; unsaved documents are autosaved
  when the app goes to the background, and offered again after Android stops it.
- **Small screens.** On a phone, Auto draws the interface at 80% so more of the desktop layout fits,
  and the first start keeps the tool panel closed so the page has a portrait screen to itself (the
  Tools tab opens it); Preferences ▸ Interface size changes the size. When the mode tabs don't fit
  they fold into one menu of the current mode, and the search box into its icon; when the side rail
  is short its panel buttons scroll above the page controls.
- **Dark theme** follows the system's.

## Known limits

- Not yet tested on a device: please report what breaks.
- The phone layout is the desktop layout, scaled and folded where it would overlap; there is no
  layout designed for small portrait screens yet (tablets and landscape suit it best).
- No screen reader: eframe's AccessKit bridge needs `GameActivity` (Java), not `NativeActivity`.
- A text field takes typing through Android's text box, not in place: the box covers the page while
  it shows, and the field's text is replaced as a whole when OK is pressed.
- If the text box can't be shown, typing falls back to key events for the session, where keyboards
  that compose words (accents, other alphabets, predictive text) may lose characters.
- Images and files can't go to the system clipboard; printing goes through another app rather than
  a printer list; "Reveal in file manager" is unavailable.
- `targetSdkVersion` is 34: Android 15 makes apps targeting 35 draw under the system bars, and a
  `NativeActivity` reports no insets for them. Google Play requires 35 for new apps; this APK is for
  installing directly.

## Unsafe code

The workspace forbids `unsafe`. This crate only denies it, because Android needs two things safe
Rust can't express, each allowed on its own item with its reason:

1. `android_main` (`src/lib.rs`) is exported with `#[unsafe(no_mangle)]`: android-activity calls the
   app by that name.
2. `with_activity` (`src/jvm.rs`) turns the activity's raw JNI global reference, which
   android-activity keeps alive, into a `jni` object (`Env::as_cast_raw`, which still checks its type
   at run time).

Every JNI call after that goes through `jni`'s checked API. This is an exception to AGENTS.md §4,
signed off by the repository owner on 2026-10-13 and listed there; nothing else in the crate may
allow unsafe code.

## Layout

- `src/lib.rs`: the entry point.
- `src/shell.rs`: the `eframe::App` around `PdfCraftApp` (input, keyboard, clipboard, insets, saving
  on suspend), the file browser's places, OCR model unpacking, and the start-up document.
- `src/jvm.rs`: JNI (storage access, the start-up intent and `ContentResolver`, handing a document to
  the running window, the clipboard, the system text box).
- `src/typing.rs`: typing through the system text box (the egui side, host-tested on real egui text
  fields with a stand-in box).
- `src/storage.rs`, `src/screen.rs`: plain functions with host tests (which `content://` and
  `file://` addresses are shared-storage paths, confined to `/storage` without `..`; display names;
  removable cards; the keyboard's inset).
- `packaging/android/AndroidManifest.xml`: the manifest; `xtask/src/android.rs`: the APK build.
