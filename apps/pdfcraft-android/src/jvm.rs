//! The Java side of Android, through JNI: storage access, the document the app was opened with,
//! and the clipboard.
//!
//! Every call goes through `jni`'s checked API (signatures and argument types are verified, Java
//! exceptions come back as errors), so a failure is an `Err` the caller reports or ignores. The
//! one unsafe step is turning the activity's raw global reference into a JNI object
//! ([`with_activity`]).

use std::io::Read;

use jni::objects::{JByteArray, JObject, JString};
use jni::refs::Global;
use jni::{Env, JValue, jni_sig, jni_str};
use winit::platform::android::activity::AndroidApp;

/// The most bytes read from a document handed over by another app.
const MAX_DOCUMENT_BYTES: usize = 512 << 20;
/// How much one `InputStream.read` asks for.
const CHUNK: usize = 1 << 16;
/// Android 11 (API 30): "all files access" replaced the storage permissions.
const API_R: i32 = 30;
/// Android 13 (API 33): photos have a permission of their own.
const API_T: i32 = 33;

/// A JNI call that failed, or a read that did.
#[derive(Debug)]
pub enum Failure {
    Jni(jni::errors::Error),
    Io(std::io::Error),
}

impl From<jni::errors::Error> for Failure {
    fn from(e: jni::errors::Error) -> Self {
        Self::Jni(e)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Jni(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

type Jni<T> = Result<T, Failure>;

/// Run `f` with a JNI environment for this thread and the app's activity.
fn with_activity<T>(app: &AndroidApp, f: impl FnOnce(&mut Env, &JObject) -> Jni<T>) -> Result<T, String> {
    // android-activity creates the singleton before `android_main` runs (same jni version).
    let vm = jni::JavaVM::singleton().map_err(|e| e.to_string())?;
    let raw: jni::sys::jobject = app.activity_as_ptr().cast();
    vm.attach_current_thread(|env| -> Jni<T> {
        // SAFETY: `activity_as_ptr` is a JNI global reference to the activity, which
        // android-activity holds for as long as the `AndroidApp` exists (`app` is borrowed for
        // this whole call). The cast only borrows it, never deletes it, and checks at run time
        // that it is a `java.lang.Object`.
        #[allow(unsafe_code)]
        let activity = unsafe { env.as_cast_raw::<Global<JObject<'static>>>(&raw)? };
        f(env, &activity)
    })
    .map_err(|e| e.to_string())
}

/// `obj.toString()`, or `None` for a null reference.
fn string_of(env: &mut Env, obj: &JObject) -> Jni<Option<String>> {
    if obj.is_null() {
        return Ok(None);
    }
    let text = env.call_method(obj, jni_str!("toString"), jni_sig!("()Ljava/lang/String;"), &[])?.l()?;
    let text: JString = env.cast_local::<JString>(text)?;
    Ok(Some(text.try_to_string(env)?))
}

/// Whether the app may read and write anywhere on shared storage: "all files access" from
/// Android 11, the storage permission before.
pub fn storage_access(app: &AndroidApp) -> bool {
    let result = with_activity(app, |env, activity| {
        if AndroidApp::sdk_version() >= API_R {
            Ok(env.call_static_method(jni_str!("android/os/Environment"), jni_str!("isExternalStorageManager"), jni_sig!("()Z"), &[])?.z()?)
        } else {
            let permission = env.new_string("android.permission.WRITE_EXTERNAL_STORAGE")?;
            let granted =
                env.call_method(activity, jni_str!("checkSelfPermission"), jni_sig!("(Ljava/lang/String;)I"), &[JValue::Object(&permission)])?.i()?;
            // PackageManager.PERMISSION_GRANTED
            Ok(granted == 0)
        }
    });
    result.unwrap_or_else(|e| {
        log::warn!("storage access check: {e}");
        false
    })
}

/// The permission that lets the app read the device's photos: their own from Android 13, the
/// storage one before.
fn photos_permission() -> &'static str {
    if AndroidApp::sdk_version() >= API_T { "android.permission.READ_MEDIA_IMAGES" } else { "android.permission.READ_EXTERNAL_STORAGE" }
}

/// Whether the app may read the device's photos (all files access covers them too).
pub fn photos_access(app: &AndroidApp) -> bool {
    if storage_access(app) {
        return true;
    }
    let result = with_activity(app, |env, activity| {
        let permission = env.new_string(photos_permission())?;
        let granted =
            env.call_method(activity, jni_str!("checkSelfPermission"), jni_sig!("(Ljava/lang/String;)I"), &[JValue::Object(&permission)])?.i()?;
        // PackageManager.PERMISSION_GRANTED
        Ok(granted == 0)
    });
    result.unwrap_or_else(|e| {
        log::warn!("photos access check: {e}");
        false
    })
}

/// The photos permission was asked for in this run.
static PHOTOS_ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Ask for the photos permission: the system's own question ("Allow PdfCraft to access photos?").
/// Refused for good (the system asks no more), the app's page in the settings opens instead, where
/// it can still be allowed. The answer is read again by [`photos_access`] (a NativeActivity never
/// hears it directly).
pub fn request_photos_access(app: &AndroidApp) {
    let main = app.clone();
    let asked_before = PHOTOS_ASKED.swap(true, std::sync::atomic::Ordering::SeqCst);
    app.run_on_java_main_thread(Box::new(move || {
        let result = with_activity(&main, |env, activity| {
            let name = env.new_string(photos_permission())?;
            // After a first refusal the system asks again (and says so here); after the second,
            // or "don't ask again", it doesn't.
            let can_ask = !asked_before
                || env
                    .call_method(
                        activity,
                        jni_str!("shouldShowRequestPermissionRationale"),
                        jni_sig!("(Ljava/lang/String;)Z"),
                        &[JValue::Object(&name)],
                    )?
                    .z()?;
            if !can_ask {
                return open_app_settings(env, activity);
            }
            let class = env.find_class(jni_str!("java/lang/String"))?;
            let permissions = env.new_object_array(1, &class, JObject::null())?;
            permissions.set_element(env, 0, &name)?;
            env.call_method(
                activity,
                jni_str!("requestPermissions"),
                jni_sig!("([Ljava/lang/String;I)V"),
                &[JValue::Object(&permissions), JValue::Int(2)],
            )?;
            Ok(())
        });
        if let Err(e) = result {
            log::warn!("asking for the photos: {e}");
        }
    }));
}

/// Open PdfCraft's page in the system settings (its permissions are a tap away there).
fn open_app_settings(env: &mut Env, activity: &JObject) -> Jni<()> {
    let package = env.call_method(activity, jni_str!("getPackageName"), jni_sig!("()Ljava/lang/String;"), &[])?.l()?;
    let package = string_of(env, &package)?.unwrap_or_default();
    let action = env.new_string("android.settings.APPLICATION_DETAILS_SETTINGS")?;
    let address = env.new_string(format!("package:{package}"))?;
    let uri = env
        .call_static_method(
            jni_str!("android/net/Uri"),
            jni_str!("parse"),
            jni_sig!("(Ljava/lang/String;)Landroid/net/Uri;"),
            &[JValue::Object(&address)],
        )?
        .l()?;
    let intent = env.new_object(
        jni_str!("android/content/Intent"),
        jni_sig!("(Ljava/lang/String;Landroid/net/Uri;)V"),
        &[JValue::Object(&action), JValue::Object(&uri)],
    )?;
    env.call_method(activity, jni_str!("startActivity"), jni_sig!("(Landroid/content/Intent;)V"), &[JValue::Object(&intent)])?;
    Ok(())
}

/// Show the system page where the user allows that access (Android 11 and later), or ask for the
/// storage permission (before). The answer is read again by [`storage_access`].
pub fn request_storage_access(app: &AndroidApp) {
    let main = app.clone();
    // Starting activities and asking for permissions belong on the UI thread.
    app.run_on_java_main_thread(Box::new(move || {
        let result = with_activity(&main, |env, activity| {
            if AndroidApp::sdk_version() >= API_R {
                let package = env.call_method(activity, jni_str!("getPackageName"), jni_sig!("()Ljava/lang/String;"), &[])?.l()?;
                let package = string_of(env, &package)?.unwrap_or_default();
                let action = env.new_string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")?;
                let address = env.new_string(format!("package:{package}"))?;
                let uri = env
                    .call_static_method(
                        jni_str!("android/net/Uri"),
                        jni_str!("parse"),
                        jni_sig!("(Ljava/lang/String;)Landroid/net/Uri;"),
                        &[JValue::Object(&address)],
                    )?
                    .l()?;
                let intent = env.new_object(
                    jni_str!("android/content/Intent"),
                    jni_sig!("(Ljava/lang/String;Landroid/net/Uri;)V"),
                    &[JValue::Object(&action), JValue::Object(&uri)],
                )?;
                let opened =
                    env.call_method(activity, jni_str!("startActivity"), jni_sig!("(Landroid/content/Intent;)V"), &[JValue::Object(&intent)]);
                if let Err(e) = opened {
                    // Some devices have only the list of every app's access, not the page for one.
                    log::info!("no all-files-access page for this app ({e}); showing the list");
                    env.exception_clear();
                    let general = env.new_string("android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION")?;
                    let intent =
                        env.new_object(jni_str!("android/content/Intent"), jni_sig!("(Ljava/lang/String;)V"), &[JValue::Object(&general)])?;
                    env.call_method(activity, jni_str!("startActivity"), jni_sig!("(Landroid/content/Intent;)V"), &[JValue::Object(&intent)])?;
                }
            } else {
                let class = env.find_class(jni_str!("java/lang/String"))?;
                let permissions = env.new_object_array(2, &class, JObject::null())?;
                for (i, name) in ["android.permission.READ_EXTERNAL_STORAGE", "android.permission.WRITE_EXTERNAL_STORAGE"].into_iter().enumerate() {
                    let name = env.new_string(name)?;
                    permissions.set_element(env, i, &name)?;
                }
                env.call_method(
                    activity,
                    jni_str!("requestPermissions"),
                    jni_sig!("([Ljava/lang/String;I)V"),
                    &[JValue::Object(&permissions), JValue::Int(1)],
                )?;
            }
            Ok(())
        });
        if let Err(e) = result {
            log::warn!("asking for storage access: {e}");
        }
    }));
}

/// A document another app handed over ("Open with", "Share").
pub enum Incoming {
    /// A file on shared storage, opened in place.
    Path(String),
    /// A document only readable through the system: its name and bytes.
    Bytes(String, Vec<u8>),
    /// A document too large to read ([`MAX_DOCUMENT_BYTES`]), by name.
    TooLarge(String),
}

/// Where the document an Activity was started with is: a file on shared storage, or an address
/// only the system can read (with what reads it, held past the Activity).
pub enum Address {
    Path(String),
    Content { address: String, uri: Global<JObject<'static>>, resolver: Global<JObject<'static>> },
}

/// The document `app`'s Activity was started with, if any: the address of a VIEW or EDIT intent,
/// or the stream of a SEND. Quick (nothing is read yet), so a second Activity can take it before
/// it finishes; [`read`] reads it afterwards, on any thread.
pub fn launch_address(app: &AndroidApp) -> Result<Option<Address>, String> {
    with_activity(app, |env, activity| {
        let intent = env.call_method(activity, jni_str!("getIntent"), jni_sig!("()Landroid/content/Intent;"), &[])?.l()?;
        if intent.is_null() {
            return Ok(None);
        }
        let action = env.call_method(&intent, jni_str!("getAction"), jni_sig!("()Ljava/lang/String;"), &[])?.l()?;
        let action = string_of(env, &action)?.unwrap_or_default();
        let uri = match action.as_str() {
            "android.intent.action.VIEW" | "android.intent.action.EDIT" => {
                env.call_method(&intent, jni_str!("getData"), jni_sig!("()Landroid/net/Uri;"), &[])?.l()?
            }
            "android.intent.action.SEND" => {
                let key = env.new_string("android.intent.extra.STREAM")?;
                env.call_method(
                    &intent,
                    jni_str!("getParcelableExtra"),
                    jni_sig!("(Ljava/lang/String;)Landroid/os/Parcelable;"),
                    &[JValue::Object(&key)],
                )?
                .l()?
            }
            _ => return Ok(None),
        };
        let Some(address) = string_of(env, &uri)? else { return Ok(None) };
        if let Some(path) = crate::storage::path_for_uri(&address).filter(|p| std::fs::File::open(p).is_ok()) {
            return Ok(Some(Address::Path(path.to_string_lossy().into_owned())));
        }
        // The application's resolver outlives the Activity; the URI grant is the app's.
        let context = env.call_method(activity, jni_str!("getApplicationContext"), jni_sig!("()Landroid/content/Context;"), &[])?.l()?;
        let resolver = env.call_method(&context, jni_str!("getContentResolver"), jni_sig!("()Landroid/content/ContentResolver;"), &[])?.l()?;
        Ok(Some(Address::Content { address, uri: env.new_global_ref(&uri)?, resolver: env.new_global_ref(&resolver)? }))
    })
}

/// Read the document at `address` (whole, so off the UI thread).
pub fn read(address: Address) -> Result<Incoming, String> {
    let (address, uri, resolver) = match address {
        Address::Path(path) => return Ok(Incoming::Path(path)),
        Address::Content { address, uri, resolver } => (address, uri, resolver),
    };
    let vm = jni::JavaVM::singleton().map_err(|e| e.to_string())?;
    vm.attach_current_thread(|env| -> Jni<Incoming> {
        let shown = display_name(env, &resolver, &uri).unwrap_or_else(|e| {
            log::warn!("the opened document's name: {e}");
            None
        });
        let name = crate::storage::display_name(shown.as_deref(), &address);
        Ok(match read_uri(env, &resolver, &uri)? {
            Some(bytes) => Incoming::Bytes(name, bytes),
            None => Incoming::TooLarge(name),
        })
    })
    .map_err(|e| e.to_string())
}

/// The task `app`'s Activity runs in (`Activity.getTaskId`).
pub fn task_id(app: &AndroidApp) -> Option<i32> {
    with_activity(app, |env, activity| Ok(env.call_method(activity, jni_str!("getTaskId"), jni_sig!("()I"), &[])?.i()?))
        .map_err(|e| log::warn!("the app's task: {e}"))
        .ok()
}

/// Bring task `task` (the running PdfCraft's) to the front, from another of the app's Activities
/// (`ActivityManager.moveTaskToFront`, which the REORDER_TASKS permission allows).
pub fn move_task_to_front(app: &AndroidApp, task: i32) {
    let result = with_activity(app, |env, activity| {
        let name = env.new_string("activity")?;
        let manager = env
            .call_method(activity, jni_str!("getSystemService"), jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"), &[JValue::Object(&name)])?
            .l()?;
        if !manager.is_null() {
            env.call_method(&manager, jni_str!("moveTaskToFront"), jni_sig!("(II)V"), &[JValue::Int(task), JValue::Int(0)])?;
        }
        Ok(())
    });
    if let Err(e) = result {
        log::warn!("bringing PdfCraft to the front: {e}");
    }
}

/// The name the content provider gives the document (`OpenableColumns.DISPLAY_NAME`).
fn display_name(env: &mut Env, resolver: &JObject, uri: &JObject) -> Jni<Option<String>> {
    let null = JObject::null();
    let cursor = env
        .call_method(
            resolver,
            jni_str!("query"),
            jni_sig!("(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;"),
            &[JValue::Object(uri), JValue::Object(&null), JValue::Object(&null), JValue::Object(&null), JValue::Object(&null)],
        )?
        .l()?;
    if cursor.is_null() {
        return Ok(None);
    }
    let name = first_display_name(env, &cursor);
    // Closed whatever happened above.
    let _ = env.call_method(&cursor, jni_str!("close"), jni_sig!("()V"), &[]);
    name
}

fn first_display_name(env: &mut Env, cursor: &JObject) -> Jni<Option<String>> {
    if !env.call_method(cursor, jni_str!("moveToFirst"), jni_sig!("()Z"), &[])?.z()? {
        return Ok(None);
    }
    let column = env.new_string("_display_name")?;
    let index = env.call_method(cursor, jni_str!("getColumnIndex"), jni_sig!("(Ljava/lang/String;)I"), &[JValue::Object(&column)])?.i()?;
    if index < 0 {
        return Ok(None);
    }
    let value = env.call_method(cursor, jni_str!("getString"), jni_sig!("(I)Ljava/lang/String;"), &[JValue::Int(index)])?.l()?;
    string_of(env, &value)
}

/// The document's bytes through `ContentResolver.openInputStream`; `None` when it holds more
/// than [`MAX_DOCUMENT_BYTES`].
fn read_uri(env: &mut Env, resolver: &JObject, uri: &JObject) -> Jni<Option<Vec<u8>>> {
    let stream =
        env.call_method(resolver, jni_str!("openInputStream"), jni_sig!("(Landroid/net/Uri;)Ljava/io/InputStream;"), &[JValue::Object(uri)])?.l()?;
    if stream.is_null() {
        return Ok(Some(Vec::new()));
    }
    let buffer = env.new_byte_array(CHUNK)?;
    let mut bytes = Vec::new();
    let read = {
        let mut reader = JavaStream { env: &mut *env, stream: &stream, buffer: &buffer, chunk: vec![0; CHUNK] };
        let limit = u64::try_from(MAX_DOCUMENT_BYTES).unwrap_or(u64::MAX).saturating_add(1);
        (&mut reader).take(limit).read_to_end(&mut bytes)
    };
    let _ = env.call_method(&stream, jni_str!("close"), jni_sig!("()V"), &[]);
    read.map_err(Failure::Io)?;
    Ok((bytes.len() <= MAX_DOCUMENT_BYTES).then_some(bytes))
}

/// A `java.io.InputStream` as a [`Read`].
struct JavaStream<'a, 'env, 'local> {
    env: &'a mut Env<'env>,
    stream: &'a JObject<'local>,
    buffer: &'a JByteArray<'local>,
    chunk: Vec<i8>,
}

impl Read for JavaStream<'_, '_, '_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let want = out.len().min(self.chunk.len());
        let want_j = i32::try_from(want).unwrap_or(i32::MAX);
        let n = self
            .env
            .call_method(self.stream, jni_str!("read"), jni_sig!("([BII)I"), &[JValue::Object(self.buffer), JValue::Int(0), JValue::Int(want_j)])
            .and_then(|v| v.i())
            .map_err(std::io::Error::other)?;
        // -1 is the end of the stream; never more than asked for.
        let Ok(n) = usize::try_from(n) else { return Ok(0) };
        let n = n.min(want);
        let Some(chunk) = self.chunk.get_mut(..n) else { return Ok(0) };
        self.buffer.get_region(self.env, 0, chunk).map_err(std::io::Error::other)?;
        for (o, c) in out.iter_mut().zip(chunk.iter()) {
            *o = u8::from_ne_bytes(c.to_ne_bytes());
        }
        Ok(n)
    }
}

/// The clipboard manager (`Context.getSystemService("clipboard")`).
fn clipboard<'local>(env: &mut Env<'local>, activity: &JObject) -> Jni<JObject<'local>> {
    let name = env.new_string("clipboard")?;
    Ok(env.call_method(activity, jni_str!("getSystemService"), jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"), &[JValue::Object(&name)])?.l()?)
}

/// Put `text` on the system clipboard.
pub fn set_clipboard_text(app: &AndroidApp, text: String) {
    let main = app.clone();
    app.run_on_java_main_thread(Box::new(move || {
        let result = with_activity(&main, |env, activity| {
            let manager = clipboard(env, activity)?;
            if manager.is_null() {
                return Ok(());
            }
            let label = env.new_string("PdfCraft")?;
            let text = env.new_string(&text)?;
            let clip = env
                .call_static_method(
                    jni_str!("android/content/ClipData"),
                    jni_str!("newPlainText"),
                    jni_sig!("(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;"),
                    &[JValue::Object(&label), JValue::Object(&text)],
                )?
                .l()?;
            env.call_method(&manager, jni_str!("setPrimaryClip"), jni_sig!("(Landroid/content/ClipData;)V"), &[JValue::Object(&clip)])?;
            Ok(())
        });
        if let Err(e) = result {
            log::warn!("copying to the clipboard: {e}");
        }
    }));
}

/// The text on the system clipboard, if any.
pub fn clipboard_text(app: &AndroidApp) -> Option<String> {
    let result = with_activity(app, |env, activity| {
        let manager = clipboard(env, activity)?;
        if manager.is_null() {
            return Ok(None);
        }
        let clip = env.call_method(&manager, jni_str!("getPrimaryClip"), jni_sig!("()Landroid/content/ClipData;"), &[])?.l()?;
        if clip.is_null() || env.call_method(&clip, jni_str!("getItemCount"), jni_sig!("()I"), &[])?.i()? < 1 {
            return Ok(None);
        }
        let item = env.call_method(&clip, jni_str!("getItemAt"), jni_sig!("(I)Landroid/content/ClipData$Item;"), &[JValue::Int(0)])?.l()?;
        if item.is_null() {
            return Ok(None);
        }
        let text = env
            .call_method(
                &item,
                jni_str!("coerceToText"),
                jni_sig!("(Landroid/content/Context;)Ljava/lang/CharSequence;"),
                &[JValue::Object(activity)],
            )?
            .l()?;
        string_of(env, &text)
    });
    result.unwrap_or_else(|e| {
        log::warn!("reading the clipboard: {e}");
        None
    })
}

/// The system's text dialog: an `EditText` in an `AlertDialog`, so every keyboard and input
/// method works (accents, other alphabets, predictive text, voice typing). A NativeActivity's own
/// window gets typing only as key events, and characters the keyboard map can't produce are lost.
///
/// It is built from framework classes only, with no Java of PdfCraft's. Which button closed it is
/// learnt without a listener (a listener would be a Java class): each button sends a `Message`
/// whose callback is a `FutureTask` around an idle `Thread`, so pressing it makes that task done,
/// which [`TextDialog::poll`] reads.
pub struct TextDialog {
    state: std::sync::Arc<std::sync::Mutex<DialogState>>,
    /// A check is queued on the Java thread (one at a time).
    checking: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

enum DialogState {
    Opening,
    Open(Box<OpenDialog>),
    Closed(Option<String>),
    Failed,
}

struct OpenDialog {
    dialog: Global<JObject<'static>>,
    edit: Global<JObject<'static>>,
    ok: Global<JObject<'static>>,
    cancel: Global<JObject<'static>>,
}

/// `EditorInfo` input types.
const TYPE_CLASS_TEXT: i32 = 0x1;
const TYPE_TEXT_VARIATION_PASSWORD: i32 = 0x80;
const TYPE_TEXT_FLAG_MULTI_LINE: i32 = 0x2_0000;
/// `DialogInterface.BUTTON_POSITIVE`, `BUTTON_NEGATIVE`.
const BUTTON_POSITIVE: i32 = -1;
const BUTTON_NEGATIVE: i32 = -2;
/// `WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_VISIBLE`.
const SOFT_INPUT_STATE_ALWAYS_VISIBLE: i32 = 5;

impl TextDialog {
    /// Show the dialog (on the Java UI thread; it appears a moment later), with `ok` and
    /// `cancel` as its buttons.
    pub fn open(app: &AndroidApp, request: crate::typing::Request, ok: String, cancel: String) -> TextDialog {
        let state = std::sync::Arc::new(std::sync::Mutex::new(DialogState::Opening));
        let shown = state.clone();
        let main = app.clone();
        app.run_on_java_main_thread(Box::new(move || {
            let built = with_activity(&main, |env, activity| build_dialog(env, activity, &request, [&ok, &cancel]));
            let next = match built {
                Ok(open) => DialogState::Open(Box::new(open)),
                Err(e) => {
                    log::warn!("showing the text dialog: {e}");
                    DialogState::Failed
                }
            };
            *shown.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = next;
        }));
        TextDialog { state, checking: std::sync::Arc::default() }
    }

    /// How the dialog ended, or `None` while it shows. Each call asks the Java UI thread to look
    /// again; its answer shows on a later call.
    pub fn poll(&self, app: &AndroidApp) -> Option<crate::typing::Outcome> {
        let open = {
            let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match &*state {
                DialogState::Opening => return None,
                DialogState::Failed => return Some(crate::typing::Outcome::Failed),
                DialogState::Closed(Some(text)) => return Some(crate::typing::Outcome::Done(text.clone())),
                DialogState::Closed(None) => return Some(crate::typing::Outcome::Cancelled),
                DialogState::Open(_) => true,
            }
        };
        if open && !self.checking.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let state = self.state.clone();
            let checking = self.checking.clone();
            let main = app.clone();
            app.run_on_java_main_thread(Box::new(move || {
                let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let DialogState::Open(dialog) = &*state {
                    let checked = with_activity(&main, |env, _| check_dialog(env, dialog));
                    match checked {
                        Ok(Some(result)) => *state = DialogState::Closed(result),
                        Ok(None) => {}
                        Err(e) => {
                            log::warn!("checking the text dialog: {e}");
                            *state = DialogState::Failed;
                        }
                    }
                }
                checking.store(false, std::sync::atomic::Ordering::SeqCst);
            }));
        }
        None
    }
}

/// A `FutureTask` that does nothing when run, and a `Message` to `handler` that runs it.
fn button_task<'local>(env: &mut Env<'local>, handler: &JObject) -> Jni<(JObject<'local>, JObject<'local>)> {
    let idle = env.new_object(jni_str!("java/lang/Thread"), jni_sig!("()V"), &[])?;
    let null = JObject::null();
    let task = env.new_object(
        jni_str!("java/util/concurrent/FutureTask"),
        jni_sig!("(Ljava/lang/Runnable;Ljava/lang/Object;)V"),
        &[JValue::Object(&idle), JValue::Object(&null)],
    )?;
    let message = env
        .call_static_method(
            jni_str!("android/os/Message"),
            jni_str!("obtain"),
            jni_sig!("(Landroid/os/Handler;Ljava/lang/Runnable;)Landroid/os/Message;"),
            &[JValue::Object(handler), JValue::Object(&task)],
        )?
        .l()?;
    Ok((task, message))
}

