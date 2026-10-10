//! Key mapping: chords, context resolution (surface / module / focus), keytips, and applying keys to an app.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use egui::{Event, Key, Modifiers, RawInput};
use mailcraft_engine::Module;
use mailcraft_engine::sample::sample_session;
use mailcraft_ui_egui::keys::{
    self, Action, Chord, KeyContext, KeytipAction, KeytipStep, KeytipTarget, keytip_step, resolve,
    resolve_with,
};
use mailcraft_ui_egui::{App, Surface};
use serde_json::json;

fn c(text: &str) -> Chord {
    Chord::parse(text).unwrap_or_else(|| panic!("cannot parse {text}"))
}

/// Command id a chord runs in a context (built-ins only).
fn id_of(text: &str, cx: &KeyContext) -> Option<String> {
    match resolve(c(text), cx)? {
        Action::Run { id, .. } | Action::RunIfEnabled { id, .. } => Some(id),
        other => Some(format!("{other:?}")),
    }
}

fn main_mail() -> KeyContext {
    KeyContext::main(Module::Mail)
}

#[test]
fn chords_parse_portable_and_outlook_forms() {
    assert_eq!(c("Mod+Shift+R"), Chord::cmd_shift(Key::R));
    assert_eq!(c("Ctrl+Shift+R"), Chord::cmd_shift(Key::R));
    assert_eq!(c("Mod+Shift+>"), Chord::cmd_shift(Key::Period));
    assert_eq!(c("Ctrl+Shift+<"), Chord::cmd_shift(Key::Comma));
    assert_eq!(c("Ctrl+*"), Chord::cmd_shift(Key::Num8));
    assert_eq!(c("Mod+Shift++"), c("Mod+Shift+="));
    assert_eq!(c("Ctrl+Del"), Chord::cmd(Key::Delete));
    assert_eq!(c("Mod+Delete"), Chord::cmd(Key::Delete));
    assert_eq!(c("F9"), Chord::plain(Key::F9));
    assert_eq!(c("Insert"), Chord::plain(Key::Insert));
    let ca = c("Ctrl+Alt+1");
    assert!(ca.cmd && ca.alt && !ca.shift && ca.key == Key::Num1);
    assert_eq!(c("Mod+u"), Chord::cmd(Key::U));
    assert_eq!(Chord::parse(""), None);
    assert_eq!(Chord::parse("Hyper+R"), None);
    assert_eq!(Chord::parse("Mod+NoSuchKey"), None);
    assert_eq!(
        keys::parse_shortcut_list("Delete / Mod+D"),
        vec![Chord::plain(Key::Delete), Chord::cmd(Key::D)]
    );
    for text in ["Mod+Shift+R", "F9", "Mod+Alt+Shift+S", "Delete", "Mod+="] {
        assert_eq!(Chord::parse(&c(text).portable()), Some(c(text)), "{text}");
    }
    // Ctrl+Shift+Plus arrives as Key::Plus.
    let ev = Chord::from_event(Key::Plus, Modifiers::COMMAND | Modifiers::SHIFT);
    assert_eq!(ev, c("Mod+Shift+="));
}

#[test]
fn every_registry_shortcut_parses() {
    let s = sample_session();
    for spec in &s.registry.specs {
        for part in spec.shortcut.split(" / ").filter(|p| !p.trim().is_empty()) {
            assert!(Chord::parse(part).is_some(), "{}: cannot parse shortcut `{part}`", spec.id);
        }
    }
}

