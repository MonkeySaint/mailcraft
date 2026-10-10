//! Context menus draw for every row kind of the sample mailbox without panicking, and helper state
//! (recent folders, status bar items) behaves.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use egui::RawInput;
use mailcraft_engine::queries::{ListRow, list_rows};
use mailcraft_engine::sample::sample_session;
use mailcraft_model::{ArrangeBy, FolderId};
use mailcraft_ui_egui::App;
use mailcraft_ui_egui::menus;

fn draw(ctx: &egui::Context, app: &mut App, f: impl Fn(&mut egui::Ui, &mut App)) {
    let mut out = ctx.run_ui(RawInput::default(), |ui| f(ui, app));
    out.textures_delta.clear();
}

#[test]
fn every_menu_draws() {
    let ctx = egui::Context::default();
    let mut app = App::new(sample_session());
    let rows = list_rows(&app.s);
    let item = rows.iter().find_map(|r| match r {
        ListRow::Item(i) => Some(i.id.0),
        ListRow::Group { .. } => None,
    });
    let group = rows.iter().find_map(|r| match r {
        ListRow::Group { label, .. } => Some(label.clone()),
        ListRow::Item(_) => None,
    });
    if let Some(item) = item {
        draw(&ctx, &mut app, |ui, app| menus::message_row_menu(ui, app, item));
        assert!(app.s.selection.items.iter().any(|i| i.0 == item), "right-click selects the row");
    }
    if let Some(label) = group {
        draw(&ctx, &mut app, |ui, app| menus::group_header_menu(ui, app, &label));
    }
    draw(&ctx, &mut app, |ui, app| menus::message_row_menu(ui, app, 987_654_321));
    let folders: Vec<FolderId> = app.s.mailbox.folders.keys().copied().collect();
    for f in folders {
        draw(&ctx, &mut app, |ui, app| menus::folder_menu(ui, app, f));
        draw(&ctx, &mut app, |ui, app| menus::favorite_folder_menu(ui, app, f));
    }
    draw(&ctx, &mut app, |ui, app| menus::folder_menu(ui, app, FolderId(u64::MAX)));
    let accounts: Vec<u32> = app.s.mailbox.accounts.iter().map(|a| a.id.0).collect();
    for a in accounts {
        draw(&ctx, &mut app, |ui, app| menus::account_menu(ui, app, a));
    }
    draw(&ctx, &mut app, |ui, app| menus::account_menu(ui, app, u32::MAX));
    draw(&ctx, &mut app, menus::status_bar_menu);
    draw(&ctx, &mut app, menus::arrange_menu);
}

#[test]
fn recent_folders_and_status_items() {
    let ctx = egui::Context::default();
    for i in 0..15 {
        menus::note_recent_folder(&ctx, FolderId(i));
    }
    menus::note_recent_folder(&ctx, FolderId(3));
    let recent = menus::recent_folders(&ctx);
    assert_eq!(recent.len(), 10);
    assert_eq!(recent[0], FolderId(3));
    assert_eq!(recent.iter().filter(|f| **f == FolderId(3)).count(), 1);

    assert!(!menus::status_bar_item_shown(&ctx, "quota"));
    assert!(menus::status_bar_item_shown(&ctx, "zoomSlider"));
    assert_eq!(menus::thousands(7219), "7,219");
    assert_eq!(menus::thousands(0), "0");
    assert_eq!(menus::thousands(1_234_567), "1,234,567");
    assert_eq!(menus::sort_labels(ArrangeBy::Date)[0].0, "Newest on Top");
    assert_eq!(menus::sort_labels(ArrangeBy::From)[0].1, "asc");
}
