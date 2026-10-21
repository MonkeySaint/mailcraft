//! Compose-window dialogs (Format Text, Insert, Options and Review tabs): Font, Paragraph, colours, lists,
//! Find / Replace / Go To, pictures, links, tables, symbols, dates, Quick Parts, themes, language, thesaurus,
//! word count, accessibility, read aloud, translate, plus the search Refine picker. Most are small forms built
//! with [`run_form`] (also used by the item dialogs).

use egui::{Context, RichText};
use serde_json::{Map, Value, json};

use super::common::{self, Dlg, Out, btn, btn_en, footer, muted, ok_cancel, primary, window, with_state};
use super::folders::last_error;
use crate::App;

// ---------------------------------------------------------------------------------------------------------
// Generic form

/// A form field.
pub(crate) enum Kind {
    Text,
    Multi,
    Num { min: f64, max: f64 },
    Combo(Vec<String>),
    Check,
    Note(String),
}

pub(crate) struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
}

pub(crate) fn f(key: &'static str, label: &'static str, kind: Kind) -> Field {
    Field { key, label, kind }
}

pub(crate) fn combo(list: &[&str]) -> Kind {
    Kind::Combo(list.iter().map(|s| (*s).to_owned()).collect())
}

pub(crate) struct Form {
    pub title: String,
    pub fields: Vec<Field>,
    pub init: Map<String, Value>,
    pub ok: &'static str,
    pub width: f32,
}

impl Form {
    pub(crate) fn new(title: impl Into<String>, fields: Vec<Field>) -> Form {
        Form { title: title.into(), fields, init: Map::new(), ok: "OK", width: 420.0 }
    }
    pub(crate) fn init(mut self, key: &str, v: Value) -> Form {
        self.init.insert(key.to_owned(), v);
        self
    }
    pub(crate) fn ok(mut self, label: &'static str) -> Form {
        self.ok = label;
        self
    }
    pub(crate) fn width(mut self, w: f32) -> Form {
        self.width = w;
        self
    }
}

#[derive(Clone, Default)]
struct FormState {
    vals: Map<String, Value>,
    error: String,
}

/// Field values of a submitted form.
pub(crate) struct Vals<'a>(pub &'a Map<String, Value>);

impl Vals<'_> {
    pub(crate) fn s(&self, k: &str) -> String {
        self.0.get(k).and_then(Value::as_str).unwrap_or_default().trim().to_owned()
    }
    pub(crate) fn n(&self, k: &str) -> f64 {
        self.0.get(k).and_then(Value::as_f64).unwrap_or(0.0)
    }
    pub(crate) fn b(&self, k: &str) -> bool {
        self.0.get(k).and_then(Value::as_bool).unwrap_or(false)
    }
}

/// Runs a command and maps a failure to its error text.
pub(crate) fn cmd(app: &mut App, id: &str, p: Value) -> Result<Value, String> {
    app.run(id, p).ok_or_else(|| last_error(app))
}