fn build_dialog(env: &mut Env, activity: &JObject, request: &crate::typing::Request, [ok_label, cancel_label]: [&String; 2]) -> Jni<OpenDialog> {
    let edit = env.new_object(jni_str!("android/widget/EditText"), jni_sig!("(Landroid/content/Context;)V"), &[JValue::Object(activity)])?;
    let kind = TYPE_CLASS_TEXT
        | if request.password { TYPE_TEXT_VARIATION_PASSWORD } else { 0 }
        | if request.multiline && !request.password { TYPE_TEXT_FLAG_MULTI_LINE } else { 0 };
    env.call_method(&edit, jni_str!("setInputType"), jni_sig!("(I)V"), &[JValue::Int(kind)])?;
    let text = env.new_string(&request.text)?;
    env.call_method(&edit, jni_str!("setText"), jni_sig!("(Ljava/lang/CharSequence;)V"), &[JValue::Object(&text)])?;
    // The caret after the text, where typing usually continues (positions count UTF-16 units).
    let end = i32::try_from(request.text.encode_utf16().count()).unwrap_or(0);
    env.call_method(&edit, jni_str!("setSelection"), jni_sig!("(I)V"), &[JValue::Int(end)])?;

    let looper = env.call_static_method(jni_str!("android/os/Looper"), jni_str!("getMainLooper"), jni_sig!("()Landroid/os/Looper;"), &[])?.l()?;
    let handler = env.new_object(jni_str!("android/os/Handler"), jni_sig!("(Landroid/os/Looper;)V"), &[JValue::Object(&looper)])?;
    let (ok, ok_message) = button_task(env, &handler)?;
    let (cancel, cancel_message) = button_task(env, &handler)?;
    // A message is sent once, so the back key gets its own, running the same task.
    let back_message = env
        .call_static_method(
            jni_str!("android/os/Message"),
            jni_str!("obtain"),
            jni_sig!("(Landroid/os/Message;)Landroid/os/Message;"),
            &[JValue::Object(&cancel_message)],
        )?
        .l()?;

    let builder =
        env.new_object(jni_str!("android/app/AlertDialog$Builder"), jni_sig!("(Landroid/content/Context;)V"), &[JValue::Object(activity)])?;
    let builder = env
        .call_method(&builder, jni_str!("setView"), jni_sig!("(Landroid/view/View;)Landroid/app/AlertDialog$Builder;"), &[JValue::Object(&edit)])?
        .l()?;
    let dialog = env.call_method(&builder, jni_str!("create"), jni_sig!("()Landroid/app/AlertDialog;"), &[])?.l()?;
    for (which, label, message) in [(BUTTON_POSITIVE, ok_label, &ok_message), (BUTTON_NEGATIVE, cancel_label, &cancel_message)] {
        let label = env.new_string(label)?;
        env.call_method(
            &dialog,
            jni_str!("setButton"),
            jni_sig!("(ILjava/lang/CharSequence;Landroid/os/Message;)V"),
            &[JValue::Int(which), JValue::Object(&label), JValue::Object(message)],
        )?;
    }
    env.call_method(&dialog, jni_str!("setCancelMessage"), jni_sig!("(Landroid/os/Message;)V"), &[JValue::Object(&back_message)])?;
    env.call_method(&dialog, jni_str!("setCanceledOnTouchOutside"), jni_sig!("(Z)V"), &[JValue::Bool(false)])?;
    let window = env.call_method(&dialog, jni_str!("getWindow"), jni_sig!("()Landroid/view/Window;"), &[])?.l()?;
    if !window.is_null() {
        env.call_method(&window, jni_str!("setSoftInputMode"), jni_sig!("(I)V"), &[JValue::Int(SOFT_INPUT_STATE_ALWAYS_VISIBLE)])?;
    }
    env.call_method(&dialog, jni_str!("show"), jni_sig!("()V"), &[])?;
    env.call_method(&edit, jni_str!("requestFocus"), jni_sig!("()Z"), &[])?;
    Ok(OpenDialog {
        dialog: env.new_global_ref(&dialog)?,
        edit: env.new_global_ref(&edit)?,
        ok: env.new_global_ref(&ok)?,
        cancel: env.new_global_ref(&cancel)?,
    })
}

