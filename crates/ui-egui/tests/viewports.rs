//! Issue #33: a message opened in its own window (an immediate viewport) made both windows slow. egui kept
//! one text-layout cache for every viewport and, at the start of each viewport's pass, dropped every layout
//! the pass before it had not used; the main window's pass and the message window's pass alternate, so each
//! dropped the other's layouts and every frame laid out all the text of both windows again. egui 0.37 keeps
//! a layout cache per viewport. These tests drive the whole app with a read window in its own viewport.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use mailcraft_engine::sample::sample_session;
use mailcraft_ui_egui::App;

/// Text layouts held in the layout cache of the viewport being drawn.
fn galleys(ctx: &egui::Context) -> usize {
    ctx.fonts(|f| f.num_galleys_in_cache())
}

/// The layout cache of one viewport pass, as the pass began (after the cache dropped what the viewport's
/// previous pass had not used) and as it ended. More at the end: the pass laid text out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Pass {
    begin: usize,
    end: usize,
}

impl Pass {
    fn new_layouts(self) -> usize {
        self.end.saturating_sub(self.begin)
    }
}

fn screen(w: f32, h: f32) -> Option<egui::Rect> {
    Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h)))
}

#[test]
fn a_read_window_and_the_main_window_keep_each_others_text_layouts() {
    let read: Arc<Mutex<Vec<(egui::ViewportId, Pass)>>> = Arc::default();
    let log = Arc::clone(&read);
    // Stands in for eframe: draws the read window's pass inside the main window's, as eframe does.
    egui::Context::set_immediate_viewport_renderer(move |ctx, vp| {
        let mut ui_cb = vp.viewport_ui_cb;
        let mut input = egui::RawInput {
            viewport_id: vp.ids.this,
            screen_rect: screen(900.0, 700.0),
            ..Default::default()
        };
        input.viewports.insert(vp.ids.this, egui::ViewportInfo::default());
        let mut pass = Pass::default();
        let mut out = ctx.run_ui(input, |ui| {
            pass.begin = galleys(ui.ctx());
            ui_cb(ui);
            pass.end = galleys(ui.ctx());
        });
        out.textures_delta.clear();
        log.lock().unwrap().push((vp.ids.this, pass));
    });

    let s = sample_session();
    let id = s.mailbox.mail.items().find(|m| m.subject == "Slides for Thursday").map(|m| m.id.0).unwrap();
    let mut app = App::new(s);
    let ctx = egui::Context::default();
    ctx.set_embed_viewports(false);
    App::install_fonts(&ctx);
    app.ui.read_windows.push(id);

    let mut main = Vec::new();
    for _ in 0..8 {
        let mut input = egui::RawInput { screen_rect: screen(1400.0, 900.0), ..Default::default() };
        // The platform has the read window from the second frame on (the main window's input lists it).
        if let Some((vid, _)) = read.lock().unwrap().last() {
            input.viewports.insert(*vid, egui::ViewportInfo::default());
        }
        let mut pass = Pass::default();
        let mut out = ctx.run_ui(input, |ui| {
            pass.begin = galleys(ui.ctx());
            app.ui_root(ui);
            pass.end = galleys(ui.ctx());
        });
        out.textures_delta.clear();
        main.push(pass);
    }
    let read: Vec<Pass> = read.lock().unwrap().iter().map(|(_, p)| *p).collect();
    assert_eq!(read.len(), main.len(), "the read window drew a pass in every main-window frame");
    eprintln!("main window passes: {main:?}\nread window passes: {read:?}");

    // Once settled (fonts installed, pictures decoded), neither window lays any text out again, and each
    // starts its pass with all of its own layouts still cached.
    let settled = |passes: &[Pass]| passes.iter().rev().take(3).copied().collect::<Vec<_>>();
    for (window, passes) in [("main", &main), ("read", &read)] {
        let last = settled(passes);
        assert!(last.iter().all(|p| p.begin > 20), "{window} window draws text: {passes:?}");
        assert!(
            last.iter().all(|p| p.new_layouts() == 0),
            "{window} window lays text out every frame: {passes:?}"
        );
        assert!(last.windows(2).all(|w| w[0] == w[1]), "{window} window's layouts are stable: {passes:?}");
    }
}
