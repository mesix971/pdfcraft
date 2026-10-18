//! Autosave and crash recovery.
//!
//! While documents have unsaved changes, their working file is written every
//! `AUTOSAVE_SECS` to a recovery folder in the user's data directory. Each entry is
//! `<key>.pdf` plus `<key>.json` (name, original path, time). Saving, discarding or closing a
//! document removes its entry. At startup the app offers snapshots even when their metadata is
//! missing or damaged; those recover without an original path, so saving asks for a destination.
//!
//! Encrypted documents are stored encrypted (the working file is), so recovery never writes
//! plaintext of a protected document to disk. The web build has no recovery store yet.
//!
//! A running instance holds an exclusively locked `instance-<pid>-<n>.lock` in the folder and
//! names it in the snapshots it writes, so a second instance can tell live autosaves from a
//! crash's leftovers and neither offers nor discards them (#2166). The OS releases the lock
//! when the process dies.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::PdfCraftApp;

/// How often unsaved changes are written to the recovery folder.
pub const AUTOSAVE_SECS: f64 = 60.0;

/// What a recovery entry records next to the PDF bytes.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecoveryMeta {
    pub key: String,
    pub name: String,
    /// Where the document was saved before (Save writes back there after recovery).
    pub path: Option<String>,
    /// Seconds since the Unix epoch when the snapshot was taken.
    pub saved_at: u64,
    /// The bytes are encrypted; recovering asks for the password.
    pub encrypted: bool,
    /// The lock file of the instance that wrote the snapshot (see the module doc); missing in
    /// snapshots from before it.
    #[serde(default)]
    pub instance: Option<String>,
}

/// The recovery folder.
#[derive(Clone, Debug)]
pub struct RecoveryStore {
    dir: PathBuf,
    /// This instance's lock, held while it runs and shared by the store's clones (one store,
    /// one instance). Unset until [`claim`](Self::claim); `Some(None)` when claiming failed
    /// (the folder can't be written, or the platform has no file locks).
    lock: Arc<OnceLock<Option<InstanceLock>>>,
}

