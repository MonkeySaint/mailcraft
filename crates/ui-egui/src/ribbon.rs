//! Ribbon: data-driven definitions for every surface and tab, Simplified and Classic, width degradation and
//! keytips (ribbon area). Controls call commands by id via `App::run`.
//!
//! The definitions live in `ribbon/*.ribbon` (grammar at the top of `main.ribbon`), parsed once by
//! [`defs::defs`]. Surfaces: `main` (Mail; Home, Send / Receive, Folder (Classic only), View, Help), `search`
//! (contextual Search tab while `app.ui.search_active`), `calendar` / `people` / `tasks` / `notes` (module tabs,
//! falling back to the Mail tabs), `compose` (Message, Insert, Options, Format Text, Review, Help) and `read`
//! (Message, Help). The Simplified ribbon is one 49-px row with a "…" overflow menu; the Classic ribbon is
//! 100 px of labelled groups. Width degradation follows plan/reference/responsive.md.
//!
//! Helpers for other areas: [`surface_key`] (the `app.ui.ribbon_tab` key), [`selected_tab`] / [`select_tab`],
//! [`tab_keytip`] (badge text for the tab row) and [`keytip`] (resolves an Alt keytip sequence).

pub mod defs;
mod draw;
pub(crate) mod state;

use crate::responsive::Frame;
use crate::{App, Surface};
use mailcraft_engine::Module;

use defs::{Tab, defs};

/// Key of a surface in `app.ui.ribbon_tab` ("main", "compose:3", "read:42").
pub fn surface_key(surface: Surface) -> String {
    match surface {
        Surface::Main => "main".to_owned(),
        Surface::Compose(id) => format!("compose:{id}"),
        Surface::Read(id) => format!("read:{id}"),
    }
}

/// Definition surface for the current module ("calendar", …), if it has its own tabs.
fn module_surface(app: &App) -> Option<&'static str> {
    match app.s.view.module {
        Module::Calendar => Some("calendar"),
        Module::People => Some("people"),
        Module::Tasks => Some("tasks"),
        Module::Notes => Some("notes"),
        _ => None,
    }
}

/// Tab names for a surface in order (File is drawn separately as the backstage button).
pub fn tabs(app: &App, surface: Surface) -> Vec<&'static str> {
    let d = defs();
    let simplified = app.s.view.simplified_ribbon;
    match surface {
        Surface::Main => {
            let mut v = d.tab_names("main", simplified);
            if app.ui.search_active {
                v.extend(d.tab_names("search", simplified));
            }
            v
        }
        Surface::Compose(id) => {
            let mut v = d.tab_names("compose", simplified);
            if drawing_selected(app, id) {
                v.extend(d.tab_names("shapeformat", simplified));
            }
            v.push("Help");
            v
        }
        Surface::Read(_) => {
            let mut v = d.tab_names("read", simplified);
            v.push("Help");
            v
        }
    }
}

/// Whether compose `cid` has a drawing object selected (the Shape Format tab shows).
fn drawing_selected(app: &App, cid: u64) -> bool {
    app.s
        .composes
        .get(&cid)
        .is_some_and(|c| c.selected_drawing.is_some_and(|id| c.body.drawings.iter().any(|d| d.id == id)))
}

/// The definition drawn for a tab name on a surface (module and window fallbacks applied).
fn resolve_tab(app: &App, surface: Surface, name: &str) -> Option<&'static Tab> {
    let d = defs();
    let simplified = app.s.view.simplified_ribbon;
    let chain: Vec<&str> = match surface {
        Surface::Main if name == "Search" => vec!["search"],
        Surface::Main => module_surface(app).into_iter().chain(["main"]).collect(),
        Surface::Compose(_) if name == "Shape Format" => vec!["shapeformat"],
        Surface::Compose(_) => vec!["compose", "main"],
        Surface::Read(_) => vec!["read", "main"],
    };
    chain.into_iter().find_map(|s| d.tab(s, simplified, name))
}

/// The selected tab of a surface (the stored one when still visible, else the first).
pub fn selected_tab(app: &App, surface: Surface) -> &'static str {
    let names = tabs(app, surface);
    let stored = app.ui.ribbon_tab.get(&surface_key(surface));
    stored
        .and_then(|s| names.iter().copied().find(|n| *n == s.as_str()))
        .or_else(|| names.first().copied())
        .unwrap_or("Home")
}

