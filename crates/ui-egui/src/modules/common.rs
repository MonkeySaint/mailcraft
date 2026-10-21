//! Shared helpers for the module views: UI state in egui memory, local dates, colours and pane drawing.

use std::collections::{BTreeMap, BTreeSet};

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Id, Layout, Rect, Response, Sense, Stroke, StrokeKind, Ui,
    UiBuilder, Vec2, WidgetInfo, WidgetType, pos2, vec2,
};
use mailcraft_engine::cmd::calendar::CalendarViewState;
use mailcraft_engine::cmd::notes::NotesViewState;
use mailcraft_engine::cmd::view::modules::{ModuleKind, ModuleView};
use mailcraft_model::{
    DateTime, FolderId, FolderKind, FolderPaneState, ItemId, NoteColor, ReadingPanePos, dates,
};

use super::editors::Editors;
use super::editors::dialogs::ModuleDialogs;
use crate::App;
use crate::theme::size;

/// Seconds per day.
pub(crate) const DAY: i64 = 86_400;

/// Everything the module views keep between frames (egui memory, one entry).
#[derive(Clone, Default)]
pub(crate) struct ModState {
    /// Calendar view state the engine's view commands take and return.
    pub cal: CalendarViewState,
    /// Date navigator month (year, month) when the user paged it away from the selected date.
    pub nav_month: Option<(i64, u8)>,
    /// Calendar folders shown (checked in My Calendars); empty = the current calendar.
    pub cal_shown: BTreeSet<u64>,
    /// "My Calendars" group collapsed.
    pub cal_group_collapsed: bool,
    /// Selected time slot (local day, minute of day) in day/week views.
    pub cal_slot: Option<(i64, i64)>,
    /// Key of the last view the grid scrolled to the start of the working day for.
    pub cal_scroll_key: String,
    /// People: business-card grid instead of the list + contact card (`modules::set_people_view`; applied to
    /// the folder's view once, see `people_cards_seen`).
    pub people_cards: bool,
    pub people_cards_seen: bool,
    pub people_search: String,
    /// Letter-index jump requested ('#' = digits).
    pub people_jump: Option<char>,
    /// Tasks: show the selected tasks folder instead of the To-Do List (`modules::set_tasks_todo`; applied
    /// to the folder's view once, see `tasks_folder_seen`).
    pub tasks_folder_view: bool,
    pub tasks_folder_seen: bool,
    /// "Type a new task" text.
    pub new_task: String,
    /// Notes icon arrangement requested with `modules::set_notes_view` (applied to the folder's view once,
    /// see `notes_view_seen`).
    pub notes_view: NotesViewState,
    pub notes_view_seen: NotesViewState,
    /// Notes Icon view: icons placed off the grid (note id → offset from the grid's top-left corner), as
    /// drawn: the saved positions (`notes::positions`) plus the icon being dragged. Cleared by Line Up Icons.
    pub note_pos: BTreeMap<u64, (f32, f32)>,
    /// The icon being dragged and where it is (saved with `notes.setPosition` when dropped).
    pub note_drag: Option<(u64, (f32, f32))>,
    /// Note being edited in place: (id, text).
    pub note_edit: Option<(u64, String)>,
    /// Note whose edit box should take focus.
    pub note_focus: bool,
    pub editors: Editors,
    /// Module dialogs (move/copy, categories, follow up, view settings, apply/save view, mail merge, task
    /// request).
    pub dialogs: ModuleDialogs,
    /// Last egui pass the item windows were drawn in.
    pub windows_pass: Option<u64>,
}

fn state_id() -> Id {
    Id::new("mailcraft.modules.state")
}

pub(crate) fn load(ctx: &egui::Context) -> ModState {
    ctx.data_mut(|d| d.get_temp::<ModState>(state_id())).unwrap_or_default()
}

pub(crate) fn store(ctx: &egui::Context, st: ModState) {
    ctx.data_mut(|d| d.insert_temp(state_id(), st));
}