/// Shows `form`; on OK calls `submit` (Ok(None) closes, Ok(Some(out)) returns `out`, Err shows the message).
pub(crate) fn run_form(
    ctx: &Context,
    app: &mut App,
    d: &Dlg<'_>,
    form: Form,
    submit: impl FnOnce(&mut App, &Vals<'_>) -> Result<Option<Out>, String>,
) -> Out {
    let uid = d.uid;
    let init = form.init.clone();
    with_state(
        ctx,
        app,
        uid,
        |_: &App| FormState { vals: init, error: String::new() },
        |st, app| {
            window(ctx, app, d, &form.title, form.width, |ui, app| {
                egui::Grid::new(("form", uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    for fld in &form.fields {
                        let v = st.vals.entry(fld.key.to_owned()).or_insert(Value::Null);
                        match &fld.kind {
                            Kind::Note(text) => {
                                ui.label("");
                                muted(ui, app, text);
                            }
                            Kind::Check => {
                                ui.label("");
                                let mut b = v.as_bool().unwrap_or(false);
                                if ui.checkbox(&mut b, fld.label).changed() {
                                    *v = json!(b);
                                }
                            }
                            Kind::Text | Kind::Multi => {
                                ui.label(fld.label);
                                let mut s = v.as_str().unwrap_or_default().to_owned();
                                let te = if matches!(fld.kind, Kind::Multi) {
                                    egui::TextEdit::multiline(&mut s).desired_rows(4)
                                } else {
                                    egui::TextEdit::singleline(&mut s)
                                };
                                if ui.add(te.desired_width(form.width - 150.0)).changed() {
                                    *v = json!(s);
                                }
                            }
                            Kind::Num { min, max } => {
                                ui.label(fld.label);
                                let mut n = v.as_f64().unwrap_or(*min);
                                if ui.add(egui::DragValue::new(&mut n).range(*min..=*max)).changed()
                                    || v.is_null()
                                {
                                    *v = json!(n);
                                }
                            }
                            Kind::Combo(choices) => {
                                ui.label(fld.label);
                                let mut s = v
                                    .as_str()
                                    .map(str::to_owned)
                                    .or_else(|| choices.first().cloned())
                                    .unwrap_or_default();
                                let list: Vec<&str> = choices.iter().map(String::as_str).collect();
                                if common::combo(ui, ("fc", uid, fld.key), &mut s, &list, form.width - 160.0)
                                    || v.is_null()
                                {
                                    *v = json!(s);
                                }
                            }
                        }
                        ui.end_row();
                    }
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
                    go = primary(ui, app, true, form.ok);
                });
                if go && matches!(res, Out::Keep) {
                    res = match submit(app, &Vals(&st.vals)) {
                        Ok(None) => Out::Close,
                        Ok(Some(o)) => o,
                        Err(e) => {
                            st.error = e;
                            Out::Keep
                        }
                    };
                }
                res
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    let c = json!(d.u64("compose"));
    let with = |extra: Value| -> Value {
        let mut p = extra;
        if let Some(o) = p.as_object_mut() {
            o.insert("compose".into(), c.clone());
        }
        p
    };
    Some(match d.name {
        "font" => font(ctx, app, d),
        "colors" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Colors",
                vec![
                    f("color", "Color (#rrggbb or name):", Kind::Text),
                    f("n", "", Kind::Note("Examples: #1f4e5f, red, automatic".into())),
                ],
            )
            .init("color", json!("#000000")),
            |app, v| {
                let target = match d.str("target").as_str() {
                    "highlight" => "format.highlight",
                    "shading" => "format.shading",
                    _ => "format.color",
                };
                cmd(app, target, with(json!({"color": v.s("color")}))).map(|_| None)
            },
        ),
        "bordersAndShading" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Borders and Shading",
                vec![
                    f(
                        "border",
                        "Border:",
                        combo(&[
                            "bottom", "top", "left", "right", "all", "outside", "inside", "insideH",
                            "insideV", "none",
                        ]),
                    ),
                    f("shading", "Shading color:", Kind::Text),
                    f("n", "", Kind::Note("Leave shading empty to keep it as it is.".into())),
                ],
            )
            .init("border", json!(if d.str("tab") == "shading" { "none" } else { "bottom" })),
            |app, v| {
                if d.str("tab") != "shading" || v.s("border") != "none" {
                    cmd(app, "format.borders", with(json!({"kind": v.s("border")})))?;
                }
                if !v.s("shading").is_empty() {
                    cmd(app, "format.shading", with(json!({"color": v.s("shading")})))?;
                }
                Ok(None)
            },
        ),
        "setNumberingValue" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Set Numbering Value",
                vec![f("start", "Set value to:", Kind::Num { min: 1.0, max: 9999.0 })],
            )
            .init("start", json!(1)),
            |app, v| cmd(app, "format.numbering", with(json!({"start": v.n("start") as u64}))).map(|_| None),
        ),
        "defineNewBullet" => {
            let cur = d
                .get("format")
                .and_then(|f| f.get("glyph"))
                .and_then(Value::as_str)
                .unwrap_or("\u{2022}")
                .to_owned();
            let glyphs: Vec<String> = BULLET_GLYPHS.iter().map(|(g, n)| format!("{g}  {n}")).collect();
            let init = glyphs
                .iter()
                .find(|g| g.starts_with(&cur))
                .cloned()
                .unwrap_or_else(|| glyphs.first().cloned().unwrap_or_default());
            run_form(
                ctx,
                app,
                d,
                Form::new(
                    "Define New Bullet",
                    vec![
                        f("symbol", "Symbol:", Kind::Combo(glyphs)),
                        f("custom", "Or type a character:", Kind::Text),
                    ],
                )
                .init("symbol", json!(init)),
                |app, v| {
                    let custom = v.s("custom");
                    let glyph = if custom.is_empty() {
                        v.s("symbol").split_whitespace().next().unwrap_or("").to_owned()
                    } else {
                        custom
                    };
                    cmd(app, "format.bullets", with(json!({"glyph": glyph}))).map(|_| None)
                },
            )
        }
        "defineNewNumberFormat" => {
            let fmt = d.get("format").cloned().unwrap_or(Value::Null);
            let style = fmt.get("numberStyle").and_then(Value::as_str).unwrap_or("decimal").to_owned();
            let pattern = format!(
                "{}{}{}",
                fmt.get("prefix").and_then(Value::as_str).unwrap_or(""),
                NUMBER_STYLES.iter().find(|(k, _, _)| *k == style).map_or("1", |(_, _, sample)| *sample),
                fmt.get("suffix").and_then(Value::as_str).unwrap_or("."),
            );
            let labels: Vec<String> = NUMBER_STYLES.iter().map(|(_, l, _)| (*l).to_owned()).collect();
            let init =
                NUMBER_STYLES.iter().find(|(k, _, _)| *k == style).map_or("1, 2, 3, …", |(_, l, _)| *l);
            run_form(
                ctx,
                app,
                d,
                Form::new(
                    "Define New Number Format",
                    vec![
                        f("style", "Number style:", Kind::Combo(labels)),
                        f("pattern", "Number format:", Kind::Text),
                        f("n", "", Kind::Note("The number (1, a, A, i or I) stands for each item's number: \"Step 1:\", \"(a)\", \"Article I.\"".into())),
                    ],
                )
                .init("style", json!(init))
                .init("pattern", json!(pattern)),
                |app, v| {
                    let label = v.s("style");
                    let key = NUMBER_STYLES.iter().find(|(_, l, _)| *l == label).map_or("decimal", |(k, _, _)| *k);
                    cmd(app, "format.numbering", with(json!({"pattern": v.s("pattern"), "numberStyle": key}))).map(|_| None)
                },
            )
        }
        "defineNewMultilevelList" | "defineNewListStyle" => {
            let style_form = d.name == "defineNewListStyle";
            let title = if style_form { "Define New List Style" } else { "Define New Multilevel List" };
            let labels: Vec<String> = MULTILEVEL.iter().map(|(_, l)| (*l).to_owned()).collect();
            let mut fields = Vec::new();
            if style_form {
                fields.push(f("name", "Name:", Kind::Text));
            }
            run_form(
                ctx,
                app,
                d,
                Form::new(
                    title,
                    fields.into_iter().chain([
                        f("start", "Start from:", Kind::Combo(labels)),
                        f("l1", "Level 1 format:", Kind::Text),
                        f("l2", "Level 2 format:", Kind::Text),
                        f("l3", "Level 3 format:", Kind::Text),
                        f("chain", "Include the numbers of the levels above (1.1, 1.1.1)", Kind::Check),
                        f("n", "", Kind::Note("Leave the levels empty to use the list chosen above. A format with no number (\u{2022}, \u{2013}) is a bullet.".into())),
                    ]).collect(),
                )
                .width(480.0),
                move |app, v| {
                    let name = v.s("name");
                    if style_form && name.is_empty() {
                        return Err("Type a name for the list style.".to_owned());
                    }
                    let levels: Vec<Value> = ["l1", "l2", "l3"]
                        .iter()
                        .map(|k| v.s(k))
                        .take_while(|t| !t.is_empty())
                        .enumerate()
                        .map(|(i, t)| {
                            let has_number = t.chars().any(|c| c.is_ascii_digit() || matches!(c, 'a' | 'A' | 'i' | 'I'));
                            if has_number {
                                json!({"pattern": t, "chain": i > 0 && v.b("chain")})
                            } else {
                                json!({"glyph": t})
                            }
                        })
                        .collect();
                    let mut p = if levels.is_empty() {
                        let label = v.s("start");
                        let key = MULTILEVEL.iter().find(|(_, l)| *l == label).map_or("numbered", |(k, _)| *k);
                        json!({"style": key})
                    } else {
                        json!({"levels": levels})
                    };
                    if style_form && let Some(o) = p.as_object_mut() {
                        o.insert("name".into(), json!(name));
                    }
                    cmd(app, "format.multilevel", with(p)).map(|_| None)
                },
            )
        }
        "paragraph" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Paragraph",
                vec![
                    f("align", "Alignment:", combo(&["left", "center", "right", "justify"])),
                    f("indent", "Indentation level:", Kind::Num { min: 0.0, max: 8.0 }),
                    f("before", "Spacing before (pt):", Kind::Num { min: 0.0, max: 200.0 }),
                    f("after", "Spacing after (pt):", Kind::Num { min: 0.0, max: 200.0 }),
                    f(
                        "line",
                        "Line spacing (multiple):",
                        combo(&["1.0", "1.15", "1.5", "2.0", "2.5", "3.0"]),
                    ),
                ],
            )
            .init("line", json!("1.0"))
            .init("after", json!(8)),
            |app, v| {
                cmd(app, "format.align", with(json!({"kind": v.s("align")})))?;
                cmd(
                    app,
                    "format.paragraphSpacing",
                    with(json!({"before": v.n("before"), "after": v.n("after")})),
                )?;
                cmd(
                    app,
                    "format.lineSpacing",
                    with(json!({"multiple": v.s("line").parse::<f64>().unwrap_or(1.0)})),
                )?;
                if v.n("indent") > 0.0 {
                    cmd(app, "format.indent", with(json!({"level": v.n("indent") as u64})))?;
                }
                Ok(None)
            },
        ),
        "saveStyleSet" => run_form(
            ctx,
            app,
            d,
            Form::new("Save as a New Style Set", vec![f("name", "File name:", Kind::Text)]),
            |app, v| {
                cmd(app, "format.styleSet", with(json!({"name": v.s("name"), "action": "save"})))
                    .map(|_| None)
            },
        ),
        "applyStyles" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Apply Styles",
                vec![
                    f("name", "Style Name:", {
                        let listed: Vec<String> =
                            d.arr("styles").iter().filter_map(Value::as_str).map(str::to_owned).collect();
                        if listed.is_empty() {
                            Kind::Combo(
                                mailcraft_engine::cmd::format::BUILTIN_STYLES
                                    .iter()
                                    .map(|s| (*s).to_owned())
                                    .collect(),
                            )
                        } else {
                            Kind::Combo(listed)
                        }
                    }),
                    f("create", "Create a new style from the selection with this name", Kind::Check),
                ],
            )
            .ok("Apply"),
            |app, v| {
                let action = if v.b("create") { "create" } else { "apply" };
                cmd(app, "format.style", with(json!({"name": v.s("name"), "action": action}))).map(|_| None)
            },
        ),
        "pasteSpecial" => {
            let html = crate::compose::clipboard_html();
            let text = crate::compose::clipboard_text();
            // Whether the clipboard holds a picture (asked once per dialog: reading it can be slow).
            let probe = egui::Id::new(("mc-paste-special-picture", d.uid));
            let has_picture = match ctx.data(|m| m.get_temp::<bool>(probe)) {
                Some(b) => b,
                None => {
                    let b = crate::compose::clipboard_image().is_some();
                    ctx.data_mut(|m| m.insert_temp(probe, b));
                    b
                }
            };
            let mut kinds: Vec<&str> = Vec::new();
            if has_picture {
                kinds.push("Picture");
            }
            if html.is_some() {
                kinds.push("HTML Format");
            }
            kinds.push("Unformatted Text");
            kinds.push("Unformatted Unicode Text");
            run_form(
                ctx,
                app,
                d,
                Form::new(
                    "Paste Special",
                    vec![
                        f("as", "As:", combo(&kinds)),
                        f(
                            "n",
                            "",
                            Kind::Note(if has_picture {
                                "The clipboard holds a picture.".into()
                            } else if html.is_some() {
                                "The clipboard holds formatted (HTML) content.".into()
                            } else {
                                "The clipboard holds text only.".into()
                            }),
                        ),
                    ],
                ),
                move |app, v| {
                    if v.s("as") == "Picture" {
                        let Some(data) = crate::compose::clipboard_image() else {
                            return Err("The clipboard no longer holds a picture.".to_owned());
                        };
                        let b64 = mailcraft_mime::transfer::encode_base64_flat(&data);
                        return cmd(app, "format.pasteSpecial", with(json!({"as": "image", "image": b64})))
                            .map(|_| None);
                    }
                    let as_html = v.s("as") == "HTML Format";
                    let mut p = json!({"as": if as_html { "html" } else { "text" }});
                    if let Some(o) = p.as_object_mut() {
                        if as_html && let Some(h) = &html {
                            o.insert("html".into(), json!(h));
                        }
                        if let Some(t) = &text {
                            o.insert("text".into(), json!(t));
                        }
                    }
                    cmd(app, "format.pasteSpecial", with(p)).map(|_| None)
                },
            )
        }
        "find" | "replace" => find_replace(ctx, app, d),
        // Find / Select in a received message: handled by the message view (reading pane or read window).
        "messageFind" => {
            if let Some(id) = d.u64("id") {
                let r =
                    crate::reading::Request::Find { text: d.str("text"), match_case: d.bool("matchCase") };
                crate::reading::request(ctx, app, id, r);
            }
            Out::Close
        }
        "messageSelect" => {
            if let Some(id) = d.u64("id") {
                let r = if d.str("what") == "similar" {
                    crate::reading::Request::SelectSimilar
                } else {
                    crate::reading::Request::SelectAll
                };
                crate::reading::request(ctx, app, id, r);
            }
            Out::Close
        }
        "multilevelGallery" => multilevel_gallery(ctx, app, d),
        "insert3dModel" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Insert 3D Model",
                vec![
                    f("path", "File name:", Kind::Text),
                    f(
                        "n",
                        "",
                        Kind::Note("OBJ and STL models are inserted as a picture of the model.".into()),
                    ),
                ],
            )
            .init("path", json!(format!("{}/", std::env::var("HOME").unwrap_or_default())))
            .width(460.0)
            .ok("Insert"),
            |app, v| cmd(app, "insert.model3d", with(json!({"path": v.s("path")}))).map(|_| None),
        ),
        "goTo" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Go To",
                vec![
                    f(
                        "what",
                        "Go to what:",
                        combo(&["start", "end", "page", "paragraph", "table", "bookmark"]),
                    ),
                    f("value", "Enter number or name:", Kind::Text),
                ],
            )
            .ok("Go To"),
            |app, v| {
                let what = v.s("what");
                let target =
                    if what == "start" || what == "end" { what } else { format!("{what}:{}", v.s("value")) };
                cmd(app, "format.goTo", with(json!({"target": target}))).map(|_| None)
            },
        ),
        "insertPicture" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Insert Picture",
                vec![f("path", "File name:", Kind::Text), f("alt", "Alt text:", Kind::Text)],
            )
            .init("path", json!(format!("{}/", std::env::var("HOME").unwrap_or_default())))
            .ok("Insert"),
            |app, v| {
                cmd(app, "insert.picture", with(json!({"path": v.s("path"), "alt": v.s("alt")})))
                    .map(|_| None)
            },
        ),
        "icons" => {
            let names: Vec<String> = d
                .str("available")
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
            run_form(
                ctx,
                app,
                d,
                Form::new("Insert Icons", vec![f("name", "Icon:", Kind::Combo(names))]).ok("Insert"),
                |app, v| cmd(app, "insert.icon", with(json!({"name": v.s("name")}))).map(|_| None),
            )
        }
        "insertChart" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Insert Chart",
                vec![
                    f("kind", "Chart type:", combo(&["column", "bar", "line", "pie"])),
                    f("title", "Title:", Kind::Text),
                    f("data", "Data (label, value per line):", Kind::Multi),
                ],
            )
            .init("kind", json!(if d.str("kind").is_empty() { "column".to_owned() } else { d.str("kind") }))
            .init("data", json!("Q1, 10\nQ2, 14\nQ3, 9")),
            |app, v| {
                let data: Vec<Value> = v
                    .s("data")
                    .lines()
                    .filter_map(|l| {
                        let (a, b) = l.split_once(',')?;
                        Some(json!([a.trim(), b.trim().parse::<f64>().ok()?]))
                    })
                    .collect();
                if data.is_empty() {
                    return Err("Enter at least one \"label, value\" line.".into());
                }
                cmd(
                    app,
                    "insert.chart",
                    with(json!({"kind": v.s("kind"), "title": v.s("title"), "data": data})),
                )
                .map(|_| None)
            },
        ),
        "screenshot" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Screenshot",
                vec![f("n", "", Kind::Note(d.str("message"))), f("path", "Saved screenshot:", Kind::Text)],
            )
            .init("path", json!(format!("{}/", std::env::var("HOME").unwrap_or_default())))
            .width(480.0)
            .ok("Insert"),
            |app, v| cmd(app, "insert.screenshot", with(json!({"path": v.s("path")}))).map(|_| None),
        ),
        "drawTable" | "insertTable" | "quickTables" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Insert Table",
                vec![
                    f("cols", "Number of columns:", Kind::Num { min: 1.0, max: 63.0 }),
                    f("rows", "Number of rows:", Kind::Num { min: 1.0, max: 200.0 }),
                    f(
                        "n",
                        "",
                        Kind::Note(if d.name == "drawTable" {
                            "Drawing tables by hand is not available; this inserts a grid instead.".into()
                        } else {
                            String::new()
                        }),
                    ),
                ],
            )
            .init("cols", json!(3))
            .init("rows", json!(2)),
            |app, v| {
                cmd(
                    app,
                    "insert.table",
                    with(json!({"rows": v.n("rows") as u64, "cols": v.n("cols") as u64})),
                )
                .map(|_| None)
            },
        ),
        "drawTextBox" => run_form(
            ctx,
            app,
            d,
            Form::new("Text Box", vec![f("text", "Text:", Kind::Multi)]).ok("Insert"),
            |app, v| cmd(app, "insert.textBox", with(json!({"text": v.s("text")}))).map(|_| None),
        ),
        "insertLink" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Insert Hyperlink",
                vec![f("text", "Text to display:", Kind::Text), f("url", "Address:", Kind::Text)],
            )
            .init("text", json!(d.str("text")))
            .init("url", json!(d.str("url")))
            .width(480.0),
            |app, v| {
                if v.s("url").is_empty() {
                    return Err("Enter an address.".into());
                }
                cmd(app, "insert.link", with(json!({"url": v.s("url"), "text": v.s("text")}))).map(|_| None)
            },
        ),
        "bookmark" => {
            let marks: Vec<String> =
                d.arr("bookmarks").iter().filter_map(Value::as_str).map(str::to_owned).collect();
            let mut fields = vec![
                f("name", "Bookmark name:", Kind::Text),
                f("action", "Action:", combo(&["add", "goTo", "delete"])),
            ];
            if !marks.is_empty() {
                fields.push(f("n", "", Kind::Note(format!("Existing: {}", marks.join(", ")))));
            }
            run_form(ctx, app, d, Form::new("Bookmark", fields), |app, v| {
                cmd(app, "insert.bookmark", with(json!({"name": v.s("name"), "action": v.s("action")})))
                    .map(|_| None)
            })
        }
        "symbol" => symbol(ctx, app, d),
        "dateTime" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Date and Time",
                vec![f(
                    "format",
                    "Available formats:",
                    combo(&[
                        "M/d/yyyy",
                        "dddd, MMMM dd, yyyy",
                        "MMMM d, yyyy",
                        "yyyy-MM-dd",
                        "d-MMM-yy",
                        "M/d/yyyy h:mm tt",
                        "h:mm tt",
                        "HH:mm",
                        "HH:mm:ss",
                    ]),
                )],
            ),
            |app, v| cmd(app, "insert.dateTime", with(json!({"format": v.s("format")}))).map(|_| None),
        ),
        "quickParts" => {
            let gallery = d.str("gallery");
            let entries: Vec<(String, String, String)> = d
                .arr("entries")
                .iter()
                .filter_map(|e| {
                    let name = e.get("name").and_then(Value::as_str).or_else(|| e.as_str())?.to_owned();
                    let g = e.get("gallery").and_then(Value::as_str).unwrap_or("quickParts").to_owned();
                    let preview = e.get("preview").and_then(Value::as_str).unwrap_or("").to_owned();
                    Some((name, g, preview))
                })
                .collect();
            let labels: Vec<String> =
                entries.iter().map(|(n, g, _)| format!("{n}  ({})", gallery_label(g))).collect();
            let previews: Vec<String> = entries.iter().map(|(n, _, p)| format!("{n}: {p}")).collect();
            let title = if gallery == "autoText" { "AutoText" } else { "Quick Parts" };
            let save_gallery = if gallery.is_empty() { "quickParts".to_owned() } else { gallery.clone() };
            run_form(
                ctx,
                app,
                d,
                Form::new(
                    title,
                    vec![
                        f("entry", "Entry:", Kind::Combo(labels.clone())),
                        f(
                            "n",
                            "",
                            Kind::Note(if previews.is_empty() {
                                "No saved entries yet: select text in the message and save it here.".into()
                            } else {
                                previews.join("\n")
                            }),
                        ),
                        f("delete", "Delete this entry (Building Blocks Organizer)", Kind::Check),
                        f("save", "Save the selection to the gallery under this name instead", Kind::Check),
                        f("newName", "New entry name:", Kind::Text),
                    ],
                )
                .width(480.0),
                move |app, v| {
                    if v.b("save") {
                        return cmd(
                            app,
                            "insert.saveToGallery",
                            with(json!({"gallery": save_gallery, "name": v.s("newName")})),
                        )
                        .map(|_| None);
                    }
                    let pick = v.s("entry");
                    let Some((name, g, _)) =
                        labels.iter().position(|l| *l == pick).and_then(|i| entries.get(i))
                    else {
                        return Err("Choose an entry.".into());
                    };
                    let action = if v.b("delete") { "delete" } else { "insert" };
                    cmd(app, "insert.quickPart", with(json!({"name": name, "gallery": g, "action": action})))
                        .map(|_| None)
                },
            )
        }
        "insertBusinessCard" => {
            let contacts: Vec<String> =
                app.s.mailbox.contacts.values().map(|c| format!("{} [{}]", c.full_name(), c.id.0)).collect();
            run_form(
                ctx,
                app,
                d,
                Form::new("Insert Business Card", vec![f("contact", "Contact:", Kind::Combo(contacts))])
                    .ok("Insert"),
                |app, v| {
                    let id = v
                        .s("contact")
                        .rsplit('[')
                        .next()
                        .unwrap_or_default()
                        .trim_end_matches(']')
                        .to_owned();
                    cmd(app, "insert.businessCard", with(json!({"contactId": id}))).map(|_| None)
                },
            )
        }
        "insertCalendar" => {
            let today = common::today(app);
            run_form(
                ctx,
                app,
                d,
                Form::new(
                    "Send a Calendar via Email",
                    vec![
                        f("calendar", "Calendar:", combo(&["Calendar"])),
                        f("from", "From (YYYY-MM-DD):", Kind::Text),
                        f("to", "To (YYYY-MM-DD):", Kind::Text),
                        f("detail", "Detail:", combo(&["availability", "limited", "full"])),
                    ],
                )
                .init("from", json!(today))
                .init("to", json!(today))
                .ok("Insert"),
                |app, v| {
                    cmd(app, "insert.calendar", with(json!({"calendar": v.s("calendar"), "from": v.s("from"), "to": v.s("to"), "detail": v.s("detail")}))).map(|_| None)
                },
            )
        }
        "insertObject" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Object",
                vec![f("path", "File name:", Kind::Text), f("asText", "Insert the file's text", Kind::Check)],
            )
            .ok("Insert"),
            |app, v| {
                cmd(app, "insert.object", with(json!({"path": v.s("path"), "asText": v.b("asText")})))
                    .map(|_| None)
            },
        ),
        "attachFile" => run_form(
            ctx,
            app,
            d,
            Form::new("Insert File", vec![f("path", "File name:", Kind::Text)])
                .init("path", json!(format!("{}/", std::env::var("HOME").unwrap_or_default())))
                .ok("Insert")
                .width(480.0),
            |app, v| cmd(app, "compose.attachFile", with(json!({"path": v.s("path")}))).map(|_| None),
        ),
        "attachItem" => {
            let items: Vec<String> = app
                .s
                .mailbox
                .items_in(app.s.selection.folder)
                .iter()
                .take(500)
                .map(|m| {
                    format!("{} [{}]", if m.subject.is_empty() { "(no subject)" } else { &m.subject }, m.id.0)
                })
                .collect();
            run_form(
                ctx,
                app,
                d,
                Form::new("Insert Item", vec![f("item", "Item:", Kind::Combo(items))])
                    .ok("Insert")
                    .width(480.0),
                |app, v| {
                    let id =
                        v.s("item").rsplit('[').next().unwrap_or_default().trim_end_matches(']').to_owned();
                    cmd(app, "compose.attachItem", with(json!({"itemId": id}))).map(|_| None)
                },
            )
        }
        "themes" | "customizeTheme" => {
            use mailcraft_engine::compose::{
                COLOR_GALLERY_LEN, THEME_COLOR_SETS, THEME_EFFECTS, THEME_FONT_SETS, THEMES,
            };
            let part = if d.str("part").is_empty() { "all".to_owned() } else { d.str("part") };
            let names: Vec<String> = match part.as_str() {
                "colors" => {
                    THEME_COLOR_SETS.iter().take(COLOR_GALLERY_LEN).map(|(n, _)| (*n).to_owned()).collect()
                }
                "fonts" => THEME_FONT_SETS.iter().map(|(n, _, _)| (*n).to_owned()).collect(),
                "effects" => THEME_EFFECTS.iter().map(|n| (*n).to_owned()).collect(),
                _ => THEMES.iter().map(|t| t.0.to_owned()).collect(),
            };
            let title = match part.as_str() {
                "colors" => "Theme Colors",
                "fonts" => "Theme Fonts",
                "effects" => "Theme Effects",
                _ => "Themes",
            };
            run_form(
                ctx,
                app,
                d,
                Form::new(
                    title,
                    vec![
                        f("name", "Name:", Kind::Combo(names)),
                        f("reset", "Reset to Theme from Template", Kind::Check),
                    ],
                ),
                move |app, v| {
                    let p = if v.b("reset") {
                        json!({"action": "reset"})
                    } else {
                        json!({"name": v.s("name"), "part": part})
                    };
                    cmd(app, "compose.theme", with(p)).map(|_| None)
                },
            )
        }
        "browseThemes" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Browse for Themes",
                vec![
                    f("path", "Theme file:", Kind::Text),
                    f(
                        "n",
                        "",
                        Kind::Note("A theme saved with Save Current Theme (MailCraft theme, .json).".into()),
                    ),
                ],
            )
            .init("path", json!(format!("{}/", std::env::var("HOME").unwrap_or_default())))
            .width(480.0)
            .ok("Open"),
            |app, v| {
                cmd(app, "compose.theme", with(json!({"action": "browse", "path": v.s("path")})))
                    .map(|_| None)
            },
        ),
        "saveTheme" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Save Current Theme",
                vec![f("name", "Theme name:", Kind::Text), f("path", "File name:", Kind::Text)],
            )
            .init("name", json!("Custom Theme"))
            .init("path", json!(format!("{}/Custom Theme.json", std::env::var("HOME").unwrap_or_default())))
            .width(480.0)
            .ok("Save"),
            |app, v| {
                cmd(
                    app,
                    "compose.theme",
                    with(json!({"action": "save", "name": v.s("name"), "path": v.s("path")})),
                )
                .map(|_| None)
            },
        ),
        "customizeColors" => customize_colors(ctx, app, d),
        "customizeFonts" => {
            let fonts: Vec<String> = THEME_FONT_CHOICES.iter().map(|s| (*s).to_owned()).collect();
            run_form(
                ctx,
                app,
                d,
                Form::new(
                    "Create New Theme Fonts",
                    vec![
                        f("heading", "Heading font:", Kind::Combo(fonts.clone())),
                        f("body", "Body font:", Kind::Combo(fonts)),
                        f("name", "Name:", Kind::Text),
                    ],
                )
                .init("heading", json!(d.str("headingFont")))
                .init("body", json!(d.str("bodyFont")))
                .init("name", json!("Custom 1"))
                .ok("Save"),
                |app, v| {
                    cmd(
                        app,
                        "compose.theme",
                        with(json!({"action": "customize", "part": "fonts", "heading": v.s("heading"), "body": v.s("body"), "name": v.s("name")})),
                    )
                    .map(|_| None)
                },
            )
        }
        "pageColor" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Page Color",
                vec![f("color", "Color (#rrggbb or name):", Kind::Text), f("none", "No Color", Kind::Check)],
            ),
            |app, v| {
                let color =
                    if v.b("none") || v.s("color").is_empty() { Value::Null } else { json!(v.s("color")) };
                cmd(app, "compose.pageColor", with(json!({"color": color}))).map(|_| None)
            },
        ),
        "equation" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Equation",
                vec![
                    f("text", "Equation:", Kind::Text),
                    f(
                        "preset",
                        "Or a built-in one:",
                        Kind::Combo(
                            std::iter::once(String::new())
                                .chain(
                                    mailcraft_engine::cmd::insert::EQUATION_PRESETS
                                        .iter()
                                        .map(|(_, n, _)| (*n).to_owned()),
                                )
                                .collect(),
                        ),
                    ),
                ],
            )
            .ok("Insert"),
            |app, v| {
                let p = if v.s("preset").is_empty() {
                    json!({"text": v.s("text")})
                } else {
                    json!({"preset": v.s("preset")})
                };
                cmd(app, "insert.equation", with(p)).map(|_| None)
            },
        ),
        "smartArt" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Choose a SmartArt Graphic",
                vec![
                    f("layout", "Layout:", combo(&["list", "process", "cycle", "hierarchy"])),
                    f("items", "Items (one per line):", Kind::Multi),
                ],
            )
            .ok("Insert"),
            |app, v| {
                let items: Vec<String> = v
                    .s("items")
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_owned)
                    .collect();
                cmd(app, "insert.smartArt", with(json!({"layout": v.s("layout"), "items": items})))
                    .map(|_| None)
            },
        ),
        "language" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Language",
                vec![f(
                    "lang",
                    "Mark selected text as:",
                    combo(&[
                        "en-US", "en-GB", "en-CA", "fr-CA", "fr-FR", "es-ES", "de-DE", "it-IT", "pt-BR",
                        "nl-NL",
                    ]),
                )],
            ),
            |app, v| cmd(app, "review.language", with(json!({"lang": v.s("lang")}))).map(|_| None),
        ),
        "thesaurus" => thesaurus(ctx, app, d),
        "translate" => translate(ctx, app, d),
        "wordCount" => word_count(ctx, app, d),
        "accessibility" => accessibility(ctx, app, d),
        "readAloud" => read_aloud(ctx, app, d),
        "searchRefine" => {
            let kinds: Vec<String> =
                d.arr("kinds").iter().filter_map(Value::as_str).map(str::to_owned).collect();
            run_form(
                ctx,
                app,
                d,
                Form::new(
                    "Refine",
                    vec![
                        f("kind", "Refine by:", Kind::Combo(kinds)),
                        f("value", "Value:", Kind::Text),
                        f(
                            "n",
                            "",
                            Kind::Note(
                                "from, to, cc, subject, body and received take a value; the others toggle."
                                    .into(),
                            ),
                        ),
                    ],
                ),
                |app, v| {
                    let p = if v.s("value").is_empty() {
                        json!({"kind": v.s("kind")})
                    } else {
                        json!({"kind": v.s("kind"), "value": v.s("value")})
                    };
                    cmd(app, "search.refine", p).map(|_| None)
                },
            )
        }
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------------------------------
// Lists, themes, building blocks