/// Selects a tab (ignored when the surface has no such tab).
pub fn select_tab(app: &mut App, surface: Surface, name: &str) {
    if tabs(app, surface).contains(&name) {
        app.ui.ribbon_tab.insert(surface_key(surface), name.to_owned());
    }
}

/// Keytip of a tab ("H", "JS", "JS2"; "" when unknown).
pub fn tab_keytip(app: &App, surface: Surface, name: &str) -> &'static str {
    resolve_tab(app, surface, name).map(|t| t.key.as_str()).unwrap_or_default()
}

/// Width the degradation rules see: the main window uses the responsive steps when computed (their
/// observed widths), other windows their own width.
fn effective_width(app: &App, surface: Surface, frame: &Frame, avail: f32) -> f32 {
    let w = if frame.width > 0.0 { frame.width } else { avail };
    if surface != Surface::Main {
        return w;
    }
    if app.s.view.simplified_ribbon {
        match frame.simplified_step {
            1 => 1920.0,
            2 => 1600.0,
            3 => 1366.0,
            4 => 1280.0,
            5 => 1024.0,
            6 => 900.0,
            7.. => w.min(700.0),
            _ => w,
        }
    } else {
        match frame.classic_step {
            1 => 1920.0,
            2 => 1366.0,
            3 => 1024.0,
            4 => 900.0,
            5 => 800.0,
            6.. => w.min(700.0),
            _ => w,
        }
    }
}

/// Normalises a keytip sequence ("Alt, H, N 1" → "HN1").
fn norm_keys(seq: &str) -> String {
    let t = seq.trim_start();
    let t = match t.get(..3) {
        Some(p)
            if p.eq_ignore_ascii_case("alt")
                && t.get(3..).is_some_and(|r| r.trim_start().starts_with(',')) =>
        {
            t.get(3..).unwrap_or_default()
        }
        _ => t,
    };
    t.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

/// Draws the ribbon body for the surface's selected tab.
pub fn ribbon(ui: &mut egui::Ui, app: &mut App, surface: Surface, frame: &Frame) {
    let mut mem = state::load_mem(ui.ctx(), surface);
    // The Search tab comes forward when the search box becomes active.
    if surface == Surface::Main {
        let was = mem.flags.get("__searchActive").copied().unwrap_or(false);
        if app.ui.search_active && !was {
            select_tab(app, surface, "Search");
        }
        mem.flags.insert("__searchActive".to_owned(), app.ui.search_active);
    }
    // The Shape Format tab comes forward when a new drawing is put in and selected.
    if let Surface::Compose(cid) = surface
        && let Some(c) = app.s.composes.get(&cid)
    {
        let key = egui::Id::new(("mailcraft-ribbon-drawing-max", cid));
        let newest = c.body.drawings.iter().map(|d| d.id).max().unwrap_or(0);
        let seen = ui.ctx().data(|d| d.get_temp::<u32>(key));
        let fresh = seen.is_some_and(|s| newest > s) && c.selected_drawing == Some(newest);
        ui.ctx().data_mut(|d| d.insert_temp(key, newest.max(seen.unwrap_or(0))));
        if fresh {
            select_tab(app, surface, "Shape Format");
        }
    }
    let simplified = app.s.view.simplified_ribbon;
    let height =
        if simplified { crate::theme::size::RIBBON_SIMPLIFIED } else { crate::theme::size::RIBBON_CLASSIC };
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width().max(0.0), height), egui::Sense::hover());
    ui.painter().rect_filled(rect, egui::CornerRadius::same(6), app.theme.ribbon);
    let name = selected_tab(app, surface);
    let Some(tab) = resolve_tab(app, surface, name) else {
        state::store_mem(ui.ctx(), surface, mem);
        return;
    };
    let keytip = app.ui.keytips.as_deref().and_then(|seq| {
        let seq = norm_keys(seq);
        seq.strip_prefix(tab.key.as_str()).map(str::to_owned)
    });
    let mut cx = draw::Cx {
        ctx: ui.ctx().clone(),
        surface,
        simplified,
        width: effective_width(app, surface, frame, rect.width()),
        tab,
        keytip,
        mem,
        id: egui::Id::new(("mailcraft-ribbon", surface_key(surface))),
        badges: Vec::new(),
        hidden: hidden_commands(app),
    };
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    if simplified {
        draw::simplified(&mut child, app, &mut cx, rect);
    } else {
        draw::classic(&mut child, app, &mut cx, rect);
    }
    draw::paint_badges(&child, app, &cx);
    // A keytip request no drawn control took (its control is not on this tab or width) must not linger and
    // pop up later.
    cx.mem.open_request = None;
    state::store_mem(ui.ctx(), surface, cx.mem);
}