/// How the dialog ended: `Some(Some(text))` for OK, `Some(None)` when cancelled or closed, `None`
/// while it shows. OK's task runs before the dialog is dismissed, so a dismissed dialog whose OK
/// task isn't done was cancelled.
fn check_dialog(env: &mut Env, dialog: &OpenDialog) -> Jni<Option<Option<String>>> {
    if env.call_method(&*dialog.ok, jni_str!("isDone"), jni_sig!("()Z"), &[])?.z()? {
        let text = env.call_method(&*dialog.edit, jni_str!("getText"), jni_sig!("()Landroid/text/Editable;"), &[])?.l()?;
        return Ok(Some(Some(string_of(env, &text)?.unwrap_or_default())));
    }
    let cancelled = env.call_method(&*dialog.cancel, jni_str!("isDone"), jni_sig!("()Z"), &[])?.z()?;
    let showing = env.call_method(&*dialog.dialog, jni_str!("isShowing"), jni_sig!("()Z"), &[])?.z()?;
    Ok((cancelled || !showing).then_some(None))
}

/// An `EditText` out of sight in the activity's window that has the keyboard while an egui text
/// field is typed in ([`crate::typing`]): every input method works there, and what it holds is
/// copied into the field as it changes. The window leaves it the keys meanwhile (winit's
/// `set_keys_to_views`). Built and read on the Java UI thread, from framework classes only.
pub struct TextMirror {
    app: AndroidApp,
    shared: std::sync::Arc<std::sync::Mutex<MirrorShared>>,
    /// A read is queued on the Java thread (one at a time).
    checking: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

struct MirrorShared {
    phase: MirrorPhase,
    seen: Option<crate::typing::Seen>,
    /// How many times the keyboard has been asked for (the view takes the focus a moment after
    /// it is added, so the first asks can come too early).
    asks: u32,
}

enum MirrorPhase {
    Opening,
    Open(Global<JObject<'static>>),
    Failed,
    /// Closed, or gone with its window.
    Gone,
}

/// `Context.INPUT_METHOD_SERVICE`.
const INPUT_METHOD_SERVICE: &str = "input_method";
/// How many reads ask for the keyboard again.
const KEYBOARD_ASKS: u32 = 4;
/// `EditorInfo.TYPE_TEXT_FLAG_CAP_SENTENCES`: a capital to start each sentence, as phone apps do.
const TYPE_TEXT_FLAG_CAP_SENTENCES: i32 = 0x4000;

fn lock(shared: &std::sync::Mutex<MirrorShared>) -> std::sync::MutexGuard<'_, MirrorShared> {
    shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl TextMirror {
    /// Add the view with `request`'s text and give it the keyboard (on the Java UI thread; it
    /// shows a moment later).
    pub fn open(app: &AndroidApp, request: crate::typing::Request) -> TextMirror {
        let shared = std::sync::Arc::new(std::sync::Mutex::new(MirrorShared { phase: MirrorPhase::Opening, seen: None, asks: 0 }));
        let for_java = shared.clone();
        let main = app.clone();
        app.run_on_java_main_thread(Box::new(move || {
            let mut state = lock(&for_java);
            // Closed before it could open: nothing to add.
            if !matches!(state.phase, MirrorPhase::Opening) {
                return;
            }
            state.phase = match with_activity(&main, |env, activity| build_mirror(env, activity, &request)) {
                Ok(edit) => {
                    winit::platform::android::set_keys_to_views(true);
                    MirrorPhase::Open(edit)
                }
                Err(e) => {
                    log::warn!("showing the text view: {e}");
                    MirrorPhase::Failed
                }
            };
        }));
        TextMirror { app: app.clone(), shared, checking: std::sync::Arc::default() }
    }

    /// How the view is doing; each call asks the Java UI thread to read it again, and its answer
    /// shows on a later call.
    pub fn poll(&self) -> crate::typing::MirrorPoll {
        use crate::typing::MirrorPoll;
        let seen = {
            let state = lock(&self.shared);
            match &state.phase {
                MirrorPhase::Opening => return MirrorPoll::Opening,
                MirrorPhase::Failed => return MirrorPoll::Failed,
                MirrorPhase::Gone => return MirrorPoll::Gone,
                MirrorPhase::Open(_) => state.seen.clone(),
            }
        };
        if !self.checking.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let shared = self.shared.clone();
            let checking = self.checking.clone();
            let main = self.app.clone();
            self.app.run_on_java_main_thread(Box::new(move || {
                let mut state = lock(&shared);
                let ask = state.asks < KEYBOARD_ASKS;
                let read = match &state.phase {
                    MirrorPhase::Open(edit) => Some(with_activity(&main, |env, activity| read_mirror(env, activity, edit, ask))),
                    _ => None,
                };
                match read {
                    Some(Ok(Some(seen))) => {
                        state.seen = Some(seen);
                        state.asks = state.asks.saturating_add(u32::from(ask));
                    }
                    Some(Ok(None)) => {
                        winit::platform::android::set_keys_to_views(false);
                        state.phase = MirrorPhase::Gone;
                    }
                    Some(Err(e)) => {
                        log::warn!("reading the text view: {e}");
                        winit::platform::android::set_keys_to_views(false);
                        // Out of the window too: left there, it would keep the keyboard from the
                        // window that typing now falls back to.
                        if let MirrorPhase::Open(edit) = std::mem::replace(&mut state.phase, MirrorPhase::Failed) {
                            discard_mirror(&main, &edit);
                        }
                    }
                    None => {}
                }
                checking.store(false, std::sync::atomic::Ordering::SeqCst);
            }));
        }
        MirrorPoll::Open(seen)
    }

