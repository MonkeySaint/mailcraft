//! Signatures and Stationery (File › Options › Mail › Signatures…, Message / Insert › Signature ▾ ›
//! Signatures…), backed by the mailbox's saved signatures:
//!
//! - **E-mail Signature**: the list of signatures with New, Delete, Rename and Save; the default signature for
//!   new messages and for replies/forwards per account; and the signature body in a rich editor — the compose
//!   body widget ([`crate::compose::body_editor_in`]) running on a private session, with a small formatting
//!   bar (bold, italic, underline, alignment, bullets). New / Delete / Rename / defaults apply at once
//!   (`compose.signatureCreate` / `Delete` / `Edit` / `Defaults`); the body is saved with Save, when another
//!   signature is selected, and with OK. Cancel drops unsaved body edits.
//! - **Personal Stationery**: the font for new messages (engine settings `mail.compose.fontFamily` and
//!   `mail.compose.fontSizePt`, applied to every new message).

use std::sync::{Arc, Mutex};

use egui::{Context, RichText};
use serde_json::{Value, json};

use super::super::common::{self, Dlg, Out, btn, btn_en, muted, ok_cancel, window, with_state};
use crate::App;
use crate::compose::{EditorHost, body_editor_in};
use mailcraft_engine::Session;
use mailcraft_engine::compose::{ComposeKind, ComposeState};
use mailcraft_model::{BodyFormat, Doc, Mailbox};

/// Dialog title.
pub const TITLE: &str = "Signatures and Stationery";
/// "(none)" entry of the default-signature lists.
const NONE: &str = "(none)";
/// Compose ids of the private editor sessions start here (far from the app's own compose ids, which key the
/// editor's egui state).
const EDITOR_ID_BASE: u64 = 1 << 48;

/// The private session the signature body is edited in.
struct SigEditor {
    session: Session,
    cid: u64,
    theme: crate::theme::Theme,
    /// Signature the editor holds.
    name: Option<String>,
}

impl SigEditor {
    fn new(uid: u64, theme: crate::theme::Theme) -> SigEditor {
        let mut session = Session::new(Mailbox::new_local("Signature"));
        session.next_compose = EDITOR_ID_BASE.saturating_add(uid.saturating_mul(1 << 16));
        SigEditor { session, cid: 0, theme, name: None }
    }

    /// Loads `body` for signature `name` (a fresh compose: no undo history, not dirty).
    fn load(&mut self, name: Option<&str>, body: Doc) {
        self.session.composes.clear();
        let c = ComposeState {
            kind: ComposeKind::New,
            format: BodyFormat::Html,
            body,
            ..ComposeState::default()
        };
        self.cid = self.session.open_compose(c);
        self.session.notices.clear();
        if let Some(c) = self.session.composes.get_mut(&self.cid) {
            c.ensure_body();
            let start = c.body_start();
            c.set_caret(start);
            c.dirty = false;
        }
        self.name = name.map(str::to_owned);
    }

    fn compose(&self) -> Option<&ComposeState> {
        self.session.composes.get(&self.cid)
    }

    fn dirty(&self) -> bool {
        self.compose().is_some_and(|c| c.dirty)
    }

    fn html(&self) -> String {
        self.compose().map(|c| mailcraft_html::doc_to_html(&c.body)).unwrap_or_default()
    }
}

