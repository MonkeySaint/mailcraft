//! Font names for the compose Font list (ribbon Font box, Font dialog): the theme's heading and body fonts,
//! then every font MailCraft knows a stand-in for ([`mailcraft_fonts::MENU_FAMILIES`]) together with the
//! fonts installed on this computer. Installed fonts come from the platform: the desktop app installs a
//! reader with [`set_installed_fonts_reader`] (it scans the system font folders in the background); without
//! one (web, tests) only the known names are listed. Text in any font is drawn with the closest bundled
//! face; recipients see the named font where they have it.

use std::sync::RwLock;

/// Lists the installed font families (may return an empty list while still scanning).
pub type InstalledFonts = fn() -> Vec<String>;

static READER: RwLock<Option<InstalledFonts>> = RwLock::new(None);

/// Most names in the list.
pub const MAX_FONT_NAMES: usize = 2_000;

/// Installs (or removes) the platform's installed-fonts reader.
pub fn set_installed_fonts_reader(reader: Option<InstalledFonts>) {
    if let Ok(mut g) = READER.write() {
        *g = reader;
    }
}

/// The installed font families the reader reports (none without a reader).
pub fn installed_fonts() -> Vec<String> {
    READER.read().ok().and_then(|g| *g).map(|r| r()).unwrap_or_default()
}

/// The known names plus `installed`, sorted without regard to case, each once, at most [`MAX_FONT_NAMES`].
pub fn merge_font_names(installed: &[String]) -> Vec<String> {
    let mut all: Vec<String> = mailcraft_fonts::MENU_FAMILIES.iter().map(|s| (*s).to_owned()).collect();
    for name in installed {
        let n = name.trim();
        // Hidden system faces start with a dot; very long names are not font names.
        if n.is_empty() || n.starts_with('.') || n.chars().count() > 128 || n.chars().any(char::is_control) {
            continue;
        }
        all.push(n.to_owned());
    }
    all.sort_by_key(|n| n.to_lowercase());
    all.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    all.truncate(MAX_FONT_NAMES);
    all
}

/// Every font the Font list offers (known stand-ins and installed fonts).
pub fn font_names() -> Vec<String> {
    merge_font_names(&installed_fonts())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_names_come_first_and_installed_fonts_join_them() {
        let base = merge_font_names(&[]);
        assert!(base.iter().any(|n| n == "Calibri") && base.iter().any(|n| n == "Times New Roman"));
        let more = merge_font_names(&[
            "Zed Sans".into(),
            "calibri".into(),
            ".Hidden UI".into(),
            String::new(),
            "x".repeat(300),
        ]);
        assert_eq!(more.len(), base.len() + 1, "{more:?}");
        assert!(more.iter().any(|n| n == "Zed Sans"));
        let lower: Vec<String> = more.iter().map(|n| n.to_lowercase()).collect();
        let mut sorted = lower.clone();
        sorted.sort();
        assert_eq!(lower, sorted);
    }

    #[test]
    fn every_known_name_has_a_bundled_stand_in() {
        for n in mailcraft_fonts::MENU_FAMILIES {
            assert!(mailcraft_fonts::bundled_family_for(n).is_some(), "{n}");
        }
    }
}
