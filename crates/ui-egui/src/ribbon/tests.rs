//! Ribbon definition checks and kittest rendering of both ribbon modes.

use std::collections::BTreeSet;

use super::defs::{CmdRef, ItemKind, Kind, defs};
use super::*;
use crate::App;

/// Shared icon names (assets/icons, drawn by the icons area).
const ICONS: &[&str] = &[
    "certificate",
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
    // Every id we use is one the feature catalog maps somewhere (or one of our own few additions).
    let catalog: BTreeSet<&str> =
        mailcraft_engine::catalog::catalog_rows().iter().filter_map(|r| r.id).collect();
    // Module commands act on the selected module item (the catalog maps some of these to mail.* ids).
    // Sign sits next to Encrypt once S/MIME is set up; the observed window had no digital ID to show it.
    let ours =
        ["help.about", "app.toggleRibbonMode", "rules.alwaysMoveFrom", "mail.addReminder", "compose.sign"];
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
fn read_window_quick_steps_act_on_the_window_message() {
    use mailcraft_model::ItemId;
    let ctx = egui::Context::default();
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let inbox = app.s.selection.folder;
    let items: Vec<ItemId> = app.s.mailbox.items_in(inbox).iter().map(|m| m.id).collect();
    let (selected, open) = (items[0], items[1]);
    app.s.selection.items = vec![selected];
    app.s.selection.focus = Some(selected);
    let v = state::with_context(&app, Surface::Read(open.0), "quicksteps.run", serde_json::json!({"id": 4}));
    assert_eq!(v["ids"], serde_json::json!([open.0]));
    // Done (step 4) after its folder is chosen: the window's message moves, the list selection stays.
    let done = app.s.mailbox.quick_steps.iter().find(|q| q.name == "Done").map(|q| q.id).unwrap();
    let archive = app
        .s
        .mailbox
        .folders
        .values()
        .find(|f| f.role == Some(mailcraft_model::FolderRole::Archive))
        .map(|f| f.id)
        .unwrap();
    let act = state::Action {
        cmd: "quicksteps.run".into(),
        params: serde_json::json!({"id": done, "folder": archive.0}),
        flip: None,
    };
    let mut mem = state::Mem::default();
    state::run(&ctx, &mut app, Surface::Read(open.0), &mut mem, &act);
    assert_eq!(app.s.mailbox.mail[&open].folder, archive);
    assert_eq!(app.s.mailbox.mail[&selected].folder, inbox);
    // Reply & Delete is greyed out for a multi-selection in the main window, not in a read window.
    app.s.selection.items = items.iter().take(2).copied().collect();
    let reply = app.s.mailbox.quick_steps.iter().find(|q| q.name == "Reply & Delete").map(|q| q.id).unwrap();
    let blocked = |app: &App, surface| {
        let vars = [("$id", serde_json::json!(reply))];
        state::quick_step_blocked(app, surface, &vars)
    };
    assert!(blocked(&app, Surface::Main).is_some());
    assert!(blocked(&app, Surface::Read(selected.0)).is_none());
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

/// View › Show as Conversations asks This folder / All mailboxes before it changes anything
/// (`plan/reference/reply-forward.md` › Conversation view), like the list's Current View menu.
#[test]
fn show_as_conversations_asks_for_the_scope() {
    use egui_kittest::kittest::Queryable;
    for simplified in [false, true] {
        let mut app = App::new(mailcraft_engine::sample::sample_session());
        app.s.view.simplified_ribbon = simplified;
        app.ui.ribbon_tab.insert("main".into(), "View".into());
        let folder = app.s.selection.folder;
        let before = mailcraft_engine::queries::view_settings(&app.s, folder).conversations;
        let mut h = harness(app, Surface::Main, 1920.0);
        h.run();
        if simplified {
            // Simplified: View › Current View ▾ › Show as Conversations.
            h.get_by_label("Current View").click();
            h.run();
        }
        h.get_by_label("Show as Conversations").click();
        h.run();
        let app = h.state();
        assert_eq!(
            mailcraft_engine::queries::view_settings(&app.s, folder).conversations,
            before,
            "nothing changes before the answer (simplified={simplified})"
        );
        assert!(crate::list::conversation_scope_pending(&h.ctx), "the scope question is up ({simplified})");
    }
}

/// View › Conversation Settings shows the selected folder's settings as the engine keeps them (also after
/// they change elsewhere, e.g. from the list's context menu or in another folder), not remembered clicks.
#[test]
fn conversation_settings_menu_follows_the_engine() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    app.run("view.conversations", serde_json::json!({"on": true, "scope": "all"}));
    let mem = state::Mem::default();
    let items = defs().menu("conversationSettings").unwrap();
    let checks = |app: &App| -> Vec<(String, Option<bool>)> {
        state::expand_items(app, Surface::Main, &mem, items)
            .into_iter()
            .filter_map(|r| match r {
                state::Resolved::Cmd { label, on, .. } => Some((label, on)),
                _ => None,
            })
            .collect()
    };
    let on_of = |app: &App, label: &str| checks(app).into_iter().find(|(l, _)| l == label).and_then(|c| c.1);
    assert_eq!(on_of(&app, "Show Messages from Other Folders"), Some(true));
    assert_eq!(on_of(&app, "Use Classic Indented View"), Some(false));
    // Changed outside the ribbon: the menu follows.
    app.run("view.conversationSettings", serde_json::json!({"setting": "classicIndent", "on": true}));
    app.run("view.conversationSettings", serde_json::json!({"setting": "fromOtherFolders", "on": false}));
    assert_eq!(on_of(&app, "Use Classic Indented View"), Some(true));
    assert_eq!(on_of(&app, "Show Messages from Other Folders"), Some(false));
    assert!(state::is_on(&app, Surface::Main, &mem, "conv:classicIndent"));
    // The click sends the opposite of what the engine has.
    let classic = state::expand_items(&app, Surface::Main, &mem, items)
        .into_iter()
        .find_map(|r| match r {
            state::Resolved::Cmd { label, action, .. } if label == "Use Classic Indented View" => {
                Some(action)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(classic.params["on"], serde_json::json!(false));
    assert_eq!(classic.flip, None);
}

#[test]
fn search_tab_recent_searches_and_toggles() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let items = state::dyn_items(&app, Surface::Main, "recentSearches");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "No Recent Searches");
    app.run("search.query", serde_json::json!({"text": "budget", "commit": true}));
    app.run("search.query", serde_json::json!({"text": "from:alice", "commit": true}));
    let items = state::dyn_items(&app, Surface::Main, "recentSearches");
    let labels: Vec<&str> = items.iter().map(|d| d.label.as_str()).collect();
    assert_eq!(labels, ["from:alice", "budget"]);
    assert!(items[0].on, "the active search is checked");
    let menu = &defs().menus["recentSearches"];
    let resolved = state::expand_items(&app, Surface::Main, &state::Mem::default(), menu);
    let Some(state::Resolved::Cmd { action, .. }) = resolved.get(1) else { panic!("{resolved:?}") };
    assert_eq!(action.params["index"], 1);
    assert!(app.run(&action.cmd, action.params.clone()).is_some());
    assert_eq!(app.s.view.search, "budget");
    // Include Deleted Items follows the engine.
    assert!(!state::is_on(&app, Surface::Main, &state::Mem::default(), "searchDeleted"));
    app.run("search.scope", serde_json::json!({"includeDeleted": true}));
    assert!(state::is_on(&app, Surface::Main, &state::Mem::default(), "searchDeleted"));
    // Every Search tab refiner command is accepted by the engine.
    for name in ["searchThisWeek", "searchMore", "searchSize", "searchCategorized", "searchSentTo"] {
        for it in &defs().menus[name] {
            if it.kind == ItemKind::Cmd && !it.cmd.id.is_empty() && it.cmd.params.trim() != "" {
                let p: serde_json::Value = serde_json::from_str(&it.cmd.params).unwrap();
                let before = app.s.view.search.clone();
                assert!(app.s.run(&it.cmd.id, &p).is_ok(), "{name}: {} {p}", it.label);
                app.s.view.search = before;
            }
        }
    }
}

#[test]
fn search_tools_locations_categories_and_empty_recent_searches() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let mem = state::Mem::default();
    // No Recent Searches is a greyed-out placeholder.
    let recent = state::expand_items(&app, Surface::Main, &mem, &defs().menus["recentSearches"]);
    assert!(matches!(recent.first(), Some(state::Resolved::Cmd { disabled: Some(_), .. })), "{recent:?}");
    // Locations to Search: every store checked; a click leaves it out, the next puts it back.
    let menu = &defs().menus["searchLocations"];
    let stores = state::expand_items(&app, Surface::Main, &mem, menu);
    let Some(state::Resolved::Cmd { on, action, .. }) = stores.first().cloned() else { panic!("{stores:?}") };
    assert_eq!(on, Some(true));
    assert_eq!(action.params["on"], false);
    assert!(app.run(&action.cmd, action.params.clone()).is_some());
    let stores = state::expand_items(&app, Surface::Main, &mem, menu);
    let Some(state::Resolved::Cmd { on, action, .. }) = stores.first().cloned() else { panic!("{stores:?}") };
    assert_eq!(on, Some(false));
    assert_eq!(action.params["on"], true);
    // Categorized: alphabetical, checked from the query's category: terms (not the focused message).
    let cats = state::dyn_items(&app, Surface::Main, "searchCategories");
    let names: Vec<String> = cats.iter().map(|d| d.label.to_lowercase()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert!(cats.iter().all(|d| !d.on));
    let first = cats.first().map(|d| d.label.clone()).unwrap();
    app.run("search.refine", serde_json::json!({"kind": "categorized", "value": first}));
    let cats = state::dyn_items(&app, Surface::Main, "searchCategories");
    assert!(cats.first().is_some_and(|d| d.on), "{}", app.s.view.search);
    assert!(cats.iter().skip(1).all(|d| !d.on));
    let names: Vec<String> =
        state::dyn_items(&app, Surface::Main, "categories").iter().map(|d| d.label.to_lowercase()).collect();
    assert_eq!(names, sorted, "Categorize lists categories alphabetically too");
    // Quick Steps: the stored steps, then the fixed Create New entry.
    let steps = state::dyn_items(&app, Surface::Main, "quicksteps");
    assert_eq!(steps.last().map(|d| d.label.as_str()), Some("Create New"));
    assert_eq!(steps.iter().filter(|d| d.label == "Create New").count(), 1);
    // Download Preferences show their state.
    assert!(state::is_on(&app, Surface::Main, &mem, "dl:full"));
    assert!(!state::is_on(&app, Surface::Main, &mem, "dl:headers"));
}

#[test]
fn module_view_galleries_and_calendar_view_commands() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let tasks: Vec<String> =
        state::dyn_items(&app, Surface::Main, "views:tasks").into_iter().map(|d| d.label).collect();
    assert!(tasks.iter().any(|l| l == "Server Tasks"), "{tasks:?}");
    // In the Calendar, View-tab commands carry the calendar's view state in and out in the same frame.
    app.s.view.module = mailcraft_engine::Module::Calendar;
    let ctx = egui::Context::default();
    let mut mem = state::Mem::default();
    let act = state::Action {
        cmd: "view.arrangeBy".into(),
        params: serde_json::json!({"key": "month"}),
        flip: None,
    };
    state::run(&ctx, &mut app, Surface::Main, &mut mem, &act);
    assert_eq!(app.ui.calendar_view, "month");
    let act =
        state::Action { cmd: "view.arrangeBy".into(), params: serde_json::json!({"key": "day"}), flip: None };
    state::run(&ctx, &mut app, Surface::Main, &mut mem, &act);
    assert_eq!(app.ui.calendar_view, "day");
}

#[test]
fn notes_change_view_lists_saved_views_and_checks_the_current_one() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let notes =
        app.s.mailbox.folders.values().find(|f| f.kind == mailcraft_model::FolderKind::Notes).unwrap().id;
    app.run("nav.selectFolder", serde_json::json!({"folder": notes.0})).unwrap();
    let items = state::dyn_items(&app, Surface::Main, "views:notes");
    assert_eq!(
        items.iter().map(|d| (d.label.as_str(), d.on)).collect::<Vec<_>>(),
        [("Icon", true), ("Notes List", false), ("Last 7 Days", false)]
    );
    app.run("view.changeView", serde_json::json!({"name": "Notes List"})).unwrap();
    app.run("view.saveViewAs", serde_json::json!({"name": "Mine", "confirm": true})).unwrap();
    let items = state::dyn_items(&app, Surface::Main, "views:notes");
    assert_eq!(items.last().map(|d| (d.label.as_str(), d.on)), Some(("Mine", true)));
    assert_eq!(items.iter().filter(|d| d.on).count(), 1);
    // The menu's entries run Change View with the saved view's name.
    run_entry(&mut app, Surface::Main, "notesChangeView", "Icon");
    run_entry(&mut app, Surface::Main, "notesChangeView", "Mine");
    let st = mailcraft_engine::cmd::view::modules::state(
        &app.s,
        notes,
        mailcraft_engine::cmd::view::modules::ModuleKind::Notes,
    );
    assert_eq!((st.view.as_str(), st.name.as_str()), ("Notes List", "Mine"));
    // Icon arrangement toggles follow the engine, not UI memory.
    let mem = state::Mem::default();
    assert!(!state::is_on(&app, Surface::Main, &mem, "notesIcon:large"), "not in the Icon view");
    app.run("notes.iconView", serde_json::json!({"mode": "small"})).unwrap();
    assert!(state::is_on(&app, Surface::Main, &mem, "notesIcon:small"));
    assert!(!state::is_on(&app, Surface::Main, &mem, "notesIcon:large"));
    // Delete, Move and Categorize wait for a selected note.
    app.s.selection.items.clear();
    app.s.selection.focus = None;
    for cmd in ["notes.delete", "notes.move", "notes.categorize", "notes.forward"] {
        assert!(state::disabled(&app, Surface::Main, cmd).is_some(), "{cmd}");
    }
}