impl EditorHost for SigEditor {
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

#[derive(Clone)]
struct SigState {
    tab: usize,
    selected: Option<String>,
    editor: Arc<Mutex<SigEditor>>,
    /// Account whose defaults are shown (`None` = no accounts: the global defaults).
    account: Option<u32>,
    /// Name prompt for New / Rename: (renaming `Some(old)` / new `None`, typed text).
    prompt: Option<(Option<String>, String)>,
    error: Option<String>,
    font: String,
}

fn names(app: &App) -> Vec<String> {
    app.s.mailbox.signatures.iter().map(|s| s.name.clone()).collect()
}

fn body_of(app: &App, name: &str) -> Doc {
    app.s.mailbox.signature(name).map(|s| s.body.clone()).unwrap_or_default()
}

/// Saves the editor's body into its signature when it changed.
fn save_body(app: &mut App, ed: &mut SigEditor) {
    let Some(name) = ed.name.clone() else {
        return;
    };
    if !ed.dirty() || app.s.mailbox.signature(&name).is_none() {
        return;
    }
    let html = ed.html();
    if app.run("compose.signatureEdit", json!({"name": name, "html": html})).is_some()
        && let Some(c) = ed.session.composes.get_mut(&ed.cid)
    {
        c.dirty = false;
    }
}

/// Selects signature `name` (saving the one being edited first).
fn select(app: &mut App, st: &mut SigState, name: Option<String>) {
    if let Ok(mut ed) = st.editor.lock() {
        save_body(app, &mut ed);
        let body = name.as_deref().map(|n| body_of(app, n)).unwrap_or_default();
        ed.load(name.as_deref(), body);
    }
    st.selected = name;
}

pub(crate) fn show(ctx: &Context, app: &mut App, d: &Dlg<'_>) -> Out {
    let uid = d.uid;
    let init = |app: &App| {
        let first = names(app).into_iter().next();
        let mut ed = SigEditor::new(uid, app.theme.clone());
        let body = first.as_deref().map(|n| body_of(app, n)).unwrap_or_default();
        ed.load(first.as_deref(), body);
        SigState {
            tab: usize::from(d.name == "stationery" && d.get("compose").is_none() && d.get("tab").is_some()),
            selected: first,
            editor: Arc::new(Mutex::new(ed)),
            account: app.s.mailbox.accounts.iter().find(|a| !a.address.trim().is_empty()).map(|a| a.id.0),
            prompt: None,
            error: None,
            font: font_choice(app),
        }
    };
    with_state(ctx, app, uid, init, |st, app| {
        if let Ok(mut ed) = st.editor.lock() {
            ed.theme = app.theme.clone();
        }
        // A signature deleted elsewhere (undo, another window) leaves the selection.
        if st.selected.as_deref().is_some_and(|n| app.s.mailbox.signature(n).is_none()) {
            let first = names(app).into_iter().next();
            select(app, st, first);
        }
        window(ctx, app, d, TITLE, 700.0, |ui, app| {
            ui.horizontal(|ui| {
                for (i, t) in ["E-mail Signature", "Personal Stationery"].iter().enumerate() {
                    if ui.selectable_label(st.tab == i, *t).clicked() {
                        st.tab = i;
                    }
                }
            });
            ui.separator();
            // Enter in the name box must not also press OK (the box closes during this frame).
            let prompt_open = st.prompt.is_some();
            if st.tab == 0 {
                signature_tab(ui, app, st, uid);
            } else {
                stationery_tab(ui, app, st, uid);
            }
            match ok_cancel(ui, app, !prompt_open && st.prompt.is_none()) {
                Some(true) => {
                    if let Ok(mut ed) = st.editor.lock() {
                        save_body(app, &mut ed);
                    }
                    let (family, size) = parse_font_choice(&st.font);
                    let err = common::set_options(
                        app,
                        &[
                            ("mail.compose.fontFamily", json!(family)),
                            ("mail.compose.fontSizePt", json!(size)),
                        ],
                    );
                    if let Some(e) = err {
                        st.error = Some(e);
                        return Out::Keep;
                    }
                    Out::Close
                }
                Some(false) => Out::Close,
                None => Out::Keep,
            }
        })
    })
}

fn signature_tab(ui: &mut egui::Ui, app: &mut App, st: &mut SigState, uid: u64) {
    let list = names(app);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(250.0);
            ui.label("Select signature to edit");
            egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0).show(
                ui,
                |ui| {
                    ui.set_min_height(120.0);
                    ui.set_width(240.0);
                    egui::ScrollArea::vertical().id_salt(("sig-list", uid)).max_height(120.0).show(
                        ui,
                        |ui| {
                            for n in &list {
                                let on = st.selected.as_deref() == Some(n.as_str());
                                if ui.selectable_label(on, n).clicked() && !on {
                                    select(app, st, Some(n.clone()));
                                }
                            }
                            if list.is_empty() {
                                muted(ui, app, "No signatures yet.");
                            }
                        },
                    );
                },
            );
            ui.horizontal(|ui| {
                let has = st.selected.is_some();
                if btn_en(ui, has && st.prompt.is_none(), "Delete")
                    && let Some(name) = st.selected.clone()
                {
                    app.run("compose.signatureDelete", json!({"name": name}));
                    if let Ok(mut ed) = st.editor.lock() {
                        ed.name = None;
                    }
                    let first = names(app).into_iter().next();
                    select(app, st, first);
                }
                if btn_en(ui, st.prompt.is_none(), "New") {
                    st.prompt = Some((None, String::new()));
                    st.error = None;
                }
                let dirty = st.editor.lock().map(|e| e.dirty()).unwrap_or(false);
                if btn_en(ui, has && dirty, "Save")
                    && let Ok(mut ed) = st.editor.lock()
                {
                    save_body(app, &mut ed);
                }
                if btn_en(ui, has && st.prompt.is_none(), "Rename") {
                    st.prompt = st.selected.clone().map(|n| (Some(n.clone()), n));
                    st.error = None;
                }
            });
        });
        ui.add_space(12.0);
        ui.vertical(|ui| defaults(ui, app, st, uid, &list));
    });
    if st.prompt.is_some() {
        name_prompt(ui, app, st);
    }
    if let Some(e) = &st.error {
        ui.colored_label(app.theme.danger, e);
    }
    ui.add_space(6.0);
    ui.label("Edit signature");
    let Ok(mut ed) = st.editor.lock() else {
        return;
    };
    if ed.name.is_none() {
        muted(ui, app, "Create a signature with New.");
        return;
    }
    format_bar(ui, app, &mut ed);
    let cid = ed.cid;
    egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).show(ui, |ui| {
        egui::ScrollArea::vertical()
            .id_salt(("sig-body", uid))
            .max_height(200.0)
            .min_scrolled_height(160.0)
            .show(ui, |ui| {
                body_editor_in(ui, &mut *ed, cid, 160.0);
            });
    });
}

