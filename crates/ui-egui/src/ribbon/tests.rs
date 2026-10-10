//! Ribbon definition checks and kittest rendering of both ribbon modes.

use std::collections::BTreeSet;

use super::defs::{CmdRef, ItemKind, Kind, defs};
use super::*;
use crate::App;

/// Shared icon names (assets/icons, drawn by the icons area).
const ICONS: &[&str] = &[
    "mail",
    "calendar",
    "people",
    "tasks",
    "notes",
    "folders",
    "shortcuts",
    "more-apps",
    "new-mail",
    "new-items",
    "delete",
    "archive",
    "ignore",
    "clean-up",
    "junk",
    "block",
    "reply",
    "reply-all",
    "forward",
    "meeting",
    "more",
    "move",
    "rules",
    "copy",
    "categorize",
    "flag",
    "flag-complete",
    "unread",
    "read",
    "search",
    "filter",
    "sort",
    "sort-up",
    "sort-down",
    "inbox",
    "drafts",
    "sent",
    "deleted",
    "junk-folder",
    "outbox",
    "archive-folder",
    "rss",
    "search-folder",
    "folder",
    "folder-open",
    "favorites",
    "account",
    "attach",
    "paperclip",
    "importance-high",
    "importance-low",
    "replied",
    "forwarded",
    "draft",
    "send",
    "save",
    "undo",
    "redo",
    "sync",
    "settings",
    "help",
    "print",
    "close",
    "minimize",
    "maximize",
    "restore",
    "chevron-down",
    "chevron-up",
    "chevron-left",
    "chevron-right",
    "back",
    "check",
    "plus",
    "minus",
    "zoom-in",
    "zoom-out",
    "address-book",
    "check-names",
    "signature",
    "link",
    "picture",
    "table",
    "bold",
    "italic",
    "underline",
    "strike",
    "font-color",
    "highlight",
    "bullets",
    "numbering",
    "indent",
    "outdent",
    "align-left",
    "align-center",
    "align-right",
    "justify",
    "paste",
    "cut",
    "format-painter",
    "spelling",
    "reading-pane",
    "folder-pane",
    "view",
    "conversation",
    "quick-steps",
    "person",
    "contact-group",
    "appointment",
    "task",
    "note",
    "today",
    "day",
    "work-week",
    "week",
    "month",
    "schedule",
    "info",
    "warning",
    "error",
    "pin",
    "overflow",
    "collapse-ribbon",
    "ribbon-display",
    "envelope-open",
    "bell",
    "calendar-reminder",
    "location",
    "private",
    "recurrence",
    "category-tag",
    "phone",
    "email",
    "ellipsis",
];

/// Icons the drawing code uses directly.
const CODE_ICONS: &[&str] = &["check", "chevron-right", "chevron-left", "chevron-down", "ellipsis"];

/// Every command reference in the definitions, with where it came from.
fn all_cmds() -> Vec<(String, CmdRef)> {
    let d = defs();
    let mut out = Vec::new();
    for t in &d.tabs {
        for g in &t.groups {
            if let Some(l) = &g.launch {
                out.push((format!("{}/{}/{} launcher", t.surface, t.name, g.name), l.clone()));
            }
            for c in &g.ctls {
                let at = format!("{}/{}/{}/{}", t.surface, t.name, g.name, c.label);
                if !c.cmd.id.is_empty() {
                    out.push((at.clone(), c.cmd.clone()));
                }
                if let Some(o) = &c.oncmd {
                    out.push((format!("{at} (on)"), o.clone()));
                }
            }
        }
    }
    for (name, items) in &d.menus {
        for it in items {
            if matches!(it.kind, ItemKind::Cmd | ItemKind::Dyn) {
                out.push((format!("menu {name}/{}", it.label), it.cmd.clone()));
            }
        }
    }
    out
}

#[test]
fn definitions_parse_cleanly() {
    let d = defs();
    assert!(d.errors.is_empty(), "ribbon definition errors:\n{}", d.errors.join("\n"));
    assert!(d.tabs.len() > 20);
    assert!(d.menus.len() > 50);
}

#[test]
fn every_command_id_exists_in_the_registry() {
    let reg = mailcraft_engine::cmd::registry();
    let mut missing = Vec::new();
    for (at, c) in all_cmds() {
        if reg.get(&c.id).is_none() {
            missing.push(format!("{at}: {}", c.id));
        }
    }
    assert!(reg.get("app.toggleRibbonMode").is_some());
    assert!(missing.is_empty(), "unknown command ids:\n{}", missing.join("\n"));
}

