//! The file dialog every picker is described with.
//!
//! On the desktop and the web this is rfd's `AsyncFileDialog`, shown by the system. Android has
//! no picker rfd can show, so there the same builder records what was asked for and the in-app
//! browser ([`crate::file_browser`]) shows it. Callers build either one with the same calls
//! (`new`, `add_filter`, `set_title`, `set_file_name`, `set_directory`).

#[cfg(not(target_os = "android"))]
pub(crate) use rfd::AsyncFileDialog;

#[cfg(target_os = "android")]
pub(crate) use crate::file_browser::Dialog as AsyncFileDialog;
