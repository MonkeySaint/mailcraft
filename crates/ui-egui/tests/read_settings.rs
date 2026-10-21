//! Reading, list, people, search, calendar and display options that change what the shell shows, driven
//! through the whole app (`App::ui_root`) and checked in the accessibility tree, the shell layout and the
//! viewport commands.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mailcraft_engine::queries::{self, ListRow};
use mailcraft_engine::sample::sample_session;
use mailcraft_ui_egui::App;
use serde_json::json;

fn build(app: App, w: f32, h: f32) -> Harness<'static, App> {
    let mut fonts_done = false;
    let mut h = Harness::builder().with_size(egui::vec2(w, h)).build_ui_state(
        move |ui, app: &mut App| {
            if !fonts_done {
                App::install_fonts(ui.ctx());
                fonts_done = true;
            }
            ui.ctx().set_embed_viewports(true);
            app.ui_root(ui);
        },
        app,
    );
    h.run_steps(4);
    h
}

/// The sample app with option `key` set to `value`.
fn app_with(options: &[(&str, serde_json::Value)]) -> App {
    let mut app = App::new(sample_session());
    for (k, v) in options {
        assert!(app.run("options.set", json!({"key": k, "value": v})).is_some(), "{k}");
    }
    app
}

fn has(h: &Harness<'_, App>, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

fn has_part(h: &Harness<'_, App>, label: &str) -> bool {
    h.query_all_by_label_contains(label).next().is_some()
}

fn first_item(app: &App) -> u64 {
    queries::list_rows(&app.s)
        .into_iter()
        .find_map(|r| match r {
            ListRow::Item(i) => Some(i.id.0),
            ListRow::Group { .. } => None,
        })
        .unwrap()
}

fn select_first(h: &mut Harness<'_, App>) -> u64 {
    let id = first_item(h.state());
    h.state_mut().run("nav.selectItem", json!({"id": id}));
    h.run_steps(3);
    id
}

#[test]
fn always_preview_off_keeps_the_message_out_of_the_reading_pane() {
    let mut h = build(app_with(&[("mail.readingPane.alwaysPreview", json!(false))]), 1600.0, 1000.0);
    let id = select_first(&mut h);
    let subject = h.state().s.mailbox.mail[&mailcraft_engine::model::ItemId(id)].subject.clone();
    assert!(has(&h, mailcraft_ui_egui::reading::PREVIEW_OFF));
    assert!(!has(&h, "More actions"), "no message header in the pane");
    // The link turns previews on again.
    h.get_by_label(mailcraft_ui_egui::reading::ALWAYS_PREVIEW_LINK).click();
    h.run_steps(3);
    assert!(h.state().s.settings.bool("mail.readingPane.alwaysPreview"));
    assert!(!has(&h, mailcraft_ui_egui::reading::PREVIEW_OFF));
    assert!(has(&h, "More actions"));
    assert!(has_part(&h, &subject));
}

#[test]
fn read_windows_show_next_and_previous_links_per_the_option() {
    let mut app = App::new(sample_session());
    let order = queries::visible_item_ids(&app.s);
    let second = order[1].0;
    app.ui.read_windows.push(second);
    let mut h = build(app, 1600.0, 1000.0);
    assert!(
        has(&h, mailcraft_ui_egui::reading::NEXT_LINK) && has(&h, mailcraft_ui_egui::reading::PREVIOUS_LINK)
    );
    h.get_by_label(mailcraft_ui_egui::reading::NEXT_LINK).click();
    h.run_steps(3);
    assert_eq!(h.state().ui.read_windows, vec![order[2].0], "the window moved to the next message");
    h.get_by_label(mailcraft_ui_egui::reading::PREVIOUS_LINK).click();
    h.run_steps(3);
    assert_eq!(h.state().ui.read_windows, vec![second]);
    // Off: no links.
    h.state_mut().run("options.set", json!({"key": "mail.other.nextPreviousLinks", "value": false}));
    h.run_steps(3);
    assert!(!has(&h, mailcraft_ui_egui::reading::NEXT_LINK));
    assert!(!has(&h, mailcraft_ui_egui::reading::PREVIOUS_LINK));
}

#[test]
fn portrait_windows_read_full_screen_only_with_the_option() {
    // 900 × 1400: wide enough for list and reading pane side by side.
    let mut h = build(App::new(sample_session()), 900.0, 1400.0);
    assert!(!h.state().frame_for(900.0, 1400.0).single_pane);
    select_first(&mut h);
    let panes = App::shell_layout(&h.ctx).panes;
    assert!(panes.contains(&"list") && panes.contains(&"reading"), "{panes:?}");

    let mut h = build(app_with(&[("mail.readingPane.portraitFullScreen", json!(true))]), 900.0, 1400.0);
    assert!(h.state().frame_for(900.0, 1400.0).single_pane);
    assert!(!h.state().frame_for(1400.0, 900.0).single_pane, "landscape keeps both panes");
    let panes = App::shell_layout(&h.ctx).panes;
    assert!(panes.contains(&"list") && !panes.contains(&"reading"), "{panes:?}");
    select_first(&mut h);
    let panes = App::shell_layout(&h.ctx).panes;
    assert!(panes.contains(&"reading") && !panes.contains(&"list"), "message full-screen: {panes:?}");
}

#[test]
fn f4_finds_in_the_message() {
    let mut h = build(App::new(sample_session()), 1600.0, 1000.0);
    select_first(&mut h);
    assert!(!has(&h, mailcraft_ui_egui::reading::FIND_LABEL));
    h.key_press(egui::Key::F4);
    h.run_steps(4);
    assert!(has(&h, mailcraft_ui_egui::reading::FIND_LABEL), "F4 opened the find bar");
}

/// Runs `n` frames and counts the window icon commands they sent.
fn icon_commands(h: &mut Harness<'_, App>, n: usize) -> usize {
    let mut count = 0;
    for _ in 0..n {
        h.step();
        count += h
            .output()
            .viewport_output
            .values()
            .flat_map(|v| v.commands.iter())
            .filter(|c| matches!(c, egui::ViewportCommand::Icon(Some(_))))
            .count();
    }
    count
}

#[test]
fn new_mail_lights_the_envelope_when_the_option_is_on() {
    for on in [true, false] {
        let mut app = app_with(&[("mail.arrival.envelope", json!(on))]);
        let inbox = app.s.selection.folder;
        let m = app.s.mailbox.items_in(inbox)[0].id;
        app.s.mailbox.set_read(&[m], false);
        let mut h = build(app, 1600.0, 1000.0);
        assert!(!has(&h, "1 new message"));
        assert_eq!(icon_commands(&mut h, 2), 0, "the app keeps its plain icon");
        mailcraft_engine::sync::arrival::on_new_mail(&mut h.state_mut().s, &[m]);
        let badge = icon_commands(&mut h, 3);
        assert_eq!(has(&h, "1 new message"), on, "status bar envelope");
        assert_eq!(badge, usize::from(on), "window icon badge");
        if on {
            // Clicking the envelope opens the folder; the message is no longer new.
            h.get_by_label("1 new message").click();
            let restored = icon_commands(&mut h, 3);
            assert!(!has(&h, "1 new message"));
            assert_eq!(restored, 1, "plain icon back");
        }
    }
    let (rgba, w, hgt) = mailcraft_ui_egui::app_icon_with_envelope_rgba(64).unwrap();
    assert_eq!((rgba.len(), w, hgt), (64 * 64 * 4, 64, 64));
    assert_ne!(Some(rgba), mailcraft_ui_egui::app_icon_rgba(64).map(|i| i.0), "the badge changes the icon");
}

/// General › "When using multiple displays" is not applicable (see settings.rs): whichever choice is made, a
/// window moved to another display keeps its physical size (the display's scale applies and the user's zoom
/// is left alone), and switching the choice back and forth changes nothing.
#[test]
fn moving_between_displays_keeps_the_physical_size_in_either_mode() {
    for mode in ["appearance", "compatibility"] {
        let mut h = build(app_with(&[("general.multipleDisplays", json!(mode))]), 1400.0, 900.0);
        assert_eq!(h.ctx.pixels_per_point(), 1.0);
        for (scale, then) in [(2.0, "appearance"), (1.0, "compatibility"), (2.0, mode)] {
            // The window moves to a display at `scale`, then the option is switched.
            if let Some(v) = h.input_mut().viewports.get_mut(&egui::ViewportId::ROOT) {
                v.native_pixels_per_point = Some(scale);
            }
            h.run_steps(3);
            assert_eq!(h.ctx.pixels_per_point(), scale, "{mode} at {scale}");
            assert_eq!(h.ctx.zoom_factor(), 1.0, "{mode} at {scale}: the zoom is untouched");
            h.state_mut().run("options.set", json!({"key": "general.multipleDisplays", "value": then}));
            h.run_steps(2);
            assert_eq!(h.ctx.pixels_per_point(), scale, "{mode} then {then}");
        }
    }
}

/// Shapes drawn in the last frame.
fn shape_count(h: &Harness<'_, App>) -> usize {
    h.output().shapes.len()
}

#[test]
fn office_background_draws_a_title_bar_pattern() {
    let counts: Vec<usize> = ["none", "circles", "geometry"]
        .iter()
        .map(|b| {
            let mut h = build(app_with(&[("general.background", json!(b))]), 1400.0, 900.0);
            h.step();
            shape_count(&h)
        })
        .collect();
    assert!(counts[1] > counts[0] && counts[2] > counts[0], "{counts:?}");
}

#[test]
fn show_read_aloud_off_takes_it_off_the_ribbon() {
    let h = build(App::new(sample_session()), 1920.0, 1000.0);
    assert!(has(&h, "Read Aloud"), "on by default (Home tab)");
    let h = build(app_with(&[("accessibility.readAloud", json!(false))]), 1920.0, 1000.0);
    assert!(!has(&h, "Read Aloud"));
    let mut app = app_with(&[("accessibility.readAloud", json!(false))]);
    app.run("options.set", json!({"key": "view.simplifiedRibbon", "value": false}));
    let h = build(app, 1920.0, 1000.0);
    assert!(!has(&h, "Read Aloud"), "classic ribbon too");
}

#[test]
fn incomplete_search_notice_sits_above_the_results() {
    let mut app = App::new(sample_session());
    if let Some(a) = app.s.mailbox.accounts.first_mut() {
        a.kind = mailcraft_engine::model::AccountKind::Imap;
    }
    let inbox = app.s.selection.folder;
    let m = app.s.mailbox.items_in(inbox)[0].id;
    if let Some(item) = app.s.mailbox.mail.get_mut(&m) {
        item.body = mailcraft_engine::model::Body::default();
        item.raw = None;
    }
    app.run("search.query", json!({"text": "the"}));
    let mut h = build(app, 1600.0, 1000.0);
    assert!(has_part(&h, "Results might be incomplete"));
    h.state_mut().run("options.set", json!({"key": "search.notifyIncomplete", "value": false}));
    h.run_steps(3);
    assert!(!has_part(&h, "Results might be incomplete"));
}

#[test]
fn people_view_switches_to_the_additional_index() {
    let mut app = app_with(&[
        ("people.additionalIndex", json!(true)),
        ("people.additionalIndexLanguage", json!("greek")),
    ]);
    app.run("people.new", json!({"fullName": "Ελένη Παππά", "email": "eleni@example.org"}));
    app.run("nav.people", json!({}));
    let mut h = build(app, 1600.0, 1000.0);
    assert!(has(&h, "Jump to a") && !has(&h, "Jump to α"));
    h.get_by_label("Show the additional index").click();
    h.run_steps(3);
    assert!(has(&h, "Jump to α") && has(&h, "Jump to π"));
    assert!(has(&h, "Show the a to z index"));
    h.get_by_label("Jump to π").click();
    h.run_steps(3);
    // Option off: neither the switch nor the Greek letters.
    h.state_mut().run("options.set", json!({"key": "people.additionalIndex", "value": false}));
    h.run_steps(3);
    assert!(has(&h, "Jump to a") && !has(&h, "Show the additional index") && !has(&h, "Jump to α"));
}

#[test]
fn calendar_shows_the_alternate_calendar_when_enabled() {
    let mut app = App::new(sample_session());
    app.run("nav.calendar", json!({}));
    let h = build(app, 1600.0, 1000.0);
    assert!(!has_part(&h, "Tishri") && !has_part(&h, "Rabi"));

    let mut app = app_with(&[
        ("calendar.alternateCalendar", json!(true)),
        ("calendar.alternateCalendarType", json!("hebrew")),
    ]);
    app.run("nav.calendar", json!({}));
    let h = build(app, 1600.0, 1000.0);
    // The sample clock sits in October 2026: Tishri / Heshvan 5787.
    assert!(has_part(&h, " 5787"), "alternate dates on the day headers or cells");
}

#[test]
fn people_peek_lists_names_with_or_without_details() {
    for names_only in [false, true] {
        let mut app = app_with(&[("people.namesOnlyInPeek", json!(names_only))]);
        let now = app.s.now();
        app.s.mailbox.record_recipients(&[("Ann Lee".into(), "ann@example.com".into())], now);
        let mut h = build(app, 1600.0, 1000.0);
        let people = h.query_all_by_label("People").next().unwrap().rect();
        h.hover_at(people.center());
        // Tooltips wait for the pointer to rest.
        for _ in 0..90 {
            h.step();
            if has(&h, mailcraft_ui_egui::chrome::PEOPLE_PEEK) {
                break;
            }
        }
        assert!(has(&h, mailcraft_ui_egui::chrome::PEOPLE_PEEK), "the peek opened");
        assert!(has(&h, "Ann Lee"));
        assert_eq!(has(&h, "ann@example.com"), !names_only);
    }
}