#[test]
fn every_command_has_a_mapped_catalog_counterpart_or_is_known() {
    // Every id we use is one the Outlook feature catalog maps somewhere (or one of our own few additions).
    let catalog: BTreeSet<&str> =
        mailcraft_engine::catalog::catalog_rows().iter().filter_map(|r| r.id).collect();
    // Module commands act on the selected module item (the catalog maps some of these to mail.* ids).
    let ours = ["help.about", "app.toggleRibbonMode", "rules.alwaysMoveFrom", "mail.addReminder"];
    let module = |id: &str| ["people.", "tasks.", "notes.", "calendar."].iter().any(|p| id.starts_with(p));
    let mut odd = Vec::new();
    for (at, c) in all_cmds() {
        if !catalog.contains(c.id.as_str()) && !ours.contains(&c.id.as_str()) && !module(&c.id) {
            odd.push(format!("{at}: {}", c.id));
        }
    }
    assert!(odd.is_empty(), "ids not in the catalog:\n{}", odd.join("\n"));
}

#[test]
fn every_params_template_is_json() {
    for (at, c) in all_cmds() {
        if !c.params.trim().is_empty() {
            let v: Result<serde_json::Value, _> = serde_json::from_str(&c.params);
            assert!(v.as_ref().is_ok_and(serde_json::Value::is_object), "{at}: bad params {}", c.params);
        }
    }
}

#[test]
fn every_icon_is_from_the_shared_list() {
    let d = defs();
    let ok = |i: &str| i.is_empty() || ICONS.contains(&i);
    let mut bad = Vec::new();
    for t in &d.tabs {
        for g in &t.groups {
            if !ICONS.contains(&g.icon()) {
                bad.push(format!("group {}: {}", g.name, g.icon()));
            }
            for c in &g.ctls {
                if !ok(&c.icon) {
                    bad.push(format!("{}/{}: {}", t.name, c.label, c.icon));
                }
            }
        }
    }
    for (name, items) in &d.menus {
        for it in items {
            if !ok(&it.icon) {
                bad.push(format!("menu {name}/{}: {}", it.label, it.icon));
            }
        }
    }
    for i in CODE_ICONS {
        assert!(ICONS.contains(i), "{i}");
    }
    // Dynamic items use fixed icons too.
    let app = App::new(mailcraft_engine::sample::sample_session());
    for src in state::SOURCES {
        for item in state::dyn_items(&app, Surface::Main, src) {
            if !ok(item.icon) {
                bad.push(format!("dyn {src}: {}", item.icon));
            }
        }
    }
    assert!(bad.is_empty(), "icons outside the shared list:\n{}", bad.join("\n"));
}

