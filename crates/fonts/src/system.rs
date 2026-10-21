//! Fonts installed on this computer.
//!
//! [`scan`] walks the platform's font folders ([`font_dirs`]) once and indexes every face it can read: family
//! name, weight, italic, file and face index within a collection. Nothing is kept in memory but the index. A
//! [`FontDb`](crate::FontDb) that has the index ([`FontDb::with_system`](crate::FontDb::with_system), or
//! [`FontDb::with_installed`](crate::FontDb::with_installed) for the process-wide one set with [`install`])
//! loads a family's files the first time a document asks for it, so text in an installed font is measured
//! and drawn in that font; names that are not installed keep their bundled stand-ins.
//!
//! Loaded font data stays in memory for the rest of the process (fonts are shared by every editor, reading
//! pane and print job) and is bounded, since the names come from mail anyone can send: at most
//! [`MAX_FACES_PER_FAMILY`] faces per family, [`MAX_LOADED_FAMILIES`] whole families, [`MAX_PREVIEW_FACES`]
//! single faces for the Font list ([`SystemFonts::load_regular`]) and [`MAX_LOADED_BYTES`] in all. A family
//! past a limit is drawn with its bundled stand-in. Unreadable folders and files and data that is not a font
//! are left out; nothing here panics.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use skrifa::MetadataProvider;
use skrifa::attribute::{Stretch, Style};
use skrifa::string::StringId;

use crate::face::FaceInfo;
use crate::{Face, MAX_FONT_FILE, resolve};

/// Deepest folder level walked below a font folder.
pub const MAX_DEPTH: usize = 6;
/// Most font files read by one scan.
pub const MAX_FILES: usize = 4_000;
/// Most faces read from one collection file.
pub const MAX_COLLECTION_FACES: u32 = 64;
/// Most faces loaded for one family (regular, bold, italic, bold italic and the weights nearest them).
pub const MAX_FACES_PER_FAMILY: usize = 8;
/// Most installed families loaded whole by the process (a message naming more gets stand-ins for the rest).
pub const MAX_LOADED_FAMILIES: usize = 48;
/// Most single regular faces loaded for Font list previews.
pub const MAX_PREVIEW_FACES: usize = 512;
/// Most font data loaded by the process in all.
pub const MAX_LOADED_BYTES: usize = 256 * 1024 * 1024;

/// One installed face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemFace {
    pub family: String,
    /// CSS weight (100–1000).
    pub weight: u16,
    pub italic: bool,
    pub path: PathBuf,
    /// Face index within a collection file (0 for single fonts).
    pub index: u32,
}

/// A face loaded from disk, with its parsed data (shared by every [`FontDb`](crate::FontDb) that uses it).
#[derive(Debug)]
pub(crate) struct LoadedFace {
    pub face: Face,
    pub info: OnceLock<FaceInfo>,
}

#[derive(Debug, Default)]
struct LoadState {
    /// Normalised family → its loaded faces.
    families: HashMap<String, Vec<&'static LoadedFace>>,
    /// Normalised family → its regular face alone (Font list previews), or `None` when it has none usable.
    regular: HashMap<String, Option<&'static LoadedFace>>,
    /// File path → its data (collections are read once for all their faces).
    files: HashMap<PathBuf, &'static [u8]>,
    bytes: usize,
}

/// The index of installed fonts (see the module docs).
#[derive(Debug, Default)]
pub struct SystemFonts {
    faces: Vec<SystemFace>,
    /// Distinct family names, sorted without regard to case.
    names: Vec<&'static str>,
    /// Normalised family name → display name.
    by_key: HashMap<String, &'static str>,
    loaded: Mutex<LoadState>,
}

impl SystemFonts {
    /// An index over `faces` (families that only differ in case are merged; hidden names starting with a dot
    /// and empty names are dropped).
    pub fn from_faces(faces: Vec<SystemFace>) -> SystemFonts {
        let mut by_key: HashMap<String, &'static str> = HashMap::new();
        let mut kept = Vec::with_capacity(faces.len());
        for mut f in faces {
            let name: String = f.family.chars().filter(|c| !c.is_control()).take(128).collect();
            let name = name.trim();
            if name.is_empty() || name.starts_with('.') {
                continue;
            }
            let key = resolve::collapse(name, false);
            if key.is_empty() {
                continue;
            }
            let display = match by_key.get(&key) {
                Some(d) => *d,
                None => {
                    // Family names are few (hundreds) and live as long as the index; leaking them lets faces
                    // carry `&'static str` names like the bundled ones.
                    let d: &'static str = Box::leak(name.to_owned().into_boxed_str());
                    by_key.insert(key, d);
                    d
                }
            };
            f.family = display.to_owned();
            kept.push(f);
        }
        let mut names: Vec<&'static str> = by_key.values().copied().collect();
        names.sort_by_key(|n| n.to_lowercase());
        SystemFonts { faces: kept, names, by_key, loaded: Mutex::new(LoadState::default()) }
    }

