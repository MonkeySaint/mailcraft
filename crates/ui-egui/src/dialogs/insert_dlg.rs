//! Insert-, Options- and Review-tab dialogs of the compose window: Create New Building Block and the
//! Building Blocks Organizer (Quick Parts, AutoText, Text Box, Quick Tables and Equation galleries), Insert
//! Item, Equation (with a live preview), Convert Text to Table, the Draw Table pad, Screenshot (saved picture
//! fallback) and Screen Clipping, Symbol, Page Color, Browse for Themes, Save Current Theme, Language,
//! Thesaurus and Read Aloud. Every action goes through `app.run`.

use egui::{Color32, Context, CornerRadius, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use serde_json::json;

use super::common::{Dlg, Out, btn, btn_en, footer, muted, primary, window, with_state};
use super::compose_dlg::cmd;
use crate::App;
use mailcraft_engine::cmd::insert::{DEFAULT_CATEGORY, GALLERIES};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "createBuildingBlock" => create_block(ctx, app, d),
        "quickParts" => organizer(ctx, app, d, None),
        "quickTables" => organizer(ctx, app, d, Some("quickTables")),
        "convertTextToTable" => convert_text(ctx, app, d),
        "drawTable" => draw_table(ctx, app, d),
        "screenshot" => saved_screenshot(ctx, app, d),
        "screenClip" => screen_clip(ctx, app, d),
        "symbol" => symbol(ctx, app, d),
        "browseThemes" => browse_themes(ctx, app, d),
        "saveTheme" => save_theme(ctx, app, d),
        "readAloud" => read_aloud(ctx, app, d),
        "language" => language(ctx, app, d),
        "thesaurus" => thesaurus(ctx, app, d),
        "attachItem" => insert_item(ctx, app, d),
        "equation" => equation(ctx, app, d),
        _ => return None,
    })
}

/// Display name of a building-block gallery.
pub(crate) fn gallery_label(g: &str) -> &'static str {
    match g {
        "autoText" => "AutoText",
        "textBoxes" => "Text Boxes",
        "quickTables" => "Quick Tables",
        "equations" => "Equations",
        _ => "Quick Parts",
    }
}

fn gallery_key(label: &str) -> &'static str {
    GALLERIES.iter().copied().find(|g| gallery_label(g) == label).unwrap_or("quickParts")
}

fn compose_of(d: &Dlg<'_>) -> u64 {
    d.u64("compose").unwrap_or(0)
}

// ---------------------------------------------------------------------------------------------------------
// Building blocks

#[derive(Clone, Default)]
struct BlockState {
    name: String,
    gallery: String,
    category: String,
    description: String,
    /// The name exists in the gallery and OK was pressed once (the next OK replaces it).
    confirm_replace: bool,
    error: String,
}

/// Create New Building Block (Save Selection to … Gallery) and, with `mode: "edit"`, Modify Building Block
/// (Edit Properties in the organizer).
fn create_block(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    let editing = d.str("mode") == "edit";
    let original = d.str("original");
    let original_gallery = d.str("gallery");
    let init = BlockState {
        name: d.str("name"),
        gallery: gallery_label(&d.str("gallery")).to_owned(),
        category: if d.str("category").is_empty() { DEFAULT_CATEGORY.to_owned() } else { d.str("category") },
        description: d.str("description"),
        ..Default::default()
    };
    let title = if editing { "Modify Building Block" } else { "Create New Building Block" };
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| init,
        |st, app| {
            window(ctx, app, d, title, 420.0, |ui, app| {
                let gallery = gallery_key(&st.gallery);
                let entries = mailcraft_engine::cmd::insert::gallery_entries(&app.s, cid, gallery);
                let mut categories: Vec<String> = vec![DEFAULT_CATEGORY.to_owned()];
                for b in &entries {
                    if !b.category.is_empty()
                        && !categories.iter().any(|c| c.eq_ignore_ascii_case(&b.category))
                    {
                        categories.push(b.category.clone());
                    }
                }
                let labels: Vec<&str> = GALLERIES.iter().map(|g| gallery_label(g)).collect();
                let before = (st.name.clone(), st.gallery.clone());
                egui::Grid::new(("bb", d.uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("Name:");
                    ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(260.0));
                    ui.end_row();
                    ui.label("Gallery:");
                    super::common::combo(ui, ("bbg", d.uid), &mut st.gallery, &labels, 250.0);
                    ui.end_row();
                    ui.label("Category:");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut st.category).desired_width(160.0));
                        let refs: Vec<&str> = categories.iter().map(String::as_str).collect();
                        let mut pick = String::new();
                        if super::common::combo(ui, ("bbc", d.uid), &mut pick, &refs, 80.0) {
                            st.category = pick;
                        }
                    });
                    ui.end_row();
                    ui.label("Description:");
                    ui.add(egui::TextEdit::singleline(&mut st.description).desired_width(260.0));
                    ui.end_row();
                    ui.label("Save in:");
                    muted(ui, app, "Building blocks of this profile");
                    ui.end_row();
                });
                if before != (st.name.clone(), st.gallery.clone()) {
                    st.confirm_replace = false;
                }
                let name = st.name.trim().to_owned();
                let same_as_original =
                    editing && name.eq_ignore_ascii_case(&original) && gallery == original_gallery;
                let taken = !same_as_original && entries.iter().any(|b| b.name.eq_ignore_ascii_case(&name));
                if taken && st.confirm_replace {
                    ui.colored_label(
                        app.theme.danger,
                        format!(
                            "{} already has an entry named \u{201c}{name}\u{201d}. OK replaces it.",
                            gallery_label(gallery)
                        ),
                    );
                }
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let mut out = Out::Keep;
                let mut go = false;
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        out = Out::Close;
                    }
                    go = primary(ui, app, !name.is_empty(), "OK")
                        || (!name.is_empty() && super::common::enter_pressed(ui));
                });
                if go && matches!(out, Out::Keep) {
                    if taken && !st.confirm_replace && !editing {
                        st.confirm_replace = true;
                        return Out::Keep;
                    }
                    let result = if editing {
                        cmd(
                            app,
                            "insert.quickPart",
                            json!({"compose": cid, "action": "edit", "name": original, "gallery": original_gallery,
                                   "newName": name, "newGallery": gallery, "category": st.category.trim(),
                                   "description": st.description.trim()}),
                        )
                    } else {
                        cmd(
                            app,
                            "insert.saveToGallery",
                            json!({"compose": cid, "gallery": gallery, "name": name,
                                   "category": st.category.trim(), "description": st.description.trim()}),
                        )
                    };
                    out = match result {
                        Ok(_) => Out::Close,
                        Err(e) => {
                            st.error = e;
                            Out::Keep
                        }
                    };
                }
                out
            })
        },
    )
}

#[derive(Clone, Default)]
struct OrganizerState {
    /// Selected entry: (gallery, name).
    pick: Option<(String, String)>,
    error: String,
}

