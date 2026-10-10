//! MailCraft's user interface (layer L6): an Outlook-style shell on egui.
//!
//! Behaviour and geometry follow the black-box notes in `plan/outlook/` (layout.md, responsive.md,
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
pub mod folders;
pub mod icons;
pub mod keys;
pub mod list;
pub mod menus;
pub mod modules;
pub mod reading;
pub mod responsive;
pub mod ribbon;
pub mod theme;
pub mod widgets;

use std::collections::BTreeMap;
use std::sync::Arc;

use mailcraft_engine::{Module, Notice, Session};
use mailcraft_model::{ItemId, ReadingPanePos};
use serde_json::Value;

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
    /// Search box has focus (shows the Search contextual tab).
    pub search_active: bool,
    pub search_text: String,
    /// Single-pane mode: reading pane shown instead of the list (narrow windows).
    pub single_pane_reading: bool,
    /// Toast / status message and when it expires (seconds, egui time).
    pub toast: Option<(String, f64)>,
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
    /// A `view.todoBar` part waiting to be applied to the To-Do Bar (egui memory) at the next frame boundary.
    pub todo_bar_request: Option<String>,
    /// To-Do Bar parts shown (calendar, people, tasks), mirrored each frame for the ribbon's check marks.
    pub todo_bar_parts: (bool, bool, bool),
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
    /// Focused item last frame (single-pane switching).
    pub last_focus: Option<ItemId>,
    /// Toasts on screen: text, expiry (egui seconds), error?
    pub toasts: Vec<(String, f64, bool)>,
    /// Focus the search box on the next frame.
    pub focus_search: bool,
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
pub(crate) fn with_shell<R>(ctx: &egui::Context, f: impl FnOnce(&mut ShellState) -> R) -> R {
    ctx.data_mut(|d| f(d.get_temp_mut_or_default::<ShellState>(shell_id())))
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
    pub fn new(s: Session) -> App {
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
        if self.s.sync_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(SYNC_BUSY_REPAINT_MS));
        } else if !self.s.sync_status().accounts.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_secs(SYNC_IDLE_REPAINT_SECS));
        }
    }

    /// Runs a command; errors become a toast (never panics).
    pub fn run(&mut self, id: &str, params: Value) -> Option<Value> {
        match self.s.run(id, &params) {
            Ok(v) => {
                // The session keeps no To-Do Bar state: the UI applies the part (see `apply_todo_bar`).
                if id == "view.todoBar"
                    && let Some(part) = v.get("todoBar").and_then(Value::as_str)
                {
                    self.ui.todo_bar_request = Some(part.to_owned());
                }
                Some(v)
            }
            Err(e) => {
                self.ui.toast = Some((e.to_string(), 0.0));
                None
            }
        }
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

    /// The responsive frame for a main window of `width`×`height`.
    pub fn frame_for(&self, width: f32, height: f32) -> responsive::Frame {
        let right = self.mail_like_module() && self.reading_pane_pos() == ReadingPanePos::Right;
        responsive::Frame::compute_for(width, height, right)
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
        egui_extras::install_image_loaders(&ctx);
        self.sync_theme(&ctx);
        // Control-channel requests (when `--control-port` asked for one) apply before drawing.
        #[cfg(not(target_arch = "wasm32"))]
        control::frame(&ctx, self);
        self.poll_sync(&ctx);
        keys::handle(&ctx, self, Surface::Main);
        self.drain_notices(&ctx);
        self.sync_window(&ctx);
        self.apply_todo_bar(&ctx);

        // The whole viewport, whatever margins the host ui carries (test harnesses wrap it in a panel frame).
        let rect = ctx.content_rect();
        let frame = self.frame_for(rect.width(), rect.height());
        self.track_single_pane(&ctx, &frame);
        let mut window = ui.new_child(egui::UiBuilder::new().id_salt("mailcraft_main_window").max_rect(rect));
        window.set_clip_rect(rect);
        chrome::main_window(&mut window, self, &frame);

        reading::read_windows(&ctx, self);
        compose::compose_windows(&ctx, self);
        backstage::backstage(&ctx, self);
        // Item windows (appointment, contact, task, Go to Date) take their requests out of `ui.dialogs`
        // before the dialogs area draws the rest; a no-op when a module view already drew them this pass.
        modules::pim_windows(&ctx, self);
        dialogs::dialogs(&ctx, self);

        self.drain_notices(&ctx);
        if self.ui.todo_bar_request.is_some() {
            ctx.request_repaint();
        }
        self.draw_toasts(&ctx);
    }

    /// Applies a pending `view.todoBar` part and mirrors the parts shown into [`UiState::todo_bar_parts`].
    fn apply_todo_bar(&mut self, ctx: &egui::Context) {
        if let Some(part) = self.ui.todo_bar_request.take() {
            modules::set_todo_bar(ctx, &part);
        }
        self.ui.todo_bar_parts = modules::todo_bar_parts(ctx);
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
        let (max, min) = (self.s.view.maximized, self.s.view.minimized);
        let (applied_max, applied_min) = with_shell(ctx, |st| (st.applied_maximized, st.applied_minimized));
        if let Some(prev) = applied_max
            && prev != max
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(max));
        }
        if min && !applied_min {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        with_shell(ctx, |st| {
            st.applied_maximized = Some(max);
            st.applied_minimized = min;
        });
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
            self.ui.search_active = true;
            self.ui.set_tab(Surface::Main, "Search");
            with_shell(ctx, |st| st.focus_search = true);
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