/// Commands an option takes off the ribbon: Accessibility › "Show Read Aloud" off hides Read Aloud.
pub fn hidden_commands(app: &App) -> Vec<&'static str> {
    let mut out = Vec::new();
    if !app.s.settings.bool("accessibility.readAloud") {
        out.push("review.readAloud");
    }
    out
}

/// Result of [`keytip`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeytipHit {
    /// Nothing matches the sequence.
    NoMatch,
    /// A prefix of one or more keytips; keep collecting keys.
    Partial,
    /// A tab was selected (keep keytip mode on to show its controls' badges).
    Tab(&'static str),
    /// A control was activated (command run or edit focused); keytip mode should end.
    Done,
    /// A dropdown opened (on the next frame); keytip mode stays on so its entries' keytips can be typed
    /// ("Alt, H, U" opens Follow Up, then "T" flags for Today).
    Menu,
}

/// Resolves the keytip keys typed after a dropdown's own keytip against its entries (submenus recurse).
/// Returns `None` when nothing matches.
fn menu_keytip(
    ctx: &egui::Context,
    app: &mut App,
    surface: Surface,
    menu: &str,
    rest: &str,
    depth: u8,
) -> Option<KeytipHit> {
    let items = defs().menu(menu)?;
    let mut partial = false;
    for it in items {
        if it.key.is_empty() || matches!(it.kind, defs::ItemKind::Sep | defs::ItemKind::Dyn) {
            continue;
        }
        if it.kind == defs::ItemKind::Sub && depth < 6 {
            if let Some(tail) = rest.strip_prefix(it.key.as_str()) {
                if tail.is_empty() {
                    return Some(KeytipHit::Menu);
                }
                if let Some(hit) = menu_keytip(ctx, app, surface, &it.target, tail, depth.saturating_add(1)) {
                    return Some(hit);
                }
            }
        } else if it.kind == defs::ItemKind::Cmd && it.key == rest {
            let mut mem = state::load_mem(ctx, surface);
            if state::disabled(app, surface, &it.cmd.id).is_none() {
                let on = it.on.as_deref().is_some_and(|s| state::is_on(app, surface, &mem, s));
                let mut act = state::action(app, &it.cmd, on, None, &[]);
                act.flip = it.on.as_deref().and_then(|s| s.strip_prefix("mem:")).map(str::to_owned);
                state::run(ctx, app, surface, &mut mem, &act);
            }
            mem.open_request = None;
            state::store_mem(ctx, surface, mem);
            egui::Popup::close_all(ctx);
            return Some(KeytipHit::Done);
        }
        if it.key.starts_with(rest) {
            partial = true;
        }
    }
    partial.then_some(KeytipHit::Partial)
}