/// Building Blocks Organizer (Insert Quick Part…, Insert AutoText…, Quick Tables…): every saved entry with its
/// gallery and category, a preview, and Insert / Delete / Edit Properties.
fn organizer(ctx: &Context, app: &mut App, d: &Dlg<'_>, only: Option<&'static str>) -> Out {
    let cid = compose_of(d);
    let filter: Option<String> =
        only.map(str::to_owned).or_else(|| Some(d.str("gallery")).filter(|g| !g.is_empty()));
    let title = match filter.as_deref() {
        Some("autoText") => "AutoText",
        Some("quickTables") => "Quick Tables",
        Some("textBoxes") => "Text Boxes",
        Some("equations") => "Equations",
        _ => "Building Blocks Organizer",
    };
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| OrganizerState::default(),
        |st, app| {
            window(ctx, app, d, title, 620.0, |ui, app| {
                let galleries: Vec<&str> = match filter.as_deref() {
                    Some(g) => GALLERIES.iter().copied().filter(|x| *x == g).collect(),
                    None => GALLERIES.to_vec(),
                };
                let entries: Vec<mailcraft_engine::compose::BuildingBlock> = galleries
                    .iter()
                    .flat_map(|g| mailcraft_engine::cmd::insert::gallery_entries(&app.s, cid, g))
                    .collect();
                if st
                    .pick
                    .as_ref()
                    .is_some_and(|(g, n)| !entries.iter().any(|b| &b.gallery == g && &b.name == n))
                {
                    st.pick = None;
                }
                let mut out = Out::Keep;
                let mut insert_now = false;
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(340.0);
                        egui::Grid::new(("bbo-h", d.uid)).num_columns(3).min_col_width(100.0).show(ui, |ui| {
                            ui.label(RichText::new("Name").strong());
                            ui.label(RichText::new("Gallery").strong());
                            ui.label(RichText::new("Category").strong());
                            ui.end_row();
                        });
                        egui::ScrollArea::vertical().id_salt(("bbo", d.uid)).max_height(240.0).show(ui, |ui| {
                            if entries.is_empty() {
                                muted(ui, app, "Nothing saved yet. Select part of a message and use Save Selection to save it here.");
                            }
                            egui::Grid::new(("bbo-g", d.uid)).num_columns(3).min_col_width(100.0).show(ui, |ui| {
                                for b in &entries {
                                    let on = st.pick.as_ref().is_some_and(|(g, n)| *g == b.gallery && *n == b.name);
                                    let r = ui.selectable_label(on, &b.name);
                                    if r.clicked() {
                                        st.pick = Some((b.gallery.clone(), b.name.clone()));
                                    }
                                    if r.double_clicked() {
                                        st.pick = Some((b.gallery.clone(), b.name.clone()));
                                        insert_now = true;
                                    }
                                    ui.label(gallery_label(&b.gallery));
                                    ui.label(if b.category.is_empty() { DEFAULT_CATEGORY } else { &b.category });
                                    ui.end_row();
                                }
                            });
                        });
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_width(230.0);
                        ui.label(RichText::new("Preview").strong());
                        let picked = st.pick.as_ref().and_then(|(g, n)| entries.iter().find(|b| &b.gallery == g && &b.name == n));
                        match picked {
                            Some(b) => {
                                let text: String = mailcraft_model::Doc { blocks: b.blocks.clone(), ..Default::default() }
                                    .text_content()
                                    .chars()
                                    .take(400)
                                    .collect();
                                egui::Frame::new().fill(app.theme.reading).inner_margin(6.0).show(ui, |ui| {
                                    ui.set_min_height(120.0);
                                    ui.label(text);
                                });
                                ui.label(RichText::new(&b.name).strong());
                                if !b.description.is_empty() {
                                    muted(ui, app, &b.description);
                                }
                            }
                            None => muted(ui, app, "Select an entry to see it here."),
                        }
                    });
                });
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let pick = st.pick.clone();
                let has = pick.is_some();
                let mut action: Option<&str> = insert_now.then_some("insert");
                footer(ui, |ui| {
                    if btn(ui, "Close") {
                        out = Out::Close;
                    }
                    if primary(ui, app, has, "Insert") {
                        action = Some("insert");
                    }
                    if btn_en(ui, has, "Delete") {
                        action = Some("delete");
                    }
                    if btn_en(ui, has, "Edit Properties...") {
                        action = Some("edit");
                    }
                });
                if let (Some(action), Some((g, n))) = (action, pick) {
                    match action {
                        "edit" => {
                            let b = entries.iter().find(|b| b.gallery == g && b.name == n);
                            return Out::open(
                                "createBuildingBlock",
                                json!({"compose": cid, "mode": "edit", "original": n, "name": n, "gallery": g,
                                       "category": b.map(|b| b.category.clone()), "description": b.map(|b| b.description.clone())}),
                            );
                        }
                        _ => match cmd(
                            app,
                            "insert.quickPart",
                            json!({"compose": cid, "gallery": g, "name": n, "action": action}),
                        ) {
                            Ok(_) if action == "insert" => out = Out::Close,
                            Ok(_) => st.pick = None,
                            Err(e) => st.error = e,
                        },
                    }
                }
                out
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------
// Equation

#[derive(Clone, Default)]
struct EquationState {
    text: String,
    error: String,
}

/// The preview texture of equation `text` (decoded from the engine's drawing; only the latest is kept).
fn equation_texture(ctx: &Context, text: &str) -> Option<egui::TextureHandle> {
    type Cached = (String, Option<egui::TextureHandle>);
    let id = egui::Id::new("equation-preview");
    if let Some((t, tex)) = ctx.data(|m| m.get_temp::<Cached>(id))
        && t == text
    {
        return tex;
    }
    let tex = mailcraft_engine::cmd::insert::equation_png(text).and_then(|(png, _, _)| {
        let img = image::load_from_memory(&png).ok()?.to_rgba8();
        let size = [usize::try_from(img.width()).ok()?, usize::try_from(img.height()).ok()?];
        let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
        Some(ctx.load_texture("equation-preview", color, egui::TextureOptions::LINEAR))
    });
    ctx.data_mut(|m| m.insert_temp::<Cached>(id, (text.to_owned(), tex.clone())));
    tex
}

/// Insert New Equation: the equation typed in linear form with a live two-dimensional preview, the
/// built-in equations to start from, and a short guide to the notation.
fn equation(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    use mailcraft_engine::cmd::insert::EQUATION_PRESETS;
    let cid = compose_of(d);
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| EquationState::default(),
        |st, app| {
            window(ctx, app, d, "Equation", 560.0, |ui, app| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Start from:");
                    for (_, name, formula) in EQUATION_PRESETS {
                        if ui.small_button(name).clicked() {
                            st.text = formula.to_owned();
                        }
                    }
                });
                ui.add(
                    egui::TextEdit::singleline(&mut st.text)
                        .hint_text("Type an equation, e.g. x=(−b±√(b^2−4ac))/(2a)")
                        .desired_width(f32::INFINITY),
                );
                muted(
                    ui,
                    app,
                    "a/b fraction · x^2 power · x_i index · √(…) root · ( ) group · ∑_(k=0)^n sum",
                );
                ui.horizontal_wrapped(|ui| {
                    for sym in
                        ["π", "θ", "α", "β", "∞", "±", "×", "÷", "≤", "≥", "≠", "≈", "√", "∑", "∏", "∫", "→"]
                    {
                        if ui.small_button(sym).clicked() {
                            st.text.push_str(sym);
                        }
                    }
                });
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .stroke(Stroke::new(1.0, app.theme.separator))
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        ui.set_min_size(Vec2::new(ui.available_width(), 90.0));
                        match equation_texture(ctx, st.text.trim()) {
                            Some(tex) => {
                                let size = tex.size_vec2();
                                let scale = (ui.available_width() / size.x).min(1.0);
                                ui.add(
                                    egui::Image::new((tex.id(), size * scale)).alt_text("Equation preview"),
                                );
                            }
                            None => muted(ui, app, "The preview shows here."),
                        }
                    });
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let mut out = Out::Keep;
                let mut go = false;
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        out = Out::Close;
                    }
                    go = primary(ui, app, !st.text.trim().is_empty(), "Insert");
                });
                if go {
                    match cmd(app, "insert.equation", json!({"compose": cid, "text": st.text.trim()})) {
                        Ok(_) => out = Out::Close,
                        Err(e) => st.error = e,
                    }
                }
                out
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------
// Insert Item

/// Most items listed for one folder.
const MAX_FOLDER_ITEMS: usize = 1000;

