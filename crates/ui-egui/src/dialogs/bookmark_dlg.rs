//! Insert › Bookmark: the message's bookmarks in a list to pick from (sorted by name or by place in the
//! message, hidden ones on request), a name box, and Add, Delete and Go To. Add closes the dialog; Delete and
//! Go To leave it open, as Close does not undo them.

use egui::Context;
use serde_json::{Value, json};

use super::common::{Dlg, Out, btn, btn_en, enter_pressed, footer, muted, primary, window, with_state};
use super::compose_dlg::cmd;
use crate::App;
use mailcraft_engine::cmd::insert::valid_bookmark;

/// One bookmark of the message.
#[derive(Clone, Debug, PartialEq)]
struct Mark {
    name: String,
    hidden: bool,
    /// Place in the message counted from the top (1 = first).
    location: u64,
}

#[derive(Clone, Default)]
struct BookmarkState {
    name: String,
    by_location: bool,
    show_hidden: bool,
    marks: Vec<Mark>,
    error: String,
}

impl BookmarkState {
    /// The bookmarks the list shows, in its order.
    fn shown(&self) -> Vec<&Mark> {
        let mut out: Vec<&Mark> = self.marks.iter().filter(|m| self.show_hidden || !m.hidden).collect();
        if self.by_location {
            out.sort_by_key(|m| m.location);
        } else {
            out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.name.cmp(&b.name)));
        }
        out
    }

    /// The bookmark the name box names, if there is one.
    fn named(&self) -> Option<&Mark> {
        let n = self.name.trim();
        self.marks.iter().find(|m| m.name == n)
    }

    /// Add is offered for a name a bookmark can have (an existing one is moved to the cursor).
    fn can_add(&self) -> bool {
        valid_bookmark(self.name.trim())
    }
}