/// Resolves an Alt keytip sequence typed so far ("H", "HN1", "JS", …): selects tabs, runs commands, opens
/// dropdowns (on the next frame) and focuses edits. Intended for the keys area.
pub fn keytip(ctx: &egui::Context, app: &mut App, surface: Surface, seq: &str) -> KeytipHit {
    let seq = norm_keys(seq);
    if seq.is_empty() {
        return KeytipHit::Partial;
    }
    let names = tabs(app, surface);
    let tab_keys: Vec<(&'static str, &'static str)> =
        names.iter().map(|n| (*n, tab_keytip(app, surface, n))).filter(|(_, k)| !k.is_empty()).collect();
    if let Some((name, _)) = tab_keys.iter().find(|(_, k)| *k == seq) {
        select_tab(app, surface, name);
        return KeytipHit::Tab(name);
    }
    let longer_tab = tab_keys.iter().any(|(_, k)| k.starts_with(seq.as_str()));
    // The selected tab first, then any tab whose key prefixes the sequence.
    let current = selected_tab(app, surface);
    let mut owners: Vec<(&'static str, &'static str)> =
        tab_keys.iter().copied().filter(|(_, k)| seq.starts_with(k)).collect();
    owners.sort_by_key(|(n, k)| (*n != current, std::cmp::Reverse(k.len())));
    for (name, key) in owners {
        let Some(tab) = resolve_tab(app, surface, name) else { continue };
        let rest = seq.get(key.len()..).unwrap_or_default();
        if rest.is_empty() {
            continue;
        }
        let mut mem = state::load_mem(ctx, surface);
        let simplified = app.s.view.simplified_ribbon;
        let hidden = hidden_commands(app);
        let exact = tab
            .groups
            .iter()
            .flat_map(|g| g.ctls.iter())
            .filter(|c| !hidden.iter().any(|h| *h == c.cmd.id))
            .find(|c| c.key == rest);
        if let Some(c) = exact {
            select_tab(app, surface, name);
            // Split buttons open their dropdown (plan/reference/ribbon-*.md: "Alt, H, R 1" opens Report's menu,
            // "Alt, H, V" Paste's); dropdowns, galleries and combos open theirs; edits take focus.
            let opens_menu = match c.kind {
                defs::Kind::Split => c.menu.is_some(),
                defs::Kind::Drop | defs::Kind::Gallery | defs::Kind::Combo => true,
                _ => false,
            };
            if opens_menu {
                mem.open_request = Some(c.key.clone());
                state::store_mem(ctx, surface, mem);
                return KeytipHit::Menu;
            }
            match c.kind {
                defs::Kind::Button | defs::Kind::Toggle | defs::Kind::Check | defs::Kind::Split
                    if !c.cmd.id.is_empty() =>
                {
                    if state::disabled(app, surface, &c.cmd.id).is_none() {
                        let on = c.on.as_deref().is_some_and(|s| state::is_on(app, surface, &mem, s));
                        let cmd = match (&c.oncmd, on) {
                            (Some(alt), true) => alt,
                            _ => &c.cmd,
                        };
                        let mut act = state::action(app, cmd, on, None, &[]);
                        act.flip = c.on.as_deref().and_then(|s| s.strip_prefix("mem:")).map(str::to_owned);
                        state::run(ctx, app, surface, &mut mem, &act);
                    }
                }
                _ => mem.open_request = Some(c.key.clone()),
            }
            state::store_mem(ctx, surface, mem);
            return KeytipHit::Done;
        }
        // Group keytips open collapsed groups (Classic); Simplified groups never collapse, and their keys
        // ("R" for Report) only prefix control keys ("R P" Reply).
        let group = tab.groups.iter().find(|g| g.key == rest && !simplified);
        if group.is_some() || (simplified && rest == "00") {
            select_tab(app, surface, name);
            mem.open_request = Some(rest.to_owned());
            state::store_mem(ctx, surface, mem);
            return KeytipHit::Menu;
        }
        // Keys typed after a dropdown's keytip pick its entries ("Alt, H, U, T"; "Alt, H, 0 0, F").
        let mut menu_owners: Vec<(&str, &str)> = tab
            .groups
            .iter()
            .flat_map(|g| g.ctls.iter())
            .filter(|c| !c.key.is_empty() && !matches!(c.kind, defs::Kind::Button | defs::Kind::Edit))
            .filter_map(|c| c.menu.as_deref().map(|m| (c.key.as_str(), m)))
            .collect();
        if simplified && let Some(more) = tab.more.as_deref() {
            menu_owners.push(("00", more));
        }
        let mut menu_partial = false;
        for (key, menu) in menu_owners {
            let Some(tail) = rest.strip_prefix(key).filter(|t| !t.is_empty()) else { continue };
            match menu_keytip(ctx, app, surface, menu, tail, 0) {
                Some(KeytipHit::Partial) => menu_partial = true,
                Some(hit) => {
                    select_tab(app, surface, name);
                    return hit;
                }
                None => {}
            }
        }
        let partial = tab
            .groups
            .iter()
            .flat_map(|g| {
                let group_key = if simplified { "" } else { g.key.as_str() };
                std::iter::once(group_key).chain(g.ctls.iter().map(|c| c.key.as_str()))
            })
            .any(|k| !k.is_empty() && k.starts_with(rest));
        // "0 0" (More Commands) is typed one key at a time.
        let more_partial = simplified && "00".starts_with(rest);
        if partial || menu_partial || more_partial {
            return KeytipHit::Partial;
        }
    }
    if longer_tab { KeytipHit::Partial } else { KeytipHit::NoMatch }
}

#[cfg(test)]
mod tests;
