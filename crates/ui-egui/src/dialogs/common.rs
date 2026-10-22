//! Shared dialog plumbing: the modal frame, per-dialog state, replies between dialogs, buttons, the folder
//! tree picker and small value helpers. Look is our own (theme tokens); sizes follow plan/reference/dialogs.md.

use egui::{
    Align, Align2, Color32, Context, Id, Key, Layout, Modifiers, RichText, Sense, Ui, Vec2, WidgetInfo,
    WidgetType,
};
use serde_json::{Value, json};

use crate::App;
use mailcraft_model::{Folder, FolderId, FolderKind, FolderRole};

/// What a dialog wants after one frame.
#[derive(Debug)]
pub(crate) enum Out {
    Keep,
    Close,
    /// Close this dialog and open these.
    CloseOpen(Vec<(String, Value)>),
    /// Keep this dialog and open these on top.
    Open(Vec<(String, Value)>),
}

impl Out {
    pub(crate) fn keeps(&self) -> bool {
        matches!(self, Out::Keep | Out::Open(_))
    }
    pub(crate) fn open(name: &str, params: Value) -> Out {
        Out::Open(vec![(name.to_owned(), params)])
    }
    pub(crate) fn close_open(name: &str, params: Value) -> Out {
        Out::CloseOpen(vec![(name.to_owned(), params)])
    }
    /// `self` unless it is `Keep`, in which case `other`.
    pub(crate) fn or(self, other: Out) -> Out {
        if matches!(self, Out::Keep) { other } else { self }
    }
}

/// One open dialog as seen by its renderer.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Dlg<'a> {
    pub name: &'a str,
    pub params: &'a Value,
    /// Unique per opened dialog (key for state and replies).
    pub uid: u64,
    /// Topmost dialog (gets Escape).
    pub top: bool,
}

impl Dlg<'_> {
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.params.get(key).filter(|v| !v.is_null())
    }
    pub(crate) fn str(&self, key: &str) -> String {
        self.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
    }
    pub(crate) fn u64(&self, key: &str) -> Option<u64> {
        self.get(key).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
    }
    pub(crate) fn bool(&self, key: &str) -> bool {
        self.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
    /// `ids` param as numbers.
    pub(crate) fn ids(&self) -> Vec<u64> {
        self.get("ids")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default()
    }
    pub(crate) fn arr(&self, key: &str) -> Vec<Value> {
        self.get(key).and_then(Value::as_array).cloned().unwrap_or_default()
    }
    /// The dialog that asked for a reply (picker dialogs opened by another dialog).
    pub(crate) fn reply_to(&self) -> Option<(u64, String)> {
        let to = self.u64("replyTo")?;
        Some((to, self.str("replyKey")))
    }
}

// ---------------------------------------------------------------------------------------------------------
// State and replies

/// Runs `f` with this dialog's state of type `T` (created by `init` on first use); keeps the state while the
/// dialog stays open and drops it when it closes.
pub(crate) fn with_state<T: Clone + Send + Sync + 'static>(
    ctx: &Context,
    app: &mut App,
    uid: u64,
    init: impl FnOnce(&App) -> T,
    f: impl FnOnce(&mut T, &mut App) -> Out,
) -> Out {
    let id = Id::new(("mc-dialog-state", uid));
    let mut st = match ctx.data_mut(|d| d.get_temp::<T>(id)) {
        Some(st) => st,
        None => init(app),
    };
    let out = f(&mut st, app);
    if out.keeps() {
        ctx.data_mut(|d| d.insert_temp(id, st));
    } else {
        ctx.data_mut(|d| d.remove::<T>(id));
    }
    out
}

type Replies = Vec<(String, Value)>;

/// Sends `value` under `key` to the dialog `to` (read with [`take_replies`]).
pub(crate) fn send_reply(ctx: &Context, to: u64, key: &str, value: Value) {
    let id = Id::new(("mc-dialog-reply", to));
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Replies>(id).push((key.to_owned(), value)));
    ctx.request_repaint();
}