#[test]
fn calendar_toggles_follow_the_calendar_view() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let mem = state::Mem::default();
    app.s.view.module = mailcraft_engine::Module::Calendar;
    // Month view (the default): Time Scale, Working Hours and the Daily Task List are greyed out.
    assert!(state::is_on(&app, Surface::Main, &mem, "calview:timeScale.30"));
    for cmd in ["calendar.timeScale", "calendar.workingHours", "calendar.dailyTaskList"] {
        assert_eq!(state::disabled(&app, Surface::Main, cmd), Some("not available in Month view"), "{cmd}");
    }
    let ctx = egui::Context::default();
    let mut m = state::Mem::default();
    let go = |app: &mut App, m: &mut state::Mem, cmd: &str, p: serde_json::Value| {
        state::run(&ctx, app, Surface::Main, m, &state::Action { cmd: cmd.into(), params: p, flip: None });
    };
    go(&mut app, &mut m, "calendar.arrange", serde_json::json!({"mode": "week"}));
    assert!(state::disabled(&app, Surface::Main, "calendar.timeScale").is_none());
    go(&mut app, &mut m, "calendar.timeScale", serde_json::json!({"minutes": 15}));
    assert!(state::is_on(&app, Surface::Main, &mem, "calview:timeScale.15"));
    assert!(!state::is_on(&app, Surface::Main, &mem, "calview:timeScale.30"));
    // `$!on` toggles flip in one click, from the saved view.
    let (_, act, _) = menu_entries(&app, Surface::Main, "timeScale").into_iter().next().unwrap();
    assert_eq!(act.params, serde_json::json!({"minutes": 60}));
    assert!(!state::is_on(&app, Surface::Main, &mem, "calview:workingHours"));
    go(&mut app, &mut m, "calendar.workingHours", serde_json::json!({"on": true}));
    assert!(state::is_on(&app, Surface::Main, &mem, "calview:workingHours"));
    go(&mut app, &mut m, "view.tighterSpacing", serde_json::json!({"on": true}));
    assert!(state::is_on(&app, Surface::Main, &mem, "calview:tighter"));
    go(&mut app, &mut m, "view.readingPane", serde_json::json!({"position": "right"}));
    assert!(state::is_on(&app, Surface::Main, &mem, "calview:rp.right"));
    // Regression: the calendar's Reading Pane menu keeps Options... ("Alt, V, P N, N").
    let pane = menu_entries(&app, Surface::Main, "calendarReadingPane");
    let options = pane.iter().find(|(label, _, _)| label == "Options...").map(|(_, act, _)| act.cmd.clone());
    assert_eq!(options.as_deref(), Some("view.readingPaneOptions"));
    assert!(state::is_on(&app, Surface::Main, &mem, "calview:dailyTaskList.normal"));
    // Download Preferences show their state.
    assert!(state::is_on(&app, Surface::Main, &mem, "dl:full"));
    assert!(!state::is_on(&app, Surface::Main, &mem, "dl:headers"));
    assert_eq!(
        state::is_on(&app, Surface::Main, &mem, "slowHeaders"),
        mailcraft_engine::sync::cmds::slow_headers_only(&app.s)
    );
    let entries = menu_entries(&app, Surface::Main, "downloadPreferences");
    assert_eq!(entries.len(), 4);
}

