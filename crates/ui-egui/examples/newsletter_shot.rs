//! Synthetic, offline visual check for centered newsletter columns (no real mailbox or remote images).
use egui_kittest::Harness;
use mailcraft_ui_egui::{
    App,
    body::{self, BodyView},
};
use std::process::ExitCode;

fn shoot(path: &str) -> Result<(), String> {
    let mut html = String::from(
        "<div style='max-width:630px;margin:0 auto;background-color:#ffffff;text-align:center'><h2>Three newsletter tiles</h2>",
    );
    let names = ["Morning", "Noon", "Evening"];
    for name in names {
        html.push_str(&format!("<div style='display:inline-block;vertical-align:top'><table width='170'><tr><td><img src='cid:{name}' width='150' height='150'><h2 style='text-align:left'>{name}</h2><p style='text-align:left'>A description for this tile.</p><p><a href='https://example.org/{name}'>Subscribe</a></p></td></tr></table></div>\n"));
    }
    html.push_str("</div>");
    let doc = mailcraft_html::html_to_doc(&html);
    let mut pictures = Vec::new();
    for (name, color) in
        names.into_iter().zip([[250, 225, 170, 255], [190, 220, 245, 255], [245, 190, 200, 255]])
    {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(150, 150, image::Rgba(color))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        pictures.push((format!("cid:{name}"), bytes.into_inner()));
    }
    let mut ready = false;
    let mut harness = Harness::builder().with_size(egui::vec2(1100.0, 700.0)).wgpu().build_ui_state(
        move |ui, view: &mut BodyView| {
            if !ready {
                App::install_fonts(ui.ctx());
                for (src, data) in &pictures {
                    body::provide_image(ui.ctx(), src, data);
                }
                ready = true;
            }
            body::show_doc(ui, view, &doc, false);
        },
        BodyView::default(),
    );
    harness.run_steps(6);
    harness.render()?.save(path).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("Usage: newsletter_shot OUTPUT.png");
        return ExitCode::from(2);
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| shoot(&path))) {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(error)) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
        Err(_) => {
            eprintln!("No compatible headless graphics renderer");
            ExitCode::from(3)
        }
    }
}
