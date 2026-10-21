//! Edit Message (read window › Actions › Edit Message): the received message becomes editable in place in
//! its read window. The body is the compose body editor ([`crate::compose::body_editor_in`]) running on a
//! private session over the message as a rich-text document (`mail::body_doc`: the HTML through
//! `html_to_doc`, or the plain text), with a subject box and a small formatting bar (bold, italic,
//! underline, alignment, bullets). Save writes the edits back to the stored message with `mail.editMessage`
//! (the document as HTML through `doc_to_html`, so the formatting stays; plain text for a plain-text
//! message); Discard leaves the message as it was. Unsaved edits are never dropped without asking: closing
//! the window, Close All Items, File › Exit and the main window's close button show the save question; while
//! edits are unsaved the window's ribbon waits; a message moved away while being edited keeps its window.
//!
//! The editor lives in egui temp memory per item; which windows edit is `UiState::editing_messages`.

use std::sync::{Arc, Mutex};

use egui::{Id, RichText, Ui};
use serde_json::{Value, json};

use crate::App;
use crate::compose::{EditorHost, body_editor_in};
use mailcraft_engine::Session;
use mailcraft_engine::compose::{ComposeKind, ComposeState};
use mailcraft_model::{BodyFormat, Doc, ItemId, Mailbox};

/// The bar above a message being edited.
pub const EDITING_NOTE: &str = "You are editing this message. Save keeps your changes in your mailbox.";
/// Asked when a window closes with unsaved edits.
pub const SAVE_PROMPT: &str = "Do you want to save the changes to this message?";
/// Said when a message being edited was deleted for good elsewhere, so its edits could not be kept.
pub const GONE_NOTE: &str = "The message you were editing was deleted, so its changes could not be kept.";
/// Hover text of a read window's ribbon while its message has unsaved edits.
pub const RIBBON_WAITS_NOTE: &str = "Save or discard your changes to the message first.";
/// Said when MailCraft is asked to close while a message has unsaved edits.
pub const EXIT_WAITS_NOTE: &str =
    "A message you are editing has unsaved changes. Save or discard them first.";
/// Label of the subject box.
pub const SUBJECT_LABEL: &str = "Subject";
/// Compose ids of the private editor sessions start here (the app's own compose ids and the signature
/// editor's stay far below), so the body editors' egui state never mixes.
const EDITOR_ID_BASE: u64 = 1 << 52;

/// The private session one message is edited in.
pub(crate) struct MsgEditor {
    session: Session,
    cid: u64,
    theme: crate::theme::Theme,
    item: u64,
    subject: String,
    original_subject: String,
    /// A plain-text message: saved as text, no formatting bar.
    plain: bool,
    /// The window asked to close with unsaved edits: the save question shows.
    asking: bool,
}

impl MsgEditor {
    fn new(app: &App, item: u64) -> Option<MsgEditor> {
        let m = app.s.mailbox.mail.get(&ItemId(item))?;
        let plain = m.body.format == BodyFormat::PlainText || m.body.html.is_none();
        let body: Doc = mailcraft_engine::cmd::mail::body_doc(m);
        let mut session = Session::new(Mailbox::new_local("Edit Message"));
        session.next_compose = EDITOR_ID_BASE.saturating_add(item.min(EDITOR_ID_BASE - 1));
        let c = ComposeState {
            kind: ComposeKind::New,
            format: if plain { BodyFormat::PlainText } else { BodyFormat::Html },
            body,
            ..ComposeState::default()
        };
        let cid = session.open_compose(c);
        session.notices.clear();
        if let Some(c) = session.composes.get_mut(&cid) {
            c.ensure_body();
            let start = c.body_start();
            c.set_caret(start);
            c.dirty = false;
        }
        Some(MsgEditor {
            session,
            cid,
            theme: app.theme.clone(),
            item,
            subject: m.subject.clone(),
            original_subject: m.subject.clone(),
            plain,
            asking: false,
        })
    }

    fn compose(&self) -> Option<&ComposeState> {
        self.session.composes.get(&self.cid)
    }

    fn body_dirty(&self) -> bool {
        self.compose().is_some_and(|c| c.dirty)
    }

    /// Unsaved edits (body or subject).
    pub(crate) fn dirty(&self) -> bool {
        self.body_dirty() || self.subject != self.original_subject
    }

    /// The `mail.editMessage` params for the edits; `None` when nothing changed.
    pub(crate) fn save_params(&self) -> Option<Value> {
        let mut p = serde_json::Map::new();
        p.insert("ids".into(), json!([self.item]));
        if self.subject != self.original_subject {
            p.insert("subject".into(), json!(self.subject));
        }
        if self.body_dirty() {
            let c = self.compose()?;
            if self.plain {
                p.insert("text".into(), json!(c.body.to_plain_text()));
            } else {
                p.insert("html".into(), json!(mailcraft_html::doc_to_html(&c.body)));
            }
        }
        (p.len() > 1).then_some(Value::Object(p))
    }
}