/// Replies sent to dialog `uid` since the last call.
pub(crate) fn take_replies(ctx: &Context, uid: u64) -> Replies {
    let id = Id::new(("mc-dialog-reply", uid));
    ctx.data_mut(|d| d.remove_temp::<Replies>(id)).unwrap_or_default()
}

/// Params for a child dialog that replies to `parent` under `key`.
pub(crate) fn child(parent: u64, key: &str, mut extra: Value) -> Value {
    if !extra.is_object() {
        extra = json!({});
    }
    if let Some(o) = extra.as_object_mut() {
        o.insert("replyTo".into(), json!(parent));
        o.insert("replyKey".into(), json!(key));
    }
    extra
}

/// Settings kept by dialogs for themselves (columns, folder descriptions, quick actions…), in egui memory.
/// The shell saves them in the profile settings (`state.uiPrefs`) and restores them at start-up, and mirrors
/// every engine option in as a read-only copy under `options.<key>` ([`crate::chrome::sync_ui_prefs`]).
/// Options themselves are read and written through the engine (`app.s.settings`, `options.set`).
pub(crate) use crate::chrome::UiPrefs;

const PREFS_ID: &str = crate::chrome::UI_PREFS_ID;

pub(crate) fn prefs(ctx: &Context) -> UiPrefs {
    ctx.data_mut(|d| d.get_temp::<UiPrefs>(Id::new(PREFS_ID))).unwrap_or_default()
}

/// Replaces the dialog settings (bumps their revision so the shell saves them).
pub(crate) fn set_prefs(ctx: &Context, mut p: UiPrefs) {
    p.rev = prefs(ctx).rev.wrapping_add(1);
    ctx.data_mut(|d| d.insert_temp(Id::new(PREFS_ID), p));
}

pub(crate) fn pref(ctx: &Context, key: &str) -> Option<Value> {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<UiPrefs>(Id::new(PREFS_ID)).values.get(key).cloned())
}

pub(crate) fn set_pref(ctx: &Context, key: &str, v: Value) {
    let mut p = prefs(ctx);
    p.values.insert(key.to_owned(), v);
    set_prefs(ctx, p);
}

/// Removes a dialog setting (nothing happens, and nothing is saved, when it is not there).
pub(crate) fn remove_pref(ctx: &Context, key: &str) {
    let mut p = prefs(ctx);
    if p.values.remove(key).is_some() {
        set_prefs(ctx, p);
    }
}

/// Sets engine options (validated, saved with the profile); returns the first error, if any.
pub(crate) fn set_options(app: &mut App, values: &[(&str, Value)]) -> Option<String> {
    let mut first = None;
    for (k, v) in values {
        if let Err(e) = app.s.set_option(k, v) {
            first.get_or_insert_with(|| e.to_string());
        }
    }
    first
}

/// Mirrors engine options into the dialog settings now (the shell also does it every frame).
pub(crate) fn sync_mirror(ctx: &Context, app: &mut App) {
    crate::chrome::sync_ui_prefs(ctx, app);
}

// ---------------------------------------------------------------------------------------------------------
// Frame

