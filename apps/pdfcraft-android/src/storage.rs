//! Android storage, as paths: where shared storage and removable cards are mounted, and which
//! file a `content://` or `file://` address handed to the app names.
//!
//! Plain functions over strings and directories, so the host runs their tests.

use std::path::{Component, Path, PathBuf};

/// Where Android mounts the device's shared storage for the current user.
pub const SHARED: &str = "/storage/emulated/0";
/// Where Android mounts shared storage and removable cards.
pub const MOUNTS: &str = "/storage";

/// The removable cards mounted in `mounts` (`/storage`): every folder except the device's own
/// storage (`emulated`) and the current user's alias of it (`self`), by name. Sorted by name.
pub fn removable(mounts: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(mounts) else { return Vec::new() };
    let mut out: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(name, _)| !matches!(name.as_str(), "emulated" | "self") && !name.starts_with('.'))
        .take(16)
        .collect();
    out.sort();
    out
}

/// The file a `file://` or `content://` address names on shared storage, when it can be known
/// from the address alone: a `file://` path, a document of the system's storage provider
/// (`com.android.externalstorage.documents`, document id `primary:Download/a.pdf` or
/// `1234-ABCD:a.pdf`), or a Downloads document with a `raw:` path.
///
/// Opening the path instead of reading the address keeps the file's place, so Save writes it
/// back. Only paths under `/storage` (or `/sdcard`) are accepted, and none with `..`: another app
/// can hand PdfCraft any address, and one must never make it open its own private files.
/// Anything else returns `None`, and the caller reads the address through the system.
pub fn path_for_uri(uri: &str) -> Option<PathBuf> {
    let (scheme, rest) = uri.split_once("://")?;
    let path = if scheme.eq_ignore_ascii_case("file") {
        // `file:///storage/...`: an empty authority; `file://localhost/...` names this device too.
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        PathBuf::from(percent_decode(rest.split(['?', '#']).next()?)?)
    } else if scheme.eq_ignore_ascii_case("content") {
        let (authority, rest) = rest.split_once('/')?;
        // A tree address names a folder and then the document inside it: the document id is last.
        let id = percent_decode(rest.rsplit_once("document/")?.1.split(['?', '#', '/']).next()?)?;
        match authority {
            "com.android.externalstorage.documents" => {
                let (volume, relative) = id.split_once(':')?;
                let base = if volume == "primary" { PathBuf::from(SHARED) } else { Path::new(MOUNTS).join(volume) };
                if volume.is_empty() || volume.contains(['/', '\\']) || volume == "." || volume == ".." {
                    return None;
                }
                base.join(relative)
            }
            "com.android.providers.downloads.documents" => PathBuf::from(id.strip_prefix("raw:")?),
            _ => return None,
        }
    } else {
        return None;
    };
    shared(&path).then_some(path)
}

