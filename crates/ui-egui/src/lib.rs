//! MailCraft's user interface (layer L6): a classic desktop mail shell on egui.
//!
//! Behaviour and geometry follow the black-box notes in `plan/reference/` (layout.md, responsive.md,
//! folders.md, message-list.md, reading-pane.md, compose.md, read-window.md, ribbon-*.md, backstage.md,
//! options.md, dialogs.md, keyboard.md, context-menus.md, search.md, modules.md). The look (colours, icons) is
//! our own. Every action goes through [`App::run`] → `Session::run` (the engine command registry).
//!
//! Interface (frozen for commit 4; areas add items, never rename): [`App`], [`UiState`], [`Surface`], the
//! per-area entry points listed in each module's docs, [`theme::Theme`], [`responsive::Frame`].

pub mod backstage;
pub mod body;
pub mod chrome;
pub mod compose;
// The control channel listens on a local TCP socket from background threads; browsers have neither.
#[cfg(not(target_arch = "wasm32"))]
pub mod control;
pub mod dialogs;
pub mod edit_message;
pub mod explorer;
pub mod folders;
pub mod frame_timing;
pub mod icons;
pub mod keys;
pub mod list;
pub mod menu_keys;
pub mod menus;
pub mod modules;
pub mod reading;
pub mod remote_images;
pub mod responsive;
pub mod ribbon;
pub mod speech;
pub mod theme;
pub mod widgets;

use std::collections::BTreeMap;
use std::sync::Arc;

use mailcraft_engine::{Module, Notice, Session};
use mailcraft_model::{ItemId, ReadingPanePos};
use serde_json::{Value, json};

/// Which window a ribbon / keyboard handler belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Surface {
    Main,
    /// Compose window (by compose id).
    Compose(u64),
    /// Opened message window (by item id).
    Read(u64),
}

/// Transient UI state (not persisted with the mailbox).
#[derive(Debug, Default)]
pub struct UiState {
    /// Selected ribbon tab per surface ("Home", "Send / Receive", "Folder", "View", "Help"; compose: "Message",
    /// "Insert", "Options", "Format Text", "Review", "Help").
    pub ribbon_tab: BTreeMap<String, String>,
    /// File backstage open on this surface, with its page.
    pub backstage: Option<(Surface, String)>,
    /// Open dialogs (id + params), topmost last.
    pub dialogs: Vec<(String, Value)>,
    /// Opened read windows (item ids).
    pub read_windows: Vec<u64>,
    /// Read windows whose message is being edited in place (Edit Message), by item id.
    pub editing_messages: Vec<u64>,
    /// More main windows opened with Open in New Window, each with its own folder and selection.
    pub explorers: Vec<explorer::ExplorerWindow>,
    /// The platform draws extra windows inside the main one (the web build, test harnesses): Open in New
    /// Window then shows the folder in the main window instead (an embedded second main window would clash
    /// with the first). Mirrored from the context every frame.
    pub embedded_viewports: bool,
    /// The egui context of the last frame: commands run outside a frame's drawing code (shortcuts, the save
    /// prompt, AutoSave) first hand any half-typed drawing text to the engine (`compose::flush_drawing_text`).
    pub egui_ctx: Option<egui::Context>,
    /// Search box has focus (shows the Search contextual tab).
    pub search_active: bool,
    pub search_text: String,
    /// Single-pane mode: reading pane shown instead of the list (narrow windows).
    pub single_pane_reading: bool,
    /// Toast / status message and when it expires (seconds, egui time).
    pub toast: Option<(String, f64)>,
    /// Pages to open in the system browser (View in Browser), opened at the next frame.
    pub open_urls: Vec<String>,
    /// Keytip mode (Alt pressed): typed sequence so far.
    pub keytips: Option<String>,
    /// Hovered list row (for hover quick actions).
    pub hover_row: Option<u64>,
    /// Calendar view state owned by the modules area ("day", "workWeek", "week", "month", "schedule").
    pub calendar_view: String,
    /// Selected item ids in non-mail modules.
    pub module_selection: Option<u64>,
    /// Local control-channel port requested on the command line (`--control-port N`) or through
    /// `MAILCRAFT_CONTROL_PORT`; `None` = control channel off. Read by `control` (native builds only).
    pub control_port: Option<u16>,
    /// To-Do Bar parts shown (calendar, people, tasks), mirrored each frame from the session's view state.
    pub todo_bar_parts: (bool, bool, bool),
    /// The To-Do Bar parts an older profile kept in the dialog settings were taken over (and removed there).
    pub todo_bar_migrated: bool,
    /// Rename Folder in place in the folder pane (the row being renamed and the text typed).
    pub inline_rename: Option<InlineRename>,
    /// Text for the system clipboard after Copy (copied messages as From / Received / To / Subject lines), put
    /// there at the next frame boundary.
    pub clipboard_out: Option<String>,
}