/// Shows a modal dialog with a title bar (title + close button) and `body` below. Escape (topmost dialog
/// only) and the close button cancel.
pub(crate) fn window(
    ctx: &Context,
    app: &mut App,
    d: &Dlg<'_>,
    title: &str,
    width: f32,
    body: impl FnOnce(&mut Ui, &mut App) -> Out,
) -> Out {
    let t = app.theme.clone();
    let frame = egui::Frame::new()
        .fill(t.pane)
        .stroke(egui::Stroke::new(1.0, t.separator))
        .corner_radius(6.0)
        .shadow(egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(60) });
    ctx.data_mut(|m| m.insert_temp(Id::new(TOP_FLAG), d.top));
    // A newly shown topmost dialog takes keyboard focus from whatever had it (e.g. the button that opened it),
    // so Enter/Space go to this dialog.
    let seen = Id::new(("mc-dialog-seen", d.uid));
    if d.top && !ctx.data_mut(|m| m.get_temp::<bool>(seen)).unwrap_or(false) {
        ctx.data_mut(|m| m.insert_temp(seen, true));
        ctx.memory_mut(|m| {
            if let Some(f) = m.focused() {
                m.surrender_focus(f);
            }
        });
    }
    let layer = Id::new(("mc-dialog", d.uid));
    let contents = |ui: &mut Ui| {
        ui.set_width(width.min(ctx.content_rect().width() - 24.0).max(200.0));
        dialog_visuals(ui, &t);
        let mut closed = false;
        egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 6, top: 6, bottom: 2 }).show(
            ui,
            |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(title).strong().size(14.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if title_close(ui, app) {
                            closed = true;
                        }
                    });
                });
            },
        );
        let max_h = (ctx.content_rect().height() - 90.0).max(160.0);
        let out = egui::Frame::new()
            .inner_margin(egui::Margin { left: 14, right: 14, top: 6, bottom: 12 })
            .show(ui, |ui| {
                ui.set_max_height(max_h);
                body(ui, app)
            });
        if closed { Out::Close } else { out.inner }
    };
    // Only the topmost dialog is a modal (one modal layer keeps egui's input routing unambiguous); dialogs
    // below it stay visible but inert under its backdrop.
    let out = if d.top {
        egui::Modal::new(layer)
            .frame(frame)
            .backdrop_color(Color32::from_black_alpha(70))
            .show(ctx, contents)
            .inner
    } else {
        egui::Area::new(layer)
            .kind(egui::UiKind::Modal)
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .interactable(false)
            .show(ctx, |ui| frame.show(ui, contents).inner)
            .inner
    };
    let esc = d.top && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
    if esc { Out::Close } else { out }
}

/// Border of idle form controls inside dialogs (our own greys).
pub(crate) fn control_border(t: &crate::theme::Theme) -> Color32 {
    if t.dark { Color32::from_rgb(0x55, 0x5b, 0x62) } else { Color32::from_rgb(0xa9, 0xaf, 0xb7) }
}

/// Dialog-local visuals: the shell theme draws idle widgets frameless (ribbon look), which leaves text fields,
/// check boxes, radio buttons and buttons invisible on a dialog's plain background. Dialogs give them a
/// border, as form controls have.
pub(crate) fn dialog_visuals(ui: &mut Ui, t: &crate::theme::Theme) {
    let border = control_border(t);
    let v = ui.visuals_mut();
    v.override_text_color = Some(t.text);
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, border);
    v.widgets.inactive.bg_fill = t.pane;
    v.widgets.inactive.weak_bg_fill = t.pane;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, t.accent);
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, t.separator);
    // Dialog fields are bordered: they keep the plain field colour.
    v.text_edit_bg_color = Some(v.extreme_bg_color);
}

fn title_close(ui: &mut Ui, app: &mut App) -> bool {
    let (rect, r) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    r.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Close dialog"));
    if r.hovered() {
        ui.painter().rect_filled(rect, 4.0, app.theme.hover);
    }
    let tint = if r.hovered() { app.theme.text } else { app.theme.text_muted };
    app.icons.paint(ui, rect.shrink(8.0), "close", tint);
    r.on_hover_text("Close").clicked()
}

/// A standard dialog button (minimum 80×26).
pub(crate) fn btn(ui: &mut Ui, label: &str) -> bool {
    btn_en(ui, true, label)
}

pub(crate) fn btn_en(ui: &mut Ui, enabled: bool, label: &str) -> bool {
    btn_w(ui, enabled, label, 80.0)
}

/// A dialog button at least `w` wide (a column of buttons of one width).
pub(crate) fn btn_w(ui: &mut Ui, enabled: bool, label: &str, w: f32) -> bool {
    ui.add_enabled(enabled, egui::Button::new(label).min_size(Vec2::new(w, 26.0))).clicked()
}