    /// Give the view the keyboard again (its field was tapped after the keyboard went, with Back
    /// or the keyboard's own button).
    pub fn show_keyboard(&self) {
        let shared = self.shared.clone();
        let main = self.app.clone();
        self.app.run_on_java_main_thread(Box::new(move || {
            let state = lock(&shared);
            if let MirrorPhase::Open(edit) = &state.phase {
                let shown = with_activity(&main, |env, activity| {
                    env.call_method(edit, jni_str!("requestFocus"), jni_sig!("()Z"), &[])?;
                    show_keyboard(env, activity, edit)
                });
                if let Err(e) = shown {
                    log::warn!("showing the keyboard again: {e}");
                }
            }
        }));
    }

    /// Select `start..end` (UTF-16 units) in the view.
    pub fn select(&self, start: usize, end: usize) {
        let shared = self.shared.clone();
        let main = self.app.clone();
        self.app.run_on_java_main_thread(Box::new(move || {
            let state = lock(&shared);
            if let MirrorPhase::Open(edit) = &state.phase
                && let Err(e) = with_activity(&main, |env, _| select_mirror(env, edit, start, end))
            {
                log::warn!("selecting in the text view: {e}");
            }
        }));
    }
}

impl Drop for TextMirror {
    /// However typing ends, the keyboard goes, the view with it, and the window takes the keys
    /// back at once.
    fn drop(&mut self) {
        let edit = {
            let mut state = lock(&self.shared);
            // Under the lock: a view still being built gives itself the keys when it is done, so
            // they come back after that, not before.
            winit::platform::android::set_keys_to_views(false);
            match std::mem::replace(&mut state.phase, MirrorPhase::Gone) {
                MirrorPhase::Open(edit) => Some(edit),
                _ => None,
            }
        };
        if let Some(edit) = edit {
            let main = self.app.clone();
            self.app.run_on_java_main_thread(Box::new(move || {
                if let Err(e) = with_activity(&main, |env, activity| remove_mirror(env, activity, &edit)) {
                    log::warn!("removing the text view: {e}");
                }
            }));
        }
    }
}

fn build_mirror(env: &mut Env, activity: &JObject, request: &crate::typing::Request) -> Jni<Global<JObject<'static>>> {
    let edit = env.new_object(jni_str!("android/widget/EditText"), jni_sig!("(Landroid/content/Context;)V"), &[JValue::Object(activity)])?;
    // Multi-line for every field: Enter arrives as a line break, which a one-line field takes as
    // its Done (`typing`).
    let kind = TYPE_CLASS_TEXT | TYPE_TEXT_FLAG_MULTI_LINE | TYPE_TEXT_FLAG_CAP_SENTENCES;
    env.call_method(&edit, jni_str!("setInputType"), jni_sig!("(I)V"), &[JValue::Int(kind)])?;
    let text = env.new_string(&request.text)?;
    env.call_method(&edit, jni_str!("setText"), jni_sig!("(Ljava/lang/CharSequence;)V"), &[JValue::Object(&text)])?;
    let end = i32::try_from(request.text.encode_utf16().count()).unwrap_or(0);
    env.call_method(&edit, jni_str!("setSelection"), jni_sig!("(I)V"), &[JValue::Int(end)])?;
    // Out of sight: a pixel, transparent, in the window's corner (the field shows the text).
    env.call_method(&edit, jni_str!("setAlpha"), jni_sig!("(F)V"), &[JValue::Float(0.0)])?;
    let params = env.new_object(jni_str!("android/view/ViewGroup$LayoutParams"), jni_sig!("(II)V"), &[JValue::Int(1), JValue::Int(1)])?;
    env.call_method(
        activity,
        jni_str!("addContentView"),
        jni_sig!("(Landroid/view/View;Landroid/view/ViewGroup$LayoutParams;)V"),
        &[JValue::Object(&edit), JValue::Object(&params)],
    )?;
    let focused = (|| -> Jni<()> {
        env.call_method(&edit, jni_str!("setFocusable"), jni_sig!("(Z)V"), &[JValue::Bool(true)])?;
        env.call_method(&edit, jni_str!("setFocusableInTouchMode"), jni_sig!("(Z)V"), &[JValue::Bool(true)])?;
        env.call_method(&edit, jni_str!("requestFocus"), jni_sig!("()Z"), &[])?;
        show_keyboard(env, activity, &edit)
    })();
    if let Err(e) = focused {
        // Added but not working: out of the window again, so it doesn't keep the keyboard.
        env.exception_clear();
        let _ = remove_mirror(env, activity, &edit);
        return Err(e);
    }
    Ok(env.new_global_ref(&edit)?)
}