/// A folder being renamed in the folder pane.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InlineRename {
    pub folder: u64,
    pub text: String,
    /// The text box got the keyboard focus.
    pub focused: bool,
    /// The row was drawn this frame (a folder whose row is not visible gets the Rename Folder dialog).
    pub drawn: bool,
    pub missed: u8,
}

impl Surface {
    /// Key of this surface in per-surface maps such as [`UiState::ribbon_tab`]: `"main"`, `"compose:3"`,
    /// `"read:42"` (the same keys as `ribbon::surface_key`).
    pub fn key(self) -> String {
        match self {
            Surface::Main => "main".to_owned(),
            Surface::Compose(id) => format!("compose:{id}"),
            Surface::Read(id) => format!("read:{id}"),
        }
    }
}

impl UiState {
    /// Selected ribbon tab on `surface` (`None` until a tab was picked; callers default to the first tab).
    pub fn tab(&self, surface: Surface) -> Option<&str> {
        self.ribbon_tab.get(&surface.key()).map(String::as_str)
    }
    /// Selects ribbon tab `name` on `surface`.
    pub fn set_tab(&mut self, surface: Surface, name: &str) {
        self.ribbon_tab.insert(surface.key(), name.to_owned());
    }
}

/// The application: session + UI state + theme + icons.
pub struct App {
    pub s: Session,
    pub ui: UiState,
    pub theme: theme::Theme,
    pub icons: icons::Icons,
    /// The platform's file dialogs (installed by the desktop app); `None` = dialogs show a path box instead.
    pub files: Option<Arc<dyn FileDialogs>>,
}

/// A file type offered by a file dialog: a name ("MBOX mailbox") and its extensions without the dot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

impl FileFilter {
    pub fn new(name: &str, extensions: &[&str]) -> FileFilter {
        FileFilter { name: name.to_owned(), extensions: extensions.iter().map(|e| (*e).to_owned()).collect() }
    }
}

/// Native file dialogs (Browse…, Save As…, Import/Export file pickers). The desktop app installs one in
/// [`App::files`]; tests install fakes; the web build has none, so dialogs fall back to typed paths. Every
/// method returns `None` when the person cancels. Implementations must not panic.
pub trait FileDialogs: Send + Sync {
    /// Open-file dialog: one file, or several when `multiple`. `start` = folder to show first.
    fn open_files(
        &self,
        title: &str,
        filters: &[FileFilter],
        multiple: bool,
        start: Option<&std::path::Path>,
    ) -> Option<Vec<std::path::PathBuf>>;
    /// Save dialog with `suggested` as the file name.
    fn save_file(
        &self,
        title: &str,
        suggested: &str,
        filters: &[FileFilter],
        start: Option<&std::path::Path>,
    ) -> Option<std::path::PathBuf>;
    /// Folder picker.
    fn pick_folder(&self, title: &str, start: Option<&std::path::Path>) -> Option<std::path::PathBuf>;
}

/// Repaint interval while a send/receive worker is busy (progress text, results).
const SYNC_BUSY_REPAINT_MS: u64 = 150;
/// Repaint interval with server accounts and nothing running (automatic send/receive, queued changes).
const SYNC_IDLE_REPAINT_SECS: u64 = 5;

/// Shell bookkeeping kept in egui temp memory (per context), never in the session.
#[derive(Clone, Default)]
pub(crate) struct ShellState {
    /// Dark flag of the visuals last installed (None = never installed in this context).
    pub applied_dark: Option<bool>,
    /// Window state last pushed to the OS window (None = first frame, nothing pushed yet).
    pub applied_maximized: Option<bool>,
    pub applied_minimized: bool,
    /// The OS reported the window minimized since the last Minimize was pushed (so a later "not minimized"
    /// means the person restored it).
    pub seen_minimized: bool,
    /// Focused item last frame (single-pane switching).
    pub last_focus: Option<ItemId>,
    /// Toasts on screen: text, expiry (egui seconds), error?
    pub toasts: Vec<(String, f64, bool)>,
    /// Focus the search box on the next frame.
    pub focus_search: bool,
    /// Text the search box selects when it takes that focus (char range; caret when empty).
    pub search_select: Option<(usize, usize)>,
    /// Ribbon temporarily shown while the display mode hides it (Show tabs only / Full-screen mode).
    pub ribbon_reveal: bool,
    /// Splitters: folder pane width, message list width (reading pane Right), list share (reading pane Bottom).
    pub folder_w: Option<f32>,
    pub list_w: Option<f32>,
    pub list_frac: Option<f32>,
    /// Left edge of the message list in window coordinates (the search box lines up with it).
    pub list_left: Option<f32>,
    /// Small rasterised app icon for the title bar.
    pub app_icon: Option<egui::TextureHandle>,
    /// What the last frame laid out (see [`ShellLayout`]).
    pub layout: ShellLayout,
    /// Content pane rectangles of the frame before (hit tests made before this frame's panes exist).
    pub prev_pane_rects: Vec<egui::Rect>,
    /// Whether the window icon last pushed carries the new-mail envelope (None = never pushed).
    pub applied_envelope: Option<bool>,
}

