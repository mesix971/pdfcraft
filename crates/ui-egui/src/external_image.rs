//! Edit text & images ▸ Edit Image Using: the picture goes to the system's own image editor as a
//! file of its own, and each time it is saved there it replaces the picture in the PDF (one undo
//! step each time).

use std::path::PathBuf;
use std::time::SystemTime;

use pdfcraft_engine::{DocId, Edit, ImageEdit};

/// The largest picture read back from the editor.
const MAX_BYTES: u64 = 256 * 1024 * 1024;

/// A picture out in the system's editor.
pub(crate) struct ExternalImage {
    doc: DocId,
    page: usize,
    index: usize,
    path: PathBuf,
    /// When the file was last written (by us, then by the editor).
    stamp: Option<SystemTime>,
    /// When the file was last looked at (seconds of egui time).
    checked: f64,
}

impl Drop for ExternalImage {
    fn drop(&mut self) {
        // The copy is the user's picture: it does not outlive the watch.
        let _ = std::fs::remove_file(&self.path);
    }
}

impl crate::PdfCraftApp {
    /// Open picture `index` of `page` in the system's default editor for its kind of file.
    pub(crate) fn edit_image_externally(&mut self, page: usize, index: usize) {
        let Some((_, id)) = self.active_ids() else { return };
        if !self.may_copy(id) {
            return;
        }
        let (ext, bytes) = match self.session.get(id).map(|d| d.page_image_file(page, index)) {
            Some(Ok(file)) => file,
            Some(Err(e)) => {
                self.notify_fmt("Couldn't save the image: {e}", &[("e", &e.to_string())]);
                return;
            }
            None => return,
        };
        // A new watch ends the earlier one (and removes its copy).
        self.external_image = None;
        let dir = std::env::temp_dir().join("pdfcraft-images");
        let nanos = SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let path = dir.join(format!("pdfcraft-page{}-image{}-{nanos}.{ext}", page + 1, index + 1));
        let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, &bytes));
        if let Err(e) = written {
            self.notify_fmt("Couldn't save the image: {e}", &[("e", &e.to_string())]);
            return;
        }
        let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if let Err(e) = launch(&path) {
            let _ = std::fs::remove_file(&path);
            self.notify_fmt("Couldn't open the image editor: {e}", &[("e", &e)]);
            return;
        }
        self.external_image = Some(ExternalImage { doc: id, page, index, path, stamp, checked: 0.0 });
        self.notify_tr("The picture is open in your image editor: save it there to update the PDF.");
    }

    /// Once a second: when the editor has saved the picture, put it in the PDF. Waits while
    /// another document is active, and stops when the document closes.
    pub(crate) fn poll_external_image(&mut self, now: f64) {
        let Some(x) = self.external_image.as_mut() else { return };
        if now - x.checked < 1.0 {
            return;
        }
        x.checked = now;
        let (doc, page, index) = (x.doc, x.page, x.index);
        if self.session.get(doc).is_none() {
            self.external_image = None;
            return;
        }
        if self.active_ids().map(|(_, id)| id) != Some(doc) {
            return;
        }
        let Some(x) = self.external_image.as_mut() else { return };
        let Ok(meta) = std::fs::metadata(&x.path) else { return };
        let modified = meta.modified().ok();
        // An editor may write the file in steps: an empty file is not a picture yet.
        if modified == x.stamp || meta.len() == 0 {
            return;
        }
        x.stamp = modified;
        if meta.len() > MAX_BYTES {
            self.notify_tr("The edited picture is too large to put back (at most 256 MB).");
            return;
        }
        let Ok(bytes) = std::fs::read(&x.path) else { return };
        let name = x.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let change = ImageEdit::Replace { name, bytes: std::sync::Arc::new(bytes) };
        if self.apply_edit(Edit::EditPageImage { page, index, change }) {
            self.notify_tr("The picture was updated from your image editor.");
        }
    }
}

/// Open `path` with the system's default program for its kind of file.
fn launch(path: &std::path::Path) -> Result<(), String> {
    // Absolute, so a name starting with '-' can't read as an option.
    let path = std::path::absolute(path).map_err(|e| e.to_string())?;
    let program = match std::env::consts::OS {
        // Explorer opens a file with the program associated with its extension.
        "windows" => "explorer",
        "macos" => "open",
        _ => "xdg-open",
    };
    // Explorer's exit code says nothing about success: only starting it counts.
    std::process::Command::new(program).arg(&path).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbImage::from_pixel(w, h, image::Rgb([20, 120, 200])).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    /// A save in the editor puts the picture back in the PDF (one undo step); the copy goes when
    /// the watch ends.
    #[test]
    fn a_save_in_the_editor_updates_the_picture() {
        let mut app = crate::PdfCraftApp::new();
        app.open_bytes("picture.png", None, png(80, 40)).unwrap();
        let id = app.views[0].id;
        let size = |app: &crate::PdfCraftApp| app.session.get(id).unwrap().page_images(0).first().map(|i| (i.width, i.height));
        assert_eq!(size(&app), Some((80, 40)));
        let dir = std::env::temp_dir().join(format!("pdfcraft-external-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("picture.png");
        std::fs::write(&path, png(80, 40)).unwrap();
        let stamp = std::fs::metadata(&path).unwrap().modified().ok();
        app.external_image = Some(ExternalImage { doc: id, page: 0, index: 0, path: path.clone(), stamp, checked: 0.0 });
        app.poll_external_image(5.0);
        assert_eq!(size(&app), Some((80, 40)), "nothing saved yet");
        // The editor saves a bigger picture (a different time stamp).
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, png(40, 20)).unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(2);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
        app.poll_external_image(5.5);
        assert_eq!(size(&app), Some((80, 40)), "at most once a second");
        app.poll_external_image(7.0);
        assert_eq!(size(&app), Some((40, 20)), "replaced from the editor's file");
        assert!(app.session.get(id).unwrap().can_undo().is_some());
        app.external_image = None;
        assert!(!path.exists(), "the copy is removed when the watch ends");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