/// Accent (default) button. Its text is the theme's `on_accent` colour (white on the light theme's dark teal,
/// near-black on the dark theme's light teal), so it keeps 4.5:1 in both.
pub(crate) fn primary(ui: &mut Ui, app: &App, enabled: bool, label: &str) -> bool {
    primary_w(ui, app, enabled, label, 80.0)
}

/// [`primary`] at least `w` wide.
pub(crate) fn primary_w(ui: &mut Ui, app: &App, enabled: bool, label: &str, w: f32) -> bool {
    let b = egui::Button::new(RichText::new(label).color(if enabled {
        app.theme.on_accent
    } else {
        app.theme.text_muted
    }))
    .fill(if enabled { app.theme.accent } else { app.theme.hover })
    .min_size(Vec2::new(w, 26.0));
    ui.add_enabled(enabled, b).clicked()
}

/// Right-aligned button row; buttons are added right to left by `f`.
pub(crate) fn footer<R>(ui: &mut Ui, f: impl FnOnce(&mut Ui) -> R) -> R {
    ui.add_space(8.0);
    let w = ui.available_width();
    ui.allocate_ui_with_layout(Vec2::new(w, 28.0), Layout::right_to_left(Align::Center), f).inner
}

const TOP_FLAG: &str = "mc-dialog-drawing-top";

/// Enter pressed while the dialog being drawn is the topmost one (default-button activation).
pub(crate) fn enter_pressed(ui: &Ui) -> bool {
    let top = ui.ctx().data_mut(|m| m.get_temp::<bool>(Id::new(TOP_FLAG))).unwrap_or(false);
    top && ui.input(|i| i.key_pressed(Key::Enter) && i.modifiers.is_none())
}

/// OK / Cancel row. Returns Some(true) for OK (or Enter when `ok_enabled`), Some(false) for Cancel.
pub(crate) fn ok_cancel(ui: &mut Ui, app: &App, ok_enabled: bool) -> Option<bool> {
    let enter = ok_enabled && enter_pressed(ui);
    footer(ui, |ui| {
        if btn(ui, "Cancel") {
            return Some(false);
        }
        if primary(ui, app, ok_enabled, "OK") || enter {
            return Some(true);
        }
        None
    })
}

/// Section heading inside a dialog page.
pub(crate) fn heading(ui: &mut Ui, app: &App, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).strong().color(app.theme.text));
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, app.theme.separator);
    ui.add_space(2.0);
}

pub(crate) fn muted(ui: &mut Ui, app: &App, text: &str) {
    ui.label(RichText::new(text).color(app.theme.text_muted));
}

/// A labelled single-line text field on one row.
pub(crate) fn field(ui: &mut Ui, label: &str, value: &mut String, width: f32) -> egui::Response {
    ui.horizontal(|ui| {
        // The label names the text box for screen readers (and tests).
        let l = ui.label(label);
        ui.add(egui::TextEdit::singleline(value).desired_width(width)).labelled_by(l.id)
    })
    .inner
}

/// "YYYY-MM-DD" → (year, month, day) of a real date.
pub(crate) fn parse_ymd(text: &str) -> Option<(i64, u8, u8)> {
    let mut parts = text.trim().splitn(3, '-');
    let y: i64 = parts.next()?.trim().parse().ok()?;
    let m: u8 = parts.next()?.trim().parse().ok()?;
    let d: u8 = parts.next()?.trim().parse().ok()?;
    mailcraft_model::dates::days_from_civil(y, m, d).map(|_| (y, m, d))
}

/// Month shown by a date box's calendar (year, month), kept while it is open.
fn shown_month(ui: &Ui, id: Id) -> Option<(i64, u8)> {
    ui.ctx().data(|d| d.get_temp::<(i64, u8)>(id))
}