#[test]
fn menus_sources_and_rules_resolve() {
    let d = defs();
    let mut bad = Vec::new();
    let has_menu = |m: &str| d.menus.contains_key(m);
    for t in &d.tabs {
        if let Some(m) = &t.more
            && !has_menu(m)
        {
            bad.push(format!("tab {}: more={m}", t.name));
        }
        for g in &t.groups {
            for c in &g.ctls {
                if let Some(m) = &c.menu
                    && !has_menu(m)
                {
                    bad.push(format!("{}: menu={m}", c.label));
                }
                if matches!(c.kind, Kind::Drop | Kind::Split | Kind::Gallery | Kind::Combo)
                    && c.menu.is_none()
                {
                    bad.push(format!("{}: {:?} without a menu", c.label, c.kind));
                }
                if let Some(s) = &c.source
                    && !state::SOURCES.contains(&s.as_str())
                {
                    bad.push(format!("{}: dyn={s}", c.label));
                }
                if c.kind == Kind::Gallery && c.source.is_some() && super::draw::gallery_template(c).is_none()
                {
                    bad.push(format!("{}: gallery menu lacks a dyn line for its source", c.label));
                }
            }
        }
    }
    for (name, items) in &d.menus {
        for it in items {
            if it.kind == ItemKind::Sub && !has_menu(&it.target) {
                bad.push(format!("menu {name}: sub {}", it.target));
            }
            if it.kind == ItemKind::Dyn && !state::SOURCES.contains(&it.target.as_str()) {
                bad.push(format!("menu {name}: dyn {}", it.target));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn control_keytips_are_unique_within_a_tab() {
    for t in &defs().tabs {
        let mut seen = BTreeSet::new();
        for g in &t.groups {
            for c in &g.ctls {
                if !c.key.is_empty() {
                    assert!(
                        seen.insert(c.key.clone()),
                        "{}/{} ({:?}): duplicate keytip {}",
                        t.surface,
                        t.name,
                        t.mode,
                        c.key
                    );
                }
            }
        }
    }
}

#[test]
fn tabs_per_surface_and_mode() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    app.s.view.simplified_ribbon = true;
    assert_eq!(tabs(&app, Surface::Main), ["Home", "Send / Receive", "View", "Help"]);
    app.s.view.simplified_ribbon = false;
    assert_eq!(tabs(&app, Surface::Main), ["Home", "Send / Receive", "Folder", "View", "Help"]);
    app.ui.search_active = true;
    assert_eq!(tabs(&app, Surface::Main).last(), Some(&"Search"));
    assert_eq!(
        tabs(&app, Surface::Compose(1)),
        ["Message", "Insert", "Options", "Format Text", "Review", "Help"]
    );
    app.s.view.simplified_ribbon = true;
    assert_eq!(
        tabs(&app, Surface::Compose(1)),
        ["Message", "Insert", "Options", "Format Text", "Review", "Help"]
    );
    assert_eq!(tabs(&app, Surface::Read(1)), ["Message", "Help"]);
    // Module Home tabs replace the Mail one.
    app.s.view.module = mailcraft_engine::Module::Calendar;
    let home = resolve_tab(&app, Surface::Main, "Home").unwrap();
    assert_eq!(home.surface, "calendar");
    assert_eq!(resolve_tab(&app, Surface::Main, "Help").unwrap().surface, "main");
    assert_eq!(tab_keytip(&app, Surface::Main, "Send / Receive"), "JS");
}

#[test]
fn toggles_reflect_state() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let mem = state::Mem::default();
    assert!(state::is_on(&app, Surface::Main, &mem, "rp:right"));
    app.run("view.readingPane", serde_json::json!({"position": "bottom"}));
    assert!(state::is_on(&app, Surface::Main, &mem, "rp:bottom"));
    assert!(!state::is_on(&app, Surface::Main, &mem, "rp:right"));
    let conv = state::is_on(&app, Surface::Main, &mem, "conversations");
    app.run("view.conversations", serde_json::Value::Null);
    assert_ne!(conv, state::is_on(&app, Surface::Main, &mem, "conversations"));
    // mem flags: plain flip and radio families.
    let mut mem = state::Mem::default();
    assert!(state::is_on(&app, Surface::Main, &mem, "mem:timeScale.30=1"));
    state::flip_mem(&mut mem, "timeScale.15");
    assert!(state::is_on(&app, Surface::Main, &mem, "mem:timeScale.15"));
    assert!(!state::is_on(&app, Surface::Main, &mem, "mem:timeScale.30=1"));
    state::flip_mem(&mut mem, "offline");
    assert!(state::is_on(&app, Surface::Main, &mem, "mem:offline"));
}

#[test]
fn params_templates_fill() {
    let app = App::new(mailcraft_engine::sample::sample_session());
    let v = state::fill(
        r#"{"on":"$!on","n":"$value","x":"$id"}"#,
        true,
        Some("11"),
        &[("$id", serde_json::json!(7))],
        &app,
    );
    assert_eq!(v, serde_json::json!({"on": false, "n": 11.0, "x": 7}));
    let v = state::with_context(&app, Surface::Compose(3), "format.bold", serde_json::Value::Null);
    assert_eq!(v, serde_json::json!({"compose": 3}));
    let v = state::with_context(&app, Surface::Read(9), "mail.delete", serde_json::json!({}));
    assert_eq!(v, serde_json::json!({"ids": [9]}));
}

#[test]
fn keytips_select_tabs_and_run_commands() {
    let ctx = egui::Context::default();
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    app.s.view.simplified_ribbon = false;
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "V"), KeytipHit::Tab("View"));
    assert_eq!(selected_tab(&app, Surface::Main), "View");
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "J"), KeytipHit::Partial);
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "QQ"), KeytipHit::NoMatch);
    let before = state::is_on(&app, Surface::Main, &state::Mem::default(), "reverse");
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "Alt, V, R S"), KeytipHit::Done);
    assert_ne!(before, state::is_on(&app, Surface::Main, &state::Mem::default(), "reverse"));
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "VP"), KeytipHit::Partial);
    // A dropdown's keytip opens it and keeps keytip mode on for its entries.
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "VPN"), KeytipHit::Menu);
    assert_eq!(state::load_mem(&ctx, Surface::Main).open_request.as_deref(), Some("PN"));
    // "Alt, V, P N, B": Reading Pane › Bottom.
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "VPNB"), KeytipHit::Done);
    assert_eq!(app.reading_pane_pos(), mailcraft_model::ReadingPanePos::Bottom);
    assert_eq!(state::load_mem(&ctx, Surface::Main).open_request, None);
}

