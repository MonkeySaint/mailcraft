//! Define New Multilevel List / Define New List Style: all nine levels, each a number (style, format, the
//! numbers of the levels above) or a bullet, started from a library list, with a preview.

use egui::{Context, RichText};
use serde_json::{Value, json};

use super::common::{self, Dlg, Out, btn, footer, muted, primary, window, with_state};
use super::compose_dlg::cmd;
use crate::App;
use mailcraft_engine::compose::{ListFormat, NumberStyle, multilevel_formats};

/// Levels a multilevel list defines.
const LEVELS: usize = 9;

/// Number styles: CSS key, label.
const STYLES: [(&str, &str); 6] = [
    ("decimal", "1, 2, 3, …"),
    ("decimal-leading-zero", "01, 02, 03, …"),
    ("lower-alpha", "a, b, c, …"),
    ("upper-alpha", "A, B, C, …"),
    ("lower-roman", "i, ii, iii, …"),
    ("upper-roman", "I, II, III, …"),
];

/// Bullet glyphs offered for a bullet level.
const GLYPHS: [&str; 10] = [
    "\u{2022}", "\u{25e6}", "\u{25aa}", "\u{25a1}", "\u{2756}", "\u{27a2}", "\u{2714}", "\u{2605}",
    "\u{2013}", "\u{25ba}",
];

/// Library lists to start from: key, label.
const LIBRARY: [(&str, &str); 9] = [
    ("numbered", "1. a. i."),
    ("numberedParen", "1) a) i)"),
    ("outline", "1. 1.1. 1.1.1."),
    ("outlinePlain", "1 / 1.1 / 1.1.1"),
    ("outlineRoman", "I. A. 1. a)"),
    ("article", "Article I. / Section 1.01"),
    ("chapter", "Chapter 1 / 1.1"),
    ("bullets", "Bullets by level"),
    ("diamonds", "Four small diamonds"),
];

/// One level as the dialog edits it.
#[derive(Clone, Debug, PartialEq)]
struct Level {
    bullet: bool,
    glyph: String,
    /// The number format ("Article I.", "(a)"): its first number stands for the level's number.
    pattern: String,
    /// CSS key of the number style.
    style: String,
    chain: bool,
}

impl Level {
    fn from_json(v: &Value) -> Level {
        if let Some(g) = v.get("glyph").and_then(Value::as_str) {
            return Level {
                bullet: true,
                glyph: g.to_owned(),
                pattern: "1.".into(),
                style: "decimal".into(),
                chain: false,
            };
        }
        Level {
            bullet: false,
            glyph: "\u{2022}".into(),
            pattern: v.get("pattern").and_then(Value::as_str).unwrap_or("1.").to_owned(),
            style: v.get("numberStyle").and_then(Value::as_str).unwrap_or("decimal").to_owned(),
            chain: v.get("chain").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    fn from_format(f: &ListFormat) -> Level {
        match f {
            ListFormat::Bullet { glyph } => Level {
                bullet: true,
                glyph: glyph.clone(),
                pattern: "1.".into(),
                style: "decimal".into(),
                chain: false,
            },
            ListFormat::Number { style, prefix, suffix, chain } => Level {
                bullet: false,
                glyph: "\u{2022}".into(),
                pattern: format!("{prefix}{}{suffix}", style.format(1)),
                style: style.css().to_owned(),
                chain: *chain,
            },
        }
    }

    fn format(&self) -> ListFormat {
        if self.bullet {
            return ListFormat::bullet(&self.glyph);
        }
        let mut f = ListFormat::from_pattern(&self.pattern, NumberStyle::parse(&self.style));
        if let ListFormat::Number { chain, .. } = &mut f {
            *chain = self.chain;
        }
        f
    }

    fn json(&self) -> Value {
        if self.bullet {
            json!({"glyph": self.glyph})
        } else {
            let key = NumberStyle::parse(&self.style).map_or("decimal", NumberStyle::css);
            json!({"pattern": self.pattern, "numberStyle": key, "chain": self.chain})
        }
    }
}

fn library_levels(key: &str) -> Vec<Level> {
    let mut v: Vec<Level> =
        multilevel_formats(key).unwrap_or_default().iter().map(Level::from_format).collect();
    while v.len() < LEVELS {
        v.push(v.last().cloned().unwrap_or_else(|| Level::from_json(&json!({"pattern": "1."}))));
    }
    v.truncate(LEVELS);
    v
}

#[derive(Clone)]
struct State {
    levels: Vec<Level>,
    at: usize,
    name: String,
    start: String,
    error: String,
}

/// The preview lines: each level's first marker, indented, as the list would show it.
fn preview(levels: &[Level]) -> Vec<String> {
    levels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let parents = vec![1u64; i];
            format!("{}{}", "   ".repeat(i), l.format().marker(1, &parents))
        })
        .collect()
}

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    let style_form = d.name == "defineNewListStyle";
    let init = |_: &App| {
        let mut levels: Vec<Level> = d.arr("levels").iter().map(Level::from_json).collect();
        if levels.is_empty() {
            levels = library_levels("numbered");
        }
        while levels.len() < LEVELS {
            levels.push(levels.last().cloned().unwrap_or_else(|| Level::from_json(&json!({}))));
        }
        levels.truncate(LEVELS);
        State { levels, at: 0, name: String::new(), start: String::new(), error: String::new() }
    };
    let title = if style_form { "Define New List Style" } else { "Define New Multilevel List" };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, title, 600.0, |ui, app| {
            if style_form {
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(240.0));
                });
            }
            ui.horizontal(|ui| {
                ui.label("Start from:");
                let labels: Vec<&str> =
                    std::iter::once("(as it is)").chain(LIBRARY.iter().map(|(_, l)| *l)).collect();
                let mut pick = if st.start.is_empty() { "(as it is)".to_owned() } else { st.start.clone() };
                if common::combo(ui, ("ml-start", uid), &mut pick, &labels, 200.0)
                    && let Some((key, label)) = LIBRARY.iter().find(|(_, l)| *l == pick)
                {
                    st.levels = library_levels(key);
                    st.start = (*label).to_owned();
                }
            });
            ui.separator();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(90.0);
                    ui.label("Level:");
                    for i in 0..LEVELS {
                        if ui.selectable_label(st.at == i, format!("{}", i + 1)).clicked() {
                            st.at = i;
                        }
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_width(270.0);
                    let at = st.at.min(LEVELS - 1);
                    if let Some(l) = st.levels.get_mut(at) {
                        level_editor(ui, app, l, uid, at);
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    ui.label("Preview:");
                    for (i, line) in preview(&st.levels).into_iter().enumerate() {
                        let text = RichText::new(format!("{line}  ────")).monospace();
                        ui.label(if i == st.at { text.strong() } else { text });
                    }
                });
            });
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            let mut res = Out::Keep;
            let mut go = false;
            footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    res = Out::Close;
                }
                go = primary(ui, app, true, "OK");
            });
            if go && matches!(res, Out::Keep) {
                if style_form && st.name.trim().is_empty() {
                    st.error = "Type a name for the list style.".into();
                    return Out::Keep;
                }
                let levels: Vec<Value> = st.levels.iter().map(Level::json).collect();
                let mut p = json!({"compose": compose, "levels": levels});
                if style_form && let Some(o) = p.as_object_mut() {
                    o.insert("name".into(), json!(st.name.trim()));
                }
                res = match cmd(app, "format.multilevel", p) {
                    Ok(_) => Out::Close,
                    Err(e) => {
                        st.error = e;
                        Out::Keep
                    }
                };
            }
            res
        })
    })
}