    /// Installed family names, sorted without regard to case.
    pub fn families(&self) -> &[&'static str] {
        &self.names
    }

    /// Every indexed face.
    pub fn faces(&self) -> &[SystemFace] {
        &self.faces
    }

    /// Whether `family` is installed (any case and spacing).
    pub fn has_family(&self, family: &str) -> bool {
        self.family_by_key(&resolve::collapse(family, true)).is_some()
    }

    /// The installed family for a normalised (lower-case, whitespace-collapsed) name.
    pub(crate) fn family_by_key(&self, key: &str) -> Option<&'static str> {
        self.by_key.get(key).copied()
    }

    /// The installed faces of `display`, regular upright faces first, then by distance from regular.
    fn entries(&self, display: &str) -> Vec<&SystemFace> {
        let mut entries: Vec<&SystemFace> = self.faces.iter().filter(|f| f.family == display).collect();
        entries.sort_by_key(|f| (f.italic, f.weight.abs_diff(400), f.path.clone(), f.index));
        entries
    }

    /// Whether `family` has been loaded whole (not just its regular face for a preview).
    pub fn is_loaded(&self, family: &str) -> bool {
        let key = resolve::collapse(family, true);
        self.loaded.lock().is_ok_and(|st| st.families.contains_key(&key))
    }

    /// The faces of `family`, loading their files on first use. Empty when the family is not installed, its
    /// files cannot be read, or the process has loaded [`MAX_LOADED_FAMILIES`] families or
    /// [`MAX_LOADED_BYTES`] already.
    pub(crate) fn load_family(&self, family: &str) -> Vec<&'static LoadedFace> {
        let key = resolve::collapse(family, true);
        let Some(display) = self.family_by_key(&key) else { return Vec::new() };
        let Ok(mut st) = self.loaded.lock() else { return Vec::new() };
        if let Some(v) = st.families.get(&key) {
            return v.clone();
        }
        if st.families.len() >= MAX_LOADED_FAMILIES {
            return Vec::new();
        }
        // The cap keeps the useful faces (see `entries`).
        let preview = st.regular.get(&key).copied().flatten();
        let mut out: Vec<&'static LoadedFace> = Vec::new();
        for e in self.entries(display) {
            if out.len() >= MAX_FACES_PER_FAMILY {
                break;
            }
            if out.iter().any(|l| l.face.weight == e.weight && l.face.italic == e.italic) {
                continue;
            }
            // The face a preview loaded already is the same one (same ids in every db).
            if let Some(p) = preview.filter(|p| same_source(&st, p, e)) {
                out.push(p);
                continue;
            }
            if let Some(lf) = load_face(&mut st, display, e) {
                out.push(lf);
            }
        }
        st.families.insert(key, out.clone());
        out
    }

    /// The regular face of `family` alone (what a Font list preview draws), loading only its file. `None`
    /// when the family is not installed, has no readable face or a limit was reached.
    pub(crate) fn load_regular(&self, family: &str) -> Option<&'static LoadedFace> {
        let key = resolve::collapse(family, true);
        let display = self.family_by_key(&key)?;
        let mut st = self.loaded.lock().ok()?;
        if let Some(v) = st.families.get(&key) {
            return v.first().copied();
        }
        if let Some(r) = st.regular.get(&key) {
            return *r;
        }
        if st.regular.len() >= MAX_PREVIEW_FACES {
            return None;
        }
        let mut found = None;
        for e in self.entries(display) {
            if let Some(lf) = load_face(&mut st, display, e) {
                found = Some(lf);
                break;
            }
        }
        st.regular.insert(key, found);
        found
    }
}

/// Whether loaded face `l` came from installed face `e` (same file data, same face index).
fn same_source(st: &LoadState, l: &LoadedFace, e: &SystemFace) -> bool {
    l.face.index == e.index
        && st.files.get(&e.path).is_some_and(|d| std::ptr::eq(d.as_ptr(), l.face.data.as_ptr()))
}