/// Labels and actions of a menu as it would be drawn.
fn menu_entries(app: &App, surface: Surface, menu: &str) -> Vec<(String, state::Action, Option<[u8; 3]>)> {
    let items = defs().menu(menu).unwrap();
    state::expand_items(app, surface, &state::Mem::default(), items)
        .into_iter()
        .filter_map(|r| match r {
            state::Resolved::Cmd { label, action, swatch, .. } => Some((label, action, swatch)),
            _ => None,
        })
        .collect()
}

fn run_entry(app: &mut App, surface: Surface, menu: &str, label: &str) {
    let (_, act, _) = menu_entries(app, surface, menu).into_iter().find(|(l, ..)| l == label).unwrap();
    let ctx = egui::Context::default();
    state::run(&ctx, app, surface, &mut state::Mem::default(), &act);
}

#[test]
fn change_view_lists_saved_mail_views() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    assert!(state::dyn_items(&app, Surface::Main, "views:mail").is_empty());
    app.run("view.saveViewAs", serde_json::json!({"name": "Mine", "confirm": true})).unwrap();
    let views = state::dyn_items(&app, Surface::Main, "views:mail");
    assert_eq!(views.iter().map(|d| d.label.as_str()).collect::<Vec<_>>(), ["Mine"]);
    // Pick Compact, then the saved view from the Change View menu: the folder shows it again.
    run_entry(&mut app, Surface::Main, "changeView", "Compact");
    let folder = app.s.selection.folder;
    let name = mailcraft_engine::cmd::view::mail::current_view_name;
    assert_eq!(name(&app.s, folder), "Compact");
    run_entry(&mut app, Surface::Main, "changeView", "Mine");
    assert_eq!(name(&app.s, folder), "Mine");
    assert!(state::dyn_items(&app, Surface::Main, "views:mail").first().unwrap().on);
}