impl EditorHost for MsgEditor {
    fn compose(&self, cid: u64) -> Option<&ComposeState> {
        self.session.composes.get(&cid)
    }
    fn run(&mut self, id: &str, params: Value) -> Option<Value> {
        let r = self.session.run(id, &params).ok();
        self.session.notices.clear();
        r
    }
    fn theme(&self) -> &crate::theme::Theme {
        &self.theme
    }
}

type Shared = Arc<Mutex<MsgEditor>>;

fn state_id(item: u64) -> Id {
    Id::new(("mailcraft.edit-message", item))
}

fn shared(ctx: &egui::Context, item: u64) -> Option<Shared> {
    ctx.data(|d| d.get_temp::<Shared>(state_id(item)))
}

/// Whether the read window of `item` edits its message.
pub fn is_editing(app: &App, item: u64) -> bool {
    app.ui.editing_messages.contains(&item)
}

/// Starts editing `item` in its read window (opening the window when needed).
pub fn start(app: &mut App, item: u64) {
    if !app.s.mailbox.mail.contains_key(&ItemId(item)) {
        return;
    }
    if !app.ui.read_windows.contains(&item) {
        app.ui.read_windows.push(item);
    }
    if !app.ui.editing_messages.contains(&item) {
        app.ui.editing_messages.push(item);
    }
}

/// Stops editing `item` (its edits are dropped).
pub fn stop(ctx: &egui::Context, app: &mut App, item: u64) {
    app.ui.editing_messages.retain(|i| *i != item);
    ctx.data_mut(|d| d.remove::<Shared>(state_id(item)));
}

/// Whether the message being edited in `item`'s window has unsaved edits.
pub fn has_unsaved_edits(ctx: &egui::Context, item: u64) -> bool {
    shared(ctx, item).is_some_and(|s| s.lock().is_ok_and(|e| e.dirty()))
}

/// Saves the edits of `item` (`mail.editMessage`) and stops editing. False when saving failed (the editor
/// stays, the error was reported).
pub fn save(ctx: &egui::Context, app: &mut App, item: u64) -> bool {
    let params = shared(ctx, item).and_then(|s| s.lock().ok().and_then(|e| e.save_params()));
    if let Some(p) = params
        && app.run("mail.editMessage", p).is_none()
    {
        return false;
    }
    stop(ctx, app, item);
    true
}

/// The window of `item` asks to close: true when it may close now (not editing, or nothing unsaved);
/// otherwise the save question shows in the window and it stays open.
pub fn may_close(ctx: &egui::Context, app: &mut App, item: u64) -> bool {
    if !is_editing(app, item) {
        return true;
    }
    if !has_unsaved_edits(ctx, item) {
        stop(ctx, app, item);
        return true;
    }
    if let Some(s) = shared(ctx, item)
        && let Ok(mut e) = s.lock()
    {
        e.asking = true;
    }
    false
}

/// Test hook: types `more` at the end of the subject being edited in `item`'s window.
#[cfg(test)]
pub(crate) fn append_to_subject(ctx: &egui::Context, item: u64, more: &str) {
    if let Some(s) = shared(ctx, item)
        && let Ok(mut e) = s.lock()
    {
        e.subject.push_str(more);
    }
}

/// MailCraft is asked to close (File › Exit, the main window's close button): true when it may close now.
/// Otherwise every message with unsaved edits shows the save question in its window, which is brought to
/// the front, and a note says why MailCraft stayed open. Editors with nothing unsaved just stop.
pub fn may_exit(ctx: &egui::Context, app: &mut App) -> bool {
    let mut waiting = false;
    for item in app.ui.editing_messages.clone() {
        if may_close(ctx, app, item) {
            continue;
        }
        waiting = true;
        ctx.send_viewport_cmd_to(crate::reading::read_viewport_id(item), egui::ViewportCommand::Focus);
    }
    if waiting {
        app.ui.toast = Some((EXIT_WAITS_NOTE.to_owned(), 0.0));
    }
    !waiting
}

/// What the editor asks of its window after a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The save question was answered (saved or not): the window closes.
    pub close: bool,
}

