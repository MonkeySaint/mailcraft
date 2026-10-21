//! Folder › Open in New Window: more main windows ("explorer" windows), each on its own folder with its own
//! selection, module and search, shown as separate OS windows (egui viewports) like the read and compose
//! windows.
//!
//! The session has one selection and one current module. An explorer window keeps its own copies in
//! [`ExplorerWindow`]; while the window draws (and while its commands run) they are swapped into the session,
//! so the ribbon, folder pane, message list, reading pane and keyboard of that window act on its folder and
//! its selection, and the main window's selection is left alone. Dialogs a command opens carry the items
//! they act on, so they keep working after the swap back. The shell's per-frame layout state is kept per
//! viewport (see `with_shell`).

use egui::{ViewportBuilder, ViewportId};
use mailcraft_engine::{Module, Selection};
use mailcraft_model::FolderId;
use serde_json::json;

use crate::{App, Surface};

/// Most explorer windows open at once.
pub const MAX_EXPLORERS: usize = 16;

/// One extra main window: its own folder, selection, module and search.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExplorerWindow {
    /// Stable id of the window (its viewport id is derived from it).
    pub id: u64,
    pub selection: Selection,
    pub module: Module,
    pub search: String,
    pub search_text: String,
    pub search_active: bool,
    pub module_selection: Option<u64>,
}

/// Swaps the window's state with the session's (twice restores both).
fn swap(app: &mut App, w: &mut ExplorerWindow) {
    std::mem::swap(&mut app.s.selection, &mut w.selection);
    std::mem::swap(&mut app.s.view.module, &mut w.module);
    std::mem::swap(&mut app.s.view.search, &mut w.search);
    std::mem::swap(&mut app.ui.search_text, &mut w.search_text);
    std::mem::swap(&mut app.ui.search_active, &mut w.search_active);
    std::mem::swap(&mut app.ui.module_selection, &mut w.module_selection);
}

/// Runs `f` with explorer window `id`'s state in the session (`None` when there is no such window).
pub fn with_window<R>(app: &mut App, id: u64, f: impl FnOnce(&mut App) -> R) -> Option<R> {
    let i = app.ui.explorers.iter().position(|w| w.id == id)?;
    let mut w = app.ui.explorers.get_mut(i).map(std::mem::take)?;
    swap(app, &mut w);
    let r = f(app);
    swap(app, &mut w);
    if let Some(slot) = app.ui.explorers.get_mut(i) {
        *slot = w;
    }
    Some(r)
}

/// Opens an explorer window on `folder`; returns its id (`None` when too many are open, the folder is gone
/// or the platform cannot show another OS window).
pub fn open(app: &mut App, folder: FolderId) -> Option<u64> {
    if app.ui.embedded_viewports
        || app.ui.explorers.len() >= MAX_EXPLORERS
        || app.s.mailbox.folder(folder).is_none()
    {
        return None;
    }
    let id = app.ui.explorers.iter().map(|w| w.id).max().map_or(1, |m| m.saturating_add(1));
    let w = ExplorerWindow {
        id,
        selection: app.s.selection.clone(),
        module: app.s.view.module,
        ..Default::default()
    };
    app.ui.explorers.push(w);
    // Selecting the folder in the window's own state sets its module and list as the main window would.
    with_window(app, id, |app| app.run("nav.selectFolder", json!({"folder": folder.0})));
    Some(id)
}

pub(crate) fn viewport_id(id: u64) -> ViewportId {
    ViewportId::from_hash_of(("mailcraft.explorer", id))
}

/// Title of an explorer window: "<folder> - MailCraft".
pub fn title(app: &App, w: &ExplorerWindow) -> String {
    let name = app.s.mailbox.folder(w.selection.folder).map(|f| f.name.clone()).unwrap_or_default();
    if name.is_empty() { "MailCraft".to_owned() } else { format!("{name} - MailCraft") }
}