#[test]
fn keytips_reach_dropdown_entries_and_split_menus() {
    let ctx = egui::Context::default();
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let first = *app.s.mailbox.mail.keys().next().unwrap();
    app.run("nav.selectItem", serde_json::json!({"id": first.0}));
    app.run("mail.flag", serde_json::json!({"kind": "clear"}));
    // Simplified: "R" is the Report group's key but only prefixes Reply (R P) etc.
    app.s.view.simplified_ribbon = true;
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "HR"), KeytipHit::Partial);
    // Split buttons open their menu ("Alt, H, D" → Delete's More Options) instead of running.
    let count = app.s.mailbox.mail.len();
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "HD"), KeytipHit::Menu);
    assert_eq!(app.s.mailbox.mail.get(&first).map(|m| m.folder), Some(app.s.selection.folder));
    assert_eq!(app.s.mailbox.mail.len(), count);
    // "Alt, H, U, T": Follow Up › Today.
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "HU"), KeytipHit::Menu);
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "HUT"), KeytipHit::Done);
    assert!(matches!(
        app.s.mailbox.mail.get(&first).map(|m| &m.flag),
        Some(mailcraft_model::FlagState::Flagged { .. })
    ));
    // Submenus: "Alt, H, N, U, P" = New Email › E-mail Message Using › Plain Text.
    let composes = app.s.composes.len();
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "HNU"), KeytipHit::Menu);
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "HNUP"), KeytipHit::Done);
    assert_eq!(app.s.composes.len(), composes + 1);
    // The "…" menu: "Alt, H, 0 0" opens it; its fixed entries take keys after it.
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "H0"), KeytipHit::Partial);
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "H00"), KeytipHit::Menu);
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "H00S"), KeytipHit::Menu);
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "H00Q"), KeytipHit::NoMatch);
    // Classic: group keytips still open collapsed groups.
    app.s.view.simplified_ribbon = false;
    assert_eq!(keytip(&ctx, &mut app, Surface::Main, "HZT"), KeytipHit::Menu);
    assert_eq!(state::load_mem(&ctx, Surface::Main).open_request.as_deref(), Some("ZT"));
}

#[test]
fn read_window_keytips_follow_the_observed_ribbon() {
    let ctx = egui::Context::default();
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    app.s.view.simplified_ribbon = true;
    let folder = app.s.selection.folder;
    let item = app.s.mailbox.mail.values().find(|m| m.folder == folder).map(|m| m.id.0).unwrap();
    let surface = Surface::Read(item);
    // uia/read-window-ribbon.txt: Mark Unread "U", Follow Up "W", Archive "O 1", Zoom "Q 1".
    assert_eq!(keytip(&ctx, &mut app, surface, "HW"), KeytipHit::Menu);
    assert_eq!(state::load_mem(&ctx, surface).open_request.as_deref(), Some("W"));
    let read = app.s.mailbox.mail.get(&mailcraft_model::ItemId(item)).map(|m| m.read);
    assert_eq!(keytip(&ctx, &mut app, surface, "HU"), KeytipHit::Done);
    assert_ne!(app.s.mailbox.mail.get(&mailcraft_model::ItemId(item)).map(|m| m.read), read);
    let d = defs();
    let tab = d.tab("read", true, "Message").unwrap();
    let keys: Vec<&str> = tab.groups.iter().flat_map(|g| g.ctls.iter()).map(|c| c.key.as_str()).collect();
    for k in ["O1", "Q1", "U", "W", "OP", "FD", "R1", "T"] {
        assert!(keys.contains(&k), "read window keytip {k}");
    }
}

