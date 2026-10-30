//! Automatic Replies (File › Info, Exchange accounts): replies the server sends while you are away, on now
//! or for a time range, with one text for your organization and one for other senders
//! (`file.automaticReplies`).

use egui::Context;
use serde_json::{Value, json};

use super::common::{Dlg, Out, combo_idx, field, heading, muted, ok_cancel, window, with_state};
use crate::App;

/// Window title.
pub const TITLE: &str = "Automatic Replies";

const OUTSIDE: [(&str, &str); 3] = [
    ("none", "Nobody: no reply leaves the organization"),
    ("contacts", "Only people in my contacts"),
    ("everyone", "Everyone who writes"),
];

#[derive(Clone, Default)]
struct State {
    account: Option<u64>,
    /// Off, on, scheduled.
    on: bool,
    scheduled: bool,
    start: String,
    end: String,
    inside: String,
    outside_text: String,
    outside: usize,
    error: Option<String>,
}

impl State {
    fn from_params(d: &Dlg<'_>) -> State {
        let state = d.str("state");
        State {
            account: d.u64("account"),
            on: state == "on" || state == "scheduled",
            scheduled: state == "scheduled",
            start: d.str("start"),
            end: d.str("end"),
            inside: d.str("insideReply"),
            outside_text: d.str("outsideReply"),
            outside: OUTSIDE.iter().position(|(k, _)| *k == d.str("outside")).unwrap_or(0),
            error: None,
        }
    }

    /// The `file.automaticReplies` params that save these settings.
    fn params(&self) -> Value {
        let state = match (self.on, self.scheduled) {
            (false, _) => "off",
            (true, false) => "on",
            (true, true) => "scheduled",
        };
        let mut p = serde_json::Map::new();
        p.insert("state".into(), json!(state));
        p.insert("insideReply".into(), json!(self.inside));
        p.insert("outsideReply".into(), json!(self.outside_text));
        p.insert("outside".into(), json!(OUTSIDE.get(self.outside).map_or("none", |o| o.0)));
        if let Some(a) = self.account {
            p.insert("account".into(), json!(a));
        }
        if self.on && self.scheduled {
            p.insert("start".into(), json!(self.start.trim()));
            p.insert("end".into(), json!(self.end.trim()));
        }
        Value::Object(p)
    }
}

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Option<Out> {
    (d.name == "automaticReplies").then(|| automatic_replies(ctx, app, d))
}

fn automatic_replies(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let first = State::from_params(d);
    with_state(
        ctx,
        app,
        d.uid,
        |_| first,
        |st, app| {
            window(ctx, app, d, TITLE, 460.0, |ui, app| {
                ui.radio_value(&mut st.on, false, "Automatic replies off");
                ui.radio_value(&mut st.on, true, "Reply automatically");
                ui.add_enabled_ui(st.on, |ui| {
                    ui.checkbox(&mut st.scheduled, "Only between these times");
                    ui.add_enabled_ui(st.scheduled, |ui| {
                        field(ui, "From:", &mut st.start, 160.0);
                        field(ui, "Until:", &mut st.end, 160.0);
                        muted(ui, app, "As YYYY-MM-DDTHH:MM, in your time zone.");
                    });
                    heading(ui, app, "Reply to people in my organization");
                    ui.add(
                        egui::TextEdit::multiline(&mut st.inside)
                            .desired_rows(4)
                            .desired_width(f32::INFINITY),
                    );
                    heading(ui, app, "Reply to people outside my organization");
                    let names: Vec<&str> = OUTSIDE.iter().map(|o| o.1).collect();
                    combo_idx(ui, ("mc-oof-outside", d.uid), &mut st.outside, &names, 280.0);
                    ui.add_enabled(
                        st.outside != 0,
                        egui::TextEdit::multiline(&mut st.outside_text)
                            .desired_rows(4)
                            .desired_width(f32::INFINITY),
                    );
                });
                if let Some(e) = &st.error {
                    ui.colored_label(app.theme.danger, e);
                }
                match ok_cancel(ui, app, true) {
                    Some(false) => Out::Close,
                    Some(true) => match app.s.run("file.automaticReplies", &st.params()) {
                        Ok(_) => Out::Close,
                        Err(e) => {
                            st.error = Some(e.to_string());
                            Out::Keep
                        }
                    },
                    None => Out::Keep,
                }
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_become_command_params() {
        let st = State {
            account: Some(3),
            on: true,
            scheduled: true,
            start: " 2026-11-02T08:00 ".into(),
            end: "2026-11-06T18:00".into(),
            inside: "Away.".into(),
            outside_text: "Out.".into(),
            outside: 1,
            error: None,
        };
        let p = st.params();
        assert_eq!(p["state"], "scheduled");
        assert_eq!(p["start"], "2026-11-02T08:00");
        assert_eq!(p["outside"], "contacts");
        assert_eq!(p["account"], 3);
        let off = State { on: false, scheduled: true, ..st };
        let p = off.params();
        assert_eq!(p["state"], "off");
        assert!(p.get("start").is_none());
    }
}