/// The items of folder `fid` as (id, label), newest first for mail.
pub(crate) fn folder_items(app: &App, fid: u64) -> Vec<(u64, String)> {
    use mailcraft_model::{FolderId, FolderKind};
    let mb = &app.s.mailbox;
    let fid = FolderId(fid);
    let untitled =
        |s: &str, none: &str| if s.trim().is_empty() { none.to_owned() } else { s.trim().to_owned() };
    let mut out: Vec<(u64, String)> = match mb.folder(fid).map(|f| f.kind) {
        Some(FolderKind::Mail) => {
            let mut mail = mb.items_in(fid);
            mail.sort_by_key(|m| std::cmp::Reverse(m.received.unix));
            mail.iter()
                .map(|m| {
                    let from = m
                        .from
                        .as_ref()
                        .map(|f| f.name.clone().unwrap_or_else(|| f.addr.clone()))
                        .unwrap_or_default();
                    (m.id.0, format!("{}  \u{2014}  {from}", untitled(&m.subject, "(no subject)")))
                })
                .collect()
        }
        Some(FolderKind::Contacts) => mb
            .contacts
            .values()
            .filter(|c| c.folder == fid)
            .map(|c| (c.id.0, untitled(&c.full_name(), "(no name)")))
            .collect(),
        Some(FolderKind::Calendar) => mb
            .appointments
            .values()
            .filter(|a| a.folder == fid)
            .map(|a| (a.id.0, untitled(&a.subject, "(no subject)")))
            .collect(),
        Some(FolderKind::Tasks) => mb
            .tasks
            .values()
            .filter(|t| t.folder == fid)
            .map(|t| (t.id.0, untitled(&t.subject, "(no subject)")))
            .collect(),
        Some(FolderKind::Notes) => mb
            .notes
            .values()
            .filter(|n| n.folder == fid)
            .map(|n| (n.id.0, untitled(n.text.lines().next().unwrap_or_default(), "(empty note)")))
            .collect(),
        _ => Vec::new(),
    };
    out.truncate(MAX_FOLDER_ITEMS);
    out
}

#[derive(Clone, Default)]
struct ItemState {
    folder: Option<u64>,
    pick: Option<u64>,
    as_text: bool,
    error: String,
}

/// Insert Item: a folder tree (mail, contacts, calendars, tasks, notes), the folder's items, and Insert as
/// Attachment or Text only.
fn insert_item(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    use super::common::{Pick, folder_tree};
    use mailcraft_model::FolderKind;
    const KINDS: &[FolderKind] =
        &[FolderKind::Mail, FolderKind::Contacts, FolderKind::Calendar, FolderKind::Tasks, FolderKind::Notes];
    let cid = compose_of(d);
    with_state(
        ctx,
        app,
        d.uid,
        |a: &App| ItemState { folder: Some(a.s.selection.folder.0), ..Default::default() },
        |st, app| {
            window(ctx, app, d, "Insert Item", 640.0, |ui, app| {
                let mut activated = false;
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(240.0);
                        ui.label("Look in:");
                        let before = st.folder;
                        folder_tree(ui, app, &mut st.folder, Pick::Kinds(KINDS), ("ii-tree", d.uid), 260.0);
                        if before != st.folder {
                            st.pick = None;
                        }
                    });
                    ui.vertical(|ui| {
                        ui.set_width(360.0);
                        ui.label("Items:");
                        let items = st.folder.map(|f| folder_items(app, f)).unwrap_or_default();
                        egui::Frame::new()
                            .stroke(Stroke::new(1.0, app.theme.separator))
                            .inner_margin(4.0)
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt(("ii-items", d.uid))
                                    .max_height(260.0)
                                    .min_scrolled_height(260.0)
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        if items.is_empty() {
                                            muted(ui, app, "This folder has no items.");
                                        }
                                        for (id, label) in &items {
                                            let r = ui.selectable_label(st.pick == Some(*id), label);
                                            if r.clicked() {
                                                st.pick = Some(*id);
                                            }
                                            if r.double_clicked() {
                                                st.pick = Some(*id);
                                                activated = true;
                                            }
                                        }
                                    });
                            });
                    });
                });
                ui.horizontal(|ui| {
                    ui.label("Insert as:");
                    ui.radio_value(&mut st.as_text, false, "Attachment");
                    ui.radio_value(&mut st.as_text, true, "Text only");
                });
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let mut out = Out::Keep;
                let mut go = activated;
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        out = Out::Close;
                    }
                    go |= primary(ui, app, st.pick.is_some(), "OK");
                });
                if go && let Some(id) = st.pick {
                    let mode = if st.as_text { "text" } else { "attachment" };
                    match cmd(
                        app,
                        "compose.attachItem",
                        json!({"compose": cid, "itemId": id.to_string(), "as": mode}),
                    ) {
                        Ok(_) => out = Out::Close,
                        Err(e) => st.error = e,
                    }
                }
                out
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------
// Tables

#[derive(Clone)]
struct ConvertState {
    separator: String,
    other: String,
    cols: u32,
    error: String,
}

const SEPARATORS: [(&str, &str); 5] = [
    ("paragraphs", "Paragraphs"),
    ("commas", "Commas"),
    ("tabs", "Tabs"),
    ("semicolons", "Semicolons"),
    ("other", "Other:"),
];

/// Convert Text to Table: where to separate the text, and the number of columns.
fn convert_text(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    let rows = d.u64("rows").unwrap_or(1);
    let init = ConvertState {
        separator: if d.str("separator").is_empty() { "commas".into() } else { d.str("separator") },
        other: "-".into(),
        cols: u32::try_from(d.u64("cols").unwrap_or(1)).unwrap_or(1).clamp(1, 63),
        error: String::new(),
    };
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| init,
        |st, app| {
            window(ctx, app, d, "Convert Text to Table", 380.0, |ui, app| {
                super::common::heading(ui, app, "Table size");
                ui.horizontal(|ui| {
                    ui.label("Number of columns:");
                    let paragraphs = st.separator == "paragraphs";
                    ui.add(egui::DragValue::new(&mut st.cols).range(1..=63));
                    if paragraphs {
                        muted(ui, app, &format!("Rows: {}", (rows as u32).div_ceil(st.cols.max(1))));
                    } else {
                        muted(ui, app, &format!("Rows: {rows}"));
                    }
                });
                super::common::heading(ui, app, "Separate text at");
                ui.horizontal_wrapped(|ui| {
                    for (key, label) in SEPARATORS {
                        ui.radio_value(&mut st.separator, key.to_owned(), label);
                    }
                    let other_on = st.separator == "other";
                    ui.add_enabled(
                        other_on,
                        egui::TextEdit::singleline(&mut st.other).char_limit(1).desired_width(24.0),
                    );
                });
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let mut out = Out::Keep;
                match super::common::ok_cancel(ui, app, true) {
                    Some(true) => {
                        let sep =
                            if st.separator == "other" { st.other.clone() } else { st.separator.clone() };
                        match cmd(
                            app,
                            "insert.table",
                            json!({"compose": cid, "mode": "fromText", "separator": sep, "cols": st.cols}),
                        ) {
                            Ok(_) => out = Out::Close,
                            Err(e) => st.error = e,
                        }
                    }
                    Some(false) => out = Out::Close,
                    None => {}
                }
                out
            })
        },
    )
}

/// Draw Table pad state: the outer box and the lines drawn inside it (pad coordinates, px).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Drawing {
    pub outer: Option<Rect>,
    /// Column lines (x).
    pub cols: Vec<f32>,
    /// Row lines (y).
    pub rows: Vec<f32>,
}

/// Smallest drawn box and nearest line spacing (px).
const MIN_BOX: f32 = 24.0;
const MIN_CELL: f32 = 8.0;
/// How near a click must be to a line for the eraser.
const ERASE_SLOP: f32 = 5.0;