/// Take `edit` out of its window after a failure, as well as it goes (a Java exception may be
/// pending from the call that failed).
fn discard_mirror(app: &AndroidApp, edit: &JObject) {
    let removed = with_activity(app, |env, activity| {
        env.exception_clear();
        remove_mirror(env, activity, edit)
    });
    if let Err(e) = removed {
        log::warn!("removing the text view after a failure: {e}");
    }
}

/// The input method manager.
fn input_methods<'local>(env: &mut Env<'local>, activity: &JObject) -> Jni<JObject<'local>> {
    let name = env.new_string(INPUT_METHOD_SERVICE)?;
    Ok(env.call_method(activity, jni_str!("getSystemService"), jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"), &[JValue::Object(&name)])?.l()?)
}

/// Ask for the keyboard for `edit`.
fn show_keyboard(env: &mut Env, activity: &JObject, edit: &JObject) -> Jni<()> {
    let imm = input_methods(env, activity)?;
    if !imm.is_null() {
        env.call_method(&imm, jni_str!("showSoftInput"), jni_sig!("(Landroid/view/View;I)Z"), &[JValue::Object(edit), JValue::Int(0)])?;
    }
    Ok(())
}

/// What the view holds, or `None` once it is gone from its window. `ask` asks for the keyboard
/// again (the first reads).
fn read_mirror(env: &mut Env, activity: &JObject, edit: &JObject, ask: bool) -> Jni<Option<crate::typing::Seen>> {
    if !env.call_method(edit, jni_str!("isAttachedToWindow"), jni_sig!("()Z"), &[])?.z()? {
        return Ok(None);
    }
    let focused = env.call_method(edit, jni_str!("hasFocus"), jni_sig!("()Z"), &[])?.z()?;
    if ask && focused {
        show_keyboard(env, activity, edit)?;
    }
    let text = env.call_method(edit, jni_str!("getText"), jni_sig!("()Landroid/text/Editable;"), &[])?.l()?;
    let text = string_of(env, &text)?.unwrap_or_default();
    let start = env.call_method(edit, jni_str!("getSelectionStart"), jni_sig!("()I"), &[])?.i()?;
    let end = env.call_method(edit, jni_str!("getSelectionEnd"), jni_sig!("()I"), &[])?.i()?;
    // -1 (no selection) reads as past the end: the end of the text.
    let units = |i: i32| usize::try_from(i).unwrap_or(usize::MAX);
    let (start, end) = (units(start.min(end)), units(start.max(end)));
    Ok(Some(crate::typing::Seen { text, start, end, focused }))
}

fn select_mirror(env: &mut Env, edit: &JObject, start: usize, end: usize) -> Jni<()> {
    let length = env.call_method(edit, jni_str!("length"), jni_sig!("()I"), &[])?.i()?;
    let at = |i: usize| i32::try_from(i).unwrap_or(i32::MAX).clamp(0, length.max(0));
    env.call_method(edit, jni_str!("setSelection"), jni_sig!("(II)V"), &[JValue::Int(at(start)), JValue::Int(at(end))])?;
    Ok(())
}

/// Put the keyboard away and take the view out of its window.
fn remove_mirror(env: &mut Env, activity: &JObject, edit: &JObject) -> Jni<()> {
    let imm = input_methods(env, activity)?;
    let token = env.call_method(edit, jni_str!("getWindowToken"), jni_sig!("()Landroid/os/IBinder;"), &[])?.l()?;
    if !imm.is_null() && !token.is_null() {
        env.call_method(&imm, jni_str!("hideSoftInputFromWindow"), jni_sig!("(Landroid/os/IBinder;I)Z"), &[JValue::Object(&token), JValue::Int(0)])?;
    }
    let parent = env.call_method(edit, jni_str!("getParent"), jni_sig!("()Landroid/view/ViewParent;"), &[])?.l()?;
    if !parent.is_null() {
        env.call_method(&parent, jni_str!("removeView"), jni_sig!("(Landroid/view/View;)V"), &[JValue::Object(edit)])?;
    }
    Ok(())
}

/// `WindowManager.LayoutParams` flags: the window paints the system bars' backgrounds (in the
/// colours it sets), and they aren't translucent.
const FLAG_DRAWS_SYSTEM_BAR_BACKGROUNDS: i32 = i32::MIN; // 0x8000_0000
const FLAG_TRANSLUCENT_STATUS: i32 = 0x0400_0000;
const FLAG_TRANSLUCENT_NAVIGATION: i32 = 0x0800_0000;
/// `View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR`, `SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR`: dark icons, for
/// a light bar.
const SYSTEM_UI_FLAG_LIGHT_STATUS_BAR: i32 = 0x2000;
const SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR: i32 = 0x10;

/// Paint the status bar `top` and the navigation bar `bottom` (the colours of the screen's top and
/// bottom), each with icons that read on it. Android draws the bars; the window says in which
/// colours.
pub fn set_system_bars(app: &AndroidApp, top: [u8; 3], bottom: [u8; 3], failed: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    use crate::screen::is_light;
    let main = app.clone();
    app.run_on_java_main_thread(Box::new(move || {
        let painted = with_activity(&main, |env, activity| {
            let window = env.call_method(activity, jni_str!("getWindow"), jni_sig!("()Landroid/view/Window;"), &[])?.l()?;
            if window.is_null() {
                return Ok(());
            }
            let argb = |[r, g, b]: [u8; 3]| i32::from_be_bytes([0xff, r, g, b]);
            env.call_method(
                &window,
                jni_str!("clearFlags"),
                jni_sig!("(I)V"),
                &[JValue::Int(FLAG_TRANSLUCENT_STATUS | FLAG_TRANSLUCENT_NAVIGATION)],
            )?;
            env.call_method(&window, jni_str!("addFlags"), jni_sig!("(I)V"), &[JValue::Int(FLAG_DRAWS_SYSTEM_BAR_BACKGROUNDS)])?;
            env.call_method(&window, jni_str!("setStatusBarColor"), jni_sig!("(I)V"), &[JValue::Int(argb(top))])?;
            env.call_method(&window, jni_str!("setNavigationBarColor"), jni_sig!("(I)V"), &[JValue::Int(argb(bottom))])?;
            let decor = env.call_method(&window, jni_str!("getDecorView"), jni_sig!("()Landroid/view/View;"), &[])?.l()?;
            if !decor.is_null() {
                let mut flags = env.call_method(&decor, jni_str!("getSystemUiVisibility"), jni_sig!("()I"), &[])?.i()?;
                for (light, flag) in [(is_light(top), SYSTEM_UI_FLAG_LIGHT_STATUS_BAR), (is_light(bottom), SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR)] {
                    flags = if light { flags | flag } else { flags & !flag };
                }
                env.call_method(&decor, jni_str!("setSystemUiVisibility"), jni_sig!("(I)V"), &[JValue::Int(flags)])?;
            }
            Ok(())
        });
        if let Err(e) = painted {
            log::warn!("painting the system bars: {e}");
            failed.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }));
}

/// Android 10 (API 29): `MediaStore.Downloads`, where an app adds files without a permission.
const API_Q: i32 = 29;
/// `Intent.FLAG_GRANT_READ_URI_PERMISSION`, `Intent.FLAG_ACTIVITY_NEW_TASK`.
const FLAG_GRANT_READ_URI_PERMISSION: i32 = 0x1;
const FLAG_ACTIVITY_NEW_TASK: i32 = 0x1000_0000;
/// Where printed PDFs go, under shared storage.
const PRINT_FOLDER: &str = "Download/PdfCraft";

/// Print through another app: put `bytes` in Download/PdfCraft as `name` and open it in the app
/// the user picks (a PDF viewer prints it). Android has no print spooler a native app can reach
/// without Java (a `PrintDocumentAdapter` is a Java class), and a file can only be handed to
/// another app through a content provider: from Android 10 the system's own (`MediaStore`), before
/// that a plain file in Download (the storage permission covers it there).
pub fn print_with_app(app: &AndroidApp, name: &str, bytes: &[u8], title: &str) -> Result<(), String> {
    // A name from the document: one file name, never a path.
    let name = crate::storage::display_name(Some(name), "");
    let name = name.as_str();
    with_activity(app, |env, activity| {
        let uri = if AndroidApp::sdk_version() >= API_Q { media_store_file(env, activity, name, bytes)? } else { download_file(env, name, bytes)? };
        let action = env.new_string("android.intent.action.VIEW")?;
        let view = env.new_object(jni_str!("android/content/Intent"), jni_sig!("(Ljava/lang/String;)V"), &[JValue::Object(&action)])?;
        let mime = env.new_string("application/pdf")?;
        env.call_method(
            &view,
            jni_str!("setDataAndType"),
            jni_sig!("(Landroid/net/Uri;Ljava/lang/String;)Landroid/content/Intent;"),
            &[JValue::Object(&uri), JValue::Object(&mime)],
        )?;
        env.call_method(
            &view,
            jni_str!("addFlags"),
            jni_sig!("(I)Landroid/content/Intent;"),
            &[JValue::Int(FLAG_GRANT_READ_URI_PERMISSION | FLAG_ACTIVITY_NEW_TASK)],
        )?;
        let title = env.new_string(title)?;
        let chooser = env
            .call_static_method(
                jni_str!("android/content/Intent"),
                jni_str!("createChooser"),
                jni_sig!("(Landroid/content/Intent;Ljava/lang/CharSequence;)Landroid/content/Intent;"),
                &[JValue::Object(&view), JValue::Object(&title)],
            )?
            .l()?;
        env.call_method(activity, jni_str!("startActivity"), jni_sig!("(Landroid/content/Intent;)V"), &[JValue::Object(&chooser)])?;
        Ok(())
    })
}

/// A new file `name` in Download/PdfCraft holding `bytes`, through `MediaStore.Downloads`; its URI.
fn media_store_file<'local>(env: &mut Env<'local>, activity: &JObject, name: &str, bytes: &[u8]) -> Jni<JObject<'local>> {
    let values = env.new_object(jni_str!("android/content/ContentValues"), jni_sig!("()V"), &[])?;
    for (key, value) in [("_display_name", name), ("mime_type", "application/pdf"), ("relative_path", PRINT_FOLDER)] {
        let key = env.new_string(key)?;
        let value = env.new_string(value)?;
        env.call_method(
            &values,
            jni_str!("put"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;)V"),
            &[JValue::Object(&key), JValue::Object(&value)],
        )?;
    }
    let collection = env
        .get_static_field(jni_str!("android/provider/MediaStore$Downloads"), jni_str!("EXTERNAL_CONTENT_URI"), jni_sig!("Landroid/net/Uri;"))?
        .l()?;
    let resolver = env.call_method(activity, jni_str!("getContentResolver"), jni_sig!("()Landroid/content/ContentResolver;"), &[])?.l()?;
    let uri = env
        .call_method(
            &resolver,
            jni_str!("insert"),
            jni_sig!("(Landroid/net/Uri;Landroid/content/ContentValues;)Landroid/net/Uri;"),
            &[JValue::Object(&collection), JValue::Object(&values)],
        )?
        .l()?;
    if uri.is_null() {
        return Err(Failure::Io(std::io::Error::other("Download refused a new file")));
    }
    let stream = env
        .call_method(&resolver, jni_str!("openOutputStream"), jni_sig!("(Landroid/net/Uri;)Ljava/io/OutputStream;"), &[JValue::Object(&uri)])?
        .l()?;
    if stream.is_null() {
        return Err(Failure::Io(std::io::Error::other("the new file in Download can't be written")));
    }
    let written = write_stream(env, &stream, bytes);
    let _ = env.call_method(&stream, jni_str!("close"), jni_sig!("()V"), &[]);
    written?;
    Ok(uri)
}