/// Draws the explorer windows (once per frame, from the shell).
pub fn explorer_windows(ctx: &egui::Context, app: &mut App) {
    if ctx.embed_viewports() {
        // No separate OS windows here: the windows' folders were shown in the main window instead.
        app.ui.explorers.clear();
        return;
    }
    let ids: Vec<u64> = app.ui.explorers.iter().map(|w| w.id).collect();
    let mut closed = Vec::new();
    for id in ids {
        let Some(w) = app.ui.explorers.iter().find(|w| w.id == id) else { continue };
        let builder = ViewportBuilder::default()
            .with_title(title(app, w))
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([360.0, 260.0]);
        let close = with_window(app, id, |app| {
            // A folder deleted meanwhile: the window falls back to the default folder.
            if app.s.mailbox.folder(app.s.selection.folder).is_none() {
                app.s.select_default_folder();
            }
            ctx.show_viewport_immediate(viewport_id(id), builder, |ui, _class| {
                let ctx = ui.ctx().clone();
                crate::keys::handle(&ctx, app, Surface::Main);
                let rect = ui.max_rect();
                let frame = app.frame_for(rect.width(), rect.height());
                let mut window =
                    ui.new_child(egui::UiBuilder::new().id_salt(("mailcraft_explorer", id)).max_rect(rect));
                window.set_clip_rect(rect);
                crate::chrome::main_window(&mut window, app, &frame);
                ctx.input(|i| i.viewport().close_requested())
            })
        })
        .unwrap_or(true);
        if close {
            closed.push(id);
        }
    }
    if !closed.is_empty() {
        app.ui.explorers.retain(|w| !closed.contains(&w.id));
        for id in closed {
            crate::reading::forget_viewport(ctx, viewport_id(id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Vec2;
    use egui_kittest::Harness;
    use mailcraft_engine::sample::sample_session;

    #[test]
    fn open_in_new_window_keeps_its_own_folder_and_selection() {
        let mut app = App::new(sample_session());
        let inbox = app.s.selection.folder;
        let acct = app.s.mailbox.folder_account(inbox).unwrap();
        let sent = app.s.mailbox.role_folder(acct, mailcraft_model::FolderRole::SentItems).unwrap();
        let main_sel = app.s.selection.clone();
        app.run("folder.openInNewWindow", json!({"folder": sent.0}));
        crate::dialogs::take_notices(&mut app);
        assert_eq!(app.ui.explorers.len(), 1, "a second main window opened");
        let w = &app.ui.explorers[0];
        assert_eq!(w.selection.folder, sent);
        assert!(title(&app, w).starts_with("Sent Items"));
        assert_eq!(app.s.selection, main_sel, "the main window keeps its folder and selection");
        // Commands run in the window's state act on its folder and selection only.
        let id = w.id;
        let sent_item = app.s.mailbox.items_in(sent).first().map(|m| m.id);
        let folder = with_window(&mut app, id, |app| {
            if let Some(item) = sent_item {
                app.run("nav.selectItem", json!({"id": item.0}));
            }
            app.s.selection.folder
        })
        .unwrap();
        assert_eq!(folder, sent);
        assert_eq!(app.s.selection, main_sel);
        if let Some(item) = sent_item {
            assert_eq!(app.ui.explorers[0].selection.focus, Some(item));
        }
    }

    #[test]
    fn embedded_platforms_show_the_folder_in_the_main_window() {
        let mut s = sample_session();
        let inbox = s.selection.folder;
        let acct = s.mailbox.folder_account(inbox).unwrap();
        let sent = s.mailbox.role_folder(acct, mailcraft_model::FolderRole::SentItems).unwrap();
        s.run("folder.openInNewWindow", &json!({"folder": sent.0})).unwrap();
        let mut h = Harness::builder().with_size(Vec2::new(1400.0, 900.0)).build_ui_state(
            |ui, app: &mut App| {
                ui.ctx().set_embed_viewports(true);
                app.ui_root(ui);
            },
            App::new(s),
        );
        h.run_steps(3);
        assert!(h.state().ui.explorers.is_empty());
        assert_eq!(h.state().s.selection.folder, sent);
    }

    #[test]
    fn explorer_count_is_capped_and_missing_folders_refused() {
        let mut app = App::new(sample_session());
        let inbox = app.s.selection.folder;
        for _ in 0..MAX_EXPLORERS {
            assert!(open(&mut app, inbox).is_some());
        }
        assert!(open(&mut app, inbox).is_none());
        app.ui.explorers.clear();
        assert!(open(&mut app, FolderId(u64::MAX)).is_none());
    }
}