impl Drawing {
    /// Adds what a pencil stroke from `a` to `b` draws: the outer box first, then a column line (mostly
    /// vertical stroke) or a row line (mostly horizontal), inside the box.
    pub(crate) fn stroke(&mut self, a: Pos2, b: Pos2) {
        let Some(outer) = self.outer else {
            let r = Rect::from_two_pos(a, b);
            if r.width() >= MIN_BOX && r.height() >= MIN_BOX {
                self.outer = Some(r);
            }
            return;
        };
        let vertical = (b.y - a.y).abs() >= (b.x - a.x).abs();
        if vertical {
            let x = (a.x + b.x) / 2.0;
            let inside = x > outer.left() + MIN_CELL && x < outer.right() - MIN_CELL;
            if inside && !self.cols.iter().any(|c| (c - x).abs() < MIN_CELL) {
                self.cols.push(x);
                self.cols.sort_by(f32::total_cmp);
            }
        } else {
            let y = (a.y + b.y) / 2.0;
            let inside = y > outer.top() + MIN_CELL && y < outer.bottom() - MIN_CELL;
            if inside && !self.rows.iter().any(|r| (r - y).abs() < MIN_CELL) {
                self.rows.push(y);
                self.rows.sort_by(f32::total_cmp);
            }
        }
    }

    /// Eraser: removes the line nearest `p` (or the whole drawing when `p` is on the outer box).
    pub(crate) fn erase(&mut self, p: Pos2) {
        if let Some(i) = self.cols.iter().position(|x| (x - p.x).abs() <= ERASE_SLOP) {
            self.cols.remove(i);
        } else if let Some(i) = self.rows.iter().position(|y| (y - p.y).abs() <= ERASE_SLOP) {
            self.rows.remove(i);
        } else if self.outer.is_some_and(|o| {
            (o.left() - p.x).abs() <= ERASE_SLOP
                || (o.right() - p.x).abs() <= ERASE_SLOP
                || (o.top() - p.y).abs() <= ERASE_SLOP
                || (o.bottom() - p.y).abs() <= ERASE_SLOP
        }) {
            *self = Drawing::default();
        }
    }

    /// Column widths (px) and the number of rows of the drawn table.
    pub(crate) fn table(&self) -> Option<(Vec<f32>, usize)> {
        let o = self.outer?;
        let mut edges = vec![o.left()];
        edges.extend(self.cols.iter().copied());
        edges.push(o.right());
        let widths: Vec<f32> = edges.windows(2).filter_map(|w| Some(w.get(1)? - w.first()?)).collect();
        Some((widths, self.rows.len() + 1))
    }
}

#[derive(Clone, Default)]
struct DrawState {
    drawing: Drawing,
    eraser: bool,
    error: String,
}

/// Draw Table: a pencil pad. Drag a box for the table, then draw lines across it for columns and rows; the
/// eraser removes a line. Insert makes the table with the drawn column widths.
fn draw_table(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| DrawState::default(),
        |st, app| {
            window(ctx, app, d, "Draw Table", 560.0, |ui, app| {
                muted(
                    ui,
                    app,
                    if st.drawing.outer.is_none() {
                        "Drag to draw the table's outside border."
                    } else {
                        "Draw lines across the table to add columns and rows."
                    },
                );
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut st.eraser, false, "Pencil");
                    ui.selectable_value(&mut st.eraser, true, "Eraser");
                    if ui.button("Clear").clicked() {
                        st.drawing = Drawing::default();
                    }
                });
                let (pad, resp) = ui.allocate_exact_size(Vec2::new(520.0, 300.0), Sense::click_and_drag());
                resp.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Table drawing area")
                });
                let t = app.theme.clone();
                let painter = ui.painter_at(pad);
                painter.rect_filled(pad, CornerRadius::same(2), t.reading);
                painter.rect_stroke(
                    pad,
                    CornerRadius::same(2),
                    Stroke::new(1.0, t.separator),
                    StrokeKind::Inside,
                );
                let to_pad = |p: Pos2| Pos2::new(p.x - pad.left(), p.y - pad.top());
                let to_screen = |p: Pos2| Pos2::new(p.x + pad.left(), p.y + pad.top());
                let drag_id = egui::Id::new(("draw-table-from", d.uid));
                // The stroke starts where the button went down (a drag is only recognised after some movement).
                if resp.drag_started()
                    && let Some(p) =
                        ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos())
                {
                    ui.ctx().data_mut(|m| m.insert_temp(drag_id, to_pad(p)));
                }
                let from: Option<Pos2> = ui.ctx().data(|m| m.get_temp(drag_id));
                let now = ui.ctx().pointer_latest_pos().map(to_pad);
                if resp.drag_stopped() {
                    if let (Some(a), Some(b)) = (from, now)
                        && !st.eraser
                    {
                        st.drawing.stroke(a, b);
                    }
                    ui.ctx().data_mut(|m| m.remove::<Pos2>(drag_id));
                } else if resp.clicked()
                    && st.eraser
                    && let Some(p) = resp.interact_pointer_pos()
                {
                    st.drawing.erase(to_pad(p));
                }
                let ink = Stroke::new(1.5, t.text);
                if let Some(o) = st.drawing.outer {
                    let o = Rect::from_min_max(to_screen(o.min), to_screen(o.max));
                    painter.rect_stroke(o, CornerRadius::ZERO, ink, StrokeKind::Middle);
                    for x in &st.drawing.cols {
                        painter.line_segment(
                            [Pos2::new(pad.left() + x, o.top()), Pos2::new(pad.left() + x, o.bottom())],
                            ink,
                        );
                    }
                    for y in &st.drawing.rows {
                        painter.line_segment(
                            [Pos2::new(o.left(), pad.top() + y), Pos2::new(o.right(), pad.top() + y)],
                            ink,
                        );
                    }
                }
                if resp.dragged()
                    && !st.eraser
                    && let (Some(a), Some(b)) = (from, now)
                {
                    let preview = Stroke::new(1.0, t.accent);
                    if st.drawing.outer.is_none() {
                        painter.rect_stroke(
                            Rect::from_two_pos(to_screen(a), to_screen(b)),
                            CornerRadius::ZERO,
                            preview,
                            StrokeKind::Middle,
                        );
                    } else {
                        painter.line_segment([to_screen(a), to_screen(b)], preview);
                    }
                }
                if !st.error.is_empty() {
                    ui.colored_label(t.danger, &st.error);
                }
                let table = st.drawing.table();
                let mut out = Out::Keep;
                let mut go = false;
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        out = Out::Close;
                    }
                    go = primary(ui, app, table.is_some(), "Insert");
                });
                if go && let Some((widths, rows)) = table {
                    match cmd(
                        app,
                        "insert.table",
                        json!({"compose": cid, "mode": "draw", "widths": widths, "rows": rows}),
                    ) {
                        Ok(_) => out = Out::Close,
                        Err(e) => st.error = e,
                    }
                }
                out
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------
// Screenshot

#[derive(Clone, Default)]
struct PathState {
    path: String,
    name: String,
    error: String,
}

/// Screenshot without a capture: why, and a saved picture to insert instead.
fn saved_screenshot(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    let message = d.str("message");
    let window_name = d.str("window");
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| PathState::default(),
        |st, app| {
            window(ctx, app, d, "Screenshot", 480.0, |ui, app| {
                if !message.is_empty() {
                    ui.label(&message);
                    ui.add_space(4.0);
                }
                ui.horizontal(|ui| {
                    ui.label("Saved screenshot:");
                    ui.add(egui::TextEdit::singleline(&mut st.path).desired_width(250.0));
                    if super::common::browse_button(ui, app)
                        && let Some(p) = super::common::browse_open(
                            app,
                            "Insert a Saved Screenshot",
                            &[crate::FileFilter::new(
                                "Pictures",
                                &["png", "jpg", "jpeg", "gif", "bmp", "webp"],
                            )],
                            &st.path,
                        )
                    {
                        st.path = p;
                    }
                });
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let mut out = Out::Keep;
                let mut go = false;
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        out = Out::Close;
                    }
                    go = primary(ui, app, !st.path.trim().is_empty(), "Insert");
                });
                if go {
                    let w = (!window_name.is_empty()).then(|| window_name.clone());
                    match cmd(
                        app,
                        "insert.screenshot",
                        json!({"compose": cid, "path": st.path.trim(), "window": w}),
                    ) {
                        Ok(_) => out = Out::Close,
                        Err(e) => st.error = e,
                    }
                }
                out
            })
        },
    )
}