/// `bytes` to a `java.io.OutputStream`, a chunk at a time through one Java array.
fn write_stream(env: &mut Env, stream: &JObject, bytes: &[u8]) -> Jni<()> {
    let buffer = env.new_byte_array(CHUNK)?;
    let mut chunk = vec![0i8; CHUNK];
    for part in bytes.chunks(CHUNK) {
        let staged = chunk.get_mut(..part.len()).ok_or_else(|| Failure::Io(std::io::Error::other("chunk")))?;
        for (s, b) in staged.iter_mut().zip(part) {
            *s = i8::from_ne_bytes([*b]);
        }
        buffer.set_region(env, 0, staged)?;
        let n = i32::try_from(part.len()).unwrap_or(0);
        env.call_method(stream, jni_str!("write"), jni_sig!("([BII)V"), &[JValue::Object(&buffer), JValue::Int(0), JValue::Int(n)])?;
    }
    Ok(())
}

/// Before Android 10: `bytes` as a file in Download/PdfCraft (the storage permission lets PdfCraft
/// write there, and other apps read it), handed over as a `file://` address. StrictMode refuses to
/// let such an address leave the app, so its VM policy is relaxed first.
fn download_file<'local>(env: &mut Env<'local>, name: &str, bytes: &[u8]) -> Jni<JObject<'local>> {
    let folder = std::path::Path::new(crate::storage::SHARED).join(PRINT_FOLDER);
    let path = folder.join(name);
    std::fs::create_dir_all(&folder).and_then(|()| std::fs::write(&path, bytes)).map_err(Failure::Io)?;
    let builder = env.new_object(jni_str!("android/os/StrictMode$VmPolicy$Builder"), jni_sig!("()V"), &[])?;
    let policy = env.call_method(&builder, jni_str!("build"), jni_sig!("()Landroid/os/StrictMode$VmPolicy;"), &[])?.l()?;
    env.call_static_method(
        jni_str!("android/os/StrictMode"),
        jni_str!("setVmPolicy"),
        jni_sig!("(Landroid/os/StrictMode$VmPolicy;)V"),
        &[JValue::Object(&policy)],
    )?;
    let path = env.new_string(path.to_string_lossy())?;
    let file = env.new_object(jni_str!("java/io/File"), jni_sig!("(Ljava/lang/String;)V"), &[JValue::Object(&path)])?;
    Ok(env
        .call_static_method(
            jni_str!("android/net/Uri"),
            jni_str!("fromFile"),
            jni_sig!("(Ljava/io/File;)Landroid/net/Uri;"),
            &[JValue::Object(&file)],
        )?
        .l()?)
}