#[test]
fn keytip_for_an_overflowed_control_opens_the_more_menu() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    app.s.view.simplified_ribbon = true;
    let mut h = harness(app, Surface::Main, 600.0);
    h.run();
    // At 600 the Tags group lives in "…"; "Alt, H, G" (Categorize) opens that menu instead.
    let ctx = h.ctx.clone();
    assert_eq!(keytip(&ctx, h.state_mut(), Surface::Main, "HG"), KeytipHit::Menu);
    h.run();
    assert!(egui::Popup::is_any_open(&ctx), "the More Commands menu opens");
    assert_eq!(state::load_mem(&ctx, Surface::Main).open_request, None);
}

fn harness(app: App, surface: Surface, width: f32) -> egui_kittest::Harness<'static, App> {
    let frame = crate::responsive::Frame { width, height: 800.0, ..Default::default() };
    egui_kittest::Harness::builder().with_size(egui::vec2(width, 140.0)).build_ui_state(
        move |ui, app: &mut App| {
            ribbon(ui, app, surface, &frame);
        },
        app,
    )
}

#[test]
fn both_ribbon_modes_render() {
    use egui_kittest::kittest::Queryable;
    for simplified in [true, false] {
        for width in [1920.0, 1024.0, 640.0] {
            let mut app = App::new(mailcraft_engine::sample::sample_session());
            app.s.view.simplified_ribbon = simplified;
            app.ui.keytips = Some("H".into());
            let mut h = harness(app, Surface::Main, width);
            h.run();
            // Reply is visible (as an icon-only button at narrow widths it keeps its label for tooltips).
            assert!(
                h.query_by_label("Reply").is_some(),
                "Reply missing (simplified={simplified}, w={width})"
            );
            if simplified {
                assert!(h.query_by_label("More Commands, Home").is_some());
            }
            h.state_mut().ui.ribbon_tab.insert("main".into(), "View".into());
            h.run();
            if width >= 1024.0 {
                // Classic groups that do not fit collapse into one dropdown named after the group.
                let shown =
                    h.query_by_label("Reverse Sort").is_some() || h.query_by_label("Arrangement").is_some();
                assert!(shown, "View tab (simplified={simplified}, w={width})");
            } else if simplified {
                // Narrow: what does not fit moves into the "…" menu.
                assert!(h.query_by_label("More Commands, View").is_some());
            }
        }
    }
}

#[test]
fn compose_read_search_and_module_ribbons_render() {
    use egui_kittest::kittest::Queryable;
    for simplified in [true, false] {
        let mut app = App::new(mailcraft_engine::sample::sample_session());
        app.s.view.simplified_ribbon = simplified;
        let cid =
            app.run("mail.newEmail", serde_json::Value::Null).and_then(|v| v["compose"].as_u64()).unwrap();
        let item = app.s.mailbox.mail.keys().next().unwrap().0;
        let mut h = harness(app, Surface::Compose(cid), 1400.0);
        h.run();
        assert!(h.query_by_label("Paste").is_some());
        for tab in ["Insert", "Options", "Format Text", "Review", "Help"] {
            h.state_mut().ui.ribbon_tab.insert(format!("compose:{cid}"), tab.into());
            h.run();
        }
        let app = std::mem::replace(h.state_mut(), App::new(mailcraft_engine::sample::sample_session()));
        drop(h);
        let mut h = harness(app, Surface::Read(item), 1200.0);
        h.run();
        assert!(h.query_by_label("Forward").is_some());
        let mut app = std::mem::replace(h.state_mut(), App::new(mailcraft_engine::sample::sample_session()));
        drop(h);
        app.ui.search_active = true;
        for module in [
            mailcraft_engine::Module::Mail,
            mailcraft_engine::Module::Calendar,
            mailcraft_engine::Module::People,
            mailcraft_engine::Module::Tasks,
            mailcraft_engine::Module::Notes,
        ] {
            app.s.view.module = module;
            let mut h = harness(app, Surface::Main, 1600.0);
            h.run();
            if module == mailcraft_engine::Module::Mail {
                // The Search tab comes forward when search becomes active.
                assert!(h.query_by_label("Has Attachments").is_some());
                h.state_mut().ui.ribbon_tab.insert("main".into(), "Home".into());
            }
            h.run();
            app = std::mem::replace(h.state_mut(), App::new(mailcraft_engine::sample::sample_session()));
        }
    }
}