#[derive(Clone, Default)]
struct ClipState {
    /// Selected region in picture pixels (x, y, w, h).
    region: Option<[u32; 4]>,
    from: Option<Pos2>,
    error: String,
}

/// Largest side of the Screen Clipping preview texture (bigger captures are shown scaled down).
const MAX_PREVIEW_SIDE: u32 = 4096;

fn clip_texture(ctx: &Context, token: u64, shot: &crate::compose::Captured) -> Option<egui::TextureHandle> {
    let id = egui::Id::new(("screen-clip-texture", token));
    if let Some(t) = ctx.data(|m| m.get_temp::<egui::TextureHandle>(id)) {
        return Some(t);
    }
    let img = image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba.clone())?;
    let img = if shot.width.max(shot.height) > MAX_PREVIEW_SIDE {
        let scale = MAX_PREVIEW_SIDE as f32 / shot.width.max(shot.height) as f32;
        let (w, h) =
            (((shot.width as f32) * scale).max(1.0) as u32, ((shot.height as f32) * scale).max(1.0) as u32);
        image::imageops::thumbnail(&img, w, h)
    } else {
        img
    };
    let size = [usize::try_from(img.width()).ok()?, usize::try_from(img.height()).ok()?];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    let tex = ctx.load_texture(format!("screen-clip-{token}"), color, egui::TextureOptions::LINEAR);
    ctx.data_mut(|m| m.insert_temp(id, tex.clone()));
    Some(tex)
}