/// Loads installed face `e` of family `display` (its file read once per process).
fn load_face(st: &mut LoadState, display: &'static str, e: &SystemFace) -> Option<&'static LoadedFace> {
    let data = file_data(st, &e.path)?;
    FaceInfo::parse(data, e.index).font.as_ref()?;
    let face = Face { family: display, weight: e.weight, italic: e.italic, data, index: e.index };
    // Faces live as long as the font data they point into (see `file_data`); both are bounded.
    Some(Box::leak(Box::new(LoadedFace { face, info: OnceLock::new() })))
}

/// The bytes of a font file, read once per process (`None` when unreadable, too big or over the budget).
fn file_data(st: &mut LoadState, path: &Path) -> Option<&'static [u8]> {
    if let Some(d) = st.files.get(path) {
        return Some(d);
    }
    let len = usize::try_from(std::fs::metadata(path).ok()?.len()).ok()?;
    if len > MAX_FONT_FILE || st.bytes.saturating_add(len) > MAX_LOADED_BYTES {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    if data.len() > MAX_FONT_FILE || st.bytes.saturating_add(data.len()) > MAX_LOADED_BYTES {
        return None;
    }
    st.bytes = st.bytes.saturating_add(data.len());
    // Font data must outlive every layout and painter that refers to it; files are read at most once.
    let data: &'static [u8] = Box::leak(data.into_boxed_slice());
    st.files.insert(path.to_owned(), data);
    Some(data)
}

static INSTALLED: OnceLock<Arc<SystemFonts>> = OnceLock::new();

/// Sets the process-wide index used by [`FontDb::with_installed`](crate::FontDb::with_installed) (the first
/// call wins; later ones are ignored).
pub fn install(fonts: SystemFonts) {
    let _ = INSTALLED.set(Arc::new(fonts));
}

/// The process-wide index, once [`install`] ran.
pub fn installed() -> Option<Arc<SystemFonts>> {
    INSTALLED.get().cloned()
}

/// The platform's font folders.
pub fn font_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "windows") {
        if let Some(w) = std::env::var_os("WINDIR") {
            dirs.push(PathBuf::from(w).join("Fonts"));
        }
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(l).join("Microsoft").join("Windows").join("Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.extend(["/System/Library/Fonts", "/Library/Fonts"].map(PathBuf::from));
        if let Some(h) = &home {
            dirs.push(h.join("Library").join("Fonts"));
        }
    } else {
        dirs.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(PathBuf::from));
        if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
            dirs.push(PathBuf::from(d).join("fonts"));
        }
        if let Some(h) = &home {
            dirs.push(h.join(".local").join("share").join("fonts"));
            dirs.push(h.join(".fonts"));
        }
    }
    dirs
}

fn is_font_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["ttf", "otf", "ttc", "otc"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Font files under `dirs` (bounded walk, symbolic links not followed), in a stable order.
pub fn font_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = dirs.iter().rev().map(|d| (d.clone(), 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let mut subdirs = Vec::new();
        for e in entries {
            let Ok(kind) = e.file_type() else { continue };
            let path = e.path();
            if kind.is_dir() && depth < MAX_DEPTH {
                subdirs.push((path, depth + 1));
            } else if kind.is_file() && is_font_file(&path) {
                out.push(path);
                if out.len() >= MAX_FILES {
                    return out;
                }
            }
        }
        stack.extend(subdirs.into_iter().rev());
    }
    out
}

/// The faces a font file declares (each face of a collection), or none for data that is not a font.
pub fn faces_in(data: &[u8], path: &Path) -> Vec<SystemFace> {
    if data.len() > MAX_FONT_FILE {
        return Vec::new();
    }
    // A collection ("ttcf" header) lists its face count after the version; anything else is one face.
    let count = match data.get(..4) {
        Some(b"ttcf") => data
            .get(8..12)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map_or(0, u32::from_be_bytes)
            .min(MAX_COLLECTION_FACES),
        _ => 1,
    };
    (0..count)
        .filter_map(|index| {
            let font = skrifa::FontRef::from_index(data, index).ok()?;
            let pick = |id: StringId| {
                font.localized_strings(id).english_or_first().map(|s| s.chars().collect::<String>())
            };
            let attrs = font.attributes();
            // Narrow and wide cuts join their typographic family ("Noto Sans" for "Noto Sans Condensed"); keep
            // them apart under their own family name so they never stand in for the normal width.
            let normal_width = (attrs.stretch.ratio() - Stretch::NORMAL.ratio()).abs() < 0.01;
            let family = if normal_width {
                pick(StringId::TYPOGRAPHIC_FAMILY_NAME).or_else(|| pick(StringId::FAMILY_NAME))
            } else {
                pick(StringId::FAMILY_NAME)
            }?;
            let weight = attrs.weight.value();
            let weight = if weight.is_finite() { weight.round().clamp(1.0, 1000.0) as u16 } else { 400 };
            let italic = !matches!(attrs.style, Style::Normal);
            Some(SystemFace { family, weight, italic, path: path.to_owned(), index })
        })
        .collect()
}

/// Indexes the fonts under `dirs` (see [`font_files`]; files over [`MAX_FONT_FILE`] are skipped).
pub fn scan(dirs: &[PathBuf]) -> SystemFonts {
    let mut faces = Vec::new();
    for path in font_files(dirs) {
        let small = std::fs::metadata(&path)
            .is_ok_and(|m| usize::try_from(m.len()).is_ok_and(|len| len <= MAX_FONT_FILE));
        if !small {
            continue;
        }
        if let Ok(data) = std::fs::read(&path) {
            faces.extend(faces_in(&data, &path));
        }
    }
    SystemFonts::from_faces(faces)
}
