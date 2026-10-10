//! Headless screenshot of the main window with the sample mailbox (egui_kittest + wgpu, no window).
//!
//! ```text
//! cargo run -p mailcraft-ui-egui --example ui_shot -- <out.png> [WIDTHxHEIGHT] [--dark] [--module NAME]
//!     [--reading right|bottom|off] [--select] [--classic] [--scale F]
//! ```
//!
//! Defaults: 1920x1040, light theme, Mail module, nothing selected, scale 1. `--module` takes mail, calendar,
//! people, tasks, notes. `--select` selects the first message so the reading pane shows it.
//!
//! wgpu needs a GPU adapter (Vulkan/Metal/DX12/GL, or a software one such as lavapipe/llvmpipe). When none can
//! be created the program prints why and exits with status 3 without writing a file.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::ExitCode;

use egui_kittest::Harness;
use mailcraft_engine::queries::{self, ListRow};
use mailcraft_engine::sample::sample_session;
use mailcraft_ui_egui::App;
use serde_json::json;

struct Opts {
    out: String,
    w: f32,
    h: f32,
    dark: bool,
    classic: bool,
    module: Option<String>,
    reading: Option<String>,
    select: bool,
    scale: f32,
}

fn parse() -> Result<Opts, String> {
    let mut args = std::env::args().skip(1);
    let mut o = Opts {
        out: String::new(),
        w: 1920.0,
        h: 1040.0,
        dark: false,
        classic: false,
        module: None,
        reading: None,
        select: false,
        scale: 1.0,
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dark" => o.dark = true,
            "--classic" => o.classic = true,
            "--select" => o.select = true,
            "--module" => o.module = Some(args.next().ok_or("--module needs a name")?),
            "--reading" => o.reading = Some(args.next().ok_or("--reading needs right, bottom or off")?),
            "--scale" => {
                let v = args.next().ok_or("--scale needs a number")?;
                o.scale = v
                    .parse::<f32>()
                    .ok()
                    .filter(|s| s.is_finite() && *s > 0.2 && *s <= 4.0)
                    .ok_or("bad --scale")?;
            }
            s if s.contains('x') && s.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
                let (w, h) = s.split_once('x').ok_or("size must be WIDTHxHEIGHT")?;
                let w = w.parse::<f32>().map_err(|_| "bad width")?;
                let h = h.parse::<f32>().map_err(|_| "bad height")?;
                if !(100.0..=8000.0).contains(&w) || !(100.0..=8000.0).contains(&h) {
                    return Err("size out of range (100..8000)".into());
                }
                (o.w, o.h) = (w, h);
            }
            s if s.starts_with('-') => return Err(format!("unknown option `{s}`")),
            s => o.out = s.to_owned(),
        }
    }
    if o.out.is_empty() {
        return Err("usage: ui_shot <out.png> [WIDTHxHEIGHT] [--dark] [--module NAME] [--reading POS] [--select] [--classic] [--scale F]".into());
    }
    Ok(o)
}

fn shoot(o: &Opts) -> Result<(), String> {
    let mut app = App::new(sample_session());
    if o.dark {
        app.run("options.set", json!({ "key": "view.darkMode", "value": true }));
    }
    if o.classic {
        app.run("app.toggleRibbonMode", json!({ "mode": "classic" }));
    }
    if let Some(pos) = &o.reading {
        app.run("view.readingPane", json!({ "position": pos }));
    }
    if o.select {
        let first = queries::list_rows(&app.s).into_iter().find_map(|r| match r {
            ListRow::Item(i) => Some(i.id.0),
            ListRow::Group { .. } => None,
        });
        if let Some(id) = first {
            app.run("nav.selectItem", json!({ "id": id }));
        }
    }
    if let Some(m) = &o.module {
        app.run(&format!("nav.{m}"), json!({}));
    }
    let mut fonts_done = false;
    let mut harness = Harness::builder()
        .with_size(egui::vec2(o.w, o.h))
        .with_pixels_per_point(o.scale)
        .wgpu()
        .build_ui_state(
            move |ui, app: &mut App| {
                if !fonts_done {
                    App::install_fonts(ui.ctx());
                    fonts_done = true;
                }
                app.ui_root(ui);
            },
            app,
        );
    // Fonts and icon textures settle over a few frames.
    harness.run_steps(6);
    let image = harness.render()?;
    image.save(&o.out).map_err(|e| format!("cannot write {}: {e}", o.out))
}

fn main() -> ExitCode {
    let o = match parse() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("ui_shot: {e}");
            return ExitCode::from(2);
        }
    };
    // kittest panics when no wgpu adapter can be created; report that instead of crashing.
    match catch_unwind(AssertUnwindSafe(|| shoot(&o))) {
        Ok(Ok(())) => {
            println!("wrote {} ({}x{} @{}x)", o.out, o.w, o.h, o.scale);
            ExitCode::SUCCESS
        }
        Ok(Err(e)) => {
            eprintln!("ui_shot: {e}");
            ExitCode::FAILURE
        }
        Err(payload) => {
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_else(|| "unknown error".into());
            eprintln!("ui_shot: wgpu could not render on this machine ({msg}); no screenshot written");
            ExitCode::from(3)
        }
    }
}