#[test]
fn main_window_mail_bindings() {
    let cx = main_mail();
    let cases = [
        ("Mod+U", "mail.markUnread"),
        ("Mod+Q", "mail.markRead"),
        ("Mod+A", "nav.selectAll"),
        ("Mod+C", "app.copy"),
        ("Delete", "mail.delete"),
        ("Mod+D", "mail.delete"),
        ("Shift+Delete", "mail.permanentDelete"),
        ("Mod+Delete", "mail.ignore"),
        ("Mod+R", "mail.reply"),
        ("Mod+Shift+R", "mail.replyAll"),
        ("Mod+F", "mail.forward"),
        ("Mod+Shift+M", "mail.newEmail"),
        ("Mod+N", "mail.newEmail"),
        ("Mod+Shift+V", "mail.move"),
        ("Mod+Shift+E", "folder.new"),
        ("Mod+Shift+P", "folder.newSearchFolder"),
        ("Mod+Shift+B", "compose.addressBook"),
        ("Mod+Shift+F", "search.advancedFind"),
        ("Mod+Shift+G", "mail.flagCustom"),
        ("Insert", "mail.flag"),
        ("Mod+O", "mail.open"),
        ("Enter", "mail.open"),
        ("Mod+Shift+I", "nav.inbox"),
        ("Mod+Shift+O", "nav.outbox"),
        ("Mod+Y", "nav.goToFolder"),
        ("Mod+Z", "app.undo"),
        ("F9", "sendReceive.all"),
        ("Mod+1", "nav.mail"),
        ("Mod+2", "nav.calendar"),
        ("Mod+3", "nav.people"),
        ("Mod+4", "nav.tasks"),
        ("Mod+Shift+A", "calendar.newAppointment"),
        ("Mod+Shift+Q", "calendar.newMeeting"),
        ("Mod+Shift+C", "people.new"),
        ("Mod+Shift+K", "tasks.new"),
        ("Down", "nav.nextItem"),
        ("Up", "nav.prevItem"),
        ("Home", "nav.firstItem"),
        ("End", "nav.lastItem"),
    ];
    for (keys, want) in cases {
        assert_eq!(id_of(keys, &cx).as_deref(), Some(want), "{keys}");
    }
    assert_eq!(resolve(c("F3"), &cx), Some(Action::FocusSearch));
    assert_eq!(resolve(c("Mod+E"), &cx), Some(Action::FocusSearch));
    assert_eq!(resolve(c("F10"), &cx), Some(Action::Keytips));
    assert_eq!(resolve(c("Mod+Shift+3"), &cx), Some(Action::QuickStep(3)));
    assert_eq!(resolve(c("Mod+F4"), &cx), Some(Action::Category("Ctrl+F4".into())));
    match resolve(c("Shift+Down"), &cx) {
        Some(Action::Run { id, params }) => {
            assert_eq!(id, "nav.nextItem");
            assert_eq!(params, json!({"extend": true}));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn clashes_resolve_by_surface_and_module() {
    let people = KeyContext::main(Module::People);
    assert_eq!(id_of("Delete", &people).as_deref(), Some("people.delete"));
    assert_eq!(id_of("Mod+F", &people).as_deref(), Some("people.forward"));
    assert_eq!(id_of("Mod+U", &people), None);
    assert_eq!(id_of("Mod+N", &people).as_deref(), Some("people.new"));
    let cal = KeyContext::main(Module::Calendar);
    assert_eq!(id_of("Mod+G", &cal).as_deref(), Some("calendar.goToDate"));
    assert_eq!(id_of("Mod+T", &cal).as_deref(), Some("calendar.today"));
    assert_eq!(id_of("Delete", &cal).as_deref(), Some("calendar.delete"));
    assert_eq!(resolve(c("Ctrl+Alt+4"), &cal), Some(Action::CalendarView("month")));
    assert_eq!(id_of("Mod+G", &main_mail()), None);
    assert_eq!(id_of("Delete", &KeyContext::main(Module::Tasks)).as_deref(), Some("tasks.delete"));
    assert_eq!(id_of("Delete", &KeyContext::main(Module::Notes)).as_deref(), Some("notes.delete"));

    let compose = KeyContext::compose(7);
    let cases = [
        ("Mod+U", "format.underline"),
        ("Mod+B", "format.bold"),
        ("Mod+I", "format.italic"),
        ("Mod+E", "format.align"),
        ("Mod+L", "format.align"),
        ("Mod+R", "format.align"),
        ("Mod+J", "format.align"),
        ("Mod+K", "insert.link"),
        ("Mod+A", "format.selectAll"),
        ("Mod+C", "format.copy"),
        ("Mod+G", "format.goTo"),
        ("Mod+Shift+>", "format.growFont"),
        ("Mod+Shift+<", "format.shrinkFont"),
        ("Mod+=", "format.sub"),
        ("Mod+Shift+=", "format.sup"),
        ("Mod+S", "compose.saveDraft"),
        ("Mod+Enter", "compose.send"),
        ("Alt+S", "compose.send"),
        ("Mod+Z", "compose.editUndo"),
        ("Mod+Y", "compose.editRedo"),
        ("F7", "review.spelling"),
    ];
    for (keys, want) in cases {
        assert_eq!(id_of(keys, &compose).as_deref(), Some(want), "{keys}");
    }
    match resolve(c("Mod+R"), &compose) {
        Some(Action::Run { params, .. }) => assert_eq!(params, json!({"compose": 7, "kind": "right"})),
        other => panic!("{other:?}"),
    }
    assert!(matches!(resolve(c("Mod+Enter"), &compose), Some(Action::RunIfEnabled { .. })));
    assert_eq!(id_of("Delete", &compose), None, "Delete edits the body");

    let read = KeyContext::read(42);
    match resolve(c("Mod+R"), &read) {
        Some(Action::Run { id, params }) => {
            assert_eq!(id, "mail.reply");
            assert_eq!(params, json!({"ids": [42]}));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(id_of("Delete", &read).as_deref(), Some("mail.delete"));
    assert_eq!(resolve(c("Mod+."), &read), Some(Action::ReadStep(true)));
    assert_eq!(resolve(c("Mod+Shift+<"), &read), Some(Action::ReadStep(false)));
}

#[test]
fn focused_text_fields_keep_their_keys() {
    let cx = KeyContext { text_focus: true, ..main_mail() };
    for keys in ["Mod+A", "Mod+C", "Mod+V", "Delete", "Home", "Up", "Mod+Z", "Enter", "Mod+U", "Insert"] {
        assert_eq!(resolve(c(keys), &cx), None, "{keys} must reach the text field");
    }
    assert_eq!(id_of("Mod+Shift+M", &cx).as_deref(), Some("mail.newEmail"));
    assert_eq!(id_of("F9", &cx).as_deref(), Some("sendReceive.all"));
    assert_eq!(id_of("Mod+2", &cx).as_deref(), Some("nav.calendar"));
    assert_eq!(resolve(c("Mod+Shift+Z"), &cx), None, "redo in the field");

    let compose_field = KeyContext { text_focus: true, ..KeyContext::compose(3) };
    assert_eq!(id_of("Mod+B", &compose_field).as_deref(), Some("format.bold"));
    assert_eq!(id_of("Mod+Enter", &compose_field).as_deref(), Some("compose.send"));
    assert_eq!(id_of("Mod+S", &compose_field).as_deref(), Some("compose.saveDraft"));
    assert_eq!(resolve(c("Mod+A"), &compose_field), None);
    assert_eq!(resolve(c("Mod+V"), &compose_field), None);
    assert_eq!(resolve(c("Mod+Z"), &compose_field), None);
}

#[test]
fn modal_states_and_escape() {
    let dialog = KeyContext { dialog: true, ..main_mail() };
    assert_eq!(resolve(c("Mod+R"), &dialog), None);
    assert_eq!(resolve(c("Delete"), &dialog), None);
    assert_eq!(resolve(c("Escape"), &dialog), Some(Action::Escape));
    let backstage = KeyContext { backstage: true, ..main_mail() };
    assert_eq!(resolve(c("Mod+Shift+M"), &backstage), None);
    assert_eq!(resolve(c("Esc"), &backstage), Some(Action::Escape));
    // Nothing to close: Escape stays with the widgets.
    assert_eq!(resolve(c("Escape"), &main_mail()), None);
    let search = KeyContext { search_active: true, ..main_mail() };
    assert_eq!(resolve(c("Escape"), &search), Some(Action::Escape));
    assert_eq!(id_of("Mod+Alt+A", &search).as_deref(), Some("search.scope"));
    assert_eq!(resolve(c("Mod+Alt+A"), &main_mail()), None);
    let popup = KeyContext { popup: true, dialog: true, ..main_mail() };
    assert_eq!(resolve(c("Escape"), &popup), None, "an open menu closes itself first");
    assert_eq!(resolve(c("Escape"), &KeyContext::compose(1)), Some(Action::Escape));
}

#[test]
fn registry_fallback_respects_module() {
    let s = sample_session();
    let people = KeyContext::main(Module::People);
    match resolve_with(c("F11"), &people, &s.registry) {
        Some(Action::Run { id, .. }) => assert_eq!(id, "people.search"),
        other => panic!("{other:?}"),
    }
    // Mail: F11 focuses the ribbon's Search People box (keyboard.md, tested keys).
    assert_eq!(resolve_with(c("F11"), &main_mail(), &s.registry), Some(Action::FocusSearchPeople));
    assert_eq!(resolve_with(c("F11"), &KeyContext::main(Module::Calendar), &s.registry), None);
    // Built-ins win over the registry's first match.
    match resolve_with(c("Mod+F"), &people, &s.registry) {
        Some(Action::Run { id, .. }) => assert_eq!(id, "people.forward"),
        other => panic!("{other:?}"),
    }
    match resolve_with(c("Mod+H"), &KeyContext::compose(2), &s.registry) {
        Some(Action::Run { id, params }) => {
            assert_eq!(id, "format.replace");
            assert_eq!(params["compose"], 2);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn keytip_matching() {
    let t =
        KeytipTarget::from_text("Alt, H, R P", "Reply", KeytipAction::Run("mail.reply".into(), json!({})));
    assert_eq!(t.path, vec!["H".to_string(), "RP".to_string()]);
    assert_eq!(t.sequence(), "HRP");
    let targets = vec![
        KeytipTarget::from_text("Alt, F", "File", KeytipAction::Backstage),
        KeytipTarget::from_text("Alt, H", "Home", KeytipAction::Tab("Home".into())),
        t.clone(),
        KeytipTarget::from_text(
            "Alt, H, R A",
            "Reply All",
            KeytipAction::Run("mail.replyAll".into(), json!({})),
        ),
        KeytipTarget::from_text("Alt, H, D", "Delete", KeytipAction::Run("mail.delete".into(), json!({}))),
        KeytipTarget::from_text(
            "Alt, 1",
            "Send/Receive",
            KeytipAction::Run("sendReceive.all".into(), json!({})),
        ),
    ];
    assert!(matches!(keytip_step(&targets, "H"), KeytipStep::Fire(ref x) if x.label == "Home"));
    assert_eq!(keytip_step(&targets, "HR"), KeytipStep::Pending);
    assert_eq!(keytip_step(&targets, "HRP"), KeytipStep::Fire(t));
    assert!(matches!(keytip_step(&targets, "hd"), KeytipStep::Fire(ref x) if x.label == "Delete"));
    assert!(
        matches!(keytip_step(&targets, "F"), KeytipStep::Fire(ref x) if x.action == KeytipAction::Backstage)
    );
    assert_eq!(keytip_step(&targets, "Q"), KeytipStep::NoMatch);
    assert_eq!(keytip_step(&targets, "HX"), KeytipStep::NoMatch);
}

fn app() -> App {
    App::new(sample_session())
}

#[test]
fn press_applies_keys_and_keytips() {
    let ctx = egui::Context::default();
    let mut app = app();
    let before = app.s.composes.len();
    let r = keys::press(&ctx, &mut app, Surface::Main, "Mod+Shift+M");
    assert_eq!(r["command"], "mail.newEmail", "{r}");
    assert_eq!(app.s.composes.len(), before + 1);

    // Alt, H selects the Home tab and keeps keytips on for its controls; Escape steps back, then leaves.
    keys::press(&ctx, &mut app, Surface::Main, "Alt");
    assert_eq!(app.ui.keytips.as_deref(), Some(""));
    keys::press(&ctx, &mut app, Surface::Main, "H");
    assert_eq!(app.ui.keytips.as_deref(), Some("H"));
    assert_eq!(Some(mailcraft_ui_egui::ribbon::selected_tab(&app, Surface::Main)), Some("Home"));
    keys::press(&ctx, &mut app, Surface::Main, "Escape");
    assert_eq!(app.ui.keytips.as_deref(), Some(""));
    keys::press(&ctx, &mut app, Surface::Main, "Escape");
    assert_eq!(app.ui.keytips, None);
    // Alt, F opens the backstage; Escape closes it.
    keys::press(&ctx, &mut app, Surface::Main, "Alt F");
    assert!(app.ui.backstage.is_some());
    assert_eq!(app.ui.keytips, None);
    keys::press(&ctx, &mut app, Surface::Main, "Escape");
    assert!(app.ui.backstage.is_none());
    // Unknown keys are reported, not fatal.
    let r = keys::press(&ctx, &mut app, Surface::Main, "Mod+Bogus");
    assert_eq!(r["error"], "unknown key");
}

#[test]
fn keytips_continue_into_dropdown_entries() {
    let ctx = egui::Context::default();
    let mut app = app();
    let first = app.s.mailbox.mail.keys().next().copied().unwrap();
    app.run("nav.selectItem", json!({"id": first.0}));
    app.run("mail.flag", json!({"kind": "clear"}));
    // Alt, H, U opens Follow Up and keeps keytips on; T then picks Today (ribbon-mail.md "Alt, H, U, T").
    keys::press(&ctx, &mut app, Surface::Main, "Alt H U");
    assert_eq!(app.ui.keytips.as_deref(), Some("HU"));
    keys::press(&ctx, &mut app, Surface::Main, "T");
    assert_eq!(app.ui.keytips, None);
    assert!(matches!(
        app.s.mailbox.mail.get(&first).map(|m| &m.flag),
        Some(mailcraft_model::FlagState::Flagged { .. })
    ));
    // Escape inside an opened dropdown steps back to the tab's keytips.
    keys::press(&ctx, &mut app, Surface::Main, "Alt H G");
    assert_eq!(app.ui.keytips.as_deref(), Some("HG"));
    keys::press(&ctx, &mut app, Surface::Main, "Escape");
    assert_eq!(app.ui.keytips.as_deref(), Some("H"));
    keys::press(&ctx, &mut app, Surface::Main, "Escape Escape");
    assert_eq!(app.ui.keytips, None);
}

#[test]
fn read_window_keys_work_with_nothing_selected_in_the_main_window() {
    let ctx = egui::Context::default();
    let mut app = app();
    let folder = app.s.selection.folder;
    let item = app.s.mailbox.mail.values().find(|m| m.read && m.folder == folder).map(|m| m.id).unwrap();
    app.s.selection.items.clear();
    app.s.selection.focus = None;
    let r = keys::press(&ctx, &mut app, Surface::Read(item.0), "Mod+U");
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(app.s.mailbox.mail.get(&item).map(|m| m.read), Some(false));
}

#[test]
fn f11_focuses_search_people() {
    let ctx = egui::Context::default();
    let mut app = app();
    app.ui.set_tab(Surface::Main, "View");
    assert_eq!(resolve(c("F11"), &main_mail()), Some(Action::FocusSearchPeople));
    let r = keys::press(&ctx, &mut app, Surface::Main, "F11");
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(mailcraft_ui_egui::ribbon::selected_tab(&app, Surface::Main), "Home");
}

#[test]
fn ribbon_registered_keytips_run_commands() {
    let ctx = egui::Context::default();
    let mut app = app();
    keys::set_keytip_targets(
        &ctx,
        Surface::Main,
        vec![KeytipTarget::from_text(
            "Alt, H, N 1",
            "New Email",
            KeytipAction::Run("mail.newEmail".into(), json!({})),
        )],
    );
    let before = app.s.composes.len();
    keys::press(&ctx, &mut app, Surface::Main, "Alt H N 1");
    assert_eq!(app.s.composes.len(), before + 1);
    assert_eq!(app.ui.keytips, None);
}

/// One frame with these events, running only the key handler.
fn frame(ctx: &egui::Context, app: &mut App, events: Vec<Event>) {
    let raw = RawInput { events, ..RawInput::default() };
    let mut out = ctx.run_ui(raw, |ui| keys::handle(ui.ctx(), app, Surface::Main));
    out.textures_delta.clear();
}

fn key(k: Key, modifiers: Modifiers) -> Event {
    Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers }
}

#[test]
fn handle_consumes_bound_events() {
    let ctx = egui::Context::default();
    let mut app = app();
    let first =
        app.s.mailbox.mail.values().find(|m| m.folder == app.s.selection.folder && m.read).map(|m| m.id);
    let Some(first) = first else { return };
    app.run("nav.selectItem", json!({"id": first.0}));
    frame(&ctx, &mut app, vec![key(Key::U, Modifiers::COMMAND)]);
    assert!(!app.s.mailbox.mail[&first].read, "Ctrl+U marks unread");
    frame(&ctx, &mut app, vec![key(Key::Q, Modifiers::COMMAND)]);
    assert!(app.s.mailbox.mail[&first].read, "Ctrl+Q marks read");

    // Bare Alt down + up toggles keytips; a letter then selects a tab.
    frame(&ctx, &mut app, vec![key(Key::AltLeft, Modifiers::ALT)]);
    frame(
        &ctx,
        &mut app,
        vec![Event::Key {
            key: Key::AltLeft,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert_eq!(app.ui.keytips.as_deref(), Some(""));
    frame(&ctx, &mut app, vec![key(Key::V, Modifiers::NONE), Event::Text("v".into())]);
    assert_eq!(Some(mailcraft_ui_egui::ribbon::selected_tab(&app, Surface::Main)), Some("View"));
    frame(&ctx, &mut app, vec![key(Key::Escape, Modifiers::NONE)]);
    frame(&ctx, &mut app, vec![key(Key::Escape, Modifiers::NONE)]);
    assert_eq!(app.ui.keytips, None);

    // F9 runs Send/Receive even with nothing selected (errors become a toast, never a panic).
    frame(&ctx, &mut app, vec![key(Key::F9, Modifiers::NONE)]);
}
