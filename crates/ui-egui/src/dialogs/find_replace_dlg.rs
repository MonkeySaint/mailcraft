//! Find and Replace for the compose body, with the More options: match case, whole words, wildcards,
//! prefix / suffix, direction, the formatting to find and to replace with, the Special codes, Find In
//! (select every match) and Reading Highlight (Highlight All / Clear Highlighting).

use egui::{Context, Ui};
use serde_json::{Map, Value, json};

use super::common::{self, Dlg, Out, btn, btn_en, footer, muted, window, with_state};
use super::folders::last_error;
use crate::App;

/// Special codes offered in Find what, with their menu labels.
const FIND_SPECIALS: [(&str, &str); 7] = [
    ("^t", "Tab"),
    ("^l", "Line break inside a paragraph"),
    ("^?", "Any one character"),
    ("^#", "Any one digit (0-9)"),
    ("^$", "Any one letter"),
    ("^w", "Run of spaces or tabs"),
    ("^^", "The ^ sign itself"),
];

/// Special codes offered in Replace with.
const REPLACE_SPECIALS: [(&str, &str); 4] = [
    ("^&", "The text that was found"),
    ("^t", "Tab"),
    ("^l", "Line break inside a paragraph"),
    ("^^", "The ^ sign itself"),
];

/// Three-state formatting choice (does not matter / on / off).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tri {
    #[default]
    Any,
    On,
    Off,
}

impl Tri {
    fn json(self) -> Option<Value> {
        match self {
            Tri::Any => None,
            Tri::On => Some(json!(true)),
            Tri::Off => Some(json!(false)),
        }
    }
    fn next(self) -> Tri {
        match self {
            Tri::Any => Tri::On,
            Tri::On => Tri::Off,
            Tri::Off => Tri::Any,
        }
    }
    fn label(self, name: &str) -> String {
        match self {
            Tri::Any => name.to_owned(),
            Tri::On => format!("{name}: on"),
            Tri::Off => format!("{name}: off"),
        }
    }
}

/// Formatting to find or replace with.
#[derive(Clone, Default, PartialEq)]
struct Fmt {
    bold: Tri,
    italic: Tri,
    underline: Tri,
    highlight: Tri,
    font: String,
    size: String,
}

impl Fmt {
    fn is_empty(&self) -> bool {
        *self == Fmt::default()
    }
    fn json(&self) -> Option<Value> {
        if self.is_empty() {
            return None;
        }
        let mut o = Map::new();
        for (k, t) in [
            ("bold", self.bold),
            ("italic", self.italic),
            ("underline", self.underline),
            ("highlight", self.highlight),
        ] {
            if let Some(v) = t.json() {
                o.insert(k.into(), v);
            }
        }
        if !self.font.trim().is_empty() {
            o.insert("font".into(), json!(self.font.trim()));
        }
        if let Ok(pt) = self.size.trim().parse::<f64>() {
            o.insert("size".into(), json!(pt));
        }
        Some(Value::Object(o))
    }
    fn summary(&self) -> String {
        let mut parts = Vec::new();
        for (n, t) in [
            ("Bold", self.bold),
            ("Italic", self.italic),
            ("Underline", self.underline),
            ("Highlight", self.highlight),
        ] {
            match t {
                Tri::On => parts.push(n.to_owned()),
                Tri::Off => parts.push(format!("Not {}", n.to_lowercase())),
                Tri::Any => {}
            }
        }
        if !self.font.trim().is_empty() {
            parts.push(format!("Font: {}", self.font.trim()));
        }
        if !self.size.trim().is_empty() {
            parts.push(format!("{} pt", self.size.trim()));
        }
        parts.join(", ")
    }
}

#[derive(Clone, Default)]
struct FindState {
    find: String,
    replace: String,
    case: bool,
    whole: bool,
    wildcards: bool,
    prefix: bool,
    suffix: bool,
    direction: String,
    more: bool,
    msg: String,
    replace_mode: bool,
    /// Which field the Format and Special menus act on (false: Find what, true: Replace with).
    in_replace: bool,
    find_fmt: Fmt,
    replace_fmt: Fmt,
}