/// Runs a command and returns its error text (from the toast) on failure.
pub(crate) fn run_checked(
    app: &mut App,
    id: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match app.run(id, params) {
        Some(v) => Ok(v),
        None => Err(app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_else(|| format!("{id} failed"))),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Dates (local day numbers = days since 1970-01-01 in the session's display offset)
// ---------------------------------------------------------------------------------------------------------

pub(crate) fn tz(app: &App) -> i32 {
    app.s.tz_offset_min
}

/// Local seconds since the epoch of `t`.
pub(crate) fn local_secs(t: DateTime, tz: i32) -> i64 {
    t.unix.saturating_add(i64::from(tz).saturating_mul(60))
}

pub(crate) fn today(app: &App) -> i64 {
    local_secs(app.s.now(), tz(app)).div_euclid(DAY)
}

/// Minute of the day now.
pub(crate) fn now_minute(app: &App) -> i64 {
    local_secs(app.s.now(), tz(app)).rem_euclid(DAY) / 60
}

pub(crate) fn ymd(day: i64) -> (i64, u8, u8) {
    dates::civil_from_days(day)
}

pub(crate) fn day_of(y: i64, m: u8, d: u8) -> Option<i64> {
    dates::days_from_civil(y, m, d)
}

/// "YYYY-MM-DD" → day number.
pub(crate) fn parse_day(s: &str) -> Option<i64> {
    let s = s.trim();
    let date = s.split(['T', ' ']).next().unwrap_or("");
    let mut p = date.split('-');
    let y: i64 = p.next()?.trim().parse().ok()?;
    let m: u8 = p.next()?.trim().parse().ok()?;
    let d: u8 = p.next()?.trim().parse().ok()?;
    if p.next().is_some() || !(1..=9999).contains(&y) {
        return None;
    }
    day_of(y, m, d)
}

/// "YYYY-MM-DDTHH:MM" → (day, minute of day).
pub(crate) fn parse_day_time(s: &str) -> Option<(i64, i64)> {
    let day = parse_day(s)?;
    let minute = match s.trim().split_once(['T', ' ']) {
        Some((_, t)) => parse_clock(t)?,
        None => 0,
    };
    Some((day, minute))
}

/// "HH:MM" (24 h) or "h:mm AM/PM" → minute of day.
pub(crate) fn parse_clock(s: &str) -> Option<i64> {
    let s = s.trim().to_ascii_uppercase();
    let (body, pm) = if let Some(b) = s.strip_suffix("PM") {
        (b.trim().to_owned(), Some(true))
    } else if let Some(b) = s.strip_suffix("AM") {
        (b.trim().to_owned(), Some(false))
    } else {
        (s.clone(), None)
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => {
            (h.trim().parse::<i64>().ok()?, m.trim().get(..2).unwrap_or(m.trim()).parse::<i64>().ok()?)
        }
        None => (body.trim().parse::<i64>().ok()?, 0),
    };
    if !(0..60).contains(&m) {
        return None;
    }
    let h = match pm {
        Some(pm) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            (h % 12) + if pm { 12 } else { 0 }
        }
        None => h,
    };
    if !(0..24).contains(&h) {
        return None;
    }
    Some(h * 60 + m)
}

pub(crate) fn day_string(day: i64) -> String {
    let (y, m, d) = ymd(day);
    format!("{y:04}-{m:02}-{d:02}")
}

pub(crate) fn day_time_string(day: i64, minute: i64) -> String {
    let minute = minute.clamp(0, 24 * 60 - 1);
    format!("{}T{:02}:{:02}", day_string(day), minute / 60, minute % 60)
}

/// Sunday = 0.
pub(crate) fn weekday(day: i64) -> u8 {
    dates::weekday_of_day(day).0
}

/// Sunday starting the week of `day`.
pub(crate) fn week_start(day: i64) -> i64 {
    day.saturating_sub(i64::from(weekday(day)))
}

pub(crate) const MONTHS: [&str; 12] = [
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

pub(crate) const WEEKDAYS: [&str; 7] =
    ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

pub(crate) fn month_name(m: u8) -> &'static str {
    MONTHS.get(usize::from(m).saturating_sub(1)).copied().unwrap_or("")
}

pub(crate) fn weekday_name(day: i64) -> &'static str {
    WEEKDAYS.get(usize::from(weekday(day))).copied().unwrap_or("")
}

pub(crate) fn weekday_short(day: i64) -> &'static str {
    weekday_name(day).get(..3).unwrap_or("")
}

/// (year, month) moved by `delta` months.
pub(crate) fn add_months(y: i64, m: u8, delta: i64) -> (i64, u8) {
    let idx = y.saturating_mul(12).saturating_add(i64::from(m) - 1).saturating_add(delta);
    (idx.div_euclid(12), u8::try_from(idx.rem_euclid(12) + 1).unwrap_or(1))
}

/// `day` moved by `delta` months, clamping the day of month.
pub(crate) fn shift_month(day: i64, delta: i64) -> i64 {
    let (y, m, d) = ymd(day);
    let (ny, nm) = add_months(y, m, delta);
    let d = d.min(dates::days_in_month(ny, nm)).max(1);
    day_of(ny, nm, d).unwrap_or(day)
}

/// "9:30 AM".
pub(crate) fn clock(minute: i64) -> String {
    let minute = minute.rem_euclid(24 * 60);
    let (h, m) = (minute / 60, minute % 60);
    let (h12, ap) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{h12}:{m:02} {ap}")
}

/// "9 AM" for the time ruler.
pub(crate) fn hour_label(hour: i64) -> String {
    match hour.rem_euclid(24) {
        0 => "12 AM".to_owned(),
        h @ 1..=11 => format!("{h} AM"),
        12 => "12 PM".to_owned(),
        h => format!("{} PM", h - 12),
    }
}

/// "10/9/2026".
pub(crate) fn short_date(day: i64) -> String {
    let (y, m, d) = ymd(day);
    format!("{m}/{d}/{y}")
}

/// "Friday, October 9, 2026".
pub(crate) fn long_date(day: i64) -> String {
    let (y, m, d) = ymd(day);
    format!("{}, {} {d}, {y}", weekday_name(day), month_name(m))
}