/// Bullet glyphs offered by Define New Bullet: (glyph, name).
const BULLET_GLYPHS: [(&str, &str); 12] = [
    ("\u{2022}", "Solid circle"),
    ("\u{25e6}", "Hollow circle"),
    ("\u{25aa}", "Solid square"),
    ("\u{25a1}", "Hollow square"),
    ("\u{2756}", "Four small diamonds"),
    ("\u{25c6}", "Diamond"),
    ("\u{27a2}", "Arrow"),
    ("\u{2192}", "Thin arrow"),
    ("\u{2714}", "Checkmark"),
    ("\u{2605}", "Star"),
    ("\u{2013}", "Dash"),
    ("\u{25ba}", "Pointer"),
];

/// Number styles of Define New Number Format: (engine key, label, sample number).
const NUMBER_STYLES: [(&str, &str, &str); 6] = [
    ("decimal", "1, 2, 3, …", "1"),
    ("decimal-leading-zero", "01, 02, 03, …", "01"),
    ("lower-alpha", "a, b, c, …", "a"),
    ("upper-alpha", "A, B, C, …", "A"),
    ("lower-roman", "i, ii, iii, …", "i"),
    ("upper-roman", "I, II, III, …", "I"),
];

/// Multilevel list definitions: (key, label).
const MULTILEVEL: [(&str, &str); 8] = [
    ("numbered", "1. a. i."),
    ("numberedParen", "1) a) i)"),
    ("outline", "1. 1.1. 1.1.1."),
    ("outlineRoman", "I. A. 1. a)"),
    ("article", "Article I. / Section 1.01"),
    ("chapter", "Chapter 1 / 1.1"),
    ("bullets", "Bullets by level"),
    ("diamonds", "Four small diamonds"),
];

