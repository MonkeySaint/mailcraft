//! Native file dialogs for the desktop app (rfd: the XDG desktop portal on Linux and the BSDs, the system
//! dialogs on Windows and macOS), installed into the UI as [`mailcraft_ui_egui::FileDialogs`] (Browse...,
//! Save As..., Import/Export file pickers) and as the compose window's file hooks (Browse This PC…, Save
//! Attachment As…).
//!
//! A dialog that cannot be shown (no portal, no display) behaves like Cancel; the UI then keeps its typed
//! path. A panic inside the dialog library is caught and also treated as Cancel.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mailcraft_ui_egui::{App, FileDialogs, FileFilter};

/// The platform's file dialogs.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeFileDialogs;

/// Runs `f`, turning a panic into `None` (never crash over a file dialog).
fn guarded<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            log::error!("the file dialog failed");
            None
        }
    }
}

fn dialog(title: &str, filters: &[FileFilter], start: Option<&Path>) -> rfd::FileDialog {
    let mut d = rfd::FileDialog::new();
    if !title.is_empty() {
        d = d.set_title(title);
    }
    for f in filters {
        let exts: Vec<&str> = f.extensions.iter().map(String::as_str).filter(|e| !e.is_empty()).collect();
        if !exts.is_empty() {
            d = d.add_filter(&f.name, &exts);
        }
    }
    if let Some(dir) = start.filter(|d| d.is_dir()) {
        d = d.set_directory(dir);
    }
    d
}

impl FileDialogs for NativeFileDialogs {
    fn open_files(
        &self,
        title: &str,
        filters: &[FileFilter],
        multiple: bool,
        start: Option<&Path>,
    ) -> Option<Vec<PathBuf>> {
        guarded(|| {
            let d = dialog(title, filters, start);
            if multiple { d.pick_files() } else { d.pick_file().map(|p| vec![p]) }
        })
    }

    fn save_file(
        &self,
        title: &str,
        suggested: &str,
        filters: &[FileFilter],
        start: Option<&Path>,
    ) -> Option<PathBuf> {
        guarded(|| {
            let mut d = dialog(title, filters, start);
            if !suggested.is_empty() {
                d = d.set_file_name(suggested);
            }
            d.save_file()
        })
    }

    fn pick_folder(&self, title: &str, start: Option<&Path>) -> Option<PathBuf> {
        guarded(|| dialog(title, &[], start).pick_folder())
    }
}

fn compose_pick_files() -> Option<Vec<PathBuf>> {
    NativeFileDialogs.open_files("Insert File", &[], true, None)
}

fn compose_pick_save(suggested: &str) -> Option<PathBuf> {
    NativeFileDialogs.save_file(mailcraft_ui_egui::compose::SAVE_TITLE, suggested, &[], None)
}

/// Installs the native dialogs into `app` and the compose window's file hooks.
pub fn install(app: &mut App) {
    app.set_file_dialogs(Arc::new(NativeFileDialogs));
    mailcraft_ui_egui::compose::set_file_hooks(mailcraft_ui_egui::compose::FileHooks {
        pick_files: Some(compose_pick_files),
        pick_save: Some(compose_pick_save),
        open_path: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_turns_panics_into_cancel() {
        let r: Option<u8> = guarded(|| panic!("boom"));
        assert_eq!(r, None);
        assert_eq!(guarded(|| Some(3)), Some(3));
    }

    #[test]
    fn install_sets_the_app_dialogs() {
        let mut app =
            App::new(mailcraft_engine::Session::new(mailcraft_engine::model::Mailbox::new_local("Test")));
        assert!(app.files.is_none());
        install(&mut app);
        assert!(app.files.is_some());
        assert!(mailcraft_ui_egui::compose::file_hooks().pick_files.is_some());
    }
}
