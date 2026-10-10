//! Painting and layout of the Simplified (one 49-px row) and Classic (100-px groups) ribbons, width
//! degradation (plan/outlook/responsive.md), dropdown menus, galleries and keytip badges.

use egui::containers::menu::SubMenu;
use egui::{
    Align2, Color32, CornerRadius, FontId, Id, Popup, PopupCloseBehavior, Pos2, Rect, Response, Sense,
    Stroke, StrokeKind, Ui, Vec2,
};

use super::defs::{Act, CmdRef, Ctl, Group, ItemKind, Kind, Rule, Size, Tab, defs};
use super::state::{self, Action, Mem, Resolved};
use crate::{App, Surface};

/// Simplified row height of a control.
const S_H: f32 = 32.0;
/// Classic small-control row height.
const C_ROW: f32 = 22.0;
const C_ROW_GAP: f32 = 2.0;
/// Classic group label band.
const C_LABEL: f32 = 18.0;
/// "…" overflow button (36 × 41).
const MORE_W: f32 = 36.0;
const MORE_H: f32 = 41.0;
/// Ribbon Display Options button at the far right.
const DISPLAY_W: f32 = 26.0;
const TEXT: f32 = 12.5;
const SMALL_TEXT: f32 = 11.5;

/// Per-frame drawing context.
pub struct Cx {
    /// Context of the ui the ribbon draws in (calendar commands keep their view state in it).
    pub ctx: egui::Context,
    pub surface: Surface,
    pub simplified: bool,
    /// Effective width for the degradation rules.
    pub width: f32,
    pub tab: &'static Tab,
    /// Keytip sequence typed after this tab's key (badges shown when `Some`).
    pub keytip: Option<String>,
    pub mem: Mem,
    pub id: Id,
    /// Keytip badges collected while drawing.
    pub badges: Vec<(Rect, String)>,
}

fn active(rules: &[Rule], classic: bool, width: f32) -> Vec<Act> {
    rules.iter().filter(|r| r.classic == classic && width <= r.max_width).map(|r| r.act).collect()
}

fn text_w(ui: &Ui, text: &str, size: f32) -> f32 {
    ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(size), Color32::WHITE).size().x
}

/// `text` cut with "…" to fit `max` points (gallery chips).
fn elide(ui: &Ui, text: &str, max: f32) -> String {
    if text_w(ui, text, TEXT) <= max {
        return text.to_owned();
    }
    let mut out: String = text.to_owned();
    while out.pop().is_some() {
        let candidate = format!("{}…", out.trim_end());
        if text_w(ui, &candidate, TEXT) <= max {
            return candidate;
        }
    }
    String::new()
}

fn muted(c: Color32) -> Color32 {
    c.gamma_multiply(0.45)
}

// ---------------------------------------------------------------------------------------------------------
// Control state and actions
// ---------------------------------------------------------------------------------------------------------

fn ctl_on(app: &App, cx: &Cx, c: &Ctl) -> bool {
    c.on.as_deref().is_some_and(|s| !s.starts_with("value:") && state::is_on(app, cx.surface, &cx.mem, s))
}

fn ctl_action(app: &App, cx: &Cx, c: &Ctl, value: Option<&str>) -> Action {
    let on = ctl_on(app, cx, c);
    let cmd = match (&c.oncmd, on) {
        (Some(alt), true) => alt,
        _ => &c.cmd,
    };
    let mut a = state::action(app, cmd, on, value, &[]);
    a.flip = c.on.as_deref().and_then(|s| s.strip_prefix("mem:")).map(str::to_owned);
    a
}

fn ctl_disabled(app: &App, cx: &Cx, c: &Ctl) -> Option<&'static str> {
    state::disabled(app, cx.surface, &c.cmd.id)
}

/// The command template of a gallery's items (the `dyn` line of its menu with the same source).
pub(super) fn gallery_template(c: &Ctl) -> Option<&'static CmdRef> {
    let src = c.source.as_deref()?;
    let menu = defs().menu(c.menu.as_deref()?)?;
    menu.iter().find(|i| i.kind == ItemKind::Dyn && i.target == src).map(|i| &i.cmd)
}

fn run(app: &mut App, cx: &mut Cx, act: &Action) {
    let ctx = cx.ctx.clone();
    state::run(&ctx, app, cx.surface, &mut cx.mem, act);
}

// ---------------------------------------------------------------------------------------------------------
// Button painting
// ---------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Look<'a> {
    icon: &'a str,
    label: Option<&'a str>,
    large: bool,
    arrow: bool,
    on: bool,
    enabled: bool,
    /// Draw a check box instead of the icon state (Check kind).
    check: bool,
}

/// Splits a large-button label into at most two lines (at the space nearest the middle).
fn wrap2(ui: &Ui, label: &str) -> (String, String) {
    if text_w(ui, label, SMALL_TEXT) <= 54.0 || !label.contains(' ') {
        return (label.to_owned(), String::new());
    }
    let mid = label.len() / 2;
    let split = label
        .char_indices()
        .filter(|(_, ch)| *ch == ' ')
        .map(|(i, _)| i)
        .min_by_key(|i| i.abs_diff(mid))
        .unwrap_or(0);
    let (a, b) = label.split_at(split);
    (a.trim().to_owned(), b.trim().to_owned())
}

fn large_w(ui: &Ui, label: &str, arrow: bool) -> f32 {
    let (a, b) = wrap2(ui, label);
    let extra = if arrow { 12.0 } else { 0.0 };
    let w =
        text_w(ui, &a, SMALL_TEXT).max(text_w(ui, &b, SMALL_TEXT) + if b.is_empty() { 0.0 } else { extra });
    (w + 12.0).max(44.0)
}

fn small_w(ui: &Ui, label: Option<&str>, arrow: bool, size: f32) -> f32 {
    let base = match label {
        Some(l) if !l.is_empty() => 6.0 + 16.0 + 5.0 + text_w(ui, l, size) + 7.0,
        _ => 28.0,
    };
    base + if arrow { 12.0 } else { 0.0 }
}