#[test]
fn theme_colour_swatches_follow_the_compose_theme() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let cid = app.run("mail.newEmail", serde_json::Value::Null).and_then(|v| v["compose"].as_u64()).unwrap();
    let s = Surface::Compose(cid);
    let base = menu_entries(&app, s, "fontColor");
    let accent = |entries: &[(String, state::Action, Option<[u8; 3]>)]| {
        entries.iter().find(|(l, ..)| l == "Dark Teal, Accent 1").cloned().unwrap()
    };
    let (_, act, before) = accent(&base);
    assert_eq!(act.params, serde_json::json!({"theme": "accent1", "tint": 0}));
    let shades = menu_entries(&app, s, "fontColorShades5");
    assert_eq!(shades.len(), 5);
    assert_eq!(shades[3].0, "Dark Teal, Accent 1, Darker 25%");
    assert_eq!(shades[3].1.params, serde_json::json!({"theme": "accent1", "tint": -25}));
    assert_eq!(menu_entries(&app, s, "shadingShades5")[0].1.cmd, "format.shading");
    app.run("compose.theme", serde_json::json!({"compose": cid, "part": "colors", "name": "Grayscale"}))
        .unwrap();
    let (_, _, after) = accent(&menu_entries(&app, s, "fontColor"));
    assert_ne!(before, after, "the swatch shows the new theme's Accent 1");
    let c = app.s.composes.get(&cid).unwrap();
    let want = mailcraft_engine::cmd::format::theme_color(c, "accent1", 0).unwrap();
    assert_eq!(after, Some([want.r, want.g, want.b]));
}

