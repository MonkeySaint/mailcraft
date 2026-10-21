//! Compose-window dialogs (Format Text, Insert, Options and Review tabs): Font, Paragraph, colours, lists,
//! Find / Replace / Go To, pictures, links, tables, dates, themes, word count, accessibility, translate, plus
//! the search Refine picker. Most are small forms built with [`run_form`] (also used by the item dialogs).
//! The Insert, Options and Review dialogs that need more than a form are in `insert_dlg`.

use egui::{Context, RichText};
use serde_json::{Map, Value, json};

use super::common::{self, Dlg, Out, btn, footer, muted, primary, window, with_state};
use super::folders::last_error;
use crate::App;

/// Send a Calendar: limit the times shown to the working hours of Options › Calendar.
pub const SEND_CALENDAR_WORKING_HOURS: &str = "Working hours only";
/// Send a Calendar: show the details of private items too.
pub const SEND_CALENDAR_PRIVATE: &str = "Show details of private items";

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
        "bordersAndShading" => super::borders_dlg::show(ctx, app, d),
        "setNumberingValue" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Set Numbering Value",
                vec![
                    f("how", "", combo(&["Number from a new start", "Carry on from the list above"])),
                    f("advance", "Skip ahead to the number below", Kind::Check),
                    f("start", "Number:", Kind::Num { min: 1.0, max: 9999.0 }),
                    f(
                        "n",
                        "",
                        Kind::Note(
                            "Carrying on counts this list on from the list above it; Skip ahead lets you pick the number to go on from."
                                .into(),
                        ),
                    ),
                ],
            )
            .init("how", json!("Number from a new start"))
            .init("start", json!(1)),
            |app, v| {
                let p = if v.s("how") == "Carry on from the list above" && !v.b("advance") {
                    json!({"continue": true})
                } else {
                    json!({"start": v.n("start") as u64})
                };
                cmd(app, "format.numbering", with(p)).map(|_| None)
            },
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
        "defineNewMultilevelList" | "defineNewListStyle" => super::multilevel_dlg::show(ctx, app, d),
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
        "find" | "replace" => super::find_replace_dlg::show(ctx, app, d),
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
                        Kind::Note("OBJ, STL, PLY and glTF models are inserted as a picture of the model, which can be turned later.".into()),
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
        "insertTable" => run_form(
            ctx,
            app,
            d,
            Form::new(
                "Insert Table",
                vec![
                    f("cols", "Number of columns:", Kind::Num { min: 1.0, max: 63.0 }),
                    f("rows", "Number of rows:", Kind::Num { min: 1.0, max: 200.0 }),
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
        "insertLink" => super::link_dlg::show(ctx, app, d),
        "sortText" => sort_text(ctx, app, d),
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
        "insertCalendar" => send_calendar(ctx, app, d),
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
        "translate" => translate(ctx, app, d),
        "wordCount" => word_count(ctx, app, d),
        "accessibility" => accessibility(ctx, app, d),
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

/// Sort Text: sort by paragraphs or a field / column, as text, numbers or dates, either way, with a header
/// row kept in place.
fn sort_text(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let table = d.str("target") == "table";
    let fields = d.u64("fields").unwrap_or(1).clamp(1, 63);
    let mut by: Vec<String> = Vec::new();
    if !table {
        by.push("Paragraphs".into());
    }
    for n in 1..=fields {
        by.push(if table { format!("Column {n}") } else { format!("Field {n}") });
    }
    let mut fields_list = vec![
        f("by", "Sort by:", Kind::Combo(by.clone())),
        f("type", "Type:", combo(&["Text", "Number", "Date"])),
        f("order", "Order:", combo(&["Ascending", "Descending"])),
        f("header", "Keep the first row on top (it is a heading)", Kind::Check),
        f("case", "Case sensitive", Kind::Check),
    ];
    if !table {
        fields_list.push(f("separator", "Separate fields at:", combo(&["Tabs", "Commas", "Other"])));
        fields_list.push(f("other", "Other character:", Kind::Text));
    }
    let sep = if d.str("separator") == "comma" { "Commas" } else { "Tabs" };
    let c = json!(d.u64("compose"));
    run_form(
        ctx,
        app,
        d,
        Form::new("Sort Text", fields_list)
            .init("by", json!(by.first().cloned().unwrap_or_default()))
            .init("type", json!("Text"))
            .init("order", json!("Ascending"))
            .init("separator", json!(sep))
            .width(440.0),
        move |app, v| {
            let mut p = json!({
                "compose": c,
                "type": v.s("type").to_lowercase(),
                "order": v.s("order").to_lowercase(),
                "header": v.b("header"),
                "matchCase": v.b("case"),
            });
            let chosen = v.s("by");
            if let Some(o) = p.as_object_mut() {
                let n = chosen.rsplit(' ').next().and_then(|n| n.parse::<u64>().ok());
                match n {
                    Some(n) => {
                        o.insert("by".into(), json!(if table { "column" } else { "field" }));
                        o.insert("field".into(), json!(n));
                    }
                    None => {
                        o.insert("by".into(), json!("paragraphs"));
                    }
                }
                if !table {
                    let separator = match v.s("separator").as_str() {
                        "Commas" => "comma".to_owned(),
                        "Other" => {
                            let other = v.s("other");
                            if other.chars().count() != 1 {
                                return Err("Type one character to separate the fields at.".into());
                            }
                            other
                        }
                        _ => "tab".to_owned(),
                    };
                    o.insert("separator".into(), json!(separator));
                }
            }
            cmd(app, "format.sort", p).map(|_| None)
        },
    )
}

#[derive(Clone, Default)]
struct PickState {
    pick: Option<String>,
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
    // "Include text boxes and shapes" (on by default; remembered while the dialog is open).
    let include_id = egui::Id::new(("mailcraft-wordcount-drawings", d.uid));
    let mut include = ctx.data(|m| m.get_temp::<bool>(include_id)).unwrap_or(true);
    let drawings = d.get("drawings").cloned().unwrap_or(Value::Null);
    let has_drawings = drawings.get("words").and_then(Value::as_u64).unwrap_or(0) > 0
        || drawings.get("charactersWithSpaces").and_then(Value::as_u64).unwrap_or(0) > 0;
    let out = window(ctx, app, d, "Word Count", 320.0, |ui, app| {
        egui::Grid::new(("wc", d.uid)).num_columns(2).spacing([20.0, 4.0]).show(ui, |ui| {
            for (label, key) in [
                ("Words", "words"),
                ("Characters (no spaces)", "charactersNoSpaces"),
                ("Characters (with spaces)", "charactersWithSpaces"),
                ("Paragraphs", "paragraphs"),
                ("Lines", "lines"),
            ] {
                let extra = if include { drawings.get(key).and_then(Value::as_u64).unwrap_or(0) } else { 0 };
                ui.label(label);
                ui.label(d.u64(key).unwrap_or(0).saturating_add(extra).to_string());
                ui.end_row();
            }
        });
        if has_drawings {
            ui.checkbox(&mut include, "Include text boxes and shapes");
        }
        footer(ui, |ui| if primary(ui, app, true, "Close") { Out::Close } else { Out::Keep })
    });
    if out.keeps() {
        ctx.data_mut(|m| m.insert_temp(include_id, include));
    } else {
        ctx.data_mut(|m| m.remove::<bool>(include_id));
    }
    out
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
                    // Go to: selects the start of the paragraph or table the issue is in, or the drawing.
                    if let (Some(at), Some(cid)) = (i.get("at"), d.u64("compose"))
                        && ui.push_id((sev, n), |ui| ui.link("Go to")).inner.clicked()
                    {
                        app.run("compose.select", json!({"compose": cid, "anchor": at, "focus": at}));
                    } else if let (Some(id), Some(cid)) =
                        (i.get("drawing").and_then(Value::as_u64), d.u64("compose"))
                        && ui.push_id((sev, n), |ui| ui.link("Select")).inner.clicked()
                    {
                        app.run("drawing.select", json!({"compose": cid, "id": id}));
                    }
                });
            }
        }
        footer(ui, |ui| if btn(ui, "Close") { Out::Close } else { Out::Keep })
    })
}