fn marks_from(d: &Dlg<'_>) -> Vec<Mark> {
    d.arr("entries")
        .iter()
        .filter_map(|e| {
            Some(Mark {
                name: e.get("name")?.as_str()?.to_owned(),
                hidden: e.get("hidden").and_then(Value::as_bool).unwrap_or(false),
                location: e.get("location").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .collect()
}

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let compose = d.u64("compose");
    let init = |_: &App| {
        let marks = marks_from(d);
        BookmarkState { marks, ..Default::default() }
    };
    with_state(ctx, app, uid, init, |st, app| {
        window(ctx, app, d, "Bookmark", 380.0, |ui, app| {
            ui.label("Bookmark name:");
            let name_box =
                ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(f32::INFINITY).char_limit(40));
            if name_box.changed() {
                st.error.clear();
            }
            let picked = st.named().map(|m| m.name.clone());
            let mut go_to_pick = None;
            egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(
                ui,
                |ui| {
                    ui.set_min_height(160.0);
                    egui::ScrollArea::vertical().id_salt(("bookmark-list", uid)).max_height(160.0).show(
                        ui,
                        |ui| {
                            ui.set_width(ui.available_width());
                            let shown: Vec<Mark> = st.shown().into_iter().cloned().collect();
                            if shown.is_empty() {
                                muted(ui, app, "No bookmarks yet. Type a name and click Add.");
                            }
                            for m in shown {
                                let r =
                                    ui.selectable_label(picked.as_deref() == Some(m.name.as_str()), &m.name);
                                if r.clicked() {
                                    st.name = m.name.clone();
                                    st.error.clear();
                                }
                                if r.double_clicked() {
                                    go_to_pick = Some(m.name.clone());
                                }
                            }
                        },
                    );
                },
            );
            ui.horizontal(|ui| {
                ui.label("Sort by:");
                ui.radio_value(&mut st.by_location, false, "Name");
                ui.radio_value(&mut st.by_location, true, "Location");
            });
            ui.checkbox(&mut st.show_hidden, "Hidden bookmarks");
            if !st.error.is_empty() {
                ui.colored_label(app.theme.danger, &st.error);
            }
            let exists = picked.is_some();
            let mut add = false;
            let mut delete = false;
            let mut go_to = go_to_pick.is_some();
            let mut res = Out::Keep;
            footer(ui, |ui| {
                if btn(ui, "Close") {
                    res = Out::Close;
                }
                if btn_en(ui, exists, "Go To") {
                    go_to = true;
                }
                if btn_en(ui, exists, "Delete") {
                    delete = true;
                }
                add = primary(ui, app, st.can_add(), "Add") || (st.can_add() && enter_pressed(ui));
            });
            if !matches!(res, Out::Keep) {
                return res;
            }
            let target = go_to_pick.or(picked).unwrap_or_else(|| st.name.trim().to_owned());
            if add {
                let name = st.name.trim().to_owned();
                return match cmd(
                    app,
                    "insert.bookmark",
                    json!({"compose": compose, "name": name, "action": "add"}),
                ) {
                    Ok(_) => Out::Close,
                    Err(e) => {
                        st.error = e;
                        Out::Keep
                    }
                };
            }
            if delete {
                match cmd(
                    app,
                    "insert.bookmark",
                    json!({"compose": compose, "name": target, "action": "delete"}),
                ) {
                    Ok(_) => {
                        st.marks.retain(|m| m.name != target);
                        st.name.clear();
                    }
                    Err(e) => st.error = e,
                }
            } else if go_to
                && let Err(e) =
                    cmd(app, "insert.bookmark", json!({"compose": compose, "name": target, "action": "goTo"}))
            {
                st.error = e;
            }
            Out::Keep
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(name: &str, location: u64) -> Mark {
        Mark { name: name.into(), hidden: name.starts_with('_'), location }
    }

    #[test]
    fn list_sorts_by_name_or_location_and_hides_hidden_ones() {
        let mut st = BookmarkState {
            marks: vec![mark("_top", 1), mark("Zebra", 2), mark("mango", 3), mark("apple", 4)],
            ..Default::default()
        };
        let names = |st: &BookmarkState| st.shown().iter().map(|m| m.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&st), ["apple", "mango", "Zebra"]);
        st.by_location = true;
        assert_eq!(names(&st), ["Zebra", "mango", "apple"]);
        st.show_hidden = true;
        assert_eq!(names(&st), ["_top", "Zebra", "mango", "apple"]);
        st.name = " mango ".into();
        assert_eq!(st.named().map(|m| m.location), Some(3));
        assert!(st.can_add());
        st.name = "_top".into();
        assert!(st.named().is_some() && !st.can_add(), "hidden names can be picked, not added");
        st.name = "2nd".into();
        assert!(st.named().is_none() && !st.can_add());
    }

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use mailcraft_engine::sample::sample_session;

    fn harness(app: App) -> Harness<'static, App> {
        let mut h = crate::test_harness().with_size(egui::vec2(1280.0, 900.0)).build_ui_state(
            |ui, app: &mut App| {
                let ctx = ui.ctx().clone();
                crate::dialogs::dialogs(&ctx, app);
            },
            app,
        );
        h.run_steps(2);
        h
    }

    fn step(h: &mut Harness<'_, App>) {
        h.run_steps(3);
    }

    fn at(app: &mut App, cid: u64, para: usize, offset: usize) {
        let pos = json!({"path": [para], "offset": offset});
        app.run("compose.select", json!({"compose": cid, "anchor": pos, "focus": pos})).unwrap();
    }

    fn bookmarks(h: &Harness<'_, App>, cid: u64) -> Vec<String> {
        h.state().s.composes.get(&cid).unwrap().bookmarks.iter().map(|b| b.name.clone()).collect()
    }

    /// Release check: the Bookmark dialog was a name box and an action combo; picking from a list, the
    /// buttons, sorting and hidden bookmarks were missing.
    #[test]
    fn bookmark_dialog_picks_from_the_list_and_adds_goes_to_and_deletes() {
        let mut app = App::new(sample_session());
        let cid = app
            .run("mail.newEmail", json!({}))
            .and_then(|v| v.get("compose").and_then(Value::as_u64))
            .unwrap();
        app.run("compose.insertText", json!({"compose": cid, "text": "One\nTwo\nThree"})).unwrap();
        at(&mut app, cid, 2, 0);
        app.run("insert.bookmark", json!({"compose": cid, "name": "alpha"})).unwrap();
        at(&mut app, cid, 1, 0);
        app.run("insert.bookmark", json!({"compose": cid, "name": "beta"})).unwrap();
        app.run("insert.link", json!({"compose": cid, "top": true, "text": "up"})).unwrap();
        at(&mut app, cid, 0, 1);
        let mut h = harness(app);
        h.state_mut().run("insert.bookmark", json!({"compose": cid}));
        step(&mut h);
        assert_eq!(h.state().ui.dialogs.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["bookmark"]);
        // The list, by name; the hidden one shows only with the box ticked.
        assert!(h.query_by_label("alpha").is_some() && h.query_by_label("beta").is_some());
        assert!(h.query_by_label("_top").is_none());
        h.get_by_label("Hidden bookmarks").click();
        step(&mut h);
        assert!(h.query_by_label("_top").is_some());
        h.get_by_label("Location").click();
        step(&mut h);

        // Pick "alpha" in the list and Go To: the cursor moves there, the dialog stays.
        h.get_by_label("alpha").click();
        step(&mut h);
        h.get_by_label("Go To").click();
        step(&mut h);
        let c = h.state().s.composes.get(&cid).unwrap().clone();
        assert_eq!(c.selection.focus.path, vec![2]);
        assert_eq!(h.state().ui.dialogs.len(), 1, "Go To leaves the dialog open");

        // Delete "beta" from the list.
        h.get_by_label("beta").click();
        step(&mut h);
        h.get_by_label("Delete").click();
        step(&mut h);
        assert!(!bookmarks(&h, cid).contains(&"beta".to_owned()));
        assert!(h.query_by_label("beta").is_none(), "gone from the list too");

        // Type a new name and Add: kept at the cursor, and the dialog closes.
        let input = h.query_all_by_role(egui::accesskit::Role::TextInput).next().unwrap();
        input.focus();
        input.type_text("gamma");
        step(&mut h);
        h.get_by_label("Add").click();
        step(&mut h);
        assert!(h.state().ui.dialogs.is_empty());
        assert!(bookmarks(&h, cid).contains(&"gamma".to_owned()));
    }
}
