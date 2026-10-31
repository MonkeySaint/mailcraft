//! Attachments of a compose: the attachments well under the header, Attach File (Browse This PC…, recent
//! files), files dropped from the OS, and the platform hooks for file pickers.
//!
//! - **Well**: one chip per attachment (type icon, name, size) with a ▾ menu: Open, Save As…, Remove. Open
//!   writes the file to a private folder under the user's cache folder and opens it with the system handler; Save As… asks for a
//!   path (the platform's save dialog when the app installed one, else a path box) and writes the bytes.
//! - **Attach File ▾ › Browse This PC…** runs the platform's open-file dialog when the app installed one
//!   ([`set_file_hooks`]; the desktop app uses the native dialog, the web build has none) and attaches every
//!   file picked; without a picker it falls back to the engine's Insert File dialog (a path box).
//! - **Recent files**: paths attached recently (kept by the engine across restarts, files that still exist),
//!   newest first, listed at the top of Attach File ▾ and Link ▾.
//! - **Drag and drop**: files dropped onto a compose window are attached; while files hover over it the
//!   window shows a drop hint.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use egui::{Align2, CornerRadius, Id, Rect, Sense, Stroke, Ui, vec2};
use serde_json::{Value, json};

use crate::App;

/// Most entries kept in the recent-files list.
pub const MAX_RECENT: usize = mailcraft_engine::cmd::compose::MAX_RECENT_FILES;
/// Drop hint text.
pub const DROP_HINT: &str = "Drop files here to attach them";

/// Platform services the compose window uses for files. All optional: without a picker Browse This PC… falls
/// back to a path box, without `open_path` Open hands a `file://` URL to the platform's URL opener.
#[derive(Clone, Copy, Debug, Default)]
pub struct FileHooks {
    /// Open-file dialog for Browse This PC… (several files; `None` = cancelled).
    pub pick_files: Option<fn() -> Option<Vec<PathBuf>>>,
    /// Save dialog for Save As… (suggested file name in, chosen path out; `None` = cancelled).
    pub pick_save: Option<fn(&str) -> Option<PathBuf>>,
    /// Opens a file with the system handler; returns whether it worked.
    pub open_path: Option<fn(&Path) -> bool>,
}

static HOOKS: RwLock<FileHooks> =
    RwLock::new(FileHooks { pick_files: None, pick_save: None, open_path: None });

/// Installs the platform's file services (call once at start-up from the app).
pub fn set_file_hooks(hooks: FileHooks) {
    match HOOKS.write() {
        Ok(mut h) => *h = hooks,
        Err(poisoned) => *poisoned.into_inner() = hooks,
    }
}

/// The installed file services.
pub fn file_hooks() -> FileHooks {
    match HOOKS.read() {
        Ok(h) => *h,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

/// Recently attached file paths that still exist, newest first.
pub fn recent_files(app: &App) -> Vec<String> {
    mailcraft_engine::cmd::compose::recent_files(&app.s)
}

/// Puts `path` at the top of the recent-files list.
pub fn remember_file(app: &mut App, path: &str) {
    mailcraft_engine::cmd::compose::remember_file(&mut app.s, path);
}

/// The file name part of a path (for menus and chips).
pub fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_owned())
}

/// Attaches the file at `path` to compose `cid` and remembers it. Returns whether it worked.
pub fn attach_path(app: &mut App, cid: u64, path: &str) -> bool {
    app.run("compose.attachFile", json!({"compose": cid, "path": path})).is_some()
}

/// Browse This PC…: the platform's picker when installed (returns true: handled), else false (the caller
/// lets the engine open its Insert File dialog).
pub fn browse(app: &mut App, cid: u64) -> bool {
    let Some(pick) = file_hooks().pick_files else {
        return false;
    };
    for p in pick().unwrap_or_default().into_iter().take(500) {
        attach_path(app, cid, &p.to_string_lossy());
    }
    true
}

// ---------------------------------------------------------------------------------------------------------
// Drag and drop