/// Fonts offered by Customize Fonts: the theme font names (shown with bundled substitutes) and the bundled
/// families.
const THEME_FONT_CHOICES: [&str; 22] = [
    "Aptos",
    "Aptos Display",
    "Calibri",
    "Calibri Light",
    "Cambria",
    "Arial",
    "Times New Roman",
    "Georgia",
    "Garamond",
    "Century Gothic",
    "Constantia",
    "Corbel",
    "Candara",
    "Franklin Gothic Book",
    "Trebuchet MS",
    "Verdana",
    "Consolas",
    "Inter",
    "Carlito",
    "Liberation Sans",
    "Liberation Serif",
    "Liberation Mono",
];

/// Display name of a building-block gallery.
fn gallery_label(g: &str) -> &'static str {
    match g {
        "autoText" => "AutoText",
        "textBoxes" => "Text Boxes",
        "quickTables" => "Quick Tables",
        "equations" => "Equations",
        _ => "Quick Parts",
    }
}

#[derive(Clone, Default)]
struct ColorsState {
    colors: Vec<[u8; 3]>,
    name: String,
    error: String,
}

/// Create New Theme Colors: one colour button per theme slot, a sample row, and a name.
fn customize_colors(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    use mailcraft_engine::compose::THEME_COLOR_SLOTS;
    let uid = d.uid;
    let palette: Vec<[u8; 3]> = d
        .arr("palette")
        .iter()
        .filter_map(Value::as_str)
        .filter_map(mailcraft_geom::Color::parse_hex)
        .map(|c| [c.r, c.g, c.b])
        .collect();
    let compose = d.u64("compose");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| ColorsState {
            colors: if palette.len() == 12 { palette.clone() } else { vec![[0, 0, 0]; 12] },
            name: "Custom 1".into(),
            error: String::new(),
        },
        |st, app| {
            window(ctx, app, d, "Create New Theme Colors", 420.0, |ui, app| {
                egui::Grid::new(("theme-colors", uid)).num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                    for (slot, c) in THEME_COLOR_SLOTS.iter().zip(st.colors.iter_mut()) {
                        ui.label(*slot);
                        egui::color_picker::color_edit_button_srgb(ui, c);
                        ui.end_row();
                    }
                    ui.label("Name:");
                    ui.text_edit_singleline(&mut st.name);
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    ui.label("Sample:");
                    for c in st.colors.iter().skip(4).take(6) {
                        let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, 2.0, egui::Color32::from_rgb(c[0], c[1], c[2]));
                    }
                });
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        return Out::Close;
                    }
                    if primary(ui, app, true, "Save") {
                        let colors: Vec<String> = st
                            .colors
                            .iter()
                            .map(|c| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]))
                            .collect();
                        let p = json!({"compose": compose, "action": "customize", "part": "colors", "colors": colors, "name": st.name});
                        return match cmd(app, "compose.theme", p) {
                            Ok(_) => Out::Close,
                            Err(e) => {
                                st.error = e;
                                Out::Keep
                            }
                        };
                    }
                    Out::Keep
                })
            })
        },
    )
}