fn arrow_shape(painter: &egui::Painter, center: Pos2, color: Color32) {
    let pts =
        vec![center + Vec2::new(-3.5, -1.5), center + Vec2::new(3.5, -1.5), center + Vec2::new(0.0, 2.5)];
    painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

fn paint_bg(ui: &Ui, app: &App, rect: Rect, hovered: bool, on: bool, down: bool) {
    let t = &app.theme;
    if on {
        ui.painter().rect_filled(rect, CornerRadius::same(4), t.selected);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, t.accent.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );
    } else if down {
        ui.painter().rect_filled(rect, CornerRadius::same(4), t.selected);
    } else if hovered {
        ui.painter().rect_filled(rect, CornerRadius::same(4), t.hover);
    }
}

fn paint_icon(ui: &Ui, app: &mut App, rect: Rect, name: &str, tint: Color32) {
    if !name.is_empty() {
        app.icons.paint(ui, rect, name, tint);
    }
}

fn paint_check(ui: &Ui, app: &mut App, rect: Rect, on: bool, color: Color32) {
    let r = Rect::from_center_size(rect.center(), Vec2::splat(12.0));
    ui.painter().rect_stroke(r, CornerRadius::same(2), Stroke::new(1.0, color), StrokeKind::Inside);
    if on {
        paint_icon(ui, app, r.shrink(1.0), "check", color);
    }
}

/// Paints a button's content (background drawn separately).
fn paint_look(ui: &Ui, app: &mut App, rect: Rect, look: Look<'_>) {
    let base = app.theme.text;
    let color = if look.enabled { base } else { muted(base) };
    let painter = ui.painter().clone();
    if look.large {
        let icon_r = Rect::from_center_size(Pos2::new(rect.center().x, rect.top() + 21.0), Vec2::splat(32.0));
        if look.check {
            paint_check(ui, app, icon_r, look.on, color);
        } else {
            paint_icon(ui, app, icon_r, look.icon, color);
        }
        let (a, b) = wrap2(ui, look.label.unwrap_or_default());
        let y = rect.top() + 46.0;
        painter.text(
            Pos2::new(rect.center().x, y),
            Align2::CENTER_TOP,
            &a,
            FontId::proportional(SMALL_TEXT),
            color,
        );
        if b.is_empty() {
            if look.arrow {
                arrow_shape(&painter, Pos2::new(rect.center().x, y + 21.0), color);
            }
        } else {
            let bw = text_w(ui, &b, SMALL_TEXT);
            let shift = if look.arrow { -6.0 } else { 0.0 };
            painter.text(
                Pos2::new(rect.center().x + shift, y + 14.0),
                Align2::CENTER_TOP,
                &b,
                FontId::proportional(SMALL_TEXT),
                color,
            );
            if look.arrow {
                arrow_shape(&painter, Pos2::new(rect.center().x + shift + bw / 2.0 + 7.0, y + 21.0), color);
            }
        }
    } else {
        let has_label = look.label.is_some_and(|l| !l.is_empty());
        let icon_x = if has_label {
            rect.left() + 6.0 + 8.0
        } else if look.arrow {
            rect.left() + 14.0
        } else {
            rect.center().x
        };
        let icon_r = Rect::from_center_size(Pos2::new(icon_x, rect.center().y), Vec2::splat(16.0));
        if look.check {
            paint_check(ui, app, icon_r, look.on, color);
        } else {
            paint_icon(ui, app, icon_r, look.icon, color);
        }
        let size = if rect.height() > 26.0 { TEXT } else { SMALL_TEXT };
        if let Some(l) = look.label.filter(|l| !l.is_empty()) {
            painter.text(
                Pos2::new(rect.left() + 27.0, rect.center().y),
                Align2::LEFT_CENTER,
                l,
                FontId::proportional(size),
                color,
            );
        }
        if look.arrow {
            arrow_shape(&painter, Pos2::new(rect.right() - 8.0, rect.center().y), color);
        }
    }
}

fn tooltip(resp: Response, label: &str, disabled: Option<&str>) -> Response {
    match disabled {
        Some(why) if !label.is_empty() => resp.on_hover_text(format!("{label}\n{why}")),
        Some(why) => resp.on_hover_text(why),
        None if !label.is_empty() => resp.on_hover_text(label),
        None => resp,
    }
}

/// A plain/toggle/drop button; returns its (clickable) response.
fn button(
    ui: &mut Ui,
    app: &mut App,
    id: Id,
    rect: Rect,
    look: Look<'_>,
    tip: &str,
    why: Option<&str>,
) -> Response {
    let sense = if look.enabled { Sense::click() } else { Sense::hover() };
    let resp = ui.interact(rect, id, sense);
    let kind =
        if look.check || look.on { egui::WidgetType::SelectableLabel } else { egui::WidgetType::Button };
    resp.widget_info(|| egui::WidgetInfo::selected(kind, look.enabled, look.on, tip));
    let open = Popup::is_id_open(ui.ctx(), Popup::default_response_id(&resp));
    paint_bg(
        ui,
        app,
        rect,
        resp.hovered() && look.enabled,
        look.on,
        open || resp.is_pointer_button_down_on(),
    );
    paint_look(ui, app, rect, look);
    tooltip(resp, tip, why)
}

/// A split button: returns (main part, ▾ part).
fn split_button(
    ui: &mut Ui,
    app: &mut App,
    id: Id,
    rect: Rect,
    look: Look<'_>,
    tip: &str,
    why: Option<&str>,
) -> (Response, Response) {
    let (main_r, arrow_r) = if look.large {
        let cut = rect.top() + 40.0;
        (
            Rect::from_min_max(rect.min, Pos2::new(rect.right(), cut)),
            Rect::from_min_max(Pos2::new(rect.left(), cut), rect.max),
        )
    } else {
        let cut = rect.right() - 13.0;
        (
            Rect::from_min_max(rect.min, Pos2::new(cut, rect.bottom())),
            Rect::from_min_max(Pos2::new(cut, rect.top()), rect.max),
        )
    };
    let sense = if look.enabled { Sense::click() } else { Sense::hover() };
    let main = ui.interact(main_r, id, sense);
    main.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, look.enabled, tip));
    let arrow = ui.interact(arrow_r, id.with("arrow"), Sense::click());
    arrow.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{tip} options")));
    let open = Popup::is_id_open(ui.ctx(), Popup::default_response_id(&arrow));
    let any_hover = (main.hovered() && look.enabled) || arrow.hovered();
    if any_hover || open || look.on {
        paint_bg(ui, app, rect, any_hover, look.on, open);
        let sep = if look.large {
            [Pos2::new(rect.left() + 3.0, main_r.bottom()), Pos2::new(rect.right() - 3.0, main_r.bottom())]
        } else {
            [Pos2::new(main_r.right(), rect.top() + 4.0), Pos2::new(main_r.right(), rect.bottom() - 4.0)]
        };
        ui.painter().line_segment(sep, Stroke::new(1.0, app.theme.separator));
    }
    paint_look(ui, app, rect, look);
    (tooltip(main, tip, why), arrow.on_hover_text(format!("{tip} options")))
}