#[test]
fn high_contrast_only_hides_low_contrast_pens() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let cid = app.run("mail.newEmail", serde_json::Value::Null).and_then(|v| v["compose"].as_u64()).unwrap();
    let s = Surface::Compose(cid);
    let pens = |app: &App| -> Vec<String> {
        menu_entries(app, s, "highlight")
            .into_iter()
            .filter(|(_, a, sw)| a.cmd == "format.highlight" && sw.is_some())
            .map(|(l, ..)| l)
            .collect()
    };
    let all = pens(&app);
    assert_eq!(all.len(), 15);
    run_entry(&mut app, s, "highlight", "High-contrast only");
    assert!(app.s.composes.get(&cid).unwrap().editor.high_contrast_only);
    let shown = pens(&app);
    assert!(!shown.is_empty() && shown.len() < all.len(), "{shown:?}");
    // Every pen still offered is accepted by the engine.
    for label in &shown {
        let (_, act, _) = menu_entries(&app, s, "highlight").into_iter().find(|(l, ..)| l == label).unwrap();
        let params = state::with_context(&app, s, &act.cmd, act.params.clone());
        assert!(app.s.run(&act.cmd, &params).is_ok(), "{label}");
    }
    run_entry(&mut app, s, "highlight", "High-contrast only");
    assert_eq!(pens(&app), all);
}

#[test]
fn styles_gallery_lists_created_styles() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let cid = app.run("mail.newEmail", serde_json::Value::Null).and_then(|v| v["compose"].as_u64()).unwrap();
    let s = Surface::Compose(cid);
    let before = state::dyn_items(&app, s, "styles").len();
    app.run("format.style", serde_json::json!({"compose": cid, "name": "Callout", "action": "create"}))
        .unwrap();
    let styles = state::dyn_items(&app, s, "styles");
    assert_eq!(styles.len(), before + 1);
    assert_eq!(styles.last().unwrap().label, "Callout");
    assert!(
        menu_entries(&app, s, "styles")
            .iter()
            .any(|(l, a, _)| l == "Callout" && a.params == serde_json::json!({"name": "Callout"}))
    );
}