// ---------------------------------------------------------------------------------------------------------
// Send a Calendar via Email

/// The Send a Calendar dialog's choices.
#[derive(Clone, Default)]
struct SendCalendar {
    /// Index into the calendars offered.
    calendar: usize,
    /// Index into the date-range presets; one past the end = "Specify dates".
    range: usize,
    from: String,
    to: String,
    detail: usize,
    working_hours: bool,
    include_private: bool,
    layout: usize,
    ics: bool,
    error: String,
}

/// "Specify dates" (the last Date Range choice).
pub(crate) const SPECIFY_DATES: &str = "Specify dates";

fn send_calendar(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    use mailcraft_engine::cmd::insert::{
        CALENDAR_DETAILS, CALENDAR_LAYOUTS, CALENDAR_RANGES, calendar_folders,
    };
    let uid = d.uid;
    let compose = d.u64("compose");
    let calendars = calendar_folders(&app.s);
    let today = common::today(app);
    with_state(
        ctx,
        app,
        uid,
        |_: &App| SendCalendar {
            from: today.clone(),
            to: today.clone(),
            detail: 1,
            ics: true,
            ..Default::default()
        },
        |st, app| {
            window(ctx, app, d, "Send a Calendar via Email", 470.0, |ui, app| {
                muted(ui, app, "Choose the calendar and how much of it to put in the message.");
                let names: Vec<&str> = calendars.iter().map(|(_, n)| n.as_str()).collect();
                let mut ranges: Vec<&str> = CALENDAR_RANGES.iter().map(|(_, l)| *l).collect();
                ranges.push(SPECIFY_DATES);
                let details: Vec<&str> = CALENDAR_DETAILS.iter().map(|(_, l)| *l).collect();
                let layouts: Vec<&str> = CALENDAR_LAYOUTS.iter().map(|(_, l)| *l).collect();
                egui::Grid::new(("sendcal", uid)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("Calendar:");
                    if names.is_empty() {
                        ui.label("(no calendars)");
                    } else {
                        common::combo_idx(ui, ("sc-cal", uid), &mut st.calendar, &names, 260.0);
                    }
                    ui.end_row();
                    ui.label("Date Range:");
                    common::combo_idx(ui, ("sc-range", uid), &mut st.range, &ranges, 180.0);
                    ui.end_row();
                    if st.range >= CALENDAR_RANGES.len() {
                        ui.label("Start:");
                        common::date_box(ui, app, ("sc-from", uid), &mut st.from, "", "Start date");
                        ui.end_row();
                        ui.label("End:");
                        common::date_box(ui, app, ("sc-to", uid), &mut st.to, "", "End date");
                        ui.end_row();
                    }
                    ui.label("Detail:");
                    common::combo_idx(ui, ("sc-detail", uid), &mut st.detail, &details, 180.0);
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut st.working_hours, SEND_CALENDAR_WORKING_HOURS);
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut st.include_private, SEND_CALENDAR_PRIVATE);
                    ui.end_row();
                    ui.label("Email Layout:");
                    common::combo_idx(ui, ("sc-layout", uid), &mut st.layout, &layouts, 180.0);
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut st.ics, "Attach the calendar as an iCalendar (.ics) file");
                    ui.end_row();
                });
                let hint = match CALENDAR_DETAILS.get(st.detail).map(|(id, _)| *id) {
                    Some("availability") => "Shows only when you are free or busy.",
                    Some("full") => "Shows subjects, locations and notes.",
                    _ => "Shows times and subjects.",
                };
                muted(ui, app, hint);
                if !st.error.is_empty() {
                    ui.colored_label(app.theme.danger, &st.error);
                }
                let mut out = Out::Keep;
                let ok = footer(ui, |ui| {
                    if btn(ui, "Cancel") {
                        out = Out::Close;
                    }
                    primary(ui, app, !calendars.is_empty(), "OK")
                });
                if ok && matches!(out, Out::Keep) {
                    let mut p = json!({
                        "detail": CALENDAR_DETAILS.get(st.detail).map_or("limited", |(id, _)| *id),
                        "layout": CALENDAR_LAYOUTS.get(st.layout).map_or("list", |(id, _)| *id),
                        "workingHours": st.working_hours,
                        "includePrivate": st.include_private,
                        "ics": st.ics,
                    });
                    if let Some(o) = p.as_object_mut() {
                        if let Some(c) = compose {
                            o.insert("compose".into(), json!(c));
                        }
                        if let Some((id, _)) = calendars.get(st.calendar) {
                            o.insert("calendar".into(), json!(id.0));
                        }
                        match CALENDAR_RANGES.get(st.range) {
                            Some((id, _)) => {
                                o.insert("range".into(), json!(id));
                            }
                            None => {
                                o.insert("from".into(), json!(st.from.trim()));
                                o.insert("to".into(), json!(st.to.trim()));
                            }
                        }
                    }
                    if app.run("insert.calendar", p).is_some() {
                        out = Out::Close;
                    } else {
                        st.error = last_error(app);
                    }
                }
                out
            })
        },
    )
}