// ---------------------------------------------------------------------------------------------------------
// Menus
// ---------------------------------------------------------------------------------------------------------

/// One menu row; returns its response.
fn menu_row(
    ui: &mut Ui,
    app: &mut App,
    icon: &str,
    swatch: Option<[u8; 3]>,
    label: &str,
    on: Option<bool>,
    enabled: bool,
    sub: bool,
) -> Response {
    let tw = text_w(ui, label, TEXT);
    let want = 34.0 + tw + 34.0;
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().max(want), 24.0),
        if enabled { Sense::click() } else { Sense::hover() },
    );
    match on {
        Some(on) => {
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, on, label))
        }
        None => resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label)),
    }
    let t = app.theme.clone();
    if resp.hovered() && enabled {
        ui.painter().rect_filled(rect, CornerRadius::same(3), t.hover);
    }
    let color = if enabled { t.text } else { muted(t.text) };
    let icon_r = Rect::from_center_size(Pos2::new(rect.left() + 14.0, rect.center().y), Vec2::splat(16.0));
    if let Some([r, g, b]) = swatch {
        let sw = Rect::from_center_size(icon_r.center(), Vec2::splat(12.0));
        ui.painter().rect_filled(sw, CornerRadius::same(2), Color32::from_rgb(r, g, b));
        ui.painter().rect_stroke(
            sw,
            CornerRadius::same(2),
            Stroke::new(1.0, t.separator),
            StrokeKind::Inside,
        );
        if on == Some(true) {
            ui.painter().rect_stroke(
                sw.expand(2.0),
                CornerRadius::same(3),
                Stroke::new(1.5, t.accent),
                StrokeKind::Inside,
            );
        }
    } else if on == Some(true) {
        ui.painter().rect_filled(icon_r.expand(2.0), CornerRadius::same(3), t.selected);
        paint_icon(ui, app, icon_r, if icon.is_empty() { "check" } else { icon }, color);
    } else {
        paint_icon(ui, app, icon_r, icon, color);
    }
    ui.painter().text(
        Pos2::new(rect.left() + 30.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(TEXT),
        color,
    );
    if sub {
        let r = Rect::from_center_size(Pos2::new(rect.right() - 12.0, rect.center().y), Vec2::splat(12.0));
        paint_icon(ui, app, r, "chevron-right", color);
    }
    resp
}

/// Keytip badge at the right end of a menu row while keytips are shown (the row's key after its menu's).
fn menu_key_badge(ui: &Ui, app: &App, row: Rect, key: &str, sub: bool) {
    if key.is_empty() || app.ui.keytips.is_none() {
        return;
    }
    let painter = ui.painter();
    let galley = painter.layout_no_wrap(key.to_owned(), FontId::monospace(10.5), app.theme.pane);
    let size = galley.size() + Vec2::new(6.0, 2.0);
    let right = row.right() - if sub { 26.0 } else { 6.0 };
    let r = Rect::from_min_size(Pos2::new(right - size.x, row.center().y - size.y / 2.0), size);
    painter.rect_filled(r, CornerRadius::same(3), app.theme.text);
    painter.galley(r.min + Vec2::new(3.0, 1.0), galley, app.theme.pane);
}

fn menu_sep(ui: &mut Ui, app: &App) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(40.0), 7.0), Sense::hover());
    ui.painter().line_segment(
        [Pos2::new(rect.left() + 4.0, rect.center().y), Pos2::new(rect.right() - 4.0, rect.center().y)],
        Stroke::new(1.0, app.theme.separator),
    );
}

/// Draws resolved menu entries; runs clicked commands and closes the menu.
fn menu_entries(ui: &mut Ui, app: &mut App, cx: &mut Cx, entries: &[Resolved], depth: u8) {
    for e in entries {
        match e {
            Resolved::Sep => menu_sep(ui, app),
            Resolved::Cmd { label, icon, swatch, on, disabled, action, key } => {
                let resp = menu_row(ui, app, icon, *swatch, label, *on, disabled.is_none(), false);
                menu_key_badge(ui, app, resp.rect, key, false);
                let resp = match disabled {
                    Some(why) => resp.on_hover_text(*why),
                    None => resp,
                };
                if resp.clicked() {
                    run(app, cx, action);
                    ui.close();
                }
            }
            Resolved::Sub { label, icon, menu, key } => {
                let resp = menu_row(ui, app, icon, None, label, None, true, true);
                menu_key_badge(ui, app, resp.rect, key, true);
                if depth < 6 {
                    let sub = defs().menu(menu).unwrap_or_default();
                    let items = state::expand_items(app, cx.surface, &cx.mem, sub);
                    SubMenu::new()
                        .show(ui, &resp, |ui| menu_entries(ui, app, cx, &items, depth.saturating_add(1)));
                }
            }
        }
    }
}

fn popup_for(resp: &Response) -> Popup<'static> {
    Popup::menu(resp).close_behavior(PopupCloseBehavior::CloseOnClickOutside)
}

/// Shows the named menu below `resp` while it is open.
fn show_menu(app: &mut App, cx: &mut Cx, resp: &Response, menu: Option<&str>) {
    let items = menu.and_then(|m| defs().menu(m)).unwrap_or_default();
    popup_for(resp).show(|ui| {
        ui.set_min_width(200.0);
        let entries = state::expand_items(app, cx.surface, &cx.mem, items);
        menu_entries(ui, app, cx, &entries, 0);
    });
}