// ---------------------------------------------------------------------------------------------------------
// Colours (our own palette)
// ---------------------------------------------------------------------------------------------------------

pub(crate) fn geom_color(c: mailcraft_geom::Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

/// Linear blend of two opaque colours (`t` = share of `b`).
pub(crate) fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| -> u8 {
        let v = f32::from(x) * (1.0 - t) + f32::from(y) * t;
        v.round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// Colour of the first category (of `names`) that has one.
pub(crate) fn category_color(app: &App, names: &[String]) -> Option<Color32> {
    names.iter().find_map(|n| {
        mailcraft_model::Category::find(&app.s.mailbox.categories, n)
            .map(|c| c.color)
            .filter(|c| *c != mailcraft_model::CategoryColor::None)
            .map(|c| geom_color(c.rgb()))
    })
}

/// Sticky-note fill.
pub(crate) fn note_fill(color: NoteColor, dark: bool) -> Color32 {
    match (color, dark) {
        (NoteColor::Yellow, false) => Color32::from_rgb(0xfb, 0xee, 0xa6),
        (NoteColor::Blue, false) => Color32::from_rgb(0xcd, 0xe4, 0xf7),
        (NoteColor::Green, false) => Color32::from_rgb(0xd3, 0xee, 0xc6),
        (NoteColor::Pink, false) => Color32::from_rgb(0xf6, 0xd0, 0xe1),
        (NoteColor::White, false) => Color32::from_rgb(0xfd, 0xfd, 0xfb),
        (NoteColor::Yellow, true) => Color32::from_rgb(0x5a, 0x50, 0x24),
        (NoteColor::Blue, true) => Color32::from_rgb(0x24, 0x41, 0x5a),
        (NoteColor::Green, true) => Color32::from_rgb(0x2c, 0x4b, 0x2a),
        (NoteColor::Pink, true) => Color32::from_rgb(0x58, 0x2d, 0x43),
        (NoteColor::White, true) => Color32::from_rgb(0x3a, 0x3d, 0x41),
    }
}

/// Persona circle colours (index from `queries::persona_color`).
pub(crate) fn persona_fill(idx: u8) -> Color32 {
    const P: [(u8, u8, u8); 16] = [
        (0x1f, 0x77, 0x8c),
        (0x8c, 0x4a, 0x1f),
        (0x4a, 0x6b, 0x1f),
        (0x6b, 0x2f, 0x8c),
        (0x2f, 0x52, 0x9e),
        (0x9e, 0x2f, 0x52),
        (0x2f, 0x7d, 0x5a),
        (0x7d, 0x6a, 0x1f),
        (0x5a, 0x3d, 0x9e),
        (0x1f, 0x5f, 0x5f),
        (0x8c, 0x3a, 0x3a),
        (0x3a, 0x5f, 0x8c),
        (0x5f, 0x7d, 0x2f),
        (0x8c, 0x2f, 0x7d),
        (0x2f, 0x6b, 0x8c),
        (0x6b, 0x4f, 0x2f),
    ];
    let (r, g, b) = P.get(usize::from(idx % 16)).copied().unwrap_or((0x1f, 0x77, 0x8c));
    Color32::from_rgb(r, g, b)
}

// ---------------------------------------------------------------------------------------------------------
// Panes
// ---------------------------------------------------------------------------------------------------------

pub(crate) const GAP: f32 = 6.0;
pub(crate) const RADIUS: u8 = 6;

/// Width of the slim bar above a module's folder pane (with its « button).
const PANE_BAR: f32 = 26.0;

/// The folder pane of a module view (Calendar, People, Tasks, Notes), following View › Folder Pane:
/// - Normal: the pane on the left (its rect is returned below a slim bar whose « button minimises it);
/// - Minimized — chosen, or automatic in narrow windows (≤ 800 wide) and Reading view — a strip with the
///   current folder's name written downwards and a » button: the button restores Normal (or, when the
///   pane is only minimised automatically, shows it as a flyout until « closes it), the name shows the
///   flyout;
/// - Off: nothing.
///
/// Returns the folder pane rect when it is shown (for the view to fill) and the main area.
pub(crate) fn module_folder_pane(ui: &mut Ui, app: &mut App) -> (Option<Rect>, Rect) {
    let full = ui.max_rect();
    let setting = app.s.view.folder_pane;
    if setting == FolderPaneState::Off || full.width() < size::FOLDER_PANE_MIN + 120.0 {
        return (None, full);
    }
    let window_w = ui.ctx().content_rect().width();
    let auto =
        setting == FolderPaneState::Normal && (app.s.view.reading_view || window_w <= size::AUTO_MINIMIZE_W);
    let minimized = setting == FolderPaneState::Minimized || auto;
    let flyout_id = Id::new("mc-modules-folder-flyout");
    let mut flyout = minimized && ui.ctx().data(|d| d.get_temp::<bool>(flyout_id)).unwrap_or(false);
    let roomy = full.width() > size::FOLDER_PANE + 200.0;
    let theme = app.theme.clone();
    let out = if (!minimized || flyout) && roomy {
        let pane = Rect::from_min_size(full.min, vec2(size::FOLDER_PANE, full.height()));
        let main = Rect::from_min_max(pos2(pane.max.x + GAP, full.min.y), full.max);
        let bar = Rect::from_min_size(pane.min, vec2(pane.width(), PANE_BAR));
        pane_bg(ui, pane, theme.pane);
        let b = Rect::from_center_size(pos2(bar.right() - 16.0, bar.center().y), Vec2::splat(19.0));
        let resp = ui.interact(b, Id::new("mc-modules-folder-minimize"), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(b, CornerRadius::same(3), theme.hover);
        }
        app.icons.paint(
            ui,
            Rect::from_center_size(b.center(), Vec2::splat(14.0)),
            "chevron-left",
            theme.text_muted,
        );
        a11y(&resp, WidgetType::Button, "Minimize the Folder Pane");
        if resp.on_hover_text("Minimize the Folder Pane").clicked() {
            if minimized {
                flyout = false;
            } else {
                app.run("view.folderPane", serde_json::json!({ "mode": "minimized" }));
            }
        }
        (Some(Rect::from_min_max(pos2(pane.min.x, bar.max.y), pane.max)), main)
    } else {
        let strip = Rect::from_min_size(full.min, vec2(size::FOLDER_PANE_MIN, full.height()));
        let main = Rect::from_min_max(pos2(strip.max.x + GAP, full.min.y), full.max);
        pane_bg(ui, strip, theme.pane);
        let b = Rect::from_min_size(pos2(strip.center().x - 9.5, strip.top() + 7.5), Vec2::splat(19.0));
        let resp = ui.interact(b, Id::new("mc-modules-folder-expand"), Sense::click());
        if resp.hovered() || flyout {
            ui.painter().rect_filled(b, CornerRadius::same(3), theme.hover);
        }
        app.icons.paint(
            ui,
            Rect::from_center_size(b.center(), Vec2::splat(14.0)),
            "chevron-right",
            theme.text_muted,
        );
        a11y(&resp, WidgetType::Button, "Expand the Folder Pane");
        if resp.on_hover_text("Expand the Folder Pane").clicked() {
            if auto {
                flyout = !flyout;
            } else {
                app.run("view.folderPane", serde_json::json!({ "mode": "normal" }));
            }
        }
        // The current folder's name, written downwards.
        let name = app.s.mailbox.folder(app.s.selection.folder).map(|f| f.name.clone()).unwrap_or_default();
        let top = b.bottom() + 12.0;
        let room = (strip.bottom() - top - 8.0).max(0.0);
        if !name.is_empty() && room > 20.0 {
            let galley = ui.painter().layout(name.clone(), FontId::proportional(13.0), theme.text, room);
            let len = galley.size().x.min(room);
            let item = Rect::from_min_size(pos2(strip.left(), top - 4.0), vec2(strip.width(), len + 8.0));
            let iresp = ui.interact(item, Id::new("mc-modules-folder-name"), Sense::click());
            if iresp.hovered() {
                ui.painter().rect_filled(item.shrink2(vec2(2.0, 0.0)), CornerRadius::same(3), theme.hover);
            }
            let pos = pos2(strip.center().x + galley.size().y / 2.0, top);
            ui.painter().with_clip_rect(item).add(
                egui::epaint::TextShape::new(pos, galley, theme.text).with_angle(std::f32::consts::FRAC_PI_2),
            );
            a11y(&iresp, WidgetType::Button, &name);
            if iresp.on_hover_text(&name).clicked() && roomy {
                flyout = !flyout;
            }
        }
        (None, main)
    };
    ui.ctx().data_mut(|d| d.insert_temp(flyout_id, flyout));
    out
}

/// Paints a contact's picture in `rect` (rounded to a circle); false when the contact has none or it cannot
/// be shown (only PNG pictures are decoded; others keep the initials).
pub(crate) fn paint_contact_picture(ui: &Ui, rect: Rect, c: &mailcraft_model::Contact) -> bool {
    let Some(pic) = c.picture.as_ref() else { return false };
    let Some(tex) = picture_texture(ui.ctx(), c.id, pic) else { return false };
    let radius =
        u8::try_from((rect.width().min(rect.height()) / 2.0).clamp(0.0, 255.0) as u32).unwrap_or(255);
    egui::Image::from_texture((tex.id(), rect.size()))
        .corner_radius(CornerRadius::same(radius))
        .paint_at(ui, rect);
    true
}

/// Cheap fingerprint of a picture (its size and both ends) to notice a changed picture.
fn picture_key(pic: &mailcraft_model::contact::ContactPicture) -> u64 {
    let d = &pic.data;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ d.len() as u64;
    let head = d.get(..d.len().min(256)).unwrap_or(&[]);
    let tail = d.get(d.len().saturating_sub(256)..).unwrap_or(&[]);
    for b in head.iter().chain(tail) {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Decodes a contact picture (PNG, JPEG or GIF, bounded size) into an image.
pub(crate) fn decode_picture(pic: &mailcraft_model::contact::ContactPicture) -> Option<egui::ColorImage> {
    crate::body::decode_contact_picture(pic)
}

/// The texture of a contact's picture, decoded once per picture and kept in egui memory.
fn picture_texture(
    ctx: &egui::Context,
    id: ItemId,
    pic: &mailcraft_model::contact::ContactPicture,
) -> Option<egui::TextureHandle> {
    let key = picture_key(pic);
    let mem = Id::new(("mc-contact-picture", id.0));
    if let Some((k, tex)) = ctx.data(|d| d.get_temp::<(u64, Option<egui::TextureHandle>)>(mem))
        && k == key
    {
        return tex;
    }
    let tex = decode_picture(pic)
        .map(|img| ctx.load_texture(format!("contact-picture-{}", id.0), img, egui::TextureOptions::LINEAR));
    ctx.data_mut(|d| d.insert_temp(mem, (key, tex.clone())));
    tex
}

/// Fills a rounded pane background.
pub(crate) fn pane_bg(ui: &Ui, rect: Rect, fill: Color32) {
    ui.painter().rect_filled(rect, CornerRadius::same(RADIUS), fill);
}

/// Runs `add` in a child Ui confined to `rect`.
pub(crate) fn child<R>(ui: &mut Ui, rect: Rect, salt: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let builder = UiBuilder::new().max_rect(rect).id_salt(salt).layout(Layout::top_down(Align::Min));
    let clip = ui.clip_rect().intersect(rect);
    ui.scope_builder(builder, |ui| {
        ui.set_clip_rect(clip);
        add(ui)
    })
    .inner
}

/// Adds an accessible name to a painted control.
pub(crate) fn a11y(resp: &Response, typ: WidgetType, label: &str) {
    let label = label.to_owned();
    resp.widget_info(move || WidgetInfo::labeled(typ, true, label.clone()));
}

/// Folder-pane group header ("My Calendars") with an expand chevron; returns true when clicked.
pub(crate) fn group_header(ui: &mut Ui, app: &mut App, text: &str, collapsed: bool, id: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    a11y(&resp, WidgetType::Button, text);
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(4), app.theme.hover);
    }
    let chev = Rect::from_center_size(pos2(rect.min.x + 12.0, rect.center().y), Vec2::splat(12.0));
    let icon = if collapsed { "chevron-right" } else { "chevron-down" };
    app.icons.paint(ui, chev, icon, app.theme.text_muted);
    ui.painter().text(
        pos2(rect.min.x + 24.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(14.0),
        app.theme.text,
    );
    let _ = id;
    resp.clicked()
}

/// Folder rows of `kind` (observed: "Calendar - account" when several accounts). Returns the clicked folder.
pub(crate) fn folder_rows(
    ui: &mut Ui,
    app: &mut App,
    kind: FolderKind,
    selected: Option<FolderId>,
    checks: Option<&BTreeSet<u64>>,
) -> (Option<FolderId>, Option<FolderId>) {
    let mut clicked = None;
    let mut toggled = None;
    let multi = app.s.mailbox.accounts.len() > 1;
    let mut folders: Vec<(String, FolderId)> = app
        .s
        .mailbox
        .folders
        .values()
        .filter(|f| f.kind == kind)
        .map(|f| {
            let label = if multi {
                let acct =
                    app.s.mailbox.account(f.account).map(|a| a.display_name.clone()).unwrap_or_default();
                format!("{} - {acct}", f.name)
            } else {
                f.name.clone()
            };
            (label, f.id)
        })
        .collect();
    folders.sort_by_key(|(name, id)| (app.s.mailbox.folder(*id).map(|f| f.sort_key()), name.clone()));
    for (label, id) in folders {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), size::FOLDER_ROW), Sense::click());
        a11y(&resp, WidgetType::Button, &label);
        let is_sel = selected == Some(id);
        if is_sel {
            ui.painter().rect_filled(rect, CornerRadius::same(4), app.theme.selected_folder);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(4), app.theme.hover);
        }
        let mut x = rect.min.x + 16.0;
        if let Some(checks) = checks {
            let on = checks.contains(&id.0);
            let b = Rect::from_center_size(pos2(x + 7.0, rect.center().y), Vec2::splat(13.0));
            let bresp = ui.interact(b.expand(3.0), Id::new(("mc-folder-check", id.0)), Sense::click());
            a11y(&bresp, WidgetType::Checkbox, &format!("Show {label}"));
            draw_check(ui, b, on, app.theme.accent, app.theme.text_muted);
            if bresp.clicked() {
                toggled = Some(id);
            }
            x += 22.0;
        }
        let font = FontId::proportional(13.0);
        let galley =
            ui.painter().layout(label.clone(), font, app.theme.text, (rect.max.x - x - 6.0).max(10.0));
        let y = rect.center().y - galley.size().y / 2.0;
        ui.painter().with_clip_rect(rect).galley(pos2(x, y), galley, app.theme.text);
        if resp.clicked() && toggled.is_none() {
            clicked = Some(id);
        }
    }
    (clicked, toggled)
}

