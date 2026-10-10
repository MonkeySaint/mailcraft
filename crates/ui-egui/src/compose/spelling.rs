//! The Spelling and Grammar dialog (F7, Review › Spelling and Grammar, Message › Editor): steps through the
//! compose's spelling and grammar issues one at a time. Each step runs `review.spelling {"next": true}`, which
//! re-checks a changed body, selects the next issue in the body and returns it; the buttons run
//! `review.ignore` (Ignore Once / Ignore All), `review.addToDictionary`, and `review.correct` (Change /
//! Change All), then step on. When nothing is left the dialog says the check is complete.
//!
//! The dialog is modal over its compose window (or over the main window for an inline response). Its state
//! lives in egui temp memory per compose.

use egui::{Color32, Id, RichText, vec2};
use serde_json::{Value, json};

use mailcraft_engine::compose::DocPos;

use crate::App;

/// Dialog title.
pub const TITLE: &str = "Spelling and Grammar";
/// Shown when no issue is left.
pub const COMPLETE: &str = "The spelling and grammar check is complete.";
/// Context chars shown on each side of the flagged word.
const CONTEXT_CHARS: usize = 80;

/// The issue on screen.
#[derive(Clone, Debug, Default, PartialEq)]
struct Current {
    start: DocPos,
    word: String,
    kind: String,
    suggestions: Vec<String>,
    /// Text before and after the word in its paragraph.
    before: String,
    after: String,
}

#[derive(Clone, Debug, Default)]
struct SpellDlg {
    open: bool,
    grammar: bool,
    language: String,
    current: Option<Current>,
    /// Selected suggestion.
    pick: usize,
}

fn state_id(cid: u64) -> Id {
    Id::new(("mailcraft-compose-spelling", cid))
}

fn load(ctx: &egui::Context, cid: u64) -> SpellDlg {
    ctx.data(|d| d.get_temp::<SpellDlg>(state_id(cid))).unwrap_or_default()
}

fn store(ctx: &egui::Context, cid: u64, st: SpellDlg) {
    ctx.data_mut(|d| d.insert_temp(state_id(cid), st));
}

/// Whether the spelling dialog of compose `cid` is open.
pub fn is_open(ctx: &egui::Context, cid: u64) -> bool {
    load(ctx, cid).open
}

/// Opens the dialog on compose `cid` (spelling and grammar) and moves to the first issue after the caret.
pub(crate) fn open(ctx: &egui::Context, app: &mut App, cid: u64) {
    open_with(ctx, app, cid, true);
}

/// [`open`] with or without grammar issues.
pub(crate) fn open_with(ctx: &egui::Context, app: &mut App, cid: u64, grammar: bool) {
    if !app.s.composes.contains_key(&cid) {
        return;
    }
    let mut st = SpellDlg { open: true, grammar, ..SpellDlg::default() };
    step(app, cid, &mut st);
    store(ctx, cid, st);
    ctx.request_repaint();
}

fn parse_pos(v: &Value) -> Option<DocPos> {
    let path = v
        .get("path")?
        .as_array()?
        .iter()
        .map(|n| n.as_u64().and_then(|n| usize::try_from(n).ok()))
        .collect::<Option<Vec<usize>>>()?;
    let offset = usize::try_from(v.get("offset")?.as_u64()?).ok()?;
    Some(DocPos::new(path, offset))
}