#[test]
fn show_all_folders_a_to_z_reads_the_engine() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let mem = state::Mem::default();
    let toggle = defs()
        .tabs
        .iter()
        .flat_map(|t| t.groups.iter().flat_map(|g| g.ctls.iter()))
        .find(|c| c.cmd.id == "folder.showAllAZ")
        .unwrap();
    assert_eq!(toggle.on.as_deref(), Some("allaz"));
    assert!(!state::is_on(&app, Surface::Main, &mem, "allaz"));
    let act = state::action(&app, &toggle.cmd, false, None, &[]);
    assert_eq!(act.params, serde_json::json!({"on": true}));
    let r = app.run(&act.cmd, act.params.clone()).unwrap();
    assert_eq!(
        state::is_on(&app, Surface::Main, &mem, "allaz"),
        r["saved"] == true,
        "on exactly when the engine kept it"
    );
    assert_eq!(
        state::is_on(&app, Surface::Main, &mem, "allaz"),
        mailcraft_engine::cmd::folder::show_all_az_on(&app.s)
    );
}

#[test]
fn followup_menu_entries_send_the_engine_params() {
    let app = App::new(mailcraft_engine::sample::sample_session());
    let find =
        |menu: &str, label: &str| defs().menu(menu).unwrap().iter().find(|i| i.label == label).cloned();
    let slow = find("downloadPreferences", "On Slow Connections Download Only Headers").unwrap();
    assert_eq!(
        state::action(&app, &slow.cmd, false, None, &[]).params,
        serde_json::json!({"slowHeadersOnly": "toggle"})
    );
    let paste = find("paste", "Set Default Paste...").unwrap();
    assert_eq!(paste.cmd.params, r#"{"page":"editorAdvanced"}"#);
    let similar = find("readSelect", "Select All Text With Similar Formatting").unwrap();
    assert_eq!(similar.cmd.id, "format.selectSimilar");
    // Message recall is not offered: IMAP, SMTP and JMAP cannot withdraw delivered mail.
    assert!(all_cmds().iter().all(|(_, c)| c.id != "mail.recall"));
}

fn compose_app() -> (App, u64) {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let cid = app.run("mail.newEmail", serde_json::Value::Null).and_then(|v| v["compose"].as_u64()).unwrap();
    (app, cid)
}

fn entry_disabled(app: &App, surface: Surface, menu: &str, label: &str) -> Option<&'static str> {
    let items = defs().menu(menu).unwrap();
    state::expand_items(app, surface, &state::Mem::default(), items)
        .into_iter()
        .find_map(|r| match r {
            state::Resolved::Cmd { label: l, disabled, .. } if l == label => Some(disabled),
            _ => None,
        })
        .unwrap()
}

#[test]
fn stop_highlighting_is_offered_only_while_the_pen_is_out() {
    let (mut app, cid) = compose_app();
    let s = Surface::Compose(cid);
    assert!(entry_disabled(&app, s, "highlight", "Stop Highlighting").is_some());
    // Highlight with nothing selected takes the pen.
    app.run("format.highlight", serde_json::json!({"compose": cid, "color": "#ffff00"})).unwrap();
    assert!(entry_disabled(&app, s, "highlight", "Stop Highlighting").is_none());
    run_entry(&mut app, s, "highlight", "Stop Highlighting");
    assert!(app.s.composes.get(&cid).unwrap().highlight_pen.is_none());
}

#[test]
fn format_painter_double_click_is_sticky() {
    let ctl = defs()
        .tabs
        .iter()
        .filter(|t| t.surface == "compose")
        .flat_map(|t| t.groups.iter())
        .flat_map(|g| g.ctls.iter())
        .find(|c| c.cmd.id == "format.formatPainter")
        .unwrap();
    let dbl = ctl.dblcmd.as_ref().expect("double-click command");
    assert_eq!(dbl.id, "format.formatPainter");
    assert!(dbl.params.contains("sticky"));
}

#[test]
fn font_list_has_theme_fonts_and_known_and_installed_fonts() {
    let (mut app, cid) = compose_app();
    let s = Surface::Compose(cid);
    let labels: Vec<String> = menu_entries(&app, s, "fonts").into_iter().map(|(l, ..)| l).collect();
    assert!(labels.iter().any(|l| l.ends_with("(Headings)")) && labels.iter().any(|l| l.ends_with("(Body)")));
    assert!(labels.len() > 30, "{labels:?}");
    for name in ["Calibri", "Times New Roman", "Courier New", "Font..."] {
        assert!(labels.iter().any(|l| l == name), "{name}");
    }
    // Picking a theme font sets that family.
    let body = labels.iter().find(|l| l.ends_with("(Body)")).unwrap().clone();
    run_entry(&mut app, s, "fonts", &body);
    let family = app.s.composes.get(&cid).unwrap().style_at_caret().font_family.unwrap_or_default();
    assert!(body.starts_with(&family), "{body} / {family}");
}