/// Paints a checkbox square.
pub(crate) fn draw_check(ui: &Ui, b: Rect, on: bool, accent: Color32, border: Color32) {
    let p = ui.painter();
    if on {
        p.rect_filled(b, CornerRadius::same(2), accent);
        let s = Stroke::new(1.6, Color32::WHITE);
        p.line_segment([pos2(b.min.x + 3.0, b.center().y), pos2(b.min.x + 5.5, b.max.y - 3.5)], s);
        p.line_segment([pos2(b.min.x + 5.5, b.max.y - 3.5), pos2(b.max.x - 3.0, b.min.y + 3.5)], s);
    } else {
        p.rect_stroke(b, CornerRadius::same(2), Stroke::new(1.0, border), StrokeKind::Inside);
    }
}

/// Text truncated to one line within `width` (painted).
pub(crate) fn paint_text_clipped(
    ui: &Ui,
    pos: egui::Pos2,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
) -> Rect {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat { font_id: font, color, ..Default::default() },
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(4.0));
    let galley = ui.painter().layout_job(job);
    let rect = Rect::from_min_size(pos, galley.size());
    ui.painter().galley(pos, galley, color);
    rect
}

/// A centred, muted "empty" message.
pub(crate) fn empty_message(ui: &Ui, rect: Rect, app: &App, title: &str, hint: &str) {
    let c = rect.center();
    ui.painter().text(
        pos2(c.x, c.y - 10.0),
        Align2::CENTER_CENTER,
        title,
        FontId::proportional(14.0),
        app.theme.text,
    );
    ui.painter().text(
        pos2(c.x, c.y + 10.0),
        Align2::CENTER_CENTER,
        hint,
        FontId::proportional(12.0),
        app.theme.text_muted,
    );
}