/// Runs `review.spelling {"next": true}` and loads the issue it selected (or none: complete).
fn step(app: &mut App, cid: u64, st: &mut SpellDlg) {
    st.pick = 0;
    let r = app.run("review.spelling", json!({"compose": cid, "next": true, "grammar": st.grammar}));
    let Some(r) = r else {
        st.current = None;
        return;
    };
    st.language = r.get("language").and_then(Value::as_str).unwrap_or("en-US").to_owned();
    st.current = r.get("current").filter(|v| !v.is_null()).and_then(|cur| {
        let start = parse_pos(cur.get("start")?)?;
        let end = cur.get("end").and_then(parse_pos).unwrap_or_else(|| start.clone());
        let word = cur.get("word").and_then(Value::as_str).unwrap_or_default().to_owned();
        let kind = cur.get("kind").and_then(Value::as_str).unwrap_or("spelling").to_owned();
        let suggestions: Vec<String> = cur
            .get("suggestions")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).take(20).collect())
            .unwrap_or_default();
        let chars = app
            .s
            .composes
            .get(&cid)
            .map(|c| super::editor::leaf_chars(&c.body, &start.path))
            .unwrap_or_default();
        let a = start.offset.min(chars.len());
        let b = end.offset.clamp(a, chars.len());
        let before: String =
            chars.get(a.saturating_sub(CONTEXT_CHARS)..a).unwrap_or_default().iter().collect();
        let after: String = chars
            .get(b..b.saturating_add(CONTEXT_CHARS).min(chars.len()))
            .unwrap_or_default()
            .iter()
            .collect();
        Some(Current { start, word, kind, suggestions, before, after })
    });
}

/// Heading over the context box for an issue kind.
fn kind_heading(kind: &str) -> &'static str {
    match kind {
        "repeated" => "Repeated word:",
        "capitalization" => "Capitalization:",
        "doubleSpace" => "Extra space:",
        _ => "Not in dictionary:",
    }
}

fn language_name(code: &str) -> String {
    match code.to_ascii_lowercase().as_str() {
        "en-us" => "English (United States)".to_owned(),
        "en-gb" => "English (United Kingdom)".to_owned(),
        "en-ca" => "English (Canada)".to_owned(),
        _ => code.to_owned(),
    }
}

/// What a button asked for.
enum Choice {
    IgnoreOnce,
    IgnoreAll,
    Add,
    Change(bool),
    Close,
}