fn level_editor(ui: &mut egui::Ui, app: &App, l: &mut Level, uid: u64, at: usize) {
    ui.label(RichText::new(format!("Level {}", at + 1)).strong());
    let mut kinds: Vec<&str> = STYLES.iter().map(|(_, label)| *label).collect();
    kinds.push("Bullet");
    let mut current = if l.bullet {
        "Bullet".to_owned()
    } else {
        STYLES
            .iter()
            .find(|(k, _)| NumberStyle::parse(k) == NumberStyle::parse(&l.style))
            .map_or("1, 2, 3, …", |(_, label)| label)
            .to_owned()
    };
    ui.label("Number style for this level:");
    if common::combo(ui, ("ml-style", uid, at), &mut current, &kinds, 200.0) {
        if current == "Bullet" {
            l.bullet = true;
        } else if let Some((key, _)) = STYLES.iter().find(|(_, label)| *label == current) {
            l.bullet = false;
            // The number in the format follows the style: "1." becomes "a." for a, b, c.
            let old = NumberStyle::parse(&l.style).unwrap_or_default().format(1);
            let new = NumberStyle::parse(key).unwrap_or_default().format(1);
            if let Some(i) = l.pattern.find(&old) {
                l.pattern.replace_range(i..i + old.len(), &new);
            } else {
                l.pattern = format!("{new}.");
            }
            l.style = (*key).to_owned();
        }
    }
    if l.bullet {
        ui.label("Bullet:");
        ui.horizontal_wrapped(|ui| {
            for g in GLYPHS {
                if ui.selectable_label(l.glyph == g, g).clicked() {
                    l.glyph = g.to_owned();
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Or type a character:");
            ui.add(egui::TextEdit::singleline(&mut l.glyph).desired_width(40.0).char_limit(4));
        });
    } else {
        ui.label("Number format:");
        ui.add(egui::TextEdit::singleline(&mut l.pattern).desired_width(200.0).char_limit(16));
        muted(
            ui,
            app,
            "The number (1, a, A, i or I) stands for this level's number: \"Step 1:\", \"(a)\", \"Article I.\".",
        );
        if at > 0 {
            ui.checkbox(&mut l.chain, "Include the numbers of the levels above (1.1, 1.1.1)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_lists_fill_nine_levels_and_round_trip() {
        for (key, _) in LIBRARY {
            let levels = library_levels(key);
            assert_eq!(levels.len(), LEVELS, "{key}");
            for l in &levels {
                assert_eq!(Level::from_json(&l.json()).format(), l.format(), "{key}");
            }
        }
        let p = preview(&library_levels("outline"));
        assert_eq!(p[0], "1.");
        assert_eq!(p[2].trim(), "1.1.1.");
        let p = preview(&library_levels("outlinePlain"));
        assert_eq!(p[1].trim(), "1.1");
        let p = preview(&library_levels("article"));
        assert_eq!(p[0], "Article I.");
    }
}