/// Attaches files dropped this frame. `area`: only when the pointer is over it (an embedded window or an
/// inline response shares the main window's input); `None` = the whole viewport is the compose window.
pub(super) fn take_dropped_files(ctx: &egui::Context, app: &mut App, cid: u64, area: Option<Rect>) {
    let (hovering, dropped, pointer) = ctx.input(|i| {
        (!i.raw.hovered_files.is_empty(), !i.raw.dropped_files.is_empty(), i.pointer.latest_pos())
    });
    let inside = match (area, pointer) {
        (None, _) => true,
        (Some(r), Some(p)) => r.contains(p),
        (Some(_), None) => false,
    };
    if !inside {
        return;
    }
    if hovering {
        paint_drop_hint(ctx, app, area.unwrap_or_else(|| ctx.content_rect()));
    }
    if !dropped {
        return;
    }
    let files = ctx.input_mut(|i| std::mem::take(&mut i.raw.dropped_files));
    for f in files.into_iter().take(500) {
        let path = f.path().to_path_buf();
        if path.is_absolute() {
            attach_path(app, cid, &path.to_string_lossy());
            continue;
        }
        // Web drops carry only a name; their bytes come from the browser.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let name = file_name(&path.to_string_lossy());
            match f.bytes() {
                Ok(data) => {
                    let b64 = mailcraft_mime::transfer::encode_base64_flat(&data);
                    app.run("compose.attachFile", json!({"compose": cid, "name": name, "data_base64": b64}));
                }
                Err(e) => app.ui.toast = Some((format!("Could not read {name}: {e}"), 0.0)),
            }
        }
    }
    ctx.request_repaint();
}

fn paint_drop_hint(ctx: &egui::Context, app: &App, area: Rect) {
    let painter =
        ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::unique("mailcraft-drop-hint")));
    let r = area.shrink(8.0);
    painter.rect_filled(r, CornerRadius::same(6), app.theme.accent.gamma_multiply(0.12));
    painter.rect_stroke(
        r,
        CornerRadius::same(6),
        Stroke::new(2.0, app.theme.accent),
        egui::StrokeKind::Inside,
    );
    painter.text(
        r.center(),
        Align2::CENTER_CENTER,
        DROP_HINT,
        egui::FontId::proportional(18.0),
        app.theme.text,
    );
    ctx.request_repaint();
}

// ---------------------------------------------------------------------------------------------------------
// The well

/// Icon for an attachment's MIME type.
pub fn icon_for(mime: &str) -> &'static str {
    let m = mime.to_ascii_lowercase();
    if m.starts_with("image/") {
        "picture"
    } else if m == "message/rfc822" {
        "email"
    } else if m.contains("calendar") {
        "calendar"
    } else if m.contains("vcard") {
        "person"
    } else if m.starts_with("text/") {
        "note"
    } else {
        "paperclip"
    }
}

/// "12 KB" / "1.2 MB".
pub fn size_text(bytes: usize) -> String {
    let b = bytes as f64;
    if b < 1024.0 * 1024.0 {
        format!("{} KB", ((b / 1024.0).ceil() as u64).max(1))
    } else {
        format!("{:.1} MB", b / (1024.0 * 1024.0))
    }
}

/// What a chip menu picked.
enum ChipAct {
    Open(usize),
    SaveAs(usize),
    Remove(usize),
}

/// The attachments well (nothing when there are no attachments). `indent`: left inset (lines the chips up
/// with the address fields).
pub(super) fn well(ui: &mut Ui, app: &mut App, cid: u64, indent: f32) {
    let Some(c) = app.s.composes.get(&cid) else {
        return;
    };
    if c.attachments.is_empty() {
        return;
    }
    let items: Vec<(String, String, usize)> =
        c.attachments.iter().map(|a| (a.filename.clone(), a.mime_type.clone(), a.data.len())).collect();
    let mut act = None;
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.add_space(indent);
        for (i, (name, mime, len)) in items.iter().enumerate() {
            egui::Frame::new()
                .stroke(Stroke::new(1.0, app.theme.separator))
                .corner_radius(CornerRadius::same(4))
                .inner_margin(egui::Margin::symmetric(6, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (ir, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                        app.icons.paint(ui, ir, icon_for(mime), app.theme.text_muted);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label(egui::RichText::new(name).color(app.theme.text));
                            ui.label(
                                egui::RichText::new(size_text(*len)).color(app.theme.text_muted).small(),
                            );
                        });
                        let menu = ui
                            .add(egui::Button::new("▾").frame(false).small())
                            .on_hover_text(format!("Attachment options for {name}"));
                        menu.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Options for {name}"))
                        });
                        egui::Popup::menu(&menu).show(|ui| {
                            ui.set_min_width(140.0);
                            if ui.button("Open").clicked() {
                                act = Some(ChipAct::Open(i));
                            }
                            if ui.button("Save As...").clicked() {
                                act = Some(ChipAct::SaveAs(i));
                            }
                            ui.separator();
                            if ui.button("Remove Attachment").clicked() {
                                act = Some(ChipAct::Remove(i));
                            }
                        });
                    });
                });
        }
    });
    ui.add_space(4.0);
    let ctx = ui.ctx().clone();
    match act {
        Some(ChipAct::Remove(i)) => {
            app.run("compose.removeAttachment", json!({"compose": cid, "index": i}));
        }
        Some(ChipAct::Open(i)) => open_attachment(&ctx, app, cid, i),
        Some(ChipAct::SaveAs(i)) => save_as(&ctx, app, cid, i),
        None => {}
    }
}