#[test]
fn change_styles_offers_colors_fonts_and_saved_style_sets() {
    let (mut app, cid) = compose_app();
    let s = Surface::Compose(cid);
    let subs: Vec<String> = defs()
        .menu("changeStyles")
        .unwrap()
        .iter()
        .filter(|i| i.kind == ItemKind::Sub)
        .map(|i| i.label.clone())
        .collect();
    assert!(subs.contains(&"Colors".to_owned()) && subs.contains(&"Fonts".to_owned()), "{subs:?}");
    app.run("format.styleSet", serde_json::json!({"compose": cid, "name": "Minimalist"})).unwrap();
    app.run("format.styleSet", serde_json::json!({"compose": cid, "action": "save", "name": "Ours"}))
        .unwrap();
    assert!(menu_entries(&app, s, "styleSet").iter().any(|(l, ..)| l == "Ours"));
    run_entry(&mut app, s, "styleSet", "Ours");
    assert_eq!(app.s.composes.get(&cid).unwrap().style_set.as_deref(), Some("Minimalist"));
}

#[test]
fn page_color_menu_has_theme_and_standard_colours() {
    let (mut app, cid) = compose_app();
    let s = Surface::Compose(cid);
    let entries = menu_entries(&app, s, "pageColor");
    assert!(entries.iter().filter(|(_, _, sw)| sw.is_some()).count() >= 20);
    let first_theme = entries.iter().find(|(_, a, _)| a.params.get("theme").is_some()).unwrap().0.clone();
    run_entry(&mut app, s, "pageColor", &first_theme);
    assert!(app.s.composes.get(&cid).unwrap().body.background.is_some());
}

#[test]
fn module_ribbon_state_follows_the_folder_and_selection() {
    use mailcraft_engine::cmd::view::modules;
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let tasks =
        app.s.mailbox.folders.values().find(|f| f.kind == mailcraft_model::FolderKind::Tasks).unwrap().id;
    app.run("nav.selectFolder", serde_json::json!({"folder": tasks.0})).unwrap();
    app.s.view.module = mailcraft_engine::Module::Tasks;
    let mem = state::Mem::default();
    // Change View: the built-in views, then the saved ones; the shown view is checked.
    let on = |app: &App| -> Vec<String> {
        state::dyn_items(app, Surface::Main, "views:tasks")
            .into_iter()
            .filter(|d| d.on)
            .map(|d| d.label)
            .collect()
    };
    assert_eq!(on(&app), ["To-Do List"]);
    app.run("view.changeView", serde_json::json!({"name": "Detailed"})).unwrap();
    app.run("view.saveViewAs", serde_json::json!({"name": "Mine", "confirm": true})).unwrap();
    let items = state::dyn_items(&app, Surface::Main, "views:tasks");
    assert_eq!(items.last().map(|d| d.label.as_str()), Some("Mine"));
    assert_eq!(on(&app), ["Mine"]);
    app.run("view.changeView", serde_json::json!({"name": "Simple List"})).unwrap();
    assert_eq!(on(&app), ["Simple List"]);
    run_entry(&mut app, Surface::Main, "tasksChangeView", "Mine");
    assert_eq!(modules::current_view_name(&app.s, tasks), "Mine");
    // Message Preview reads the tasks folder's own setting (Off by default), not the mail view's.
    assert!(state::is_on(&app, Surface::Main, &mem, "preview:0"));
    assert!(!state::is_on(&app, Surface::Main, &mem, "preview:1"));
    app.run("view.messagePreview", serde_json::json!({"lines": 2})).unwrap();
    assert!(state::is_on(&app, Surface::Main, &mem, "preview:2"));
    // Arrangement: the gallery lists the module's arrangements with the current one checked.
    app.run("view.arrangeBy", serde_json::json!({"key": "importance"})).unwrap();
    let arr = state::dyn_items(&app, Surface::Main, "arrange");
    assert_eq!(arr.len(), 6);
    assert!(arr.iter().any(|d| d.label == "Importance" && d.on));
    assert!(state::is_on(&app, Surface::Main, &mem, "marr:importance"));
    assert!(!state::is_on(&app, Surface::Main, &mem, "marr:dueDate"));
    // Tags toggles show the selected task's state.
    let id = app.s.mailbox.tasks.values().find(|t| t.folder == tasks).unwrap().id;
    app.ui.module_selection = Some(id.0);
    app.s.selection.items = vec![id];
    app.s.selection.focus = Some(id);
    assert!(!state::is_on(&app, Surface::Main, &mem, "pim:private"));
    run_entry_toggle(&mut app, "Private");
    assert!(app.s.mailbox.tasks[&id].private);
    assert!(state::is_on(&app, Surface::Main, &mem, "pim:private"));
    run_entry_toggle(&mut app, "Private");
    assert!(!app.s.mailbox.tasks[&id].private, "the toggle turns it off again");
    app.run("tasks.importance", serde_json::json!({"level": "high", "on": true})).unwrap();
    assert!(state::is_on(&app, Surface::Main, &mem, "pim:high"));
    assert!(!state::is_on(&app, Surface::Main, &mem, "pim:low"));
    // Categorize › the categories of the selected task are checked.
    app.run("tasks.categorize", serde_json::json!({"name": "Red category", "on": true})).unwrap();
    assert!(
        state::dyn_items(&app, Surface::Main, "categories").iter().any(|d| d.label == "Red category" && d.on)
    );
    // The People Arrange By is greyed out in the People view (no arrangement there).
    let contacts =
        app.s.mailbox.folders.values().find(|f| f.kind == mailcraft_model::FolderKind::Contacts).unwrap().id;
    app.run("nav.selectFolder", serde_json::json!({"folder": contacts.0})).unwrap();
    assert!(app.s.enabled("view.arrangeBy").is_some());
    app.run("view.changeView", serde_json::json!({"name": "List"})).unwrap();
    assert!(app.s.enabled("view.arrangeBy").is_none());
    assert!(state::is_on(&app, Surface::Main, &mem, "marr:company"), "List arranges by company");
}