// ---------------------------------------------------------------------------------------------------------

const FAMILIES: [&str; 6] =
    ["Inter", "Carlito", "Liberation Sans", "Liberation Serif", "Liberation Mono", "Caladea"];
const SIZES: [&str; 16] =
    ["8", "9", "10", "10.5", "11", "12", "14", "16", "18", "20", "22", "24", "28", "36", "48", "72"];

#[derive(Clone, Default)]
struct FontState {
    family: String,
    size: String,
    color: String,
    flags: [bool; 6],
    initial: [bool; 6],
}

const FLAG_CMDS: [(&str, &str); 6] = [
    ("Bold", "format.bold"),
    ("Italic", "format.italic"),
    ("Underline", "format.underline"),
    ("Strikethrough", "format.strike"),
    ("Superscript", "format.sup"),
    ("Subscript", "format.sub"),
];

fn font(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    let init = |_: &App| {
        let s = d.get("style").cloned().unwrap_or(Value::Null);
        let b = |k: &str| s.get(k).and_then(Value::as_bool).unwrap_or(false);
        let flags = [b("bold"), b("italic"), b("underline"), b("strike"), b("superscript"), b("subscript")];
        FontState {
            family: s.get("font_family").and_then(Value::as_str).unwrap_or("Inter").to_owned(),
            size: s
                .get("size_pt")
                .and_then(Value::as_f64)
                .map_or_else(|| "11".to_owned(), |n| format!("{n}")),
            color: String::new(),
            flags,
            initial: flags,
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Font", 460.0, |ui, app| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label("Font:");
                    common::combo(ui, ("ffam", uid), &mut st.family, &FAMILIES, 200.0);
                });
                ui.vertical(|ui| {
                    ui.label("Size:");
                    common::combo(ui, ("fsize", uid), &mut st.size, &SIZES, 70.0);
                });
            });
            ui.horizontal(|ui| {
                ui.label("Font color:");
                ui.add(egui::TextEdit::singleline(&mut st.color).hint_text("Automatic").desired_width(120.0));
            });
            common::heading(ui, app, "Effects");
            egui::Grid::new(("feff", uid)).num_columns(2).show(ui, |ui| {
                for (i, (label, _)) in FLAG_CMDS.iter().enumerate() {
                    if let Some(f) = st.flags.get_mut(i) {
                        ui.checkbox(f, *label);
                    }
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
            common::heading(ui, app, "Preview");
            let mut rt = RichText::new(if st.family.is_empty() { "Inter" } else { &st.family })
                .size(st.size.parse::<f32>().unwrap_or(11.0).clamp(6.0, 40.0));
            if st.flags[0] {
                rt = rt.strong();
            }
            if st.flags[1] {
                rt = rt.italics();
            }
            if st.flags[2] {
                rt = rt.underline();
            }
            if st.flags[3] {
                rt = rt.strikethrough();
            }
            ui.label(rt);
            match ok_cancel(ui, app, true) {
                Some(true) => {
                    let c = json!(compose);
                    app.run("format.font", json!({"compose": c, "family": st.family}));
                    if let Ok(pt) = st.size.parse::<f64>() {
                        app.run("format.fontSize", json!({"compose": c, "pt": pt}));
                    }
                    if !st.color.trim().is_empty() {
                        app.run("format.color", json!({"compose": c, "color": st.color.trim()}));
                    }
                    for (i, (_, id)) in FLAG_CMDS.iter().enumerate() {
                        if st.flags.get(i) != st.initial.get(i) {
                            app.run(id, json!({"compose": c}));
                        }
                    }
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

#[derive(Clone, Default)]
struct FindState {
    find: String,
    replace: String,
    case: bool,
    msg: String,
    replace_mode: bool,
}

fn find_replace(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| FindState { replace_mode: d.name == "replace", ..Default::default() },
        |st, app| {
            window(ctx, app, d, "Find and Replace", 460.0, |ui, app| {
                ui.horizontal(|ui| {
                    if ui.selectable_label(!st.replace_mode, "Find").clicked() {
                        st.replace_mode = false;
                    }
                    if ui.selectable_label(st.replace_mode, "Replace").clicked() {
                        st.replace_mode = true;
                    }
                });
                ui.separator();
                common::field(ui, "Find what:", &mut st.find, 300.0);
                if st.replace_mode {
                    common::field(ui, "Replace with:", &mut st.replace, 290.0);
                }
                ui.checkbox(&mut st.case, "Match case");
                if !st.msg.is_empty() {
                    muted(ui, app, &st.msg);
                }
                let c = json!(compose);
                footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        return Out::Close;
                    }
                    let has = !st.find.is_empty();
                    if btn_en(ui, has, "Find Next") {
                        st.msg = match app
                            .run("format.find", json!({"compose": c, "text": st.find, "matchCase": st.case}))
                        {
                            Some(v) if v.get("found").and_then(Value::as_bool) == Some(false) => {
                                "No more matches.".into()
                            }
                            Some(_) => String::new(),
                            None => last_error(app),
                        };
                    }
                    if st.replace_mode {
                        if btn_en(ui, has, "Replace All") {
                            st.msg = match app.run("format.replace", json!({"compose": c, "find": st.find, "replace": st.replace, "all": true, "matchCase": st.case})) {
                            Some(v) => format!("Replaced {} occurrence(s).", v.get("replaced").and_then(Value::as_u64).unwrap_or(0)),
                            None => last_error(app),
                        };
                        }
                        if btn_en(ui, has, "Replace") {
                            st.msg = match app.run("format.replace", json!({"compose": c, "find": st.find, "replace": st.replace, "matchCase": st.case})) {
                            Some(_) => String::new(),
                            None => last_error(app),
                        };
                        }
                    }
                    Out::Keep
                })
            })
        },
    )
}

#[derive(Clone, Default)]
struct SymState {
    code: String,
}

const SYMBOLS: &str = "©®™§¶†‡•…–—‘’“”«»‹›€£¥¢°±×÷≠≤≥≈∞√∑∏∂∫µπΩαβγδ←→↑↓↔⇒✓✗★☆♥♦♣♠☺☹½¼¾¹²³";

fn symbol(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| SymState::default(),
        |st, app| {
            window(ctx, app, d, "Symbol", 440.0, |ui, app| {
                let mut out = Out::Keep;
                ui.horizontal_wrapped(|ui| {
                    for ch in SYMBOLS.chars() {
                        if ui
                            .add(
                                egui::Button::new(RichText::new(ch.to_string()).size(18.0))
                                    .min_size(egui::vec2(30.0, 30.0)),
                            )
                            .clicked()
                        {
                            app.run("insert.symbol", json!({"compose": compose, "char": ch.to_string()}));
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Character code:");
                    ui.add(egui::TextEdit::singleline(&mut st.code).hint_text("U+00A9").desired_width(100.0));
                    if btn_en(ui, !st.code.trim().is_empty(), "Insert") {
                        app.run("insert.symbol", json!({"compose": compose, "char": st.code.trim()}));
                    }
                });
                if footer(ui, |ui| btn(ui, "Close")) {
                    out = Out::Close;
                }
                out
            })
        },
    )
}

#[derive(Clone, Default)]
struct PickState {
    pick: Option<String>,
}

fn thesaurus(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let word = d.str("word");
    let syns: Vec<String> = d.arr("synonyms").iter().filter_map(Value::as_str).map(str::to_owned).collect();
    with_state(
        ctx,
        app,
        uid,
        |_: &App| PickState::default(),
        |st, app| {
            window(ctx, app, d, "Thesaurus", 360.0, |ui, app| {
                ui.label(RichText::new(&word).strong().size(16.0));
                if syns.is_empty() {
                    muted(ui, app, "No synonyms found.");
                }
                for s in &syns {
                    if ui.selectable_label(st.pick.as_deref() == Some(s.as_str()), s).clicked() {
                        st.pick = Some(s.clone());
                    }
                }
                footer(ui, |ui| {
                    if btn(ui, "Close") {
                        return Out::Close;
                    }
                    if btn_en(ui, st.pick.is_some(), "Insert") {
                        app.run(
                            "review.thesaurus",
                            json!({"compose": d.u64("compose"), "word": word, "replace": st.pick}),
                        );
                        return Out::Close;
                    }
                    Out::Keep
                })
            })
        },
    )
}

fn translate(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let text = d.str("text");
    with_state(
        ctx,
        app,
        uid,
        |_: &App| PickState { pick: Some("en".into()) },
        |st, app| {
            window(ctx, app, d, "Translator", 460.0, |ui, app| {
                ui.label(RichText::new("Text").strong());
                egui::ScrollArea::vertical()
                    .id_salt(("tr", uid))
                    .max_height(140.0)
                    .show(ui, |ui| ui.label(&text));
                let mut lang = st.pick.clone().unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label("Translate to:");
                    common::combo(
                        ui,
                        ("trl", uid),
                        &mut lang,
                        &["en", "fr", "es", "de", "it", "pt", "nl", "ja", "zh"],
                        100.0,
                    );
                });
                st.pick = Some(lang);
                muted(
                    ui,
                    app,
                    "No translation service is connected to MailCraft, so nothing can be translated yet.",
                );
                footer(ui, |ui| if btn(ui, "Close") { Out::Close } else { Out::Keep })
            })
        },
    )
}