/// Screen Clipping: the captured screen, scaled to fit; drag a region and insert it (or the whole screen).
fn screen_clip(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    let token = d.u64("clip").unwrap_or(0);
    let Some(shot) = crate::compose::capture::clip_image(token) else {
        return Out::Close;
    };
    let out = with_state(
        ctx,
        app,
        d.uid,
        |_: &App| ClipState::default(),
        |st, app| {
            window(ctx, app, d, "Screen Clipping", 960.0, |ui, app| {
                muted(ui, app, "Drag across the part of the screen to insert.");
                let Some(tex) = clip_texture(ctx, token, &shot) else {
                    ui.label("The capture cannot be shown.");
                    return if footer(ui, |ui| btn(ui, "Close")) { Out::Close } else { Out::Keep };
                };
                let avail = Vec2::new(920.0, 540.0);
                let scale = (avail.x / shot.width as f32).min(avail.y / shot.height as f32).min(1.0);
                let size = Vec2::new(shot.width as f32 * scale, shot.height as f32 * scale);
                let (rect, resp) = ui.allocate_exact_size(size, Sense::drag());
                resp.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Captured screen")
                });
                ui.painter().image(
                    tex.id(),
                    rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                let to_px = |p: Pos2| -> (u32, u32) {
                    let x = ((p.x - rect.left()) / scale).clamp(0.0, shot.width as f32);
                    let y = ((p.y - rect.top()) / scale).clamp(0.0, shot.height as f32);
                    (x as u32, y as u32)
                };
                if resp.drag_started() {
                    st.from = ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos());
                }
                if (resp.dragged() || resp.drag_stopped())
                    && let (Some(a), Some(b)) = (st.from, ui.ctx().pointer_latest_pos())
                {
                    let (ax, ay) = to_px(a);
                    let (bx, by) = to_px(b);
                    let (x, y) = (ax.min(bx), ay.min(by));
                    let (w, h) = (ax.abs_diff(bx), ay.abs_diff(by));
                    st.region = (w >= 2 && h >= 2).then_some([x, y, w, h]);
                }
                if let Some([x, y, w, h]) = st.region {
                    let r = Rect::from_min_size(
                        Pos2::new(rect.left() + x as f32 * scale, rect.top() + y as f32 * scale),
                        Vec2::new(w as f32 * scale, h as f32 * scale),
                    );
                    // Dim what is outside the region.
                    let dim = Color32::from_black_alpha(110);
                    let p = ui.painter();
                    p.rect_filled(Rect::from_min_max(rect.min, Pos2::new(rect.right(), r.top())), 0.0, dim);
                    p.rect_filled(Rect::from_min_max(Pos2::new(rect.left(), r.bottom()), rect.max), 0.0, dim);
                    p.rect_filled(
                        Rect::from_min_max(Pos2::new(rect.left(), r.top()), Pos2::new(r.left(), r.bottom())),
                        0.0,
                        dim,
                    );
                    p.rect_filled(
                        Rect::from_min_max(
                            Pos2::new(r.right(), r.top()),
                            Pos2::new(rect.right(), r.bottom()),
                        ),
                        0.0,
                        dim,
                    );
                    p.rect_stroke(
                        r,
                        CornerRadius::ZERO,
                        Stroke::new(1.5, app.theme.accent),
                        StrokeKind::Outside,
                    );
                    muted(ui, app, &format!("{w} × {h} pixels"));
                }
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let mut out = Out::Keep;
                let mut pick: Option<Option<[u32; 4]>> = None;
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        out = Out::Close;
                    }
                    if primary(ui, app, st.region.is_some(), "Insert") {
                        pick = Some(st.region);
                    }
                    if btn(ui, "Insert Whole Screen") {
                        pick = Some(None);
                    }
                });
                if let Some(region) = pick {
                    let part = match region {
                        Some([x, y, w, h]) => crate::compose::capture::crop(&shot, x, y, w, h),
                        None => Some((*shot).clone()),
                    };
                    match part {
                        Some(part) if crate::compose::capture::insert_capture(app, cid, &part, None) => {
                            out = Out::Close
                        }
                        Some(_) => st.error = super::folders::last_error(app),
                        None => st.error = "Drag across the screen to choose a region.".into(),
                    }
                }
                out
            })
        },
    );
    if !out.keeps() {
        crate::compose::capture::drop_clip(token);
        ctx.data_mut(|m| m.remove::<egui::TextureHandle>(egui::Id::new(("screen-clip-texture", token))));
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Symbol

/// Symbol subsets (Unicode block names and ranges).
pub(crate) const SYMBOL_SUBSETS: [(&str, u32, u32); 14] = [
    ("Latin-1 Supplement", 0xa1, 0xff),
    ("Latin Extended-A", 0x100, 0x17f),
    ("Greek and Coptic", 0x391, 0x3c9),
    ("Cyrillic", 0x410, 0x44f),
    ("General Punctuation", 0x2010, 0x2044),
    ("Currency Symbols", 0x20a0, 0x20c0),
    ("Letterlike Symbols", 0x2100, 0x214f),
    ("Number Forms", 0x2150, 0x218b),
    ("Arrows", 0x2190, 0x21ff),
    ("Mathematical Operators", 0x2200, 0x22ff),
    ("Box Drawing", 0x2500, 0x257f),
    ("Geometric Shapes", 0x25a0, 0x25ff),
    ("Miscellaneous Symbols", 0x2600, 0x26ff),
    ("Dingbats", 0x2700, 0x27bf),
];

/// The characters of subset `i` (unassigned and control code points left out).
pub(crate) fn subset_chars(i: usize) -> Vec<char> {
    let Some((_, from, to)) = SYMBOL_SUBSETS.get(i) else {
        return Vec::new();
    };
    (*from..=*to)
        .filter_map(char::from_u32)
        .filter(|c| !c.is_control() && !c.is_whitespace() && !(0x3a2..=0x3a2).contains(&u32::from(*c)))
        .collect()
}

#[derive(Clone, Default)]
struct SymbolState {
    subset: usize,
    pick: Option<char>,
    code: String,
    error: String,
    /// 0 = (normal text), else a symbol font of `SYMBOL_FONTS` (index + 1).
    font: usize,
    /// The Special Characters tab is shown.
    special: bool,
}

/// Special Characters: (name, character). Names are MailCraft's own.
pub(crate) const SPECIAL_CHARACTERS: [(&str, char); 20] = [
    ("Em dash", '\u{2014}'),
    ("En dash", '\u{2013}'),
    ("Nonbreaking hyphen", '\u{2011}'),
    ("Optional hyphen", '\u{00AD}'),
    ("Em space", '\u{2003}'),
    ("En space", '\u{2002}'),
    ("Quarter em space", '\u{2005}'),
    ("Nonbreaking space", '\u{00A0}'),
    ("Copyright", '\u{00A9}'),
    ("Registered", '\u{00AE}'),
    ("Trademark", '\u{2122}'),
    ("Section", '\u{00A7}'),
    ("Paragraph", '\u{00B6}'),
    ("Ellipsis", '\u{2026}'),
    ("Single opening quote", '\u{2018}'),
    ("Single closing quote", '\u{2019}'),
    ("Double opening quote", '\u{201C}'),
    ("Double closing quote", '\u{201D}'),
    ("Zero-width space (optional break)", '\u{200B}'),
    ("Word joiner (no break)", '\u{2060}'),
];

/// The characters a symbol font offers in the dialog: (label for the tooltip, character).
fn symbol_font_chars(font: &str) -> Vec<(String, char)> {
    mailcraft_engine::cmd::insert::symbol_fonts::characters(font)
        .into_iter()
        .map(|(code, ch)| (format!("{font}: {code}"), ch))
        .collect()
}

/// More Symbols…: recently used symbols, a subset picker over a character grid, the selected character's
/// code, and a typed character code.
fn symbol(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| SymbolState::default(),
        |st, app| {
            window(ctx, app, d, "Symbol", 520.0, |ui, app| {
                ui.horizontal(|ui| {
                    if ui.selectable_label(!st.special, "Symbols").clicked() {
                        st.special = false;
                    }
                    if ui.selectable_label(st.special, "Special Characters").clicked() {
                        st.special = true;
                    }
                });
                ui.separator();
                let mut insert: Option<String> = None;
                if st.special {
                    egui::ScrollArea::vertical().id_salt(("sym-special", d.uid)).max_height(300.0).show(
                        ui,
                        |ui| {
                            egui::Grid::new(("sym-special-grid", d.uid)).num_columns(2).striped(true).show(
                                ui,
                                |ui| {
                                    for (name, ch) in SPECIAL_CHARACTERS {
                                        let shown = if ch.is_whitespace()
                                            || u32::from(ch) == 0xAD
                                            || (0x200B..=0x2060).contains(&u32::from(ch))
                                        {
                                            format!("U+{:04X}", u32::from(ch))
                                        } else {
                                            ch.to_string()
                                        };
                                        ui.label(shown);
                                        if ui.selectable_label(st.pick == Some(ch), name).clicked() {
                                            st.pick = Some(ch);
                                        }
                                        ui.end_row();
                                    }
                                },
                            );
                        },
                    );
                    let mut out = Out::Keep;
                    footer(ui, |ui| {
                        if btn(ui, "Close") {
                            out = Out::Close;
                        }
                        if primary(ui, app, st.pick.is_some(), "Insert") {
                            insert = st.pick.map(String::from);
                        }
                    });
                    if let Some(ch) = insert
                        && !crate::compose::insert_symbol(app, cid, &ch)
                    {
                        st.error = super::folders::last_error(app);
                    }
                    return out;
                }
                let names: Vec<&str> = SYMBOL_SUBSETS.iter().map(|(n, ..)| *n).collect();
                let mut fonts: Vec<&str> = vec!["(normal text)"];
                fonts.extend(mailcraft_engine::cmd::insert::symbol_fonts::SYMBOL_FONTS);
                ui.horizontal(|ui| {
                    ui.label("Font:");
                    super::common::combo_idx(ui, ("sym-font", d.uid), &mut st.font, &fonts, 140.0);
                    if st.font == 0 {
                        ui.add_space(12.0);
                        ui.label("Subset:");
                        super::common::combo_idx(ui, ("sym-subset", d.uid), &mut st.subset, &names, 200.0);
                    }
                });
                let font_name = fonts.get(st.font).copied().unwrap_or("(normal text)");
                let chars: Vec<(String, char)> = if st.font == 0 {
                    subset_chars(st.subset)
                        .into_iter()
                        .map(|c| (format!("U+{:04X}", u32::from(c)), c))
                        .collect()
                } else {
                    symbol_font_chars(font_name)
                };
                egui::ScrollArea::vertical().id_salt(("sym", d.uid)).max_height(230.0).show(ui, |ui| {
                    egui::Grid::new(("sym-grid", d.uid)).spacing([2.0, 2.0]).show(ui, |ui| {
                        for (i, (tip, ch)) in chars.iter().enumerate() {
                            let on = st.pick == Some(*ch);
                            let b = egui::Button::new(RichText::new(ch.to_string()).size(17.0))
                                .selected(on)
                                .min_size(Vec2::splat(30.0));
                            let r = ui.add(b).on_hover_text(format!("{tip} (U+{:04X})", u32::from(*ch)));
                            if r.clicked() {
                                st.pick = Some(*ch);
                            }
                            if r.double_clicked() {
                                insert = Some(ch.to_string());
                            }
                            if (i + 1) % 16 == 0 {
                                ui.end_row();
                            }
                        }
                    });
                });
                ui.label("Recently used symbols:");
                ui.horizontal_wrapped(|ui| {
                    let recent = crate::compose::recent_symbols();
                    if recent.is_empty() {
                        muted(ui, app, "None yet");
                    }
                    for s in recent {
                        if ui
                            .add(egui::Button::new(RichText::new(&s).size(17.0)).min_size(Vec2::splat(30.0)))
                            .clicked()
                        {
                            insert = Some(s);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if let Some(ch) = st.pick {
                        ui.label(format!("Character code: U+{:04X}", u32::from(ch)));
                    }
                    ui.add_space(12.0);
                    ui.label("Type a code:");
                    ui.add(egui::TextEdit::singleline(&mut st.code).hint_text("U+00A9").desired_width(90.0));
                });
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let mut out = Out::Keep;
                footer(ui, |ui| {
                    if btn(ui, "Close") {
                        out = Out::Close;
                    }
                    let typed = !st.code.trim().is_empty();
                    if primary(ui, app, typed || st.pick.is_some(), "Insert") {
                        insert = Some(if typed {
                            st.code.trim().to_owned()
                        } else {
                            st.pick.map(String::from).unwrap_or_default()
                        });
                    }
                });
                if let Some(ch) = insert
                    && !crate::compose::insert_symbol(app, cid, &ch)
                {
                    st.error = super::folders::last_error(app);
                }
                out
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------
// Options: themes (Page Color › More Colors… is the Colors dialog of `format_dlg`)

const THEME_FILTER: [&str; 1] = ["json"];
/// Office theme files (a theme package or its theme part).
const OFFICE_THEME_FILTER: [&str; 2] = ["thmx", "xml"];
/// Every theme file Browse for Themes opens.
const ALL_THEME_FILTER: [&str; 3] = ["thmx", "xml", "json"];

/// Browse for Themes: a theme file (MailCraft theme, .json) to apply; it joins the Themes gallery.
fn browse_themes(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| PathState::default(),
        |st, app| {
            window(ctx, app, d, "Browse for Themes", 480.0, |ui, app| {
                ui.horizontal(|ui| {
                    ui.label("Theme file:");
                    ui.add(egui::TextEdit::singleline(&mut st.path).desired_width(270.0));
                    if super::common::browse_button(ui, app)
                        && let Some(p) = super::common::browse_open(
                            app,
                            "Browse for Themes",
                            &[
                                crate::FileFilter::new("All themes", &ALL_THEME_FILTER),
                                crate::FileFilter::new("Office themes", &OFFICE_THEME_FILTER),
                                crate::FileFilter::new("MailCraft themes", &THEME_FILTER),
                            ],
                            &st.path,
                        )
                    {
                        st.path = p;
                    }
                });
                muted(
                    ui,
                    app,
                    "An Office theme (.thmx, or its theme .xml) or a theme saved with Save Current Theme. Opened \
                     themes are added to the Themes gallery.",
                );
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let mut out = Out::Keep;
                let mut go = false;
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        out = Out::Close;
                    }
                    go = primary(ui, app, !st.path.trim().is_empty(), "Open");
                });
                if go {
                    match cmd(
                        app,
                        "compose.theme",
                        json!({"compose": cid, "action": "browse", "path": st.path.trim()}),
                    ) {
                        Ok(_) => out = Out::Close,
                        Err(e) => st.error = e,
                    }
                }
                out
            })
        },
    )
}

/// Save Current Theme: a name for the Themes gallery and, optionally, a file to share it.
fn save_theme(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    let name = if d.str("name").is_empty() { "Custom Theme".to_owned() } else { d.str("name") };
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| PathState { name, ..Default::default() },
        |st, app| {
            window(ctx, app, d, "Save Current Theme", 480.0, |ui, app| {
                egui::Grid::new(("save-theme", d.uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("Theme name:");
                    ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(270.0));
                    ui.end_row();
                    ui.label("Also save to file:");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut st.path)
                                .hint_text("(optional)")
                                .desired_width(200.0),
                        );
                        if super::common::browse_button(ui, app) {
                            let suggested = if st.path.trim().is_empty() {
                                format!("{}.thmx", st.name.trim())
                            } else {
                                st.path.clone()
                            };
                            if let Some(p) = super::common::browse_save(
                                app,
                                "Save Current Theme",
                                &[
                                    crate::FileFilter::new("Office themes", &["thmx"]),
                                    crate::FileFilter::new("MailCraft themes", &THEME_FILTER),
                                ],
                                &suggested,
                            ) {
                                st.path = p;
                            }
                        }
                    });
                    ui.end_row();
                });
                muted(ui, app, "The theme is added to the Themes gallery of every message.");
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let mut out = Out::Keep;
                let mut go = false;
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        out = Out::Close;
                    }
                    go = primary(ui, app, !st.name.trim().is_empty(), "Save");
                });
                if go {
                    let path = (!st.path.trim().is_empty()).then(|| st.path.trim().to_owned());
                    match cmd(
                        app,
                        "compose.theme",
                        json!({"compose": cid, "action": "save", "name": st.name.trim(), "path": path}),
                    ) {
                        Ok(_) => out = Out::Close,
                        Err(e) => st.error = e,
                    }
                }
                out
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------
// Review: Language, Read Aloud