/// Opens a popup now if a keytip asked for this control.
fn honour_open_request(ui: &Ui, cx: &mut Cx, key: &str, resp: &Response) {
    if !key.is_empty() && cx.mem.open_request.as_deref() == Some(key) {
        cx.mem.open_request = None;
        Popup::open_id(ui.ctx(), Popup::default_response_id(resp));
    }
}

/// A control rendered as a menu entry (Simplified "…" overflow, collapsed groups).
fn ctl_in_menu(ui: &mut Ui, app: &mut App, cx: &mut Cx, c: &Ctl) {
    let why = ctl_disabled(app, cx, c);
    match c.kind {
        Kind::Button | Kind::Toggle | Kind::Check => {
            let on = c.on.as_ref().map(|_| ctl_on(app, cx, c));
            let resp = menu_row(ui, app, &c.icon, None, &c.label, on, why.is_none(), false);
            if resp.clicked() {
                let act = ctl_action(app, cx, c, None);
                run(app, cx, &act);
                ui.close();
            }
        }
        Kind::Edit => {
            let key = c.key.clone();
            let mut text = cx.mem.text.get(&key).cloned().unwrap_or_default();
            let resp = ui
                .add(egui::TextEdit::singleline(&mut text).hint_text(c.label.as_str()).desired_width(200.0));
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            cx.mem.text.insert(key, text.clone());
            if enter && !text.trim().is_empty() {
                let act = ctl_action(app, cx, c, Some(&text));
                run(app, cx, &act);
                ui.close();
            }
        }
        Kind::Split | Kind::Drop | Kind::Gallery | Kind::Combo => {
            let resp = menu_row(ui, app, &c.icon, None, &c.label, None, true, true);
            let mut entries: Vec<Resolved> = Vec::new();
            if c.kind == Kind::Split {
                entries.push(Resolved::Cmd {
                    label: c.label.clone(),
                    icon: c.icon.clone(),
                    swatch: None,
                    key: c.key.clone(),
                    on: None,
                    disabled: why,
                    action: ctl_action(app, cx, c, None),
                });
                entries.push(Resolved::Sep);
            }
            let items = c.menu.as_deref().and_then(|m| defs().menu(m)).unwrap_or_default();
            entries.extend(state::expand_items(app, cx.surface, &cx.mem, items));
            SubMenu::new().show(ui, &resp, |ui| menu_entries(ui, app, cx, &entries, 1));
        }
    }
}

fn badge(cx: &mut Cx, rect: Rect, key: &str) {
    if !key.is_empty() {
        cx.badges.push((rect, key.to_owned()));
    }
}

/// Paints collected keytip badges matching the typed sequence.
pub fn paint_badges(ui: &Ui, app: &App, cx: &Cx) {
    let Some(rest) = cx.keytip.as_deref() else {
        return;
    };
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, cx.id.with("keytips")));
    for (rect, key) in &cx.badges {
        if !key.starts_with(rest) || !ui.clip_rect().contains(rect.center()) {
            continue;
        }
        let galley = painter.layout_no_wrap(key.clone(), FontId::monospace(11.0), app.theme.pane);
        let size = galley.size() + Vec2::new(8.0, 2.0);
        let r = Rect::from_center_size(Pos2::new(rect.center().x, rect.bottom() - size.y / 2.0), size);
        painter.rect_filled(r, CornerRadius::same(3), app.theme.text);
        painter.galley(r.min + Vec2::new(4.0, 1.0), galley, app.theme.pane);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Shared control drawing (both modes)
// ---------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Style {
    large: bool,
    label: bool,
    narrow: bool,
}

fn ctl_width(ui: &Ui, c: &Ctl, st: Style, simplified: bool) -> f32 {
    let size = if simplified { TEXT } else { SMALL_TEXT };
    match c.kind {
        Kind::Edit => c.width.unwrap_or(140.0),
        Kind::Combo => c.width.unwrap_or(80.0) + 14.0,
        Kind::Gallery if simplified => {
            if st.narrow {
                139.0
            } else {
                c.width.unwrap_or(240.0)
            }
        }
        // Classic galleries narrow to one column (Quick Steps 295 → 171 at 1366, responsive.md).
        Kind::Gallery if st.narrow => 150.0,
        Kind::Gallery => c.width.unwrap_or(200.0),
        _ if st.large => large_w(ui, &c.label, c.kind.has_arrow()),
        _ => small_w(ui, st.label.then_some(c.label.as_str()), c.kind.has_arrow(), size),
    }
}

fn draw_ctl(ui: &mut Ui, app: &mut App, cx: &mut Cx, c: &Ctl, rect: Rect, st: Style) {
    let id = cx.id.with((cx.tab.name.as_str(), c.key.as_str(), c.label.as_str()));
    let why = ctl_disabled(app, cx, c);
    let on = ctl_on(app, cx, c);
    let look = Look {
        icon: &c.icon,
        label: st.label.then_some(c.label.as_str()),
        large: st.large,
        arrow: c.kind.has_arrow(),
        on: on && c.kind != Kind::Check,
        enabled: why.is_none(),
        check: c.kind == Kind::Check && !cx.simplified,
    };
    badge(cx, rect, &c.key);
    match c.kind {
        Kind::Button | Kind::Toggle | Kind::Check => {
            let resp = button(ui, app, id, rect, look, &c.label, why);
            if resp.clicked() {
                let act = ctl_action(app, cx, c, None);
                run(app, cx, &act);
            }
        }
        Kind::Split => {
            let (main, arrow) = split_button(ui, app, id, rect, look, &c.label, why);
            if main.clicked() {
                let act = ctl_action(app, cx, c, None);
                run(app, cx, &act);
            }
            honour_open_request(ui, cx, &c.key, &arrow);
            show_menu(app, cx, &arrow, c.menu.as_deref());
        }
        Kind::Drop => {
            let look = Look { enabled: true, ..look };
            let resp = button(ui, app, id, rect, look, &c.label, None);
            honour_open_request(ui, cx, &c.key, &resp);
            show_menu(app, cx, &resp, c.menu.as_deref());
        }
        Kind::Combo => combo(ui, app, cx, c, id, rect),
        Kind::Edit => edit(ui, app, cx, c, id, rect, why),
        Kind::Gallery => gallery(ui, app, cx, c, id, rect, st),
    }
}