/// Draws the message being edited (in place of the message view).
pub fn editor_ui(ui: &mut Ui, app: &mut App, item: u64) -> Outcome {
    let ctx = ui.ctx().clone();
    let st = match shared(&ctx, item) {
        Some(s) => s,
        None => match MsgEditor::new(app, item) {
            Some(e) => {
                let s: Shared = Arc::new(Mutex::new(e));
                ctx.data_mut(|d| d.insert_temp(state_id(item), s.clone()));
                s
            }
            None => {
                stop(&ctx, app, item);
                return Outcome::default();
            }
        },
    };
    let mut out = Outcome::default();
    // What the bar's buttons asked for (run after the editor's lock is released).
    let mut action: Option<&str> = None;
    {
        let Ok(mut ed) = st.lock() else { return out };
        egui::Frame::new().fill(app.theme.info_bar).inner_margin(egui::Margin::symmetric(12, 6)).show(
            ui,
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ed.asking {
                        ui.label(RichText::new(SAVE_PROMPT).color(app.theme.text));
                        if ui.button("Save").clicked() {
                            action = Some("saveClose");
                        }
                        if ui.button("Don't Save").clicked() {
                            action = Some("discardClose");
                        }
                        if ui.button("Cancel").clicked() {
                            ed.asking = false;
                        }
                    } else {
                        ui.label(RichText::new(EDITING_NOTE).color(app.theme.text));
                        if ui.add_enabled(ed.dirty(), egui::Button::new("Save")).clicked() {
                            action = Some("save");
                        }
                        if ui.button("Discard").clicked() {
                            action = Some("discard");
                        }
                    }
                });
            },
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let l = ui.label(SUBJECT_LABEL);
            let w = (ui.available_width() - 12.0).max(80.0);
            ui.add(egui::TextEdit::singleline(&mut ed.subject).desired_width(w)).labelled_by(l.id);
        });
        ui.add_space(4.0);
        if !ed.plain {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                format_bar(ui, app, &mut *ed);
            });
        }
        ui.separator();
        let cid = ed.cid;
        egui::ScrollArea::vertical().id_salt(("mc-edit-message", item)).auto_shrink([false, false]).show(
            ui,
            |ui| {
                let h = ui.available_height();
                let min_h = if h.is_finite() { h.max(160.0) } else { 240.0 };
                body_editor_in(ui, &mut *ed, cid, min_h);
            },
        );
    }
    match action {
        Some("save") => {
            save(&ctx, app, item);
        }
        Some("discard") => stop(&ctx, app, item),
        Some("saveClose") => out.close = save(&ctx, app, item),
        Some("discardClose") => {
            stop(&ctx, app, item);
            out.close = true;
        }
        _ => {}
    }
    out
}

/// Bold / Italic / Underline, alignment and bullets for a body edited on a private session.
pub(crate) fn format_bar(ui: &mut Ui, app: &mut App, ed: &mut dyn EditorHostBar) {
    let cid = ed.cid();
    let (bold, italic, underline, align, bullets) = match ed.host().compose(cid) {
        Some(c) => (
            c.selection_all(|s, h| s.bold == Some(true) || (h && s.bold != Some(false))),
            c.selection_all(|s, _| s.italic == Some(true)),
            c.selection_all(|s, _| s.underline == Some(true)),
            c.para_at_caret().align,
            matches!(c.list_at_caret(), Some((_, mailcraft_model::rich::ListKind::Bullet))),
        ),
        None => return,
    };
    use mailcraft_model::rich::Align;
    let mut run: Option<(&str, Value)> = None;
    let side = 26.0;
    if crate::widgets::icon_toggle(ui, app, "bold", "Bold", bold, side).clicked() {
        run = Some(("format.bold", json!({})));
    }
    if crate::widgets::icon_toggle(ui, app, "italic", "Italic", italic, side).clicked() {
        run = Some(("format.italic", json!({})));
    }
    if crate::widgets::icon_toggle(ui, app, "underline", "Underline", underline, side).clicked() {
        run = Some(("format.underline", json!({})));
    }
    ui.separator();
    for (icon, label, kind, on) in [
        ("align-left", "Align Left", "left", align == Align::Start),
        ("align-center", "Center", "center", align == Align::Center),
        ("align-right", "Align Right", "right", align == Align::End),
    ] {
        if crate::widgets::icon_toggle(ui, app, icon, label, on, side).clicked() {
            run = Some(("format.align", json!({"kind": kind})));
        }
    }
    ui.separator();
    if crate::widgets::icon_toggle(ui, app, "bullets", "Bullets", bullets, side).clicked() {
        run = Some(("format.bullets", json!({})));
    }
    if let Some((id, mut params)) = run {
        if let Some(o) = params.as_object_mut() {
            o.insert("compose".into(), json!(cid));
        }
        ed.host_mut().run(id, params);
        ui.ctx().memory_mut(|m| m.request_focus(crate::compose::editor_id(cid)));
    }
}

/// A private-session body editor the small formatting bar drives (the message editor here, the signature
/// editor in Signatures and Stationery).
pub(crate) trait EditorHostBar {
    fn cid(&self) -> u64;
    fn host(&self) -> &dyn EditorHost;
    fn host_mut(&mut self) -> &mut dyn EditorHost;
}

