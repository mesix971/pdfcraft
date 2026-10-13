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

/// The document the app was started with, if any: the address of a VIEW or EDIT intent, or the
/// stream of a SEND. Reads it whole, so call it off the UI thread.
pub fn launch_document(app: &AndroidApp) -> Result<Option<Incoming>, String> {
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
            return Ok(Some(Incoming::Path(path.to_string_lossy().into_owned())));
        }
        let resolver = env.call_method(activity, jni_str!("getContentResolver"), jni_sig!("()Landroid/content/ContentResolver;"), &[])?.l()?;
        let shown = display_name(env, &resolver, &uri).unwrap_or_else(|e| {
            log::warn!("the opened document's name: {e}");
            None
        });
        let name = crate::storage::display_name(shown.as_deref(), &address);
        Ok(Some(match read_uri(env, &resolver, &uri)? {
            Some(bytes) => Incoming::Bytes(name, bytes),
            None => Incoming::TooLarge(name),
        }))
    })
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