/// Month names for the date box's calendar.
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A date box: a "YYYY-MM-DD" field (empty = none; shown in red while it is not a date) with a ▾ button
/// that opens a month calendar to pick from. Returns true when the date changed.
pub(crate) fn date_box(
    ui: &mut Ui,
    app: &App,
    id: impl std::hash::Hash + std::fmt::Debug,
    text: &mut String,
    hint: &str,
    label: &str,
) -> bool {
    let id = Id::new(("mc-date-box", id));
    let before = text.clone();
    ui.horizontal(|ui| {
        let ok = text.trim().is_empty() || parse_ymd(text).is_some();
        let mut edit = egui::TextEdit::singleline(text).hint_text(hint).desired_width(104.0).id_salt(id);
        if !ok {
            edit = edit.text_color(app.theme.danger);
        }
        let r = ui.add(edit).on_hover_text("Date as YYYY-MM-DD");
        r.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, label));
        let pick = ui.menu_button("\u{25BE}", |ui| {
            let today = parse_ymd(&today(app)).unwrap_or((2026, 1, 1));
            let (y, m) = shown_month(ui, id)
                .or_else(|| parse_ymd(text).map(|(y, m, _)| (y, m)))
                .unwrap_or((today.0, today.1));
            let mut month = (y, m);
            ui.horizontal(|ui| {
                if ui.small_button("\u{25C0}").on_hover_text("Previous month").clicked() {
                    month = if m == 1 { (y.saturating_sub(1), 12) } else { (y, m - 1) };
                }
                let name = MONTHS.get(usize::from(m.saturating_sub(1))).copied().unwrap_or("");
                ui.label(RichText::new(format!("{name} {y}")).strong());
                if ui.small_button("\u{25B6}").on_hover_text("Next month").clicked() {
                    month = if m == 12 { (y.saturating_add(1), 1) } else { (y, m + 1) };
                }
            });
            ui.ctx().data_mut(|d| d.insert_temp(id, month));
            let Some(first) = mailcraft_model::dates::days_from_civil(y, m, 1) else { return };
            let lead = i64::from(mailcraft_model::dates::weekday_of_day(first).0);
            let len = i64::from(mailcraft_model::dates::days_in_month(y, m));
            let chosen = parse_ymd(text);
            egui::Grid::new((id, "grid")).spacing([2.0, 2.0]).show(ui, |ui| {
                for w in ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"] {
                    ui.label(RichText::new(w).small());
                }
                ui.end_row();
                let cells = (lead + len + 6) / 7 * 7;
                for cell in 0..cells {
                    let day = cell - lead + 1;
                    if (1..=len).contains(&day) {
                        let d = u8::try_from(day).unwrap_or(1);
                        let on = chosen == Some((y, m, d));
                        let mut t = RichText::new(format!("{day:>2}"));
                        if (y, m, d) == today {
                            t = t.strong().color(app.theme.accent);
                        }
                        if ui.add(egui::Button::selectable(on, t).min_size(Vec2::new(24.0, 20.0))).clicked() {
                            *text = format!("{y:04}-{m:02}-{d:02}");
                            ui.ctx().data_mut(|dd| dd.remove::<(i64, u8)>(id));
                            ui.close();
                        }
                    } else {
                        ui.label("");
                    }
                    if cell % 7 == 6 {
                        ui.end_row();
                    }
                }
            });
            ui.horizontal(|ui| {
                if ui.small_button("Today").clicked() {
                    *text = format!("{:04}-{:02}-{:02}", today.0, today.1, today.2);
                    ui.ctx().data_mut(|dd| dd.remove::<(i64, u8)>(id));
                    ui.close();
                }
                if ui.small_button("None").clicked() {
                    text.clear();
                    ui.ctx().data_mut(|dd| dd.remove::<(i64, u8)>(id));
                    ui.close();
                }
            });
        });
        pick.response.on_hover_text("Pick a date");
    });
    *text != before
}

