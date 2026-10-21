//! Installed fonts: a background thread indexes the platform's font folders once at start-up
//! (`mailcraft_fonts::system::scan`) and installs the index process-wide, so message bodies, the compose
//! editor and print jobs draw text in the installed font it names (`FontDb::with_installed`), and the compose
//! Font list offers every installed family (`mailcraft_ui_egui::compose::set_installed_fonts_reader`). Until
//! the scan finishes the list holds only the fonts MailCraft knows and text uses the bundled stand-ins. Every
//! failure (unreadable folder or file, not a font) just leaves that entry out; the walk is bounded (folder
//! depth, number of files, file size).

fn read() -> Vec<String> {
    mailcraft_fonts::system::installed()
        .map(|sys| sys.families().iter().map(|f| (*f).to_owned()).collect())
        .unwrap_or_default()
}

/// Starts the scan and installs the reader (call once at start-up).
pub fn install() {
    mailcraft_ui_egui::compose::set_installed_fonts_reader(Some(read));
    let spawned = std::thread::Builder::new().name("mailcraft-font-scan".into()).spawn(|| {
        let dirs = mailcraft_fonts::system::font_dirs();
        mailcraft_fonts::system::install(mailcraft_fonts::system::scan(&dirs));
    });
    if let Err(e) = spawned {
        log::debug!("font scan not started: {e}");
    }
}