/// Bold / Italic / Underline, alignment and bullets for the signature body.
fn format_bar(ui: &mut egui::Ui, app: &mut App, ed: &mut SigEditor) {
    let cid = ed.cid;
    let (bold, italic, underline, align, bullets) = match ed.compose() {
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
    ui.horizontal(|ui| {
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
    });
    if let Some((id, mut params)) = run {
        if let Some(o) = params.as_object_mut() {
            o.insert("compose".into(), json!(cid));
        }
        EditorHost::run(ed, id, params);
        ui.ctx().memory_mut(|m| m.request_focus(crate::compose::editor_id(cid)));
    }
}

/// The New / Rename name box.
fn name_prompt(ui: &mut egui::Ui, app: &mut App, st: &mut SigState) {
    let Some((old, text)) = st.prompt.as_mut() else {
        return;
    };
    let mut done: Option<bool> = None;
    egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(6.0).show(ui, |ui| {
        let title = if old.is_some() { "Rename signature" } else { "New Signature" };
        ui.label(RichText::new(title).strong());
        ui.horizontal(|ui| {
            let l = ui.label("Type a name for this signature:");
            let r = ui.add(egui::TextEdit::singleline(text).desired_width(220.0)).labelled_by(l.id);
            if !r.has_focus() && !r.lost_focus() && text.is_empty() {
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                done = Some(true);
            }
        });
        ui.horizontal(|ui| {
            if btn_en(ui, !text.trim().is_empty(), "OK") {
                done = Some(true);
            }
            if btn(ui, "Cancel") {
                done = Some(false);
            }
        });
    });
    match done {
        Some(true) => {
            let name = text.trim().to_owned();
            let result = match old.clone() {
                Some(o) => app.run("compose.signatureEdit", json!({"name": o, "newName": name})),
                None => app.run("compose.signatureCreate", json!({"name": name, "html": ""})),
            };
            match result {
                Some(r) => {
                    let final_name = r
                        .get("created")
                        .or_else(|| r.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or(name);
                    let renamed = old.is_some();
                    st.prompt = None;
                    st.error = None;
                    if renamed {
                        if let Ok(mut ed) = st.editor.lock() {
                            ed.name = Some(final_name.clone());
                        }
                        st.selected = Some(final_name);
                    } else {
                        select(app, st, Some(final_name));
                    }
                }
                None => {
                    st.error = app.ui.toast.as_ref().map(|(t, _)| t.clone());
                }
            }
        }
        Some(false) => {
            st.prompt = None;
            st.error = None;
        }
        None => {}
    }
}

/// Choose default signature: account, new messages, replies/forwards.
fn defaults(ui: &mut egui::Ui, app: &mut App, st: &mut SigState, uid: u64, list: &[String]) {
    ui.label("Choose default signature");
    let accounts: Vec<(u32, String)> = app
        .s
        .mailbox
        .accounts
        .iter()
        .filter(|a| !a.address.trim().is_empty())
        .map(|a| (a.id.0, a.address.clone()))
        .collect();
    let account = st.account.filter(|id| accounts.iter().any(|(a, _)| a == id));
    let mut choices: Vec<&str> = vec![NONE];
    choices.extend(list.iter().map(String::as_str));
    egui::Grid::new(("sig-defaults", uid)).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
        ui.label("E-mail account:");
        if accounts.is_empty() {
            muted(ui, app, "All accounts");
        } else {
            let mut idx = accounts.iter().position(|(a, _)| Some(*a) == account).unwrap_or(0);
            let labels: Vec<&str> = accounts.iter().map(|(_, s)| s.as_str()).collect();
            if common::combo_idx(ui, ("sig-account", uid), &mut idx, &labels, 220.0) {
                st.account = accounts.get(idx).map(|(a, _)| *a);
            }
        }
        ui.end_row();
        let acc = account.map(mailcraft_model::AccountId);
        for (label, reply, key, salt) in [
            ("New messages:", false, "newMessages", "sig-new"),
            ("Replies/forwards:", true, "replies", "sig-reply"),
        ] {
            ui.label(label);
            let current = app.s.mailbox.default_signature(acc, reply).map(|s| s.name.clone());
            let mut value = current.clone().unwrap_or_else(|| NONE.to_owned());
            common::combo(ui, (salt, uid), &mut value, &choices, 220.0);
            if Some(&value) != current.as_ref() && !(value == NONE && current.is_none()) {
                let v = if value == NONE { Value::Null } else { json!(value) };
                let mut p = json!({ key: v });
                if let (Some(o), Some(a)) = (p.as_object_mut(), account) {
                    o.insert("account".into(), json!(a));
                }
                app.run("compose.signatureDefaults", p);
            }
            ui.end_row();
        }
    });
}