impl FindState {
    fn params(&self, compose: Option<u64>) -> Map<String, Value> {
        let mut p = Map::new();
        p.insert("compose".into(), json!(compose));
        p.insert("matchCase".into(), json!(self.case));
        if self.more {
            p.insert("wholeWord".into(), json!(self.whole && !self.wildcards));
            p.insert("wildcards".into(), json!(self.wildcards));
            p.insert("prefix".into(), json!(self.prefix && !self.wildcards));
            p.insert("suffix".into(), json!(self.suffix && !self.wildcards));
            p.insert("direction".into(), json!(self.direction));
            if let Some(f) = self.find_fmt.json() {
                p.insert("format".into(), f);
            }
            if let Some(f) = self.replace_fmt.json() {
                p.insert("replaceFormat".into(), f);
            }
        }
        p
    }
}

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    let init = |_: &App| FindState {
        replace_mode: d.name == "replace",
        find: d.str("text"),
        direction: "all".into(),
        ..Default::default()
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Find and Replace", if st.more { 560.0 } else { 460.0 }, |ui, app| {
            ui.horizontal(|ui| {
                if ui.selectable_label(!st.replace_mode, "Find").clicked() {
                    st.replace_mode = false;
                }
                if ui.selectable_label(st.replace_mode, "Replace").clicked() {
                    st.replace_mode = true;
                }
            });
            ui.separator();
            let r = common::field(ui, "Find what:", &mut st.find, 300.0);
            if r.gained_focus() {
                st.in_replace = false;
            }
            if st.more && !st.find_fmt.is_empty() {
                muted(ui, app, &format!("Format: {}", st.find_fmt.summary()));
            }
            if st.replace_mode {
                let r = common::field(ui, "Replace with:", &mut st.replace, 290.0);
                if r.gained_focus() {
                    st.in_replace = true;
                }
                if st.more && !st.replace_fmt.is_empty() {
                    muted(ui, app, &format!("Format: {}", st.replace_fmt.summary()));
                }
            }
            if st.more {
                more_options(ui, app, st, uid);
            } else {
                ui.checkbox(&mut st.case, "Match case");
            }
            if !st.msg.is_empty() {
                muted(ui, app, &st.msg);
            }
            footer(ui, |ui| buttons(ui, app, st, compose))
        })
    })
}

fn more_options(ui: &mut Ui, app: &App, st: &mut FindState, uid: u64) {
    ui.separator();
    ui.horizontal(|ui| {
        ui.label("Search:");
        let mut dir = st.direction.clone();
        if common::combo(ui, ("fr-dir", uid), &mut dir, &["all", "down", "up"], 80.0) {
            st.direction = dir;
        }
    });
    egui::Grid::new(("fr-opts", uid)).num_columns(2).spacing([24.0, 2.0]).show(ui, |ui| {
        ui.checkbox(&mut st.case, "Match case");
        ui.add_enabled(!st.wildcards, egui::Checkbox::new(&mut st.prefix, "Match prefix"));
        ui.end_row();
        ui.add_enabled(!st.wildcards, egui::Checkbox::new(&mut st.whole, "Find whole words only"));
        ui.add_enabled(!st.wildcards, egui::Checkbox::new(&mut st.suffix, "Match suffix"));
        ui.end_row();
        ui.checkbox(&mut st.wildcards, "Use wildcards");
        ui.end_row();
    });
    if st.wildcards {
        muted(
            ui,
            app,
            "Wildcards: ? one character, * any run, [abc] [a-z] [!x] one of, < > word start / end, @ one or more, {n,m} a count, \\ before a wildcard character finds it. Wildcard searches match case.",
        );
    }
    ui.horizontal(|ui| {
        let target = if st.in_replace && st.replace_mode { "Replace with" } else { "Find what" };
        ui.label(format!("For {target}:"));
        ui.menu_button("Format", |ui| {
            let fmt = if st.in_replace && st.replace_mode { &mut st.replace_fmt } else { &mut st.find_fmt };
            for (name, t) in [
                ("Bold", &mut fmt.bold),
                ("Italic", &mut fmt.italic),
                ("Underline", &mut fmt.underline),
                ("Highlight", &mut fmt.highlight),
            ] {
                if ui.button(t.label(name)).clicked() {
                    *t = t.next();
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Font:");
                ui.add(egui::TextEdit::singleline(&mut fmt.font).desired_width(110.0));
            });
            ui.horizontal(|ui| {
                ui.label("Size:");
                ui.add(egui::TextEdit::singleline(&mut fmt.size).desired_width(40.0));
            });
        });
        ui.menu_button("Special", |ui| {
            let replacing = st.in_replace && st.replace_mode;
            let list: &[(&str, &str)] = if replacing { &REPLACE_SPECIALS } else { &FIND_SPECIALS };
            for (code, label) in list {
                if ui.button(*label).clicked() {
                    if replacing {
                        st.replace.push_str(code);
                    } else {
                        st.find.push_str(code);
                    }
                    ui.close();
                }
            }
        });
        let empty =
            if st.in_replace && st.replace_mode { st.replace_fmt.is_empty() } else { st.find_fmt.is_empty() };
        if btn_en(ui, !empty, "No Formatting") {
            if st.in_replace && st.replace_mode {
                st.replace_fmt = Fmt::default();
            } else {
                st.find_fmt = Fmt::default();
            }
        }
    });
}