/// An open, exclusively locked `instance-<pid>-<n>.lock` in the folder, released and removed
/// with the last clone of its store.
#[derive(Debug)]
struct InstanceLock {
    name: String,
    path: PathBuf,
    file: File,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Tells this process's stores apart: tests make several, and a dropped one's lock is free.
static CLAIMS: AtomicU64 = AtomicU64::new(0);

impl RecoveryStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), lock: Arc::default() }
    }

    /// Mark the folder as in use by a running instance (see the module doc). Without a lock
    /// (the folder can't be written, or the platform has no file locks) snapshots are written
    /// and offered as before.
    pub fn claim(&self) {
        self.lock.get_or_init(|| {
            if let Err(e) = std::fs::create_dir_all(&self.dir) {
                log::warn!("recovery folder {} can't be made: {e}", self.dir.display());
                return None;
            }
            let name = format!("instance-{}-{}.lock", std::process::id(), CLAIMS.fetch_add(1, Ordering::Relaxed));
            let path = self.dir.join(&name);
            let locked = File::options().read(true).write(true).create(true).truncate(false).open(&path).and_then(|file| match file.try_lock() {
                Ok(()) => Ok(file),
                Err(TryLockError::WouldBlock) => Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "held by another instance")),
                Err(TryLockError::Error(e)) => Err(e),
            });
            match locked {
                Ok(file) => Some(InstanceLock { name, path, file }),
                Err(e) => {
                    let _ = std::fs::remove_file(&path);
                    log::warn!("recovery folder {} can't be claimed: {e}", self.dir.display());
                    None
                }
            }
        });
    }

    fn own_lock(&self) -> Option<&InstanceLock> {
        self.lock.get().and_then(Option::as_ref)
    }

    /// The name of this instance's lock file, for the snapshots it writes.
    pub fn instance(&self) -> Option<String> {
        self.own_lock().map(|l| l.name.clone())
    }

    /// Whether `meta` was written by another instance that is still running. A lock file that
    /// is no longer held is a crash's leftover and is removed. The name comes from the sidecar,
    /// which is untrusted, so only a plain `instance-….lock` name in the folder is looked at.
    fn written_by_a_live_instance(&self, meta: &RecoveryMeta) -> bool {
        let Some(name) = meta.instance.as_deref() else { return false };
        if self.own_lock().is_some_and(|l| l.name == name) {
            return false;
        }
        let plain = name.starts_with("instance-")
            && name.ends_with(".lock")
            && !name.contains("..")
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
        if !plain {
            return false;
        }
        let path = self.dir.join(name);
        let Ok(file) = File::options().read(true).write(true).open(&path) else { return false };
        match file.try_lock() {
            Err(TryLockError::WouldBlock) => true,
            Ok(()) => {
                let _ = file.unlock();
                drop(file);
                let _ = std::fs::remove_file(&path);
                false
            }
            // Can't tell (no file locks here): offered, as before.
            Err(TryLockError::Error(_)) => false,
        }
    }

    /// `Recovery` in the portable data folder in portable mode ([`crate::portable`]), otherwise
    /// the platform's per-user data folder: `~/Library/Application Support/PdfCraft/Recovery`
    /// (macOS), `%LOCALAPPDATA%\PdfCraft\Recovery` (Windows), or
    /// `$XDG_DATA_HOME/pdfcraft/recovery` / `~/.local/share/pdfcraft/recovery` (others).
    pub fn default_dir() -> Option<PathBuf> {
        // Lowercase beside the lowercase data folder of the platforms that name it so.
        let lower = crate::portable::data_dir().is_none() && !cfg!(any(target_os = "macos", windows));
        crate::portable::app_data_dir().map(|d| d.join(if lower { "recovery" } else { "Recovery" }))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Write (or replace) an entry. The PDF is written before its metadata, both atomically, so
    /// a listed entry always has complete bytes.
    pub fn write(&self, meta: &RecoveryMeta, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        crate::editing::write_atomically(&self.dir.join(format!("{}.pdf", meta.key)).to_string_lossy(), bytes)?;
        let json = serde_json::to_vec_pretty(meta).map_err(std::io::Error::other)?;
        crate::editing::write_atomically(&self.dir.join(format!("{}.json", meta.key)).to_string_lossy(), &json)
    }

    pub fn remove(&self, key: &str) {
        let _ = std::fs::remove_file(self.dir.join(format!("{key}.json")));
        let _ = std::fs::remove_file(self.dir.join(format!("{key}.pdf")));
    }

    /// Snapshots, newest first, leaving out what another running instance is autosaving.
    /// Listing never removes a snapshot; missing metadata uses the file name.
    pub fn list(&self) -> Vec<RecoveryMeta> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut out = Vec::new();
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("pdf") || !entry.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let Some(key) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            let meta = std::fs::read(path.with_extension("json"))
                .and_then(|b| serde_json::from_slice::<RecoveryMeta>(&b).map_err(std::io::Error::other))
                .and_then(|m| if m.key == key { Ok(m) } else { Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "snapshot key mismatch")) });
            out.push(meta.unwrap_or_else(|e| {
                log::warn!("recovering snapshot without metadata: {}: {e}", path.display());
                let saved_at = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                RecoveryMeta {
                    key: key.to_string(),
                    name: format!("{key}.pdf"),
                    path: None,
                    saved_at,
                    // This is only a display hint; opening the PDF still detects encryption.
                    encrypted: false,
                    instance: None,
                }
            }));
        }
        out.retain(|m| !self.written_by_a_live_instance(m));
        out.sort_by_key(|m| std::cmp::Reverse(m.saved_at));
        out
    }

    /// Rewrite `meta`'s sidecar when it is missing, unreadable or names another key (a snapshot
    /// recovered without metadata), so damaged metadata doesn't stay behind. The PDF is untouched.
    pub fn repair_meta(&self, meta: &RecoveryMeta) {
        let path = self.dir.join(format!("{}.json", meta.key));
        let readable = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<RecoveryMeta>(&b).ok()).is_some_and(|m| m.key == meta.key);
        if readable {
            return;
        }
        let written = serde_json::to_vec_pretty(meta)
            .map_err(std::io::Error::other)
            .and_then(|json| crate::editing::write_atomically(&path.to_string_lossy(), &json));
        if let Err(e) = written {
            log::warn!("couldn't replace recovery metadata {}: {e}", path.display());
        }
    }

    pub fn read(&self, key: &str) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.dir.join(format!("{key}.pdf")))
    }

    /// Remove every entry (clean quit, or "Discard all"), and metadata left without a snapshot;
    /// not another running PdfCraft's.
    pub fn clear(&self) {
        for m in self.list() {
            self.remove(&m.key);
        }
        let Ok(dir) = std::fs::read_dir(&self.dir) else { return };
        for entry in dir.flatten() {
            let path = entry.path();
            // Only sidecars whose PDF is gone: they hold no document content.
            if path.extension().and_then(|e| e.to_str()) == Some("json")
                && entry.file_type().is_ok_and(|t| t.is_file())
                && !path.with_extension("pdf").exists()
            {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// Autosaves written off the UI thread: a large document takes a while to write and sync, and
/// the periodic autosave would freeze the window meanwhile. One writer, so writes land in order;
/// an entry forgotten meanwhile (its document saved, discarded or closed) is never left behind: the
/// writer checks the forgotten keys before it writes, and again after, removing what was forgotten
/// during the write. It doesn't hold their lock while it writes: saving or closing a document takes
/// it, and would wait for a large write.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Autosaver {
    jobs: std::sync::mpsc::Sender<Job>,
    forgotten: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

#[cfg(not(target_arch = "wasm32"))]
enum Job {
    Write(RecoveryStore, RecoveryMeta, std::sync::Arc<Vec<u8>>),
    /// Answered once every job sent before it is done.
    Flush(std::sync::mpsc::Sender<()>),
    /// Tests: the writer waits here until told to go on, so what comes meanwhile is decided first.
    #[cfg(test)]
    Hold(std::sync::mpsc::Receiver<()>),
}

#[cfg(not(target_arch = "wasm32"))]
fn forgotten_keys(m: &std::sync::Mutex<std::collections::HashSet<String>>) -> std::sync::MutexGuard<'_, std::collections::HashSet<String>> {
    // A writer that panicked left the set as it was: still the right one.
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(not(target_arch = "wasm32"))]
impl Autosaver {
    fn start() -> Option<Self> {
        let (jobs, rx) = std::sync::mpsc::channel::<Job>();
        let forgotten = std::sync::Arc::<std::sync::Mutex<std::collections::HashSet<String>>>::default();
        let gone = forgotten.clone();
        let spawned = std::thread::Builder::new().name("pdfcraft-autosave".into()).spawn(move || {
            for job in rx {
                match job {
                    Job::Write(store, meta, bytes) => {
                        if forgotten_keys(&gone).contains(&meta.key) {
                            continue;
                        }
                        #[cfg(test)]
                        tests::before_write(&store);
                        if let Err(e) = store.write(&meta, &bytes) {
                            log::warn!("autosave failed: {e}");
                        }
                        // Forgotten while it was written: its removal may have come first.
                        if forgotten_keys(&gone).contains(&meta.key) {
                            store.remove(&meta.key);
                        }
                    }
                    Job::Flush(done) => {
                        let _ = done.send(());
                    }
                    #[cfg(test)]
                    Job::Hold(go_on) => {
                        let _ = go_on.recv();
                    }
                }
            }
        });
        spawned.map_err(|e| log::warn!("autosave writer: {e}")).ok()?;
        Some(Self { jobs, forgotten })
    }
}

/// Tells apart the entries this process makes ("<time>-<pid>-<document>-<serial>").
static AUTOSAVE_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl PdfCraftApp {
    /// Turn on autosave into `store`, and look for documents a previous session left behind.
    pub fn enable_recovery(&mut self, store: RecoveryStore) {
        store.claim();
        self.recoverable = store.list();
        if !self.recoverable.is_empty() {
            self.dialog = Some(crate::Dialog::Recovery);
        }
        self.recovery = Some(store);
    }

    /// The snapshots due: documents changed since the last autosave, each with its entry.
    fn autosave_jobs(&mut self) -> Vec<(RecoveryMeta, std::sync::Arc<Vec<u8>>)> {
        if self.recovery.is_none() {
            return Vec::new();
        }
        let mut jobs = Vec::new();
        // The instance that writes them, so another running instance leaves them alone.
        let instance = self.recovery.as_ref().and_then(RecoveryStore::instance);
        for snap in self.session.autosave_snapshots() {
            // Never a key used before, even within the same second for the same document: an
            // entry forgotten is never written again.
            let serial = AUTOSAVE_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let key = self
                .recovery_keys
                .entry(snap.doc)
                .or_insert_with(|| format!("{}-{}-{}-{serial}", now_secs(), std::process::id(), snap.doc.0))
                .clone();
            jobs.push((
                RecoveryMeta { key, name: snap.name, path: snap.path, saved_at: now_secs(), encrypted: snap.encrypted, instance: instance.clone() },
                snap.bytes,
            ));
        }
        jobs
    }

    /// Hand `jobs` to the writer (started the first time); written here where it can't run.
    fn autosave_in_background(&mut self, jobs: Vec<(RecoveryMeta, std::sync::Arc<Vec<u8>>)>) {
        let Some(store) = self.recovery.clone() else { return };
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.autosaver.is_none() {
                self.autosaver = Autosaver::start();
            }
            if let Some(writer) = &self.autosaver {
                for (meta, bytes) in jobs {
                    if let Err(std::sync::mpsc::SendError(Job::Write(store, meta, bytes))) = writer.jobs.send(Job::Write(store.clone(), meta, bytes))
                        && let Err(e) = store.write(&meta, &bytes)
                    {
                        log::warn!("autosave failed: {e}");
                    }
                }
                return;
            }
        }
        for (meta, bytes) in jobs {
            if let Err(e) = store.write(&meta, &bytes) {
                log::warn!("autosave failed: {e}");
            }
        }
    }

    /// Write snapshots of documents changed since the last autosave, and wait until they (and
    /// every autosave before them) are on disk: the app going to the background, tests.
    pub fn autosave_now(&mut self) {
        let jobs = self.autosave_jobs();
        self.autosave_in_background(jobs);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(writer) = &self.autosaver {
            let (done, wait) = std::sync::mpsc::channel();
            if writer.jobs.send(Job::Flush(done)).is_ok() {
                let _ = wait.recv();
            }
        }
    }

    /// Remove entry `key` for good: one queued or being written for it never lands after.
    fn drop_recovery_entry(&self, store: &RecoveryStore, key: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        let _writer = self.autosaver.as_ref().map(|w| {
            let mut gone = forgotten_keys(&w.forgotten);
            gone.insert(key.to_owned());
            gone
        });
        store.remove(key);
    }

    /// Drop a document's recovery entry (it was saved, discarded or closed).
    pub(crate) fn forget_recovery(&mut self, doc: pdfcraft_engine::DocId) {
        if let (Some(store), Some(key)) = (self.recovery.clone(), self.recovery_keys.remove(&doc)) {
            self.drop_recovery_entry(&store, &key);
        }
    }

    /// Autosave when the interval has passed (called every frame), off the UI thread.
    pub(crate) fn autosave_tick(&mut self, now: f64) {
        if self.recovery.is_none() {
            return;
        }
        if now - self.last_autosave >= AUTOSAVE_SECS {
            self.last_autosave = now;
            let jobs = self.autosave_jobs();
            self.autosave_in_background(jobs);
        }
    }

    /// Reopen recovered documents (`keys`), as unsaved changes at their original paths.
    pub fn recover(&mut self, keys: &[String]) {
        let Some(store) = self.recovery.clone() else { return };
        for key in keys {
            let Some(meta) = self.recoverable.iter().find(|m| &m.key == key).cloned() else { continue };
            #[cfg(not(target_arch = "wasm32"))]
            if self.ctx.is_some() && !self.run_inline {
                self.startup_superseded = true;
                let result = self.schedule_open(crate::OpenRequest {
                    name: meta.name.clone(),
                    path: None,
                    stamp: None,
                    input: crate::open_worker::Input::Recovery(store.clone(), key.clone()),
                    password: None,
                    activate: true,
                    after_open: None,
                    recovered: Some(meta.clone()),
                    then: None,
                });
                if let Err(error) = result {
                    self.notify_fmt("Couldn't recover {name}: {e}", &[("name", &meta.name), ("e", &error)]);
                }
                continue;
            }
            let bytes = match store.read(key) {
                Ok(b) => b,
                Err(e) => {
                    self.notify_fmt("Couldn't recover {name}: {e}", &[("name", &meta.name), ("e", &e.to_string())]);
                    continue;
                }
            };
            match self.open_bytes(&meta.name, None, bytes) {
                Ok(()) if self.password_prompt.is_none() => self.finish_recovery(&meta),
                // Encrypted: the password prompt opens it, and finishes its recovery once it
                // does. The association travels with the prompt, so cancelling it cannot hand
                // this snapshot's path to the next document that is unlocked (#813).
                Ok(()) => {
                    if let Some(prompt) = self.password_prompt.as_mut() {
                        prompt.recovered = Some(meta.clone());
                    }
                }
                Err(e) => self.notify_fmt("Couldn't recover {name}: {e}", &[("name", &meta.name), ("e", &e.to_string())]),
            }
        }
        self.recoverable.retain(|m| !keys.contains(&m.key));
    }

    /// The most recently opened tab came from `meta`: keep its recovery entry and path.
    pub(crate) fn finish_recovery(&mut self, meta: &RecoveryMeta) {
        let Some(id) = self.views.last().map(|v| v.id) else { return };
        self.session.mark_recovered(id, meta.path.clone());
        self.recovery_keys.insert(id, meta.key.clone());
        // A snapshot recovered without readable metadata gets a fresh sidecar now, so the
        // damaged one doesn't linger until the next autosave (or forever, if none comes).
        if let Some(store) = &self.recovery {
            let mut meta = meta.clone();
            if let Some(doc) = self.session.get(id) {
                meta.encrypted = doc.info.encrypted;
            }
            store.repair_meta(&meta);
        }
    }

    /// Delete recovery entries the user chose not to recover.
    pub fn discard_recovered(&mut self, keys: &[String]) {
        if let Some(store) = &self.recovery {
            for k in keys {
                store.remove(k);
            }
        }
        self.recoverable.retain(|m| !keys.contains(&m.key));
    }

    /// Clean shutdown: documents were saved or discarded, so nothing needs recovering.
    pub fn shutdown_recovery(&mut self) {
        if self.first_dirty().is_none()
            && let Some(store) = self.recovery.clone()
        {
            for key in self.recovery_keys.values() {
                self.drop_recovery_entry(&store, key);
            }
            self.recovery_keys.clear();
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// A writer about to write into this folder says so, and waits to be told to go on.
    type Pause = (std::path::PathBuf, std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);
    static BEFORE_WRITE: std::sync::Mutex<Option<Pause>> = std::sync::Mutex::new(None);

    /// The writer's pause point between its first check and the write (only for the folder a test
    /// asked it for: other tests' writers run alongside).
    pub(super) fn before_write(store: &RecoveryStore) {
        let pause = {
            let mut held = BEFORE_WRITE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if held.as_ref().is_some_and(|(dir, ..)| dir == store.dir()) { held.take() } else { None }
        };
        if let Some((_, reached, go_on)) = pause {
            let _ = reached.send(());
            let _ = go_on.recv();
        }
    }

    /// A document forgotten while its autosave is being written leaves no entry: the write lands
    /// after the removal, and the writer takes it back out.
    #[test]
    fn an_autosave_forgotten_while_written_is_taken_back() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-autosave-during-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = RecoveryStore::new(&dir);
        let mut app = PdfCraftApp::new();
        app.enable_recovery(store.clone());
        let pdf = b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >> endobj\n4 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF";
        app.open_bytes("a.pdf", None, pdf.to_vec()).unwrap();
        assert!(app.apply_edit(pdfcraft_engine::Edit::DeletePages { pages: vec![0] }));
        let (reached_tx, reached) = std::sync::mpsc::channel();
        let (go_on, go_on_rx) = std::sync::mpsc::channel();
        *BEFORE_WRITE.lock().unwrap() = Some((store.dir().to_path_buf(), reached_tx, go_on_rx));
        app.autosave_tick(AUTOSAVE_SECS + 1.0);
        // The writer has checked the entry and is about to write it: now it is forgotten.
        reached.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
        let Some((_, id)) = app.active_ids() else { panic!("a document") };
        app.forget_recovery(id);
        go_on.send(()).unwrap();
        app.autosave_now();
        assert!(store.list().is_empty(), "{:?}", store.list());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The periodic autosave writes off the UI thread, and a document saved or closed while its
    /// snapshot waits to be written leaves no entry behind.
    #[test]
    fn a_background_autosave_never_outlives_its_document() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-autosave-bg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = RecoveryStore::new(&dir);
        let mut app = PdfCraftApp::new();
        app.enable_recovery(store.clone());
        let pdf = b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >> endobj\n4 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF";
        app.open_bytes("a.pdf", None, pdf.to_vec()).unwrap();
        assert!(app.apply_edit(pdfcraft_engine::Edit::DeletePages { pages: vec![0] }));
        // The writer is held while the snapshot waits behind it and the document is forgotten.
        app.autosaver = Autosaver::start();
        let (go_on, held) = std::sync::mpsc::channel();
        let Some(writer) = &app.autosaver else { panic!("a writer") };
        writer.jobs.send(Job::Hold(held)).unwrap();
        app.autosave_tick(AUTOSAVE_SECS + 1.0);
        let Some((_, id)) = app.active_ids() else { panic!("a document") };
        app.forget_recovery(id);
        go_on.send(()).unwrap();
        app.autosave_now();
        assert!(store.list().is_empty(), "{:?}", store.list());
        // And one still open is there once written.
        assert!(app.apply_edit(pdfcraft_engine::Edit::RotatePages { pages: vec![0], degrees: 90 }));
        app.autosave_tick(2.0 * AUTOSAVE_SECS + 2.0);
        app.autosave_now();
        assert_eq!(store.list().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
