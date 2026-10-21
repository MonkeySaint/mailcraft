//! The appointment window's notes: the compose body editor ([`crate::compose::body_editor_in`]) on a private
//! session, with the small formatting bar (bold, italic, underline, alignment, bullets), so the notes keep
//! their formatting. Saving sends them as HTML (`bodyHtml`); untouched notes are sent as they came.

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::App;
use crate::compose::{EditorHost, body_editor_in};
use mailcraft_engine::Session;
use mailcraft_engine::compose::{ComposeKind, ComposeState};
use mailcraft_model::{BodyFormat, Doc, Mailbox};

/// Compose ids of the notes editors start here (far from the app's own compose ids and the other private
/// editors), so the body editors' egui state never mixes.
const EDITOR_ID_BASE: u64 = 1 << 50;
/// Height of the notes box.
pub(crate) const NOTES_HEIGHT: f32 = 120.0;

/// The private session one item window's notes are edited in.
pub(crate) struct NotesEditor {
    session: Session,
    cid: u64,
    theme: crate::theme::Theme,
}

/// Shared handle kept in the (cloneable) item window state.
pub(crate) type SharedNotes = Arc<Mutex<NotesEditor>>;

impl NotesEditor {
    /// Notes from `html` (formatted) or `text` (plain) for the window numbered `nonce`.
    pub(crate) fn new(app: &App, nonce: u64, text: &str, html: Option<&str>) -> NotesEditor {
        let mut session = Session::new(Mailbox::new_local("Notes"));
        session.next_compose = EDITOR_ID_BASE.saturating_add((nonce % (1 << 30)).saturating_mul(1 << 8));
        let body = match html.filter(|h| !h.trim().is_empty()) {
            Some(h) => mailcraft_html::html_to_doc(h),
            None => Doc::from_plain_text(text),
        };
        let c = ComposeState {
            kind: ComposeKind::New,
            format: BodyFormat::Html,
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
        NotesEditor { session, cid, theme: app.theme.clone() }
    }

    pub(crate) fn shared(app: &App, nonce: u64, text: &str, html: Option<&str>) -> SharedNotes {
        Arc::new(Mutex::new(NotesEditor::new(app, nonce, text, html)))
    }

    fn compose(&self) -> Option<&ComposeState> {
        self.session.composes.get(&self.cid)
    }

    /// Edited since the window opened.
    pub(crate) fn dirty(&self) -> bool {
        self.compose().is_some_and(|c| c.dirty)
    }

    /// The notes as HTML.
    pub(crate) fn html(&self) -> String {
        self.compose().map(|c| mailcraft_html::doc_to_html(&c.body)).unwrap_or_default()
    }

    /// Replaces the notes with plain `text` (a test, or the window reset).
    #[cfg(test)]
    pub(crate) fn type_text(&mut self, text: &str) {
        let cid = self.cid;
        self.run("compose.insertText", serde_json::json!({"compose": cid, "text": text}));
    }

    /// Draws the formatting bar and the notes box.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, app: &mut App, salt: u64) {
        ui.horizontal(|ui| crate::edit_message::format_bar(ui, app, self));
        let cid = self.cid;
        egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(("mc-appt-notes", salt))
                .max_height(NOTES_HEIGHT * 1.5)
                .min_scrolled_height(NOTES_HEIGHT)
                .show(ui, |ui| {
                    body_editor_in(ui, self, cid, NOTES_HEIGHT);
                });
        });
    }
}

impl EditorHost for NotesEditor {
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

impl crate::edit_message::EditorHostBar for NotesEditor {
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