/// Multilevel List › Gallery Filter: the gallery's sections the filter shows (Current List, List Library,
/// List Styles); clicking one applies it.
fn multilevel_gallery(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let filter = d.str("filter");
    let title = match filter.as_str() {
        "current" => "Multilevel List: Current List",
        "library" => "Multilevel List: List Library",
        _ => "Multilevel List: All",
    };
    let shown: Vec<String> = d.arr("shown").iter().filter_map(Value::as_str).map(str::to_owned).collect();
    let current = d.get("current").cloned();
    let library = d.arr("library");
    let styles = d.arr("styles");
    let compose = d.u64("compose");
    window(ctx, app, d, title, 360.0, |ui, app| {
        let mut pick: Option<Value> = None;
        if shown.is_empty() {
            muted(ui, app, "The text at the cursor is not in a list.");
        }
        if shown.iter().any(|s| s == "current") {
            common::heading(ui, app, "Current List");
            let sample = current
                .as_ref()
                .and_then(|c| c.get("sample").or_else(|| c.get("glyph")))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            ui.label(format!("{sample}  (the list at the cursor)"));
        }
        for (key, heading, list) in
            [("library", "List Library", &library), ("styles", "List Styles", &styles)]
        {
            if !shown.iter().any(|s| s == key) {
                continue;
            }
            common::heading(ui, app, heading);
            for item in list {
                let label = item.get("label").and_then(Value::as_str).unwrap_or_default();
                let sample = item.get("sample").and_then(Value::as_str).unwrap_or_default();
                let text = if sample.is_empty() { label.to_owned() } else { format!("{sample}  {label}") };
                if ui.button(text).clicked() {
                    pick = item.get("style").cloned();
                }
            }
        }
        if let Some(style) = pick {
            let _ = app.run("format.multilevel", json!({"compose": compose, "style": style}));
            return Out::Close;
        }
        footer(ui, |ui| if btn(ui, "Close") { Out::Close } else { Out::Keep })
    })
}