/// Folder for the copies Open hands to other programs: the user's cache folder (`$XDG_CACHE_HOME`,
/// `~/.cache`, `%LOCALAPPDATA%`), not the system temp folder, which is often RAM-backed; the temp folder
/// only when no cache folder is known.
#[cfg(not(target_arch = "wasm32"))]
pub fn open_copies_dir() -> PathBuf {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = var("XDG_CACHE_HOME")
        .filter(|p| p.is_absolute())
        .or_else(|| var("LOCALAPPDATA"))
        .or_else(|| {
            let home = var("HOME")?;
            Some(if cfg!(target_os = "macos") { home.join("Library/Caches") } else { home.join(".cache") })
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("mailcraft").join("open-attachments")
}

/// A copy of attachment `i` for Open (a private folder per compose).
#[cfg(not(target_arch = "wasm32"))]
fn temp_copy(app: &App, cid: u64, i: usize) -> Result<PathBuf, String> {
    let a = app.s.composes.get(&cid).and_then(|c| c.attachments.get(i)).ok_or("the attachment is gone")?;
    let dir = open_copies_dir().join(format!("{}-{cid}-{i}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = file_name(&a.filename);
    let name = if name.is_empty() || name == "." || name == ".." { "attachment".to_owned() } else { name };
    let path = dir.join(name);
    std::fs::write(&path, &a.data).map_err(|e| e.to_string())?;
    Ok(path)
}

fn open_attachment(ctx: &egui::Context, app: &mut App, cid: u64, i: usize) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        match temp_copy(app, cid, i) {
            Ok(path) => {
                let opened = file_hooks().open_path.is_some_and(|open| open(&path));
                if !opened {
                    ctx.open_url(egui::OpenUrl::new_tab(format!("file://{}", path.to_string_lossy())));
                }
            }
            Err(e) => app.ui.toast = Some((format!("Could not open the attachment: {e}"), 0.0)),
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (ctx, cid, i);
        app.ui.toast = Some(("Opening attachments is not available in the browser.".into(), 0.0));
    }
}

/// Writes attachment `i` of compose `cid` to `path`.
pub fn save_attachment_to(app: &App, cid: u64, i: usize, path: &Path) -> Result<(), String> {
    let a = app.s.composes.get(&cid).and_then(|c| c.attachments.get(i)).ok_or("the attachment is gone")?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::fs::write(path, &a.data).map_err(|e| e.to_string())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (a, path);
        Err("saving files is not available in the browser".into())
    }
}

fn save_as(ctx: &egui::Context, app: &mut App, cid: u64, i: usize) {
    let Some(name) = app.s.composes.get(&cid).and_then(|c| c.attachments.get(i)).map(|a| a.filename.clone())
    else {
        return;
    };
    if let Some(pick) = file_hooks().pick_save {
        if let Some(path) = pick(&name)
            && let Err(e) = save_attachment_to(app, cid, i, &path)
        {
            app.ui.toast = Some((format!("Could not save {name}: {e}"), 0.0));
        }
        return;
    }
    let dir = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
    let st = SaveAs { index: i, path: dir.join(file_name(&name)).to_string_lossy().into_owned() };
    ctx.data_mut(|d| d.insert_temp(save_id(cid), st));
}

/// The fallback Save As… path box.
#[derive(Clone, Debug, Default)]
struct SaveAs {
    index: usize,
    path: String,
}

fn save_id(cid: u64) -> Id {
    Id::unique(("mailcraft-compose-save-attachment", cid))
}

/// Title of the fallback Save As box.
pub const SAVE_TITLE: &str = "Save Attachment";

/// Draws the fallback Save As… box of compose `cid` when open.
pub(super) fn save_as_dialog(ctx: &egui::Context, app: &mut App, cid: u64) {
    let Some(mut st) = ctx.data(|d| d.get_temp::<SaveAs>(save_id(cid))) else {
        return;
    };
    let theme = app.theme.clone();
    let mut done: Option<bool> = None;
    let modal =
        egui::Modal::new(Id::unique(("mailcraft-compose-save-attachment-modal", cid))).show(ctx, |ui| {
            crate::dialogs::form_visuals(ui, &theme);
            ui.set_width(460.0);
            ui.label(egui::RichText::new(SAVE_TITLE).strong().size(14.0));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let l = ui.label("File name:");
                ui.add(egui::TextEdit::singleline(&mut st.path).desired_width(360.0)).labelled_by(l.id);
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let size = vec2(80.0, 26.0);
                if ui
                    .add_enabled(!st.path.trim().is_empty(), egui::Button::new("Save").min_size(size))
                    .clicked()
                {
                    done = Some(true);
                }
                if ui.add(egui::Button::new("Cancel").min_size(size)).clicked() {
                    done = Some(false);
                }
            });
        });
    if done.is_none() && modal.should_close() {
        done = Some(false);
    }
    match done {
        Some(true) => {
            if let Err(e) = save_attachment_to(app, cid, st.index, Path::new(st.path.trim())) {
                app.ui.toast = Some((format!("Could not save the attachment: {e}"), 0.0));
            }
            ctx.data_mut(|d| d.remove::<SaveAs>(save_id(cid)));
        }
        Some(false) => {
            ctx.data_mut(|d| d.remove::<SaveAs>(save_id(cid)));
        }
        None => {
            ctx.data_mut(|d| d.insert_temp(save_id(cid), st));
        }
    }
}