/// Fonts offered for new messages ("Family Npt"; the first keeps the default body font).
const FONT_CHOICES: [&str; 5] =
    [DEFAULT_FONT, "Inter 11pt", "Carlito 11pt", "Liberation Serif 12pt", "Liberation Mono 10pt"];
const DEFAULT_FONT: &str = "Default font";

/// "Family Npt" → (family, size); the default choice → ("", 0).
fn parse_font_choice(choice: &str) -> (String, i64) {
    if choice == DEFAULT_FONT {
        return (String::new(), 0);
    }
    match choice.rsplit_once(' ') {
        Some((family, size)) => {
            let pt =
                size.trim_end_matches("pt").parse::<i64>().ok().filter(|p| (1..=72).contains(p)).unwrap_or(0);
            (family.trim().to_owned(), pt)
        }
        None => (choice.trim().to_owned(), 0),
    }
}

/// The current new-message font as a choice text.
fn font_choice(app: &App) -> String {
    let family = app.s.settings.text("mail.compose.fontFamily");
    let size = app.s.settings.int("mail.compose.fontSizePt");
    match (family.trim().is_empty(), size) {
        (true, _) => DEFAULT_FONT.to_owned(),
        (false, 0) => family,
        (false, n) => format!("{family} {n}pt"),
    }
}

fn stationery_tab(ui: &mut egui::Ui, app: &mut App, st: &mut SigState, uid: u64) {
    common::heading(ui, app, "New mail messages");
    ui.horizontal(|ui| {
        ui.label("Font:");
        common::combo(ui, ("sfont", uid), &mut st.font, &FONT_CHOICES, 200.0);
    });
    muted(ui, app, "The font applies to new messages you write.");
}
