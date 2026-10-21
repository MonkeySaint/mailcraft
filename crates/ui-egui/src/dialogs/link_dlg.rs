//! Insert Hyperlink: link to an existing file or web page (with Browse), a place in this message (top,
//! headings, bookmarks) or an e-mail address with a subject; the text to display and a ScreenTip.

use egui::{Context, RichText};
use serde_json::{Map, Value, json};

use super::common::{self, Dlg, Out, browse_open, btn, footer, muted, primary, window, with_state};
use super::compose_dlg::cmd;
use crate::App;

/// Link to: the kinds of target.
const MODES: [(&str, &str); 3] =
    [("web", "Web page or file"), ("place", "A place in this message"), ("email", "E-mail address")];

#[derive(Clone, Default)]
struct LinkState {
    mode: String,
    text: String,
    url: String,
    /// A file picked with Browse: OK attaches it (a link to a file on this computer would show the
    /// sender's folders and open nothing for the people the message goes to).
    file: String,
    /// A place in this message: "top", "h:N" (heading N) or "b:name" (bookmark).
    place: String,
    email: String,
    subject: String,
    tip: String,
    show_tip: bool,
    error: String,
}

impl LinkState {
    /// The `insert.link` params for OK.
    fn params(&self, compose: Option<u64>) -> Result<Value, String> {
        let mut p = Map::new();
        p.insert("compose".into(), json!(compose));
        if !self.text.trim().is_empty() {
            p.insert("text".into(), json!(self.text));
        }
        p.insert("screenTip".into(), json!(self.tip.trim()));
        match self.mode.as_str() {
            "place" => {
                if self.place == "top" {
                    p.insert("top".into(), json!(true));
                } else if let Some(n) = self.place.strip_prefix("h:").and_then(|n| n.parse::<u64>().ok()) {
                    p.insert("heading".into(), json!(n));
                } else if let Some(b) = self.place.strip_prefix("b:") {
                    p.insert("bookmark".into(), json!(b));
                } else {
                    return Err("Select a place in this message.".into());
                }
            }
            "email" => {
                if self.email.trim().is_empty() {
                    return Err("Type an e-mail address.".into());
                }
                p.insert("email".into(), json!(self.email.trim()));
                p.insert("subject".into(), json!(self.subject.trim()));
            }
            _ if !self.file.trim().is_empty() => {
                return Ok(json!({"compose": compose, "path": self.file.trim()}));
            }
            _ => {
                if self.url.trim().is_empty() {
                    return Err("Type an address.".into());
                }
                p.insert("url".into(), json!(self.url.trim()));
            }
        }
        Ok(Value::Object(p))
    }
}

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    let bookmarks: Vec<String> =
        d.arr("bookmarks").iter().filter_map(Value::as_str).map(str::to_owned).collect();
    let headings: Vec<(u64, u64, String)> = d
        .arr("headings")
        .iter()
        .filter_map(|h| {
            Some((
                h.get("index")?.as_u64()?,
                h.get("level").and_then(Value::as_u64).unwrap_or(1),
                h.get("text").and_then(Value::as_str).unwrap_or_default().to_owned(),
            ))
        })
        .collect();
    let init = |_: &App| {
        let url = d.str("url");
        let mode = match d.str("mode").as_str() {
            "email" | "place" => d.str("mode"),
            _ => "web".into(),
        };
        let place = url
            .strip_prefix('#')
            .map(|b| if b == "_top" { "top".to_owned() } else { format!("b:{b}") })
            .unwrap_or_default();
        let tip = d.str("screenTip");
        LinkState {
            mode,
            text: d.str("text"),
            url: if url.starts_with('#') || url.starts_with("mailto:") { String::new() } else { url },
            place,
            email: d.str("email"),
            subject: d.str("subject"),
            show_tip: !tip.is_empty(),
            tip,
            file: String::new(),
            error: String::new(),
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Insert Hyperlink", 620.0, |ui, app| {
            ui.horizontal(|ui| {
                ui.label("Text to display:");
                ui.add(egui::TextEdit::singleline(&mut st.text).desired_width(330.0));
                if btn(ui, "ScreenTip...") {
                    st.show_tip = !st.show_tip;
                }
            });
            if st.show_tip {
                ui.horizontal(|ui| {
                    ui.label("ScreenTip text:");
                    ui.add(egui::TextEdit::singleline(&mut st.tip).desired_width(330.0).char_limit(255));
                });
            }
            ui.separator();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(150.0);
                    ui.label("Link to:");
                    for (key, label) in MODES {
                        if ui.selectable_label(st.mode == key, label).clicked() {
                            st.mode = key.to_owned();
                        }
                    }
                });
                ui.separator();
                ui.vertical(|ui| match st.mode.as_str() {
                    "place" => {
                        ui.label("Select a place in this message:");
                        egui::ScrollArea::vertical().id_salt(("link-place", uid)).max_height(200.0).show(ui, |ui| {
                            if ui.selectable_label(st.place == "top", "Top of the message").clicked() {
                                st.place = "top".into();
                            }
                            ui.label(RichText::new("Headings").strong());
                            if headings.is_empty() {
                                muted(ui, app, "No headings.");
                            }
                            for (i, level, text) in &headings {
                                let key = format!("h:{i}");
                                let indent = "    ".repeat(usize::try_from(level.saturating_sub(1)).unwrap_or(0).min(5));
                                let label = if text.trim().is_empty() { "(empty heading)" } else { text.as_str() };
                                if ui.selectable_label(st.place == key, format!("{indent}{label}")).clicked() {
                                    st.place = key;
                                }
                            }
                            ui.label(RichText::new("Bookmarks").strong());
                            if bookmarks.is_empty() {
                                muted(ui, app, "No bookmarks. Insert › Bookmark adds one.");
                            }
                            for b in &bookmarks {
                                let key = format!("b:{b}");
                                if ui.selectable_label(st.place == key, b).clicked() {
                                    st.place = key;
                                }
                            }
                        });
                    }
                    "email" => {
                        egui::Grid::new(("link-email", uid)).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                            ui.label("E-mail address:");
                            ui.add(egui::TextEdit::singleline(&mut st.email).hint_text("name@example.com").desired_width(260.0));
                            ui.end_row();
                            ui.label("Subject:");
                            ui.add(egui::TextEdit::singleline(&mut st.subject).desired_width(260.0));
                            ui.end_row();
                        });
                    }
                    _ => {
                        ui.horizontal(|ui| {
                            ui.label("Address:");
                            let r = ui.add(egui::TextEdit::singleline(&mut st.url).hint_text("https://").desired_width(280.0));
                            if r.changed() {
                                st.file.clear();
                            }
                        });
                        ui.horizontal(|ui| {
                            if common::browse_button(ui, app)
                                && let Some(path) = browse_open(app, "Attach a File", &[], &st.file)
                            {
                                st.url.clear();
                                st.file = path;
                            }
                            if !st.file.is_empty() {
                                let name = std::path::Path::new(&st.file)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| st.file.clone());
                                ui.label(format!("To attach: {name}"));
                            }
                        });
                        muted(
                            ui,
                            app,
                            "A file picked with Browse is attached to the message, so the people it goes to can open it.",
                        );
                    }
                });
            });
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            let mut res = Out::Keep;
            let mut go = false;
            let had_link = !d.str("url").is_empty();
            let mut remove = false;
            footer(ui, |ui| {
                if btn(ui, "Cancel") {
                    res = Out::Close;
                }
                if had_link && btn(ui, "Remove Link") {
                    remove = true;
                }
                go = primary(ui, app, true, "OK");
            });
            if remove {
                return match cmd(app, "insert.link", json!({"compose": compose, "url": ""})) {
                    Ok(_) => Out::Close,
                    Err(e) => {
                        st.error = e;
                        Out::Keep
                    }
                };
            }
            if go && matches!(res, Out::Keep) {
                res = match st.params(compose).and_then(|p| cmd(app, "insert.link", p)) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_params_per_mode() {
        let mut st = LinkState {
            mode: "web".into(),
            url: "example.com".into(),
            text: "site".into(),
            tip: " Tip ".into(),
            ..Default::default()
        };
        let p = st.params(Some(1)).unwrap();
        assert_eq!(
            (p["url"].as_str(), p["text"].as_str(), p["screenTip"].as_str()),
            (Some("example.com"), Some("site"), Some("Tip"))
        );
        st.mode = "place".into();
        assert!(st.params(None).is_err());
        st.place = "h:2".into();
        assert_eq!(st.params(None).unwrap()["heading"], 2);
        st.place = "b:Notes".into();
        assert_eq!(st.params(None).unwrap()["bookmark"], "Notes");
        st.place = "top".into();
        assert_eq!(st.params(None).unwrap()["top"], true);
        st.mode = "email".into();
        assert!(st.params(None).is_err());
        st.email = "a@example.com".into();
        st.subject = "Hi".into();
        let p = st.params(None).unwrap();
        assert_eq!((p["email"].as_str(), p["subject"].as_str()), (Some("a@example.com"), Some("Hi")));
        // A browsed file is attached, never linked by a file: address.
        st.mode = "web".into();
        st.file = "/home/someone/report.pdf".into();
        let p = st.params(Some(1)).unwrap();
        assert_eq!(p["path"], "/home/someone/report.pdf");
        assert!(p.get("url").is_none());
    }
}