/// What the shell laid out in the last frame (for tests, the control channel and other areas).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShellLayout {
    /// Panes drawn in the content area, left to right / top to bottom: "folders", "list", "reading",
    /// "todoBar", "calendar", "people", "tasks", "notes".
    pub panes: Vec<&'static str>,
    /// Pane rectangles in the same order.
    pub rects: Vec<egui::Rect>,
    /// Folder pane width (0 = off, 26 = minimised strip).
    pub folder_pane_width: f32,
    /// Tab row and ribbon body shown.
    pub tabs_shown: bool,
    pub ribbon_shown: bool,
}

impl ShellLayout {
    /// Rectangle of pane `name`, if drawn.
    pub fn rect(&self, name: &str) -> Option<egui::Rect> {
        self.panes.iter().position(|p| *p == name).and_then(|i| self.rects.get(i).copied())
    }
}

pub(crate) fn shell_id() -> egui::Id {
    egui::Id::new("mailcraft_shell_state")
}

/// Reads and updates the shell state of `ctx`.
///
/// Each OS window (viewport) has its own: an explorer window (`explorer.rs`) lays out its own panes.
pub(crate) fn with_shell<R>(ctx: &egui::Context, f: impl FnOnce(&mut ShellState) -> R) -> R {
    let vp = ctx.viewport_id();
    let id = if vp == egui::ViewportId::ROOT { shell_id() } else { shell_id().with(vp) };
    ctx.data_mut(|d| f(d.get_temp_mut_or_default::<ShellState>(id)))
}

/// The `select` char range of a `searchBox` request (`[start, end]`, kept in order and within the query).
pub(crate) fn search_select(params: &Value) -> Option<(usize, usize)> {
    let a = params.pointer("/select/0")?.as_u64()?;
    let b = params.pointer("/select/1").and_then(Value::as_u64).unwrap_or(a);
    let len = params.get("query").and_then(Value::as_str).map_or(0, |q| q.chars().count());
    let clamp = |n: u64| usize::try_from(n).unwrap_or(usize::MAX).min(len);
    Some((clamp(a.min(b)), clamp(a.max(b))))
}

/// The bundled app icon (our own artwork).
pub const APP_ICON_SVG: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/app-icon/mailcraft.svg"));