/// The current folder of a module kind (selected folder when it holds that kind, else the default).
pub(crate) fn current_folder(
    app: &App,
    kind: mailcraft_engine::cmd::calendar::pim::Kind,
) -> Option<FolderId> {
    mailcraft_engine::cmd::calendar::pim::current_folder(&app.s, kind)
}

/// Selects a folder through the engine (`nav.selectFolder`).
pub(crate) fn select_folder(app: &mut App, folder: FolderId) {
    app.run("nav.selectFolder", serde_json::json!({ "folder": folder.0 }));
}

// ---------------------------------------------------------------------------------------------------------
// Selection, module views, layout
// ---------------------------------------------------------------------------------------------------------

/// Selects a module item (or nothing): the module selection the views draw and the engine selection, so
/// ribbon commands, keyboard commands and their enabled checks act on it.
pub(crate) fn select_item(app: &mut App, id: Option<u64>) {
    app.ui.module_selection = id;
    match id {
        Some(i) => {
            app.s.selection.items = vec![ItemId(i)];
            app.s.selection.focus = Some(ItemId(i));
        }
        None => {
            app.s.selection.items.clear();
            app.s.selection.focus = None;
        }
    }
}

/// Keeps the module selection and the engine selection in step at the start of a module view's frame: an
/// item the engine selected (Delete selects the neighbour) wins over the view's; a view selection the engine
/// does not know yet (an item window saved, a click) is pushed to the engine. `is_item` tells which ids
/// belong to the view.
pub(crate) fn sync_selection(app: &mut App, is_item: impl Fn(&App, ItemId) -> bool) {
    let engine = app.s.selection.focus.or_else(|| app.s.selection.items.first().copied());
    let view = app.ui.module_selection.map(ItemId);
    match (engine, view) {
        (Some(e), v) if Some(e) != v && is_item(app, e) => app.ui.module_selection = Some(e.0),
        (_, Some(v)) if Some(v) != engine && is_item(app, v) => select_item(app, Some(v.0)),
        (_, Some(v)) if !is_item(app, v) => app.ui.module_selection = None,
        _ => {}
    }
}