/// Runs the Tasks Home tab toggle labelled `label` as the ribbon would (its `$!on` filled from its state).
fn run_entry_toggle(app: &mut App, label: &str) {
    let tab = defs().tabs.iter().find(|t| t.surface == "tasks" && t.name == "Home").unwrap();
    let item = tab.groups.iter().flat_map(|g| g.ctls.iter()).find(|i| i.label == label).unwrap();
    let mem = state::Mem::default();
    let on = item.on.as_deref().is_some_and(|s| state::is_on(app, Surface::Main, &mem, s));
    let act = state::action(app, &item.cmd, on, None, &[]);
    let ctx = egui::Context::default();
    state::run(&ctx, app, Surface::Main, &mut state::Mem::default(), &act);
}

#[test]
fn work_offline_and_download_preferences_show_the_engine_state() {
    let mut app = App::new(mailcraft_engine::sample::sample_session());
    let mem = state::Mem::default();
    assert!(!state::is_on(&app, Surface::Main, &mem, "offline"));
    app.run("sendReceive.workOffline", serde_json::json!({})).unwrap();
    assert!(state::is_on(&app, Surface::Main, &mem, "offline"));
    assert!(
        state::is_on(&app, Surface::Main, &mem, "dl:full")
            || state::is_on(&app, Surface::Main, &mem, "dl:headersThenFull")
            || state::is_on(&app, Surface::Main, &mem, "dl:headers")
    );
    let (mode, slow) = mailcraft_engine::sync::cmds::shown_preferences(&app.s);
    assert!(state::is_on(&app, Surface::Main, &mem, &format!("dl:{mode}")));
    assert_eq!(state::is_on(&app, Surface::Main, &mem, "slowHeaders"), slow);
    // Exactly one download mode is checked, and it follows a change.
    let modes = ["full", "headersThenFull", "headers"];
    for want in ["headers", "full", "headersThenFull"] {
        app.run("sendReceive.preferences", serde_json::json!({"mode": want})).unwrap();
        assert_eq!(mailcraft_engine::sync::cmds::shown_preferences(&app.s).0, want);
        for m in modes {
            assert_eq!(
                state::is_on(&app, Surface::Main, &mem, &format!("dl:{m}")),
                m == want,
                "{want}: dl:{m}"
            );
        }
    }
    // On Slow Connections Download Only Headers follows its toggle both ways.
    for _ in 0..2 {
        let before = state::is_on(&app, Surface::Main, &mem, "slowHeaders");
        app.run("sendReceive.preferences", serde_json::json!({"slowHeadersOnly": "toggle"})).unwrap();
        assert_eq!(state::is_on(&app, Surface::Main, &mem, "slowHeaders"), !before);
        assert_eq!(mailcraft_engine::sync::cmds::shown_preferences(&app.s).1, !before);
    }
}