/// A time box ("HH:MM") with a menu of half-hour times. Returns true when it changed.
pub(crate) fn time_box(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    text: &mut String,
    label: &str,
) -> bool {
    let before = text.clone();
    let id = Id::new(("mc-time-box", id));
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(text).desired_width(52.0).id_salt(id));
        r.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, label));
        ui.menu_button("\u{25BE}", |ui| {
            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                for half in 0..48u32 {
                    let t = format!("{:02}:{:02}", half / 2, (half % 2) * 30);
                    if ui.selectable_label(*text == t, &t).clicked() {
                        *text = t;
                        ui.close();
                    }
                }
            });
        });
    });
    *text != before
}

/// Combo box over string choices; returns true when changed.
pub(crate) fn combo(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    value: &mut String,
    choices: &[&str],
    width: f32,
) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).selected_text(value.as_str()).width(width).show_ui(ui, |ui| {
        for c in choices {
            if ui.selectable_label(value == c, *c).clicked() && value != c {
                *value = (*c).to_owned();
                changed = true;
            }
        }
    });
    changed
}

/// Combo over an index.
pub(crate) fn combo_idx(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    idx: &mut usize,
    choices: &[&str],
    width: f32,
) -> bool {
    let mut changed = false;
    let cur = choices.get(*idx).copied().unwrap_or_default();
    egui::ComboBox::from_id_salt(id).selected_text(cur).width(width).show_ui(ui, |ui| {
        for (i, c) in choices.iter().enumerate() {
            if ui.selectable_label(*idx == i, *c).clicked() && *idx != i {
                *idx = i;
                changed = true;
            }
        }
    });
    changed
}

// ---------------------------------------------------------------------------------------------------------
// Folder tree

/// Which folders a picker offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pick {
    /// Folders that hold mail (move/copy messages, rules).
    Mail,
    /// Any folder that can contain folders (New Folder, Move Folder parents).
    Parent,
    /// Every folder.
    Any,
    /// Folders of these kinds (calendars and task lists for iCalendar, contacts for vCard/CSV).
    Kinds(&'static [FolderKind]),
}

fn pickable(f: &Folder, pick: Pick) -> bool {
    match pick {
        Pick::Mail => f.kind == FolderKind::Mail,
        Pick::Parent => f.kind != FolderKind::Search,
        Pick::Any => true,
        Pick::Kinds(k) => k.contains(&f.kind),
    }
}

/// Icon name for a folder.
pub(crate) fn folder_icon(f: &Folder) -> &'static str {
    match f.role {
        Some(FolderRole::Inbox) => "inbox",
        Some(FolderRole::Drafts) => "drafts",
        Some(FolderRole::SentItems) => "sent",
        Some(FolderRole::DeletedItems) => "deleted",
        Some(FolderRole::Archive) => "archive-folder",
        Some(FolderRole::JunkEmail) => "junk-folder",
        Some(FolderRole::Outbox) => "outbox",
        Some(FolderRole::RssFeeds) => "rss",
        Some(FolderRole::SearchFolders) => "search-folder",
        Some(FolderRole::Calendar) => "calendar",
        Some(FolderRole::Contacts) => "people",
        Some(FolderRole::Tasks) => "tasks",
        Some(FolderRole::Notes) => "notes",
        _ => match f.kind {
            FolderKind::Search => "search-folder",
            FolderKind::Calendar => "calendar",
            FolderKind::Contacts => "people",
            FolderKind::Tasks => "tasks",
            FolderKind::Notes => "notes",
            _ => "folder",
        },
    }
}

/// Result of one frame of a folder tree.
#[derive(Debug, Default)]
pub(crate) struct TreeEvent {
    /// A row was double-clicked (pickers treat it as OK).
    pub activated: bool,
}