/// Makes sure the engine's selected folder is a folder of the module (the View commands act on it): when
/// another folder is selected, the module's current folder is selected.
pub(crate) fn ensure_folder(
    app: &mut App,
    kind: mailcraft_engine::cmd::calendar::pim::Kind,
) -> Option<FolderId> {
    let folder = current_folder(app, kind)?;
    if app.s.selection.folder != folder {
        select_folder(app, folder);
    }
    Some(folder)
}

/// The module view of `folder` (the engine's per-folder view state).
pub(crate) fn module_view(app: &App, folder: Option<FolderId>, kind: ModuleKind) -> ModuleView {
    folder
        .map(|f| mailcraft_engine::cmd::view::modules::state(&app.s, f, kind))
        .unwrap_or_else(|| ModuleView::defaults(kind))
}

/// Runs `view.changeView` for the module's current folder.
pub(crate) fn change_view(app: &mut App, name: &str) {
    app.run("view.changeView", serde_json::json!({ "name": name }));
}

/// Row height with View › Use Tighter Spacing applied.
pub(crate) fn row_height(app: &App, base: f32) -> f32 {
    let tight = mailcraft_engine::queries::view_settings(&app.s, app.s.selection.folder).tighter_spacing;
    if tight { (base * 0.78).round() } else { base }
}