/// Whether `path` is an absolute path on shared storage or a removable card, without `..`.
fn shared(path: &Path) -> bool {
    let mut parts = path.components();
    if parts.next() != Some(Component::RootDir) {
        return false;
    }
    let top = parts.next();
    let top_ok = matches!(top, Some(Component::Normal(t)) if t == "storage" || t == "sdcard");
    top_ok && path.components().all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

/// `%XX` escapes decoded as UTF-8; `None` when an escape is malformed or the result isn't UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A file name for a document read from an address: the name the system gave (`display`), else
/// the address's last part, without folders, and `document.pdf` when neither gives one.
pub fn display_name(display: Option<&str>, uri: &str) -> String {
    let from_uri = || {
        let last = uri.split(['?', '#']).next().unwrap_or_default().rsplit('/').next().unwrap_or_default();
        percent_decode(last).map(|s| s.rsplit([':', '/']).next().unwrap_or_default().to_string())
    };
    let name = display.map(str::to_string).or_else(from_uri).unwrap_or_default();
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default().trim().chars().filter(|c| !c.is_control()).take(200).collect::<String>();
    if name.is_empty() || name == "." || name == ".." { "document.pdf".to_string() } else { name }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_provider_documents_are_their_shared_storage_paths() {
        let cases = [
            ("content://com.android.externalstorage.documents/document/primary%3ADownload%2Freport.pdf", "/storage/emulated/0/Download/report.pdf"),
            ("content://com.android.externalstorage.documents/document/1234-ABCD%3ADocs%2Fa%20b.pdf", "/storage/1234-ABCD/Docs/a b.pdf"),
            (
                "content://com.android.externalstorage.documents/tree/primary%3ADownload/document/primary%3ADownload%2Fx.pdf",
                "/storage/emulated/0/Download/x.pdf",
            ),
            (
                "content://com.android.providers.downloads.documents/document/raw%3A%2Fstorage%2Femulated%2F0%2FDownload%2Fy.pdf",
                "/storage/emulated/0/Download/y.pdf",
            ),
            ("file:///storage/emulated/0/Documents/%C3%A9t%C3%A9.pdf", "/storage/emulated/0/Documents/été.pdf"),
            ("file:///sdcard/a.pdf", "/sdcard/a.pdf"),
        ];
        for (uri, path) in cases {
            assert_eq!(path_for_uri(uri), Some(PathBuf::from(path)), "{uri}");
        }
    }

    #[test]
    fn addresses_that_could_reach_private_files_are_read_through_the_system_instead() {
        for uri in [
            "content://com.android.externalstorage.documents/document/primary%3A..%2F..%2F..%2Fdata%2Fdata%2Fai.storyteller.pdfcraft%2Ffiles%2Fx",
            "content://com.android.externalstorage.documents/document/..%3Ax.pdf",
            "content://com.android.externalstorage.documents/document/%3Ax.pdf",
            "content://com.android.providers.downloads.documents/document/raw%3A%2Fdata%2Fdata%2Fai.storyteller.pdfcraft%2Ffiles%2Fa.pdf",
            "content://com.android.providers.downloads.documents/document/raw%3Astorage%2Fa.pdf",
            "content://com.android.providers.downloads.documents/document/msf%3A123",
            "content://media/external/file/42",
            "content://com.example.files/root/storage/emulated/0/a.pdf",
            "file:///data/data/ai.storyteller.pdfcraft/files/a.pdf",
            "file:///storage/emulated/0/../../data/a.pdf",
            "file://otherhost/storage/a.pdf",
            "http://example.com/storage/a.pdf",
            "content://com.android.externalstorage.documents/document/primary%3Abad%ZZ",
            "content://com.android.externalstorage.documents/document/primary%3A%FF.pdf",
            "",
            "storage/emulated/0/a.pdf",
        ] {
            assert_eq!(path_for_uri(uri), None, "{uri}");
        }
    }

    #[test]
    fn display_names_never_carry_folders_or_control_characters() {
        assert_eq!(display_name(Some("Facture 2026.pdf"), "content://x/1"), "Facture 2026.pdf");
        assert_eq!(display_name(Some("../../evil.pdf"), "content://x/1"), "evil.pdf");
        assert_eq!(display_name(Some("a\u{0}b\n.pdf"), "content://x/1"), "ab.pdf");
        assert_eq!(
            display_name(None, "content://com.android.externalstorage.documents/document/primary%3ADownload%2Fr%C3%A9sum%C3%A9.pdf"),
            "résumé.pdf"
        );
        assert_eq!(display_name(None, "content://media/external/file/42"), "42");
        assert_eq!(display_name(Some(""), "content://x/"), "document.pdf");
        assert_eq!(display_name(None, "content://x/%ZZ"), "document.pdf");
        assert_eq!(display_name(Some(".."), "content://x/1"), "document.pdf");
    }

    #[test]
    fn removable_cards_are_the_mounts_other_than_the_devices_own_storage() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-android-mounts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["emulated", "self", "1234-ABCD", "0000-1111", ".hidden"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(dir.join("file"), b"").unwrap();
        let names: Vec<String> = removable(&dir).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["0000-1111", "1234-ABCD"]);
        assert!(removable(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