fn combo(ui: &mut Ui, app: &mut App, cx: &mut Cx, c: &Ctl, id: Id, rect: Rect) {
    let r = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 22.0f32.min(rect.height())));
    let resp = ui.interact(r, id, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, c.label.as_str()));
    let t = app.theme.clone();
    ui.painter().rect_filled(r, CornerRadius::same(3), t.pane);
    let border = if resp.hovered() { t.accent } else { t.separator };
    ui.painter().rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, border), StrokeKind::Inside);
    let mut value = state::combo_value(app, cx.surface, c.on.as_deref());
    if value.is_empty() {
        value.clone_from(&c.label);
    }
    let clip = ui.painter().with_clip_rect(r.shrink2(Vec2::new(4.0, 1.0)));
    clip.text(
        Pos2::new(r.left() + 6.0, r.center().y),
        Align2::LEFT_CENTER,
        value,
        FontId::proportional(SMALL_TEXT),
        t.text,
    );
    arrow_shape(ui.painter(), Pos2::new(r.right() - 7.0, r.center().y), t.text);
    let resp = resp.on_hover_text(c.label.as_str());
    honour_open_request(ui, cx, &c.key, &resp);
    show_menu(app, cx, &resp, c.menu.as_deref());
}

fn edit(ui: &mut Ui, app: &mut App, cx: &mut Cx, c: &Ctl, id: Id, rect: Rect, why: Option<&str>) {
    let r = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 24.0f32.min(rect.height())));
    let key = c.key.clone();
    let mut text = cx.mem.text.get(&key).cloned().unwrap_or_default();
    let te = egui::TextEdit::singleline(&mut text)
        .id(id)
        .hint_text(c.label.as_str())
        .desired_width(r.width() - 8.0);
    let resp = ui.put(r, te);
    if cx.mem.open_request.as_deref() == Some(key.as_str()) {
        cx.mem.open_request = None;
        resp.request_focus();
    }
    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    cx.mem.text.insert(key, text.clone());
    if enter && why.is_none() && !text.trim().is_empty() {
        let act = ctl_action(app, cx, c, Some(&text));
        run(app, cx, &act);
    }
}

fn gallery(ui: &mut Ui, app: &mut App, cx: &mut Cx, c: &Ctl, id: Id, rect: Rect, st: Style) {
    let t = app.theme.clone();
    let template = gallery_template(c);
    let items = c.source.as_deref().map(|s| state::dyn_items(app, cx.surface, s)).unwrap_or_default();
    let arrow_w = 16.0;
    let body = Rect::from_min_max(rect.min, Pos2::new(rect.right() - arrow_w, rect.bottom()));
    let arrow_r = Rect::from_min_max(Pos2::new(rect.right() - arrow_w, rect.top()), rect.max);
    if !cx.simplified {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(3),
            Stroke::new(1.0, t.separator),
            StrokeKind::Inside,
        );
    }
    let disabled = template.map_or(Some("not available"), |tpl| state::disabled(app, cx.surface, &tpl.id));
    // Chips: one row (Simplified) or three rows of ≥ 100-px columns (Classic: Quick Steps 2, Arrange By 4,
    // a narrowed gallery 1).
    let (cols, rows) = if cx.simplified {
        (usize::MAX, 1usize)
    } else {
        (((body.width() - 6.0) / 100.0).floor().max(1.0) as usize, 3usize)
    };
    let chip_h = if cx.simplified { 28.0 } else { (body.height() - 6.0) / rows as f32 };
    let mut x = body.left() + 3.0;
    let mut placed = 0usize;
    for (i, item) in items.iter().enumerate() {
        let (cell, ok) = if cx.simplified {
            let w = small_w(ui, Some(&item.label), false, TEXT).min(140.0);
            let r = Rect::from_min_size(Pos2::new(x, body.center().y - chip_h / 2.0), Vec2::new(w, chip_h));
            x += w + 2.0;
            (r, r.right() <= body.right())
        } else {
            // Row-major, as observed (Date, From, To, Categories on the first row).
            let col = placed % cols;
            let row = placed / cols;
            let cw = (body.width() - 6.0) / cols as f32;
            let r = Rect::from_min_size(
                Pos2::new(body.left() + 3.0 + col as f32 * cw, body.top() + 3.0 + row as f32 * chip_h),
                Vec2::new(cw, chip_h),
            );
            (r, row < rows)
        };
        if !ok {
            break;
        }
        placed = placed.saturating_add(1);
        let shown = elide(ui, &item.label, cell.width() - 31.0);
        let look = Look {
            icon: item.icon,
            label: Some(&shown),
            large: false,
            arrow: false,
            on: item.on,
            enabled: disabled.is_none(),
            check: false,
        };
        let resp = if let Some([r, g, b]) = item.swatch {
            let resp = button(ui, app, id.with(i), cell, Look { icon: "", ..look }, &item.label, disabled);
            let sw =
                Rect::from_center_size(Pos2::new(cell.left() + 14.0, cell.center().y), Vec2::splat(12.0));
            ui.painter().rect_filled(sw, CornerRadius::same(2), Color32::from_rgb(r, g, b));
            resp
        } else {
            button(ui, app, id.with(i), cell, look, &item.label, disabled)
        };
        if resp.clicked()
            && let Some(tpl) = template
        {
            let act = state::action(app, tpl, item.on, None, &item.vars);
            run(app, cx, &act);
        }
    }
    if placed == 0 && !st.narrow {
        ui.painter().text(
            body.left_center() + Vec2::new(8.0, 0.0),
            Align2::LEFT_CENTER,
            &c.label,
            FontId::proportional(SMALL_TEXT),
            muted(t.text),
        );
    }
    let resp = button(
        ui,
        app,
        id.with("more"),
        arrow_r,
        Look { icon: "", label: None, large: false, arrow: true, on: false, enabled: true, check: false },
        &c.label,
        None,
    );
    honour_open_request(ui, cx, &c.key, &resp);
    show_menu(app, cx, &resp, c.menu.as_deref());
}