impl EditorHostBar for MsgEditor {
    fn cid(&self) -> u64 {
        self.cid
    }
    fn host(&self) -> &dyn EditorHost {
        self
    }
    fn host_mut(&mut self) -> &mut dyn EditorHost {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use mailcraft_engine::sample::sample_session;

    fn html_item(app: &App) -> u64 {
        app.s
            .mailbox
            .mail
            .values()
            .find(|m| m.body.html.as_deref().is_some_and(|h| h.contains("<b>") || h.contains("<strong>")))
            .or_else(|| app.s.mailbox.mail.values().find(|m| m.body.html.is_some()))
            .map(|m| m.id.0)
            .unwrap()
    }

    #[test]
    fn edit_message_opens_the_read_window_editing() {
        let mut app = App::new(sample_session());
        let id = html_item(&app);
        app.run("mail.editMessage", json!({"ids": [id]})).unwrap();
        // The notice lands in the dialogs module, which opens the window in edit mode.
        for n in std::mem::take(&mut app.s.notices) {
            if let mailcraft_engine::Notice::OpenDialog(name, p) = n {
                crate::dialogs::open(&mut app, &name, p);
            }
        }
        assert_eq!(app.ui.read_windows, [id]);
        assert!(is_editing(&app, id));
    }

    #[test]
    fn saving_keeps_the_formatting_and_the_edit() {
        let mut app = App::new(sample_session());
        let id = html_item(&app);
        let mut ed = MsgEditor::new(&app, id).unwrap();
        assert!(!ed.plain);
        assert!(ed.save_params().is_none(), "nothing changed yet");
        let cid = ed.cid;
        // Bold text typed at the start of the body.
        EditorHost::run(&mut ed, "format.bold", json!({"compose": cid}));
        EditorHost::run(&mut ed, "compose.insertText", json!({"compose": cid, "text": "EDITED "}));
        ed.subject.push_str(" (annotated)");
        let p = ed.save_params().unwrap();
        let html = p["html"].as_str().unwrap().to_owned();
        assert!(html.contains("EDITED"), "{html}");
        app.run("mail.editMessage", p).unwrap();
        let m = &app.s.mailbox.mail[&ItemId(id)];
        assert!(m.subject.ends_with("(annotated)"));
        let stored = m.body.html.as_deref().unwrap();
        assert!(stored.contains("EDITED"), "{stored}");
        // The message is still HTML and its formatting survives (bold markup is kept).
        assert!(
            stored.contains("<b>") || stored.contains("<strong>") || stored.contains("font-weight"),
            "{stored}"
        );
        assert!(m.body.text.as_deref().unwrap_or_default().contains("EDITED"));
    }

    #[test]
    fn plain_text_messages_save_as_text() {
        let mut s = sample_session();
        let id = s.mailbox.mail.values().next().map(|m| m.id).unwrap();
        if let Some(m) = s.mailbox.mail.get_mut(&id) {
            m.body.format = BodyFormat::PlainText;
            m.body.html = None;
            m.body.text = Some("line one".into());
        }
        let app = App::new(s);
        let mut ed = MsgEditor::new(&app, id.0).unwrap();
        assert!(ed.plain);
        let cid = ed.cid;
        EditorHost::run(&mut ed, "compose.insertText", json!({"compose": cid, "text": "new "}));
        let p = ed.save_params().unwrap();
        assert!(p.get("html").is_none());
        assert!(p["text"].as_str().unwrap().contains("new line one"), "{p}");
    }

    #[test]
    fn closing_with_unsaved_edits_asks_and_discard_closes() {
        let mut app = App::new(sample_session());
        let id = html_item(&app);
        start(&mut app, id);
        let mut h = Harness::builder().with_size(egui::vec2(900.0, 700.0)).build_ui_state(
            move |ui, app: &mut App| {
                let o = editor_ui(ui, app, id);
                if o.close {
                    app.ui.read_windows.retain(|w| *w != id);
                }
            },
            app,
        );
        h.run_steps(2);
        h.get_by_label(EDITING_NOTE);
        // Unchanged: closing just stops editing.
        let ctx = h.ctx.clone();
        assert!(!has_unsaved_edits(&ctx, id));
        // An edit, then a close request: the question shows instead.
        if let Some(s) = shared(&ctx, id) {
            s.lock().unwrap().subject.push('!');
        }
        let asked = !may_close(&ctx, h.state_mut(), id);
        assert!(asked);
        h.run_steps(2);
        h.get_by_label(SAVE_PROMPT);
        h.get_by_label("Don't Save").click();
        h.run_steps(2);
        assert!(!is_editing(h.state(), id));
        assert!(h.state().ui.read_windows.is_empty());
        assert!(!h.state().s.mailbox.mail[&ItemId(id)].subject.ends_with('!'));
    }
}