/// Proofing languages offered: (tag, name).
const LANGUAGES: [(&str, &str); 14] = [
    ("en-US", "English (United States)"),
    ("en-GB", "English (United Kingdom)"),
    ("en-CA", "English (Canada)"),
    ("en-AU", "English (Australia)"),
    ("fr-CA", "French (Canada)"),
    ("fr-FR", "French (France)"),
    ("es-ES", "Spanish (Spain)"),
    ("es-MX", "Spanish (Mexico)"),
    ("de-DE", "German (Germany)"),
    ("it-IT", "Italian (Italy)"),
    ("pt-BR", "Portuguese (Brazil)"),
    ("nl-NL", "Dutch (Netherlands)"),
    ("ja-JP", "Japanese"),
    ("zh-CN", "Chinese (Simplified)"),
];

#[derive(Clone)]
struct LangState {
    /// (tag, name) rows: [`LANGUAGES`], plus the message's own language when it is not one of them (set by
    /// a command, the CLI or MCP), so OK never swaps it for another.
    choices: Vec<(String, String)>,
    /// Whether each row's spelling is checked (bundled English or an installed dictionary).
    checked: Vec<bool>,
    pick: usize,
    /// The row of the message's language (its tag is only sent when another row is picked).
    was: usize,
    no_proof: bool,
    error: String,
}

/// What the Language dialog says when the picked language has no dictionary.
pub const NO_DICTIONARY_NOTICE: &str = "There is no dictionary for this language on this computer, so spelling \
     will not be checked. Install a Hunspell dictionary for it (or add its folder in Editor Options).";

/// The Language dialog's rows for a message in language `current`, and the row to select: [`LANGUAGES`],
/// the languages of the dictionaries installed on the computer (`installed` tags), and `current` itself.
fn language_choices(current: &str, installed: &[String]) -> (Vec<(String, String)>, usize) {
    let mut choices: Vec<(String, String)> =
        LANGUAGES.iter().map(|(t, n)| ((*t).to_owned(), (*n).to_owned())).collect();
    for tag in installed.iter().take(200) {
        let tag: String = tag.trim().chars().take(35).collect();
        if tag.is_empty() || choices.iter().any(|(t, _)| t.eq_ignore_ascii_case(&tag)) {
            continue;
        }
        let name = match mailcraft_engine::translate::language_name(&tag) {
            Some(n) => format!("{n} ({tag})"),
            None => tag.clone(),
        };
        choices.push((tag, name));
    }
    let current = current.trim();
    let pick = match choices.iter().position(|(t, _)| t.eq_ignore_ascii_case(current)) {
        Some(i) => i,
        None if !current.is_empty() => {
            let tag: String = current.chars().take(35).collect();
            choices.push((tag.clone(), tag));
            choices.len() - 1
        }
        None => 0,
    };
    (choices, pick)
}

/// Set Proofing Language: the message's language, which ones are spell-checked (English with the bundled
/// dictionary, others with a dictionary installed on the computer), and "Do not check spelling or grammar".
fn language(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    let current = d.str("lang");
    let installed: Vec<String> =
        d.arr("dictionaries").iter().filter_map(|v| v.as_str().map(str::to_owned)).collect();
    let (choices, pick) = language_choices(&current, &installed);
    let checked =
        choices.iter().map(|(tag, _)| mailcraft_engine::cmd::review::has_dictionary(&app.s, tag)).collect();
    let init =
        LangState { choices, checked, pick, was: pick, no_proof: d.bool("noProof"), error: String::new() };
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| init,
        |st, app| {
            window(ctx, app, d, "Language", 420.0, |ui, app| {
                ui.label("Mark this message as:");
                egui::ScrollArea::vertical().id_salt(("lang", d.uid)).max_height(220.0).show(ui, |ui| {
                    for (i, (_, name)) in st.choices.iter().enumerate() {
                        let dict = st.checked.get(i).copied().unwrap_or(false);
                        let label = if dict { format!("\u{2713} {name}") } else { format!("    {name}") };
                        if ui.selectable_label(st.pick == i, label).clicked() {
                            st.pick = i;
                        }
                    }
                });
                muted(
                    ui,
                    app,
                    "\u{2713} Spelling is checked: English with MailCraft's dictionary, other languages with the \
                     dictionaries installed on this computer.",
                );
                if !st.checked.get(st.pick).copied().unwrap_or(true) && !st.no_proof {
                    ui.colored_label(app.theme.danger, NO_DICTIONARY_NOTICE);
                }
                ui.checkbox(&mut st.no_proof, "Do not check spelling or grammar");
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let mut out = Out::Keep;
                match super::common::ok_cancel(ui, app, true) {
                    Some(true) => {
                        // The language is only changed when another row was picked.
                        let mut p = json!({"compose": cid, "noProof": st.no_proof});
                        if st.pick != st.was
                            && let (Some((tag, _)), Some(o)) = (st.choices.get(st.pick), p.as_object_mut())
                        {
                            o.insert("lang".into(), json!(tag));
                        }
                        match cmd(app, "review.language", p) {
                            Ok(_) => out = Out::Close,
                            Err(e) => st.error = e,
                        }
                    }
                    Some(false) => out = Out::Close,
                    None => {}
                }
                out
            })
        },
    )
}