/// Rasterises [`APP_ICON_SVG`] at `size`×`size`: straight (non-premultiplied) RGBA, as window icons want.
pub fn app_icon_rgba(size: u32) -> Option<(Vec<u8>, u32, u32)> {
    let size = size.clamp(1, 1024);
    let tree = resvg::usvg::Tree::from_data(APP_ICON_SVG, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    let svg = tree.size();
    let (w, h) = (svg.width().max(1.0), svg.height().max(1.0));
    #[allow(clippy::cast_precision_loss)]
    let scale = size as f32 / w.max(h);
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let mut rgba = Vec::with_capacity(pixmap.pixels().len() * 4);
    for p in pixmap.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some((rgba, size, size))
}

/// [`app_icon_rgba`] with the new-mail badge in the lower right corner: a filled circle (our own red) holding
/// a white envelope outline. Straight RGBA.
pub fn app_icon_with_envelope_rgba(size: u32) -> Option<(Vec<u8>, u32, u32)> {
    use resvg::tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
    let size = size.clamp(16, 1024);
    let (base, _, _) = app_icon_rgba(size)?;
    let mut pm = Pixmap::new(size, size)?;
    for (px, &[r, g, b, a]) in pm.pixels_mut().iter_mut().zip(base.as_chunks::<4>().0) {
        *px = resvg::tiny_skia::ColorU8::from_rgba(r, g, b, a).premultiply();
    }
    #[allow(clippy::cast_precision_loss)]
    let s = size as f32;
    let (cx, cy, r) = (s * 0.74, s * 0.74, s * 0.25);
    let mut paint = Paint { anti_alias: true, ..Paint::default() };
    paint.set_color(Color::from_rgba8(0xc4, 0x2b, 0x1c, 0xff));
    let circle = PathBuilder::from_circle(cx, cy, r)?;
    pm.fill_path(&circle, &paint, FillRule::Winding, Transform::identity(), None);
    // Envelope: a rectangle with the flap's V.
    let (w, h) = (r * 1.15, r * 0.8);
    let (x0, y0) = (cx - w / 2.0, cy - h / 2.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x0, y0);
    pb.line_to(x0 + w, y0);
    pb.line_to(x0 + w, y0 + h);
    pb.line_to(x0, y0 + h);
    pb.close();
    pb.move_to(x0, y0);
    pb.line_to(cx, cy + h * 0.1);
    pb.line_to(x0 + w, y0);
    let env = pb.finish()?;
    paint.set_color(Color::WHITE);
    let stroke = Stroke { width: (s * 0.035).max(1.0), ..Stroke::default() };
    pm.stroke_path(&env, &paint, &stroke, Transform::identity(), None);
    let mut rgba = Vec::with_capacity(pm.pixels().len() * 4);
    for p in pm.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some((rgba, size, size))
}

/// [`app_icon_rgba`] as an egui image.
pub fn app_icon_image(size: u32) -> Option<egui::ColorImage> {
    let (rgba, w, h) = app_icon_rgba(size)?;
    let size = [usize::try_from(w).ok()?, usize::try_from(h).ok()?];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, &rgba))
}

/// egui font name of one bundled face: `"<family> <weight>"`, plus `" italic"` for italic faces.
pub fn face_key(family: &str, weight: u16, italic: bool) -> String {
    if italic { format!("{family} {weight} italic") } else { format!("{family} {weight}") }
}

/// The bundled font database (built once; immutable).
fn font_db() -> &'static mailcraft_fonts::FontDb {
    static DB: std::sync::OnceLock<mailcraft_fonts::FontDb> = std::sync::OnceLock::new();
    DB.get_or_init(mailcraft_fonts::FontDb::bundled)
}

/// egui font family for a CSS-style family list, weight and style, resolved against the bundled faces with
/// the same aliases the layout engine uses (Aptos → Inter, Calibri → Carlito, Arial → Liberation Sans…).
/// Each bundled face is installed by [`App::install_fonts`] as `FontFamily::Name(face_key(..))`.
pub fn font_family(families: &str, weight: u16, italic: bool) -> egui::FontFamily {
    let db = font_db();
    let id = db.resolve(families, weight, italic);
    match db.face(id) {
        Some(f) => egui::FontFamily::Name(face_key(f.family, f.weight, f.italic).into()),
        None => egui::FontFamily::Proportional,
    }
}

/// Semibold UI font (headers, unread subjects).
pub fn ui_semibold() -> egui::FontFamily {
    font_family(mailcraft_fonts::UI_FAMILY, 600, false)
}

impl App {
    pub fn new(mut s: Session) -> App {
        // Internet calendars are fetched on the UI's worker threads (`modules::calendar_background`).
        s.sync.background_calendars = true;
        // Translations go to the translation server on a worker thread, so a slow server never freezes the
        // window (`mailcraft_engine::translate`).
        s.background_translate = true;
        let theme = theme::Theme::for_dark(s.view.dark);
        App { s, ui: UiState::default(), theme, icons: icons::Icons::default(), files: None }
    }

    /// Installs the platform's file dialogs.
    pub fn set_file_dialogs(&mut self, files: Arc<dyn FileDialogs>) {
        self.files = Some(files);
    }