/// Params of an Attach File command that names no file (Browse This PC…).
pub fn names_no_file(params: &Value) -> bool {
    ["path", "data_base64", "name"].iter().all(|k| params.get(k).is_none_or(Value::is_null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_files_keep_the_newest_first_and_survive_in_the_settings() {
        let dir = std::env::temp_dir().join(format!("mailcraft-recent-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app =
            App::new(mailcraft_engine::Session::new(mailcraft_engine::model::Mailbox::new_local("L")));
        let path = |i: usize| dir.join(format!("f{i}.txt")).to_string_lossy().into_owned();
        for i in 0..30 {
            std::fs::write(path(i), b"x").unwrap();
            remember_file(&mut app, &path(i));
        }
        remember_file(&mut app, &path(3));
        let list = recent_files(&app);
        assert_eq!(list.len(), MAX_RECENT);
        assert_eq!(list[0], path(3));
        assert_eq!(list.iter().filter(|p| **p == path(3)).count(), 1);
        // A file that is gone is left out.
        std::fs::remove_file(path(29)).unwrap();
        mailcraft_engine::cmd::compose::recheck_recent_files(&app.s);
        assert!(!recent_files(&app).contains(&path(29)));
        remember_file(&mut app, "   ");
        assert_eq!(file_name("/a/b/report.pdf"), "report.pdf");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn icons_and_sizes() {
        assert_eq!(icon_for("image/png"), "picture");
        assert_eq!(icon_for("message/rfc822"), "email");
        assert_eq!(icon_for("text/calendar"), "calendar");
        assert_eq!(icon_for("application/pdf"), "paperclip");
        assert_eq!(size_text(10), "1 KB");
        assert_eq!(size_text(3 * 1024 * 1024), "3.0 MB");
        assert!(names_no_file(&json!({"compose": 1})));
        assert!(!names_no_file(&json!({"path": "/x"})));
    }
}
