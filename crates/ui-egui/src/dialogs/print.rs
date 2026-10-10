//! Print dialogs: Print (Print Options: printer, print style, copies, page range, preview) and Page Setup
//! (paper, orientation, margins, header and footer with fields). Both edit the session's print settings shared
//! with the backstage Print page ([`crate::backstage::print_settings`]); jobs run through the engine's
//! `print.*` commands.

use egui::{Context, RichText};
use serde_json::{Value, json};

use super::common::{Dlg, Out, btn, footer, heading, muted, ok_cancel, primary, window, with_state};
use crate::App;
use crate::backstage::{
    PAPERS, PRINT_FIELDS, PrintSettings, SAVE_AS_PDF, print_preview, print_settings, print_styles,
    printer_combo, run_print, set_print_settings,
};

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    Some(match d.name {
        "print" => print(ctx, app, d),
        "pageSetup" => page_setup(ctx, app, d),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------------------------------
// Print

#[derive(Clone, Default)]
struct PrintState {
    started: bool,
    /// Page range: all pages (false) or the `pages` field (true).
    some_pages: bool,
    error: String,
}

fn print(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let ids = d.ids();
    let style = d.str("style");
    let setup = d.get("setup").cloned();
    with_state(
        ctx,
        app,
        d.uid,
        |_: &App| PrintState::default(),
        |st, app| {
            let mut ps = print_settings(ctx, app);
            if !st.started {
                st.started = true;
                if !style.is_empty() {
                    ps.style = style.clone();
                }
                if let Some(s) = &setup {
                    ps.apply_setup(s);
                }
                st.some_pages = !ps.pages.trim().is_empty();
                ps.page = 0;
            }
            let (styles, default_style) = print_styles(app, &ids);
            let out = window(ctx, app, d, "Print", 760.0, |ui, app| {
                let mut out = Out::Keep;
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(250.0);
                        heading(ui, app, "Printer");
                        ui.horizontal(|ui| {
                            ui.label("Name:");
                            printer_combo(ui, ("mc-pd-printer", d.uid), &mut ps, 190.0);
                        });
                        if ps.printer == SAVE_AS_PDF {
                            ui.horizontal(|ui| {
                                ui.label("File:");
                                ui.add(
                                    egui::TextEdit::singleline(&mut ps.path)
                                        .hint_text("Choose when printing")
                                        .desired_width(200.0),
                                );
                            });
                        } else {
                            muted(ui, app, "Sent to the system print service.");
                        }
                        heading(ui, app, "Print style");
                        for (id, label) in &styles {
                            let on = if ps.style.is_empty() { *id == default_style } else { *id == ps.style };
                            if ui.radio(on, label.as_str()).clicked() {
                                ps.style = id.clone();
                                ps.page = 0;
                            }
                        }
                        if btn(ui, "Page Setup...") {
                            let st_id =
                                if ps.style.is_empty() { default_style.clone() } else { ps.style.clone() };
                            out = Out::open("pageSetup", json!({"style": st_id, "setup": ps.setup_json()}));
                        }
                        heading(ui, app, "Copies");
                        ui.horizontal(|ui| {
                            ui.label("Number of copies:");
                            ui.add(egui::DragValue::new(&mut ps.copies).range(1..=99));
                        });
                        heading(ui, app, "Page range");
                        if ui.radio(!st.some_pages, "All").clicked() {
                            st.some_pages = false;
                        }
                        ui.horizontal(|ui| {
                            if ui.radio(st.some_pages, "Pages:").clicked() {
                                st.some_pages = true;
                            }
                            let r = ui.add_enabled(
                                st.some_pages,
                                egui::TextEdit::singleline(&mut ps.pages)
                                    .hint_text("1-3, 5")
                                    .desired_width(120.0),
                            );
                            if r.changed() {
                                st.some_pages = true;
                            }
                        });
                        if !st.some_pages {
                            ps.pages.clear();
                        }
                        muted(ui, app, &format!("{} item(s) selected.", ids.len()));
                        if !st.error.is_empty() {
                            ui.colored_label(app.theme.danger, &st.error);
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        let job = ps.job(&ids);
                        print_preview(ui, app, &job, &mut ps, &format!("mc-pd-{}", d.uid), 420.0);
                    });
                });
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        out = Out::Close;
                    }
                    if primary(ui, app, true, "Print") {
                        match run_print(app, &mut ps, &ids) {
                            Ok(text) => {
                                out = Out::close_open("message", json!({"title": "Print", "text": text}))
                            }
                            Err(e) => st.error = e,
                        }
                    }
                });
                out
            });
            set_print_settings(ctx, ps);
            out
        },
    )
}