    /// Applies finished send/receive work and keeps frames coming while it runs: every frame calls
    /// `Session::poll_sync`; a busy worker repaints often, and with server accounts the automatic
    /// send/receive timer and queued server changes are checked every few seconds.
    pub fn poll_sync(&mut self, ctx: &egui::Context) {
        if self.s.poll_sync() {
            ctx.request_repaint();
        }
        if self.s.sync_busy()
            || mailcraft_engine::translate::busy(&self.s)
            || mailcraft_engine::search_index::indexing(&self.s)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(SYNC_BUSY_REPAINT_MS));
        } else if !self.s.sync_status().accounts.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_secs(SYNC_IDLE_REPAINT_SECS));
        }
    }

    /// Runs a command; errors become a toast (never panics).
    pub fn run(&mut self, id: &str, params: Value) -> Option<Value> {
        // Text typed into a text box, shape or WordArt lives in the editor until the edit ends: every other
        // command (Send, Save, AutoSave, Close…) sees it first.
        if !id.starts_with("drawing.")
            && let Some(ctx) = self.ui.egui_ctx.clone()
        {
            let cids: Vec<u64> = match params.get("compose").and_then(Value::as_u64) {
                Some(cid) => vec![cid],
                None => self.s.composes.keys().copied().collect(),
            };
            for cid in cids {
                compose::flush_drawing_text(&ctx, self, cid);
            }
        }
        if let Some(asked) = self.confirm_purge(id, &params) {
            return Some(asked);
        }
        match self.s.run(id, &params) {
            Ok(v) => {
                if id == "view.todoBar" {
                    self.ui.todo_bar_parts = modules::todo_bar_parts(self);
                }
                // Copied messages also reach other programs as text.
                if id == "app.copy"
                    && let Some(text) = v.get("text").and_then(Value::as_str).filter(|t| !t.is_empty())
                {
                    self.ui.clipboard_out = Some(text.to_owned());
                }
                Some(v)
            }
            Err(e) => {
                self.ui.toast = Some((e.to_string(), 0.0));
                None
            }
        }
    }

    /// Delete on messages already in Deleted Items removes them for good: with Advanced › "Prompt for
    /// confirmation before permanently deleting items" on, it asks first (as Shift+Delete does), wherever
    /// Delete came from (ribbon, menus, keys). Returns the reply when it asked.
    fn confirm_purge(&mut self, id: &str, params: &Value) -> Option<Value> {
        if id != "mail.delete"
            || params.get("confirm").and_then(Value::as_bool) == Some(true)
            || !self.s.settings.bool("advanced.confirmPermanentDelete")
            || (self.s.enabled(id).is_some() && params.get("ids").is_none())
        {
            return None;
        }
        let ids = self.s.target_ids(params);
        let purged = mailcraft_engine::cmd::mail::purged_by_delete(&self.s, &ids);
        if purged == 0 {
            return None;
        }
        let text = if ids.len() == 1 {
            "The selected item will be permanently deleted.".to_owned()
        } else if purged == ids.len() {
            format!("The {} selected items will be permanently deleted.", ids.len())
        } else {
            format!("{purged} of the {} selected items will be permanently deleted.", ids.len())
        };
        let list: Vec<u64> = ids.iter().map(|i| i.0).collect();
        dialogs::confirm(self, "MailCraft", &text, id, json!({"ids": list, "confirm": true}));
        Some(json!({"dialog": "confirm"}))
    }

    /// Effective reading pane position of the selected folder (its saved view, else the window default).
    pub fn reading_pane_pos(&self) -> ReadingPanePos {
        self.s
            .mailbox
            .views
            .get(&self.s.selection.folder)
            .map_or(self.s.view.reading_pane, |v| v.reading_pane)
    }

    /// Whether the current module shows the mail panes (folder pane, list, reading pane).
    pub fn mail_like_module(&self) -> bool {
        matches!(self.s.view.module, Module::Mail | Module::Folders | Module::Shortcuts)
    }

    /// The responsive frame for a main window of `width`×`height`. Reading Pane options › "Turn on automatic
    /// full-screen reading in portrait orientation": a window taller than it is wide shows the list, and a
    /// message opened from it full-screen (single-pane reading with a back button), wherever the reading pane
    /// sits (not when it is off).
    pub fn frame_for(&self, width: f32, height: f32) -> responsive::Frame {
        let pos = self.reading_pane_pos();
        let right = self.mail_like_module() && pos == ReadingPanePos::Right;
        let mut f = responsive::Frame::compute_for(width, height, right);
        if self.mail_like_module()
            && pos != ReadingPanePos::Off
            && f.height > f.width
            && self.s.settings.bool("mail.readingPane.portraitFullScreen")
        {
            f.single_pane = true;
        }
        f
    }

    /// One frame of the whole UI (main window + secondary viewports + dialogs).
    pub fn ui(&mut self, ctx: &egui::Context) {
        let mut root = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("mailcraft_root"),
            egui::UiBuilder::new().layer_id(egui::LayerId::background()).max_rect(ctx.content_rect()),
        );
        self.ui_root(&mut root);
    }

    /// One frame drawn from a root `ui` (eframe's `App::ui` and test harnesses hand one over; [`App::ui`]
    /// builds it from the context). The main window always covers the whole viewport.
    pub fn ui_root(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut timer = frame_timing::FrameTimer::start();
        self.ui.egui_ctx = Some(ctx.clone());
        self.ui.embedded_viewports = ctx.embed_viewports();
        // The main window was asked to close (its close button, or File › Exit): a message with unsaved
        // Edit Message changes asks first and MailCraft stays open.
        if ctx.input(|i| i.viewport().close_requested()) && !edit_message::may_exit(&ctx, self) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        egui_extras::install_image_loaders(&ctx);
        self.sync_theme(&ctx);
        // Control-channel requests (when `--control-port` asked for one) apply before drawing.
        #[cfg(not(target_arch = "wasm32"))]
        control::frame(&ctx, self);
        timer.lap("control");
        self.poll_sync(&ctx);
        timer.lap("sync results");
        modules::calendar_background(&ctx, self);
        dialogs::auto_archive_tick(&ctx, self);
        compose::clipboard_watch(&ctx, self);
        keys::handle(&ctx, self, Surface::Main);
        self.drain_notices(&ctx);
        self.sync_window(&ctx);
        self.sync_envelope(&ctx);
        self.apply_todo_bar(&ctx);
        timer.lap("background work");

        // The whole viewport, whatever margins the host ui carries (test harnesses wrap it in a panel frame).
        let rect = ctx.content_rect();
        let frame = self.frame_for(rect.width(), rect.height());
        self.track_single_pane(&ctx, &frame);
        let mut window = ui.new_child(egui::UiBuilder::new().id_salt("mailcraft_main_window").max_rect(rect));
        window.set_clip_rect(rect);
        chrome::main_window(&mut window, self, &frame);
        timer.lap("main window");

        reading::read_windows(&ctx, self);
        timer.lap("message windows");
        explorer::explorer_windows(&ctx, self);
        compose::compose_windows(&ctx, self);
        timer.lap("other windows");
        backstage::backstage(&ctx, self);
        // Item windows (appointment, contact, task, Go to Date) take their requests out of `ui.dialogs`
        // before the dialogs area draws the rest; a no-op when a module view already drew them this pass.
        modules::pim_windows(&ctx, self);
        self.inline_rename_fallback();
        dialogs::dialogs(&ctx, self);

        self.drain_notices(&ctx);
        if let Some(text) = self.ui.clipboard_out.take() {
            ctx.copy_text(text);
        }
        self.draw_toasts(&ctx);
        timer.lap("dialogs");
        timer.finish();
    }

    /// A folder renamed in place whose row did not show (collapsed parent, pane hidden) gets the Rename
    /// Folder dialog instead.
    fn inline_rename_fallback(&mut self) {
        let Some(r) = self.ui.inline_rename.as_mut() else { return };
        if r.drawn {
            r.drawn = false;
            r.missed = 0;
            return;
        }
        r.missed = r.missed.saturating_add(1);
        if r.missed > 2
            && let Some(r) = self.ui.inline_rename.take()
        {
            dialogs::open(self, "renameFolder", json!({"folder": r.folder, "name": r.text, "inline": false}));
        }
    }

    /// Mirrors the To-Do Bar parts the session keeps into [`UiState::todo_bar_parts`]; parts an older profile
    /// kept in the dialog settings are taken over once (the old setting is removed with that).
    pub(crate) fn apply_todo_bar(&mut self, ctx: &egui::Context) {
        if !self.ui.todo_bar_migrated
            && let Some(old) = modules::take_saved_todo_bar(ctx)
        {
            self.ui.todo_bar_migrated = true;
            if !self.s.view.todo_bar.shown() {
                self.s.view.todo_bar = old;
            }
        }
        self.ui.todo_bar_parts = modules::todo_bar_parts(self);
    }

    /// Installs bundled fonts into egui (Inter for UI; all bundled faces as named families for message bodies).
    pub fn install_fonts(ctx: &egui::Context) {
        ctx.set_fonts(font_definitions());
    }

    /// Layout of the main window's last frame in `ctx`.
    pub fn shell_layout(ctx: &egui::Context) -> ShellLayout {
        with_shell(ctx, |st| st.layout.clone())
    }

    /// Queues a toast (bottom centre, 5 s).
    pub fn toast(&mut self, ctx: &egui::Context, text: impl Into<String>, error: bool) {
        let text = text.into();
        let until = ctx.input(|i| i.time) + theme::size::TOAST_SECS;
        self.ui.toast = Some((text.clone(), until));
        with_shell(ctx, |st| {
            st.toasts.retain(|(t, _, _)| *t != text);
            st.toasts.push((text, until, error));
            if st.toasts.len() > 4 {
                st.toasts.remove(0);
            }
        });
        ctx.request_repaint();
    }

    fn sync_theme(&mut self, ctx: &egui::Context) {
        if self.theme.dark != self.s.view.dark {
            self.theme = theme::Theme::for_dark(self.s.view.dark);
        }
        let dark = self.theme.dark;
        if with_shell(ctx, |st| st.applied_dark) != Some(dark) {
            self.theme.apply(ctx);
            with_shell(ctx, |st| st.applied_dark = Some(dark));
        }
    }

    /// Pushes Maximize / Minimize changes made by commands to the OS window (not on the first frame, so the
    /// window keeps the size the app opened it with).
    fn sync_window(&mut self, ctx: &egui::Context) {
        // The OS restored a window minimized by `app.minimize`: the view state follows, so the next Minimize
        // (from the control channel or MCP) is pushed again.
        let os_min = ctx.input(|i| i.viewport().minimized);
        let seen = with_shell(ctx, |st| {
            if os_min == Some(true) {
                st.seen_minimized = true;
            }
            st.seen_minimized
        });
        if self.s.view.minimized && seen && os_min == Some(false) {
            self.s.view.minimized = false;
            with_shell(ctx, |st| st.seen_minimized = false);
        }
        let (max, min) = (self.s.view.maximized, self.s.view.minimized);
        let (applied_max, applied_min) = with_shell(ctx, |st| (st.applied_maximized, st.applied_minimized));
        if let Some(prev) = applied_max
            && prev != max
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(max));
        }
        if min && !applied_min {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            with_shell(ctx, |st| st.seen_minimized = false);
        }
        with_shell(ctx, |st| {
            st.applied_maximized = Some(max);
            st.applied_minimized = min;
        });
    }

    /// Options › Mail › Message arrival › "Show an envelope icon in the taskbar": while new mail waits
    /// ([`mailcraft_engine::new_mail::envelope`]) the window icon carries an envelope badge (the taskbar, dock
    /// or window list shows it where the platform uses the window icon; the browser build has none) and the
    /// status bar shows an envelope ([`chrome::status_bar`]).
    fn sync_envelope(&mut self, ctx: &egui::Context) {
        let on = mailcraft_engine::new_mail::envelope(&self.s) > 0;
        let applied = with_shell(ctx, |st| st.applied_envelope);
        if applied == Some(on) || (applied.is_none() && !on) {
            // The app opens with the plain icon already set.
            with_shell(ctx, |st| st.applied_envelope = Some(on));
            return;
        }
        let icon = if on { app_icon_with_envelope_rgba(256) } else { app_icon_rgba(256) };
        if let Some((rgba, width, height)) = icon {
            ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(std::sync::Arc::new(egui::IconData {
                rgba,
                width,
                height,
            }))));
        }
        with_shell(ctx, |st| st.applied_envelope = Some(on));
    }

    /// Single-pane mode: a newly focused message replaces the list with the reading pane.
    fn track_single_pane(&mut self, ctx: &egui::Context, frame: &responsive::Frame) {
        let focus = self.s.selection.focus;
        let last = with_shell(ctx, |st| std::mem::replace(&mut st.last_focus, focus));
        if focus.is_none() {
            self.ui.single_pane_reading = false;
        } else if frame.single_pane && focus != last {
            self.ui.single_pane_reading = true;
        }
    }

    /// Drains `s.notices`: dialogs, toasts, and the few requests the shell handles itself.
    fn drain_notices(&mut self, ctx: &egui::Context) {
        if self.s.notices.is_empty() {
            return;
        }
        let notices = std::mem::take(&mut self.s.notices);
        for n in notices {
            match n {
                Notice::Info(text) => self.toast(ctx, text, false),
                Notice::Error(text) => self.toast(ctx, text, true),
                Notice::NewItemAlert(text, _) => self.toast(ctx, text, false),
                // Compose windows draw straight from `s.composes`.
                Notice::Compose(_) => {}
                Notice::OpenDialog(id, params) => self.open_dialog(ctx, id, params),
            }
        }
        ctx.request_repaint();
    }

    fn open_dialog(&mut self, ctx: &egui::Context, id: String, params: Value) {
        if id == "reminders" {
            // View › Reminders Window: the modules area's Reminders window (it scans for due reminders itself).
            modules::open_reminders(ctx, self);
        } else if id == "searchBox" {
            // `search.focus` (Ctrl+E / F3): focus the title-bar search box and bring up the Search tab.
            // A refiner that waits for a value (`select`: char range) puts the caret between its quotes.
            self.ui.search_active = true;
            self.ui.set_tab(Surface::Main, "Search");
            let select = search_select(&params);
            with_shell(ctx, |st| {
                st.focus_search = true;
                st.search_select = select;
            });
        } else {
            // Everything else (incl. backstage, readWindow, exit) is routed by the dialogs area.
            dialogs::open(self, &id, params);
        }
    }

    fn draw_toasts(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        // `App::run` errors arrive with expiry 0.0: start their clock now.
        if let Some((text, until)) = self.ui.toast.clone() {
            if until <= 0.0 {
                self.toast(ctx, text, true);
            } else if until < now {
                self.ui.toast = None;
            }
        }
        let toasts = with_shell(ctx, |st| {
            st.toasts.retain(|(_, until, _)| *until >= now);
            st.toasts.clone()
        });
        let Some(next) = toasts.iter().map(|(_, u, _)| *u).reduce(f64::min) else {
            return;
        };
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((next - now).clamp(0.05, 10.0)));
        let bottom = if self.s.view.status_bar { theme::size::STATUS_BAR } else { 0.0 } + 16.0;
        let theme = self.theme.clone();
        egui::Area::new(egui::Id::new("mailcraft_toasts"))
            .order(egui::Order::Tooltip)
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
            .interactable(true)
            .show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.vertical_centered(|ui| {
                    for (i, (text, _, error)) in toasts.iter().enumerate() {
                        let fill = if *error { theme.danger } else { theme.chrome };
                        let r = egui::Frame::new()
                            .fill(fill)
                            .corner_radius(egui::CornerRadius::same(6))
                            .inner_margin(egui::Margin::symmetric(14, 8))
                            .shadow(egui::Shadow {
                                offset: [0, 2],
                                blur: 8,
                                spread: 0,
                                color: egui::Color32::from_black_alpha(60),
                            })
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(egui::RichText::new(text).color(theme.chrome_text))
                                        .wrap()
                                        .sense(egui::Sense::click()),
                                )
                            });
                        if r.inner.clicked() {
                            // Click dismisses.
                            let gone = text.clone();
                            with_shell(ui.ctx(), |st| st.toasts.retain(|(t, _, _)| *t != gone));
                        }
                        if i + 1 < toasts.len() {
                            ui.add_space(6.0);
                        }
                    }
                });
            });
    }
}