/// Folder tree (accounts as headers, folders indented) in a scroll area of `height`; `sel` is the selected
/// folder id. Folders not matching `pick` are shown greyed and cannot be selected.
pub(crate) fn folder_tree(
    ui: &mut Ui,
    app: &mut App,
    sel: &mut Option<u64>,
    pick: Pick,
    salt: impl std::hash::Hash + std::fmt::Debug,
    height: f32,
) -> TreeEvent {
    let mut ev = TreeEvent::default();
    let frame = egui::Frame::new().stroke(egui::Stroke::new(1.0, app.theme.separator)).inner_margin(4.0);
    frame.show(ui, |ui| {
        egui::ScrollArea::vertical()
            .id_salt(salt)
            .max_height(height)
            .min_scrolled_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let accounts: Vec<(mailcraft_model::AccountId, String)> =
                    app.s.mailbox.accounts.iter().map(|a| (a.id, account_title(a))).collect();
                for (acct, name) in accounts {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                        app.icons.paint(ui, r, "account", app.theme.text_muted);
                        ui.label(RichText::new(name).strong());
                    });
                    let roots = app.s.mailbox.children(acct, None);
                    for f in roots {
                        tree_rows(ui, app, sel, pick, f, 1, &mut ev);
                    }
                }
            });
    });
    ev
}

pub(crate) fn account_title(a: &mailcraft_model::Account) -> String {
    if a.address.is_empty() { a.display_name.clone() } else { a.address.clone() }
}

fn tree_rows(
    ui: &mut Ui,
    app: &mut App,
    sel: &mut Option<u64>,
    pick: Pick,
    id: FolderId,
    depth: u32,
    ev: &mut TreeEvent,
) {
    if depth > 40 {
        return;
    }
    let Some(f) = app.s.mailbox.folder(id).cloned() else { return };
    let ok = pickable(&f, pick);
    let selected = *sel == Some(id.0);
    let row_h = 22.0;
    let w = ui.available_width();
    let (rect, r) =
        ui.allocate_exact_size(Vec2::new(w, row_h), if ok { Sense::click() } else { Sense::hover() });
    r.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, ok, selected, &f.name));
    if selected {
        ui.painter().rect_filled(rect, 3.0, app.theme.selected);
    } else if r.hovered() && ok {
        ui.painter().rect_filled(rect, 3.0, app.theme.hover);
    }
    let x = rect.left() + 4.0 + 16.0 * depth as f32;
    let icon = egui::Rect::from_min_size(egui::pos2(x, rect.center().y - 8.0), Vec2::splat(16.0));
    let color = if ok { app.theme.text } else { app.theme.text_muted };
    app.icons.paint(ui, icon, folder_icon(&f), color);
    ui.painter().text(
        egui::pos2(x + 22.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &f.name,
        egui::FontId::proportional(13.0),
        color,
    );
    if r.clicked() {
        *sel = Some(id.0);
    }
    if r.double_clicked() {
        *sel = Some(id.0);
        ev.activated = true;
    }
    let kids = app.s.mailbox.children(f.account, Some(id));
    for k in kids {
        tree_rows(ui, app, sel, pick, k, depth + 1, ev);
    }
}

