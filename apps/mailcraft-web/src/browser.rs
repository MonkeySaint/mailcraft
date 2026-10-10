//! Browser glue (wasm32 only): the page's canvas, `Date`, `localStorage` and the eframe web runner.

use eframe::wasm_bindgen::JsCast as _;
use eframe::wasm_bindgen::JsValue;

use crate::persist;
use crate::shell::{Host, WebShell};

/// Id of the `<canvas>` in `index.html`.
const CANVAS_ID: &str = "mailcraft_canvas";
/// Id of the loading notice in `index.html` (removed once the app runs, or replaced by an error).
const LOADING_ID: &str = "mailcraft_loading";

/// [`Host`] backed by the browser window.
struct BrowserHost {
    storage: Option<web_sys::Storage>,
}

impl BrowserHost {
    fn new(window: &web_sys::Window) -> Self {
        // `local_storage` fails (or is absent) in some private modes and sandboxed frames: run unsaved then.
        let storage = window.local_storage().ok().flatten();
        if storage.is_none() {
            log::warn!("browser storage is not available; changes will not be kept");
        }
        BrowserHost { storage }
    }
}

impl Host for BrowserHost {
    fn now_ms(&self) -> f64 {
        js_sys::Date::now()
    }

    fn tz_offset_min(&self) -> i32 {
        // `getTimezoneOffset` is minutes *behind* UTC (240 for UTC-4); ours is east positive.
        let behind = js_sys::Date::new_0().get_timezone_offset();
        if !behind.is_finite() {
            return 0;
        }
        #[expect(clippy::cast_possible_truncation, reason = "clamped to ±14 h first")]
        let behind = behind.clamp(-840.0, 840.0).round() as i32;
        -behind
    }

    fn load(&self) -> Option<String> {
        self.storage.as_ref()?.get_item(persist::STORAGE_KEY).ok().flatten()
    }

    fn save(&mut self, text: &str) -> Result<(), String> {
        let Some(storage) = self.storage.as_ref() else {
            return Err("browser storage is not available".into());
        };
        storage.set_item(persist::STORAGE_KEY, text).map_err(|e| {
            let detail = js_text(&e);
            if detail.contains("Quota") || detail.contains("quota") {
                "the browser's storage for this site is full".to_owned()
            } else {
                detail
            }
        })
    }
}

/// Readable text for a JavaScript error value.
fn js_text(v: &JsValue) -> String {
    if let Some(s) = v.as_string() {
        return s;
    }
    if let Some(e) = v.dyn_ref::<js_sys::Error>() {
        return String::from(e.message());
    }
    format!("{v:?}")
}

/// True when the page address asks to forget the saved mailbox (`?reset`).
fn reset_requested(window: &web_sys::Window) -> bool {
    let search = window.location().search().unwrap_or_default();
    search.trim_start_matches('?').split('&').any(|kv| kv.split('=').next() == Some("reset"))
}

/// Shows `text` in the loading notice (startup failures; the canvas may not draw anything).
fn show_failure(document: &web_sys::Document, text: &str) {
    if let Some(el) = document.get_element_by_id(LOADING_ID) {
        el.set_text_content(Some(text));
        let _ = el.set_attribute("class", "failed");
    }
}

pub fn main() {
    // Fails only when a logger is already installed; keep going with that one.
    let _ = eframe::WebLogger::init(log::LevelFilter::Info);
    wasm_bindgen_futures::spawn_local(async {
        let Some(window) = web_sys::window() else { return };
        let Some(document) = window.document() else { return };
        if let Err(e) = start(&window, &document).await {
            log::error!("MailCraft could not start: {e}");
            show_failure(&document, &format!("MailCraft could not start: {e}"));
        }
    });
}

async fn start(window: &web_sys::Window, document: &web_sys::Document) -> Result<(), String> {
    let canvas = document
        .get_element_by_id(CANVAS_ID)
        .ok_or_else(|| format!("the page has no element with id `{CANVAS_ID}`"))?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| format!("`{CANVAS_ID}` is not a canvas"))?;

    let host = BrowserHost::new(window);
    if reset_requested(window)
        && let Some(storage) = host.storage.as_ref()
        && let Err(e) = storage.remove_item(persist::STORAGE_KEY)
    {
        log::warn!("could not clear the saved mailbox: {}", js_text(&e));
    }
    // A stored document that cannot be read falls back to the sample (with a notice); the bad copy stays
    // until the next save replaces it.
    let shell = WebShell::new(host);

    let runner = eframe::WebRunner::new();
    runner
        .start(
            canvas,
            eframe::WebOptions::default(),
            Box::new(move |cc| {
                mailcraft_ui_egui::App::install_fonts(&cc.egui_ctx);
                Ok(Box::new(shell))
            }),
        )
        .await
        .map_err(|e| js_text(&e))?;

    if let Some(el) = document.get_element_by_id(LOADING_ID) {
        el.remove();
    }
    Ok(())
}