fn word_count(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    window(ctx, app, d, "Word Count", 320.0, |ui, app| {
        egui::Grid::new(("wc", d.uid)).num_columns(2).spacing([20.0, 4.0]).show(ui, |ui| {
            for (label, key) in [
                ("Words", "words"),
                ("Characters (no spaces)", "charactersNoSpaces"),
                ("Characters (with spaces)", "charactersWithSpaces"),
                ("Paragraphs", "paragraphs"),
                ("Lines", "lines"),
            ] {
                ui.label(label);
                ui.label(d.u64(key).unwrap_or(0).to_string());
                ui.end_row();
            }
        });
        footer(ui, |ui| if primary(ui, app, true, "Close") { Out::Close } else { Out::Keep })
    })
}

fn accessibility(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    window(ctx, app, d, "Accessibility", 420.0, |ui, app| {
        let issues = d.arr("issues");
        if issues.is_empty() {
            ui.label("No accessibility issues found.");
        }
        for (sev, title) in [("error", "Errors"), ("warning", "Warnings"), ("tip", "Tips")] {
            let list: Vec<&Value> =
                issues.iter().filter(|i| i.get("severity").and_then(Value::as_str) == Some(sev)).collect();
            if list.is_empty() {
                continue;
            }
            common::heading(ui, app, title);
            for (n, i) in list.into_iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("•  {}", i.get("message").and_then(Value::as_str).unwrap_or_default()));
                    // Go to: selects the start of the paragraph or table the issue is in.
                    if let (Some(at), Some(cid)) = (i.get("at"), d.u64("compose"))
                        && ui.push_id((sev, n), |ui| ui.link("Go to")).inner.clicked()
                    {
                        app.run("compose.select", json!({"compose": cid, "anchor": at, "focus": at}));
                    }
                });
            }
        }
        footer(ui, |ui| if btn(ui, "Close") { Out::Close } else { Out::Keep })
    })
}

fn read_aloud(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let text = d.str("text");
    window(ctx, app, d, "Read Aloud", 460.0, |ui, app| {
        muted(
            ui,
            app,
            "Speech output is not available in this build; the text that would be read is shown below.",
        );
        egui::ScrollArea::vertical().id_salt(("ra", uid)).max_height(200.0).show(ui, |ui| ui.label(&text));
        footer(ui, |ui| if btn(ui, "Stop") { Out::Close } else { Out::Keep })
    })
}