/// Group labels collapsed in `folder` (View › Expand/Collapse).
pub(crate) fn collapsed_labels(app: &App, folder: FolderId) -> BTreeSet<String> {
    app.s
        .view
        .collapsed_groups
        .get(&folder)
        .map(|v| v.iter().filter(|e| mailcraft_engine::cmd::view::is_group_label(e)).cloned().collect())
        .unwrap_or_default()
}

/// Collapses or expands a group of a module view: the engine's group commands act on the selected item's
/// group, so an item of the group (`first`) is selected first.
pub(crate) fn toggle_group(app: &mut App, first: ItemId, label: &str, collapsed: bool) {
    let inside = app.s.selection.focus == Some(first);
    if !inside {
        select_item(app, Some(first.0));
    }
    let id = if collapsed { "view.expandGroup" } else { "view.collapseGroup" };
    // The header names its group: an item filed under several categories has a row in each.
    app.run(id, serde_json::json!({ "label": label }));
}

/// A group header row (▾/▸, label, item count); returns true when clicked.
pub(crate) fn group_row(ui: &mut Ui, app: &mut App, label: &str, count: usize, collapsed: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    let state = if collapsed { "collapsed" } else { "expanded" };
    a11y(&resp, WidgetType::Button, &format!("Group {label}: {count} items, {state}"));
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(3), app.theme.hover);
    }
    let chev = Rect::from_center_size(pos2(rect.min.x + 10.0, rect.center().y), Vec2::splat(12.0));
    app.icons.paint(ui, chev, if collapsed { "chevron-right" } else { "chevron-down" }, app.theme.text_muted);
    let text = format!("{label}: {count}");
    ui.painter().text(
        pos2(rect.min.x + 22.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(12.5),
        if label == "Overdue" { app.theme.danger } else { app.theme.accent },
    );
    ui.painter().hline(rect.x_range(), rect.max.y - 1.0, Stroke::new(1.0, app.theme.separator));
    resp.clicked()
}

/// Splits a module's content into the item list and the reading pane by the folder's reading pane position
/// (`Off` or too little room → no pane). With the pane on the right the list is `list_w` wide; at the
/// bottom it takes `frac` of the height.
pub(crate) fn reading_split(rect: Rect, pos: ReadingPanePos, list_w: f32, frac: f32) -> (Rect, Option<Rect>) {
    match pos {
        ReadingPanePos::Right if rect.width() > list_w + 280.0 => {
            let list = Rect::from_min_size(rect.min, vec2(list_w, rect.height()));
            (list, Some(Rect::from_min_max(pos2(list.max.x + 1.0, rect.min.y), rect.max)))
        }
        ReadingPanePos::Bottom if rect.height() > 260.0 => {
            let h = (rect.height() * frac.clamp(0.2, 0.8)).round();
            let list = Rect::from_min_size(rect.min, vec2(rect.width(), h));
            (list, Some(Rect::from_min_max(pos2(rect.min.x, list.max.y + 1.0), rect.max)))
        }
        _ => (rect, None),
    }
}

/// Draws the separator between the list and the reading pane.
pub(crate) fn pane_separator(ui: &Ui, app: &App, list: Rect, pane: Rect) {
    let stroke = Stroke::new(1.0, app.theme.separator);
    if (pane.min.y - list.max.y).abs() < 2.0 && pane.min.x <= list.min.x + 1.0 {
        ui.painter().hline(list.x_range(), list.max.y, stroke);
    } else {
        ui.painter().vline(list.max.x, list.y_range(), stroke);
    }
}

/// The To-Do Bar (View › Layout › To-Do Bar) on the right of a module's content when it is on and there is
/// room; returns the rest of `rect`.
pub(crate) fn todo_bar_strip(ui: &mut Ui, app: &mut App, rect: Rect) -> Rect {
    let w = super::TODO_BAR_WIDTH;
    if !super::todo_bar_shown(app) || rect.width() < w + 420.0 {
        return rect;
    }
    let bar = Rect::from_min_max(pos2(rect.max.x - w, rect.min.y), rect.max);
    pane_bg(ui, bar, app.theme.pane);
    child(ui, bar.shrink(6.0), "mc-module-todo-bar", |ui| super::todo_bar(ui, app));
    Rect::from_min_max(rect.min, pos2(bar.min.x - GAP, rect.max.y))
}