/// Folder path text ("Inbox\Projects") or empty.
pub(crate) fn folder_path(app: &App, id: Option<u64>) -> String {
    id.map(|i| app.s.mailbox.folder_path(FolderId(i))).unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------------------
// Values

/// "YYYY-MM-DD" (and optional " HH:MM") in the session's offset → unix seconds.
pub(crate) fn parse_date(app: &App, date: &str, time: &str) -> Option<i64> {
    let d = date.trim();
    let mut parts = d.split('-');
    let y: i64 = parts.next()?.trim().parse().ok()?;
    let m: u8 = parts.next()?.trim().parse().ok()?;
    let day: u8 = parts.next()?.trim().parse().ok()?;
    let (h, mi) = parse_clock(time).unwrap_or((0, 0));
    mailcraft_model::dates::local_datetime(y, m, day, h, mi, 0, app.s.tz_offset_min).map(|t| t.unix)
}

/// "HH:MM" (24 h) or "H:MM AM/PM".
pub(crate) fn parse_clock(t: &str) -> Option<(u8, u8)> {
    let t = t.trim().to_ascii_uppercase();
    if t.is_empty() {
        return None;
    }
    let (body, pm, am) = if let Some(b) = t.strip_suffix("PM") {
        (b.trim().to_owned(), true, false)
    } else if let Some(b) = t.strip_suffix("AM") {
        (b.trim().to_owned(), false, true)
    } else {
        (t.clone(), false, false)
    };
    let (h, m) = body.split_once(':').unwrap_or((body.as_str(), "0"));
    let mut h: u8 = h.trim().parse().ok()?;
    let m: u8 = m.trim().parse().ok()?;
    if pm && h < 12 {
        h += 12;
    }
    if am && h == 12 {
        h = 0;
    }
    (h < 24 && m < 60).then_some((h, m))
}

/// Unix seconds → ("YYYY-MM-DD", "HH:MM") in the session's offset.
pub(crate) fn date_parts(app: &App, unix: i64) -> (String, String) {
    let t = mailcraft_model::dates::LocalTime::of(
        mailcraft_model::DateTime { unix, offset_min: app.s.tz_offset_min },
        app.s.tz_offset_min,
    );
    (format!("{:04}-{:02}-{:02}", t.year, t.month, t.day), format!("{:02}:{:02}", t.hour, t.minute))
}

/// Today's date "YYYY-MM-DD" (session clock).
pub(crate) fn today(app: &App) -> String {
    date_parts(app, app.s.now().unix).0
}

/// Splits "a; b, c" into trimmed non-empty parts.
pub(crate) fn split_list(s: &str) -> Vec<String> {
    s.split([';', ',']).map(str::trim).filter(|x| !x.is_empty()).map(str::to_owned).collect()
}

/// Bytes as "12 KB" / "3.4 MB".
pub(crate) fn size_text(bytes: u64) -> String {
    mailcraft_engine::queries::size_text(bytes)
}

// ---------------------------------------------------------------------------------------------------------
// File dialogs (Browse...)

/// Folder to start a file dialog in: `path` itself when it is a folder, else its parent when that exists.
fn start_dir(path: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(path.trim());
    if path.trim().is_empty() {
        return None;
    }
    if p.is_dir() {
        return Some(p.to_path_buf());
    }
    p.parent().filter(|d| d.is_dir()).map(std::path::Path::to_path_buf)
}

/// A Browse... button shown when the app installed file dialogs; returns true when clicked.
pub(crate) fn browse_button(ui: &mut Ui, app: &App) -> bool {
    app.files.is_some() && ui.button("Browse...").clicked()
}

/// Runs the platform's open-file dialog; the chosen file as a string.
pub(crate) fn browse_open(
    app: &App,
    title: &str,
    filters: &[crate::FileFilter],
    current: &str,
) -> Option<String> {
    let files = app.files.clone()?;
    let start = start_dir(current);
    let picked = files.open_files(title, filters, false, start.as_deref())?;
    picked.into_iter().next().map(|p| p.display().to_string())
}

/// Runs the platform's save dialog, suggesting the file name of `current`.
pub(crate) fn browse_save(
    app: &App,
    title: &str,
    filters: &[crate::FileFilter],
    current: &str,
) -> Option<String> {
    let files = app.files.clone()?;
    let path = std::path::Path::new(current.trim());
    let name = if path.is_dir() {
        String::new()
    } else {
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    };
    let start = start_dir(current);
    files.save_file(title, &name, filters, start.as_deref()).map(|p| p.display().to_string())
}

/// Runs the platform's folder picker.
pub(crate) fn browse_folder(app: &App, title: &str, current: &str) -> Option<String> {
    let files = app.files.clone()?;
    let start = start_dir(current);
    files.pick_folder(title, start.as_deref()).map(|p| p.display().to_string())
}