/// Runs a find / replace command and turns its answer into the dialog's message.
fn run(app: &mut App, id: &str, p: Map<String, Value>, ok: impl FnOnce(&Value) -> String) -> String {
    match app.run(id, Value::Object(p)) {
        Some(v) => ok(&v),
        None => last_error(app),
    }
}

fn buttons(ui: &mut Ui, app: &mut App, st: &mut FindState, compose: Option<u64>) -> Out {
    if btn(ui, "Cancel") {
        return Out::Close;
    }
    let has = !st.find.is_empty();
    if btn_en(ui, has, "Find Next") {
        let mut p = st.params(compose);
        p.insert("text".into(), json!(st.find));
        st.msg = run(app, "format.find", p, |v| {
            if v.get("found").and_then(Value::as_bool) == Some(false) {
                "No more matches.".into()
            } else {
                String::new()
            }
        });
    }
    if st.replace_mode {
        if btn_en(ui, has, "Replace All") {
            let mut p = st.params(compose);
            p.insert("find".into(), json!(st.find));
            p.insert("replace".into(), json!(st.replace));
            p.insert("all".into(), json!(true));
            st.msg = run(app, "format.replace", p, |v| {
                format!("Replaced {} occurrence(s).", v.get("replaced").and_then(Value::as_u64).unwrap_or(0))
            });
        }
        if btn_en(ui, has, "Replace") {
            let mut p = st.params(compose);
            p.insert("find".into(), json!(st.find));
            p.insert("replace".into(), json!(st.replace));
            st.msg = run(app, "format.replace", p, |v| {
                if v.get("found").and_then(Value::as_bool) == Some(false) {
                    "No more matches.".into()
                } else {
                    String::new()
                }
            });
        }
    } else {
        ui.add_enabled_ui(has, |ui| {
            ui.menu_button("Find In", |ui| {
                for (scope, label) in [("main", "Main Document"), ("selection", "Current Selection")] {
                    if ui.button(label).clicked() {
                        let mut p = st.params(compose);
                        p.insert("text".into(), json!(st.find));
                        p.insert("findIn".into(), json!(scope));
                        st.msg = run(app, "format.find", p, |v| {
                            format!(
                                "{} item(s) found and selected.",
                                v.get("count").and_then(Value::as_u64).unwrap_or(0)
                            )
                        });
                        ui.close();
                    }
                }
            });
            ui.menu_button("Reading Highlight", |ui| {
                if ui.button("Highlight All").clicked() {
                    let mut p = st.params(compose);
                    p.insert("text".into(), json!(st.find));
                    p.insert("highlightAll".into(), json!(true));
                    st.msg = run(app, "format.find", p, |v| {
                        format!(
                            "{} item(s) highlighted.",
                            v.get("highlighted").and_then(Value::as_u64).unwrap_or(0)
                        )
                    });
                    ui.close();
                }
                if ui.button("Clear Highlighting").clicked() {
                    let p = json!({"compose": compose, "highlightAll": false});
                    st.msg = run(app, "format.find", p.as_object().cloned().unwrap_or_default(), |_| {
                        String::new()
                    });
                    ui.close();
                }
            });
        });
    }
    if btn(ui, if st.more { "<< Less" } else { "More >>" }) {
        st.more = !st.more;
    }
    Out::Keep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_carry_the_more_options() {
        let mut st = FindState { direction: "up".into(), whole: true, ..Default::default() };
        let p = st.params(Some(3));
        assert!(p.get("wholeWord").is_none(), "Less hides the More options");
        st.more = true;
        st.find_fmt.bold = Tri::On;
        st.replace_fmt.italic = Tri::Off;
        let p = st.params(Some(3));
        assert_eq!(p["wholeWord"], true);
        assert_eq!(p["direction"], "up");
        assert_eq!(p["format"], json!({"bold": true}));
        assert_eq!(p["replaceFormat"], json!({"italic": false}));
        // Wildcards switch off the word options.
        st.wildcards = true;
        assert_eq!(st.params(None)["wholeWord"], false);
        assert_eq!(Tri::Any.next().next().next(), Tri::Any);
        assert_eq!(st.find_fmt.summary(), "Bold");
    }
}