/// Draws the dialog of compose `cid` when open (call once per frame from the compose's viewport).
pub(crate) fn dialog(ctx: &egui::Context, app: &mut App, cid: u64) {
    let mut st = load(ctx, cid);
    if !st.open {
        return;
    }
    if !app.s.composes.contains_key(&cid) {
        ctx.data_mut(|d| d.remove::<SpellDlg>(state_id(cid)));
        return;
    }
    let theme = app.theme.clone();
    let mut choice: Option<Choice> = None;
    let modal = egui::Modal::new(Id::new(("mailcraft-compose-spelling-modal", cid))).show(ctx, |ui| {
        crate::dialogs::form_visuals(ui, &theme);
        ui.set_width(520.0);
        ui.label(RichText::new(format!("{TITLE}: {}", language_name(&st.language))).strong().size(14.0));
        ui.add_space(8.0);
        let Some(cur) = st.current.clone() else {
            ui.label(COMPLETE);
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new("OK").min_size(vec2(80.0, 26.0))).clicked() {
                    choice = Some(Choice::Close);
                }
            });
            return;
        };
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(370.0);
                ui.label(kind_heading(&cur.kind));
                egui::Frame::new().stroke(egui::Stroke::new(1.0, theme.separator)).inner_margin(6.0).show(
                    ui,
                    |ui| {
                        ui.set_width(356.0);
                        ui.set_min_height(64.0);
                        let mut job = egui::text::LayoutJob::default();
                        let normal = egui::TextFormat {
                            color: theme.text,
                            font_id: egui::FontId::proportional(13.0),
                            ..Default::default()
                        };
                        let flagged = egui::TextFormat {
                            color: Color32::from_rgb(0xc4, 0x26, 0x26),
                            font_id: egui::FontId::proportional(13.0),
                            underline: egui::Stroke::new(1.0, Color32::from_rgb(0xc4, 0x26, 0x26)),
                            ..Default::default()
                        };
                        job.append(&cur.before, 0.0, normal.clone());
                        job.append(&cur.word, 0.0, flagged);
                        job.append(&cur.after, 0.0, normal);
                        job.wrap.max_width = 350.0;
                        ui.label(job);
                    },
                );
                ui.add_space(6.0);
                ui.label("Suggestions:");
                egui::Frame::new().stroke(egui::Stroke::new(1.0, theme.separator)).inner_margin(4.0).show(
                    ui,
                    |ui| {
                        ui.set_width(360.0);
                        ui.set_min_height(90.0);
                        if cur.suggestions.is_empty() {
                            ui.weak("(No suggestions)");
                        }
                        for (i, s) in cur.suggestions.iter().enumerate() {
                            let label = if s.is_empty() { "(delete)" } else { s.as_str() };
                            let r = ui.selectable_label(st.pick == i, label);
                            if r.clicked() {
                                st.pick = i;
                            }
                            if r.double_clicked() {
                                st.pick = i;
                                choice = Some(Choice::Change(false));
                            }
                        }
                    },
                );
            });
            ui.vertical(|ui| {
                let size = vec2(130.0, 26.0);
                let has_sugg = !cur.suggestions.is_empty();
                if ui.add(egui::Button::new("Ignore Once").min_size(size)).clicked() {
                    choice = Some(Choice::IgnoreOnce);
                }
                if ui.add(egui::Button::new("Ignore All").min_size(size)).clicked() {
                    choice = Some(Choice::IgnoreAll);
                }
                if ui
                    .add_enabled(
                        cur.kind == "spelling",
                        egui::Button::new("Add to Dictionary").min_size(size),
                    )
                    .clicked()
                {
                    choice = Some(Choice::Add);
                }
                ui.add_space(8.0);
                if ui.add_enabled(has_sugg, egui::Button::new("Change").min_size(size)).clicked() {
                    choice = Some(Choice::Change(false));
                }
                if ui.add_enabled(has_sugg, egui::Button::new("Change All").min_size(size)).clicked() {
                    choice = Some(Choice::Change(true));
                }
                ui.add_space(8.0);
                if ui.add(egui::Button::new("Close").min_size(size)).clicked() {
                    choice = Some(Choice::Close);
                }
            });
        });
    });
    if choice.is_none() && modal.should_close() {
        choice = Some(Choice::Close);
    }
    let at = st.current.as_ref().map(|c| json!({"path": c.start.path, "offset": c.start.offset}));
    let acted = choice.is_some();
    match choice {
        None => {}
        Some(Choice::Close) => {
            st.open = false;
            st.current = None;
        }
        Some(Choice::IgnoreOnce) => {
            app.run("review.ignore", json!({"compose": cid, "at": at}));
            step(app, cid, &mut st);
        }
        Some(Choice::IgnoreAll) => {
            app.run("review.ignore", json!({"compose": cid, "all": true, "at": at}));
            step(app, cid, &mut st);
        }
        Some(Choice::Add) => {
            app.run("review.addToDictionary", json!({"compose": cid, "at": at}));
            step(app, cid, &mut st);
        }
        Some(Choice::Change(all)) => {
            let replacement =
                st.current.as_ref().and_then(|c| c.suggestions.get(st.pick).cloned()).unwrap_or_default();
            app.run(
                "review.correct",
                json!({"compose": cid, "replacement": replacement, "at": at, "all": all}),
            );
            step(app, cid, &mut st);
        }
    }
    if st.open {
        store(ctx, cid, st);
    } else {
        ctx.data_mut(|d| d.remove::<SpellDlg>(state_id(cid)));
        ctx.memory_mut(|m| m.request_focus(super::editor_id(cid)));
    }
    if acted {
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_parse_and_kinds_have_headings() {
        assert_eq!(parse_pos(&json!({"path": [1, 0, 2], "offset": 4})), Some(DocPos::new(vec![1, 0, 2], 4)));
        assert_eq!(parse_pos(&json!({"path": [-1], "offset": 4})), None);
        assert_eq!(parse_pos(&json!({"offset": 4})), None);
        assert_eq!(kind_heading("spelling"), "Not in dictionary:");
        assert_eq!(kind_heading("repeated"), "Repeated word:");
        assert_eq!(language_name("en-US"), "English (United States)");
        assert_eq!(language_name("fr-CA"), "fr-CA");
    }
}