/// Small toggle buttons for a module's views (the ribbon's Current View gallery, also in the view's
/// toolbar). Returns the view picked.
pub(crate) fn view_switch(
    ui: &mut Ui,
    app: &mut App,
    views: &[(&str, &str)],
    current: &str,
) -> Option<String> {
    let mut picked = None;
    for (name, icon) in views.iter().rev() {
        if crate::widgets::toggle_button(ui, app, icon, name, current == *name).clicked() && current != *name
        {
            picked = Some((*name).to_owned());
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        let d = parse_day("2026-10-09").unwrap();
        assert_eq!(day_string(d), "2026-10-09");
        assert_eq!(weekday(d), 5);
        assert_eq!(weekday_name(d), "Friday");
        assert_eq!(week_start(d), d - 5);
        assert_eq!(long_date(d), "Friday, October 9, 2026");
        assert_eq!(short_date(d), "10/9/2026");
        assert_eq!(parse_day("2026-02-30"), None);
        assert_eq!(parse_day("junk"), None);
        assert_eq!(parse_day_time("2026-10-09T14:30"), Some((d, 870)));
        assert_eq!(day_time_string(d, 870), "2026-10-09T14:30");
    }

    #[test]
    fn clocks() {
        assert_eq!(clock(0), "12:00 AM");
        assert_eq!(clock(9 * 60 + 30), "9:30 AM");
        assert_eq!(clock(12 * 60), "12:00 PM");
        assert_eq!(clock(23 * 60 + 5), "11:05 PM");
        assert_eq!(parse_clock("9:30 AM"), Some(570));
        assert_eq!(parse_clock("12:00 AM"), Some(0));
        assert_eq!(parse_clock("12:15 pm"), Some(735));
        assert_eq!(parse_clock("17:45"), Some(1065));
        assert_eq!(parse_clock("25:00"), None);
        assert_eq!(parse_clock("13:00 PM"), None);
        assert_eq!(hour_label(0), "12 AM");
        assert_eq!(hour_label(15), "3 PM");
    }

    #[test]
    fn months() {
        assert_eq!(add_months(2026, 12, 1), (2027, 1));
        assert_eq!(add_months(2026, 1, -1), (2025, 12));
        let jan31 = parse_day("2026-01-31").unwrap();
        assert_eq!(day_string(shift_month(jan31, 1)), "2026-02-28");
        assert_eq!(month_name(13), "");
    }

    #[test]
    fn blend_mixes() {
        let c = blend(Color32::BLACK, Color32::WHITE, 0.5);
        assert!(c.r() > 120 && c.r() < 135);
    }

    fn folder_pane_harness(module: &str, mode: &str, width: f32) -> egui_kittest::Harness<'static, App> {
        let mut app = App::new(mailcraft_engine::sample::sample_session());
        app.run(module, serde_json::json!({}));
        app.run("view.folderPane", serde_json::json!({ "mode": mode }));
        let view: fn(&mut Ui, &mut App) = match module {
            "nav.tasks" => crate::modules::tasks,
            "nav.notes" => crate::modules::notes,
            "nav.calendar" => crate::modules::calendar,
            _ => crate::modules::people,
        };
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(width, 800.0))
            .build_ui_state(move |ui: &mut Ui, app: &mut App| view(ui, app), app);
        h.run_ok();
        h
    }

    fn labelled(h: &egui_kittest::Harness<'_, App>, label: &str) -> bool {
        use egui_kittest::kittest::Queryable;
        h.query_all_by_label(label).next().is_some()
    }

    fn press(h: &mut egui_kittest::Harness<'_, App>, label: &str) {
        use egui_kittest::kittest::Queryable;
        if let Some(n) = h.query_all_by_label(label).next() {
            n.click();
        }
        h.run_ok();
        h.run_ok();
    }

    #[test]
    fn minimized_folder_pane_is_a_strip_in_modules() {
        for (module, group) in [
            ("nav.calendar", "My Calendars"),
            ("nav.people", "My Contacts"),
            ("nav.tasks", "My Tasks"),
            ("nav.notes", "My Notes"),
        ] {
            let mut h = folder_pane_harness(module, "minimized", 1400.0);
            assert!(labelled(&h, "Expand the Folder Pane"), "{module}: strip shown");
            assert!(!labelled(&h, group), "{module}: pane hidden");
            press(&mut h, "Expand the Folder Pane");
            assert_eq!(h.state().s.view.folder_pane, FolderPaneState::Normal, "{module}");
            assert!(labelled(&h, group) && labelled(&h, "Minimize the Folder Pane"), "{module}");
            press(&mut h, "Minimize the Folder Pane");
            assert_eq!(h.state().s.view.folder_pane, FolderPaneState::Minimized, "{module}");
            let h = folder_pane_harness(module, "off", 1400.0);
            assert!(!labelled(&h, "Expand the Folder Pane") && !labelled(&h, group), "{module}: off");
        }
    }

    #[test]
    fn narrow_window_minimizes_the_pane_with_a_flyout() {
        let mut h = folder_pane_harness("nav.people", "normal", 760.0);
        assert!(labelled(&h, "Expand the Folder Pane") && !labelled(&h, "My Contacts"));
        press(&mut h, "Expand the Folder Pane");
        assert!(labelled(&h, "My Contacts"), "flyout");
        assert_eq!(h.state().s.view.folder_pane, FolderPaneState::Normal, "the setting stays");
        press(&mut h, "Minimize the Folder Pane");
        assert!(!labelled(&h, "My Contacts"), "flyout closed");
        assert_eq!(h.state().s.view.folder_pane, FolderPaneState::Normal);
    }
}