// ---------------------------------------------------------------------------------------------------------
// Page Setup

#[derive(Clone)]
struct SetupState {
    ps: PrintSettings,
    /// Header/footer slot the field buttons insert into: 0..3 header, 3..6 footer.
    slot: usize,
}

const SLOT_NAMES: [&str; 3] = ["Left", "Center", "Right"];

fn page_setup(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let style = d.str("style");
    let label = d.str("label");
    let setup = d.get("setup").cloned().unwrap_or(Value::Null);
    let base = print_settings(ctx, app);
    let init = move |_: &App| {
        let mut ps = base;
        ps.apply_setup(&setup);
        SetupState { ps, slot: 4 }
    };
    with_state(ctx, app, d.uid, init, |st, app| {
        let title = if label.is_empty() { "Page Setup".to_owned() } else { format!("Page Setup: {label}") };
        window(ctx, app, d, &title, 520.0, |ui, app| {
            heading(ui, app, "Paper");
            ui.horizontal(|ui| {
                ui.label("Size:");
                let mut paper = st.ps.paper.unwrap_or(if style == "smallBooklet" { 3 } else { 0 });
                if egui::ComboBox::from_id_salt(("mc-ps-paper", d.uid))
                    .width(120.0)
                    .selected_text(PAPERS.get(paper).copied().unwrap_or("Letter"))
                    .show_index(ui, &mut paper, PAPERS.len(), |i| PAPERS.get(i).copied().unwrap_or(""))
                    .changed()
                {
                    st.ps.paper = Some(paper);
                }
            });
            heading(ui, app, "Orientation");
            let land = st.ps.landscape.unwrap_or(style == "monthly");
            ui.horizontal(|ui| {
                if ui.radio(!land, "Portrait").clicked() {
                    st.ps.landscape = Some(false);
                }
                if ui.radio(land, "Landscape").clicked() {
                    st.ps.landscape = Some(true);
                }
            });
            heading(ui, app, "Margins (inches)");
            egui::Grid::new(("mc-ps-margins", d.uid)).num_columns(4).show(ui, |ui| {
                for (i, name) in ["Top:", "Bottom:", "Left:", "Right:"].iter().enumerate() {
                    if let Some(m) = st.ps.margins.get_mut(i) {
                        ui.label(*name);
                        ui.add(egui::DragValue::new(m).range(0.0..=4.0).speed(0.01).fixed_decimals(2));
                    }
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
            if style == "memo" {
                ui.checkbox(&mut st.ps.new_page_per_item, "Start each item on a new page");
            }
            heading(ui, app, "Header");
            slots(ui, &mut st.ps.header, &mut st.slot, 0, d.uid);
            heading(ui, app, "Footer");
            slots(ui, &mut st.ps.footer, &mut st.slot, 3, d.uid);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Insert:").color(app.theme.text_muted));
                for f in PRINT_FIELDS {
                    if ui
                        .small_button(f)
                        .on_hover_text("Insert into the selected header or footer box")
                        .clicked()
                    {
                        let target = if st.slot < 3 {
                            st.ps.header.get_mut(st.slot)
                        } else {
                            st.ps.footer.get_mut(st.slot - 3)
                        };
                        if let Some(t) = target {
                            if !t.is_empty() && !t.ends_with(' ') {
                                t.push(' ');
                            }
                            t.push_str(f);
                        }
                    }
                }
            });
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    let mut shared = print_settings(ctx, app);
                    let ps = &st.ps;
                    shared.paper = ps.paper;
                    shared.landscape = ps.landscape;
                    shared.margins = ps.margins;
                    shared.header = ps.header.clone();
                    shared.footer = ps.footer.clone();
                    shared.new_page_per_item = ps.new_page_per_item;
                    set_print_settings(ctx, shared);
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

/// Left / Center / Right boxes of a header or footer; focusing one makes it the field-insert target.
fn slots(ui: &mut egui::Ui, v: &mut [String; 3], slot: &mut usize, base: usize, uid: u64) {
    ui.horizontal(|ui| {
        for (i, s) in v.iter_mut().enumerate() {
            ui.vertical(|ui| {
                ui.label(RichText::new(SLOT_NAMES.get(i).copied().unwrap_or("")).small());
                let r = ui.add(
                    egui::TextEdit::singleline(s).id_salt(("mc-ps-slot", uid, base + i)).desired_width(150.0),
                );
                if r.gained_focus() || r.has_focus() {
                    *slot = base + i;
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use serde_json::json;

    use crate::backstage::{PrintSettings, SAVE_AS_PDF, print_settings, run_print, set_print_settings};
    use crate::{App, Surface};
    use mailcraft_engine::sample::sample_session;

    fn harness(app: App) -> Harness<'static, App> {
        let mut h = Harness::builder().with_size(egui::vec2(1280.0, 900.0)).build_ui_state(
            |ui, app: &mut App| {
                let ctx = ui.ctx().clone();
                crate::backstage::backstage(&ctx, app);
                crate::dialogs::dialogs(&ctx, app);
            },
            app,
        );
        h.run_steps(3);
        h
    }

    fn app_with_selection() -> (App, u64) {
        let mut app = App::new(sample_session());
        let folder = app.s.selection.folder;
        let id = app.s.mailbox.items_in(folder).first().map(|m| m.id.0).unwrap();
        app.run("nav.selectItem", json!({"id": id})).unwrap();
        (app, id)
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let base = std::env::current_exe()
            .ok()
            .and_then(|e| e.ancestors().nth(3).map(|t| t.join("tmp")))
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp"));
        let dir = base.join(format!("mc-print-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn backstage_print_page_shows_styles_printer_and_preview_controls() {
        let (mut app, _) = app_with_selection();
        app.ui.backstage = Some((Surface::Main, "print".into()));
        let mut h = harness(app);
        for label in
            ["Memo Style", "Table Style", "Print Options...", "Page Setup...", "One Page", "Actual Size"]
        {
            assert!(h.query_by_label(label).is_some(), "{label}");
        }
        assert!(h.query_by_label("of 1").is_some(), "one-page memo preview");
        h.get_by_label("Table Style").click();
        h.run_steps(3);
        let ps = print_settings(&h.ctx.clone(), h.state_mut());
        assert_eq!(ps.style, "table");
        assert_eq!(ps.printer, SAVE_AS_PDF);
        h.get_by_label("Multiple Pages").click();
        h.run_steps(2);
        assert!(h.query_by_label("of 1").is_some());
    }

    #[test]
    fn save_as_pdf_from_the_ui_settings() {
        let (mut app, id) = app_with_selection();
        let dir = scratch("pdf");
        let path = dir.join("out.pdf");
        let mut ps = PrintSettings {
            path: path.display().to_string(),
            landscape: Some(true),
            ..PrintSettings::default()
        };
        let msg = run_print(&mut app, &mut ps, &[id]).unwrap();
        assert!(msg.starts_with("Saved 1 page(s)"), "{msg}");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF-1.7"));
        ps.printer = "-bad".into();
        assert!(run_print(&mut app, &mut ps, &[id]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn print_dialog_opens_page_setup_which_updates_the_settings() {
        let (mut app, id) = app_with_selection();
        crate::dialogs::open(&mut app, "print", json!({"ids": [id], "style": "memo"}));
        let mut h = harness(app);
        for label in ["Printer", "Print style", "Copies", "Page range", "Pages:"] {
            assert!(h.query_by_label(label).is_some(), "{label}");
        }
        h.get_by_label("Page Setup...").click();
        h.run_steps(3);
        let names: Vec<String> = h.state().ui.dialogs.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(names, ["print", "pageSetup"]);
        assert!(h.query_by_label("Start each item on a new page").is_some());
        h.get_by_label("Landscape").click();
        h.run_steps(2);
        h.get_by_label("OK").click();
        h.run_steps(3);
        let names: Vec<String> = h.state().ui.dialogs.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(names, ["print"]);
        let ctx = h.ctx.clone();
        let ps = print_settings(&ctx, h.state_mut());
        assert_eq!(ps.landscape, Some(true));
        assert_eq!(ps.setup_json()["orientation"], "landscape");
        set_print_settings(&ctx, PrintSettings::default());
    }
}