#[derive(Clone, Default)]
struct ThesaurusState {
    query: String,
    /// The word whose list is shown.
    shown: String,
    pick: Option<String>,
    /// Words looked up before (Back).
    back: Vec<String>,
}

/// Thesaurus: related words for the selected word, a box to look up another word (double-click a word to
/// look it up), Back, and Insert (replaces the selected word).
fn thesaurus(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let cid = compose_of(d);
    let word = d.str("word");
    let init = ThesaurusState { query: word.clone(), shown: word.clone(), ..Default::default() };
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| init,
        |st, app| {
            window(ctx, app, d, "Thesaurus", 380.0, |ui, app| {
                let mut look: Option<String> = None;
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut st.query).hint_text("Look up").desired_width(220.0),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if btn(ui, "Look Up") || enter {
                        look = Some(st.query.trim().to_owned());
                    }
                    if btn_en(ui, !st.back.is_empty(), "Back")
                        && let Some(prev) = st.back.pop()
                    {
                        st.query = prev.clone();
                        st.shown = prev;
                        st.pick = None;
                    }
                });
                let syns = mailcraft_engine::cmd::review::synonyms(&st.shown);
                let head = mailcraft_engine::cmd::review::thesaurus_headword(&st.shown);
                ui.label(RichText::new(head.unwrap_or(st.shown.as_str())).strong().size(16.0));
                egui::ScrollArea::vertical().id_salt(("thes", d.uid)).max_height(260.0).show(ui, |ui| {
                    if syns.is_empty() {
                        muted(ui, app, "No related words found.");
                    }
                    for s in &syns {
                        let r = ui.selectable_label(st.pick.as_deref() == Some(*s), *s);
                        if r.clicked() {
                            st.pick = Some((*s).to_owned());
                        }
                        if r.double_clicked() {
                            look = Some((*s).to_owned());
                        }
                    }
                });
                if let Some(w) = look.filter(|w| !w.is_empty() && *w != st.shown) {
                    st.back.push(std::mem::replace(&mut st.shown, w.clone()));
                    st.query = w;
                    st.pick = None;
                }
                let mut out = Out::Keep;
                footer(ui, |ui| {
                    if btn(ui, "Close") {
                        out = Out::Close;
                    }
                    if primary(ui, app, st.pick.is_some() && !word.is_empty(), "Insert") {
                        app.run(
                            "review.thesaurus",
                            json!({"compose": cid, "word": word, "replace": st.pick}),
                        );
                        out = Out::Close;
                    }
                });
                out
            })
        },
    )
}

/// Why Read Aloud cannot speak without a backend.
pub(crate) const NO_SPEECH: &str =
    "Speech is not available in this version of MailCraft; the text that would be read is shown below.";

/// Read Aloud: plays the text paragraph by paragraph through the platform's speech, with Play/Pause,
/// Previous, Next, Stop and a reading speed; the paragraph being read is highlighted.
fn read_aloud(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    use crate::speech::{PlayState, Player};
    let text = d.str("text");
    let backend = crate::speech::speech();
    let out = with_state(
        ctx,
        app,
        d.uid,
        |_: &App| Player::new(&text),
        |pl, app| {
            if let Some(b) = backend.as_deref()
                && pl.tick(b)
            {
                ctx.request_repaint_after(std::time::Duration::from_millis(150));
            }
            window(ctx, app, d, "Read Aloud", 500.0, |ui, app| {
                if backend.is_none() {
                    muted(ui, app, NO_SPEECH);
                }
                let mut scroll_to: Option<egui::Rect> = None;
                egui::ScrollArea::vertical().id_salt(("ra", d.uid)).max_height(220.0).show(ui, |ui| {
                    for (i, piece) in pl.pieces.iter().enumerate() {
                        let current = i == pl.index && pl.state != PlayState::Stopped;
                        let label = if current {
                            RichText::new(piece).background_color(app.theme.search_hit).color(Color32::BLACK)
                        } else {
                            RichText::new(piece)
                        };
                        let r = ui.label(label);
                        if current {
                            scroll_to = Some(r.rect);
                        }
                    }
                    if let Some(r) = scroll_to {
                        ui.scroll_to_rect(r, None);
                    }
                });
                if let Some(b) = backend.as_deref() {
                    ui.horizontal(|ui| {
                        let has = !pl.pieces.is_empty();
                        if btn_en(ui, has, "Previous") {
                            pl.step(b, -1);
                        }
                        let play = if pl.state == PlayState::Playing { "Pause" } else { "Play" };
                        if btn_en(ui, has, play) {
                            pl.toggle(b);
                        }
                        if btn_en(ui, has, "Next") {
                            pl.step(b, 1);
                        }
                        if btn_en(ui, pl.state != PlayState::Stopped, "Stop") {
                            pl.stop(b);
                        }
                        ui.add_space(12.0);
                        ui.label("Reading speed:");
                        ui.add(egui::Slider::new(&mut pl.rate, -5..=5).show_value(false));
                    });
                }
                if !pl.error.is_empty() {
                    ui.colored_label(app.theme.danger, &pl.error);
                }
                if footer(ui, |ui| btn(ui, "Close")) { Out::Close } else { Out::Keep }
            })
        },
    );
    if !out.keeps()
        && let Some(b) = backend
    {
        b.stop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_makes_box_then_columns_and_rows() {
        let mut dr = Drawing::default();
        dr.stroke(Pos2::new(10.0, 10.0), Pos2::new(12.0, 12.0));
        assert!(dr.outer.is_none(), "too small for a table");
        dr.stroke(Pos2::new(10.0, 10.0), Pos2::new(310.0, 110.0));
        assert_eq!(dr.table(), Some((vec![300.0], 1)));
        dr.stroke(Pos2::new(110.0, 5.0), Pos2::new(110.0, 120.0));
        dr.stroke(Pos2::new(112.0, 5.0), Pos2::new(112.0, 120.0));
        dr.stroke(Pos2::new(20.0, 60.0), Pos2::new(300.0, 61.0));
        dr.stroke(Pos2::new(400.0, 5.0), Pos2::new(400.0, 120.0));
        let (widths, rows) = dr.table().unwrap();
        assert_eq!(widths, vec![100.0, 200.0], "a second line next to the first and one outside are ignored");
        assert_eq!(rows, 2);
        dr.erase(Pos2::new(111.0, 50.0));
        assert_eq!(dr.table(), Some((vec![300.0], 2)));
        dr.erase(Pos2::new(10.0, 50.0));
        assert_eq!(dr, Drawing::default());
    }

    #[test]
    fn symbol_subsets_have_printable_characters() {
        for (i, (name, ..)) in SYMBOL_SUBSETS.iter().enumerate() {
            let chars = subset_chars(i);
            assert!(!chars.is_empty(), "{name}");
            assert!(chars.iter().all(|c| !c.is_control()));
        }
        assert!(subset_chars(99).is_empty());
        assert!(subset_chars(0).contains(&'©'));
    }

    #[test]
    fn gallery_labels_round_trip() {
        for g in GALLERIES {
            assert_eq!(gallery_key(gallery_label(g)), g);
        }
    }
}