// ---------------------------------------------------------------------------------------------------------
// Simplified ribbon
// ---------------------------------------------------------------------------------------------------------

struct SItem {
    group: usize,
    order: usize,
    ctl: &'static Ctl,
    st: Style,
    w: f32,
}

/// Lays out and draws the Simplified ribbon row.
pub fn simplified(ui: &mut Ui, app: &mut App, cx: &mut Cx, rect: Rect) {
    let tab = cx.tab;
    let mut items: Vec<SItem> = Vec::new();
    let mut overflow: Vec<(usize, &'static Ctl)> = Vec::new();
    let mut order = 0usize;
    for (gi, g) in tab.groups.iter().enumerate() {
        let gacts = active(&g.rules, false, cx.width);
        for (ci, c) in g.ctls.iter().enumerate() {
            order = order.saturating_add(1);
            let cacts = active(&c.rules, false, cx.width);
            if gacts.contains(&Act::Overflow) || cacts.contains(&Act::Overflow) {
                overflow.push((order, c));
                continue;
            }
            let mut label = c.size != Size::Icon;
            if gacts.contains(&Act::NoLabel) || cacts.contains(&Act::NoLabel) {
                label = false;
            }
            if gacts.contains(&Act::NoLabelButFirst) && ci > 0 {
                label = false;
            }
            let narrow = gacts.contains(&Act::Narrow) || cacts.contains(&Act::Narrow);
            let st = Style { large: false, label, narrow };
            items.push(SItem { group: gi, order, ctl: c, st, w: ctl_width(ui, c, st, true) });
        }
    }
    let avail = rect.width() - 8.0 - MORE_W - DISPLAY_W - 12.0;
    let total = |items: &[SItem]| -> f32 {
        let mut t = 0.0;
        let mut last = None;
        for it in items {
            if last.is_some_and(|g| g != it.group) {
                t += 13.0;
            }
            t += it.w + 2.0;
            last = Some(it.group);
        }
        t
    };
    while total(&items) > avail {
        let Some(it) = items.pop() else { break };
        overflow.push((it.order, it.ctl));
    }
    overflow.sort_by_key(|(o, _)| *o);
    // A keytip aimed at a control that moved into the "…" menu opens that menu instead.
    if let Some(req) = cx.mem.open_request.as_deref()
        && overflow.iter().any(|(_, c)| c.key == req)
    {
        cx.mem.open_request = Some("00".to_owned());
    }

    let mut x = rect.left() + 8.0;
    let mut last_group = None;
    for it in &items {
        if last_group.is_some_and(|g| g != it.group) {
            let sx = x + 5.0;
            ui.painter().line_segment(
                [Pos2::new(sx, rect.center().y - 11.0), Pos2::new(sx, rect.center().y + 11.0)],
                Stroke::new(1.0, app.theme.separator),
            );
            x += 13.0;
        }
        let r = Rect::from_min_size(Pos2::new(x, rect.center().y - S_H / 2.0), Vec2::new(it.w, S_H));
        draw_ctl(ui, app, cx, it.ctl, r, it.st);
        x += it.w + 2.0;
        last_group = Some(it.group);
    }

    // "…" More Commands (always at the right end), then Ribbon Display Options.
    let more_r = Rect::from_min_size(
        Pos2::new(rect.right() - DISPLAY_W - 6.0 - MORE_W, rect.center().y - MORE_H / 2.0),
        Vec2::new(MORE_W, MORE_H),
    );
    let fixed = tab.more.as_deref().and_then(|m| defs().menu(m)).unwrap_or_default();
    let label = format!("More Commands, {}", tab.name);
    let resp = button(
        ui,
        app,
        cx.id.with(("more", tab.name.as_str())),
        more_r,
        Look {
            icon: "ellipsis",
            label: None,
            large: false,
            arrow: false,
            on: false,
            enabled: true,
            check: false,
        },
        &label,
        None,
    );
    badge(cx, more_r, "00");
    honour_open_request(ui, cx, "00", &resp);
    popup_for(&resp).show(|ui| {
        ui.set_min_width(220.0);
        for (_, c) in &overflow {
            ctl_in_menu(ui, app, cx, c);
        }
        let entries = state::expand_items(app, cx.surface, &cx.mem, fixed);
        if !overflow.is_empty() && !entries.is_empty() {
            menu_sep(ui, app);
        }
        menu_entries(ui, app, cx, &entries, 0);
        if overflow.is_empty() && entries.is_empty() {
            ui.label("No more commands");
        }
    });
    display_options(ui, app, cx, rect);
}

/// Ribbon Display Options (⌄ at the bottom-right corner).
fn display_options(ui: &mut Ui, app: &mut App, cx: &mut Cx, rect: Rect) {
    let r = Rect::from_min_size(
        Pos2::new(rect.right() - DISPLAY_W - 2.0, rect.bottom() - DISPLAY_W - 2.0),
        Vec2::splat(DISPLAY_W),
    );
    let resp = button(
        ui,
        app,
        cx.id.with("display-options"),
        r,
        Look {
            icon: "chevron-down",
            label: None,
            large: false,
            arrow: false,
            on: false,
            enabled: true,
            check: false,
        },
        "Ribbon Display Options",
        None,
    );
    let v = app.s.view.clone();
    let entries: Vec<(&str, &str, bool)> = vec![
        ("Simplified Ribbon", "simplified", v.simplified_ribbon),
        ("Classic Ribbon", "classic", !v.simplified_ribbon),
        ("", "", false),
        ("Full-screen mode", "autoHide", v.ribbon_auto_hide),
        ("Show tabs only", "tabsOnly", v.ribbon_collapsed && !v.ribbon_auto_hide),
        ("Always show Ribbon", "always", !v.ribbon_collapsed && !v.ribbon_auto_hide),
    ];
    popup_for(&resp).show(|ui| {
        ui.set_min_width(200.0);
        for (label, mode, on) in entries {
            if label.is_empty() {
                menu_sep(ui, app);
                continue;
            }
            if menu_row(ui, app, "", None, label, Some(on), true, false).clicked() {
                let act = Action {
                    cmd: "app.toggleRibbonMode".into(),
                    params: serde_json::json!({"mode": mode}),
                    flip: None,
                };
                run(app, cx, &act);
                ui.close();
            }
        }
    });
}

// ---------------------------------------------------------------------------------------------------------
// Classic ribbon
// ---------------------------------------------------------------------------------------------------------

enum Cell {
    Large(&'static Ctl, Style, f32),
    Column(Vec<(&'static Ctl, Style, f32)>),
    Rows(Vec<Vec<(&'static Ctl, Style, f32)>>),
}

impl Cell {
    fn width(&self) -> f32 {
        match self {
            Cell::Large(_, _, w) => *w,
            Cell::Column(v) => v.iter().map(|(_, _, w)| *w).fold(0.0, f32::max),
            Cell::Rows(rows) => {
                rows.iter().map(|r| r.iter().map(|(_, _, w)| *w + 1.0).sum::<f32>()).fold(0.0, f32::max)
            }
        }
    }
}

struct GroupLayout {
    group: &'static Group,
    collapsed: bool,
    cells: Vec<Cell>,
    width: f32,
}

fn layout_group(ui: &Ui, cx: &Cx, g: &'static Group, collapsed: bool) -> GroupLayout {
    let gacts = active(&g.rules, true, cx.width);
    let label_w = text_w(ui, &g.name, SMALL_TEXT) + if g.launch.is_some() { 22.0 } else { 10.0 };
    if collapsed {
        let w = large_w(ui, &g.name, true).max(52.0) + 8.0;
        return GroupLayout { group: g, collapsed, cells: Vec::new(), width: w };
    }
    let mut cells: Vec<Cell> = Vec::new();
    let style = |c: &Ctl| -> Style {
        let cacts = active(&c.rules, true, cx.width);
        let iconly = gacts.contains(&Act::IconOnly) || cacts.contains(&Act::IconOnly);
        let small = gacts.contains(&Act::Small) || cacts.contains(&Act::Small);
        let large = c.size == Size::Large && !iconly && !small;
        let nolabel = gacts.contains(&Act::NoLabel) || cacts.contains(&Act::NoLabel);
        let narrow = gacts.contains(&Act::Narrow) || cacts.contains(&Act::Narrow);
        let label = match c.size {
            Size::Icon => false,
            _ if iconly => false,
            _ if large => true,
            _ => !nolabel,
        };
        Style { large, label, narrow }
    };
    if g.rows {
        let mut rows: Vec<Vec<(&'static Ctl, Style, f32)>> = vec![Vec::new()];
        for c in &g.ctls {
            if c.row_break && rows.last().is_some_and(|r| !r.is_empty()) {
                rows.push(Vec::new());
            }
            let st = style(c);
            let st = Style { large: false, ..st };
            let w = ctl_width(ui, c, st, false);
            if let Some(r) = rows.last_mut() {
                r.push((c, st, w));
            }
        }
        cells.push(Cell::Rows(rows));
    } else {
        for c in &g.ctls {
            let st = style(c);
            let w = ctl_width(ui, c, st, false);
            if st.large || c.kind == Kind::Gallery {
                cells.push(Cell::Large(c, st, w));
            } else {
                match cells.last_mut() {
                    Some(Cell::Column(col)) if col.len() < 3 => col.push((c, st, w)),
                    _ => cells.push(Cell::Column(vec![(c, st, w)])),
                }
            }
        }
    }
    let content: f32 = cells.iter().map(|c| c.width() + 3.0).sum::<f32>() + 8.0;
    GroupLayout { group: g, collapsed, cells, width: content.max(label_w) }
}

fn draw_group(ui: &mut Ui, app: &mut App, cx: &mut Cx, gl: &GroupLayout, rect: Rect) {
    let g = gl.group;
    let top = rect.top() + 3.0;
    let content_h = rect.height() - C_LABEL - 4.0;
    if gl.collapsed {
        let r =
            Rect::from_min_size(Pos2::new(rect.left() + 4.0, top), Vec2::new(rect.width() - 8.0, content_h));
        let look = Look {
            icon: g.icon(),
            label: Some(&g.name),
            large: true,
            arrow: true,
            on: false,
            enabled: true,
            check: false,
        };
        let resp = button(
            ui,
            app,
            cx.id.with(("group", cx.tab.name.as_str(), g.name.as_str())),
            r,
            look,
            &g.name,
            None,
        );
        badge(cx, r, &g.key);
        honour_open_request(ui, cx, &g.key, &resp);
        let expanded = layout_group(ui, cx, g, false);
        popup_for(&resp).show(|ui| {
            let size = Vec2::new(expanded.width + 8.0, crate::theme::size::RIBBON_CLASSIC);
            let (pr, _) = ui.allocate_exact_size(size, Sense::hover());
            ui.painter().rect_filled(pr, CornerRadius::same(4), app.theme.ribbon);
            draw_group(ui, app, cx, &expanded, pr);
        });
    } else {
        let mut x = rect.left() + 4.0;
        for cell in &gl.cells {
            let w = cell.width();
            match cell {
                Cell::Large(c, st, _) => {
                    let h = if c.kind == Kind::Gallery { content_h - 4.0 } else { content_h };
                    let r = Rect::from_min_size(
                        Pos2::new(x, top + if c.kind == Kind::Gallery { 2.0 } else { 0.0 }),
                        Vec2::new(w, h),
                    );
                    draw_ctl(ui, app, cx, c, r, *st);
                }
                Cell::Column(col) => {
                    for (i, (c, st, _)) in col.iter().enumerate() {
                        let y = top + 2.0 + i as f32 * (C_ROW + C_ROW_GAP);
                        let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, C_ROW));
                        draw_ctl(ui, app, cx, c, r, *st);
                    }
                }
                Cell::Rows(rows) => {
                    for (ri, row) in rows.iter().enumerate() {
                        let y = top + 2.0 + ri as f32 * (C_ROW + C_ROW_GAP);
                        let mut rx = x;
                        for (c, st, cw) in row {
                            let r = Rect::from_min_size(Pos2::new(rx, y), Vec2::new(*cw, C_ROW));
                            draw_ctl(ui, app, cx, c, r, *st);
                            rx += cw + 1.0;
                        }
                    }
                }
            }
            x += w + 3.0;
        }
    }
    // Group label and dialog launcher.
    let ly = rect.bottom() - C_LABEL / 2.0 - 1.0;
    let color = app.theme.text_muted;
    ui.painter().text(
        Pos2::new(rect.center().x, ly),
        Align2::CENTER_CENTER,
        &g.name,
        FontId::proportional(SMALL_TEXT),
        color,
    );
    if let Some(launch) = &g.launch
        && !gl.collapsed
    {
        let r = Rect::from_center_size(Pos2::new(rect.right() - 9.0, ly), Vec2::splat(14.0));
        let why = state::disabled(app, cx.surface, &launch.id);
        let resp =
            ui.interact(r, cx.id.with(("launch", cx.tab.name.as_str(), g.name.as_str())), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(2), app.theme.hover);
        }
        let c = if why.is_none() { color } else { muted(color) };
        let p = ui.painter();
        let (a, b) = (r.min + Vec2::new(4.0, 4.0), r.max - Vec2::new(4.0, 4.0));
        p.line_segment([a, Pos2::new(b.x, a.y)], Stroke::new(1.0, c));
        p.line_segment([a, Pos2::new(a.x, b.y)], Stroke::new(1.0, c));
        p.line_segment([a + Vec2::new(1.5, 1.5), b], Stroke::new(1.0, c));
        let spec_label = app.s.registry.get(&launch.id).map(|s| s.label).unwrap_or_default();
        let resp = tooltip(resp, spec_label, why);
        if resp.clicked() && why.is_none() {
            let act = state::action(app, launch, false, None, &[]);
            run(app, cx, &act);
        }
    }
}

/// Lays out and draws the Classic ribbon (groups, collapse, horizontal scroll at ≤ 700).
pub fn classic(ui: &mut Ui, app: &mut App, cx: &mut Cx, rect: Rect) {
    let tab = cx.tab;
    let avail = rect.width() - 8.0 - DISPLAY_W - 4.0;
    let mut collapsed: Vec<bool> =
        tab.groups.iter().map(|g| active(&g.rules, true, cx.width).contains(&Act::Collapse)).collect();
    let lay = |ui: &Ui, cx: &Cx, collapsed: &[bool]| -> Vec<GroupLayout> {
        tab.groups.iter().zip(collapsed.iter()).map(|(g, c)| layout_group(ui, cx, g, *c)).collect()
    };
    let total = |ls: &[GroupLayout]| ls.iter().map(|l| l.width + 9.0).sum::<f32>();
    let mut layouts = lay(ui, cx, &collapsed);
    // Beyond the observed rules, groups collapse right-to-left until the row fits; at ≤ 700 the ribbon
    // scrolls instead.
    if total(&layouts) > avail && cx.width > 700.0 {
        for i in (0..collapsed.len()).rev() {
            if collapsed.get(i).copied().unwrap_or(true) {
                continue;
            }
            if let Some(c) = collapsed.get_mut(i) {
                *c = true;
            }
            layouts = lay(ui, cx, &collapsed);
            if total(&layouts) <= avail {
                break;
            }
        }
    }
    // A keytip aimed at a control inside a collapsed group opens the group's dropdown instead.
    if let Some(req) = cx.mem.open_request.clone()
        && let Some(g) = layouts.iter().find(|l| l.collapsed && l.group.ctls.iter().any(|c| c.key == req))
    {
        cx.mem.open_request = Some(g.group.key.clone());
    }
    let content_w = total(&layouts);
    let key = super::surface_key(cx.surface);
    let max_off = (content_w - avail).max(0.0);
    let mut off = cx.mem.scroll.get(&key).copied().unwrap_or(0.0).clamp(0.0, max_off);
    let body = Rect::from_min_max(rect.min, Pos2::new(rect.left() + avail + 4.0, rect.bottom()));
    if max_off > 0.0 {
        let hover = ui.rect_contains_pointer(body);
        if hover {
            let d = ui.input(|i| i.smooth_scroll_delta);
            off = (off - d.x - d.y).clamp(0.0, max_off);
        }
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
    child.set_clip_rect(body.intersect(ui.clip_rect()));
    let mut x = rect.left() + 4.0 - off;
    for (i, gl) in layouts.iter().enumerate() {
        let gr = Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(gl.width, rect.height()));
        draw_group(&mut child, app, cx, gl, gr);
        x += gl.width + 4.0;
        if i + 1 < layouts.len() {
            child.painter().line_segment(
                [Pos2::new(x, rect.top() + 6.0), Pos2::new(x, rect.bottom() - 6.0)],
                Stroke::new(1.0, app.theme.separator),
            );
        }
        x += 5.0;
    }
    if max_off > 0.0 {
        let step = (avail * 0.6).max(120.0);
        if off < max_off {
            let r = Rect::from_min_size(
                Pos2::new(body.right() - 18.0, rect.top() + 2.0),
                Vec2::new(18.0, rect.height() - 4.0),
            );
            ui.painter().rect_filled(r, CornerRadius::same(3), app.theme.ribbon);
            let resp = button(
                ui,
                app,
                cx.id.with("scroll-right"),
                r,
                Look {
                    icon: "chevron-right",
                    label: None,
                    large: false,
                    arrow: false,
                    on: false,
                    enabled: true,
                    check: false,
                },
                "Scroll right",
                None,
            );
            if resp.clicked() {
                off = (off + step).min(max_off);
            }
        }
        if off > 0.0 {
            let r = Rect::from_min_size(
                Pos2::new(rect.left(), rect.top() + 2.0),
                Vec2::new(18.0, rect.height() - 4.0),
            );
            ui.painter().rect_filled(r, CornerRadius::same(3), app.theme.ribbon);
            let resp = button(
                ui,
                app,
                cx.id.with("scroll-left"),
                r,
                Look {
                    icon: "chevron-left",
                    label: None,
                    large: false,
                    arrow: false,
                    on: false,
                    enabled: true,
                    check: false,
                },
                "Scroll left",
                None,
            );
            if resp.clicked() {
                off = (off - step).max(0.0);
            }
        }
    }
    cx.mem.scroll.insert(key, off);
    display_options(ui, app, cx, rect);
}