/// Font definitions: every bundled face under its [`face_key`], each family name under its regular face, Inter
/// as the proportional default and Liberation Mono as the monospace default; egui's built-in fonts stay as
/// fallbacks (symbols, emoji).
pub fn font_definitions() -> egui::FontDefinitions {
    use egui::{FontData, FontFamily};
    let mut defs = egui::FontDefinitions::default();
    let fallback_prop = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let fallback_mono = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let db = font_db();
    // family name → (key of the face closest to regular, its distance from 400 upright)
    let mut regular: BTreeMap<&'static str, (String, u32)> = BTreeMap::new();
    for f in &db.faces {
        let key = face_key(f.family, f.weight, f.italic);
        defs.font_data.insert(key.clone(), Arc::new(FontData::from_static(f.data)));
        let mut list = vec![key.clone()];
        list.extend(fallback_prop.iter().cloned());
        defs.families.insert(FontFamily::Name(key.clone().into()), list);
        let distance = u32::from(f.weight.abs_diff(400)) + if f.italic { 1000 } else { 0 };
        let better = regular.get(f.family).is_none_or(|(_, d)| distance < *d);
        if better {
            regular.insert(f.family, (key, distance));
        }
    }
    for (family, (key, _)) in &regular {
        let mut list = vec![key.clone()];
        list.extend(fallback_prop.iter().cloned());
        defs.families.insert(FontFamily::Name((*family).into()), list);
    }
    let ui_key = face_key(mailcraft_fonts::UI_FAMILY, 400, false);
    if defs.font_data.contains_key(&ui_key) {
        let mut list = vec![ui_key];
        list.extend(fallback_prop.iter().cloned());
        defs.families.insert(FontFamily::Proportional, list);
    }
    let mono = db.resolve("monospace", 400, false);
    if let Some(f) = db.face(mono) {
        let mut list = vec![face_key(f.family, f.weight, f.italic)];
        list.extend(fallback_mono.iter().cloned());
        defs.families.insert(FontFamily::Monospace, list);
    }
    defs
}
